#!/usr/bin/env python3
"""Atomic local intent for an exact create-only Draft PR publication."""

from __future__ import annotations

import argparse
import datetime
import errno
import hashlib
import json
import os
import re
import secrets
import stat
import tempfile
from pathlib import Path
from typing import Any, Dict, NoReturn


MAX_INTENT_BYTES = 256 * 1024
INTENT_KEYS = {
    "schema", "kind", "phase", "created_at", "updated_at", "repo_id",
    "head_sha", "tree_sha", "branch", "base", "base_sha", "remote",
    "repo_slug", "fetch_url", "push_url", "viewer_login", "spec_sha256",
    "verify", "verify_sha256", "title", "title_sha256", "body",
    "body_sha256", "draft", "create_only", "merge", "release", "pr_url",
    "pr_number", "push_attempted",
}
FIELDS = {
    "phase",
    "repo_id",
    "head_sha",
    "tree_sha",
    "branch",
    "base",
    "base_sha",
    "remote",
    "repo_slug",
    "fetch_url",
    "push_url",
    "viewer_login",
    "spec_sha256",
    "verify",
    "verify_sha256",
    "title",
    "title_sha256",
    "body_sha256",
    "pr_url",
    "pr_number",
    "push_attempted",
}
MUTABLE_FIELDS = {
    "phase", "created_at", "updated_at", "pr_url", "pr_number",
    "push_attempted",
}


def fail(message: str) -> NoReturn:
    print("error: %s" % message, file=os.sys.stderr)
    raise SystemExit(2)


