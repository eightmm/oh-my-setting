#!/usr/bin/env python3
"""Fail-closed JSONL checks and durable appends without following symlinks."""

import argparse
import errno
import os
import stat
import sys
import uuid


MAX_ROW_BYTES = 1024 * 1024
MAX_FILE_BYTES = 16 * 1024 * 1024
MAX_RECOVERY_BYTES = 256 * 1024 * 1024


def fail(message):
    print("error: %s" % message, file=sys.stderr)
    raise SystemExit(2)


def same_file(left, right):
    return left.st_dev == right.st_dev and left.st_ino == right.st_ino


def is_reparse(info):
    flag = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
    return bool(flag and getattr(info, "st_file_attributes", 0) & flag)


def safe_regular(info):
    return (stat.S_ISREG(info.st_mode) and not stat.S_ISLNK(info.st_mode) and
            not is_reparse(info) and info.st_nlink == 1)


def physical_root(root, label="artifact index"):
    candidate = os.path.realpath(os.path.abspath(root))
    try:
        info = os.lstat(candidate)
    except OSError as exc:
        fail("cannot inspect %s repository: %s" % (label, exc))
    if (stat.S_ISLNK(info.st_mode) or is_reparse(info) or
            not stat.S_ISDIR(info.st_mode)):
        fail("%s repository must be a real directory" % label)
    return candidate


def _inside(path, root):
    try:
        return os.path.normcase(os.path.commonpath([path, root])) == os.path.normcase(root)
    except ValueError:
        return False


def _fsync_directory_path(path, label):
    """Persist a newly created child name where directory fsync exists."""
    if os.name == "nt":
        return
    descriptor = None
    try:
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_CLOEXEC", 0))
        os.fsync(descriptor)
    except OSError as exc:
        if exc.errno not in (errno.EBADF, errno.EINVAL, errno.ENOTSUP):
            fail("cannot persist %s parent: %s" % (label, exc))
    finally:
        if descriptor is not None:
            os.close(descriptor)


def canonical_repo_path(root, path, label="artifact index", create_parent=False):
    """Return one physical, repo-bound spelling without following components."""
    logical_root = os.path.abspath(root)
    root = physical_root(root, label)
    if os.path.isabs(path):
        supplied = os.path.abspath(path)
        # A benign ancestry alias is only a spelling of the repo root. Rebase
        # the suffix without realpath-ing any in-repo component, so a planted
        # .oms/artifacts symlink is still visible and refused below.
        if _inside(supplied, logical_root):
            target = os.path.join(root, os.path.relpath(supplied, logical_root))
        else:
            target = supplied
    else:
        target = os.path.join(root, path)
    target = os.path.abspath(target)
    target = os.path.normpath(target)
    if target == root or not _inside(target, root):
        fail("%s must stay inside the physical repository" % label)

    parent = os.path.dirname(target)
    relative = os.path.relpath(parent, root)
    current = root
    missing_component = False
    if relative != os.curdir:
        for component in relative.split(os.sep):
            if component in ("", os.curdir, os.pardir):
                fail("%s path has an unsafe component" % label)
            current = os.path.join(current, component)
            if missing_component:
                continue
            created = False
            try:
                info = os.lstat(current)
            except FileNotFoundError:
                if not create_parent:
                    missing_component = True
                    continue
                try:
                    os.mkdir(current, 0o700)
                    created = True
                except FileExistsError:
                    pass
                except OSError as exc:
                    fail("cannot create %s parent safely: %s" % (label, exc))
                try:
                    info = os.lstat(current)
                except OSError as exc:
                    fail("cannot inspect created %s parent: %s" % (label, exc))
            except OSError as exc:
                fail("cannot inspect %s parent: %s" % (label, exc))
            if (stat.S_ISLNK(info.st_mode) or is_reparse(info) or
                    not stat.S_ISDIR(info.st_mode)):
                fail("%s parent components must be real directories" % label)
            if created:
                # The child directory itself will be fsynced when a leaf is
                # published below it; first make its name durable in the
                # already-existing parent so a power loss cannot leave a
                # repaired index whose quarantine directory vanished.
                _fsync_directory_path(os.path.dirname(current), label)

    try:
        leaf = os.lstat(target)
    except FileNotFoundError:
        leaf = None
    except OSError as exc:
        fail("cannot inspect %s: %s" % (label, exc))
    if leaf is not None and not safe_regular(leaf):
        fail("%s must be a regular non-symlink, non-hard-linked file" % label)
    return target


