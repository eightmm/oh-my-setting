#!/usr/bin/env python3
"""Shared hook state for oh-my-setting prompt routing and turn guards."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


SKIP_PREFIXES = ("<", "/")
READ_TERMS = (
    "review",
    "assess",
    "evaluate",
    "explain",
    "compare",
    "inspect",
    "audit",
    "summarize",
    "analyse",
    "analyze",
    "investigate",
    "검토",
    "평가",
    "분석",
    "리뷰",
    "설명",
    "조사",
    "비교",
)
WRITE_TERMS = (
    "add",
    "implement",
    "fix",
    "change",
    "modify",
    "update",
    "refactor",
    "remove",
    "delete",
    "create",
    "generate",
    "write",
    "apply",
    "install",
    "scaffold",
    "build",
    "구현",
    "수정",
    "추가",
    "변경",
    "삭제",
    "제거",
    "고쳐",
    "만들",
    "작성",
    "적용",
    "설치",
    "업데이트",
    "진행",
)
REVIEW_TERMS = ("review", "audit", "검토", "리뷰")
ML_TERMS = (
    "ml",
    "machine learning",
    "training",
    "train",
    "hyperparameter",
    "experiment",
    "slurm",
    "dataset",
    "leakage",
    "학습",
    "실험",
    "데이터",
)
RELEASE_TERMS = (
    "commit",
    "push",
    "release",
    "deploy",
    "publish",
    "pr",
    "pull request",
    "autoupdate",
    "auto-update",
    "ci",
    "커밋",
    "푸시",
    "배포",
    "릴리즈",
)
HIGH_RISK_TERMS = RELEASE_TERMS + (
    "auth",
    "secret",
    "token",
    "credential",
    "database",
    "schema",
    "migration",
    "dependency",
    "hook",
    "plugin",
    "api",
    "slurm",
    "checkpoint",
    "인증",
    "시크릿",
    "토큰",
    "스키마",
    "마이그레이션",
    "의존성",
    "훅",
    "플러그인",
)
VERIFICATION_DISCLOSURE_RE = re.compile(
    r"(?:\b(?:verification|verified|tests?|not verified)"
    r"|(?:검증|테스트|미검증|검증하지 않음))"
    r"\s*[:：]\s*\S",
    re.IGNORECASE,
)
CHITCHAT_RE = re.compile(
    r"^\s*(hi|hello|hey|thanks|thank you|안녕|안녕하세요|고마워|고맙|감사)\b",
    re.IGNORECASE,
)
GOAL_RE = re.compile(r"^\s*(goal|objective|목표)\s*[:：]\s*(.+)$", re.IGNORECASE)
# Live-peer detection bounds, matching the SessionStart advisory in
# resume-hook.sh: same 15-minute window, same bounded tail of the ledger.
PEER_WINDOW_SEC = 900
PEER_TAIL_ROWS = 200
PEER_LATCH_MAX = 16


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8", "replace")).hexdigest()


def load_payload() -> tuple[dict[str, Any], str]:
    raw = os.environ.get("OMS_HOOK_PAYLOAD")
    if raw is None:
        raw = sys.stdin.read()
    try:
        payload = json.loads(raw or "{}")
    except Exception:
        return {}, raw or ""
    if not isinstance(payload, dict):
        return {}, raw or ""
    return payload, raw or ""


def payload_cwd(payload: dict[str, Any]) -> str:
    cwd = payload.get("cwd") or payload.get("currentWorkingDirectory") or ""
    return str(cwd) if cwd else ""


def repo_root(cwd: str) -> Path | None:
    if not cwd:
        return None
    path = Path(cwd).expanduser()
    if not path.is_dir():
        return None
    try:
        proc = subprocess.run(
            ["git", "-C", str(path), "rev-parse", "--show-toplevel"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=2,
        )
        if proc.returncode == 0 and proc.stdout.strip():
            return Path(proc.stdout.strip())
    except Exception:
        pass
    try:
        return path.resolve()
    except Exception:
        return path


def is_harness_child() -> bool:
    return os.environ.get("OMS_HARNESS_CHILD") == "1"


def hook_repo(payload: dict[str, Any]) -> Path | None:
    """Resolve child events to primary state, never the delegated worktree."""
    if is_harness_child():
        state_repo = os.environ.get("OMS_STATE_REPO", "")
        if state_repo:
            return repo_root(state_repo)
    return repo_root(payload_cwd(payload))


def child_event_fields() -> dict[str, str]:
    def safe(name: str, default: str) -> str:
        value = os.environ.get(name, default)
        return value if re.match(r"^[A-Za-z0-9._:-]{1,160}$", value) else default

    return {
        "origin": safe("OMS_HARNESS_ORIGIN", "unknown"),
        "parent_agent": safe("OMS_HARNESS_PARENT_AGENT", "unknown"),
        "call_id": safe("OMS_HARNESS_CALL_ID", "") if os.environ.get("OMS_HARNESS_CALL_ID") else "",
    }


def ensure_oms(repo: Path) -> Path:
    oms = repo / ".oms"
    oms.mkdir(parents=True, exist_ok=True)
    ignore = oms / ".gitignore"
    if not ignore.exists():
        ignore.write_text("*\n", encoding="utf-8")
    hooks = oms / "hooks"
    hooks.mkdir(parents=True, exist_ok=True)
    (hooks / "sessions").mkdir(parents=True, exist_ok=True)
    return hooks


def session_hash(payload: dict[str, Any]) -> str:
    session = str(payload.get("session_id") or payload.get("sessionId") or "nosession")
    return sha256_text(session)[:32]


def session_state_path(hooks_dir: Path, payload: dict[str, Any]) -> Path:
    return hooks_dir / "sessions" / f"{session_hash(payload)}.json"


def task_route_state_path(hooks_dir: Path, payload: dict[str, Any]) -> Path:
    return hooks_dir / "sessions" / f"{session_hash(payload)}.task.json"


def load_state(path: Path) -> dict[str, Any]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return {}
    return data if isinstance(data, dict) else {}


def write_json_atomic(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=path.name + ".", suffix=".tmp", dir=str(path.parent))
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False, sort_keys=True)
            handle.write("\n")
        os.replace(tmp, path)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


@contextlib.contextmanager
def event_file_lock(path: Path, timeout: float = 1.0):
    """Portable short lock shared by hook appends and GC compaction."""
    lock = path.parent / ".events-lockdir"
    deadline = time.monotonic() + timeout
    while True:
        try:
            lock.mkdir()
            break
        except FileExistsError:
            try:
                stale = time.time() - lock.stat().st_mtime > 60
            except OSError:
                stale = False
            if stale:
                displaced = lock.parent / (lock.name + ".stale.%d" % os.getpid())
                try:
                    os.replace(lock, displaced)
                    shutil.rmtree(displaced, ignore_errors=True)
                    continue
                except OSError:
                    pass
            if time.monotonic() >= deadline:
                raise TimeoutError("hook event file is busy")
            time.sleep(0.02)
    try:
        yield
    finally:
        shutil.rmtree(lock, ignore_errors=True)


def append_event(repo: Path | None, payload: dict[str, Any], **fields: Any) -> None:
    if repo is None:
        return
    try:
        hooks_dir = ensure_oms(repo)
        raw_turn = payload.get("turn_id") or payload.get("turnId") or ""
        turn_id = bounded_name(raw_turn, 120)
        row = {
            "schema": 1,
            "ts": utc_now(),
            "agent": bounded_name(os.environ.get("OMS_AGENT"), 40) or "hook",
            "hook": bounded_name(
                payload.get("hook_event_name") or payload.get("hookEventName"), 80
            ) or "unknown",
            "session": session_hash(payload),
            "turn_id": turn_id,
            "cwd_hash": sha256_text(payload_cwd(payload))[:16] if payload_cwd(payload) else "",
        }
        if raw_turn and not turn_id:
            row["turn_id_hash"] = sha256_text(str(raw_turn))[:16]
        row.update({k: v for k, v in fields.items() if v is not None})
        events_path = hooks_dir / "events.jsonl"
        with event_file_lock(events_path):
            with events_path.open("a", encoding="utf-8") as handle:
                json.dump(row, handle, ensure_ascii=False, sort_keys=True)
                handle.write("\n")
    except Exception:
        return


def bounded_name(value: Any, limit: int = 120) -> str:
    """Return one content-free identifier, never arbitrary hook text."""
    if isinstance(value, dict):
        value = value.get("id") or value.get("name") or value.get("display_name") or ""
    if not isinstance(value, (str, int, float)):
        return ""
    text = str(value).strip()
    if not text or len(text) > limit or not re.fullmatch(r"[A-Za-z0-9_.:+() /-]+", text):
        return ""
    return text


def nonnegative_number(value: Any) -> int | float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value < 0:
        return None
    if isinstance(value, float) and not value.is_integer():
        return round(value, 3)
    return int(value)


def usage_containers(payload: dict[str, Any]) -> list[dict[str, Any]]:
    containers = [payload]
    for key in ("usage", "token_usage", "tokenUsage", "model_usage", "modelUsage", "metrics"):
        value = payload.get(key)
        if isinstance(value, dict):
            containers.append(value)
    response = payload.get("tool_response") or payload.get("toolResponse")
    if isinstance(response, dict):
        nested = response.get("usage")
        if isinstance(nested, dict):
            containers.append(nested)
    return containers


def first_metric(containers: list[dict[str, Any]], *names: str) -> int | float | None:
    for container in containers:
        for name in names:
            value = nonnegative_number(container.get(name))
            if value is not None:
                return value
    return None


def cmd_telemetry(_: argparse.Namespace) -> int:
    payload, _ = load_payload()
    repo = hook_repo(payload)
    # Hooks do not adopt arbitrary repositories merely because an agent opened
    # one. `oms init` is the explicit ownership boundary.
    if repo is None or not (repo / ".oms").is_dir():
        return 0

    containers = usage_containers(payload)
    raw_response = payload.get("tool_response") or payload.get("toolResponse")
    response = raw_response if isinstance(raw_response, dict) else {}
    fields: dict[str, Any] = {"action": "telemetry"}

    tool_name = bounded_name(payload.get("tool_name") or payload.get("toolName"), 80)
    model = bounded_name(payload.get("model") or payload.get("model_name") or payload.get("modelName"), 160)
    subagent_type = bounded_name(
        payload.get("subagent_type") or payload.get("subagentType") or payload.get("agent_type"),
        80,
    )
    if tool_name:
        fields["tool_name"] = tool_name
    if model:
        fields["model"] = model
    if subagent_type:
        fields["subagent_type"] = subagent_type

    metrics = (
        (("duration_ms", "durationMs"), "duration_ms"),
        (("input_tokens", "inputTokens", "prompt_tokens"), "input_tokens"),
        (("output_tokens", "outputTokens", "completion_tokens"), "output_tokens"),
        (("cache_read_input_tokens", "cache_read_tokens", "cacheReadTokens"), "cache_read_tokens"),
        (("cache_creation_input_tokens", "cache_creation_tokens", "cacheCreationTokens"), "cache_creation_tokens"),
        (("reasoning_tokens", "reasoning_output_tokens", "reasoningTokens"), "reasoning_tokens"),
        (("cost_usd", "total_cost_usd", "costUsd"), "cost_usd"),
    )
    for source_names, target in metrics:
        value = first_metric(containers, *source_names)
        if value is None and target == "duration_ms":
            value = first_metric([response], *source_names)
        if value is not None:
            fields[target] = value

    success = payload.get("success")
    if not isinstance(success, bool):
        success = response.get("success")
    if isinstance(success, bool):
        fields["success"] = success

    for source, target in (
        (payload.get("agent_id") or payload.get("agentId"), "agent_id_hash"),
        (payload.get("parent_agent_id") or payload.get("parentAgentId"), "parent_agent_id_hash"),
    ):
        if isinstance(source, (str, int)) and str(source):
            fields[target] = sha256_text(str(source))[:16]

    append_event(repo, payload, **fields)
    return 0


def cmd_compact_events(args: argparse.Namespace) -> int:
    """Drop parseable old transient hook rows under the append lock."""
    path = Path(args.path)
    if path.is_symlink() or not path.is_file():
        raise RuntimeError("hook event path is missing or unsafe")
    with event_file_lock(path, timeout=5.0):
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines(True)

        def old(line: str) -> bool:
            try:
                row = json.loads(line)
                value = row.get("ts", "") if isinstance(row, dict) else ""
                normalized = value[:-1] + "+00:00" if value.endswith("Z") else value
                stamp = datetime.fromisoformat(normalized)
                if stamp.tzinfo is None:
                    stamp = stamp.replace(tzinfo=timezone.utc)
                return int(stamp.timestamp()) < args.cutoff
            except (AttributeError, TypeError, ValueError):
                return False

        kept = [line for line in lines if not old(line)]
        if args.apply and len(kept) != len(lines):
            fd, temporary = tempfile.mkstemp(
                prefix=".oms-replace.", dir=str(path.parent)
            )
            try:
                with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as handle:
                    handle.writelines(kept)
                os.replace(temporary, path)
            finally:
                if os.path.exists(temporary):
                    os.unlink(temporary)
    print("%d\t%d" % (len(lines), len(kept)))
    return 0


def term_matches(text: str, term: str) -> bool:
    """Keep localized substring matching, but bound ASCII routing terms."""
    if not term.isascii():
        return term in text
    pattern = r"(?<![A-Za-z0-9_])" + re.escape(term) + r"(?![A-Za-z0-9_])"
    return re.search(pattern, text) is not None


def has_any(text: str, terms: tuple[str, ...]) -> bool:
    return any(term_matches(text, term) for term in terms)


def classify_prompt(prompt: str) -> dict[str, Any]:
    lower = prompt.lower()
    write = has_any(lower, WRITE_TERMS)
    read = has_any(lower, READ_TERMS)
    if has_any(lower, RELEASE_TERMS) and (write or not read):
        workflow = "release"
    elif has_any(lower, ML_TERMS):
        workflow = "ml-experiment" if write else "research"
    elif has_any(lower, REVIEW_TERMS):
        workflow = "review"
    elif write:
        workflow = "task"
    elif read:
        workflow = "research"
    else:
        workflow = "question"

    high = (write and has_any(lower, HIGH_RISK_TERMS)) or workflow in {"release", "ml-experiment"}
    risk = "high" if high else "medium" if workflow in {"task", "review"} else "low"
    guard = workflow in {"task", "review", "ml-experiment", "release"}
    return {"workflow": workflow, "risk": risk, "guard": guard}


def should_skip_prompt(prompt: str) -> bool:
    stripped = prompt.strip()
    return not stripped or stripped.startswith(SKIP_PREFIXES) or len(stripped) < 4


def env_int(name: str, default: int, minimum: int = 0, maximum: int | None = None) -> int:
    try:
        value = int(os.environ.get(name, str(default)) or default)
    except ValueError:
        value = default
    value = max(minimum, value)
    if maximum is not None:
        value = min(maximum, value)
    return value


def prompt_excerpt(prompt: str) -> str:
    limit = env_int("OMS_AUTO_TASK_PROMPT_CHARS", 600, minimum=80, maximum=4000)
    text = re.sub(r"\s+", " ", prompt.strip())
    if len(text) > limit:
        return text[: max(0, limit - 3)].rstrip() + "..."
    return text


def prompt_goal(prompt: str) -> str:
    first = prompt.strip().splitlines()[0] if prompt.strip() else ""
    match = GOAL_RE.match(first)
    if match:
        return prompt_excerpt(match.group(2))
    return ""


def prompt_has_content_after_goal(prompt: str) -> bool:
    lines = prompt.strip().splitlines()
    if not lines or not GOAL_RE.match(lines[0]):
        return False
    return any(line.strip() for line in lines[1:])


def active_task_file(repo: Path) -> Path:
    return repo / ".oms" / "task" / "current.md"


def task_metadata(path: Path) -> dict[str, str]:
    metadata: dict[str, str] = {}
    try:
        for raw in path.read_text(encoding="utf-8", errors="replace").splitlines():
            if raw.startswith("## "):
                break
            match = re.match(r"^- ([a-z_]+):\s*(.*)$", raw)
            if match:
                metadata[match.group(1)] = match.group(2).strip()
    except Exception:
        return {}
    return metadata


def task_goal(path: Path) -> str:
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except Exception:
        return ""
    in_goal = False
    goal: list[str] = []
    for raw in lines:
        if raw == "## Goal":
            in_goal = True
            continue
        if in_goal and raw.startswith("## "):
            break
        if in_goal and raw.strip():
            goal.append(raw.strip())
    return " ".join(goal)


def normalized_goal(value: str) -> str:
    return re.sub(r"\s+", " ", value.strip()).casefold()


def task_is_stale(metadata: dict[str, str]) -> bool:
    ttl = env_int("OMS_AGENT_TASK_TTL", 604800, minimum=0, maximum=315360000)
    if ttl == 0:
        return True
    value = metadata.get("last_activity") or metadata.get("updated") or ""
    if not value:
        return False
    try:
        then = datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    except ValueError:
        return False
    return (datetime.now(timezone.utc) - then).total_seconds() >= ttl


def agent_task_script() -> Path:
    return Path(__file__).resolve().parent.parent / "agent-task.sh"


def run_agent_task(repo: Path, args: list[str], stdin_text: str | None = None) -> int:
    script = agent_task_script()
    if not script.exists():
        return 127
    timeout = env_int("OMS_AUTO_TASK_TIMEOUT", 2, minimum=1, maximum=10)
    try:
        proc = subprocess.run(
            [str(script), "--repo", str(repo), *args],
            input=stdin_text,
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return 124
    except Exception:
        return 1
    return int(proc.returncode)


def should_auto_task(prompt: str) -> bool:
    if is_harness_child():
        return False
    if os.environ.get("OMS_AUTO_TASK_OFF") == "1":
        return False
    return os.environ.get("OMS_AUTO_TASK") == "1" and not bool(CHITCHAT_RE.match(prompt.strip()))


def auto_task_record(payload: dict[str, Any], prompt: str, route: dict[str, Any]) -> None:
    if not should_auto_task(prompt):
        return
    repo = hook_repo(payload)
    if repo is None:
        return
    try:
        hooks_dir = ensure_oms(repo)
        state_path = task_route_state_path(hooks_dir, payload)
        prompt_hash = sha256_text(prompt)
        turn_id = str(payload.get("turn_id") or payload.get("turnId") or "")
        previous = load_state(state_path)
        if previous.get("prompt_hash") == prompt_hash:
            append_event(
                repo,
                payload,
                action="auto_task",
                status="deduped",
                workflow=route["workflow"],
                risk=route["risk"],
                prompt_hash=prompt_hash,
            )
            return

        excerpt = prompt_excerpt(prompt)
        if not excerpt:
            return
        explicit_goal = prompt_goal(prompt)
        task_file = active_task_file(repo)
        existed = task_file.exists() and task_file.stat().st_size > 0
        agent = os.environ.get("OMS_AGENT") or "hook"
        status = "appended" if existed else "created"
        source_session = session_hash(payload)

        if existed:
            metadata = task_metadata(task_file)
            task_status = metadata.get("status") or "active"
            old_source = metadata.get("source_session") or ""
            stale = task_is_stale(metadata)
            same_explicit_goal = bool(
                explicit_goal
                and normalized_goal(explicit_goal) == normalized_goal(task_goal(task_file))
            )
            if same_explicit_goal and not prompt_has_content_after_goal(prompt):
                write_json_atomic(
                    state_path,
                    {
                        "schema": 1,
                        "updated_at": utc_now(),
                        "session": session_hash(payload),
                        "turn_id": turn_id,
                        "prompt_hash": prompt_hash,
                        "status": "deduped",
                    },
                )
                append_event(
                    repo,
                    payload,
                    action="auto_task",
                    status="deduped",
                    workflow=route["workflow"],
                    risk=route["risk"],
                    prompt_hash=prompt_hash,
                    task=".oms/task/current.md",
                )
                return
            # Session changes are not a task boundary: the packet exists to
            # hand active work across sessions/providers. Rotate only on an
            # explicit goal/lifecycle boundary or deterministic inactivity TTL.
            rotate = (
                task_status in {"verified", "closed"}
                or stale
                or bool(explicit_goal and not same_explicit_goal)
            )
            if rotate:
                goal = explicit_goal or f"Respond to user request: {excerpt}"
                rc = run_agent_task(
                    repo,
                    ["rotate", "--goal", goal, "--source-session", source_session,
                     "--next", "Respond to the latest user request."],
                )
                if rc != 0:
                    status = "timeout" if rc == 124 else "skipped_sensitive_or_error"
                    append_event(
                        repo,
                        payload,
                        action="auto_task",
                        status=status,
                        workflow=route["workflow"],
                        risk=route["risk"],
                        prompt_hash=prompt_hash,
                    )
                    return
                status = "rotated"
            elif not old_source:
                run_agent_task(repo, ["update", "--source-session", source_session])

        if not existed:
            goal = prompt_goal(prompt) or f"Respond to user request: {excerpt}"
            rc = run_agent_task(
                repo,
                ["init", "--goal", goal, "--source-session", source_session,
                 "--next", "Respond to the latest user request."],
            )
            if rc != 0:
                status = "timeout" if rc == 124 else "skipped_sensitive_or_error"
                append_event(
                    repo,
                    payload,
                    action="auto_task",
                    status=status,
                    workflow=route["workflow"],
                    risk=route["risk"],
                    prompt_hash=prompt_hash,
                )
                write_json_atomic(
                    state_path,
                    {
                        "schema": 1,
                        "updated_at": utc_now(),
                        "session": session_hash(payload),
                        "turn_id": turn_id,
                        "prompt_hash": prompt_hash,
                        "status": status,
                    },
                )
                return

        note = f"User prompt ({route['workflow']}/{route['risk']}): {excerpt}"
        rc = run_agent_task(repo, ["append", "--agent", agent, "--stdin"], note + "\n")
        if rc == 0:
            run_agent_task(repo, ["update", "--next", "Respond to the latest user request."])
        else:
            status = "timeout" if rc == 124 else "skipped_sensitive_or_error"

        write_json_atomic(
            state_path,
            {
                "schema": 1,
                "updated_at": utc_now(),
                "session": session_hash(payload),
                "turn_id": turn_id,
                "prompt_hash": prompt_hash,
                "status": status,
            },
        )
        append_event(
            repo,
            payload,
            action="auto_task",
            status=status,
            workflow=route["workflow"],
            risk=route["risk"],
            prompt_hash=prompt_hash,
            task=".oms/task/current.md",
        )
    except Exception:
        return


def route_state(payload: dict[str, Any], prompt: str, route: dict[str, Any]) -> None:
    repo = hook_repo(payload)
    if repo is None:
        return
    hooks_dir = repo / ".oms" / "hooks"
    state_path = session_state_path(hooks_dir, payload)
    # A read-only prompt must replace this session's prior guarded route, or a
    # release/task request remains armed and blocks unrelated later answers.
    # Do not adopt a new repository merely because somebody asked a question.
    if not route["guard"] and not state_path.is_file():
        return
    if route["guard"]:
        hooks_dir = ensure_oms(repo)
        state_path = session_state_path(hooks_dir, payload)
    previous = load_state(state_path)
    turn_id = str(payload.get("turn_id") or payload.get("turnId") or "")
    previous_turn = str(previous.get("turn_id") or "")
    # Without a payload turn id both sides are empty, so the old equality read
    # every prompt as the same turn and carried a spent block budget forever.
    same_turn = bool(turn_id) and previous_turn == turn_id
    # A routed prompt is the one stable turn boundary every host shares, so a
    # monotonic route sequence gives guard telemetry a content-free per-turn
    # identity even when the Stop payload carries no id. Telemetry only: the
    # block budget stays keyed by guard_turn_key.
    route_seq = previous.get("route_seq")
    if isinstance(route_seq, bool) or not isinstance(route_seq, int) or route_seq < 0:
        route_seq = 0
    state = {
        "schema": 1,
        "updated_at": utc_now(),
        "session": session_hash(payload),
        "turn_id": turn_id,
        "prompt_hash": sha256_text(prompt),
        "prompt_length": len(prompt),
        "workflow": route["workflow"],
        "risk": route["risk"],
        "guard": bool(route["guard"]),
        "guard_blocks": previous.get("guard_blocks", {}) if same_turn else {},
        "stop_seq": previous.get("stop_seq", 0),
        "route_seq": route_seq if same_turn else route_seq + 1,
    }
    write_json_atomic(state_path, state)
    append_event(
        repo,
        payload,
        action="route",
        status="recorded",
        workflow=route["workflow"],
        risk=route["risk"],
        guard=route["guard"],
        prompt_hash=state["prompt_hash"],
    )


def load_skills(manifest_path: str) -> list[dict[str, Any]]:
    try:
        data = json.loads(Path(manifest_path).read_text(encoding="utf-8"))
    except Exception:
        return []
    skills = data.get("skills") if isinstance(data, dict) else []
    return [s for s in skills if isinstance(s, dict)]


def fresh_skill_names(payload: dict[str, Any], scored_names: list[str]) -> list[str]:
    max_n = int(os.environ.get("OMS_ROUTER_MAX", "2") or 2)
    names = scored_names[:max_n]
    session = str(payload.get("session_id") or payload.get("sessionId") or "nosession")[:64]
    safe = "".join(c for c in session if c.isalnum() or c in "-_") or "nosession"
    turn = str(payload.get("turn_id") or payload.get("turnId") or "")[:64]
    if not turn:
        # Without a stable turn identifier, persistent dedupe would suppress an
        # identical request in every later turn. Prefer a repeated hint over a
        # false session-wide suppression.
        return names
    safe_turn = "".join(c for c in turn if c.isalnum() or c in "-_") or "noturn"
    state_dir = Path(os.environ.get("TMPDIR", "/tmp")) / f"oms-skill-router.{os.getuid()}"
    state = state_dir / f"{safe}.{safe_turn}"
    seen: set[str] = set()
    try:
        seen = {line.strip() for line in state.read_text(encoding="utf-8").splitlines()}
    except Exception:
        pass
    fresh = [name for name in names if name not in seen]
    if fresh:
        try:
            state_dir.mkdir(parents=True, exist_ok=True)
            with state.open("a", encoding="utf-8") as handle:
                for name in fresh:
                    handle.write(name + "\n")
        except Exception:
            pass
    return fresh


# Context pressure: an advisory, never a control decision. The 2026-08-06
# cross-family debate on the codex-context-checkpoint plugin converged on
# keeping the one valuable piece — a measured early warning plus durable
# capture — and rejecting forced migration, per-turn HUD injection into model
# context, and prompt-only handoffs. Migration stays the agent's call.


def hud_cache_dir() -> Path:
    cache_dir = os.environ.get("OMS_HUD_CACHE_DIR", "").strip()
    if not cache_dir:
        cache_dir = os.path.join(tempfile.gettempdir(), "oh-my-setting-hud")
    return Path(cache_dir)


def percent_left_from_cache(payload: dict[str, Any]) -> float | None:
    """Read the statusline's authoritative context reading, TTL-bounded."""
    session = str(payload.get("session_id") or payload.get("sessionId") or "")
    if not session:
        return None
    path = hud_cache_dir() / ("ctx-%s.json" % sha256_text(session)[:24])
    ttl = env_int("OMS_CTX_CACHE_TTL", 600, minimum=5, maximum=86400)
    try:
        if time.time() - path.stat().st_mtime > ttl:
            return None
        data = json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return None
    used = data.get("used_percentage") if isinstance(data, dict) else None
    if isinstance(used, bool) or not isinstance(used, (int, float)):
        return None
    if not 0 <= used <= 100:
        return None
    return 100.0 - float(used)