def now() -> str:
    return (
        datetime.datetime.now(datetime.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )


def repo_id(repo: str) -> str:
    return hashlib.sha256(str(Path(repo).resolve()).encode("utf-8")).hexdigest()


def digest(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def immutable_digest(row: Dict[str, Any]) -> str:
    frozen = {key: row[key] for key in sorted(set(row) - MUTABLE_FIELDS)}
    encoded = json.dumps(
        frozen, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def state_digest(row: Dict[str, Any]) -> str:
    encoded = json.dumps(
        row, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def fsync_parent(path: Path) -> None:
    """Durably publish a directory entry, ignoring only unsupported fsync."""
    unsupported = {
        errno.EINVAL,
        getattr(errno, "ENOTSUP", errno.EINVAL),
        getattr(errno, "EOPNOTSUPP", errno.EINVAL),
    }
    if os.name == "nt":
        unsupported.update({errno.EACCES, errno.EPERM, errno.EBADF})
    try:
        flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
        directory_fd = os.open(str(path.parent), flags)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    except OSError as exc:
        if exc.errno in unsupported:
            return
        fail("cannot durably sync publication directory: %s" % exc)


def fsync_directory_fd(directory_fd: int) -> None:
    unsupported = {
        errno.EINVAL,
        getattr(errno, "ENOTSUP", errno.EINVAL),
        getattr(errno, "EOPNOTSUPP", errno.EINVAL),
    }
    try:
        os.fsync(directory_fd)
    except OSError as exc:
        if exc.errno in unsupported:
            return
        fail("cannot durably sync guarded publication directory: %s" % exc)


def directory_identity(info: os.stat_result) -> str:
    return "%x:%x" % (info.st_dev, info.st_ino)


def checked_publish_fd(directory_fd: int, expected_identity: str) -> os.stat_result:
    try:
        info = os.fstat(directory_fd)
    except OSError as exc:
        fail("cannot inspect guarded publication directory: %s" % exc)
    if (
        not stat.S_ISDIR(info.st_mode)
        or directory_identity(info) != expected_identity
    ):
        fail("guarded publication directory identity changed")
    if hasattr(os, "getuid"):
        if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) & 0o077:
            fail("guarded publication directory must be private")
    return info


def atomic_block_at(
    directory_fd: int, marker_name: str, row: Dict[str, Any]
) -> None:
    """Replace one terminal marker through a pre-verifier directory handle."""
    encoded = (json.dumps(row, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
    temp_name = ".%s.%s" % (marker_name, secrets.token_hex(8))
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    flags |= getattr(os, "O_NOFOLLOW", 0)
    temp_fd = -1
    try:
        temp_fd = os.open(temp_name, flags, 0o600, dir_fd=directory_fd)
        offset = 0
        while offset < len(encoded):
            offset += os.write(temp_fd, encoded[offset:])
        os.fsync(temp_fd)
        os.close(temp_fd)
        temp_fd = -1
        os.replace(
            temp_name,
            marker_name,
            src_dir_fd=directory_fd,
            dst_dir_fd=directory_fd,
        )
        fsync_directory_fd(directory_fd)
    except OSError as exc:
        fail("cannot write guarded publication revocation: %s" % exc)
    finally:
        if temp_fd >= 0:
            os.close(temp_fd)
        try:
            os.unlink(temp_name, dir_fd=directory_fd)
        except FileNotFoundError:
            pass
        except OSError:
            pass


def target_path(
    repo: str, value: str, must_exist: bool, preserve_final: bool = False
) -> Path:
    repo_root = Path(repo).resolve()
    oms_root = repo_root / ".oms"
    root = oms_root / "publish"
    for directory, label in ((oms_root, ".oms"), (root, ".oms/publish")):
        if directory.is_symlink():
            fail("%s must not be a symlink" % label)
        if directory.exists() and not directory.is_dir():
            fail("%s must be a directory" % label)
    path = Path(value)
    if not path.is_absolute():
        path = repo_root / path
    # Normalize dot segments without following symlinks, then reject every
    # existing symlink component under the private publication root.
    lexical = Path(os.path.abspath(str(path)))
    try:
        relative = lexical.relative_to(root)
    except ValueError:
        fail("intent must stay under .oms/publish")
    cursor = root
    for part in relative.parts[:-1] if relative.parts else ():
        cursor = cursor / part
        if cursor.is_symlink():
            fail("intent ancestors must not be symlinks")
    if must_exist:
        try:
            resolved = lexical.resolve(strict=True)
        except OSError as exc:
            fail("cannot resolve intent: %s" % exc)
        if lexical.is_symlink() or not resolved.is_file():
            fail("intent must be a regular non-symlink file")
    elif preserve_final:
        # A verifier may replace the validated intent itself with a symlink.
        # Revocation must name the original lexical slot, never its new target.
        resolved = lexical
    else:
        resolved = lexical.resolve()
    try:
        resolved.relative_to(root)
    except ValueError:
        fail("intent must stay under .oms/publish")
    if not must_exist:
        # Check containment before creating anything: a rejected helper call
        # must not leave attacker-selected directories outside the repo.
        if not root.exists():
            root.mkdir(parents=True, mode=0o700, exist_ok=True)
        resolved.parent.mkdir(parents=True, exist_ok=True)
    if root.exists():
        info = root.stat()
        if hasattr(os, "getuid"):
            if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) & 0o077:
                fail(".oms/publish must be private to the current user")
    if oms_root.exists() and hasattr(os, "getuid"):
        info = oms_root.stat()
        if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) & 0o022:
            fail(".oms must not be writable by another user")
    if must_exist and hasattr(os, "getuid"):
        info = resolved.stat()
        if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) & 0o077:
            fail("intent must be private to the current user")
    return resolved


def validate_row(row: Dict[str, Any], expected_repo: str) -> Dict[str, Any]:
    if not isinstance(row, dict) or row.get("schema") != 1 or row.get("kind") != "draft-pr-intent":
        fail("intent has an unsupported contract")
    if set(row) != INTENT_KEYS:
        fail("intent fields do not match the exact contract")
    if row.get("repo_id") != repo_id(expected_repo):
        fail("intent belongs to another repository")
    if row.get("phase") not in ("prepared", "pushing", "pushed", "complete"):
        fail("intent phase is invalid")
    for name in ("head_sha", "tree_sha", "base_sha"):
        value = row.get(name)
        if (not isinstance(value, str) or len(value) not in (40, 64) or
                any(ch not in "0123456789abcdef" for ch in value)):
            fail("intent %s is invalid" % name)
    identifier = row.get("repo_id")
    if (not isinstance(identifier, str) or len(identifier) != 64 or
            any(ch not in "0123456789abcdef" for ch in identifier)):
        fail("intent repository id is invalid")
    if row.get("draft") is not True or row.get("create_only") is not True:
        fail("intent is not a create-only draft publication")
    if row.get("merge") is not False or row.get("release") is not False:
        fail("intent carries forbidden merge/release authority")
    if row.get("push_attempted") is not (row["phase"] != "prepared"):
        fail("intent push-attempt state is inconsistent")
    for name, maximum in (("verify", 4096), ("title", 512), ("body", 64 * 1024)):
        value = row.get(name)
        if not isinstance(value, str) or len(value.encode("utf-8")) > maximum or "\0" in value:
            fail("intent %s is invalid" % name)
    if any(ch in row["title"] for ch in "\r\n"):
        fail("intent title must be one line")
    for field, source in (
        ("verify_sha256", row.get("verify", "")),
        ("title_sha256", row.get("title", "")),
        ("body_sha256", row.get("body", "")),
    ):
        value = row.get(field)
        if (not isinstance(value, str) or len(value) != 64 or
                any(ch not in "0123456789abcdef" for ch in value) or
                value != digest(source)):
            fail("intent %s does not match its content" % field)
    for name, maximum in (
        ("created_at", 64), ("updated_at", 64), ("branch", 256), ("base", 256),
        ("remote", 256), ("repo_slug", 512), ("fetch_url", 4096),
        ("push_url", 4096), ("pr_url", 4096),
    ):
        value = row.get(name)
        if (not isinstance(value, str) or len(value.encode("utf-8")) > maximum or
                any(ch in value for ch in "\0\r\n")):
            fail("intent %s is invalid" % name)
    for name in ("created_at", "updated_at", "branch", "base", "remote", "repo_slug"):
        if not row[name]:
            fail("intent %s is empty" % name)
    viewer = row.get("viewer_login")
    if not isinstance(viewer, str) or not re.fullmatch(r"[A-Za-z0-9-]{1,64}", viewer):
        fail("intent GitHub viewer is invalid")
    spec = row.get("spec_sha256", "")
    if (not isinstance(spec, str) or
            (spec and (len(spec) != 64 or any(ch not in "0123456789abcdef" for ch in spec)))):
        fail("intent PROJECT.md digest is invalid")
    if not row["fetch_url"] or not row["push_url"]:
        fail("intent Git remote URLs are empty")
    number = row.get("pr_number")
    if number is not None and (isinstance(number, bool) or not isinstance(number, int) or number < 1):
        fail("intent PR number is invalid")
    if row["phase"] == "complete" and (not row["pr_url"] or number is None):
        fail("complete intent has no exact PR identity")
    if row["phase"] != "complete" and (row["pr_url"] or number is not None):
        fail("incomplete intent carries a PR identity")
    return row


def read_json(path: Path, expected_repo: str) -> Dict[str, Any]:
    try:
        if path.stat().st_size > MAX_INTENT_BYTES:
            fail("intent is too large")
        with path.open(encoding="utf-8") as handle:
            row = json.load(handle)
    except (OSError, ValueError) as exc:
        fail("cannot read intent: %s" % exc)
    return validate_row(row, expected_repo)


def atomic_write(path: Path, row: Dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    encoded = json.dumps(row, ensure_ascii=False, indent=2) + "\n"
    temp_name = ""
    try:
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", newline="\n", dir=str(path.parent),
            prefix=".%s." % path.name, delete=False,
        ) as handle:
            temp_name = handle.name
            os.chmod(temp_name, 0o600)
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temp_name, str(path))
        fsync_parent(path)
    finally:
        if temp_name and os.path.exists(temp_name):
            os.unlink(temp_name)


def atomic_create(path: Path, row: Dict[str, Any]) -> bool:
    """Publish a fully written intent only if no path exists; never replace."""
    path.parent.mkdir(parents=True, exist_ok=True)
    encoded = json.dumps(row, ensure_ascii=False, indent=2) + "\n"
    temp_name = ""
    try:
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", newline="\n", dir=str(path.parent),
            prefix=".%s." % path.name, delete=False,
        ) as handle:
            temp_name = handle.name
            os.chmod(temp_name, 0o600)
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        try:
            os.link(temp_name, str(path))
        except FileExistsError:
            return False
        fsync_parent(path)
        return True
    except OSError as exc:
        fail("cannot create intent atomically: %s" % exc)
    finally:
        if temp_name and os.path.exists(temp_name):
            os.unlink(temp_name)


