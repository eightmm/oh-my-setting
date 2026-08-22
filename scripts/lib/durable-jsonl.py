#!/usr/bin/env python3
"""Fail-closed JSONL checks and durable appends without following symlinks."""

import argparse
import errno
import os
import stat
import sys


MAX_ROW_BYTES = 1024 * 1024
MAX_FILE_BYTES = 16 * 1024 * 1024


def fail(message):
    print("error: %s" % message, file=sys.stderr)
    raise SystemExit(2)


def same_file(left, right):
    return left.st_dev == right.st_dev and left.st_ino == right.st_ino


def checked_parent(path, label=None):
    target = os.path.abspath(path)
    name = label or os.path.basename(target)
    if (not name or len(name) > 80 or
            any(char in name for char in "\r\n\t")):
        fail("durable writer label must be 1..80 plain-text characters")
    parent = os.path.dirname(target)
    if not parent or not os.path.isdir(parent) or os.path.islink(parent):
        fail("%s parent must be a real directory" % name)
    if os.path.realpath(parent) != parent:
        fail("%s parent must not cross a symlink boundary" % name)
    return target, parent


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
        os.rename(tmp_name, name)
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
