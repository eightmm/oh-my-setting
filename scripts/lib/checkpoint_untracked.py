"""Untracked files of a checkpoint: capture into a size-capped tar, and write back only what is missing.

A checkpoint's patches cover tracked state. The files Git does not track and
does not ignore are what `git clean` deletes for good, so `checkpoint create
--untracked` copies them here. Restore never replaces a file that exists: it
writes a captured file only where nothing is at that path now.

Commands (all print one JSON object):
  capture REPO DIR             write DIR/untracked.tar
  match ROOT LABEL HEAD INDEX WORKTREE FINGERPRINT   id of an identical checkpoint, or ""
  restore REPO TAR [--apply]   count, and with --apply write, the missing files
"""

import hashlib
import io
import json
import os
import stat
import subprocess
import sys
import tarfile

MAX_FILE_BYTES = 8 * 1024 * 1024
MAX_TOTAL_BYTES = 64 * 1024 * 1024
MAX_FILES = 5000
SKIP_TOP = (".git", ".oms")


def untracked_names(repo):
    done = subprocess.run(
        ["git", "-C", repo, "ls-files", "--others", "--exclude-standard", "-z", "--"],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL, check=False)
    if done.returncode:
        raise OSError("could not list untracked files")
    names = [os.fsdecode(raw) for raw in done.stdout.split(b"\0") if raw]
    return sorted(name for name in names if safe_name(name))


def safe_name(name):
    if not name or name.startswith("/") or "\n" in name or "\r" in name:
        return False
    parts = name.split("/")
    if any(part in ("", ".", "..") for part in parts):
        return False
    return parts[0] not in SKIP_TOP


def capture(repo, directory):
    """Write DIR/untracked.tar from the regular untracked files that fit the caps.

    The fingerprint covers every listed name: a captured file by its content
    digest, an omitted one by the word "omitted", so two captures compare equal
    only when the same files hold the same bytes."""
    total = captured = omitted = 0
    lines = []
    path = os.path.join(directory, "untracked.tar")
    with tarfile.open(path, "w", format=tarfile.PAX_FORMAT) as archive:
        for name in untracked_names(repo):
            source = os.path.join(repo, name)
            try:
                info = os.lstat(source)
            except OSError:
                continue
            if (not stat.S_ISREG(info.st_mode) or info.st_size > MAX_FILE_BYTES
                    or total + info.st_size > MAX_TOTAL_BYTES or captured >= MAX_FILES):
                omitted += 1
                lines.append("%s\0omitted\n" % name)
                continue
            try:
                fd = os.open(source, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
                with os.fdopen(fd, "rb") as handle:
                    data = handle.read(MAX_FILE_BYTES + 1)
            except OSError:
                omitted += 1
                lines.append("%s\0omitted\n" % name)
                continue
            if len(data) > MAX_FILE_BYTES or total + len(data) > MAX_TOTAL_BYTES:
                omitted += 1
                lines.append("%s\0omitted\n" % name)
                continue
            member = tarfile.TarInfo(name)
            member.size = len(data)
            member.mode = stat.S_IMODE(info.st_mode)
            member.mtime = int(info.st_mtime)
            member.uid = member.gid = 0
            member.uname = member.gname = ""
            archive.addfile(member, io.BytesIO(data))
            total += len(data)
            captured += 1
            lines.append("%s\0%s\n" % (name, hashlib.sha256(data).hexdigest()))
    with open(path, "rb") as handle:
        digest = hashlib.sha256(handle.read()).hexdigest()
    os.chmod(path, 0o600)
    return {
        "untracked_captured": captured,
        "untracked_omitted": omitted,
        "untracked_capture_bytes": total,
        "untracked_sha256": digest,
        "untracked_fingerprint": hashlib.sha256(
            "".join(lines).encode("utf-8", "surrogateescape")).hexdigest(),
    }


def match(root, label, head, index_hash, worktree_hash, fingerprint):
    """Newest checkpoint with this label that already holds exactly this state."""
    try:
        names = sorted(os.listdir(root), reverse=True)
    except OSError:
        return ""
    for name in names:
        if not name.startswith("cp-"):
            continue
        directory = os.path.join(root, name)
        meta_path = os.path.join(directory, "meta.json")
        if os.path.islink(directory) or os.path.islink(meta_path):
            continue
        try:
            with open(meta_path, encoding="utf-8") as handle:
                meta = json.load(handle)
        except (OSError, ValueError):
            continue
        if not isinstance(meta, dict) or meta.get("id") != name or meta.get("label") != label:
            continue
        if (meta.get("head") == head and meta.get("index_sha256") == index_hash
                and meta.get("worktree_sha256") == worktree_hash
                and (meta.get("untracked_fingerprint") or "-") == fingerprint):
            return name
    return ""


def _parent_is_plain(repo, name):
    """Every existing directory between the repository and the file is a real directory."""
    current = repo
    for part in name.split("/")[:-1]:
        current = os.path.join(current, part)
        try:
            info = os.lstat(current)
        except FileNotFoundError:
            return True
        except OSError:
            return False
        if not stat.S_ISDIR(info.st_mode):
            return False
    return True


def restore(repo, archive_path, apply):
    missing = present = written = refused = 0
    with tarfile.open(archive_path, "r") as archive:
        for member in archive:
            name = member.name
            if not member.isreg() or not safe_name(name) or member.size > MAX_FILE_BYTES:
                refused += 1
                continue
            target = os.path.join(repo, name)
            if os.path.lexists(target):
                present += 1
                continue
            if not _parent_is_plain(repo, name):
                refused += 1
                continue
            missing += 1
            if not apply:
                continue
            source = archive.extractfile(member)
            if source is None:
                refused += 1
                continue
            data = source.read()
            try:
                os.makedirs(os.path.dirname(target) or repo, exist_ok=True)
                if not _parent_is_plain(repo, name):
                    raise OSError("unsafe parent")
                flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
                fd = os.open(target, flags, member.mode & 0o777 or 0o600)
                with os.fdopen(fd, "wb") as handle:
                    handle.write(data)
                os.utime(target, (member.mtime, member.mtime))
            except OSError:
                refused += 1
                continue
            written += 1
    return {"missing": missing, "present": present, "written": written, "refused": refused,
            "applied": bool(apply)}


def main(argv):
    try:
        if len(argv) == 3 and argv[0] == "capture":
            report = capture(argv[1], argv[2])
        elif len(argv) == 7 and argv[0] == "match":
            report = {"id": match(*argv[1:])}
        elif len(argv) in (3, 4) and argv[0] == "restore" and argv[3:] in ([], ["--apply"]):
            report = restore(argv[1], argv[2], len(argv) == 4)
        else:
            print("usage: checkpoint_untracked.py capture|match|restore ...", file=sys.stderr)
            return 2
    except (OSError, tarfile.TarError, ValueError) as error:
        print("error: %s" % error, file=sys.stderr)
        return 1
    print(json.dumps(report, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
