"""Fork file state through checkpoints and the existing scratch-worktree owner."""
from __future__ import annotations

import difflib
import json
import os
from pathlib import Path
import stat
import tarfile

import checkpoint_untracked
from oms_runtime.common import CoreError, read_json, sha256_bytes
from .common import (
    MAX_PATCH_BYTES, base_record, command, excerpt, git, git_env, ident, label, load,
    owned_bytes, plain_chain, repository, save, script, state_dir,
)

DIFF_OPTIONS = ("--no-color", "--src-prefix=a/", "--dst-prefix=b/", "--binary", "--full-index",
                "--no-ext-diff", "--no-textconv", "--no-renames")
MAX_COMPARE_PATHS = 5000


def patches(repo):
    return (git(repo, "diff", *DIFF_OPTIONS, "--cached", "HEAD", "--"),
            git(repo, "diff", *DIFF_OPTIONS, "--"))


def head(repo):
    return git(repo, "rev-parse", "HEAD", limit=256).decode().strip()


def inspect_files(repo):
    if git(repo, "ls-files", "--", ".oms", limit=8192).strip():
        raise CoreError("forking refuses tracked .oms state; native forks do not inherit OMS authority")
    if git(repo, "ls-files", "--unmerged", "-z", limit=8192):
        raise CoreError("resolve unmerged paths before creating a session fork")
    entries = git(repo, "ls-files", "-v", "-z").split(b"\0")
    if any(entry and (entry[:1].islower() or entry[:1] == b"S") for entry in entries):
        raise CoreError("forking refuses assume-unchanged or skip-worktree entries; their hidden edits cannot be inherited")
    if any(entry.startswith(b"160000 ") for entry in git(repo, "ls-files", "--stage", "-z").split(b"\0")):
        raise CoreError("session file forks do not copy submodule worktrees")
    visible = git(repo, "diff", "--cached", "--name-only", "-z", "--ita-visible-in-index", "HEAD", "--")
    invisible = git(repo, "diff", "--cached", "--name-only", "-z", "--ita-invisible-in-index", "HEAD", "--")
    if visible != invisible:
        raise CoreError("intent-to-add entries cannot be checkpointed; stage or unstage them first")
    staged, unstaged = patches(repo)
    names = checkpoint_untracked.untracked_names(str(repo))
    copied = omitted = size = 0
    for name in names:
        path = repo / name
        try:
            info = path.lstat()
            plain_chain(path)
        except (OSError, CoreError):
            omitted += 1
            continue
        if stat.S_ISREG(info.st_mode) and info.st_nlink != 1:
            raise CoreError("forking refuses multiply-linked untracked files")
        if (not stat.S_ISREG(info.st_mode) or
                info.st_size > checkpoint_untracked.MAX_FILE_BYTES or
                size + info.st_size > checkpoint_untracked.MAX_TOTAL_BYTES or
                copied >= checkpoint_untracked.MAX_FILES):
            omitted += 1
            continue
        copied += 1
        size += info.st_size
    return {"head": head(repo), "staged_bytes": len(staged), "unstaged_bytes": len(unstaged),
            "untracked_captured": copied, "untracked_omitted": omitted, "untracked_capture_bytes": size}