def percent_left_from_codex_transcript(payload: dict[str, Any]) -> float | None:
    """Tail-scan a codex rollout for its latest token_count event.

    The rollout format is not a stable interface, so every shape mismatch
    means "no reading", never an error.
    """
    transcript = payload.get("transcript_path") or payload.get("transcriptPath") or ""
    if not isinstance(transcript, str) or not transcript:
        return None
    path = Path(transcript)
    try:
        if not path.is_file():
            return None
        size = path.stat().st_size
        with path.open("rb") as handle:
            if size > 262144:
                handle.seek(size - 262144)
            tail = handle.read().decode("utf-8", "replace")
    except OSError:
        return None
    for line in reversed(tail.splitlines()):
        if '"token_count"' not in line:
            continue
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if not isinstance(row, dict):
            continue
        event = row.get("payload") if isinstance(row.get("payload"), dict) else row
        if event.get("type") != "token_count":
            continue
        info = event.get("info")
        if not isinstance(info, dict):
            continue
        last = info.get("last_token_usage")
        used = last.get("total_tokens") if isinstance(last, dict) else None
        window = info.get("model_context_window")
        if (
            not isinstance(used, bool)
            and isinstance(used, (int, float))
            and used >= 0
            and not isinstance(window, bool)
            and isinstance(window, (int, float))
            and window > 0
        ):
            return max(0.0, min(100.0, (window - used) / window * 100.0))
    return None


