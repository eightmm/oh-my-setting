#!/usr/bin/env python3
"""Validate and atomically update the bounded autopilot's outer receipt."""

from __future__ import annotations

import argparse
import ctypes
import errno
import hashlib
import json
import math
import os
import re
import shlex
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any


RECEIPT_LIMIT = 64 * 1024
PROPOSAL_LIMIT = 1024 * 1024
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
GIT_SHA = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
SAFE = re.compile(r"[A-Za-z0-9._/-]+\Z")
REMOTE = re.compile(r"[A-Za-z0-9._-]+\Z")
UPDATED = re.compile(
    r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z\Z"
)
STAGES = {
    "proposing",
    "proposal-review",
    "approved",
    "driving",
    "reviewing",
    "publishing",
    "parked",
    "done",
}
EFFORTS = {"auto", "low", "medium", "high", "xhigh", "max", "ultra"}


class ReceiptError(Exception):
    pass


def regular_bytes(path: Path, limit: int, label: str) -> bytes:
    try:
        before = path.lstat()
    except OSError as exc:
        raise ReceiptError("%s is unavailable" % label) from exc
    if not stat.S_ISREG(before.st_mode):
        raise ReceiptError("%s must be a regular non-symlink file" % label)
    if before.st_size > limit:
        raise ReceiptError("%s exceeds its size limit" % label)
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(str(path), flags)
    except OSError as exc:
        raise ReceiptError("%s cannot be opened safely" % label) from exc
    try:
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode) or (
            opened.st_dev,
            opened.st_ino,
        ) != (before.st_dev, before.st_ino):
            raise ReceiptError("%s changed while it was opened" % label)
        chunks = bytearray()
        while len(chunks) <= limit:
            chunk = os.read(descriptor, min(65536, limit + 1 - len(chunks)))
            if not chunk:
                break
            chunks.extend(chunk)
        if len(chunks) > limit:
            raise ReceiptError("%s exceeds its size limit" % label)
        after = path.lstat()
        if not stat.S_ISREG(after.st_mode) or (
            after.st_dev,
            after.st_ino,
        ) != (opened.st_dev, opened.st_ino):
            raise ReceiptError("%s changed while it was read" % label)
        return bytes(chunks)
    finally:
        os.close(descriptor)