def sidecar_path(intent: Path, suffix: str) -> Path:
    return Path(str(intent) + suffix)


def require_not_blocked(intent: Path, repo: str) -> None:
    marker = sidecar_path(intent, ".blocked")
    if not marker.exists() and not marker.is_symlink():
        return
    marker = target_path(repo, str(marker), True)
    read_sidecar(marker, repo, "draft-pr-blocked", intent.name)
    fail("publication intent is terminally blocked")


def read_sidecar(path: Path, repo: str, kind: str, intent_name: str) -> Dict[str, Any]:
    try:
        if path.stat().st_size > 16 * 1024:
            fail("publication sidecar is too large")
        with path.open(encoding="utf-8") as handle:
            row = json.load(handle)
    except (OSError, ValueError) as exc:
        fail("cannot read publication sidecar: %s" % exc)
    expected = {"schema", "kind", "repo_id", "intent_name", "created_at"}
    if kind == "draft-pr-push-attempt":
        expected.add("immutable_sha256")
    if (not isinstance(row, dict) or set(row) != expected or row.get("schema") != 1 or
            row.get("kind") != kind or row.get("repo_id") != repo_id(repo) or
            row.get("intent_name") != intent_name or
            not isinstance(row.get("created_at"), str) or not row["created_at"]):
        fail("publication sidecar has an invalid contract")
    if kind == "draft-pr-push-attempt":
        value = row.get("immutable_sha256")
        if (not isinstance(value, str) or len(value) != 64 or
                any(ch not in "0123456789abcdef" for ch in value)):
            fail("push-attempt sidecar digest is invalid")
    return row