def _checkpoint(repo):
    plain_chain(repo / ".oms" / "checkpoints", directory=True)
    raw = command(["bash", script("checkpoint.sh"), "--repo", str(repo), "create", "--untracked",
                   "--label", "session-fork", "--json"], limit=1024 * 1024, env=git_env())
    try:
        metadata = json.loads(raw)
    except ValueError:
        raise CoreError("checkpoint did not return valid metadata") from None
    directory = plain_chain(repo / ".oms" / "checkpoints" / ident(metadata["id"], "checkpoint ID"), directory=True)
    for name, key in (("index.patch", "index_sha256"), ("worktree.patch", "worktree_sha256"),
                      ("untracked.tar", "untracked_sha256")):
        bound = (checkpoint_untracked.MAX_TOTAL_BYTES + checkpoint_untracked.MAX_FILES * 4096
                 if name.endswith(".tar") else MAX_PATCH_BYTES)
        data = owned_bytes(directory / name, bound)
        if sha256_bytes(data) != metadata.get(key):
            raise CoreError("checkpoint content checksum changed")
    _validate_archive(directory / "untracked.tar")
    # The checkpoint's diff serialization does not disable rename detection.
    # Compare using exactly that format, not a semantically equivalent patch.
    for cached, filename in ((True, "index.patch"), (False, "worktree.patch")):
        options = list(DIFF_OPTIONS[:-1])
        if cached:
            options += ["--cached", "HEAD"]
        current = git(repo, "diff", *options, "--")
        if current != owned_bytes(directory / filename, MAX_PATCH_BYTES):
            raise CoreError("source tracked files changed during checkpoint creation; retry")
    if head(repo) != metadata["head"]:
        raise CoreError("source HEAD changed during checkpoint creation; retry")
    return metadata, directory


def _validate_archive(path):
    total = count = 0
    with tarfile.open(path, "r") as archive:
        for member in archive:
            total += member.size
            count += 1
            if (not member.isreg() or not checkpoint_untracked.safe_name(member.name) or member.size < 0
                    or member.size > checkpoint_untracked.MAX_FILE_BYTES
                    or total > checkpoint_untracked.MAX_TOTAL_BYTES or count > checkpoint_untracked.MAX_FILES):
                raise CoreError("checkpoint untracked archive contains an unsafe or oversized entry")


def create(repo, source, title="", owner_pid=None):
    title = label(title)
    inspect_files(repo)
    state_dir(repo, True)
    metadata, directory = _checkpoint(repo)
    row = dict(base_record(repo, "fork"), label=title, source=source,
               source_head=metadata["head"], checkpoint_id=metadata["id"], status="preparing",
               native_session=None, conversation_status="not_started", files=metadata)
    lineage = repo / ".oms" / "sessions" / "origin.json"
    if lineage.exists():
        parent = read_json(plain_chain(lineage), limit=8192)
        if isinstance(parent, dict) and parent.get("repo") == str(repo):
            row["parent_fork"] = {"id": ident(parent["fork_id"]), "repo": parent["parent_repo"]}
    save(repo, "forks", row)
    command_args = ["bash", script("scratch-worktree.sh"), "add", "--repo", str(repo),
                    "--base", metadata["head"], "--owner-pid", str(owner_pid or os.getppid())]
    try:
        target = Path(command(command_args, limit=8192, env=git_env()).decode().strip()).resolve(strict=True)
        row["worktree"] = str(target)
        save(repo, "forks", row)
        # Session experiments must remain inspectable after their terminal exits.
        # Git's lock makes ordinary scratch cleanup preserve the registration.
        git(repo, "worktree", "lock", "--reason", "OMS session exploration " + row["id"], str(target))
        for name, indexed in (("index.patch", True), ("worktree.patch", False)):
            patch = directory / name
            if patch.stat().st_size:
                git(target, "apply", *( ["--index"] if indexed else []), str(patch))
        result = checkpoint_untracked.restore(str(target), str(directory / "untracked.tar"), True)
        if result["refused"] or result["written"] != metadata["untracked_captured"]:
            raise CoreError("some captured untracked files could not be restored into the fork")
        state_dir(target, True)
        from oms_runtime.common import atomic_write_json
        atomic_write_json(target / ".oms" / "sessions" / "origin.json",
                          {"schema": 1, "repo": str(target), "parent_repo": str(repo), "fork_id": row["id"]})
        row["status"] = "prepared"
        save(repo, "forks", row)
        return row
    except Exception:
        row["status"] = "files_failed"
        save(repo, "forks", row)
        # A partial new checkout and its recovery checkpoint are evidence, not
        # disposable output. Never force-remove either on a preparation error.
        raise


