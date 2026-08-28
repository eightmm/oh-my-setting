#!/usr/bin/env python3
"""Export content-free OMS artifact, lifecycle, approval, and landing metadata."""

import argparse
import datetime
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Tuple

from trace_context import persisted_context


ROOT = Path(__file__).resolve().parents[2]
SAFE_NAME = re.compile(r"^[A-Za-z0-9_.-]{1,80}$")
SAFE_LABEL = re.compile(r"^[A-Za-z0-9_.:+/@-]{1,160}$")
FALLBACK_REASONS = {
    "capacity",
    "capacity-no-fallback",
    "capacity-dirty-worktree",
    "model-unavailable",
    "policy-declined",
    "model-safeguard",
}
REASONING_EFFORTS = {"low", "medium", "high", "xhigh", "max", "ultra"}
GEN_AI_PROVIDERS = {
    "codex": "openai",
    "claude": "anthropic",
    "antigravity": "gcp.gen_ai",
    "agy": "gcp.gen_ai",
}


def fail(message: str) -> "NoReturn":
    print("error: %s" % message, file=sys.stderr)
    raise SystemExit(2)


def positive(value: str) -> int:
    try:
        number = int(value)
    except ValueError:
        raise argparse.ArgumentTypeError("must be a positive integer")
    if number <= 0:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return number