def component_snapshot(root, target, label="artifact index"):
    root = physical_root(root, label)
    parent = os.path.dirname(target)
    relative = os.path.relpath(parent, root)
    paths = [root]
    current = root
    if relative != os.curdir:
        for component in relative.split(os.sep):
            current = os.path.join(current, component)
            paths.append(current)
    snapshot = []
    for path in paths:
        try:
            info = os.lstat(path)
        except OSError as exc:
            fail("cannot inspect %s component: %s" % (label, exc))
        if (stat.S_ISLNK(info.st_mode) or is_reparse(info) or
                not stat.S_ISDIR(info.st_mode)):
            fail("%s parent components must be real directories" % label)
        snapshot.append((path, info))
    return snapshot


def components_unchanged(snapshot, label="artifact index"):
    for path, before in snapshot:
        try:
            after = os.lstat(path)
        except OSError as exc:
            fail("%s parent changed: %s" % (label, exc))
        if (stat.S_ISLNK(after.st_mode) or is_reparse(after) or
                not stat.S_ISDIR(after.st_mode) or not same_file(before, after)):
            fail("%s parent changed during mutation" % label)


def _read_descriptor(descriptor, limit, label):
    chunks = []
    remaining = limit + 1
    while remaining:
        chunk = os.read(descriptor, min(1024 * 1024, remaining))
        if not chunk:
            break
        chunks.append(chunk)
        remaining -= len(chunk)
    body = b"".join(chunks)
    if len(body) > limit:
        fail("%s exceeds the %d-byte recovery limit" % (label, limit))
    return body


def _snapshot_matches(before, after):
    return (same_file(before, after) and before.st_size == after.st_size and
            getattr(before, "st_mtime_ns", None) == getattr(after, "st_mtime_ns", None) and
            getattr(before, "st_ctime_ns", None) == getattr(after, "st_ctime_ns", None) and
            after.st_nlink == 1)


