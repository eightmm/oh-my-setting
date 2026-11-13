"""Raw cancellation proofs; temporary compatibility until the native plan port.

Historical lookup may use an exact immutable digest. New cancellations and
explicit captures always read the declared source afresh, never this catalog.
"""

import argparse
import contextlib
import errno
import hashlib
import json
import os
import re
import runpy
import stat
import uuid


MAX_BYTES = 64 * 1024
CATALOG = '.oms/plan/decision-artifacts-v1'
D = runpy.run_path(os.path.join(os.path.dirname(__file__), 'durable-jsonl.py'))
PRIVATE_GROUP = runpy.run_path(os.path.join(os.path.dirname(__file__), 'main_reviewer.py'))['_private_group']
DIR_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
READ_FLAGS = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC


def relative(value):
    if (not isinstance(value, str) or not value or len(value) > 240 or
            value.startswith('/') or '\\' in value or
            any(ord(c) < 32 or ord(c) == 127 for c in value) or
            any(p in ('', '.', '..') for p in value.split('/'))):
        raise ValueError('decision artifact path must be a bounded repo-relative path')
    return value


def digest(value):
    if not isinstance(value, str) or not re.fullmatch(r'[0-9a-f]{64}', value):
        raise ValueError('decision artifact requires a lowercase SHA-256')
    return value


def trusted(info, directory=False, private=False):
    mode = stat.S_IMODE(info.st_mode)
    try:
        unsafe_write = mode & 0o002 or (mode & 0o020 and not PRIVATE_GROUP(info.st_gid, os.getuid()))
    except KeyError:
        raise ValueError('decision artifact group ownership is unproven') from None
    if (info.st_uid != os.getuid() or
            (directory and (not stat.S_ISDIR(info.st_mode) or mode & 0o500 != 0o500)) or
            (not directory and (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or
                                not mode & 0o400)) or
            unsafe_write or (private and mode != (0o700 if directory else 0o600))):
        raise ValueError('decision artifact has unsafe ownership, permissions or links')


def sync_directory(fd):
    try:
        os.fsync(fd)
    except OSError as exc:
        # Same portability floor as the existing durable-state writer.
        if exc.errno not in (errno.EBADF, errno.EINVAL, errno.ENOTSUP):
            raise


@contextlib.contextmanager
def parent(repo, rel, catalog=False, create=False):
    """Anchor every in-repo component; only the private catalog leaf is created."""
    relative(rel)
    root = os.path.realpath(repo)
    opened = []
    try:
        fd = os.open(root, DIR_FLAGS)
        opened.append((fd, None, root, False))
        trusted(os.fstat(fd), directory=True)
        parts = rel.split('/')[:-1]
        for index, name in enumerate(parts):
            private = catalog and index == len(parts) - 1
            if private and create:
                try:
                    os.mkdir(name, 0o700, dir_fd=fd)
                except FileExistsError:
                    pass
                sync_directory(fd)
            child = os.open(name, DIR_FLAGS, dir_fd=fd)
            opened.append((child, fd, name, private))
            trusted(os.fstat(child), directory=True, private=private)
            fd = child

        def recheck():
            for held, ancestor, name, private in opened:
                info = os.fstat(held)
                named = os.stat(name, dir_fd=ancestor, follow_symlinks=False)
                trusted(info, directory=True, private=private)
                trusted(named, directory=True, private=private)
                if not D['same_file'](info, named):
                    raise ValueError('decision artifact parent changed during access')

        recheck()
        yield fd, recheck
        recheck()
    finally:
        for fd, _, _, _ in reversed(opened):
            os.close(fd)


def read_at(directory, name, private=False, missing=False, durable=False):
    try:
        fd = os.open(name, READ_FLAGS, dir_fd=directory)
    except FileNotFoundError:
        if missing:
            return None
        raise ValueError('cancellation decision artifact is missing') from None
    try:
        before = os.fstat(fd)
        trusted(before, private=private)
        chunks = []
        remaining = MAX_BYTES + 1
        while remaining:
            chunk = os.read(fd, remaining)
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        raw = b''.join(chunks)
        if len(raw) > MAX_BYTES:
            raise ValueError('cancellation decision artifact exceeds 65536 bytes')
        after = os.fstat(fd)
        named = os.stat(name, dir_fd=directory, follow_symlinks=False)
        trusted(after, private=private)
        trusted(named, private=private)
        if (not D['_snapshot_matches'](before, after) or
                not D['_snapshot_matches'](after, named)):
            raise ValueError('cancellation decision artifact changed during read')
        if durable:
            os.fsync(fd)
        return raw
    finally:
        os.close(fd)


def source_bytes(repo, rel, missing=False):
    with parent(repo, rel) as (directory, _):
        # Missing final file permits history lookup; missing or unsafe ancestors
        # never do. A dangling leaf symlink is an unsafe open, not absence.
        return read_at(directory, rel.split('/')[-1], missing=missing)


def historical_bytes(repo, rel, expected):
    relative(rel)
    digest(expected)
    raw = source_bytes(repo, rel, missing=True)
    if raw is not None and hashlib.sha256(raw).hexdigest() == expected:
        return raw
    try:
        with parent(repo, CATALOG + '/' + expected, catalog=True) as (directory, _):
            proof = read_at(directory, expected, private=True, missing=True)
    except FileNotFoundError:
        proof = None
    if proof is None or hashlib.sha256(proof).hexdigest() != expected:
        raise ValueError('cancellation decision artifact digest changed or exact snapshot is missing/corrupt')
    return proof


def snapshot_decision(repo, rel, expected):
    relative(rel)
    digest(expected)
    raw = source_bytes(repo, rel)
    if hashlib.sha256(raw).hexdigest() != expected:
        raise ValueError('decision artifact digest changed or is not the reviewed SHA-256')
    target = CATALOG + '/' + expected
    with parent(repo, target, catalog=True, create=True) as (directory, recheck):
        existing = read_at(directory, expected, private=True, missing=True, durable=True)
        created = False
        if existing is None:
            temporary = '.' + expected + '.tmp.' + uuid.uuid4().hex
            flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC
            fd = os.open(temporary, flags, 0o600, dir_fd=directory)
            try:
                try:
                    os.fchmod(fd, 0o600)
                    D['_write_all'](fd, raw, 'decision snapshot')
                    os.fsync(fd)
                finally:
                    os.close(fd)
                recheck()
                try:
                    os.link(temporary, expected, src_dir_fd=directory, dst_dir_fd=directory,
                            follow_symlinks=False)
                    created = True
                except FileExistsError:
                    pass
            finally:
                os.unlink(temporary, dir_fd=directory)
            # A kill between link and unlink leaves two links and is refused.
            existing = read_at(directory, expected, private=True, durable=True)
        if existing != raw or hashlib.sha256(existing).hexdigest() != expected:
            raise ValueError('decision snapshot is occupied by different or corrupt bytes')
        sync_directory(directory)
    return {'snapshot_path': target, 'sha256': expected, 'created': created}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--decision-artifact', required=True)
    parser.add_argument('--expected-decision-artifact-sha256', required=True)
    args = parser.parse_args()
    if (os.environ.get('OMS_HARNESS_CHILD') == '1' or
            os.environ.get('OMS_HARNESS_DELEGATE_DEPTH', '0') not in ('', '0')):
        parser.error('snapshot-decision is parent-only')
    try:
        result = snapshot_decision(args.repo, args.decision_artifact,
                                   args.expected_decision_artifact_sha256)
    except (ValueError, OSError) as exc:
        parser.exit(2, 'error: %s\n' % exc)
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    main()
