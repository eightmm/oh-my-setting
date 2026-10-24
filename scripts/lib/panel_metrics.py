"""Local numeric readings for an explicitly selected room main."""

import json
import math
import os
from pathlib import Path
import re
import runpy
import stat
import subprocess
import tempfile
import time
from decimal import Decimal, InvalidOperation
from datetime import datetime, timezone

from dashboard_projection import mapping, number
from call_ending import call_ending
from panel_view import TERMINAL_STATES


def percent(value):
    if number(value) is None or value > 100:
        return None
    return round(value)


def cached(consumer, now):
    if not isinstance(consumer, str) or not re.fullmatch(r"[0-9a-f]{32}", consumer):
        return {}
    folder = Path(os.environ.get("OMS_HUD_CACHE_DIR") or Path(tempfile.gettempdir()) / "oh-my-setting-hud")
    path = folder / ("ctx-" + consumer[:24] + ".json")
    try:
        if folder.is_symlink() or path.is_symlink():
            return {}
        fd = os.open(str(path), os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
        with os.fdopen(fd, "rb") as handle:
            info = os.fstat(handle.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > 8192:
                return {}
            if hasattr(os, "getuid") and info.st_uid != os.getuid():
                return {}
            raw = handle.read(8193)
        data = json.loads(raw)
        timestamp = mapping(data).get("ts")
        if number(timestamp) is None:
            return {}
        if timestamp > now:
            return {}
        if now - timestamp > 600:
            return {"source": "cache stale"}
        used = percent(data.get("used_percentage"))
        week = mapping(data.get("seven_day"))
        weekly = percent(week.get("used_percentage"))
        reset = week.get("resets_at")
        if number(reset) is not None and reset <= now:
            weekly = None
        return {"context_left": None if used is None else 100 - used,
                "weekly_used": weekly, "source": "HUD"}
    except (OSError, ValueError, RecursionError):
        return {}


def pane_reading(target, provider):
    """Mirror only numeric fields in the proven pane's visible native footer."""
    pane = mapping(target).get("pane")
    if mapping(target).get("host") == "herdr":
        try:
            import panel_host
            return numeric_reading(panel_host.read(target), provider)
        except (OSError, ValueError, KeyError, TypeError):
            return {}
    if not isinstance(pane, str) or not re.fullmatch(r"%[0-9]+", pane):
        return {}
    try:
        result = subprocess.run(["tmux", "display-message", "-p", "-t", pane,
                                 "#{pane_height}\t#{pane_dead}"], capture_output=True, text=True,
                                check=False, timeout=2, stdin=subprocess.DEVNULL)
        fields = result.stdout.replace("\r", "").strip().split("\t")
        if result.returncode or len(fields) != 2 or not fields[0].isdigit() or fields[1] != "0":
            return {}
        height = int(fields[0])
        if not 1 <= height <= 1000:
            return {}
        result = subprocess.run(["tmux", "capture-pane", "-p", "-t", pane,
                                 "-S", str(max(0, height - 8)), "-E", str(height - 1)],
                                capture_output=True, text=True, check=False, timeout=2,
                                stdin=subprocess.DEVNULL)
        if result.returncode or len(result.stdout) > 16384:
            return {}
        text = result.stdout
        return numeric_reading(text, provider)
    except (OSError, ValueError, subprocess.SubprocessError):
        return {}


def numeric_reading(text, provider):
    # Only whole fields of the pane's last status lines count; chat text quoting "Context 5% left" does not.
    lines = [line for line in text.splitlines() if line.strip()][-2:]
    fields = [f.strip() for line in lines for f in re.split(r"\s[·|/]\s", line)]

    def found(pattern):
        hits = [re.fullmatch(pattern, f, re.I if provider == "codex" else 0) for f in fields]
        return [int(m.group(1)) for m in hits if m]

    if provider == "codex":
        context, weekly = found(r"Context\s+(\d{1,3})%\s+left"), found(r"weekly\s+(\d{1,3})%\s+left")
        reading = {"context_left": percent(context[-1]) if context else None,
                   "weekly_used": 100 - weekly[-1] if weekly and weekly[-1] <= 100 else None}
    else:
        context, weekly = found(r"ctx\s+\[[#-]+\]\s+(\d{1,3})%(?:\s.*)?"), found(r"7d\s+(\d{1,3})%(?:\s.*)?")
        used = percent(context[-1]) if context else None
        reading = {"context_left": None if used is None else 100 - used,
                   "weekly_used": percent(weekly[-1]) if weekly else None}
    return dict(reading, source="TUI") if any(v is not None for v in reading.values()) else {}


CODEX_WEEK_SECONDS = 10080 * 60
CODEX_TAIL_BYTES = 256 * 1024
_CODEX_QUOTA_CACHE = (None, {})


def _codex_session_file(home):
    remaining = 4096

    def entries(folder, pattern, directory):
        nonlocal remaining
        found = []
        with os.scandir(folder) as scan:
            for entry in scan:
                remaining -= 1
                if remaining < 0:
                    raise ValueError("Codex session search limit reached")
                if (re.fullmatch(pattern, entry.name) and
                        (entry.is_dir(follow_symlinks=False) if directory
                         else entry.is_file(follow_symlinks=False))):
                    found.append(Path(entry.path))
        return sorted(found, reverse=True)

    days = []
    for year in entries(home / "sessions", r"[0-9]{4}", True):
        for month in entries(year, r"(?:0[1-9]|1[0-2])", True):
            for day in entries(month, r"(?:0[1-9]|[12][0-9]|3[01])", True):
                days.append(day)
                if len(days) == 3:
                    break
            if len(days) == 3:
                break
        if len(days) == 3:
            break
    files = [path for day in days for path in entries(day, r"rollout-.*\.jsonl", False)]
    return max(files, key=lambda path: path.stat().st_mtime_ns) if files else None


def codex_weekly_reading(now):
    """Use only the newest log's bounded quota tail, rechecking expiry on cache hits."""
    global _CODEX_QUOTA_CACHE
    try:
        home = Path(os.environ.get("CODEX_HOME") or Path.home() / ".codex").expanduser()
        path = _codex_session_file(home)
        if path is None:
            return {}
        info = path.stat()
        if not stat.S_ISREG(info.st_mode) or now - info.st_mtime > CODEX_WEEK_SECONDS:
            return {}
        key = (str(path), info.st_ino, info.st_mtime_ns, info.st_size)
        cached_key, limits = _CODEX_QUOTA_CACHE
        if key != cached_key:
            limits = {}
            fd = os.open(str(path), os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
                         | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_BINARY", 0))
            with os.fdopen(fd, "rb") as handle:
                opened = os.fstat(handle.fileno())
                if not stat.S_ISREG(opened.st_mode):
                    return {}
                start = max(0, opened.st_size - CODEX_TAIL_BYTES)
                handle.seek(start)
                body = handle.read(CODEX_TAIL_BYTES)
            if start:
                body = body.partition(b"\n")[2]
            for line in reversed(body.splitlines()):
                if b'"rate_limits"' not in line:
                    continue
                try:
                    row = mapping(json.loads(line))
                except (ValueError, UnicodeError, RecursionError):
                    continue
                payload = mapping(row.get("payload"))
                if "rate_limits" in row or "rate_limits" in payload:
                    limits = mapping(row.get("rate_limits") if "rate_limits" in row
                                     else payload.get("rate_limits"))
                    break
            _CODEX_QUOTA_CACHE = (key, limits)
        for name in ("primary", "secondary"):
            window = mapping(limits.get(name))
            if window.get("window_minutes") != 10080:
                continue
            used, reset = percent(window.get("used_percent")), number(window.get("resets_at"))
            if used is not None and reset is not None and reset > now:
                return {"weekly_used": used, "source": "session log"}
        return {}
    except (OSError, ValueError, TypeError, OverflowError, RuntimeError):
        return {}


def collect(report, main_attempt=None, now=None, repo=None, _tick=None):
    from panel_chats import current_models, windows
    from room_view import nodes
    room = mapping(report.get("room"))
    mains = [m for m in nodes(report) if m.get("role") == "main" and m.get("harness") != "pi"]
    panes = windows(repo, room["id"], room, _tick=_tick) if repo is not None and room.get("id") else {}
    now = time.time() if now is None else now
    current = current_models(repo, mains)
    output = {}
    for provider in ("claude", "codex"):
        candidates = [m for m in mains if m.get("provider") == provider]
        selected = [m for m in candidates if main_attempt and
                    (m["participant"] == main_attempt or m.get("attempt") == main_attempt)]
        # Earlier mains of the room stay in its record; nodes() orders the live, then the newest, first.
        selected = selected or candidates[:1]
        row = {"context_left": None, "weekly_used": None, "source": "unavailable"}
        if provider == "codex":
            row.update(codex_weekly_reading(now))
            row.update(native_models=current, native_model_checked=repo is not None)
        if selected:
            main = selected[0]
            native = current.get(main["participant"], {})
            bound_live = (provider == "codex" and repo is not None and main.get("consumer")
                          and main["state"] not in TERMINAL_STATES)
            row.update(participant=main["participant"],
                       model=native.get("model") if bound_live else main.get("model"))
            reading = cached(main.get("consumer"), now) if provider == "claude" else {}
            targets = panes.get(main["participant"], [])
            if len(targets) == 1:
                visible = pane_reading(targets[0], provider)
                if not reading or reading.get("source") == "cache stale":
                    reading = visible or reading
                else:
                    for key in ("weekly_used", "context_left"):
                        if reading.get(key) is None and visible.get(key) is not None:
                            reading[key] = visible[key]
                            reading["source"] = "HUD/TUI"
            row.update({key: value for key, value in reading.items() if value is not None})
            if main["state"] in {"done", "failed", "cancelled", "timed_out", "abandoned"}:
                row["context_left"] = None
        output[provider] = row
    return output


def _stamp(value):
    if not isinstance(value, str) or not value:
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=timezone.utc)
        return parsed.timestamp()
    except (ValueError, OverflowError):
        return None


def _duration(value):
    match = re.fullmatch(r"([0-9]+(?:\.[0-9]+)?)(s|m|h|d|w)", value or "")
    if not match:
        raise ValueError("--since must be a positive finite duration such as 30m, 2h or 7d")
    try:
        seconds = float(Decimal(match.group(1)) * Decimal(
            {"s": 1, "m": 60, "h": 3600, "d": 86400, "w": 604800}[match.group(2)]))
    except (InvalidOperation, OverflowError, ValueError):
        seconds = float("inf")
    if not math.isfinite(seconds) or seconds <= 0:
        raise ValueError("--since must be a positive finite duration such as 30m, 2h or 7d")
    return seconds


METRIC_TAIL_BYTES = 16 * 1024 * 1024
METRIC_TAIL_ROWS = 20000
METRIC_ATTEMPTS = 1000
PLAN_TASK_STATES = {"ready", "claimed", "running", "review", "landing", "blocked", "done", "cancelled"}


def _attempt_history(repo):
    """Project at most the newest 1000 created attempts from a bounded ledger tail."""
    from panel_results import DURABLE

    relative = ".oms/lifecycle/events.jsonl"
    target = DURABLE["canonical_repo_path"](str(repo), relative, "panel metrics lifecycle", False)
    parent = os.path.dirname(target)
    try:
        parent_info = os.lstat(parent)
    except FileNotFoundError:
        return [], False
    if not stat.S_ISDIR(parent_info.st_mode) or stat.S_ISLNK(parent_info.st_mode):
        raise ValueError("panel metrics lifecycle parent is unsafe")
    ancestry = DURABLE["component_snapshot"](str(repo), target, "panel metrics lifecycle")
    directory = None
    descriptor = None
    try:
        if os.name != "nt":
            directory = os.open(parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
                                | getattr(os, "O_NOFOLLOW", 0))
            if not DURABLE["same_file"](parent_info, os.fstat(directory)):
                raise ValueError("panel metrics lifecycle parent changed")
        flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(os.path.basename(target) if directory is not None else target,
                             flags, **({} if directory is None else {"dir_fd": directory}))
        opened = os.fstat(descriptor)
        if not DURABLE["safe_regular"](opened):
            raise ValueError("panel metrics lifecycle must be a regular single-link file")
        named = (os.stat(os.path.basename(target), dir_fd=directory, follow_symlinks=False)
                 if directory is not None else os.lstat(target))
        if not DURABLE["safe_regular"](named) or not DURABLE["same_file"](opened, named):
            raise ValueError("panel metrics lifecycle changed before read")
        start = max(0, opened.st_size - METRIC_TAIL_BYTES)
        os.lseek(descriptor, start, os.SEEK_SET)
        chunks, remaining = [], min(METRIC_TAIL_BYTES, opened.st_size - start)
        while remaining:
            chunk = os.read(descriptor, min(1024 * 1024, remaining))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        body = b"".join(chunks)
        after = os.fstat(descriptor)
        named_after = (os.stat(os.path.basename(target), dir_fd=directory, follow_symlinks=False)
                       if directory is not None else os.lstat(target))
        if (not DURABLE["safe_regular"](named_after) or not DURABLE["same_file"](after, named_after)
                or not DURABLE["_snapshot_matches"](opened, after)):
            raise ValueError("panel metrics lifecycle changed during read")
        DURABLE["components_unchanged"](ancestry, "panel metrics lifecycle")
    except FileNotFoundError:
        return [], False
    finally:
        if descriptor is not None:
            os.close(descriptor)
        if directory is not None:
            os.close(directory)

    truncated = start > 0 or len(body) < max(0, opened.st_size - start)
    if start and b"\n" in body:
        body = body.split(b"\n", 1)[1]
    elif start:
        body = b""
    lines = body.splitlines()
    if len(lines) > METRIC_TAIL_ROWS:
        lines = lines[-METRIC_TAIL_ROWS:]
        truncated = True
    if body and not body.endswith(b"\n"):
        lines = lines[:-1]
        truncated = True
    rows, malformed = [], False
    for line in lines:
        if not line:
            continue
        try:
            row = json.loads(line)
        except (ValueError, UnicodeError, RecursionError):
            malformed = True
            continue
        if not isinstance(row, dict):
            malformed = True
            continue
        rows.append(row)
    created_ids = []
    for row in reversed(rows):
        if row.get("event_type") == "attempt.created" and row.get("attempt_id") not in created_ids:
            created_ids.append(row.get("attempt_id"))
            if len(created_ids) >= METRIC_ATTEMPTS:
                break
    selected = set(created_ids)
    attempts, incomplete = {}, truncated or malformed
    terminal = {"done", "failed", "cancelled", "timed_out", "abandoned"}
    for event_order, row in enumerate(rows):
        attempt_id = row.get("attempt_id")
        if attempt_id not in selected:
            continue
        kind, stamp = row.get("event_type"), row.get("ts")
        if kind == "attempt.created":
            attempts[attempt_id] = {"attempt_id": attempt_id, "task_id": row.get("task_id", ""),
                "parent_attempt_id": row.get("parent_attempt_id", ""), "provider": row.get("provider", ""),
                "tool": row.get("tool", ""), "refs": row.get("refs", {}), "state": row.get("to_state"),
                "created_at": stamp, "created_order": event_order,
                "updated_at": stamp, "terminal": False, "transitions": [],
                "usage_tokens": 0, "usage_reports": 0}
        elif attempt_id in attempts:
            current = attempts[attempt_id]
            if kind == "attempt.state_changed":
                current["state"] = row.get("to_state")
                current["terminal"] = row.get("to_state") in terminal
                current["reason_code"] = row.get("reason_code", "")
                current["transitions"].append({"state": row.get("to_state"), "reason_code": row.get("reason_code", ""),
                                               "ts": stamp, "order": event_order})
            elif kind == "attempt.usage":
                usage = row.get("usage")
                if isinstance(usage, dict) and isinstance(usage.get("tokens"), int) and not isinstance(usage.get("tokens"), bool):
                    current["usage_tokens"] += usage["tokens"]
                    current["usage_reports"] += 1
                if row.get("refs"):
                    current["refs"].update(row["refs"])
            elif row.get("refs"):
                current["refs"].update(row["refs"])
            current["updated_at"] = stamp
    if truncated and len(created_ids) < METRIC_ATTEMPTS:
        incomplete = True
    return list(attempts.values()), incomplete


def aggregate(repo, since=None, now=None):
    """Aggregate bounded recorded evidence read only.

    Follow-up task IDs are not linked by any producer record; rework therefore
    counts only additional write rounds for the same task ID.
    """
    from panel_results import _query, _read, _indexed, outcomes, settled_pending_admissions

    repo = Path(repo)
    now = time.time() if now is None else now
    seconds = _duration(since) if since is not None else None
    cutoff = now - seconds if seconds is not None else None
    if cutoff is not None and not math.isfinite(cutoff):
        raise ValueError("--since duration is outside the supported time range")
    plan_path = repo / ".oms/plan/tasks.json"
    incomplete_reasons = []
    try:
        plan_bytes = _read(repo, ".oms/plan/tasks.json", 4 * 1024 * 1024, missing=True)
    except (OSError, ValueError, UnicodeError):
        plan_bytes = None
        incomplete_reasons.append("plan evidence unavailable or unsafe")
    if plan_bytes is None:
        plan = {}
    elif not plan_bytes and not os.path.lexists(str(plan_path)):
        plan = {}
    else:
        try:
            plan = json.loads(plan_bytes.decode("utf-8"))
            if (not isinstance(plan, dict) or "tasks" not in plan
                    or not isinstance(plan.get("tasks"), (dict, list))):
                raise ValueError("invalid plan shape")
            plan_rows = list(plan["tasks"].values()) if isinstance(plan["tasks"], dict) else plan["tasks"]
            if any(not isinstance(task, dict) or not isinstance(task.get("id"), str) for task in plan_rows):
                raise ValueError("invalid plan task row")
        except (ValueError, UnicodeError):
            plan = {}
            incomplete_reasons.append("plan evidence malformed")
    tasks = plan.get("tasks", {}) if isinstance(plan, dict) else {}
    tasks = list(tasks.values()) if isinstance(tasks, dict) else tasks if isinstance(tasks, list) else []
    valid_tasks = []
    for task in tasks:
        state = task.get("state", "ready")
        if type(state) is not str or state not in PLAN_TASK_STATES:
            incomplete_reasons.append("plan task state malformed")
            continue
        valid_tasks.append(task)
    tasks = valid_tasks
    attempts, incomplete_attempts = _attempt_history(repo)
    selected_attempt_ids = {row.get("attempt_id") for row in attempts
                            if isinstance(row.get("attempt_id"), str)}
    artifact_index_read = False
    try:
        index_store = runpy.run_path(str(Path(__file__).with_name("artifact-index-store.py")))
        artifact_body = index_store["read_index"](str(repo), ".oms/artifacts/index.jsonl", missing_ok=True)
        artifact_index_read = True
        retained_artifacts = [json.loads(line) for line in artifact_body.splitlines()]
        artifacts = [row for row in retained_artifacts
                     if isinstance(row.get("attempt_id"), str)
                     and row.get("attempt_id") in selected_attempt_ids]
    except (OSError, ValueError, SystemExit, UnicodeError):
        retained_artifacts = []
        artifacts = []
        incomplete_reasons.append("artifact evidence unavailable or unsafe")
    incomplete = incomplete_attempts or bool(incomplete_reasons)
    if incomplete_attempts:
        incomplete_reasons.append("lifecycle evidence is partial")

    task_ids = {row.get("task_id") for row in attempts
                if isinstance(row.get("task_id"), str) and row.get("task_id")
                and not row["task_id"].startswith("preflight-")}

    class RetainedResultRecords:
        def records(self):
            return attempts, retained_artifacts

    decisions = outcomes(repo, task_ids, _tick=RetainedResultRecords())

    def in_window(stamp):
        timestamp = stamp if isinstance(stamp, (int, float)) and math.isfinite(stamp) else _stamp(stamp)
        return cutoff is None or (timestamp is not None and timestamp >= cutoff)

    started_states = {"claimed", "running", "review", "landing", "done"}
    implementation_calls = [row for row in attempts if row.get("refs", {}).get("panel_role") == "worker"
                            and row.get("refs", {}).get("panel_access") == "write"]
    attempts_by_task = {}
    for attempt in implementation_calls:
        ident = attempt.get("task_id")
        if ident:
            attempts_by_task.setdefault(ident, []).append(attempt)
    preflight_ids = {row.get("task_id") for row in attempts
                     if isinstance(row.get("task_id"), str) and row["task_id"].startswith("preflight-")}
    preflight_ids.update(task.get("id") for task in tasks
                         if isinstance(task.get("id"), str) and task["id"].startswith("preflight-"))
    rate_tasks = [task for task in tasks if not str(task.get("id", "")).startswith("preflight-")]
    started = []
    first_start = {}
    for task in rate_tasks:
        history = task.get("history") if isinstance(task.get("history"), list) else []
        claim_times = [_stamp(item.get("ts")) for item in history if isinstance(item, dict)
                       and item.get("kind") in {"claim", "start", "started"}]
        call_times = [_stamp(row.get("created_at")) for row in attempts_by_task.get(task.get("id"), [])]
        first = min((stamp for stamp in claim_times + call_times if stamp is not None), default=None)
        cancellation = task.get("cancellation") if isinstance(task.get("cancellation"), dict) else {}
        lease_epoch = task.get("lease_epoch", 0)
        retained_claim = (type(lease_epoch) is int and lease_epoch > 0) or bool(
            task.get("lease_id") or task.get("claimed_by_participant"))
        cancelled_after_start = task.get("state") == "cancelled" and cancellation.get("state") not in {"ready", "blocked"}
        ever_started = (task.get("state") in started_states or cancelled_after_start
                        or retained_claim
                        or bool(attempts_by_task.get(task.get("id")))
                        or any(isinstance(item, dict) and item.get("kind") in {"claim", "start", "started"}
                               for item in history))
        if ever_started and in_window(first):
            started.append(task)
            first_start[task.get("id")] = first
        elif ever_started and cutoff is not None and first is None:
            incomplete = True
    finished = sum(task.get("state") in {"done", "landed"} or bool(task.get("landing")) for task in started)
    calls = [row for row in attempts if not str(row.get("task_id", "")).startswith("preflight-")]
    ending_counts = {}
    for call in calls:
        if not in_window(call.get("updated_at")):
            continue
        evidence = dict(call.get("refs", {}))
        evidence.update(state=call.get("state"), reason_code=call.get("reason_code"))
        # Lifecycle records retain the terminal state and reason. Preserve any
        # richer typed result evidence carried in refs for the shared classifier.
        ending = call_ending(evidence)
        if ending is not None:
            ending_counts[ending] = ending_counts.get(ending, 0) + 1
    # Reconcile only explicitly recorded peer-delegate write attempts. The
    # lifecycle state is the source of the state tally; artifact call rows are
    # used only for stage attribution and typed error metadata.
    delegate_writes = [row for row in calls if row.get("tool") == "peer-delegate"
                       and row.get("refs", {}).get("panel_role") == "worker"
                       and row.get("refs", {}).get("panel_access") == "write"
                       and in_window(row.get("updated_at"))]
    delegate_states = {}
    for attempt in delegate_writes:
        state = attempt.get("state")
        label = state if isinstance(state, str) and state else "unknown"
        delegate_states[label] = delegate_states.get(label, 0) + 1
    state_totals = {"done": 0, "failed": 0, "cancelled": 0, "review": 0, "other_by_state": {}}
    for state, count in delegate_states.items():
        if state in state_totals and state != "other_by_state":
            state_totals[state] += count
        else:
            state_totals["other_by_state"][state] = count
    calls_by_attempt = {}
    for row in artifacts:
        calls_by_attempt.setdefault(row["attempt_id"], []).append(row)

    def first_failing_stage(attempt):
        indexed = calls_by_attempt.get(attempt.get("attempt_id"), [])
        stages = {"implement": [], "verify": [], "review": []}
        if attempt.get("reason_code") in {"provider_failed", "provider_timeout"}:
            return "implement"
        if attempt.get("reason_code") == "verification_failed" or attempt.get("verify_status") == "failed":
            return "verify"
        for row in indexed:
            kind = row.get("kind")
            verify_exit = row.get("verify_exit")
            verify_not_run = type(verify_exit) is int and verify_exit == 125
            # Verify may still run after a provider failure; the provider failed first.
            provider_failed = row.get("provider_error_exit") is not None or row.get("state") == "provider_error"
            verify_failed = (not verify_not_run and not provider_failed and
                             (type(verify_exit) is int and verify_exit != 0
                              or row.get("reason_code") == "verification_failed"
                              or row.get("verify_status") == "failed"))
            stage = "review" if kind == "review" else "implement" if kind in (
                "delegate", "call", "ask", "consult") else None
            if stage and not verify_failed:
                stages[stage].append(row)
            if verify_failed:
                stages["verify"].append(row)
        for stage in ("implement", "verify", "review"):
            for row in stages[stage]:
                if stage == "verify":
                    code = row.get("verify_exit")
                    if (type(code) is int and code not in (0, 125)
                            or row.get("reason_code") == "verification_failed"
                            or row.get("verify_status") == "failed"
                            or row.get("state") in {"failed", "provider_error", "timed_out"}):
                        return stage
                    continue
                code = row.get("exit")
                if type(code) is int and code != 0 or row.get("state") in {"failed", "provider_error", "timed_out"}:
                    return stage
        return "unknown"

    failure_reason_counts, failure_stage_counts = {}, {}
    # The categories peer-common's provider classifier records; anything else is unknown.
    provider_categories = {name: 0 for name in ("capacity", "model_unavailable", "provider_down", "unknown")}
    undecided_ages = {"under_1d": 0, "1d_to_7d": 0, "7d_or_more": 0, "unknown": 0}
    for attempt in delegate_writes:
        state = attempt.get("state")
        if state not in {"failed", "provider_error", "review"}:
            continue
        reason = attempt.get("reason_code")
        stage = first_failing_stage(attempt)
        if state == "review" and not (isinstance(reason, str) and reason):
            reason = "unknown" if stage != "unknown" else "awaiting_review"
        reason = reason if isinstance(reason, str) and reason else "unknown"
        failure_reason_counts[reason] = failure_reason_counts.get(reason, 0) + 1
        if state == "review" and reason == "awaiting_review":
            stage = "none"
        failure_stage_counts[stage] = failure_stage_counts.get(stage, 0) + 1
        if state == "review":
            stamp = _stamp(attempt.get("updated_at"))
            if stamp is None:
                undecided_ages["unknown"] += 1
            else:
                age = max(0, now - stamp)
                bucket = "under_1d" if age < 86400 else "1d_to_7d" if age < 604800 else "7d_or_more"
                undecided_ages[bucket] += 1
        attempt_calls = calls_by_attempt.get(attempt.get("attempt_id"), [])
        refs = attempt.get("refs", {})
        has_provider_error = (state == "provider_error" or attempt.get("reason_code") == "provider_failed"
                              or refs.get("provider_error_exit") is not None
                              or any(row.get("provider_error_exit") is not None for row in attempt_calls))
        if has_provider_error:
            category = refs.get("provider_error_category") or refs.get("provider_error")
            if category is None:
                category = next((row.get("provider_error_category") for row in attempt_calls
                                 if row.get("provider_error_category") is not None), None)
            if category not in provider_categories or category == "unknown":
                category = "unknown"
            provider_categories[category] += 1
    # peer-delegate indexes its context_mode on the delegate artifact row.
    context_modes = {}
    for attempt in delegate_writes:
        delegate_rows = [row for row in calls_by_attempt.get(attempt.get("attempt_id"), [])
                         if row.get("kind") == "delegate"]
        mode = next((row.get("context_mode") for row in delegate_rows
                     if isinstance(row.get("context_mode"), str)),
                    "unrecorded" if delegate_rows else "no artifact row")
        entry = context_modes.setdefault(mode, {"attempts": 0, "done": 0})
        entry["attempts"] += 1
        entry["done"] += attempt.get("state") == "done"
    write_calls = [row for row in calls if row.get("refs", {}).get("panel_role") == "worker"
                   and row.get("refs", {}).get("panel_access") == "write"
                   and not any(item.get("reason_code") == "manual_clear" for item in row.get("transitions", []))]
    write_groups = {}
    for call in write_calls:
        if call.get("task_id"):
            write_groups.setdefault(call.get("task_id"), []).append(call)
    # Same cohort as completion: tasks first started in the window.
    reworked = {task_id for task_id in first_start if len(write_groups.get(task_id, [])) > 1}

    def has_write_successor(call, transition):
        if call.get("refs", {}).get("panel_role") != "worker" or call.get("refs", {}).get("panel_access") != "write":
            return False
        settled_at = _stamp(transition.get("ts"))
        settled_order = transition.get("order", -1)
        for candidate in write_groups.get(call.get("task_id"), []):
            created = _stamp(candidate.get("created_at"))
            created_order = candidate.get("created_order", -1)
            if (candidate.get("attempt_id") != call.get("attempt_id") and created is not None
                    and (created, created_order) > (settled_at, settled_order)):
                return True
        return False

    abandoned = 0
    clears, cancels = 0, 0
    for call in calls:
        for transition in call.get("transitions", []):
            stamp = _stamp(transition.get("ts"))
            if stamp is None or not in_window(transition.get("ts")):
                continue
            state, reason = transition.get("state"), transition.get("reason_code")
            if reason == "manual_clear":
                clears += 1
            elif state == "cancelled":
                cancels += 1
            if (state in {"abandoned", "timed_out", "guard-stopped"} or
                    state in {"failed", "blocked"} and str(reason).startswith("guard")):
                if not has_write_successor(call, transition):
                    abandoned += 1
    plan_cancels = [task.get("cancellation", {}).get("ts") for task in tasks
                    if isinstance(task.get("cancellation"), dict)]
    cancels += sum(in_window(stamp) for stamp in plan_cancels)
    owner_failures = 0
    for row in retained_artifacts:
        if row.get("kind") != "panel-result" or not in_window(row.get("ts")):
            continue
        try:
            result = json.loads(_indexed(repo, row, 1024 * 1024))
            owner_failures += int(isinstance(result, dict) and result.get("kind") == "oms-panel-result"
                                  and result.get("outcome") == "failed")
        except (OSError, ValueError, TypeError):
            continue

    by_id = {attempt.get("attempt_id"): attempt for attempt in calls}
    verdicts = [row for row in artifacts if row.get("kind") == "review-outcome"
                and row.get("attempt_id") in by_id]
    waits = []
    for verdict in verdicts:
        reviewer = by_id[verdict["attempt_id"]]
        reviewer_start = _stamp(reviewer.get("created_at"))
        decided = _stamp(verdict.get("ts"))
        if reviewer.get("task_id") != verdict.get("task_id") or reviewer_start is None or decided is None:
            continue
        candidates = []
        for call in write_calls:
            if call.get("task_id") != verdict.get("task_id"):
                continue
            settled = max((_stamp(item.get("ts")) for item in call.get("transitions", [])
                           if item.get("state") in {"done", "failed", "cancelled", "timed_out", "abandoned"}
                           and _stamp(item.get("ts")) is not None and _stamp(item.get("ts")) <= reviewer_start),
                          default=None)
            if settled is not None:
                candidates.append((settled, call))
        if candidates:
            settled, _call = max(candidates, key=lambda item: item[0])
            if decided >= reviewer_start and in_window(settled):
                waits.append(decided - settled)
    waits.sort()

    def percentile(values, fraction):
        if not values:
            return None
        position = (len(values) - 1) * fraction
        lower = int(position)
        upper = min(lower + 1, len(values) - 1)
        return round(values[lower] + (values[upper] - values[lower]) * (position - lower), 2)

    def age_stats(stamps):
        ages = sorted(max(0.0, now - stamp) for stamp in stamps if stamp is not None)
        return {"count": len(stamps), "age_coverage": len(ages),
                "oldest_seconds": round(max(ages), 2) if ages else None,
                "median_seconds": percentile(ages, .5)}

    review_tasks = [task for task in tasks if task.get("state") == "review"]
    review_ages = [_stamp(task.get("updated_at") or task.get("claimed_at")) for task in review_tasks]
    try:
        closed_states = {"done", "blocked", "cancelled"}
        admission_ids = settled_pending_admissions(
            repo, [task.get("id") for task in tasks
                   if task.get("id") and task.get("state") not in closed_states])
    except (OSError, ValueError, KeyError, TypeError):
        admission_ids = set()
        incomplete = True
        incomplete_reasons.append("admission evidence unavailable")
    admission_tasks = [task for task in tasks if task.get("id") in admission_ids]
    admission_ages = [_stamp(task.get("updated_at") or task.get("claimed_at")) for task in admission_tasks]
    until = datetime.fromtimestamp(now, timezone.utc).isoformat().replace("+00:00", "Z")
    token_decisions = {name: {"tokens": 0, "tasks": 0, "attempts": 0}
                       for name in ("accepted", "completed", "rejected", "failed", "undecided", "untasked")}
    task_sets = {name: set() for name in token_decisions}
    unreported_tokens = 0
    for attempt in attempts:
        refs = attempt.get("refs", {})
        if refs.get("panel_role") != "worker" or refs.get("panel_access") not in {"read", "write"}:
            continue
        if not in_window(attempt.get("updated_at")):
            continue
        task_id = attempt.get("task_id")
        if not isinstance(task_id, str) or not task_id:
            bucket = "untasked"
            task_key = None
        else:
            task_key = task_id
            outcome = decisions.get(task_id)
            bucket = outcome if outcome in {"accepted", "completed", "rejected", "failed"} else "undecided"
        entry = token_decisions[bucket]
        entry["attempts"] += 1
        if task_key is not None:
            task_sets[bucket].add(task_key)
        if attempt.get("usage_reports", 0):
            entry["tokens"] += attempt.get("usage_tokens", 0)
        else:
            unreported_tokens += 1
    for bucket, task_set in task_sets.items():
        token_decisions[bucket]["tasks"] = len(task_set)
    token_decisions["unreported_attempts"] = unreported_tokens
    def has_claim_evidence(task):
        history = task.get("history") if isinstance(task.get("history"), list) else []
        has_history = any(isinstance(item, dict) and item.get("kind") in {"claim", "start", "started"}
                          for item in history)
        lease_epoch = task.get("lease_epoch", 0)
        has_lease = ((type(lease_epoch) is int and lease_epoch > 0)
                     or bool(task.get("lease_id") or task.get("claimed_by_participant")))
        cancellation = task.get("cancellation") if isinstance(task.get("cancellation"), dict) else {}
        cancelled_after_start = (task.get("state") == "cancelled"
                                 and cancellation.get("state") not in {"ready", "blocked"})
        return has_history or has_lease or task.get("state") in started_states or cancelled_after_start

    coverage = {"attempt_limit": METRIC_ATTEMPTS, "artifact_limit": None if artifact_index_read else 0,
                "attempts_observed": len(attempts), "artifact_events_observed": len(artifacts),
                "lifecycle_missing_task_ids": sorted({task.get("id") for task in rate_tasks
                    if task.get("id") and task.get("id") not in attempts_by_task and has_claim_evidence(task)}),
                "incomplete": bool(incomplete)}
    metric_scope = {"since": since, "until": until, "coverage": coverage}
    return {"schema": 1, "kind": "oms-panel-metrics", "since": since, "until": until,
            "window": {"since": since, "until": until}, "coverage": coverage,
            "preflight": {"tasks": len(preflight_ids), "attempts": sum(
                str(row.get("task_id", "")).startswith("preflight-") for row in attempts)},
            "pending": {"scope": "current snapshot of shared-plan tasks", "awaiting_review": age_stats(review_ages),
                        "awaiting_admission": age_stats(admission_ages)},
            "metric_scope": {"completion": metric_scope, "review_wait_seconds": metric_scope,
                             "rework": metric_scope, "abandoned_calls": metric_scope,
                             "interventions": metric_scope},
            "completion": {"finished": finished, "started": len(started),
                           "rate": round(finished / len(started), 4) if started else None},
            "review_wait_seconds": {"count": len(waits), "median": percentile(waits, .5),
                                    "p90": percentile(waits, .9)},
            "rework": {"tasks": len(reworked), "started": len(started),
                       "rate": round(len(reworked) / len(started), 4) if started else None},
            "abandoned_calls": abandoned,
            "endings": ending_counts,
            "tokens_by_decision": token_decisions,
            "write_attempts": {"total": len(delegate_writes), "states": state_totals,
                               "state_counts": delegate_states,
                               "reconciles": (state_totals["done"] + state_totals["failed"]
                                              + state_totals["cancelled"] + state_totals["review"]
                                              + sum(state_totals["other_by_state"].values())
                                              == len(delegate_writes)),
                               "failed_and_undecided": {"reason_code": failure_reason_counts,
                                                        "first_failing_stage": failure_stage_counts,
                                                        "undecided_age_buckets": undecided_ages},
                               "provider_failed_categories": provider_categories,
                               "provider_error_category_field": "provider_error_category or refs.provider_error",
                               "telemetry": {"context_modes": dict(sorted(context_modes.items())),
                                             "claude_fork_subagents": "not_recorded",
                                             "claude_fork_subagents_missing_field": "refs.claude_fork_subagents"}},
            "interventions": {"manual_clears": clears, "cancels": cancels,
                              "owner_finalized_failures": owner_failures,
                              "total": clears + cancels + owner_failures},
            "incomplete": bool(incomplete),
            "incomplete_reason": "; ".join(incomplete_reasons) if incomplete_reasons else None}


def token_amount(value):
    value = int(value)
    if value >= 1000000:
        return "%gM" % (value / 1000000.0)
    if value >= 1000:
        return "%gk" % (value / 1000.0)
    return str(value)


def metrics_text(report):
    completion, waits = report["completion"], report["review_wait_seconds"]
    rework = report["rework"]
    def rate(value):
        return "none" if value is None else "%.1f%%" % (value * 100)
    def seconds(value):
        return "none" if value is None else "%ss" % value
    return "\n".join((
        "Panel metrics (since %s; until %s)" % (report.get("since") or "all retained", report.get("until", "unknown")),
        "coverage: %s attempts, %s artifact events (limits %s/%s); plan tasks without an observed write attempt=%d%s" % (
            report.get("coverage", {}).get("attempts_observed", "?"),
            report.get("coverage", {}).get("artifact_events_observed", "?"),
            report.get("coverage", {}).get("attempt_limit", "?"),
            ("all retained" if report.get("coverage", {}).get("artifact_limit") is None
             else "unavailable" if report.get("coverage", {}).get("artifact_limit") == 0
             else report.get("coverage", {}).get("artifact_limit")),
            len(report.get("coverage", {}).get("lifecycle_missing_task_ids", [])),
            "; partial evidence" if report.get("incomplete") else ""),
        "preflight excluded from rates: %d tasks, %d attempts" % (
            report.get("preflight", {}).get("tasks", 0), report.get("preflight", {}).get("attempts", 0)),
        "plan tasks awaiting review: n=%d oldest=%s median=%s" % (
            report.get("pending", {}).get("awaiting_review", {}).get("count", 0),
            seconds(report.get("pending", {}).get("awaiting_review", {}).get("oldest_seconds")),
            seconds(report.get("pending", {}).get("awaiting_review", {}).get("median_seconds"))),
        "plan tasks awaiting admission: n=%d oldest=%s median=%s" % (
            report.get("pending", {}).get("awaiting_admission", {}).get("count", 0),
            seconds(report.get("pending", {}).get("awaiting_admission", {}).get("oldest_seconds")),
            seconds(report.get("pending", {}).get("awaiting_admission", {}).get("median_seconds"))),
        "pending counts use the current snapshot; rates and durations use the stated window",
        "completion: %d/%d (%s)" % (completion["finished"], completion["started"], rate(completion["rate"])),
        "review wait: n=%d median=%s p90=%s" % (waits["count"], seconds(waits["median"]), seconds(waits["p90"])),
        "rework: %d/%d (%s)" % (rework["tasks"], rework["started"], rate(rework["rate"])),
        "abandoned calls: %d" % report["abandoned_calls"],
        "peer-delegate write attempts: %d; states=%s (reconciles=%s)" % (
            report.get("write_attempts", {}).get("total", 0),
            json.dumps(report.get("write_attempts", {}).get("state_counts", {}), sort_keys=True),
            report.get("write_attempts", {}).get("reconciles", False)),
        "tokens by decision: %s; unreported attempts=%d" % (
            "; ".join("%s %s/%d tasks (%s per task)" % (
                name, token_amount(entry["tokens"]), entry["tasks"],
                token_amount(entry["tokens"] / entry["tasks"]) if entry["tasks"] else "--")
                for name, entry in report.get("tokens_by_decision", {}).items()
                if name != "unreported_attempts"),
            report.get("tokens_by_decision", {}).get("unreported_attempts", 0)),
        "failed/undecided write reasons/stages: %s / %s; undecided ages=%s" % (
            json.dumps(report.get("write_attempts", {}).get("failed_and_undecided", {}).get("reason_code", {}), sort_keys=True),
            json.dumps(report.get("write_attempts", {}).get("failed_and_undecided", {}).get("first_failing_stage", {}), sort_keys=True),
            json.dumps(report.get("write_attempts", {}).get("failed_and_undecided", {}).get("undecided_age_buckets", {}), sort_keys=True)),
        "provider failure categories: %s; context modes (done/attempts)=%s" % (
            json.dumps(report.get("write_attempts", {}).get("provider_failed_categories", {}), sort_keys=True),
            ", ".join("%s %d/%d" % (mode, entry["done"], entry["attempts"]) for mode, entry in
                      report.get("write_attempts", {}).get("telemetry", {}).get("context_modes", {}).items()) or "none"),
        "main interventions: %d (manual clears=%d, cancels=%d, owner failures=%d)" % (
            report["interventions"]["total"], report["interventions"]["manual_clears"],
            report["interventions"]["cancels"], report["interventions"]["owner_finalized_failures"]),
        "incomplete: %s" % report.get("incomplete_reason") if report.get("incomplete")
        else "incomplete: no"))


def usage_line(report, width=100, unicode=True):
    """One row for both providers; a provider without a reading stays on the row as "Codex --"."""
    parts = []
    for provider, title in (("claude", "Claude"), ("codex", "Codex")):
        row = mapping(mapping(report.get("provider_status")).get(provider))
        week_used, context_left = percent(row.get("weekly_used")), percent(row.get("context_left"))
        week = None if week_used is None else 100 - week_used
        context = None if context_left is None else 100 - context_left
        if week is None and context is None:
            parts.append(title + " --")
            continue
        mark = lambda value: "--" if value is None else "%s%%" % value
        parts.append("%s week %s%s%sctx %s%s" % (
            title, mark(week), " left" if week is not None else "", " · " if unicode else " / ",
            mark(context), " used" if context is not None else ""))
    line = (" │ " if unicode else " | ").join(parts)
    if len(line) > width:
        line = (" │ " if unicode else " | ").join(
            p.replace(" week ", " W ").replace("ctx ", "C ") for p in parts)
    return line