def read_no_follow(root, path, label="artifact index", missing_ok=False,
                   max_bytes=MAX_RECOVERY_BYTES):
    target = canonical_repo_path(root, path, label, False)
    parent = os.path.dirname(target)
    name = os.path.basename(target)
    try:
        parent_before = os.lstat(parent)
    except FileNotFoundError:
        if missing_ok:
            return b""
        fail("%s does not exist" % label)
    except OSError as exc:
        fail("cannot inspect %s parent: %s" % (label, exc))
    if (stat.S_ISLNK(parent_before.st_mode) or is_reparse(parent_before) or
            not stat.S_ISDIR(parent_before.st_mode)):
        fail("%s parent must be a real directory" % label)
    ancestry = component_snapshot(root, target, label)

    # POSIX anchors the leaf open to an already-open, no-follow parent. Native
    # Windows has no dir_fd support, so it uses the pathname plus the same
    # ancestry snapshot and named/opened-inode comparisons on both sides.
    directory = None
    if os.name != "nt":
        parent_flags = (os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) |
                        getattr(os, "O_CLOEXEC", 0) |
                        getattr(os, "O_NOFOLLOW", 0))
        try:
            directory = os.open(parent, parent_flags)
        except OSError as exc:
            fail("cannot anchor %s parent: %s" % (label, exc))
        try:
            anchored = os.fstat(directory)
            if (not stat.S_ISDIR(anchored.st_mode) or is_reparse(anchored) or
                    not same_file(parent_before, anchored)):
                fail("%s parent changed before read" % label)
        except BaseException:
            os.close(directory)
            raise
    try:
        components_unchanged(ancestry, label)
    except BaseException:
        if directory is not None:
            os.close(directory)
        raise

    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_CLOEXEC", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        if directory is None:
            descriptor = os.open(target, flags)
        else:
            descriptor = os.open(name, flags, dir_fd=directory)
    except FileNotFoundError:
        if directory is not None:
            os.close(directory)
        if missing_ok:
            return b""
        fail("%s does not exist" % label)
    except OSError as exc:
        if directory is not None:
            os.close(directory)
        fail("cannot open %s safely: %s" % (label, exc))
    try:
        opened = os.fstat(descriptor)
        if not safe_regular(opened):
            fail("%s must be a regular non-symlink, non-hard-linked file" % label)
        try:
            named = (os.lstat(target) if directory is None else
                     os.stat(name, dir_fd=directory, follow_symlinks=False))
        except OSError as exc:
            fail("%s pathname changed before read: %s" % (label, exc))
        if not safe_regular(named) or not same_file(opened, named):
            fail("%s pathname changed before read" % label)
        body = _read_descriptor(descriptor, max_bytes, label)
        after = os.fstat(descriptor)
        try:
            named_after = (os.lstat(target) if directory is None else
                           os.stat(name, dir_fd=directory, follow_symlinks=False))
        except OSError as exc:
            fail("%s pathname changed during read: %s" % (label, exc))
        if (not safe_regular(named_after) or not same_file(after, named_after) or
                not _snapshot_matches(opened, after)):
            fail("%s changed during read" % label)
        components_unchanged(ancestry, label)
        return body
    finally:
        os.close(descriptor)
        if directory is not None:
            os.close(directory)


def _write_all(descriptor, data, label):
    offset = 0
    while offset < len(data):
        written = os.write(descriptor, data[offset:])
        if written <= 0:
            fail("%s write was short" % label)
        offset += written


