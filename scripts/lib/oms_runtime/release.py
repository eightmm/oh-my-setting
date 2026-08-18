"""Explicit stable/edge update channel resolution over the existing update transaction."""

from __future__ import annotations

import re
import subprocess
from pathlib import Path
from typing import Any, Dict

from . import RUNTIME_SCHEMA
from .common import CoreError, atomic_write_json, canonical_json, git_head, read_json, relative_path, run_output, sha256_bytes, utc_now

CHANNEL_FILE = Path("config/update-channels.json")


def default_manifest() -> Dict[str, Any]:
    return {"schema": 1, "channels": {"stable": {"version": "0.4.0", "ref": "fa1e9e2cf66ee8b9a5adb642a5ef766c1381db9b", "auto_apply": False, "policy": "pinned reviewed commit"}, "edge": {"version": "edge", "ref": "main", "auto_apply": False, "policy": "clean fast-forward development channel"}}}


def load_manifest(repo: Path) -> Dict[str, Any]:
    path = repo / CHANNEL_FILE
    raw = read_json(path, default=default_manifest())
    if not isinstance(raw, dict) or raw.get("schema") != 1 or not isinstance(raw.get("channels"), dict):
        raise CoreError("invalid update channel manifest: %s" % path)
    for name in ("stable", "edge"):
        row = raw["channels"].get(name)
        if not isinstance(row, dict) or not isinstance(row.get("ref"), str) or not row["ref"]:
            raise CoreError("update channel %s has no ref" % name)
    return raw


def resolve(repo: Path, channel: str, *, fetch: bool = False) -> Dict[str, Any]:
    if channel not in ("stable", "edge"):
        raise CoreError("channel must be stable or edge")
    manifest = load_manifest(repo)
    row = dict(manifest["channels"][channel])
    ref = str(row["ref"])
    if fetch:
        remote = run_output(["git", "-C", str(repo), "remote", "get-url", "origin"])
        if not remote:
            raise CoreError("cannot fetch channel without origin remote")
        result = subprocess.run(["git", "-C", str(repo), "fetch", "--prune", "--tags", "origin"], check=False)
        if result.returncode != 0:
            raise CoreError("failed to fetch origin while resolving channel", exit_code=result.returncode)
    candidates = [ref]
    if channel == "edge" and not ref.startswith("origin/"):
        candidates.insert(0, "origin/" + ref)
    resolved = ""
    selected = ""
    for candidate in candidates:
        value = run_output(["git", "-C", str(repo), "rev-parse", "--verify", candidate + "^{commit}"])
        if value:
            resolved = value
            selected = candidate
            break
    return {"schema": RUNTIME_SCHEMA, "channel": channel, "configured_ref": ref, "selected_ref": selected or None, "resolved_commit": resolved or None, "ready": bool(resolved), "current_commit": git_head(repo) or None, "version": row.get("version"), "policy": row.get("policy"), "auto_apply": bool(row.get("auto_apply", False)), "manifest_digest": sha256_bytes(canonical_json(manifest)), "apply_command": ("oms update --ref %s" % resolved) if resolved else None}


def status(repo: Path) -> Dict[str, Any]:
    return {"schema": RUNTIME_SCHEMA, "manifest": relative_path(repo / CHANNEL_FILE, repo) or CHANNEL_FILE.as_posix(), "stable": resolve(repo, "stable"), "edge": resolve(repo, "edge")}


def apply(repo: Path, channel: str, *, fetch: bool = True, no_tools: bool = True) -> Dict[str, Any]:
    target = resolve(repo, channel, fetch=fetch)
    if not target["ready"]:
        raise CoreError("update channel is not resolvable: %s" % channel, exit_code=3)
    script = repo / "scripts" / "update.sh"
    if not script.is_file() or script.is_symlink():
        raise CoreError("existing transactional update script is unavailable")
    command = ["bash", str(script), "--ref", str(target["resolved_commit"])]
    if no_tools:
        command.append("--no-tools")
    result = subprocess.run(command, cwd=str(repo), check=False)
    return {"schema": RUNTIME_SCHEMA, "channel": channel, "target": target, "exit": result.returncode, "applied": result.returncode == 0, "transaction": "scripts/update.sh"}


def promote(repo: Path, commit: str, version: str, *, expected_manifest_digest: str = "") -> Dict[str, Any]:
    if not re.fullmatch(r"[0-9a-f]{40,64}", commit):
        raise CoreError("stable commit must be a full lowercase Git object id")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?", version):
        raise CoreError("stable version must be semantic-version shaped")
    resolved = run_output(["git", "-C", str(repo), "rev-parse", "--verify", commit + "^{commit}"])
    if not resolved or resolved != commit:
        # Name the repo that was searched: `oms` resolves to the INSTALLED
        # checkout, which has not fetched a release commit that only exists in
        # the development checkout yet — the 0.6.0 promotion hit exactly this
        # and the bare message read as a mystery instead of a wrong front door.
        raise CoreError(
            "stable commit %s is not available in %s; promote runs against the"
            " checkout that owns the channel manifest — from the release"
            " checkout use its own front door (bash scripts/oms runtime release"
            " promote ...), or update this install first" % (commit[:12], repo)
        )
    if not re.fullmatch(r"[0-9a-f]{64}", expected_manifest_digest):
        raise CoreError("stable promotion requires the exact reviewed manifest digest")
    tracked_dirty = run_output(["git", "-C", str(repo), "status", "--porcelain=v1", "--untracked-files=no"])
    if tracked_dirty:
        raise CoreError("stable promotion requires a clean tracked worktree", exit_code=3)
    manifest = load_manifest(repo)
    current_digest = sha256_bytes(canonical_json(manifest))
    if expected_manifest_digest != current_digest:
        raise CoreError("update channel manifest changed since review", exit_code=75)
    manifest["channels"]["stable"] = {"version": version, "ref": commit, "auto_apply": False, "policy": "pinned reviewed commit; promotion requires exact manifest digest", "promoted_at": utc_now()}
    path = repo / CHANNEL_FILE
    atomic_write_json(path, manifest, mode=0o644)
    return {"schema": RUNTIME_SCHEMA, "promoted": True, "commit": commit, "version": version, "manifest": relative_path(path, repo), "previous_manifest_digest": current_digest, "new_manifest_digest": sha256_bytes(canonical_json(manifest))}