def digest(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def text(value: Any, label: str, *, empty: bool = False) -> str:
    if not isinstance(value, str) or (not empty and not value):
        raise ReceiptError("%s is invalid" % label)
    if any(ord(char) < 32 or ord(char) == 127 for char in value):
        raise ReceiptError("%s contains control characters" % label)
    return value


def boolean(value: Any, label: str) -> bool:
    if not isinstance(value, bool):
        raise ReceiptError("%s is invalid" % label)
    return value


def integer(value: Any, label: str, low: int, high: int) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or not low <= value <= high:
        raise ReceiptError("%s is invalid" % label)
    return value


def validate_duration(value: Any, label: str) -> str:
    value = text(value, label)
    match = re.fullmatch(r"([1-9][0-9]*)([smh]?)", value)
    if not match:
        raise ReceiptError("%s is invalid" % label)
    seconds = int(match.group(1)) * {"": 1, "s": 1, "m": 60, "h": 3600}[match.group(2)]
    if seconds > 24 * 60 * 60:
        raise ReceiptError("%s exceeds 24h" % label)
    return value


def normalize_allowed(value: Any) -> str:
    source = text(value, "allowed envelope")
    items = []
    for item in re.split(r"[,\s]+", source):
        if not item:
            continue
        item = item.strip().replace("\\", "/")
        while item.startswith("./"):
            item = item[2:]
        item = item.rstrip("/") or "."
        if (
            item.startswith("/")
            or re.match(r"^[A-Za-z]:", item)
            or (
                item != "."
                and any(part in {"", ".", ".."} for part in item.split("/"))
            )
        ):
            raise ReceiptError("allowed envelope is invalid")
        items.append(item)
    normalized = ",".join(sorted(set(items)))
    if not normalized or normalized != source:
        raise ReceiptError("allowed envelope is not canonical")
    return normalized


def load_receipt(path: Path) -> tuple[dict[str, Any], bytes]:
    payload = regular_bytes(path, RECEIPT_LIMIT, "outer receipt")
    try:
        row = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise ReceiptError("outer receipt is not valid UTF-8 JSON") from exc
    if not isinstance(row, dict):
        raise ReceiptError("outer receipt is not an object")
    validate_receipt(row)
    return row, payload


def validate_receipt_location(path: Path, repo: str) -> tuple[str, str]:
    repo_root = os.path.realpath(repo)
    expected_plan_root = os.path.join(repo_root, ".oms", "plan")
    expected_path = os.path.join(expected_plan_root, "autopilot-run.json")
    if os.path.realpath(path.parent) != expected_plan_root:
        raise ReceiptError("outer receipt directory escapes the physical repository")
    if os.path.realpath(path) != expected_path:
        raise ReceiptError("outer receipt path is not canonical")
    try:
        parent_state = path.parent.lstat()
    except OSError as exc:
        raise ReceiptError("outer receipt directory is unavailable") from exc
    if not stat.S_ISDIR(parent_state.st_mode):
        raise ReceiptError("outer receipt directory must be a regular directory")
    return repo_root, expected_plan_root


def validate_receipt(row: dict[str, Any]) -> None:
    expected = {
        "schema",
        "kind",
        "stage",
        "repo",
        "spec_sha256",
        "providers",
        "routing",
        "contract",
        "proposal",
        "branch",
        "updated",
    }
    if set(row) != expected or row.get("schema") != 1 or row.get("kind") != "autopilot-run":
        raise ReceiptError("outer receipt does not match schema 1")
    if row.get("stage") not in STAGES:
        raise ReceiptError("outer receipt stage is invalid")
    repo_value = text(row.get("repo"), "repo")
    if not os.path.isabs(repo_value):
        raise ReceiptError("repo path is not absolute")
    if not SHA256.fullmatch(text(row.get("spec_sha256"), "spec digest")):
        raise ReceiptError("spec digest is invalid")
    providers = row.get("providers")
    routing = row.get("routing")
    contract = row.get("contract")
    proposal = row.get("proposal")
    if not isinstance(providers, dict) or set(providers) != {"planner", "worker", "reviewer"}:
        raise ReceiptError("provider binding is invalid")
    for role in ("planner", "worker", "reviewer"):
        if text(providers.get(role), "%s provider" % role) not in {
            "codex",
            "claude",
            "antigravity",
        }:
            raise ReceiptError("%s provider is invalid" % role)
    if not isinstance(routing, dict) or set(routing) != {
        "provider_timeout",
        "planner",
        "worker",
        "reviewer",
    }:
        raise ReceiptError("routing binding is invalid")
    validate_duration(routing.get("provider_timeout"), "provider timeout")
    for role in ("planner", "worker", "reviewer"):
        route = routing.get(role)
        if not isinstance(route, dict) or set(route) != {
            "model",
            "fallback_model",
            "reasoning_effort",
            "timeout",
        }:
            raise ReceiptError("%s routing is invalid" % role)
        model = text(route.get("model"), "%s model" % role, empty=True)
        fallback_model = text(
            route.get("fallback_model"), "%s fallback model" % role, empty=True
        )
        if len(model) > 160 or len(fallback_model) > 160:
            raise ReceiptError("%s model is too long" % role)
        if model == "provider-default" or fallback_model == "provider-default":
            raise ReceiptError("%s model uses a reserved value" % role)
        if route.get("reasoning_effort") not in EFFORTS:
            raise ReceiptError("%s reasoning effort is invalid" % role)
        validate_duration(route.get("timeout"), "%s timeout" % role)
    contract_keys = {
        "allowed",
        "base",
        "base_sha",
        "remote",
        "max_cycles",
        "initial_tasks",
        "replan_tasks",
        "auto_repair",
        "retry_known",
        "review_mode",
        "draft_pr",
    }
    if not isinstance(contract, dict) or set(contract) != contract_keys:
        raise ReceiptError("run contract is invalid")
    normalize_allowed(contract.get("allowed"))
    if not SAFE.fullmatch(text(contract.get("base"), "base")):
        raise ReceiptError("base is invalid")
    if not REMOTE.fullmatch(text(contract.get("remote"), "remote")):
        raise ReceiptError("remote is invalid")
    if not GIT_SHA.fullmatch(text(contract.get("base_sha"), "base digest")):
        raise ReceiptError("base digest is invalid")
    integer(contract.get("max_cycles"), "max cycles", 1, 10)
    integer(contract.get("initial_tasks"), "initial tasks", 1, 12)
    integer(contract.get("replan_tasks"), "replan tasks", 1, 2)
    boolean(contract.get("auto_repair"), "auto repair")
    boolean(contract.get("retry_known"), "retry known")
    boolean(contract.get("draft_pr"), "draft PR")
    if contract.get("review_mode") not in {"shadow", "gate", "off"}:
        raise ReceiptError("review mode is invalid")
    if not isinstance(proposal, dict) or set(proposal) != {"path", "sha256"}:
        raise ReceiptError("proposal binding is invalid")
    proposal_path = text(proposal.get("path"), "proposal path", empty=True)
    proposal_sha = text(proposal.get("sha256"), "proposal digest", empty=True)
    if bool(proposal_path) != bool(proposal_sha):
        raise ReceiptError("proposal path and digest must be present together")
    if proposal_sha and not SHA256.fullmatch(proposal_sha):
        raise ReceiptError("proposal digest is invalid")
    if proposal_path and not os.path.isabs(proposal_path):
        raise ReceiptError("proposal path is not absolute")
    if row["stage"] == "proposing" and proposal_path:
        raise ReceiptError("proposing receipt cannot bind a proposal")
    if row["stage"] == "proposal-review" and not proposal_path:
        raise ReceiptError("proposal-review receipt lacks a proposal")
    branch = text(row.get("branch"), "branch")
    canonical_branch = "oms/autopilot-%s" % row["spec_sha256"][:12]
    recovery_pattern = re.compile(re.escape(canonical_branch) + r"-r[1-9][0-9]*\Z")
    if branch != canonical_branch and not recovery_pattern.fullmatch(branch):
        raise ReceiptError("branch is outside the deterministic autopilot lineage")
    if not UPDATED.fullmatch(text(row.get("updated"), "updated")):
        raise ReceiptError("updated timestamp is invalid")


def resolve_base(repo: str, remote: str, base: str) -> str:
    refs = ("refs/remotes/%s/%s" % (remote, base), "refs/heads/%s" % base)
    for ref in refs:
        result = subprocess.run(
            ["git", "-C", repo, "rev-parse", "--verify", ref + "^{commit}"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            check=False,
        )
        if result.returncode == 0:
            return result.stdout.strip()
    raise ReceiptError("bound base cannot be resolved")


def validate_live(row: dict[str, Any], repo: str) -> None:
    expected_repo = os.path.realpath(repo)
    if os.path.realpath(row["repo"]) != expected_repo:
        raise ReceiptError("outer receipt belongs to a different repository")
    spec_payload = regular_bytes(Path(expected_repo) / "PROJECT.md", PROPOSAL_LIMIT, "PROJECT.md")
    if digest(spec_payload) != row["spec_sha256"]:
        raise ReceiptError("PROJECT.md differs from the outer receipt")
    contract = row["contract"]
    if resolve_base(expected_repo, contract["remote"], contract["base"]) != contract["base_sha"]:
        raise ReceiptError("bound base ref moved")
    branch = subprocess.run(
        ["git", "-C", expected_repo, "symbolic-ref", "--quiet", "--short", "HEAD"],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        check=False,
    )
    current_branch = branch.stdout.strip()
    allowed_branches = {row["branch"]}
    if row["stage"] in {"proposing", "proposal-review", "parked"}:
        allowed_branches.add(contract["base"])
    canonical_branch = "oms/autopilot-%s" % row["spec_sha256"][:12]
    if row["stage"] in {"proposing", "proposal-review", "parked"} and row["branch"] == canonical_branch:
        if re.fullmatch(re.escape(canonical_branch) + r"-r[1-9][0-9]*", current_branch):
            allowed_branches.add(current_branch)
    if branch.returncode != 0 or current_branch not in allowed_branches:
        raise ReceiptError("current branch differs from the outer receipt")
    proposal = row["proposal"]
    if proposal["path"]:
        proposal_path = Path(proposal["path"])
        plan_root = os.path.realpath(os.path.join(expected_repo, ".oms", "plan"))
        if os.path.dirname(os.path.realpath(proposal_path)) != plan_root:
            raise ReceiptError("approved proposal is outside the private plan directory")
        proposal_payload = regular_bytes(proposal_path, PROPOSAL_LIMIT, "approved proposal")
        if digest(proposal_payload) != proposal["sha256"]:
            raise ReceiptError("approved proposal digest changed")


def immutable_view(row: dict[str, Any]) -> dict[str, Any]:
    return {
        key: row[key]
        for key in row
        if key not in {"stage", "updated", "proposal", "branch"}
    }


def valid_transition(before: str, after: str) -> bool:
    allowed = {
        "proposing": {"proposing", "proposal-review", "parked"},
        "proposal-review": {"proposal-review", "approved", "parked"},
        "approved": {"proposal-review", "approved", "driving", "parked"},
        "driving": {
            "proposing",
            "proposal-review",
            "approved",
            "driving",
            "reviewing",
            "parked",
        },
        "reviewing": {
            "proposal-review",
            "approved",
            "reviewing",
            "publishing",
            "done",
            "parked",
        },
        "publishing": {
            "proposal-review",
            "approved",
            "publishing",
            "done",
            "parked",
        },
        "parked": {
            "parked",
            "proposing",
            "proposal-review",
            "approved",
            "driving",
            "reviewing",
            "publishing",
        },
        "done": {"done"},
    }
    return after in allowed.get(before, set())


def valid_proposal_transition(before: dict[str, Any], after: dict[str, Any]) -> bool:
    if before["proposal"] == after["proposal"]:
        return True
    # A bounded remainder deliberately clears the completed tranche's binding
    # before asking the planner. Only the active drive or an operator-visible
    # park may start that new two-step approval boundary.
    if (
        after["stage"] == "proposing"
        and after["proposal"] == {"path": "", "sha256": ""}
        and before["stage"] in {"driving", "parked"}
    ):
        return True
    # The only other mutable edge is the planner's empty proposing receipt
    # becoming the exact path+digest that the parent must review.
    return (
        before["stage"] == "proposing"
        and before["proposal"] == {"path": "", "sha256": ""}
        and after["stage"] == "proposal-review"
        and bool(after["proposal"]["path"])
    )


def valid_branch_transition(before: dict[str, Any], after: dict[str, Any]) -> bool:
    if before["branch"] == after["branch"]:
        return True
    canonical = "oms/autopilot-%s" % before["spec_sha256"][:12]
    recovery = re.fullmatch(
        re.escape(canonical) + r"-r[1-9][0-9]*", after["branch"]
    )
    return (
        before["branch"] == canonical
        and recovery is not None
        and before["stage"] in {"proposing", "proposal-review", "parked"}
        and after["stage"] in {"proposing", "proposal-review", "approved", "driving"}
    )


def option_args(row: dict[str, Any]) -> list[str]:
    providers = row["providers"]
    routing = row["routing"]
    contract = row["contract"]
    args = [
        "oms",
        "autopilot",
        "--repo",
        row["repo"],
        "--planner",
        providers["planner"],
        "--worker",
        providers["worker"],
        "--reviewer",
        providers["reviewer"],
        "--allowed",
        contract["allowed"],
        "--base",
        contract["base"],
        "--remote",
        contract["remote"],
        "--max-cycles",
        str(contract["max_cycles"]),
        "--initial-tasks",
        str(contract["initial_tasks"]),
        "--replan-tasks",
        str(contract["replan_tasks"]),
        "--provider-timeout",
        routing["provider_timeout"],
        "--planner-timeout",
        routing["planner"]["timeout"],
        "--worker-timeout",
        routing["worker"]["timeout"],
        "--reviewer-timeout",
        routing["reviewer"]["timeout"],
        "--planner-reasoning-effort",
        routing["planner"]["reasoning_effort"],
        "--worker-reasoning-effort",
        routing["worker"]["reasoning_effort"],
        "--reviewer-reasoning-effort",
        routing["reviewer"]["reasoning_effort"],
        "--review-mode",
        contract["review_mode"],
    ]
    for role in ("planner", "worker", "reviewer"):
        if routing[role]["model"]:
            args.extend(("--%s-model" % role, routing[role]["model"]))
        if routing[role]["fallback_model"]:
            args.extend(
                ("--%s-fallback-model" % role, routing[role]["fallback_model"])
            )
    if contract["auto_repair"]:
        args.append("--auto-repair")
    if contract["retry_known"]:
        args.append("--retry-known")
    if contract["draft_pr"]:
        args.append("--draft-pr")
    args.append("propose" if row["stage"] == "proposing" else "run")
    if row["proposal"]["path"]:
        args.extend(
            (
                "--proposal",
                row["proposal"]["path"],
                "--expected-proposal-sha256",
                row["proposal"]["sha256"],
            )
        )
    return args


def row_from_args(args: argparse.Namespace) -> dict[str, Any]:
    return {
        "schema": 1,
        "kind": "autopilot-run",
        "stage": args.stage,
        "repo": os.path.realpath(args.repo),
        "spec_sha256": args.spec_sha256,
        "providers": {
            "planner": args.planner,
            "worker": args.worker,
            "reviewer": args.reviewer,
        },
        "routing": {
            "provider_timeout": args.provider_timeout,
            "planner": {
                "model": args.planner_model,
                "fallback_model": args.planner_fallback_model,
                "reasoning_effort": args.planner_reasoning_effort,
                "timeout": args.planner_timeout,
            },
            "worker": {
                "model": args.worker_model,
                "fallback_model": args.worker_fallback_model,
                "reasoning_effort": args.worker_reasoning_effort,
                "timeout": args.worker_timeout,
            },
            "reviewer": {
                "model": args.reviewer_model,
                "fallback_model": args.reviewer_fallback_model,
                "reasoning_effort": args.reviewer_reasoning_effort,
                "timeout": args.reviewer_timeout,
            },
        },
        "contract": {
            "allowed": args.allowed,
            "base": args.base,
            "base_sha": args.base_sha,
            "remote": args.remote,
            "max_cycles": args.max_cycles,
            "initial_tasks": args.initial_tasks,
            "replan_tasks": args.replan_tasks,
            "auto_repair": args.auto_repair,
            "retry_known": args.retry_known,
            "review_mode": args.review_mode,
            "draft_pr": args.draft_pr,
        },
        "proposal": {"path": args.proposal, "sha256": args.proposal_sha256},
        "branch": args.branch,
        "updated": args.updated,
    }


def add_binding_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--stage", required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--spec-sha256", required=True)
    for role in ("planner", "worker", "reviewer"):
        parser.add_argument("--%s" % role, required=True)
        parser.add_argument("--%s-model" % role, default="")
        parser.add_argument("--%s-fallback-model" % role, default="")
        parser.add_argument("--%s-reasoning-effort" % role, required=True)
        parser.add_argument("--%s-timeout" % role, required=True)
    parser.add_argument("--provider-timeout", required=True)
    parser.add_argument("--allowed", required=True)
    parser.add_argument("--base", required=True)
    parser.add_argument("--base-sha", required=True)
    parser.add_argument("--remote", required=True)
    parser.add_argument("--max-cycles", type=int, required=True)
    parser.add_argument("--initial-tasks", type=int, required=True)
    parser.add_argument("--replan-tasks", type=int, required=True)
    parser.add_argument("--auto-repair", action="store_true")
    parser.add_argument("--retry-known", action="store_true")
    parser.add_argument("--review-mode", required=True)
    parser.add_argument("--draft-pr", action="store_true")
    parser.add_argument("--proposal", default="")
    parser.add_argument("--proposal-sha256", default="")
    parser.add_argument("--branch", required=True)
    parser.add_argument("--updated", required=True)


def atomic_write(path: Path, payload: bytes) -> None:
    parent = path.parent
    try:
        parent_state = parent.lstat()
    except OSError as exc:
        raise ReceiptError("outer receipt directory is unavailable") from exc
    if not stat.S_ISDIR(parent_state.st_mode):
        raise ReceiptError("outer receipt directory must be a regular directory")
    descriptor, temporary = tempfile.mkstemp(prefix=".autopilot-run.", dir=str(parent))
    try:
        os.chmod(temporary, 0o600)
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        fsync_directory(parent)
    except Exception:
        try:
            os.unlink(temporary)
        except OSError:
            pass
        raise


def fsync_directory(path: Path) -> None:
    # POSIX directory fsync makes os.replace durable. Native Windows Python
    # cannot open directories this way; os.replace remains atomic there and
    # the file itself was already flushed above.
    if os.name != "posix":
        return
    descriptor = os.open(str(path), os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        try:
            os.fsync(descriptor)
        except OSError as exc:
            if exc.errno not in {errno.EBADF, errno.EINVAL, errno.ENOTSUP}:
                raise
    finally:
        os.close(descriptor)


def supervise_phase(args: argparse.Namespace) -> int:
    command = list(args.argv)
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        raise ReceiptError("supervised phase requires a command")
    wall = float(args.wall)
    grace = float(args.kill_after)
    if (
        not math.isfinite(wall)
        or not math.isfinite(grace)
        or wall <= 0
        or grace <= 0
        # The public maximum is 10 worker cycles, five routed attempts per
        # cycle, one five-attempt repair, and a one-minute shutdown margin:
        # 55 days + 60 seconds when each attempt uses the 24 hour maximum.
        or wall > 56 * 24 * 60 * 60
        or grace > 30
    ):
        raise ReceiptError("supervised phase bounds are invalid")
    output_path = getattr(args, "output", None)
    metadata_path = getattr(args, "metadata", None)
    output_limit = int(getattr(args, "output_limit", 0) or 0)
    if output_path is None and output_limit:
        raise ReceiptError("an output limit requires --output")
    if output_path is not None and not 1 <= output_limit <= 64 * 1024 * 1024:
        raise ReceiptError("supervised output limit is invalid")
    cwd = getattr(args, "cwd", None)
    if cwd is not None:
        cwd = os.path.realpath(cwd)
        try:
            cwd_state = os.lstat(cwd)
        except OSError as exc:
            raise ReceiptError("supervised working directory is unavailable") from exc
        if not stat.S_ISDIR(cwd_state.st_mode):
            raise ReceiptError("supervised working directory is not a directory")

    def open_existing_regular(path_value: str, label: str) -> int:
        path = Path(path_value)
        try:
            before = path.lstat()
        except OSError as exc:
            raise ReceiptError("%s is unavailable" % label) from exc
        if not stat.S_ISREG(before.st_mode):
            raise ReceiptError("%s must be a regular non-symlink file" % label)
        flags = os.O_WRONLY | os.O_TRUNC
        flags |= getattr(os, "O_BINARY", 0) | getattr(os, "O_CLOEXEC", 0)
        flags |= getattr(os, "O_NOFOLLOW", 0)
        try:
            descriptor = os.open(str(path), flags)
        except OSError as exc:
            raise ReceiptError("%s cannot be opened safely" % label) from exc
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode) or (
            opened.st_dev,
            opened.st_ino,
        ) != (before.st_dev, before.st_ino):
            os.close(descriptor)
            raise ReceiptError("%s changed while it was opened" % label)
        return descriptor

    def write_metadata(payload: dict[str, Any]) -> None:
        if metadata_path is None:
            return
        descriptor = open_existing_regular(metadata_path, "supervision metadata")
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, sort_keys=True, separators=(",", ":"))
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())

    output_descriptor = None
    if output_path is not None:
        output_descriptor = open_existing_regular(output_path, "supervised output")

    # Linux can make the supervisor the adoption point for a daemonizing
    # grandchild. This closes the fork+setsid+parent-exit gap that process-group
    # signaling alone cannot cover. Other POSIX systems retain the periodically
    # refreshed PID/start-time identities below.
    child_subreaper = False
    if sys.platform.startswith("linux"):
        try:
            libc = ctypes.CDLL(None, use_errno=True)
            if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
                error_number = ctypes.get_errno()
                raise OSError(error_number, os.strerror(error_number))
            child_subreaper = True
        except (AttributeError, OSError) as exc:
            if output_descriptor is not None:
                os.close(output_descriptor)
            raise ReceiptError("cannot establish the supervised child boundary") from exc

    creation_flags = 0
    if os.name == "nt":
        creation_flags = getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)
    # The phase can announce readiness immediately after Popen. Queue signals
    # during the small interval before whole-tree tracking/handlers are ready;
    # otherwise a parent HUP/INT/TERM could hit Python's default disposition
    # after the child starts but before cleanup authority is installed.
    pending_signals: list[int] = []

    def queue_initial_signal(signum: int, _frame: object) -> None:
        if not pending_signals:
            pending_signals.append(signum)

    supervised_signals = []
    for signal_name in ("SIGHUP", "SIGINT", "SIGTERM"):
        forwarded_signal = getattr(signal, signal_name, None)
        if forwarded_signal is not None:
            supervised_signals.append(forwarded_signal)
            signal.signal(forwarded_signal, queue_initial_signal)
    output_limited = threading.Event()
    reader_failed = threading.Event()
    reader = None
    process = None
    try:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            stdin=subprocess.DEVNULL if output_path is not None else None,
            stdout=subprocess.PIPE if output_path is not None else None,
            stderr=subprocess.STDOUT if output_path is not None else None,
            start_new_session=(os.name == "posix"),
            creationflags=creation_flags,
        )
    except FileNotFoundError as exc:
        if output_descriptor is not None:
            os.close(output_descriptor)
        print("error: %s" % exc, file=sys.stderr)
        write_metadata(
            {
                "exit": 127,
                "timed_out": False,
                "output_limited": False,
                "launch_error": True,
                "supervision_error": False,
            }
        )
        return 127
    except (OSError, PermissionError, subprocess.SubprocessError) as exc:
        if output_descriptor is not None:
            os.close(output_descriptor)
        print("error: %s" % exc, file=sys.stderr)
        write_metadata(
            {
                "exit": 126,
                "timed_out": False,
                "output_limited": False,
                "launch_error": True,
                "supervision_error": False,
            }
        )
        return 126

    written = [0]

    def drain_output() -> None:
        assert process is not None
        assert process.stdout is not None
        assert output_descriptor is not None
        try:
            with os.fdopen(output_descriptor, "wb") as handle:
                while True:
                    chunk = process.stdout.read(65536)
                    if not chunk:
                        break
                    room = max(0, output_limit - written[0])
                    if room:
                        selected = chunk[:room]
                        handle.write(selected)
                        written[0] += len(selected)
                    if len(chunk) > room:
                        output_limited.set()
                        break
                handle.flush()
        except OSError:
            reader_failed.set()

    if output_path is not None:
        if process.stdout is None:
            os.close(output_descriptor)
            raise ReceiptError("supervised output pipe is unavailable")
        reader = threading.Thread(target=drain_output, daemon=True)
        reader.start()

    # pid -> (ppid, pgid, sid, process start identity). A provider's own
    # timeout wrapper may create a second process group or session. Killing
    # only the group leader would orphan that nested group, so periodically
    # retain every member that belongs to the phase. The start identity lets a
    # legitimate setsid/reparent update keep its PID without trusting PID reuse.
    owned: dict[int, tuple[int, int, int, str]] = {}

    def process_table() -> dict[int, tuple[int, int, int, str]]:
        commands = (
            [
                "ps", "-e", "-o", "pid=", "-o", "ppid=", "-o", "pgid=",
                "-o", "sid=", "-o", "lstart=",
            ],
            [
                "ps", "-e", "-o", "pid=", "-o", "ppid=", "-o", "pgid=",
                "-o", "sess=", "-o", "lstart=",
            ],
            [
                "ps", "-ax", "-o", "pid=", "-o", "ppid=", "-o", "pgid=",
                "-o", "sess=", "-o", "lstart=",
            ],
        )
        for command_line in commands:
            probe = None
            try:
                probe = subprocess.Popen(
                    command_line,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                    text=True,
                )
                probe_pid = probe.pid
                stdout, _stderr = probe.communicate(timeout=2)
            except (OSError, subprocess.SubprocessError):
                if probe is not None and probe.poll() is None:
                    probe.kill()
                    try:
                        probe.wait(timeout=1)
                    except subprocess.TimeoutExpired:
                        pass
                continue
            if probe.returncode != 0:
                continue
            rows: dict[int, tuple[int, int, int, str]] = {}
            malformed = False
            for line in stdout.splitlines():
                fields = line.split()
                if (
                    len(fields) < 5
                    or any(not value.isdigit() for value in fields[:4])
                    or not " ".join(fields[4:])
                ):
                    malformed = True
                    break
                pid, ppid, pgid, sid = (int(value) for value in fields[:4])
                rows[pid] = (ppid, pgid, sid, " ".join(fields[4:]))
            if not malformed:
                # The enumeration process is our own short-lived child, not a
                # member of the phase when Linux subreaper ancestry is active.
                rows.pop(probe_pid, None)
                return rows
        raise ReceiptError("cannot enumerate the supervised process session")

    def reap_adopted() -> None:
        if not child_subreaper or process is None or process.poll() is None:
            return
        while True:
            try:
                child, _status = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                return
            if child <= 0:
                return

    def refresh_owned() -> dict[int, tuple[int, int, int, str]]:
        if os.name != "posix":
            return {}
        reap_adopted()
        rows = process_table()
        selected = {pid for pid, values in rows.items() if values[2] == process.pid}
        selected.add(process.pid)
        ancestry = set(selected)
        if child_subreaper:
            # A daemonized descendant is reparented here after its immediate
            # shell exits. The ps probe itself was removed above.
            ancestry.add(os.getpid())
        changed = True
        while changed:
            changed = False
            for pid, values in rows.items():
                if values[0] in ancestry and pid not in ancestry:
                    ancestry.add(pid)
                    selected.add(pid)
                    changed = True
        for pid, identity in tuple(owned.items()):
            current = rows.get(pid)
            if current is None or current[3] != identity[3]:
                del owned[pid]
            else:
                # setsid and reparent legitimately change the first three
                # fields while process start identity remains stable.
                owned[pid] = current
        for pid in selected:
            if pid in rows and pid != os.getpid():
                owned[pid] = rows[pid]
        return rows

    def group_alive() -> bool:
        if os.name != "posix":
            return process.poll() is None
        rows = refresh_owned()
        for pid, identity in owned.items():
            if rows.get(pid) == identity:
                return True
        return False

    def signal_cached(signum: int) -> None:
        own_group = os.getpgrp()
        groups = {
            identity[1]
            for identity in owned.values()
            if identity[1] > 1 and identity[1] != own_group
        }
        if process.pid > 1:
            groups.add(process.pid)
        for pgid in groups:
            try:
                os.killpg(pgid, signum)
            except (ProcessLookupError, PermissionError):
                pass
        for pid in tuple(owned):
            if pid > 1 and pid != os.getpid():
                try:
                    os.kill(pid, signum)
                except (ProcessLookupError, PermissionError):
                    pass

    def signal_owned(signum: int) -> None:
        rows = refresh_owned()
        own_group = os.getpgrp()
        groups = {
            identity[1]
            for pid, identity in owned.items()
            if rows.get(pid) == identity and identity[1] > 1 and identity[1] != own_group
        }
        for pgid in groups:
            try:
                os.killpg(pgid, signum)
            except (ProcessLookupError, PermissionError):
                pass
        # A descendant can start a fresh session between table snapshots. Its
        # PID remains in the owned identity set; signal it directly as well.
        for pid, identity in tuple(owned.items()):
            if pid > 1 and rows.get(pid) == identity:
                try:
                    os.kill(pid, signum)
                except (ProcessLookupError, PermissionError):
                    pass

    if os.name == "posix":
        try:
            refresh_owned()
        except ReceiptError:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
            try:
                process.wait(timeout=max(1.0, grace))
            except subprocess.TimeoutExpired:
                pass
            raise

    def stop_group(first_signal: int) -> None:
        enumeration_ok = True
        if os.name == "posix":
            try:
                signal_owned(first_signal)
            except ReceiptError:
                enumeration_ok = False
                # Never interpret process-table failure as completion. Signal
                # every recently observed identity plus the original group.
                signal_cached(first_signal)
        else:
            # taskkill without /F is the closest portable tree-wide graceful
            # request on native Windows. Escalate with /F below.
            try:
                subprocess.run(
                    ["taskkill", "/PID", str(process.pid), "/T"],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=max(1.0, grace),
                    check=False,
                )
            except (OSError, subprocess.SubprocessError):
                try:
                    process.terminate()
                except OSError:
                    pass
        deadline = time.monotonic() + grace
        alive = True
        while alive and time.monotonic() < deadline:
            process.poll()
            if os.name == "posix":
                if enumeration_ok:
                    try:
                        alive = group_alive()
                    except ReceiptError:
                        enumeration_ok = False
                        alive = True
                else:
                    alive = process.poll() is None or bool(owned)
            else:
                alive = process.poll() is None
            if not alive:
                break
            time.sleep(0.02)
        if alive:
            if os.name == "posix":
                if enumeration_ok:
                    try:
                        signal_owned(signal.SIGKILL)
                    except ReceiptError:
                        signal_cached(signal.SIGKILL)
                else:
                    signal_cached(signal.SIGKILL)
            else:
                try:
                    subprocess.run(
                        ["taskkill", "/PID", str(process.pid), "/T", "/F"],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        timeout=max(1.0, grace),
                        check=False,
                    )
                except (OSError, subprocess.SubprocessError):
                    try:
                        process.kill()
                    except OSError:
                        pass
        try:
            process.wait(timeout=max(1.0, grace))
        except subprocess.TimeoutExpired:
            pass
        reap_adopted()

    interrupted = [False]

    def forward(signum: int, _frame: object) -> None:
        if interrupted[0]:
            return
        interrupted[0] = True
        stop_group(signum)
        raise SystemExit(128 + signum)

    for forwarded in supervised_signals:
        signal.signal(forwarded, forward)
    if pending_signals:
        forward(pending_signals[0], None)
    timed_out = False
    supervision_error = False
    raw_exit = None
    try:
        deadline = time.monotonic() + wall
        # Linux subreaper adoption preserves an escaped child after its parent
        # exits, so a one-second scan is sufficient there. Other POSIX systems
        # need a tighter periodic ancestry snapshot to retain a setsid child.
        refresh_interval = 1.0 if child_subreaper else 0.1
        next_refresh = time.monotonic()
        while True:
            now = time.monotonic()
            if os.name == "posix" and now >= next_refresh:
                try:
                    refresh_owned()
                except ReceiptError as exc:
                    print("error: %s" % exc, file=sys.stderr)
                    supervision_error = True
                    stop_group(signal.SIGTERM)
                    break
                next_refresh = now + refresh_interval
            raw_exit = process.poll()
            if raw_exit is not None:
                if os.name == "posix":
                    try:
                        refresh_owned()
                    except ReceiptError as exc:
                        print("error: %s" % exc, file=sys.stderr)
                        supervision_error = True
                        stop_group(signal.SIGTERM)
                break
            if reader_failed.is_set():
                print("error: cannot capture supervised phase output", file=sys.stderr)
                supervision_error = True
                stop_group(signal.SIGTERM)
                break
            if output_limited.is_set():
                stop_group(signal.SIGTERM)
                break
            if now >= deadline:
                timed_out = True
                print(
                    "error: %s phase timed out after %ss" % (args.label, args.wall),
                    file=sys.stderr,
                )
                stop_group(signal.SIGTERM)
                break
            time.sleep(min(0.02, max(0.001, deadline - now)))

        if raw_exit is None:
            raw_exit = process.poll()
        if not supervision_error and os.name == "posix":
            try:
                if group_alive():
                    # A command can return while a background descendant still
                    # owns the captured pipe or an escaped session.
                    stop_group(signal.SIGTERM)
            except ReceiptError as exc:
                print("error: %s" % exc, file=sys.stderr)
                supervision_error = True
                signal_cached(signal.SIGKILL)
        elif not supervision_error and process.poll() is None:
            stop_group(signal.SIGTERM)
    finally:
        try:
            still_alive = process.poll() is None
            if not still_alive and os.name == "posix":
                try:
                    still_alive = group_alive()
                except ReceiptError:
                    still_alive = bool(owned)
            if still_alive:
                stop_group(signal.SIGTERM)
        finally:
            if reader is not None:
                reader.join(timeout=max(1.0, grace))
                if reader.is_alive() and process.stdout is not None:
                    process.stdout.close()
                    reader.join(timeout=1)
            reap_adopted()

    if supervision_error:
        exit_code = 126
    elif timed_out:
        exit_code = 124
    elif output_limited.is_set():
        exit_code = 125
    elif raw_exit is None:
        exit_code = 126
        supervision_error = True
    else:
        exit_code = 128 + (-raw_exit) if raw_exit < 0 else raw_exit
    write_metadata(
        {
            "exit": exit_code,
            "timed_out": timed_out,
            "output_limited": output_limited.is_set(),
            "launch_error": False,
            "supervision_error": supervision_error,
        }
    )
    return exit_code


