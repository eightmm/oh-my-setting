"""Optional capability profiles and dependency plans."""

from __future__ import annotations

import os
import shutil
from pathlib import Path
from typing import Any, Dict, List, Sequence

from . import RUNTIME_SCHEMA
from .common import CoreError, atomic_write_json, read_json, utc_now

BUILTIN_PROFILE_DEFS: Dict[str, Dict[str, Any]] = {
    "core": {"description": "Local state, typed projections and one coding-agent backend.", "required": ["bash", "git", "python3"], "any_of": [["codex", "claude", "agy"]], "min_present": {}, "optional": [], "managed_tools": ["primary-provider"]},
    "council": {"description": "Cross-provider consultation, review and delegation.", "inherits": ["core"], "required": [], "any_of": [], "min_present": {"commands": ["codex", "claude", "agy"], "count": 2}, "optional": [], "managed_tools": ["secondary-provider"]},
    "github": {"description": "GitHub status, branch publication and Draft PR integration.", "required": ["gh"], "any_of": [], "min_present": {}, "optional": [], "managed_tools": ["gh"]},
    "notion": {"description": "Optional Work Journal mirror to Notion.", "required": ["ntn"], "any_of": [], "min_present": {}, "optional": [], "managed_tools": ["ntn"]},
    "research": {"description": "Reproducible ML/research runs and locked Python environments.", "required": ["uv"], "any_of": [], "min_present": {}, "optional": ["nvidia-smi"], "managed_tools": ["uv"]},
    "hpc": {"description": "Slurm reconciliation and cluster job control.", "required": ["sbatch", "squeue", "sacct"], "any_of": [], "min_present": {}, "optional": ["sinfo", "scontrol", "nvidia-smi"], "managed_tools": []},
    "container": {"description": "Container-isolated runtime backend.", "required": [], "any_of": [["docker", "podman"]], "min_present": {}, "optional": [], "managed_tools": []},
    "remote": {"description": "External remote execution adapter.", "required": [], "any_of": [], "min_present": {}, "optional": [], "managed_tools": [], "environment": ["OMS_REMOTE_ADAPTER"]},
    "full": {"description": "Compatibility profile with all provider, GitHub, Notion and research adapters.", "inherits": ["core", "council", "github", "notion", "research"], "required": [], "any_of": [], "min_present": {}, "optional": [], "managed_tools": ["all-providers"]},
}


def _load_profile_defs() -> Dict[str, Dict[str, Any]]:
    path = Path(__file__).resolve().parents[3] / "config" / "capability-profiles.json"
    raw = read_json(path, default=None)
    if raw is None:
        return BUILTIN_PROFILE_DEFS
    if not isinstance(raw, dict) or raw.get("schema") != 1 or not isinstance(raw.get("profiles"), dict):
        raise CoreError("invalid capability profile catalog: %s" % path)
    profiles: Dict[str, Dict[str, Any]] = {}
    for name, spec in raw["profiles"].items():
        if name not in BUILTIN_PROFILE_DEFS or not isinstance(spec, dict):
            raise CoreError("unsupported capability profile in catalog: %s" % name)
        profiles[str(name)] = dict(spec)
    missing = sorted(set(BUILTIN_PROFILE_DEFS) - set(profiles))
    if missing:
        raise CoreError("capability profile catalog is missing: %s" % ", ".join(missing))
    return profiles


PROFILE_DEFS = _load_profile_defs()


def _expand(names: Sequence[str]) -> List[str]:
    result: List[str] = []

    def visit(name: str) -> None:
        if name not in PROFILE_DEFS:
            raise CoreError("unknown capability profile: %s" % name)
        for inherited in PROFILE_DEFS[name].get("inherits", []):
            visit(str(inherited))
        if name not in result:
            result.append(name)

    for item in names:
        visit(item)
    return result