def create_sidecar(path: Path, repo: str, kind: str, intent_name: str,
                   immutable_sha256: str = "") -> None:
    row: Dict[str, Any] = {
        "schema": 1,
        "kind": kind,
        "repo_id": repo_id(repo),
        "intent_name": intent_name,
        "created_at": now(),
    }
    if immutable_sha256:
        row["immutable_sha256"] = immutable_sha256
    if not atomic_create(path, row):
        current = read_sidecar(path, repo, kind, intent_name)
        if immutable_sha256 and current.get("immutable_sha256") != immutable_sha256:
            fail("publication sidecar belongs to different immutable fields")


def command_create(args: argparse.Namespace) -> int:
    path = target_path(args.repo, args.path, False)
    for suffix in (".blocked", ".push-attempted"):
        marker = sidecar_path(path, suffix)
        if marker.exists() or marker.is_symlink():
            fail("publication path was already spent; use a new branch name")
    try:
        body = Path(args.body_file).read_text(encoding="utf-8")
    except OSError as exc:
        fail("cannot read PR body: %s" % exc)
    if len(args.verify.encode("utf-8")) > 4096 or len(args.title.encode("utf-8")) > 512:
        fail("verify command or title is too large")
    if len(body.encode("utf-8")) > 64 * 1024:
        fail("PR body is too large")
    if any("\0" in value for value in (args.verify, args.title, body)):
        fail("intent text contains NUL")
    if not re.fullmatch(r"[A-Za-z0-9-]{1,64}", args.viewer_login):
        fail("GitHub viewer login is invalid")
    if args.spec_sha256 and (
        len(args.spec_sha256) != 64
        or any(ch not in "0123456789abcdef" for ch in args.spec_sha256)
    ):
        fail("PROJECT.md digest is invalid")
    if any(not value or any(ch in value for ch in "\0\r\n")
           for value in (args.fetch_url, args.push_url)):
        fail("Git remote URLs are invalid")
    created = now()
    row: Dict[str, Any] = {
        "schema": 1,
        "kind": "draft-pr-intent",
        "phase": "prepared",
        "created_at": created,
        "updated_at": created,
        "repo_id": repo_id(args.repo),
        "head_sha": args.head,
        "tree_sha": args.tree,
        "branch": args.branch,
        "base": args.base,
        "base_sha": args.base_sha,
        "remote": args.remote,
        "repo_slug": args.repo_slug,
        "fetch_url": args.fetch_url,
        "push_url": args.push_url,
        "viewer_login": args.viewer_login,
        "spec_sha256": args.spec_sha256,
        "verify": args.verify,
        "verify_sha256": digest(args.verify),
        "title": args.title,
        "title_sha256": digest(args.title),
        "body": body,
        "body_sha256": digest(body),
        "draft": True,
        "create_only": True,
        "merge": False,
        "release": False,
        "pr_url": "",
        "pr_number": None,
        "push_attempted": False,
    }
    validate_row(row, args.repo)
    if not atomic_create(path, row):
        marker = target_path(
            args.repo, str(sidecar_path(path, ".blocked")), False
        )
        try:
            current = read_json(
                target_path(args.repo, str(path), True), args.repo
            )
        except SystemExit:
            create_sidecar(
                marker, args.repo, "draft-pr-blocked", path.name
            )
            fail("existing intent is invalid and was terminally blocked")
        if immutable_digest(current) != immutable_digest(row):
            create_sidecar(
                marker, args.repo, "draft-pr-blocked", path.name
            )
            fail("existing intent mismatch was terminally blocked")
        if current["phase"] != "prepared":
            fail("existing exact intent already crossed the push boundary")
    print(str(path))
    return 0


