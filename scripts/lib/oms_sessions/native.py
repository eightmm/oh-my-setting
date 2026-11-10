"""Explicit interactive native actions, gated by the installed CLI's help."""
from __future__ import annotations

import os
import json
import hashlib
from pathlib import Path
import re
import shutil
import subprocess
import uuid

from oms_runtime.common import CoreError, sha256_bytes, utc_now
from .common import base_record, command, git_env, plain_chain, save, script, transition
from .transcripts import discover, read_source, roots
from .worktrees import fork_target


def _flag(help_text, name):
    return re.search(r"(?<![\w-])" + re.escape(name) + r"(?=[\s,=<\[]|$)", help_text) is not None


def _managed_resume(source, repo):
    from panel_native import _native_resume_binding
    plain_chain(repo / ".oms" / "threads", directory=True)
    try:
        states, _ = _native_resume_binding(source["provider"], repo, source["session_id"])
    except ValueError:
        raise CoreError("native owner discovery is incomplete; resume through the existing panel") from None
    digest = hashlib.sha256(source["session_id"].encode()).hexdigest()[:32]
    providers = {"pi", "codex"} if source["provider"] == "pi" else {source["provider"]}
    matches = {state["id"] for state in states for member in state["participants"]
               if member["provider"] in providers and digest in (member.get("consumer"), member.get("initial_consumer"))}
    if not matches:
        return None
    if len(matches) != 1 or source["provider"] == "pi":
        raise CoreError("managed native identity is ambiguous or unsupported here; use the existing room chat navigation")
    # Reuse all live-owner, room-binding and lease checks in the established
    # native helper. A new standalone resume must not bypass those checks.
    return ["bash", script("panel.sh"), "--repo", str(repo), "--room", next(iter(matches)),
            "--host", "inline", "--launch", source["provider"], "--resume", source["session_id"]]


def plan(source, cwd, fork=False, child_id=None):
    provider = source["provider"]
    if provider in {"claude", "codex"}:
        selected = plain_chain(Path(source["path"]))
        configured = [plain_chain(path, directory=True) for _, path, _ in roots(provider)]
        if not any(selected.is_relative_to(path) for path in configured):
            raise CoreError("selected transcript is outside the configured native store; custom roots are read-only for ID-based native launches")
    binary = shutil.which(provider)
    if not binary:
        raise CoreError("%s CLI is not installed; native session was preserved" % provider)
    binary = str(Path(binary).resolve())
    help_text = command([binary, "--help"], cwd=cwd, limit=256 * 1024, timeout=5).decode("utf-8", "replace")
    session = source["session_id"]
    expected_id = None
    if provider == "codex":
        action = "fork" if fork else "resume"
        if not re.search(r"(?m)^\s*" + action + r"\s", help_text):
            raise CoreError("installed Codex does not advertise %s" % action)
        action_help = command([binary, action, "--help"], cwd=cwd, limit=256 * 1024, timeout=5).decode("utf-8", "replace")
        if not _flag(action_help, "--cd"):
            raise CoreError("installed Codex does not advertise --cd for %s; worktree routing is unverified" % action)
        argv = [binary, action, session, "--cd", str(cwd)]
    elif provider == "claude":
        if not _flag(help_text, "--resume") or fork and not _flag(help_text, "--fork-session"):
            raise CoreError("installed Claude does not advertise the required native resume/fork flags")
        argv = [binary, "--resume", session]
        if fork:
            argv.append("--fork-session")
            if _flag(help_text, "--session-id"):
                expected_id = child_id or str(uuid.uuid4())
                argv += ["--session-id", expected_id]
    elif provider == "pi":
        flag = "--fork" if fork else "--session"
        if not _flag(help_text, flag):
            raise CoreError("installed Pi does not advertise %s" % flag)
        # Pi accepts path or ID. The already verified exact file avoids an
        # ambiguous ID/prefix search in another worktree's session directory.
        argv = [binary, flag, source["path"]]
    else:
        raise CoreError("unsupported native provider")
    managed = _managed_resume(source, cwd) if not fork else None
    return {"argv": managed or argv, "cwd": str(cwd), "provider": provider, "source_session": session,
            "action": "fork" if fork else "resume", "expected_native_session": expected_id,
            "capability": "installed CLI --help", "route": "managed-panel" if managed else "native", "dry_run": True}