def atomic_mutate(path, transform, root, missing_ok=False, create_parent=False,
                  max_output=MAX_FILE_BYTES, max_input=MAX_RECOVERY_BYTES,
                  label="artifact index", preserve_existing=False,
                  expected_exists=None):
    """Copy-on-write one repo-bound file; failure leaves the old bytes intact."""
    target = canonical_repo_path(root, path, label, create_parent)
    parent = os.path.dirname(target)
    name = os.path.basename(target)
    ancestry = component_snapshot(root, target, label)

    # Native Windows Python cannot open or fsync directory descriptors. Its
    # pathname branch retains the same component/leaf/CAS checks and uses a
    # same-directory temp plus os.replace; POSIX additionally anchors every
    # leaf operation to the already-open physical parent.
    directory = None
    if os.name != "nt":
        try:
            parent_before = os.lstat(parent)
            directory = os.open(parent, os.O_RDONLY | getattr(os, "O_CLOEXEC", 0))
        except OSError as exc:
            fail("cannot anchor %s parent: %s" % (label, exc))
        anchored = os.fstat(directory)
        if (stat.S_ISLNK(parent_before.st_mode) or is_reparse(parent_before) or
                not same_file(parent_before, anchored)):
            os.close(directory)
            fail("%s parent changed before mutation" % label)

    temp_name = ""
    temp_path = ""
    temp_exists = False
    try:
        before = None
        descriptor = None
        flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_CLOEXEC", 0)
        flags |= getattr(os, "O_NOFOLLOW", 0)
        try:
            try:
                if directory is None:
                    descriptor = os.open(target, flags)
                else:
                    descriptor = os.open(name, flags, dir_fd=directory)
            except FileNotFoundError:
                if not missing_ok:
                    fail("%s does not exist" % label)
            except OSError as exc:
                fail("cannot open %s safely: %s" % (label, exc))
            if descriptor is None:
                old = b""
            else:
                before = os.fstat(descriptor)
                if not safe_regular(before):
                    fail("%s must be a regular non-symlink, non-hard-linked file" % label)
                old = _read_descriptor(descriptor, max_input, label)
                after_read = os.fstat(descriptor)
                if not _snapshot_matches(before, after_read):
                    fail("%s changed during mutation read" % label)
        finally:
            if descriptor is not None:
                os.close(descriptor)

        if expected_exists is True and before is None:
            fail("%s disappeared before mutation" % label)
        if expected_exists is False and before is not None:
            fail("%s appeared before creation" % label)

        if before is not None and preserve_existing:
            return old
        new = transform(old)
        if not isinstance(new, bytes):
            fail("%s mutation must return bytes" % label)
        if before is not None and new == old:
            return old
        if len(new) > max_output:
            fail("%s exceeds the %d-byte durable evidence limit" % (label, max_output))

        # Replacement permissions alone are not enough. A read-only ledger is
        # an intentional pending-receipt failure signal: probe O_WRONLY without
        # O_TRUNC, verify the same inode, and close before publishing the temp.
        if before is not None:
            if (stat.S_IMODE(before.st_mode) & 0o222) == 0:
                fail("%s is not writable" % label)
            probe_flags = (os.O_WRONLY | getattr(os, "O_BINARY", 0) |
                           getattr(os, "O_CLOEXEC", 0) |
                           getattr(os, "O_NOFOLLOW", 0))
            try:
                if directory is None:
                    probe = os.open(target, probe_flags)
                else:
                    probe = os.open(name, probe_flags, dir_fd=directory)
            except OSError as exc:
                fail("cannot open %s for a non-truncating write probe: %s" %
                     (label, exc))
            try:
                probed = os.fstat(probe)
                if not safe_regular(probed) or not _snapshot_matches(before, probed):
                    fail("%s changed before the write probe" % label)
            finally:
                os.close(probe)

        components_unchanged(ancestry, label)
        temp_name = ".%s.tmp.%d.%s" % (name, os.getpid(), uuid.uuid4().hex)
        temp_path = os.path.join(parent, temp_name)
        write_flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0)
        write_flags |= getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        try:
            if directory is None:
                temp_descriptor = os.open(temp_path, write_flags, 0o600)
            else:
                temp_descriptor = os.open(temp_name, write_flags, 0o600, dir_fd=directory)
            temp_exists = True
        except OSError as exc:
            fail("cannot create %s temp safely: %s" % (label, exc))
        try:
            _write_all(temp_descriptor, new, label)
            if before is not None:
                try:
                    os.fchmod(temp_descriptor, stat.S_IMODE(before.st_mode))
                except (AttributeError, OSError):
                    pass
            os.fsync(temp_descriptor)
        finally:
            os.close(temp_descriptor)

        components_unchanged(ancestry, label)
        # Compare the named leaf to the snapshot immediately before replace.
        try:
            named = (os.lstat(target) if directory is None else
                     os.stat(name, dir_fd=directory, follow_symlinks=False))
        except FileNotFoundError:
            named = None
        except OSError as exc:
            fail("%s pathname changed before replace: %s" % (label, exc))
        if before is None:
            if named is not None:
                fail("%s appeared during mutation" % label)
        elif named is None or not safe_regular(named) or not _snapshot_matches(before, named):
            fail("%s pathname changed before replace" % label)

        if directory is None:
            os.replace(temp_path, target)
        else:
            os.replace(temp_name, name, src_dir_fd=directory, dst_dir_fd=directory)
        temp_exists = False
        if directory is not None:
            try:
                os.fsync(directory)
            except OSError as exc:
                if exc.errno not in (errno.EBADF, errno.EINVAL, errno.ENOTSUP):
                    raise
        return new
    finally:
        if temp_exists:
            try:
                if directory is None:
                    os.unlink(temp_path)
                else:
                    os.unlink(temp_name, dir_fd=directory)
            except OSError:
                pass
        if directory is not None:
            os.close(directory)