def fork_target(repo, row):
    target = plain_chain(Path(row.get("worktree", "")), directory=True)
    if not target.is_absolute() or repository(str(target)) != target or target == repo:
        raise CoreError("fork worktree is missing or no longer a repository root")
    marker = owned_bytes(target.parent / ".oh-my-setting-tmp", 8192).decode("utf-8")
    fields = dict(line.split("=", 1) for line in marker.splitlines() if "=" in line)
    if fields.get("repo") != str(repo) or fields.get("worktree") != str(target):
        raise CoreError("fork worktree marker no longer matches its source")
    def common(root):
        raw = git(root, "rev-parse", "--git-common-dir", limit=8192).decode().strip()
        return (root / raw).resolve()
    if common(target) != common(repo):
        raise CoreError("fork worktree belongs to another Git repository")
    return target


def _file_state(repo, name):
    path = repo / name
    try:
        plain_chain(path)
        info = path.lstat()
    except FileNotFoundError:
        return {"kind": "missing"}, None
    except CoreError:
        return {"kind": "symlink", "omitted": True}, None
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        return {"kind": "nonregular", "omitted": True}, None
    if info.st_size > checkpoint_untracked.MAX_FILE_BYTES:
        return {"kind": "oversized", "bytes": info.st_size, "omitted": True}, None
    data = owned_bytes(path, checkpoint_untracked.MAX_FILE_BYTES)
    return {"kind": "file", "sha256": sha256_bytes(data), "bytes": len(data),
            "executable": bool(info.st_mode & 0o111)}, data


def _changed(repo, base):
    raw = git(repo, "diff", "--no-ext-diff", "--no-textconv", "--no-renames", "--name-only", "-z", base, "--")
    names = {os.fsdecode(item) for item in raw.split(b"\0") if item}
    raw = git(repo, "diff", "--no-ext-diff", "--no-textconv", "--no-renames", "--cached", "--name-only", "-z", base, "--")
    names.update(os.fsdecode(item) for item in raw.split(b"\0") if item)
    names.update(checkpoint_untracked.untracked_names(str(repo)))
    if len(names) > MAX_COMPARE_PATHS:
        raise CoreError("fork comparison exceeds 5000 changed paths")
    return names


def _artifacts(repo):
    root = plain_chain(repo / ".oms" / "artifacts", directory=True)
    if not root.exists():
        return {"files": [], "truncated": False}
    result, pending, size, entries = [], [(root, 0)], 0, 0
    while pending:
        folder, level = pending.pop()
        with os.scandir(folder) as items:
            for item in items:
                entries += 1
                if entries > 1000:
                    return {"files": result, "truncated": True}
                if item.is_dir(follow_symlinks=False) and level < 3:
                    pending.append((Path(item.path), level + 1))
                elif item.is_file(follow_symlinks=False):
                    rel = Path(item.path).relative_to(repo).as_posix()
                    info = item.stat(follow_symlinks=False)
                    if size + info.st_size > checkpoint_untracked.MAX_TOTAL_BYTES:
                        return {"files": result, "truncated": True}
                    state, _ = _file_state(repo, rel)
                    result.append(dict(state, path=rel))
                    size += info.st_size
    return {"files": sorted(result, key=lambda row: row["path"]), "truncated": False}