def _environment():
    environment = git_env()
    # A conversation copy is not a copy of a room membership, attempt, task,
    # or publication grant. Keep native credentials, settings and restrictions.
    for key in list(environment):
        if key.startswith(("OMS_ROOM_", "OMS_ATTEMPT_", "OMS_TASK_", "OMS_PARENT_", "OMS_WORKER_")):
            environment.pop(key, None)
    for key in ("OMS_AGENT", "OMS_AGENT_ATTEMPT_ID", "OMS_AGENT_TASK_ID", "OMS_PEER_ATTEMPT_ID"):
        environment.pop(key, None)
    return environment


def execute(request):
    try:
        result = subprocess.run(request["argv"], cwd=request["cwd"], env=_environment(), check=False)
        return result.returncode
    except OSError:
        raise CoreError("native CLI could not start; prepared files and source session were preserved") from None


def _child_session(source, cwd, before, expected):
    items, stats = discover(cwd, [(source["provider"], Path(source["root"]), 4)])
    if stats["truncated"]:
        return None
    eligible = []
    for item in items:
        if item["session_id"] == source["session_id"] or item["session_id"] in before:
            continue
        if expected:
            if item["session_id"] == expected:
                eligible.append(item)
            continue
        # A concurrently created session in this directory is not necessarily
        # our child. Require the native provider's explicit source link.
        try:
            first = json.loads(read_source(item).splitlines()[0])
            if source["provider"] == "codex":
                linked = first.get("payload", {}).get("forked_from_id") == source["session_id"]
            else:
                linked = first.get("parentSession") == source["path"]
            if linked:
                eligible.append(item)
        except (CoreError, ValueError, IndexError, AttributeError):
            continue
    return eligible[0]["session_id"] if len(eligible) == 1 else None


def launch_fork(repo, row, apply=False):
    if row["status"] != "prepared" or row.get("conversation_status") != "not_started":
        raise CoreError("this fork has already been launched or preparation failed; use its recorded native session to resume")
    target = fork_target(repo, row)
    source = row["source"]
    if sha256_bytes(read_source(source)) != source["sha256"]:
        raise CoreError("source conversation changed after preparation; create a new fork from the current session")
    request = plan(source, target, fork=True)
    if not apply:
        return dict(request, id=row["id"], worktree=str(target))
    existing, stats = discover(target, [(source["provider"], Path(source["root"]), 4)])
    if stats["truncated"]:
        raise CoreError("native discovery is incomplete; narrow the transcript root before launch")
    row = transition(repo, "forks", row["id"], {"status": "prepared", "conversation_status": "not_started",
                                               "worktree": row["worktree"], "source": row["source"]},
                     {"conversation_status": "launch_requested", "status": "launched", "launched_at": utc_now(),
                      "expected_native_session": request["expected_native_session"]})
    # Once execution begins, do not retry automatically: even a failing native
    # process may have created a new conversation or made file edits.
    try:
        code = execute(request)
    except CoreError:
        row.update(conversation_status="start_failed")
        save(repo, "forks", row)
        raise
    row.update(conversation_status="exited", native_returncode=code,
               native_session=_child_session(source, target, {item["session_id"] for item in existing},
                                             request["expected_native_session"]))
    save(repo, "forks", row)
    return dict(row, dry_run=False)


def conversation(repo, source, *, fork=False, apply=False):
    request = plan(source, repo, fork=fork)
    if not apply:
        return request
    row = dict(base_record(repo, "conversation"), source=source, action=request["action"],
               conversation_status="launch_requested", files_restored=False)
    save(repo, "conversations", row)
    code = execute(request)
    row.update(conversation_status="exited", native_returncode=code)
    save(repo, "conversations", row)
    return dict(row, dry_run=False,
                note="OMS did not restore files. The native interactive session can edit files after launch.")