def checked_parent(path, label=None):
    target = os.path.abspath(path)
    name = label or os.path.basename(target)
    if (not name or len(name) > 80 or
            any(char in name for char in "\r\n\t")):
        fail("durable writer label must be 1..80 plain-text characters")
    parent = os.path.dirname(target)
    if not parent or not os.path.isdir(parent) or os.path.islink(parent):
        fail("%s parent must be a real directory" % name)
    # An ancestry symlink (a linked /tmp, a linked home) is the host's layout,
    # not an escape: anchor on the physical directory instead of refusing it.
    # The leaf guarantees stay put — the named parent may not itself be a
    # link, and the leaf is opened O_NOFOLLOW against an anchored handle.
    parent = os.path.realpath(parent)
    return os.path.join(parent, os.path.basename(target)), parent


def check_existing(path, missing_ok, label="progress.jsonl"):
    target, _ = checked_parent(path, label)
    try:
        info = os.lstat(target)
    except FileNotFoundError:
        if missing_ok:
            return
        fail("%s does not exist" % label)
    except OSError as exc:
        fail("cannot inspect %s: %s" % (label, exc))
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        fail("%s must be a regular non-symlink file" % label)


def _append_windows(target, parent, data, label, capture):
    """Pathname implementation for native Windows (no directory handles)."""
    try:
        parent_before = os.lstat(parent)
    except OSError as exc:
        fail("cannot inspect %s parent: %s" % (label, exc))
    if (stat.S_ISLNK(parent_before.st_mode) or is_reparse(parent_before) or
            not stat.S_ISDIR(parent_before.st_mode)):
        fail("%s parent must be a real directory" % label)
    check_existing(target, True, label)
    access = os.O_RDWR if capture else os.O_WRONLY
    flags = access | os.O_APPEND | os.O_CREAT | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(target, flags, 0o600)
    except OSError as exc:
        fail("cannot open %s safely: %s" % (label, exc))
    contents = None
    try:
        opened = os.fstat(descriptor)
        if not safe_regular(opened):
            fail("%s must be a regular non-symlink, non-hard-linked file" % label)
        if opened.st_size > MAX_FILE_BYTES - len(data):
            fail("%s exceeds the %d-byte durable evidence limit" %
                 (label, MAX_FILE_BYTES))
        try:
            parent_named = os.lstat(parent)
            named_before = os.lstat(target)
        except OSError as exc:
            fail("%s pathname changed before append: %s" % (label, exc))
        if (stat.S_ISLNK(parent_named.st_mode) or is_reparse(parent_named) or
                not same_file(parent_before, parent_named) or
                not safe_regular(named_before) or not same_file(opened, named_before)):
            fail("%s pathname changed before append" % label)
        written = os.write(descriptor, data)
        if written != len(data):
            fail("%s append was short" % label)
        os.fsync(descriptor)
        try:
            parent_after = os.lstat(parent)
            named_after = os.lstat(target)
        except OSError as exc:
            fail("%s pathname changed during append: %s" % (label, exc))
        if (stat.S_ISLNK(parent_after.st_mode) or is_reparse(parent_after) or
                not same_file(parent_before, parent_after) or
                not safe_regular(named_after) or not same_file(opened, named_after)):
            fail("%s pathname changed during append" % label)
        if capture:
            os.lseek(descriptor, 0, os.SEEK_SET)
            contents = _read_descriptor(descriptor, MAX_FILE_BYTES, label)
    finally:
        os.close(descriptor)
    return contents