def command_field(args: argparse.Namespace) -> int:
    if args.name not in FIELDS:
        fail("unsupported intent field")
    row = read_json(target_path(args.repo, args.path, True), args.repo)
    value = row.get(args.name)
    print("" if value is None else value)
    return 0


def command_body(args: argparse.Namespace) -> int:
    path = target_path(args.repo, args.path, True)
    require_not_blocked(path, args.repo)
    row = read_json(path, args.repo)
    if immutable_digest(row) != args.expected_immutable_sha256:
        fail("intent immutable fields changed after validation")
    if state_digest(row) != args.expected_state_sha256:
        fail("intent state changed before body extraction")
    output = Path(args.output)
    if not output.parent.is_dir():
        fail("body output parent does not exist")
    with output.open("w", encoding="utf-8", newline="\n") as handle:
        os.chmod(str(output), 0o600)
        handle.write(row["body"])
    return 0


def command_mark(args: argparse.Namespace) -> int:
    path = target_path(args.repo, args.path, True)
    require_not_blocked(path, args.repo)
    row = read_json(path, args.repo)
    if immutable_digest(row) != args.expected_immutable_sha256:
        fail("intent immutable fields changed before phase transition")
    if state_digest(row) != args.expected_state_sha256:
        fail("intent state changed before phase transition")
    if row["phase"] != args.expected_phase:
        fail("intent phase changed before transition")
    order = {"prepared": 0, "pushing": 1, "pushed": 2, "complete": 3}
    if order[args.phase] < order[row["phase"]]:
        fail("intent phase cannot move backward")
    if order[args.phase] > order[row["phase"]] + 1:
        fail("intent phase cannot skip a remote effect")
    if args.phase == "complete" and (not args.url or args.number is None or args.number < 1):
        fail("complete intent requires the observed PR URL and number")
    row["phase"] = args.phase
    if args.phase == "pushing":
        row["push_attempted"] = True
    row["updated_at"] = now()
    if args.url:
        row["pr_url"] = args.url
    if args.number is not None:
        row["pr_number"] = args.number
    validate_row(row, args.repo)
    atomic_write(path, row)
    print(state_digest(row))
    return 0


def command_inspect_pr(args: argparse.Namespace) -> int:
    try:
        with open(args.json_file, encoding="utf-8") as handle:
            rows = json.load(handle)
    except (OSError, ValueError) as exc:
        fail("cannot parse gh PR response: %s" % exc)
    if not isinstance(rows, list):
        fail("gh PR response is not a list")
    if not rows:
        print("none")
        return 0
    if len(rows) != 1 or not isinstance(rows[0], dict):
        fail("multiple PRs exist for the source branch")
    row = rows[0]
    if (
        row.get("state") != "OPEN"
        or row.get("isDraft") is not True
        or row.get("headRefName") != args.branch
        or row.get("baseRefName") != args.base
        or row.get("baseRefOid") != args.base_sha
        or row.get("headRefOid") != args.head
        or row.get("isCrossRepository") is not False
        or digest(str(row.get("title", ""))) != args.title_sha256
        or digest(str(row.get("body", ""))) != args.body_sha256
    ):
        fail("existing PR is not the exact open draft intent")
    print("exact\t%s\t%s" % (row.get("url", ""), row.get("number", "")))
    return 0