def check(names: Sequence[str]) -> Dict[str, Any]:
    expanded = _expand(names)
    required: List[str] = []
    optional: List[str] = []
    any_groups: List[List[str]] = []
    environment: List[str] = []
    min_groups: List[Dict[str, Any]] = []
    for name in expanded:
        spec = PROFILE_DEFS[name]
        required.extend(spec.get("required", []))
        optional.extend(spec.get("optional", []))
        any_groups.extend(spec.get("any_of", []))
        environment.extend(spec.get("environment", []))
        if spec.get("min_present"):
            min_groups.append(dict(spec["min_present"], profile=name))
    required = sorted(set(required))
    optional = sorted(set(optional) - set(required))
    required_status = {name: shutil.which(name) is not None for name in required}
    optional_status = {name: shutil.which(name) is not None for name in optional}
    any_status: List[Dict[str, Any]] = []
    for group in any_groups:
        present = [name for name in group if shutil.which(name)]
        any_status.append({"commands": group, "present": present, "satisfied": bool(present)})
    minimum_status: List[Dict[str, Any]] = []
    for group in min_groups:
        commands = [str(value) for value in group.get("commands", [])]
        count = int(group.get("count", 1))
        present = [name for name in commands if shutil.which(name)]
        minimum_status.append({"profile": group.get("profile"), "commands": commands, "required_count": count, "present": present, "satisfied": len(present) >= count})
    env_status = {name: bool(os.environ.get(name)) for name in sorted(set(environment))}
    missing = [name for name, present in required_status.items() if not present]
    missing.extend("env:%s" % name for name, present in env_status.items() if not present)
    missing.extend("one-of:%s" % "|".join(str(name) for name in row["commands"]) for row in any_status if not row["satisfied"])
    missing.extend("at-least-%d:%s" % (int(row["required_count"]), "|".join(str(name) for name in row["commands"])) for row in minimum_status if not row["satisfied"])
    ready = not missing
    return {"schema": RUNTIME_SCHEMA, "requested": list(names), "expanded": expanded, "ready": ready, "required": required_status, "any_of": any_status, "minimum": minimum_status, "environment": env_status, "optional": optional_status, "missing": sorted(missing), "installation_policy": "missing optional adapters disable only their capability; local state remains usable"}


def state_path(repo: Path) -> Path:
    return repo / ".oms" / "runtime" / "profiles.json"


def current(repo: Path) -> Dict[str, Any]:
    row = read_json(state_path(repo), default=None)
    if not isinstance(row, dict):
        return {"schema": 1, "requested": ["core"], "configured": False, "check": check(["core"])}
    names = row.get("requested", ["core"])
    if not isinstance(names, list):
        names = ["core"]
    normalized = [str(name) for name in names if str(name) in PROFILE_DEFS] or ["core"]
    return dict(row, requested=normalized, configured=True, check=check(normalized))


def apply(repo: Path, names: Sequence[str], *, allow_missing: bool = False) -> Dict[str, Any]:
    result = check(names)
    if not result["ready"] and not allow_missing:
        raise CoreError("capability profile is not ready: %s" % ", ".join(result["missing"]), exit_code=3)
    row = {"schema": 1, "updated_at": utc_now(), "requested": list(names), "expanded": result["expanded"], "ready_at_write": result["ready"], "missing_at_write": result["missing"]}
    atomic_write_json(state_path(repo), row)
    return dict(row, check=result)


def install_plan(names: Sequence[str], primary_provider: str = "codex") -> Dict[str, Any]:
    if primary_provider not in ("codex", "claude", "agy"):
        raise CoreError("primary provider must be codex, claude, or agy")
    requested: List[str] = []
    for name in names:
        value = str(name)
        if value not in requested:
            requested.append(value)
    expanded = _expand(requested)
    provider_order = ["codex", "claude", "agy"]
    secondary_provider = next(name for name in provider_order if name != primary_provider)
    managed: List[str] = []

    def add(tool: str) -> None:
        if tool not in managed:
            managed.append(tool)

    for name in expanded:
        for tool in PROFILE_DEFS[name].get("managed_tools", []):
            if tool == "primary-provider":
                selected = [primary_provider]
            elif tool == "secondary-provider":
                candidates = [value for value in provider_order if value != primary_provider]
                present = [value for value in candidates if shutil.which(value)]
                selected = [present[0] if present else secondary_provider]
            elif tool == "all-providers":
                selected = provider_order
            else:
                selected = [str(tool)]
            for value in selected:
                add(value)
    if any(tool in managed for tool in ("codex", "claude", "ntn")):
        managed.insert(0, "node")
    command = ["oms", "install-profile"]
    for name in requested:
        command.extend(["--profile", name])
    command.extend(["--primary-provider", primary_provider])
    return {"schema": 1, "requested": requested, "expanded": expanded, "primary_provider": primary_provider, "managed_tools": managed, "notion_optional": "notion" not in expanded, "github_optional": "github" not in expanded, "selective_installer": "scripts/install-profile.sh", "command": command}