def ctx_state_path(hooks_dir: Path, payload: dict[str, Any]) -> Path:
    return hooks_dir / "sessions" / f"{session_hash(payload)}.ctx.json"


def start_handoff_capture(repo: Path, payload: dict[str, Any], agent: str, left: float,
                          note: str | None = None) -> bool:
    """Detach a mechanical digest capture; the advisory never waits on it."""
    if os.environ.get("OMS_CTX_CAPTURE", "1") != "1":
        return False
    script = Path(__file__).resolve().parent.parent / "session-handoff.sh"
    if not script.exists():
        return False
    command = [
        "bash",
        str(script),
        "capture",
        "--agent",
        agent,
        "--cwd",
        payload_cwd(payload) or str(repo),
        "--note",
        note or "auto: context pressure (~%d%% left)" % int(left),
    ]
    session = str(payload.get("session_id") or payload.get("sessionId") or "")
    if session:
        command += ["--session", session]
    try:
        subprocess.Popen(
            command,
            cwd=str(repo),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
    except Exception:
        return False
    return True


def peers_state_path(hooks_dir: Path, payload: dict[str, Any]) -> Path:
    # A file of its own, never ctx.json: the pressure advisory's writer
    # full-replaces that document, so a latch co-located there would be wiped
    # on every band transition (and wipe the band stage if written back).
    return hooks_dir / "sessions" / f"{session_hash(payload)}.peers.json"


def event_epoch(value: Any) -> float | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        normalized = value[:-1] + "+00:00" if value.endswith("Z") else value
        stamp = datetime.fromisoformat(normalized)
    except ValueError:
        return None
    if stamp.tzinfo is None:
        stamp = stamp.replace(tzinfo=timezone.utc)
    return stamp.timestamp()


def live_peer_sessions(events: Path, me: str, now: float) -> dict[str, float]:
    """Neighbor session hashes seen recently, this session's children excluded."""
    try:
        with events.open(encoding="utf-8", errors="replace") as handle:
            lines = handle.readlines()[-PEER_TAIL_ROWS:]
    except OSError:
        return {}
    seen: dict[str, float] = {}
    for line in lines:
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if not isinstance(row, dict):
            continue
        session = row.get("session")
        if not isinstance(session, str) or not session or session == me:
            continue
        # Harness children write rows under their own session hashes into the
        # primary repo's ledger, so without this the session's own delegated
        # workers read as a live neighbor on every delegation run.
        if row.get("action") == "ignored_child" or row.get("origin"):
            continue
        when = event_epoch(row.get("ts"))
        if when is None or now - when > PEER_WINDOW_SEC:
            continue
        seen[session] = max(seen.get(session, 0.0), when)
    return seen


def peer_advisory_hint(payload: dict[str, Any]) -> str | None:
    """Tell the incumbent session, once per neighbor, that it is not alone."""
    if os.environ.get("OMS_PEER_ADVISORY", "1") != "1":
        return None
    repo = hook_repo(payload)
    # Adopted repos only: the hook ledger is the evidence, and without .oms
    # there is nowhere to keep the once-per-neighbor latch.
    if repo is None or not (repo / ".oms").is_dir():
        return None
    events = repo / ".oms" / "hooks" / "events.jsonl"
    if not events.is_file():
        return None
    now = time.time()
    me = session_hash(payload)
    peers = live_peer_sessions(events, me, now)
    if not peers:
        return None

    hooks_dir = ensure_oms(repo)
    state_path = peers_state_path(hooks_dir, payload)
    announced = [h for h in load_state(state_path).get("announced", []) if isinstance(h, str)]
    fresh = [h for h in sorted(peers) if h not in announced]
    if not fresh:
        return None
    write_json_atomic(
        state_path,
        {
            "schema": 1,
            "updated_at": utc_now(),
            "session": me,
            "announced": (announced + fresh)[-PEER_LATCH_MAX:],
        },
    )
    append_event(repo, payload, action="peer_advisory", status="announced", peers=len(fresh))
    minutes = max(0, int((now - max(peers[h] for h in fresh)) // 60))
    return (
        "[oms] another session is live in this worktree (last activity ~%dm ago)"
        " — a dirty-tree `git add`/`commit` can pick up its hunks; stage with"
        " `git add -p` or work in a separate worktree." % minutes
    )


def context_pressure_hint(payload: dict[str, Any]) -> str | None:
    if os.environ.get("OMS_CTX_PRESSURE", "1") != "1":
        return None
    repo = hook_repo(payload)
    # Adopted repos only: without .oms there is nowhere to keep the once-per-
    # band latch, and an unlatched advisory would nag on every prompt.
    if repo is None or not (repo / ".oms").is_dir():
        return None
    left = percent_left_from_cache(payload)
    agent = "claude" if left is not None else "codex"
    if left is None:
        left = percent_left_from_codex_transcript(payload)
    if left is None:
        return None

    warn = env_int("OMS_CTX_WARN_PCT", 15, minimum=1, maximum=90)
    urgent = min(env_int("OMS_CTX_URGENT_PCT", 8, minimum=0, maximum=89), warn)
    # A silent digest capture well before the advisory bands (user decision
    # 2026-09-03, superseding the 2026-08-06 council's advisory-only verdict).
    capture = env_int("OMS_CTX_CAPTURE_PCT", 30, minimum=0, maximum=90)
    rearm = max(env_int("OMS_CTX_REARM_PCT", 30, minimum=2, maximum=100), warn, capture)
    hooks_dir = ensure_oms(repo)
    state_path = ctx_state_path(hooks_dir, payload)
    state = load_state(state_path)
    stage = state.get("stage") if state.get("stage") in ("warn", "urgent") else None

    def save(new_stage: str | None, captured: bool = False) -> None:
        write_json_atomic(
            state_path,
            {
                "schema": 1,
                "updated_at": utc_now(),
                "session": session_hash(payload),
                "stage": new_stage,
                "captured": captured,
                "percent_left": round(left, 1),
            },
        )

    if left > rearm:
        # Compaction or a fresh reading recovered the window; re-arm.
        if stage or state.get("captured"):
            save(None)
        return None
    rank = {"warn": 1, "urgent": 2}
    new_stage = "urgent" if left <= urgent else "warn" if left <= warn else None
    if new_stage is None or (stage and rank[new_stage] <= rank[stage]):
        if new_stage is None and left <= capture and not state.get("captured"):
            started = start_handoff_capture(repo, payload, agent, left)
            save(stage, captured=True)
            append_event(repo, payload, action="context_capture", status="early",
                         percent_left=round(left, 1), source=agent,
                         capture="started" if started else "skipped")
        # Between bands, or a band this session already announced; only an
        # escalation (warn -> urgent) speaks again before re-arming.
        return None
    save(new_stage, captured=True)
    captured = start_handoff_capture(repo, payload, agent, left)
    append_event(
        repo,
        payload,
        action="context_pressure",
        status=new_stage,
        percent_left=round(left, 1),
        source=agent,
        capture="started" if captured else "skipped",
    )
    if new_stage == "urgent":
        return (
            "[oms] context nearly exhausted (~%d%% left) — wrap up now: finish or"
            " record the current step, then migrate to a fresh session; do not"
            " start new multi-step work." % int(left)
        )
    return (
        "[oms] context low (~%d%% left) — a handoff digest capture %s; finish the"
        " current atomic step, then continue in a fresh session from"
        " `oms session-handoff list` (or keep going deliberately)."
        % (int(left), "was started" if captured else "is worth running")
    )


def cmd_route(args: argparse.Namespace) -> int:
    payload, _ = load_payload()
    if is_harness_child():
        append_event(hook_repo(payload), payload, action="ignored_child", status="route", **child_event_fields())
        return 0
    # Context pressure is orthogonal to prompt content: it must fire even on
    # turns the skill router skips (slash commands, system-ish notifications),
    # because near exhaustion those may be the only turns left.
    try:
        pressure = context_pressure_hint(payload)
    except Exception:
        pressure = None
    if pressure:
        print(pressure)
    # A neighbor that joined after this session started is invisible to the
    # SessionStart advisory, which only ever warns the newcomer. Kept in its
    # own try so a raising advisory cannot silence the other.
    try:
        peers = peer_advisory_hint(payload)
    except Exception:
        peers = None
    if peers:
        print(peers)
    prompt = str(payload.get("prompt") or "")
    if should_skip_prompt(prompt):
        return 0

    route = classify_prompt(prompt)
    route_state(payload, prompt, route)
    auto_task_record(payload, prompt, route)

    lower = prompt.strip().lower()
    scored: list[tuple[int, str]] = []
    for skill in load_skills(args.manifest):
        if not skill.get("enabled") or not skill.get("triggers"):
            continue
        # Machine-conditional skills are not linked where their required
        # commands are absent; suggesting one there would name a skill the
        # session cannot load.
        if any(
            shutil.which(str(req)) is None
            for req in (skill.get("requires") or [])
        ):
            continue
        triggers = [str(t).lower() for t in skill.get("triggers", [])]
        hits = sum(1 for trigger in triggers if trigger in lower)
        if hits:
            scored.append((-hits, str(skill.get("name") or "")))
    if not scored:
        return 0
    scored.sort()
    fresh = fresh_skill_names(payload, [name for _, name in scored if name])
    repo = repo_root(payload_cwd(payload))
    if route["guard"]:
        append_event(
            repo,
            payload,
            action="skill_hint",
            status="hinted" if fresh else "deduped",
            workflow=route["workflow"],
            risk=route["risk"],
            skills=fresh,
        )
    if fresh:
        print("oh-my-setting skill hint: " + ", ".join(fresh))
    return 0


def git_dirty(repo: Path) -> bool:
    try:
        proc = subprocess.run(
            ["git", "-C", str(repo), "status", "--short"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=3,
        )
    except Exception:
        return False
    return bool(proc.stdout.strip())


# Shared wording with turn-guard.sh, which reports the failures that never
# reach this process at all.
GUARD_UNAVAILABLE = "oh-my-setting turn guard: unavailable (%s); this turn was not checked."


def max_blocks_per_turn() -> int:
    raw = os.environ.get("OMS_TURN_GUARD_MAX_BLOCKS_PER_TURN", "1")
    try:
        return max(0, int(raw))
    except ValueError:
        return 1


def assistant_message(payload: dict[str, Any]) -> str:
    for key in ("last_assistant_message", "lastAssistantMessage", "message"):
        value = payload.get(key)
        if isinstance(value, str):
            return value
    return ""


def has_verification_disclosure(message: str) -> bool:
    """Require an explicit verification-status field, not a keyword mention."""
    return bool(VERIFICATION_DISCLOSURE_RE.search(message))


def guard_turn_key(payload: dict[str, Any], state: dict[str, Any]) -> tuple[str, str, int]:
    """Key the block budget per turn even when the Stop payload carries no id.

    Claude Code's Stop payload has no turn identifier, so an empty key spent the
    whole session's budget on the first block. A Stop that continues a blocked
    turn is marked stop_hook_active; only an unmarked Stop opens the next turn,
    which keeps the cap a loop fuse instead of a per-event allowance.
    """
    seq = state.get("stop_seq")
    if isinstance(seq, bool) or not isinstance(seq, int) or seq < 0:
        seq = 0
    raw = str(payload.get("turn_id") or payload.get("turnId") or "")
    if raw:
        return raw, "payload", seq
    continuing = bool(payload.get("stop_hook_active") or payload.get("stopHookActive"))
    seq = max(seq, 1) if continuing else seq + 1
    return "stop-%d" % seq, "counter", seq


def cmd_guard(_: argparse.Namespace) -> int:
    if os.environ.get("OMS_TURN_GUARD_OFF") == "1":
        return 0
    payload, _ = load_payload()
    repo = hook_repo(payload)
    if is_harness_child():
        append_event(repo, payload, action="ignored_child", status="turn_guard", **child_event_fields())
        return 0
    if repo is None:
        return 0
    hooks_dir = ensure_oms(repo)
    state_path = session_state_path(hooks_dir, payload)
    state = load_state(state_path)
    if not state:
        append_event(repo, payload, action="turn_guard", status="allow_no_state")
        return 0

    budget = session_budget_reason(repo, payload, state, state_path)
    if budget:
        print(json.dumps({"decision": "block", "reason": budget}, ensure_ascii=False))
        return 0

    dirty = git_dirty(repo)
    risk = str(state.get("risk") or "low")
    workflow = str(state.get("workflow") or "unknown")
    guard = bool(state.get("guard"))
    should_guard = guard and (risk == "high" or os.environ.get("OMS_TURN_GUARD_STRICT") == "1")

    # Observation identity for every guard outcome: content-free, and purely
    # telemetry — the block budget below still uses guard_turn_key unchanged.
    # Without a payload turn id, the routed prompt's sequence anchors the key,
    # so a block and its corrected continuation share one identity while the
    # next routed turn opens a new one.
    turn_key, key_source, stop_seq = guard_turn_key(payload, state)
    obs_route_seq = state.get("route_seq")
    if isinstance(obs_route_seq, bool) or not isinstance(obs_route_seq, int) or obs_route_seq < 0:
        obs_route_seq = 0
    if key_source == "payload":
        obs_source = "payload"
        obs_key = turn_key
    else:
        obs_source = "route"
        obs_key = "route-%d:%s" % (obs_route_seq, turn_key)
    turn_obs = sha256_text(session_hash(payload) + ":" + obs_key)[:16]

    if not should_guard:
        append_event(repo, payload, action="turn_guard", status="allow", workflow=workflow, risk=risk, dirty=dirty, turn_obs=turn_obs, turn_obs_source=obs_source, eligible=False)
        return 0

    if has_verification_disclosure(assistant_message(payload)):
        append_event(repo, payload, action="turn_guard", status="allow_verified", workflow=workflow, risk=risk, dirty=dirty, turn_obs=turn_obs, turn_obs_source=obs_source, eligible=True)
        return 0

    blocks = state.get("guard_blocks")
    if not isinstance(blocks, dict):
        blocks = {}
    count = int(blocks.get(turn_key, 0) or 0)
    limit = max_blocks_per_turn()
    if count >= limit:
        append_event(repo, payload, action="turn_guard", status="allow_block_limit", workflow=workflow, risk=risk, dirty=dirty, turn_key_source=key_source, turn_obs=turn_obs, turn_obs_source=obs_source, eligible=True)
        return 0

    blocks[turn_key] = count + 1
    state["guard_blocks"] = blocks
    state["stop_seq"] = stop_seq
    state["updated_at"] = utc_now()
    write_json_atomic(state_path, state)
    append_event(repo, payload, action="turn_guard", status="block_unverified", workflow=workflow, risk=risk, dirty=dirty, turn_key_source=key_source, turn_obs=turn_obs, turn_obs_source=obs_source, eligible=True)
    reason = (
        "oh-my-setting turn guard: high-risk "
        + workflow
        + " result lacks an explicit 'Verification:' or 'Not verified:' line. "
        "Add one with the command/result or reason."
    )
    print(json.dumps({"decision": "block", "reason": reason}, ensure_ascii=False))
    return 0


def session_budget_reason(repo: Path, payload: dict[str, Any], state: dict[str, Any],
                          state_path: Path) -> str | None:
    """Count Stops and wall-clock per routed session; past the budget, block the
    Stop once per further quarter so the model lands what is verified and hands
    off instead of running until the context dies (user decision 2026-09-03)."""
    turns_cap = env_int("OMS_SESSION_BUDGET_TURNS", 200, minimum=0)
    hours_cap = env_int("OMS_SESSION_BUDGET_HOURS", 6, minimum=0)
    started = str(state.get("started_at") or "")
    try:
        began = datetime.strptime(started, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    except ValueError:
        began = datetime.now(timezone.utc)
        state["started_at"] = began.strftime("%Y-%m-%dT%H:%M:%SZ")
    turns = int(state.get("budget_turns") or 0) + 1
    hours = (datetime.now(timezone.utc) - began).total_seconds() / 3600
    state["budget_turns"] = turns
    state["updated_at"] = utc_now()
    band = int(4 * max(turns / turns_cap if turns_cap else 0, hours / hours_cap if hours_cap else 0))
    latched = int(state.get("budget_band") or 0)
    continuing = bool(payload.get("stop_hook_active") or payload.get("stopHookActive"))
    if band >= 4 and band > latched and not continuing:
        state["budget_band"] = band
    write_json_atomic(state_path, state)
    if band < 4 or band <= latched or continuing:
        return None
    captured = start_handoff_capture(repo, payload, "claude", 0.0,
                                     note="auto: session budget (%d turns, %.1fh)" % (turns, hours))
    append_event(repo, payload, action="session_budget", status="block", turns=turns,
                 hours=round(hours, 1), capture="started" if captured else "skipped")
    return (
        "[oms] session budget: %d turns and %.1fh since this session started (budget %d turns / %dh);"
        " a handoff digest capture %s. Land what is verified, summarize the state for the user,"
        " and stop; continue only on an explicit go-ahead." % (
            turns, hours, turns_cap, hours_cap, "was started" if captured else "was skipped"))


def cmd_repo(_: argparse.Namespace) -> int:
    payload, _ = load_payload()
    repo = hook_repo(payload)
    if repo is not None:
        print(repo)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="oh-my-setting hook state helper")
    sub = parser.add_subparsers(dest="cmd", required=True)
    route = sub.add_parser("route")
    route.add_argument("--manifest", required=True)
    route.set_defaults(func=cmd_route)
    guard = sub.add_parser("guard")
    guard.set_defaults(func=cmd_guard)
    telemetry = sub.add_parser("telemetry")
    telemetry.set_defaults(func=cmd_telemetry)
    compact = sub.add_parser("compact-events")
    compact.add_argument("--path", required=True)
    compact.add_argument("--cutoff", required=True, type=int)
    compact.add_argument("--apply", action="store_true")
    compact.set_defaults(func=cmd_compact_events)
    repo = sub.add_parser("repo")
    repo.set_defaults(func=cmd_repo)
    args = parser.parse_args()
    return int(args.func(args))


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        # Provider hooks are fail-open. The maintenance command is explicit and
        # must fail loudly rather than claim it compacted state when it did not.
        if len(sys.argv) > 1 and sys.argv[1] == "compact-events":
            print("error: hook event compaction: %s" % error, file=sys.stderr)
            raise SystemExit(2)
        if len(sys.argv) > 1 and sys.argv[1] == "guard":
            # Fail-open still holds, but the turn nobody guarded must not read
            # as a turn that passed the guard.
            with contextlib.suppress(Exception):
                print(json.dumps({"systemMessage": GUARD_UNAVAILABLE % "helper error"}))
        raise SystemExit(0)
