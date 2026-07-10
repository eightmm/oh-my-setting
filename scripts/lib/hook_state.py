#!/usr/bin/env python3
"""Shared hook state for oh-my-setting prompt routing and turn guards."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
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
VERIFY_RE = re.compile(
    r"\b(verified|verification|not verified|not run|skipped|test|tests|passed|failed|checked)\b"
    r"|검증|테스트|확인|실행|통과|실패|미검증",
    re.IGNORECASE,
)
CHITCHAT_RE = re.compile(
    r"^\s*(hi|hello|hey|thanks|thank you|안녕|안녕하세요|고마워|고맙|감사)\b",
    re.IGNORECASE,
)
GOAL_RE = re.compile(r"^\s*(goal|objective|목표)\s*[:：]\s*(.+)$", re.IGNORECASE)


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


def append_event(repo: Path | None, payload: dict[str, Any], **fields: Any) -> None:
    if repo is None:
        return
    try:
        hooks_dir = ensure_oms(repo)
        row = {
            "schema": 1,
            "ts": utc_now(),
            "agent": os.environ.get("OMS_AGENT") or "hook",
            "hook": str(payload.get("hook_event_name") or payload.get("hookEventName") or "unknown"),
            "session": session_hash(payload),
            "turn_id": str(payload.get("turn_id") or payload.get("turnId") or ""),
            "cwd_hash": sha256_text(payload_cwd(payload))[:16] if payload_cwd(payload) else "",
        }
        row.update({k: v for k, v in fields.items() if v is not None})
        with (hooks_dir / "events.jsonl").open("a", encoding="utf-8") as handle:
            json.dump(row, handle, ensure_ascii=False, sort_keys=True)
            handle.write("\n")
    except Exception:
        return


def has_any(text: str, terms: tuple[str, ...]) -> bool:
    return any(term in text for term in terms)


def classify_prompt(prompt: str) -> dict[str, Any]:
    lower = prompt.lower()
    write = has_any(lower, WRITE_TERMS)
    read = has_any(lower, READ_TERMS)
    if has_any(lower, RELEASE_TERMS):
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

    high = has_any(lower, HIGH_RISK_TERMS) or workflow in {"release", "ml-experiment"}
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


def active_task_file(repo: Path) -> Path:
    return repo / ".oms" / "task" / "current.md"


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
    if os.environ.get("OMS_AUTO_TASK_OFF") == "1" or os.environ.get("OMS_AUTO_TASK") == "0":
        return False
    return not bool(CHITCHAT_RE.match(prompt.strip()))


def auto_task_record(payload: dict[str, Any], prompt: str, route: dict[str, Any]) -> None:
    if not should_auto_task(prompt):
        return
    repo = repo_root(payload_cwd(payload))
    if repo is None:
        return
    try:
        hooks_dir = ensure_oms(repo)
        state_path = task_route_state_path(hooks_dir, payload)
        prompt_hash = sha256_text(prompt)
        turn_id = str(payload.get("turn_id") or payload.get("turnId") or "")
        previous = load_state(state_path)
        if previous.get("prompt_hash") == prompt_hash and previous.get("turn_id") == turn_id:
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
        task_file = active_task_file(repo)
        existed = task_file.exists() and task_file.stat().st_size > 0
        agent = os.environ.get("OMS_AGENT") or "hook"
        status = "appended" if existed else "created"

        if not existed:
            goal = prompt_goal(prompt) or f"Respond to user request: {excerpt}"
            rc = run_agent_task(
                repo,
                ["init", "--goal", goal, "--next", "Respond to the latest user request."],
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
    repo = repo_root(payload_cwd(payload))
    if repo is None:
        return
    hooks_dir = ensure_oms(repo)
    state_path = session_state_path(hooks_dir, payload)
    previous = load_state(state_path)
    turn_id = str(payload.get("turn_id") or payload.get("turnId") or "")
    previous_turn = str(previous.get("turn_id") or "")
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
        "guard_blocks": previous.get("guard_blocks", {}) if previous_turn == turn_id else {},
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


def cmd_route(args: argparse.Namespace) -> int:
    payload, _ = load_payload()
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
        triggers = [str(t).lower() for t in skill.get("triggers", [])]
        hits = sum(1 for trigger in triggers if trigger in lower)
        if hits:
            scored.append((-hits, str(skill.get("name") or "")))
    if not scored:
        return 0
    scored.sort()
    fresh = fresh_skill_names(payload, [name for _, name in scored if name])
    repo = repo_root(payload_cwd(payload))
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
        print(
            "oh-my-setting skill hint: this request may match installed skill(s): "
            + ", ".join(fresh)
            + ". If relevant, invoke via the Skill tool before proceeding; ignore if not."
            + " (each skill hinted once per turn)"
        )
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


def cmd_guard(_: argparse.Namespace) -> int:
    if os.environ.get("OMS_TURN_GUARD_OFF") == "1":
        return 0
    payload, _ = load_payload()
    repo = repo_root(payload_cwd(payload))
    if repo is None:
        return 0
    hooks_dir = ensure_oms(repo)
    state_path = session_state_path(hooks_dir, payload)
    state = load_state(state_path)
    if not state:
        append_event(repo, payload, action="turn_guard", status="allow_no_state")
        return 0

    dirty = git_dirty(repo)
    risk = str(state.get("risk") or "low")
    workflow = str(state.get("workflow") or "unknown")
    guard = bool(state.get("guard"))
    should_guard = guard and (dirty or risk == "high" or os.environ.get("OMS_TURN_GUARD_STRICT") == "1")
    if not should_guard:
        append_event(repo, payload, action="turn_guard", status="allow", workflow=workflow, risk=risk, dirty=dirty)
        return 0

    if VERIFY_RE.search(assistant_message(payload)):
        append_event(repo, payload, action="turn_guard", status="allow_verified", workflow=workflow, risk=risk, dirty=dirty)
        return 0

    turn_id = str(payload.get("turn_id") or payload.get("turnId") or "")
    blocks = state.get("guard_blocks")
    if not isinstance(blocks, dict):
        blocks = {}
    count = int(blocks.get(turn_id, 0) or 0)
    limit = max_blocks_per_turn()
    if count >= limit:
        append_event(repo, payload, action="turn_guard", status="allow_block_limit", workflow=workflow, risk=risk, dirty=dirty)
        return 0

    blocks[turn_id] = count + 1
    state["guard_blocks"] = blocks
    state["updated_at"] = utc_now()
    write_json_atomic(state_path, state)
    append_event(repo, payload, action="turn_guard", status="block_unverified", workflow=workflow, risk=risk, dirty=dirty)
    reason = (
        "oh-my-setting turn guard: this looks like a "
        + workflow
        + " turn with "
        + risk
        + " risk, but the final answer did not state verification. Continue briefly: "
        "summarize changed files, run or mention the relevant verification if feasible, "
        "or explicitly say not verified and why."
    )
    print(json.dumps({"decision": "block", "reason": reason}, ensure_ascii=False))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="oh-my-setting hook state helper")
    sub = parser.add_subparsers(dest="cmd", required=True)
    route = sub.add_parser("route")
    route.add_argument("--manifest", required=True)
    route.set_defaults(func=cmd_route)
    guard = sub.add_parser("guard")
    guard.set_defaults(func=cmd_guard)
    args = parser.parse_args()
    return int(args.func(args))


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception:
        raise SystemExit(0)