def main() -> int:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    write = sub.add_parser("write")
    write.add_argument("path")
    write.add_argument("--expected", required=True)
    add_binding_args(write)
    inspect = sub.add_parser("inspect")
    inspect.add_argument("path")
    inspect.add_argument("--repo", required=True)
    inspect.add_argument("--continuation", action="store_true")
    digest_parser = sub.add_parser("digest")
    digest_parser.add_argument("path")
    metadata = sub.add_parser("metadata")
    metadata.add_argument("path")
    archive = sub.add_parser("archive-done")
    archive.add_argument("path")
    archive.add_argument("--repo", required=True)
    archive.add_argument("--expected", required=True)
    abandon = sub.add_parser("abandon")
    abandon.add_argument("path")
    abandon.add_argument("--repo", required=True)
    abandon.add_argument("--expected", required=True)
    abandon.add_argument("--reason", required=True)
    abandon.add_argument("--updated", required=True)
    supervisor = sub.add_parser("supervise")
    supervisor.add_argument("--wall", type=float, required=True)
    supervisor.add_argument("--kill-after", type=float, default=1)
    supervisor.add_argument("--label", required=True)
    supervisor.add_argument("--cwd")
    supervisor.add_argument("--output")
    supervisor.add_argument("--output-limit", type=int, default=0)
    supervisor.add_argument("--metadata")
    supervisor.add_argument("argv", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    try:
        if args.command == "supervise":
            return supervise_phase(args)
        path = Path(args.path)
        if args.command == "digest":
            print(digest(regular_bytes(path, RECEIPT_LIMIT, "outer receipt")))
            return 0
        if args.command == "metadata":
            row, payload = load_receipt(path)
            print("%s\t%s\t%s" % (row["stage"], row["spec_sha256"], digest(payload)))
            return 0
        if args.command == "abandon":
            # The append-only exit for a receipt whose frozen contract turned
            # out to be the problem (a too-short wall killed the first call
            # and could not be amended — observed live, 2026-08-18). Mirrors
            # archive-done's idiom: CAS, content-addressed preservation of
            # the exact bytes, then a typed abandon record naming the
            # predecessor digest and reason. The live path frees for a fresh
            # propose with a new contract; nothing is mutated or lost.
            validate_receipt_location(path, args.repo)
            row, payload = load_receipt(path)
            current = digest(payload)
            if current != args.expected:
                raise ReceiptError("outer receipt CAS changed")
            if row["stage"] == "done":
                raise ReceiptError("a done outer receipt is archived, not abandoned")
            if not UPDATED.fullmatch(args.updated):
                raise ReceiptError("abandon requires a valid --updated timestamp")
            reason = args.reason.strip()
            if not reason or len(reason) > 400:
                raise ReceiptError("abandon requires a nonempty --reason (max 400 chars)")
            archive_path = path.with_name("autopilot-run.%s.json" % current)
            record_path = path.with_name("autopilot-run.%s.abandoned.json" % current)
            if archive_path.exists() or archive_path.is_symlink():
                archived = regular_bytes(archive_path, RECEIPT_LIMIT, "archived receipt")
                if digest(archived) != current:
                    raise ReceiptError("outer receipt archive path is occupied")
            else:
                # Copy before unlink: a crash between steps must never leave
                # the abandoned contract without its only preserved copy.
                atomic_write(archive_path, payload)
            record = {
                "schema": 1,
                "kind": "autopilot-abandon",
                "predecessor": current,
                "stage_at_abandon": row["stage"],
                "spec_sha256": row["spec_sha256"],
                "branch": row.get("branch", ""),
                "reason": reason,
                "updated": args.updated,
            }
            atomic_write(
                record_path,
                (json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8"),
            )
            os.unlink(path)
            fsync_directory(path.parent)
            print(record_path)
            return 0
        if args.command == "archive-done":
            validate_receipt_location(path, args.repo)
            row, payload = load_receipt(path)
            current = digest(payload)
            if current != args.expected:
                raise ReceiptError("outer receipt CAS changed")
            if row["stage"] != "done":
                raise ReceiptError("only a done outer receipt can be archived")
            archive_path = path.with_name("autopilot-run.%s.json" % current)
            if archive_path.exists() or archive_path.is_symlink():
                archived = regular_bytes(archive_path, RECEIPT_LIMIT, "archived receipt")
                if digest(archived) != current:
                    raise ReceiptError("outer receipt archive path is occupied")
                os.unlink(path)
            else:
                os.replace(path, archive_path)
            fsync_directory(path.parent)
            print(archive_path)
            return 0
        if args.command == "inspect":
            validate_receipt_location(path, args.repo)
            row, payload = load_receipt(path)
            validate_live(row, args.repo)
            print("outer run: stage=%s receipt=%s" % (row["stage"], digest(payload)))
            if args.continuation and row["stage"] != "done":
                print("parent-agent continuation:")
                print("  " + " ".join(shlex.quote(value) for value in option_args(row)))
            return 0
        row = row_from_args(args)
        validate_receipt(row)
        _repo_root, expected_plan_root = validate_receipt_location(path, args.repo)
        if row["proposal"]["path"]:
            proposal_path = Path(row["proposal"]["path"])
            if os.path.dirname(os.path.realpath(proposal_path)) != expected_plan_root:
                raise ReceiptError("approved proposal is outside the private plan directory")
            proposal_payload = regular_bytes(
                proposal_path, PROPOSAL_LIMIT, "approved proposal"
            )
            if digest(proposal_payload) != row["proposal"]["sha256"]:
                raise ReceiptError("approved proposal digest changed")
        current = "absent"
        old_row = None
        if path.exists() or path.is_symlink():
            old_row, old_payload = load_receipt(path)
            current = digest(old_payload)
        if args.expected != current:
            raise ReceiptError("outer receipt CAS changed (expected %s, found %s)" % (args.expected, current))
        if old_row is not None:
            if immutable_view(old_row) != immutable_view(row):
                raise ReceiptError("outer receipt immutable contract changed")
            if not valid_transition(old_row["stage"], row["stage"]):
                raise ReceiptError(
                    "outer receipt stage transition is invalid (%s -> %s)"
                    % (old_row["stage"], row["stage"])
                )
            if not valid_branch_transition(old_row, row):
                raise ReceiptError("outer receipt branch transition is invalid")
            if not valid_proposal_transition(old_row, row):
                raise ReceiptError("outer receipt proposal transition is invalid")
        payload = (json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
        if len(payload) > RECEIPT_LIMIT:
            raise ReceiptError("outer receipt exceeds its size limit")
        latest = "absent"
        if path.exists() or path.is_symlink():
            latest = digest(regular_bytes(path, RECEIPT_LIMIT, "outer receipt"))
        if latest != current:
            raise ReceiptError("outer receipt changed before its atomic update")
        atomic_write(path, payload)
        print(digest(payload))
        return 0
    except (OSError, ReceiptError, subprocess.SubprocessError) as exc:
        print("error: %s" % exc, file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
