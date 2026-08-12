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


def checked_parent(path):
    target = os.path.abspath(path)
    parent = os.path.dirname(target)
    if not parent or not os.path.isdir(parent) or os.path.islink(parent):
        fail("progress.jsonl parent must be a real directory")
    if os.path.realpath(parent) != parent:
        fail("progress.jsonl parent must not cross a symlink boundary")
    return target, parent


def check_existing(path, missing_ok):
    target, _ = checked_parent(path)
    try:
        info = os.lstat(target)
    except FileNotFoundError:
        if missing_ok:
            return
        fail("progress.jsonl does not exist")
    except OSError as exc:
        fail("cannot inspect progress.jsonl: %s" % exc)
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        fail("progress.jsonl must be a regular non-symlink file")


def append(path, data):
    if not data or len(data) > MAX_ROW_BYTES or not data.endswith(b"\n"):
        fail("progress.jsonl row must be 1..%d bytes and end in newline" % MAX_ROW_BYTES)
    if b"\x00" in data or data.count(b"\n") != 1:
        fail("progress.jsonl append requires exactly one text row")

    target, parent = checked_parent(path)
    basename = os.path.basename(target)
    try:
        parent_before = os.lstat(parent)
    except OSError as exc:
        fail("cannot inspect progress.jsonl parent: %s" % exc)
    if stat.S_ISLNK(parent_before.st_mode) or not stat.S_ISDIR(parent_before.st_mode):
        fail("progress.jsonl parent must be a real directory")
    try:
        os.chdir(parent)
        directory = os.open(".", os.O_RDONLY | getattr(os, "O_CLOEXEC", 0))
    except OSError as exc:
        fail("cannot anchor progress.jsonl parent: %s" % exc)
    anchored_parent = os.fstat(directory)
    if not same_file(parent_before, anchored_parent):
        os.close(directory)
        fail("progress.jsonl parent changed before append")
    try:
        parent_named = os.lstat(parent)
    except OSError as exc:
        os.close(directory)
        fail("progress.jsonl parent changed before append: %s" % exc)
    if stat.S_ISLNK(parent_named.st_mode) or not same_file(anchored_parent, parent_named):
        os.close(directory)
        fail("progress.jsonl parent changed before append")

    # Inspect and open the leaf relative to the anchored current-directory
    # handle. Even if another process renames the directory after this check,
    # the write cannot escape to a replacement pathname or symlink target.
    check_existing(target, True)
    flags = os.O_WRONLY | os.O_APPEND | os.O_CREAT | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(basename, flags, 0o600)
    except OSError as exc:
        os.close(directory)
        if exc.errno in (errno.ELOOP, errno.EMLINK):
            fail("progress.jsonl must not be a symbolic link")
        fail("cannot open progress.jsonl safely: %s" % exc)
    try:
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode):
            fail("progress.jsonl must be a regular file")
        if opened.st_size > MAX_FILE_BYTES - len(data):
            fail("progress.jsonl exceeds the %d-byte durable evidence limit" % MAX_FILE_BYTES)
        try:
            parent_named = os.lstat(parent)
            named_before = os.lstat(target)
        except OSError as exc:
            fail("progress.jsonl pathname changed before append: %s" % exc)
        if (stat.S_ISLNK(parent_named.st_mode) or
                not same_file(anchored_parent, parent_named) or
                stat.S_ISLNK(named_before.st_mode) or not same_file(opened, named_before)):
            fail("progress.jsonl pathname changed before append")

        # One O_APPEND write keeps a JSON row indivisible on the supported local
        # filesystems. A short write is an error: retrying would let another
        # writer interleave bytes into the same row.
        written = os.write(descriptor, data)
        if written != len(data):
            fail("progress.jsonl append was short")
        os.fsync(descriptor)
        try:
            parent_named = os.lstat(parent)
            named_after = os.lstat(target)
        except OSError as exc:
            fail("progress.jsonl pathname changed during append: %s" % exc)
        if (stat.S_ISLNK(parent_named.st_mode) or
                not same_file(anchored_parent, parent_named) or
                stat.S_ISLNK(named_after.st_mode) or not same_file(opened, named_after)):
            fail("progress.jsonl pathname changed during append")
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


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("check", "append"))
    parser.add_argument("path")
    args = parser.parse_args()
    if args.action == "check":
        check_existing(args.path, True)
        return
    data = sys.stdin.buffer.read(MAX_ROW_BYTES + 1)
    append(args.path, data)


if __name__ == "__main__":
    main()