def append(path, data, label="progress.jsonl", capture=False):
    if (not label or len(label) > 80 or
            any(char in label for char in "\r\n\t")):
        fail("durable writer label must be 1..80 plain-text characters")
    if not data or len(data) > MAX_ROW_BYTES or not data.endswith(b"\n"):
        fail("%s row must be 1..%d bytes and end in newline" % (label, MAX_ROW_BYTES))
    if b"\x00" in data or data.count(b"\n") != 1:
        fail("%s append requires exactly one text row" % label)

    target, parent = checked_parent(path, label)
    basename = os.path.basename(target)
    if os.name == "nt":
        return _append_windows(target, parent, data, label, capture)
    try:
        parent_before = os.lstat(parent)
    except OSError as exc:
        fail("cannot inspect %s parent: %s" % (label, exc))
    if stat.S_ISLNK(parent_before.st_mode) or not stat.S_ISDIR(parent_before.st_mode):
        fail("%s parent must be a real directory" % label)
    try:
        os.chdir(parent)
        directory = os.open(".", os.O_RDONLY | getattr(os, "O_CLOEXEC", 0))
    except OSError as exc:
        fail("cannot anchor %s parent: %s" % (label, exc))
    anchored_parent = os.fstat(directory)
    if not same_file(parent_before, anchored_parent):
        os.close(directory)
        fail("%s parent changed before append" % label)
    try:
        parent_named = os.lstat(parent)
    except OSError as exc:
        os.close(directory)
        fail("%s parent changed before append: %s" % (label, exc))
    if stat.S_ISLNK(parent_named.st_mode) or not same_file(anchored_parent, parent_named):
        os.close(directory)
        fail("%s parent changed before append" % label)

    # Inspect and open the leaf relative to the anchored current-directory
    # handle. Even if another process renames the directory after this check,
    # the write cannot escape to a replacement pathname or symlink target.
    check_existing(target, True, label)
    access = os.O_RDWR if capture else os.O_WRONLY
    flags = access | os.O_APPEND | os.O_CREAT | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(basename, flags, 0o600)
    except OSError as exc:
        os.close(directory)
        if exc.errno in (errno.ELOOP, errno.EMLINK):
            fail("%s must not be a symbolic link" % label)
        fail("cannot open %s safely: %s" % (label, exc))
    contents = None
    try:
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode):
            fail("%s must be a regular file" % label)
        if opened.st_nlink != 1:
            # A second name on this inode means the append would mutate a
            # file outside the ledger's own path — the hard-link twin of the
            # leaf symlink this writer already refuses.
            fail("%s must not be hard-linked" % label)
        if opened.st_size > MAX_FILE_BYTES - len(data):
            fail("%s exceeds the %d-byte durable evidence limit" % (label, MAX_FILE_BYTES))
        try:
            parent_named = os.lstat(parent)
            named_before = os.lstat(target)
        except OSError as exc:
            fail("%s pathname changed before append: %s" % (label, exc))
        if (stat.S_ISLNK(parent_named.st_mode) or
                not same_file(anchored_parent, parent_named) or
                stat.S_ISLNK(named_before.st_mode) or not same_file(opened, named_before)):
            fail("%s pathname changed before append" % label)

        # One O_APPEND write keeps a JSON row indivisible on the supported local
        # filesystems. A short write is an error: retrying would let another
        # writer interleave bytes into the same row.
        written = os.write(descriptor, data)
        if written != len(data):
            fail("%s append was short" % label)
        os.fsync(descriptor)
        try:
            parent_named = os.lstat(parent)
            named_after = os.lstat(target)
        except OSError as exc:
            fail("%s pathname changed during append: %s" % (label, exc))
        if (stat.S_ISLNK(parent_named.st_mode) or
                not same_file(anchored_parent, parent_named) or
                stat.S_ISLNK(named_after.st_mode) or not same_file(opened, named_after)):
            fail("%s pathname changed during append" % label)
        if capture:
            os.lseek(descriptor, 0, os.SEEK_SET)
            chunks = []
            remaining = MAX_FILE_BYTES + 1
            while remaining:
                chunk = os.read(descriptor, min(1024 * 1024, remaining))
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
            contents = b"".join(chunks)
            if len(contents) > MAX_FILE_BYTES:
                fail("%s exceeds the %d-byte durable evidence limit" % (label, MAX_FILE_BYTES))
    finally:
        os.close(descriptor)

    # Persist creation/rename metadata where the host permits directory fsync.
    try:
        try:
            os.fsync(directory)
        except OSError as exc:
            if exc.errno not in (errno.EBADF, errno.EINVAL, errno.ENOTSUP):
                raise
    finally:
        os.close(directory)
    return contents