def command_canonical(args: argparse.Namespace) -> int:
    path = target_path(args.repo, args.path, True)
    read_json(path, args.repo)
    print(str(path))
    return 0


def command_immutable(args: argparse.Namespace) -> int:
    row = read_json(target_path(args.repo, args.path, True), args.repo)
    print(immutable_digest(row))
    return 0


def command_state(args: argparse.Namespace) -> int:
    row = read_json(target_path(args.repo, args.path, True), args.repo)
    print(state_digest(row))
    return 0


def command_block(args: argparse.Namespace) -> int:
    # The verifier may have deleted or corrupted the intent. The caller froze
    # and validated this path before executing it, so the terminal marker must
    # remain creatable even when the intent file itself no longer is.
    intent = target_path(args.repo, args.path, False, preserve_final=True)
    marker = target_path(
        args.repo, str(sidecar_path(intent, ".blocked")), False,
        preserve_final=True,
    )
    create_sidecar(marker, args.repo, "draft-pr-blocked", intent.name)
    print(str(marker))
    return 0


def command_dir_id(args: argparse.Namespace) -> int:
    intent = target_path(args.repo, args.path, True)
    try:
        path_info = intent.parent.stat()
        fd_info = os.fstat(args.dir_fd)
    except OSError as exc:
        fail("cannot bind publication directory handle: %s" % exc)
    if (path_info.st_dev, path_info.st_ino) != (fd_info.st_dev, fd_info.st_ino):
        fail("publication directory handle does not match the intent")
    identity = directory_identity(fd_info)
    checked_publish_fd(args.dir_fd, identity)
    print(identity)
    return 0


def command_block_at(args: argparse.Namespace) -> int:
    if not re.fullmatch(r"[0-9a-f]+:[0-9a-f]+", args.expected_dir_id):
        fail("guarded publication directory identity is invalid")
    if not re.fullmatch(r"[A-Za-z0-9._-]{1,240}\.json", args.intent_name):
        fail("guarded publication intent name is invalid")
    checked_publish_fd(args.dir_fd, args.expected_dir_id)
    marker_name = args.intent_name + ".blocked"
    row: Dict[str, Any] = {
        "schema": 1,
        "kind": "draft-pr-blocked",
        "repo_id": repo_id(args.repo),
        "intent_name": args.intent_name,
        "created_at": now(),
    }
    atomic_block_at(args.dir_fd, marker_name, row)
    print(marker_name)
    return 0


def command_blocked(args: argparse.Namespace) -> int:
    intent = target_path(args.repo, args.path, False, preserve_final=True)
    marker = sidecar_path(intent, ".blocked")
    if not marker.exists() and not marker.is_symlink():
        print("0")
        return 0
    marker = target_path(args.repo, str(marker), True)
    read_sidecar(marker, args.repo, "draft-pr-blocked", intent.name)
    print("1")
    return 0


def command_arm(args: argparse.Namespace) -> int:
    intent = target_path(args.repo, args.path, True)
    require_not_blocked(intent, args.repo)
    row = read_json(intent, args.repo)
    if immutable_digest(row) != args.expected_immutable_sha256:
        fail("intent immutable fields changed before push arming")
    if state_digest(row) != args.expected_state_sha256:
        fail("intent state changed before push arming")
    marker = target_path(
        args.repo, str(sidecar_path(intent, ".push-attempted")), False
    )
    create_sidecar(
        marker, args.repo, "draft-pr-push-attempt", intent.name,
        args.expected_immutable_sha256,
    )
    print(str(marker))
    return 0