def repo_root(path: str) -> Path:
    try:
        result = subprocess.run(
            ["git", "-C", path, "rev-parse", "--show-toplevel"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        fail("cannot resolve --repo: %s" % exc)
    if result.returncode != 0 or not result.stdout.strip():
        fail("--repo is not a git worktree")
    return Path(result.stdout.strip().rstrip("\r")).resolve()


def read_rows(path: Path, limit: int) -> List[Tuple[int, Dict[str, Any]]]:
    rows: List[Tuple[int, Dict[str, Any]]] = []
    try:
        handle = path.open(encoding="utf-8", errors="replace")
    except FileNotFoundError:
        return rows
    except OSError as exc:
        fail("cannot read telemetry source: %s" % exc)
    with handle:
        for line_number, raw in enumerate(handle, 1):
            if len(raw) > 1024 * 1024:
                continue
            raw = raw.strip()
            if not raw:
                continue
            try:
                value = json.loads(raw)
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict):
                rows.append((line_number, value))
    return rows[-limit:]


def digest(namespace: str, value: str, size: int) -> str:
    raw = ("oh-my-setting\0" + namespace + "\0" + value).encode("utf-8")
    return hashlib.sha256(raw).hexdigest()[:size]


def event_span_id(event_id: str) -> str:
    return digest("artifact-span", event_id, 16)


def persisted_ids(
    row: Dict[str, Any], fallback_trace: str, fallback_span: str
) -> Tuple[str, str, Optional[str], Optional[int]]:
    context = persisted_context(row)
    if context is None:
        return fallback_trace, fallback_span, None, None
    flags = int(context["trace_flags"], 16)
    return (
        context["trace_id"],
        context["span_id"],
        context.get("parent_span_id"),
        flags,
    )


def gen_ai_values(
    *, provider: Any, operation: str, model: Any = None,
    input_tokens: Any = None, output_tokens: Any = None,
    cache_read_tokens: Any = None, cache_creation_tokens: Any = None,
) -> Dict[str, Any]:
    canonical = GEN_AI_PROVIDERS.get(str(provider)) if provider else None
    return {
        "gen_ai.operation.name": operation,
        "gen_ai.provider.name": canonical,
        "gen_ai.request.model": safe_label(model),
        "gen_ai.usage.input_tokens": safe_metric(input_tokens, integer=True),
        "gen_ai.usage.output_tokens": safe_metric(output_tokens, integer=True),
        "gen_ai.usage.cache_read.input_tokens": safe_metric(cache_read_tokens, integer=True),
        "gen_ai.usage.cache_creation.input_tokens": safe_metric(cache_creation_tokens, integer=True),
    }


def unix_nanos(value: Any) -> Optional[int]:
    if not isinstance(value, str) or len(value) > 64:
        return None
    try:
        text = value[:-1] + "+00:00" if value.endswith("Z") else value
        stamp = datetime.datetime.fromisoformat(text)
        if stamp.tzinfo is None:
            stamp = stamp.replace(tzinfo=datetime.timezone.utc)
        return int(stamp.timestamp() * 1_000_000_000)
    except (OverflowError, ValueError):
        return None


def safe_number(value: Any) -> Optional[float]:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    number = float(value)
    if number != number or number in (float("inf"), float("-inf")):
        return None
    return number


def safe_metric(value: Any, *, integer: bool = False) -> Optional[Any]:
    if isinstance(value, bool):
        return None
    if integer:
        return value if isinstance(value, int) and value >= 0 else None
    if not isinstance(value, (int, float)):
        return None
    number = safe_number(value)
    return value if number is not None and number >= 0 else None


def safe_exit(value: Any) -> Optional[int]:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def any_value(value: Any) -> Optional[Dict[str, Any]]:
    if isinstance(value, bool):
        return {"boolValue": value}
    if isinstance(value, int):
        return {"intValue": str(value)}
    if isinstance(value, float):
        number = safe_number(value)
        return None if number is None else {"doubleValue": number}
    if isinstance(value, str) and len(value) <= 160:
        return {"stringValue": value}
    return None


def attributes(values: Dict[str, Any]) -> List[Dict[str, Any]]:
    output = []
    for key in sorted(values):
        wrapped = any_value(values[key])
        if wrapped is not None:
            output.append({"key": key, "value": wrapped})
    return output


def safe_label(value: Any) -> Optional[str]:
    if isinstance(value, str) and SAFE_LABEL.fullmatch(value):
        return value
    return None


def safe_choice(value: Any, choices: set) -> Optional[str]:
    return value if isinstance(value, str) and value in choices else None


def correlation(namespace: str, value: Any) -> Optional[str]:
    """Return a joinable opaque key without exporting caller-chosen IDs."""
    if not isinstance(value, str) or not value or len(value) > 256:
        return None
    return digest("correlation-" + namespace, value, 24)


def correlation_attributes(row: Dict[str, Any]) -> Dict[str, Any]:
    return {
        "oms.correlation.run": correlation("run", row.get("run_id")),
        "oms.correlation.attempt": correlation("attempt", row.get("attempt_id")),
        "oms.correlation.parent_attempt": correlation(
            "attempt", row.get("parent_attempt_id")
        ),
        "oms.correlation.task": correlation(
            "task", row.get("task_id") or row.get("task")
        ),
        "oms.correlation.operation": correlation(
            "operation", row.get("operation_id")
        ),
        "oms.correlation.approval": correlation(
            "approval", row.get("approval_id") or row.get("approval")
        ),
        "oms.correlation.landing": correlation("landing", row.get("landing_id")),
    }


def exit_failed(row: Dict[str, Any]) -> bool:
    for key in ("exit", "verify_exit"):
        value = row.get(key)
        if isinstance(value, bool):
            continue
        if isinstance(value, (int, float)) and value != 0:
            return True
        if isinstance(value, str) and value.lstrip("-").isdigit() and int(value) != 0:
            return True
    return False


def artifact_trace_id(row: Dict[str, Any]) -> Optional[str]:
    context = persisted_context(row)
    if context is not None:
        return context["trace_id"]
    event_id = row.get("event_id")
    if not isinstance(event_id, str) or not SAFE_NAME.fullmatch(event_id):
        return None
    trace_key = row.get("run_id") or row.get("operation_id") or event_id
    if not isinstance(trace_key, str):
        trace_key = event_id
    return digest("artifact-trace", trace_key, 32)


def artifact_span(
    row: Dict[str, Any], trace_by_event: Dict[str, str], gen_ai: bool
) -> Optional[Dict[str, Any]]:
    if row.get("schema") != 1:
        return None
    event_id = row.get("event_id")
    kind = row.get("kind")
    if not isinstance(event_id, str) or not SAFE_NAME.fullmatch(event_id):
        return None
    if not isinstance(kind, str) or not SAFE_NAME.fullmatch(kind):
        return None
    end = unix_nanos(row.get("ts"))
    if end is None:
        return None
    duration = safe_number(row.get("duration_s"))
    if duration is None or duration < 0:
        duration = 0.0
    start = max(0, end - int(duration * 1_000_000_000))
    trace_id = artifact_trace_id(row)
    if trace_id is None:
        return None

    values = {
        "oms.source": "artifact",
        "oms.kind": kind,
        "oms.provider": safe_label(row.get("provider")),
        "oms.exit": safe_exit(row.get("exit")),
        "oms.verify_exit": safe_exit(row.get("verify_exit")),
        "oms.model_class": safe_choice(
            row.get("model_class"),
            {"explicit", "provider-default", "fast", "balanced", "deep"},
        ),
        "oms.selected_model": safe_label(row.get("selected_model")),
        "oms.reasoning_effort": safe_choice(
            row.get("reasoning_effort"), REASONING_EFFORTS
        ),
        "oms.fallback_used": (
            row.get("fallback_used") if isinstance(row.get("fallback_used"), bool) else None
        ),
        "oms.fallback_reason": safe_choice(row.get("fallback_reason"), FALLBACK_REASONS),
        "oms.duration_s": safe_metric(row.get("duration_s")),
        "oms.tokens": safe_metric(row.get("tokens"), integer=True),
        "oms.usage.trust": (
            "advisory_provider_reported"
            if safe_metric(row.get("tokens"), integer=True) is not None
            else None
        ),
    }
    values.update(correlation_attributes(row))
    if gen_ai:
        values.update(
            gen_ai_values(
                provider=row.get("provider"),
                operation="invoke_agent",
                model=row.get("selected_model"),
            )
        )
    trace_id, span_id, persisted_parent, trace_flags = persisted_ids(
        row, trace_id, event_span_id(event_id)
    )
    span: Dict[str, Any] = {
        "traceId": trace_id,
        "spanId": span_id,
        "name": "oms." + kind,
        "kind": 1,
        "startTimeUnixNano": str(start),
        "endTimeUnixNano": str(end),
        "attributes": attributes(values),
        "status": {"code": 2 if exit_failed(row) else 0},
    }
    if trace_flags is not None:
        span["flags"] = trace_flags
    if persisted_parent is not None:
        span["parentSpanId"] = persisted_parent
    parent = row.get("parent_event_id")
    if (
        persisted_parent is None
        and
        isinstance(parent, str)
        and SAFE_NAME.fullmatch(parent)
        and trace_by_event.get(parent) == trace_id
    ):
        span["parentSpanId"] = event_span_id(parent)
    return span


def hook_span(line_number: int, row: Dict[str, Any], gen_ai: bool) -> Optional[Dict[str, Any]]:
    if row.get("schema") != 1 or row.get("action") != "telemetry":
        return None
    hook = row.get("hook")
    session = row.get("session")
    if not isinstance(hook, str) or not SAFE_NAME.fullmatch(hook):
        return None
    if not isinstance(session, str) or not session or len(session) > 256:
        return None
    end = unix_nanos(row.get("ts"))
    if end is None:
        return None
    duration_ms = safe_number(row.get("duration_ms"))
    if duration_ms is None or duration_ms < 0:
        duration_ms = 0.0
    start = max(0, end - int(duration_ms * 1_000_000))
    stable = "%s\0%s\0%s\0%d" % (session, row.get("ts"), hook, line_number)
    values = {
        "oms.source": "hook",
        "oms.agent": safe_label(row.get("agent")),
        "oms.hook": hook,
        "oms.tool_name": safe_label(row.get("tool_name")),
        "oms.model": safe_label(row.get("model")),
        "oms.subagent_type": safe_label(row.get("subagent_type")),
        "oms.success": row.get("success") if isinstance(row.get("success"), bool) else None,
        "oms.duration_ms": safe_metric(row.get("duration_ms")),
        "oms.input_tokens": safe_metric(row.get("input_tokens"), integer=True),
        "oms.output_tokens": safe_metric(row.get("output_tokens"), integer=True),
        "oms.cache_read_tokens": safe_metric(row.get("cache_read_tokens"), integer=True),
        "oms.cache_creation_tokens": safe_metric(row.get("cache_creation_tokens"), integer=True),
        "oms.cost_usd": safe_metric(row.get("cost_usd")),
        "oms.usage.trust": "advisory_provider_reported",
    }
    if gen_ai:
        values.update(
            gen_ai_values(
                provider=row.get("agent"),
                operation="execute_tool",
                model=row.get("model"),
                input_tokens=row.get("input_tokens"),
                output_tokens=row.get("output_tokens"),
                cache_read_tokens=row.get("cache_read_tokens"),
                cache_creation_tokens=row.get("cache_creation_tokens"),
            )
        )
    return {
        "traceId": digest("hook-trace", session, 32),
        "spanId": digest("hook-span", stable, 16),
        "name": "oms.hook." + hook,
        "kind": 1,
        "startTimeUnixNano": str(start),
        "endTimeUnixNano": str(end),
        "attributes": attributes(values),
        "status": {"code": 2 if row.get("success") is False else 0},
    }


def lifecycle_span(
    row: Dict[str, Any], context_by_attempt: Dict[str, Dict[str, Any]], gen_ai: bool
) -> Optional[Dict[str, Any]]:
    if row.get("schema") != 1:
        return None
    event_id = row.get("event_id")
    attempt_id = row.get("attempt_id")
    event_type = row.get("event_type")
    if not isinstance(event_id, str) or not SAFE_NAME.fullmatch(event_id):
        return None
    if not isinstance(attempt_id, str) or not SAFE_NAME.fullmatch(attempt_id):
        return None
    if not isinstance(event_type, str) or not SAFE_NAME.fullmatch(event_type):
        return None
    end = unix_nanos(row.get("ts"))
    if end is None:
        return None

    merged = dict(context_by_attempt.get(attempt_id, {}))
    merged.update(row)
    usage = row.get("usage") if isinstance(row.get("usage"), dict) else {}
    actor = row.get("actor") if isinstance(row.get("actor"), dict) else {}
    tokens = safe_metric(usage.get("tokens"), integer=True)
    cost = safe_metric(usage.get("cost_microusd"), integer=True)
    duration = safe_metric(usage.get("duration_ms"), integer=True)
    usage_trust = None
    if tokens is not None or cost is not None:
        usage_trust = "advisory_provider_reported"
    elif duration is not None and actor.get("name") == "attempt-runner":
        usage_trust = "supervisor_measured"
    elif usage:
        usage_trust = "advisory_unverified"
    values = {
        "oms.source": "lifecycle",
        "oms.event_type": event_type,
        "oms.from_state": safe_label(row.get("from_state")),
        "oms.to_state": safe_label(row.get("to_state")),
        "oms.reason_code": safe_label(row.get("reason_code")),
        "oms.provider": safe_label(merged.get("provider")),
        "oms.tool": safe_label(merged.get("tool")),
        "oms.actor": safe_label(actor.get("name")),
        "oms.sequence": safe_metric(row.get("seq"), integer=True),
        "oms.tokens": tokens,
        "oms.cost_microusd": cost,
        "oms.duration_ms": duration,
        "oms.usage.trust": usage_trust,
    }
    values.update(correlation_attributes(merged))
    if gen_ai:
        values.update(
            gen_ai_values(
                provider=merged.get("provider"),
                operation="invoke_agent",
            )
        )
    failed = row.get("to_state") in {"failed", "timed_out", "blocked"}
    trace_id, span_id, parent_span_id, trace_flags = persisted_ids(
        row,
        digest("lifecycle-trace", attempt_id, 32),
        digest("lifecycle-span", event_id, 16),
    )
    span = {
        "traceId": trace_id,
        "spanId": span_id,
        "name": "oms.lifecycle." + event_type,
        "kind": 1,
        "startTimeUnixNano": str(end),
        "endTimeUnixNano": str(end),
        "attributes": attributes(values),
        "status": {"code": 2 if failed else 0},
    }
    if parent_span_id is not None:
        span["parentSpanId"] = parent_span_id
    if trace_flags is not None:
        span["flags"] = trace_flags
    return span


def approval_span(
    row: Dict[str, Any], context_by_approval: Dict[str, Dict[str, Any]]
) -> Optional[Dict[str, Any]]:
    if row.get("schema") != 1:
        return None
    event_id = row.get("event_id")
    approval_id = row.get("approval_id")
    event_type = row.get("event_type")
    if not isinstance(event_id, str) or not SAFE_NAME.fullmatch(event_id):
        return None
    if not isinstance(approval_id, str) or not SAFE_NAME.fullmatch(approval_id):
        return None
    if (
        not isinstance(event_type, str)
        or not event_type.startswith("approval.")
        or not SAFE_NAME.fullmatch(event_type)
    ):
        return None
    end = unix_nanos(row.get("ts"))
    if end is None:
        return None

    merged = dict(context_by_approval.get(approval_id, {}))
    merged.update(row)
    values = {
        "oms.source": "approval",
        "oms.event_type": event_type,
        "oms.state": safe_label(row.get("state")),
        "oms.action": safe_label(merged.get("action")),
        "oms.profile": safe_label(merged.get("profile")),
        "oms.version": safe_metric(row.get("version"), integer=True),
    }
    values.update(correlation_attributes(merged))
    failed = event_type in {"approval.failed", "approval.interrupted"}
    trace_id, span_id, parent_span_id, trace_flags = persisted_ids(
        row,
        digest("approval-trace", approval_id, 32),
        digest("approval-span", event_id, 16),
    )
    span = {
        "traceId": trace_id,
        "spanId": span_id,
        "name": "oms." + event_type,
        "kind": 1,
        "startTimeUnixNano": str(end),
        "endTimeUnixNano": str(end),
        "attributes": attributes(values),
        "status": {"code": 2 if failed else 0},
    }
    if parent_span_id is not None:
        span["parentSpanId"] = parent_span_id
    if trace_flags is not None:
        span["flags"] = trace_flags
    return span


def landing_span(
    line_number: int,
    row: Dict[str, Any],
    context_by_landing: Dict[str, Dict[str, Any]],
) -> Optional[Dict[str, Any]]:
    if row.get("schema") != 1:
        return None
    landing_id = row.get("landing_id")
    event = row.get("event")
    if not isinstance(landing_id, str) or not SAFE_NAME.fullmatch(landing_id):
        return None
    if not isinstance(event, str) or not SAFE_NAME.fullmatch(event):
        return None
    end = unix_nanos(row.get("ts"))
    if end is None:
        return None

    merged = dict(context_by_landing.get(landing_id, {}))
    merged.update(row)
    stable = "%s\0%s\0%s\0%d" % (landing_id, event, row.get("ts"), line_number)
    values = {
        "oms.source": "landing",
        "oms.event": event,
        "oms.reason": safe_label(row.get("reason")),
    }
    values.update(correlation_attributes(merged))
    failed = event in {"applied-pending-receipt", "not-applied-pending-receipt"}
    trace_id, span_id, parent_span_id, trace_flags = persisted_ids(
        row,
        digest("landing-trace", landing_id, 32),
        digest("landing-span", stable, 16),
    )
    span = {
        "traceId": trace_id,
        "spanId": span_id,
        "name": "oms.landing." + event,
        "kind": 1,
        "startTimeUnixNano": str(end),
        "endTimeUnixNano": str(end),
        "attributes": attributes(values),
        "status": {"code": 2 if failed else 0},
    }
    if parent_span_id is not None:
        span["parentSpanId"] = parent_span_id
    if trace_flags is not None:
        span["flags"] = trace_flags
    return span


def private_approval_path(repo: Path) -> Path:
    state_home = os.environ.get("XDG_STATE_HOME")
    if state_home:
        base = Path(state_home).expanduser()
    elif os.name == "nt" and os.environ.get("LOCALAPPDATA"):
        base = Path(os.environ["LOCALAPPDATA"])
    else:
        base = Path.home() / ".local" / "state"
    repo_hash = hashlib.sha256(str(repo.resolve()).encode("utf-8")).hexdigest()
    return base / "oh-my-setting" / "approvals" / (repo_hash + ".jsonl")


def version() -> str:
    try:
        value = (ROOT / "VERSION").read_text(encoding="utf-8").strip()
    except OSError:
        value = "unknown"
    return value[:80] or "unknown"


def envelope(span: Dict[str, Any], scope_name: str, release: str) -> Dict[str, Any]:
    return {
        "resourceSpans": [
            {
                "resource": {
                    "attributes": attributes(
                        {
                            "service.name": "oh-my-setting",
                            "service.version": release,
                        }
                    )
                },
                "scopeSpans": [
                    {
                        "scope": {"name": scope_name, "version": release},
                        "spans": [span],
                    }
                ],
            }
        ]
    }


def encoded_lines(repo: Path, limit: int, hooks: bool, gen_ai: bool) -> Iterable[str]:
    release = version()
    artifact_path = repo / ".oms" / "artifacts" / "index.jsonl"
    artifact_rows = [row for _, row in read_rows(artifact_path, limit)]
    trace_by_event = {
        row["event_id"]: trace_id
        for row in artifact_rows
        for trace_id in [artifact_trace_id(row)]
        if trace_id is not None
    }
    for row in artifact_rows:
        span = artifact_span(row, trace_by_event, gen_ai)
        if span is not None:
            yield json.dumps(
                envelope(span, "oh-my-setting.artifacts", release),
                ensure_ascii=False,
                separators=(",", ":"),
                sort_keys=True,
            )
    if hooks:
        hook_path = repo / ".oms" / "hooks" / "events.jsonl"
        for line_number, row in read_rows(hook_path, limit):
            span = hook_span(line_number, row, gen_ai)
            if span is not None:
                yield json.dumps(
                    envelope(span, "oh-my-setting.hooks", release),
                    ensure_ascii=False,
                    separators=(",", ":"),
                    sort_keys=True,
                )

    lifecycle_path = repo / ".oms" / "lifecycle" / "events.jsonl"
    lifecycle_rows = [row for _, row in read_rows(lifecycle_path, limit)]
    context_by_attempt: Dict[str, Dict[str, Any]] = {}
    for row in lifecycle_rows:
        attempt_id = row.get("attempt_id")
        if (
            row.get("event_type") == "attempt.created"
            and isinstance(attempt_id, str)
        ):
            context_by_attempt[attempt_id] = {
                key: row.get(key)
                for key in (
                    "attempt_id",
                    "parent_attempt_id",
                    "run_id",
                    "task_id",
                    "provider",
                    "tool",
                )
            }
    for row in lifecycle_rows:
        span = lifecycle_span(row, context_by_attempt, gen_ai)
        if span is not None:
            yield json.dumps(
                envelope(span, "oh-my-setting.lifecycle", release),
                ensure_ascii=False,
                separators=(",", ":"),
                sort_keys=True,
            )

    approval_rows = [
        row for _, row in read_rows(private_approval_path(repo), limit)
    ]
    context_by_approval: Dict[str, Dict[str, Any]] = {}
    for row in approval_rows:
        approval_id = row.get("approval_id")
        if (
            row.get("event_type") == "approval.requested"
            and isinstance(approval_id, str)
        ):
            context_by_approval[approval_id] = {
                key: row.get(key)
                for key in (
                    "approval_id",
                    "attempt_id",
                    "task_id",
                    "action",
                    "profile",
                )
            }
    for row in approval_rows:
        span = approval_span(row, context_by_approval)
        if span is not None:
            yield json.dumps(
                envelope(span, "oh-my-setting.approvals", release),
                ensure_ascii=False,
                separators=(",", ":"),
                sort_keys=True,
            )

    landing_path = repo / ".oms" / "landings.jsonl"
    landing_rows = read_rows(landing_path, limit)
    context_by_landing: Dict[str, Dict[str, Any]] = {}
    for _, row in landing_rows:
        landing_id = row.get("landing_id")
        if row.get("event") == "intent" and isinstance(landing_id, str):
            context_by_landing[landing_id] = {
                key: row.get(key)
                for key in ("landing_id", "task", "approval")
            }
    for line_number, row in landing_rows:
        span = landing_span(line_number, row, context_by_landing)
        if span is not None:
            yield json.dumps(
                envelope(span, "oh-my-setting.landings", release),
                ensure_ascii=False,
                separators=(",", ":"),
                sort_keys=True,
            )


def write_file(path: Path, lines: Iterable[str], force: bool) -> None:
    if not path.parent.is_dir():
        fail("output parent directory does not exist")
    temp_name = ""
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            newline="\n",
            prefix=".%s." % path.name,
            dir=str(path.parent),
            delete=False,
        ) as handle:
            temp_name = handle.name
            os.chmod(temp_name, 0o600)
            for line in lines:
                handle.write(line + "\n")
            handle.flush()
            os.fsync(handle.fileno())
        if force:
            os.replace(temp_name, str(path))
            temp_name = ""
        else:
            try:
                os.link(temp_name, str(path))
            except FileExistsError:
                os.unlink(temp_name)
                temp_name = ""
                fail("output exists; pass --force to replace it")
            os.unlink(temp_name)
            temp_name = ""
    except OSError as exc:
        if temp_name:
            try:
                os.unlink(temp_name)
            except OSError:
                pass
        fail("cannot write output: %s" % exc)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Export whitelisted OMS metadata as one OTLP request per JSONL line."
    )
    parser.add_argument("--repo", default=".", help="repository whose local metadata is exported")
    parser.add_argument("--limit", type=positive, default=1000, help="maximum artifact and hook rows")
    parser.add_argument("--output", default="-", help="local JSONL file, or - for stdout")
    parser.add_argument("--force", action="store_true", help="replace an existing output file")
    parser.add_argument("--no-hooks", action="store_true", help="exclude local hook telemetry")
    parser.add_argument(
        "--gen-ai",
        action="store_true",
        help="add content-free OpenTelemetry GenAI semantic attributes",
    )
    args = parser.parse_args()
    if args.force and args.output == "-":
        fail("--force is only valid with --output FILE")

    repo = repo_root(args.repo)
    lines = encoded_lines(repo, args.limit, not args.no_hooks, args.gen_ai)
    if args.output == "-":
        for line in lines:
            print(line)
    else:
        write_file(Path(args.output).resolve(), lines, args.force)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