def compare(repo, left_id, right_id, include_patch=False):
    left, right = load(repo, "forks", left_id), load(repo, "forks", right_id)
    lrepo, rrepo = fork_target(repo, left), fork_target(repo, right)
    if left["source_head"] != right["source_head"]:
        raise CoreError("comparison requires forks from the same saved HEAD")
    names = _changed(lrepo, left["source_head"]) | _changed(rrepo, right["source_head"])
    rows, consumed, patch_bytes = [], 0, 0
    for name in sorted(names):
        if not checkpoint_untracked.safe_name(name):
            continue
        a, adata = _file_state(lrepo, name)
        b, bdata = _file_state(rrepo, name)
        consumed += len(adata or b"") + len(bdata or b"")
        if consumed > checkpoint_untracked.MAX_TOTAL_BYTES:
            raise CoreError("fork comparison exceeds the 64 MiB content bound")
        if a == b and not a.get("omitted"):
            continue
        row = {"path": name, "left": a, "right": b, "equal": False if not a.get("omitted") and not b.get("omitted") else None}
        if include_patch and (adata is not None or a["kind"] == "missing") and (bdata is not None or b["kind"] == "missing"):
            try:
                av, bv = (adata or b"").decode("utf-8"), (bdata or b"").decode("utf-8")
                if "\0" not in av + bv:
                    patch = "".join(difflib.unified_diff(av.splitlines(True), bv.splitlines(True),
                                                       fromfile="left/" + name, tofile="right/" + name))
                    safe, redacted = excerpt(patch, 0, len(patch))
                    encoded = safe.encode()
                    bounded = encoded[:min(16384, 65536 - patch_bytes)].decode("utf-8", "ignore")
                    row.update(patch=bounded, patch_redacted=redacted, patch_truncated=len(encoded) > len(bounded.encode()))
                    patch_bytes += len(bounded.encode())
            except UnicodeError:
                row["binary"] = True
        rows.append(row)
        if len(rows) >= 200:
            break
    def summary(record, target):
        staged, unstaged = patches(target)
        return {"id": record["id"], "worktree": str(target), "head": head(target),
                "staged_sha256": sha256_bytes(staged), "unstaged_sha256": sha256_bytes(unstaged),
                "staged_bytes": len(staged), "unstaged_bytes": len(unstaged), "artifacts": _artifacts(target)}
    return {"schema": 1, "kind": "oms-session-compare", "base": left["source_head"],
            "left": summary(left, lrepo), "right": summary(right, rrepo), "files": rows,
            "truncated": len(rows) >= 200,
            "note": "Current worktree content plus separate index/worktree patch digests; artifact bytes are never printed."}


def restore_files(repo, checkpoint, apply=False, untracked_only=False):
    key = ident(checkpoint, "checkpoint ID")
    if not key.startswith("cp-"):
        raise CoreError("expected an OMS checkpoint ID")
    metadata_path = plain_chain(repo / ".oms" / "checkpoints" / key / "meta.json")
    metadata = read_json(metadata_path, limit=1024 * 1024)
    if not isinstance(metadata, dict) or metadata.get("id") != key:
        raise CoreError("checkpoint is missing or invalid")
    for name, field in (("index.patch", "index_sha256"), ("worktree.patch", "worktree_sha256")):
        if sha256_bytes(owned_bytes(metadata_path.parent / name, MAX_PATCH_BYTES)) != metadata.get(field):
            raise CoreError("checkpoint patch checksum does not match")
    archive = metadata_path.parent / "untracked.tar"
    if metadata.get("untracked_sha256") or archive.exists():
        bound = checkpoint_untracked.MAX_TOTAL_BYTES + checkpoint_untracked.MAX_FILES * 4096
        if sha256_bytes(owned_bytes(archive, bound)) != metadata.get("untracked_sha256"):
            raise CoreError("checkpoint untracked archive checksum does not match")
        _validate_archive(archive)
    if not untracked_only and metadata.get("head") != head(repo):
        raise CoreError("file restore requires the same HEAD as checkpoint creation; HEAD is never moved")
    argv = ["bash", script("checkpoint.sh"), "--repo", str(repo), "restore", key, "--json"]
    if apply:
        argv.append("--apply")
    if untracked_only:
        argv.append("--untracked-only")
    report = json.loads(command(argv, limit=1024 * 1024, env=git_env()))
    return dict(report, kind="oms-session-restore-files", conversation_changed=False,
                note="Tracked restore keeps the checkpoint HEAD and creates a recovery backup. Existing untracked files are preserved.")