def write(path, data, label=None, max_bytes=MAX_ROW_BYTES):
    """Publish a whole body file with the same no-follow discipline as append.

    Temp-then-rename inside the anchored physical directory: the rename is
    cwd-relative, so a concurrent directory swap cannot redirect it, and a
    symlink planted at the target name is replaced by the rename instead of
    being followed — the content can only ever land in the real directory.
    """
    target, parent = checked_parent(path, label)
    name = os.path.basename(target)
    display = label or name
    if not data or len(data) > max_bytes:
        if label:
            fail("%s must be 1..%d bytes" % (label, max_bytes))
        fail("durable body must be 1..%d bytes" % max_bytes)
    if os.name == "nt":
        try:
            parent_before = os.lstat(parent)
        except OSError as exc:
            fail("cannot inspect %s parent: %s" % (display, exc))
        if (stat.S_ISLNK(parent_before.st_mode) or is_reparse(parent_before) or
                not stat.S_ISDIR(parent_before.st_mode)):
            fail("%s parent must be a real directory" % display)
        tmp_name = ".%s.tmp.%d.%s" % (name, os.getpid(), uuid.uuid4().hex)
        tmp_path = os.path.join(parent, tmp_name)
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0)
        flags |= getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        try:
            descriptor = os.open(tmp_path, flags, 0o600)
        except OSError as exc:
            fail("cannot create %s temp safely: %s" % (display, exc))
        try:
            try:
                _write_all(descriptor, data, display)
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
            parent_after = os.lstat(parent)
            if (stat.S_ISLNK(parent_after.st_mode) or is_reparse(parent_after) or
                    not same_file(parent_before, parent_after)):
                fail("%s parent changed before write" % display)
            os.replace(tmp_path, target)
            tmp_path = ""
        except BaseException:
            try:
                if tmp_path:
                    os.unlink(tmp_path)
            except OSError:
                pass
            raise
        return
    try:
        parent_before = os.lstat(parent)
    except OSError as exc:
        fail("cannot inspect %s parent: %s" % (display, exc))
    if stat.S_ISLNK(parent_before.st_mode) or not stat.S_ISDIR(parent_before.st_mode):
        fail("%s parent must be a real directory" % display)
    try:
        os.chdir(parent)
        directory = os.open(".", os.O_RDONLY | getattr(os, "O_CLOEXEC", 0))
    except OSError as exc:
        fail("cannot anchor %s parent: %s" % (display, exc))
    if not same_file(parent_before, os.fstat(directory)):
        os.close(directory)
        fail("%s parent changed before write" % display)
    tmp_name = ".%s.tmp.%d" % (name, os.getpid())
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(tmp_name, flags, 0o600)
    except OSError as exc:
        os.close(directory)
        fail("cannot create %s temp safely: %s" % (display, exc))
    try:
        try:
            written = os.write(descriptor, data)
            if written != len(data):
                fail("%s write was short" % display)
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        # os.replace, not os.rename: POSIX rename already overwrites, but on
        # Windows os.rename refuses an existing destination — and this path's
        # whole job is replacing a ledger that exists.
        os.replace(tmp_name, name)
    except BaseException:
        try:
            os.unlink(tmp_name)
        except OSError:
            pass
        os.close(directory)
        raise
    try:
        try:
            os.fsync(directory)
        except OSError as exc:
            if exc.errno not in (errno.EBADF, errno.EINVAL, errno.ENOTSUP):
                raise
    finally:
        os.close(directory)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--label")
    parser.add_argument("action", choices=("check", "append", "write"))
    parser.add_argument("path")
    args = parser.parse_args()
    if args.action == "check":
        check_existing(args.path, True, args.label or "progress.jsonl")
        return
    data = sys.stdin.buffer.read(MAX_ROW_BYTES + 1)
    if args.action == "write":
        write(args.path, data, args.label)
        return
    append(args.path, data, args.label or "progress.jsonl")


if __name__ == "__main__":
    main()