def command_armed(args: argparse.Namespace) -> int:
    intent = target_path(args.repo, args.path, True)
    row = read_json(intent, args.repo)
    marker = sidecar_path(intent, ".push-attempted")
    if not marker.exists() and not marker.is_symlink():
        print("0")
        return 0
    marker = target_path(args.repo, str(marker), True)
    sidecar = read_sidecar(
        marker, args.repo, "draft-pr-push-attempt", intent.name
    )
    if sidecar["immutable_sha256"] != immutable_digest(row):
        fail("push-attempt sidecar does not match the publication intent")
    print("1")
    return 0


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    sub = root.add_subparsers(dest="command", required=True)
    create = sub.add_parser("create")
    create.add_argument("--path", required=True)
    create.add_argument("--repo", required=True)
    create.add_argument("--head", required=True)
    create.add_argument("--tree", required=True)
    create.add_argument("--branch", required=True)
    create.add_argument("--base", required=True)
    create.add_argument("--base-sha", required=True)
    create.add_argument("--remote", required=True)
    create.add_argument("--repo-slug", required=True)
    create.add_argument("--fetch-url", required=True)
    create.add_argument("--push-url", required=True)
    create.add_argument("--viewer-login", required=True)
    create.add_argument("--spec-sha256", default="")
    create.add_argument("--verify", required=True)
    create.add_argument("--title", required=True)
    create.add_argument("--body-file", required=True)
    create.set_defaults(func=command_create)

    field = sub.add_parser("field")
    field.add_argument("--path", required=True)
    field.add_argument("--repo", required=True)
    field.add_argument("--name", required=True)
    field.set_defaults(func=command_field)

    body = sub.add_parser("body")
    body.add_argument("--path", required=True)
    body.add_argument("--repo", required=True)
    body.add_argument("--output", required=True)
    body.add_argument("--expected-immutable-sha256", required=True)
    body.add_argument("--expected-state-sha256", required=True)
    body.set_defaults(func=command_body)

    mark = sub.add_parser("mark")
    mark.add_argument("--path", required=True)
    mark.add_argument("--repo", required=True)
    mark.add_argument("--phase", choices=("pushing", "pushed", "complete"), required=True)
    mark.add_argument("--url", default="")
    mark.add_argument("--number", type=int)
    mark.add_argument("--expected-immutable-sha256", required=True)
    mark.add_argument("--expected-state-sha256", required=True)
    mark.add_argument(
        "--expected-phase", choices=("prepared", "pushing", "pushed"), required=True
    )
    mark.set_defaults(func=command_mark)

    inspect = sub.add_parser("inspect-pr")
    inspect.add_argument("--json-file", required=True)
    inspect.add_argument("--branch", required=True)
    inspect.add_argument("--base", required=True)
    inspect.add_argument("--base-sha", required=True)
    inspect.add_argument("--head", required=True)
    inspect.add_argument("--title-sha256", required=True)
    inspect.add_argument("--body-sha256", required=True)
    inspect.set_defaults(func=command_inspect_pr)

    canonical = sub.add_parser("canonical")
    canonical.add_argument("--path", required=True)
    canonical.add_argument("--repo", required=True)
    canonical.set_defaults(func=command_canonical)

    immutable = sub.add_parser("immutable")
    immutable.add_argument("--path", required=True)
    immutable.add_argument("--repo", required=True)
    immutable.set_defaults(func=command_immutable)

    state = sub.add_parser("state")
    state.add_argument("--path", required=True)
    state.add_argument("--repo", required=True)
    state.set_defaults(func=command_state)

    block = sub.add_parser("block")
    block.add_argument("--path", required=True)
    block.add_argument("--repo", required=True)
    block.set_defaults(func=command_block)

    dir_id = sub.add_parser("dir-id")
    dir_id.add_argument("--path", required=True)
    dir_id.add_argument("--repo", required=True)
    dir_id.add_argument("--dir-fd", type=int, required=True)
    dir_id.set_defaults(func=command_dir_id)

    block_at = sub.add_parser("block-at")
    block_at.add_argument("--repo", required=True)
    block_at.add_argument("--intent-name", required=True)
    block_at.add_argument("--dir-fd", type=int, required=True)
    block_at.add_argument("--expected-dir-id", required=True)
    block_at.set_defaults(func=command_block_at)

    blocked = sub.add_parser("blocked")
    blocked.add_argument("--path", required=True)
    blocked.add_argument("--repo", required=True)
    blocked.set_defaults(func=command_blocked)

    arm = sub.add_parser("arm")
    arm.add_argument("--path", required=True)
    arm.add_argument("--repo", required=True)
    arm.add_argument("--expected-immutable-sha256", required=True)
    arm.add_argument("--expected-state-sha256", required=True)
    arm.set_defaults(func=command_arm)

    armed = sub.add_parser("armed")
    armed.add_argument("--path", required=True)
    armed.add_argument("--repo", required=True)
    armed.set_defaults(func=command_armed)
    return root


def main() -> int:
    args = parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
