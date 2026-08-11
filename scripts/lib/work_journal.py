#!/usr/bin/env python3
"""Internal canonical store and deterministic renderer for Work Journal.

This module is intentionally not dispatched by ``oms``. Lifecycle scripts call
the shell observer in work-journal.sh, which holds the harness file lock before
entering this module.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import math
import os
import pathlib
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from typing import (
    Any,
    Callable,
    Dict,
    Iterable,
    List,
    Mapping,
    Optional,
    Sequence,
    Tuple,
    Union,
)

try:
    from zoneinfo import ZoneInfo
except ImportError:  # pragma: no cover - only old Python installations
    ZoneInfo = None  # type: ignore


SCHEMA_VERSION = 1
RENDERER_VERSION = 1
INDEX_SCHEMA_VERSION = 2
INDEX_RECENT_EVENT_LIMIT = 256
CONFIG_SCHEMA_VERSION = 1
DIGEST_SCHEMA_VERSION = 1
DIGEST_MAX_ITEMS = 3
ANNOTATION_WINDOW_DAYS = 7
HANDOFF_POINTER_MAX_AGE_SECONDS = 48 * 3600
MAX_TEXT_BYTES = 2000
MAX_EXPORT_BYTES = 65536
MAX_COLLECTION_ITEMS = 64
MAX_SOURCE_BYTES = 2 * 1024 * 1024
VERIFICATION_STATUSES = {
    "passed",
    "failed",
    "skipped",
    "not_verified",
    "not_applicable",
}
EVENT_TYPES = {
    "agent_state",
    "annotation",
    "ci",
    "commit",
    "correction",
    "experiment",
    "handoff",
    "job",
    "patch_admit",
    "patch_review",
    "phase_outcome",
    "pull_request",
    "run",
    "task_outcome",
    "validation",
}
# Rendered section headings. The canonical Markdown stays template-driven; the
# language only changes labels, never which facts are admitted. Changing the
# language re-renders on the next materialization of a dirty period, so run
# `oms journal rebuild` once after switching to convert existing summaries.
_HEADINGS = {
    "ko": {
        "empty": "- 기록 없음",
        "daily_progress": "핵심 진전",
        "daily_by_project": "프로젝트별 작업",
        "verified": "검증된 것",
        "unverified": "아직 검증되지 않은 것",
        "failed": "실패하거나 보류한 접근",
        "decisions": "의사결정",
        "blockers": "Blockers",
        "next": "다음 우선순위",
        "work": "작업",
        "evidence": "관련 evidence",
        "result": "결과",
        "interpretation": "해석",
        "weekly_by_project": "프로젝트별 진전",
        "weekly_verified": "완료하거나 검증한 작업",
        "recurring_blockers": "반복 Blockers",
        "weekly_decisions": "주요 의사결정",
        "trends": "비교 가능한 실험 추세",
        "weekly_next": "다음 주 우선순위",
        "repeated": "반복 %d회",
        "sessions": "세션",
        "sessions_restore": "복원: `oms session-handoff list`로 해당 세션의 핸드오프를 찾을 수 있음",
    },
    "en": {
        "empty": "- none recorded",
        "daily_progress": "Key progress",
        "daily_by_project": "Work by project",
        "verified": "Verified",
        "unverified": "Not yet verified",
        "failed": "Failed or parked approaches",
        "decisions": "Decisions",
        "blockers": "Blockers",
        "next": "Next priorities",
        "work": "work",
        "evidence": "related evidence",
        "result": "result",
        "interpretation": "interpretation",
        "weekly_by_project": "Progress by project",
        "weekly_verified": "Completed or verified",
        "recurring_blockers": "Recurring blockers",
        "weekly_decisions": "Key decisions",
        "trends": "Comparable metric trends",
        "weekly_next": "Next week priorities",
        "repeated": "repeated %d times",
        "sessions": "Sessions",
        "sessions_restore": "restore: `oms session-handoff list` locates each session's handoff",
    },
}
_EMPTY_MARKERS = {"- 기록 없음", "- none", "- none recorded"}


def _locale_language() -> str:
    """Language implied by the environment's locale, or empty.

    Read from the environment strings rather than the locale module: the
    answer must not depend on which locales happen to be generated on the
    machine, and hooks inherit these variables verbatim.
    """

    for name in ("LC_ALL", "LC_MESSAGES", "LANG"):
        value = os.environ.get(name, "").strip()
        if not value:
            continue
        tag = re.split(r"[._@-]", value)[0].lower()
        return tag if tag in _HEADINGS else ""
    return ""


def journal_language() -> str:
    """Label language for rendered summaries.

    Resolution order: the environment override, the language pinned in the
    Work Journal config, the machine locale, then English. The pinned config
    exists because the two can legitimately disagree — a journal is written in
    its author's language, not in the language their shell reports — and
    because hooks render in whatever environment the lifecycle script
    inherited, where a shell variable cannot be assumed.
    """

    value = os.environ.get("OMS_WORK_JOURNAL_LANG", "").strip().lower()
    if value in _HEADINGS:
        return value
    return configured_journal_language() or _locale_language() or "en"


def _headings() -> Dict[str, str]:
    return _HEADINGS[journal_language()]


DEFAULT_NOTION_PROPERTIES = {
    "title": "Name",
    "key": "Work Journal Key",
    "hash": "Content Hash",
    "project": "Project",
    "kind": "Kind",
    "period": "Period",
    "blocker": "Has Blocker",
    # Optional columns: written only when the user's database carries them.
    "sessions": "Sessions",
    "commits": "Commits",
    "verified": "Verified",
}
_NOTION_ID_RE = re.compile(
    r"^(?:[0-9A-Fa-f]{32}|[0-9A-Fa-f]{8}(?:-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12})$"
)

_SECRET_KEY_RE = re.compile(
    r"(?i)(?:^|_)(?:authorization|api_?key|token|secret|password|passwd|credential)s?(?:_|$)"
)
_RAW_KEY_RE = re.compile(
    r"(?i)^(?:raw|stdout|stderr|transcript|conversation|environment|env|diff|"
    r"output_log|log_text|full_log|workspace_dump)$"
)
_AUTH_RE = re.compile(r"(?i)\b(authorization\s*[:=]\s*).*$")
_BEARER_RE = re.compile(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}")
_SECRET_ASSIGN_RE = re.compile(
    r"(?i)\b(?P<key>api[\s_-]?key|token|secret|password|passwd|credential)"
    r"\s*[:=]\s*(?:\"[^\"]*\"|'[^']*'|[^\s,;]+)"
)
_TOKEN_RE = re.compile(
    r"(?:gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}|"
    r"sk-(?:live|test)?[_-]?[A-Za-z0-9_-]{10,}|"
    r"xox[bap]-[A-Za-z0-9-]{10,}|hf_[A-Za-z0-9]{20,})"
)
_CREDENTIAL_URL_RE = re.compile(
    r"(?P<scheme>[A-Za-z][A-Za-z0-9+.-]*://)"
    r"(?P<userinfo>[^/\s@]+@)(?P<host>[^/\s]+)"
)
_POSIX_PRIVATE_PATH_RE = re.compile(r"(?:/home|/Users)/[^/\s]+(?:/[^\s]*)?")
_WINDOWS_PRIVATE_PATH_RE = re.compile(r"(?i)[A-Z]:\\Users\\[^\\\s]+(?:\\[^\s]*)?")


class JournalError(Exception):
    """Base Work Journal error."""


class SchemaError(JournalError):
    """Invalid or unsupported event schema."""


# Section ordering for the human-facing Notion view: the reader wants the
# day's judgment first (decisions, blockers, what is next) and the raw
# progress listing last. Unknown titles keep their relative order in between.
_NOTION_SECTION_RANK = {
    "의사결정": 0, "Decisions": 0, "주요 의사결정": 0, "Key decisions": 0,
    "Blockers": 1, "반복 Blockers": 1, "Recurring blockers": 1,
    "다음 우선순위": 2, "Next priorities": 2,
    "다음 주 우선순위": 2, "Next week priorities": 2,
    "실패하거나 보류한 접근": 4, "Failed or parked approaches": 4,
    "핵심 진전": 5, "Key progress": 5,
    "프로젝트별 작업": 5, "Work by project": 5,
    "프로젝트별 진전": 5, "Progress by project": 5,
    "세션": 6, "Sessions": 6,
}


def notion_presentation(content: str) -> str:
    """Human-facing rendering of a summary for the Notion mirror.

    The local daily files are the evidence layer: every claim carries an
    event-id citation, commit bullets carry their hashes, and one packet
    update can emit near-identical bullets from its update and close events.
    Notion is the surface the human actually reads, so the mirror drops the
    trailing ``[wj_...]`` citations, the ``Commit <hash>:`` prefixes and the
    raw evidence bullets, floats decisions, blockers, and next priorities
    above the progress listing, folds a bullet that several sections repeat
    down to its single highest-ranked section, and removes sections left
    saying nothing. Deterministic text transforms only — nothing is
    summarized or rewritten, and the local files keep full provenance.
    """

    citation = re.compile(r"\s*\[wj_[0-9a-f]{8,}.*\]\s*$")
    commit_prefix = re.compile(r"^- ((?:작업|work): )?Commit [0-9a-f]{7,40}: ")
    # The renderer indents an evidence bullet under its work item, so this
    # anchor has to tolerate leading space; a top-level-only anchor let full
    # commit hashes and handoff digests through to the human page.
    evidence_bullet = re.compile(r"^\s*- (관련 evidence|related evidence): ")
    lines: List[str] = []
    for line in content.splitlines():
        # Raw evidence references (full hashes, task ids) are the local
        # provenance layer, not reading material.
        if evidence_bullet.match(line):
            continue
        if line.startswith("- "):
            lines.append(
                commit_prefix.sub(
                    lambda m: "- " + (m.group(1) or ""), citation.sub("", line)
                )
            )
        else:
            lines.append(line)

    preamble: List[str] = []
    sections: List[Dict[str, Any]] = []
    for line in lines:
        if line.startswith("## "):
            sections.append({"title": line[3:].strip(), "lines": [line]})
        elif sections:
            sections[-1]["lines"].append(line)
        else:
            preamble.append(line)

    ordered = sorted(sections, key=lambda s: _NOTION_SECTION_RANK.get(s["title"], 3))

    # The same fact reaches progress, verified, and by-project listings, so a
    # per-section fold left the reader the same bullet three times. Folding
    # runs over the reordered sections rather than the source order: the
    # section the reader meets first is the one that keeps the bullet.
    seen: set = set()

    def fold(block: Iterable[str]) -> List[str]:
        kept: List[str] = []
        for line in block:
            if line.startswith("- ") and line not in _EMPTY_MARKERS:
                if line in seen:
                    continue
                seen.add(line)
            kept.append(line)
        return kept

    preamble = fold(preamble)
    for section in ordered:
        section["lines"] = fold(section["lines"])

    def says_nothing(section: Dict[str, Any]) -> bool:
        # A heading left behind by the fold — including the ``### project``
        # heading of an emptied by-project section — is not a body.
        body = [
            l for l in section["lines"][1:] if l.strip() and not l.startswith("#")
        ]
        return not body or all(l in _EMPTY_MARKERS for l in body)

    ordered = [s for s in ordered if not says_nothing(s)]

    # Decisions are the page's judgment layer: mark their bullets as quotes so
    # the exporter renders them as callouts instead of one more bullet list.
    # Deterministic transform only — the text itself is untouched.
    for section in ordered:
        if _NOTION_SECTION_RANK.get(section["title"], 3) == 0:
            section["lines"] = [
                "> " + line[2:]
                if line.startswith("- ") and line not in _EMPTY_MARKERS
                else line
                for line in section["lines"]
            ]

    out = list(preamble)
    for section in ordered:
        while out and not out[-1].strip():
            out.pop()
        out.extend(["", *section["lines"]])
    return "\n".join(out).strip("\n")


def _sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _canonical_bytes(value: Any) -> bytes:
    return json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
    ).encode("utf-8")


def _truncate_utf8(value: str, maximum: int = MAX_TEXT_BYTES) -> str:
    raw = value.encode("utf-8")
    if len(raw) <= maximum:
        return value
    return raw[:maximum].decode("utf-8", errors="ignore")


def sanitize_text(value: str, maximum: int = MAX_TEXT_BYTES) -> str:
    """Redact common credentials and machine-private paths, then bound bytes."""

    clean = "".join(char if char >= " " or char == "\t" else " " for char in str(value))
    clean = _AUTH_RE.sub(r"\1[REDACTED]", clean)
    clean = _BEARER_RE.sub("Bearer [REDACTED]", clean)
    clean = _SECRET_ASSIGN_RE.sub(
        lambda match: match.group("key") + "=[REDACTED]", clean
    )
    clean = _TOKEN_RE.sub("[REDACTED]", clean)
    clean = _CREDENTIAL_URL_RE.sub(
        lambda match: match.group("scheme") + "[REDACTED]@" + match.group("host"), clean
    )
    clean = _POSIX_PRIVATE_PATH_RE.sub("<private-path>", clean)
    clean = _WINDOWS_PRIVATE_PATH_RE.sub("<private-path>", clean)
    clean = re.sub(r"[ \t]+", " ", clean).strip()
    return _truncate_utf8(clean, maximum)


def sanitize_multiline(value: str, maximum: int = MAX_EXPORT_BYTES) -> str:
    """Sanitize rendered Markdown without collapsing its line structure.

    sanitize_text() folds newlines into spaces, which turns a summary into one
    unparseable line: heading/bullet detection in the Notion exporter and the
    blocker scan both read line starts.
    """

    lines = [sanitize_text(line) for line in str(value).splitlines()]
    return _truncate_utf8("\n".join(lines), maximum)


def sanitize(value: Any, key: str = "") -> Any:
    """Recursively sanitize bounded structured data.

    Raw-heavy keys are discarded even when their current value looks harmless;
    secret-shaped keys retain only an explicit redaction marker.
    """

    if key and _RAW_KEY_RE.match(key):
        return None
    if key and _SECRET_KEY_RE.search(key):
        return "[REDACTED]"
    if value is None or isinstance(value, bool):
        return value
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    if isinstance(value, str):
        return sanitize_text(value)
    if isinstance(value, Mapping):
        clean: Dict[str, Any] = {}
        for index, (raw_key, raw_value) in enumerate(value.items()):
            if index >= MAX_COLLECTION_ITEMS:
                break
            child_key = sanitize_text(str(raw_key), 120)
            if not child_key or _RAW_KEY_RE.match(child_key):
                continue
            child = sanitize(raw_value, child_key)
            if child is not None:
                clean[child_key] = child
        return clean
    if isinstance(value, (list, tuple)):
        clean_list = []
        for item in value[:MAX_COLLECTION_ITEMS]:
            clean = sanitize(item)
            if clean is not None:
                clean_list.append(clean)
        return clean_list
    return sanitize_text(str(value))


def _utc_rfc3339(value: dt.datetime) -> str:
    if value.tzinfo is None:
        raise SchemaError("timestamp must include timezone")
    return value.astimezone(dt.timezone.utc).replace(microsecond=0).isoformat().replace(
        "+00:00", "Z"
    )


def parse_rfc3339(value: str) -> dt.datetime:
    if not isinstance(value, str) or not value:
        raise SchemaError("occurred_at must be an RFC 3339 timestamp")
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise SchemaError("occurred_at must be an RFC 3339 timestamp") from exc
    if parsed.tzinfo is None:
        raise SchemaError("occurred_at must include timezone")
    return parsed.astimezone(dt.timezone.utc)


def _zoneinfo_candidate() -> Optional[str]:
    env_tz = os.environ.get("TZ", "").lstrip(":")
    if env_tz:
        return env_tz
    try:
        current = dt.datetime.now().astimezone().tzinfo
        key = getattr(current, "key", "")
        if key:
            return str(key)
    except Exception:
        pass
    try:
        configured = pathlib.Path("/etc/timezone").read_text(encoding="utf-8").strip()
        if configured:
            return configured
    except OSError:
        pass
    try:
        target = os.path.realpath("/etc/localtime").replace("\\", "/")
        marker = "/zoneinfo/"
        if marker in target:
            return target.split(marker, 1)[1]
    except OSError:
        pass
    return None


def resolve_timezone(name: Optional[str] = None) -> Tuple[str, Optional[dt.tzinfo]]:
    explicit = name or os.environ.get("OMS_WORK_JOURNAL_TIMEZONE")
    requested = explicit or _zoneinfo_candidate()
    if requested:
        if requested in ("UTC", "Etc/UTC"):
            return requested, dt.timezone.utc
        if ZoneInfo is None:
            return "system-local", None
        try:
            if ZoneInfo is not None:
                return requested, ZoneInfo(requested)
        except Exception as exc:
            try:
                # Distinguish an invalid configured name from a platform with
                # no timezone database at all (stock Windows Python). The latter
                # keeps capture alive in explicit, recorded system-local mode.
                ZoneInfo("Etc/UTC")
            except Exception:
                return "system-local", None
            if explicit:
                raise JournalError("invalid Work Journal timezone") from exc
    # ``astimezone(None)`` asks the operating system for the offset applicable
    # to each event timestamp. Keeping None here is DST-safe on Windows, where
    # Python often has no IANA database and ``now().astimezone().tzinfo`` is only
    # the current fixed offset.
    return "system-local", None


def _git_output(repo: pathlib.Path, args: Sequence[str]) -> str:
    try:
        return subprocess.check_output(
            ["git", "-C", str(repo)] + list(args),
            stderr=subprocess.DEVNULL,
            text=True,
        ).strip()
    except (OSError, subprocess.CalledProcessError):
        return ""


def canonical_github_repository(remote: str) -> Optional[Tuple[str, str]]:
    """Normalize common GitHub remotes to a protocol-independent key/name."""

    value = str(remote or "").strip().replace("\\", "/")
    match = re.match(
        r"^[A-Za-z][A-Za-z0-9+.-]*://(?:[^@/]+@)?([^/:]+)(?::[0-9]+)?/(.+)$",
        value,
    )
    if match is None:
        match = re.match(r"^(?:[^@/:]+@)?([^/:]+):(.+)$", value)
    if match is None or match.group(1).lower() != "github.com":
        return None
    path = match.group(2).split("?", 1)[0].split("#", 1)[0].strip("/")
    if path.lower().endswith(".git"):
        path = path[:-4]
    parts = [part for part in path.split("/") if part]
    if len(parts) != 2 or any(part in (".", "..") for part in parts):
        return None
    slug = "%s/%s" % (parts[0].lower(), parts[1].lower())
    return "github.com/%s" % slug, slug


def project_identity(repo: pathlib.Path) -> Tuple[str, str]:
    root = _git_output(repo, ["rev-parse", "--show-toplevel"])
    canonical = pathlib.Path(root or repo).resolve()
    remote = _git_output(canonical, ["config", "--get", "remote.origin.url"])
    github = canonical_github_repository(remote)
    if github is not None:
        identity, name = github
    else:
        identity = sanitize_text(remote, 1000) if remote else str(canonical)
        name = sanitize_text(canonical.name, 120) or "project"
    project_id = "proj_" + _sha256_bytes(identity.encode("utf-8"))[:20]
    return project_id, name


def _clean_record(
    value: Any,
    allowed: Sequence[str],
    required: Sequence[str] = (),
) -> Dict[str, Any]:
    if not isinstance(value, Mapping):
        raise SchemaError("structured field must be an object")
    output: Dict[str, Any] = {}
    for key in allowed:
        if key not in value:
            continue
        clean = sanitize(value[key], key)
        if clean is not None and clean != "" and clean != [] and clean != {}:
            output[key] = clean
    for key in required:
        if not output.get(key):
            raise SchemaError("missing required field: %s" % key)
    return output


def _clean_records(value: Any, allowed: Sequence[str]) -> List[Dict[str, Any]]:
    if not isinstance(value, list):
        return []
    rows = []
    for item in value[:MAX_COLLECTION_ITEMS]:
        try:
            row = _clean_record(item, allowed)
        except SchemaError:
            continue
        if row:
            rows.append(row)
    return rows


def _event_identity(
    normalized: Mapping[str, Any],
    *,
    authoritative_source_id: bool,
    operation_id: Optional[str],
) -> str:
    source = normalized.get("source") or {}
    source_id = source.get("id") if isinstance(source, Mapping) else None
    if authoritative_source_id and source_id:
        basis = {
            "project_id": normalized["project_id"],
            "source_type": source.get("type"),
            "source_id": source_id,
        }
    elif operation_id:
        basis = {
            "project_id": normalized["project_id"],
            "operation_id": operation_id,
            "event_type": normalized["event_type"],
        }
    else:
        basis = {
            key: value
            for key, value in normalized.items()
            if key
            not in {
                "event_id",
                "recorded_at",
                "occurred_at",
                "local_date",
                "iso_week",
                "timezone",
            }
        }
    return "wj_" + _sha256_bytes(_canonical_bytes(basis))[:32]


def normalize_event(
    payload: Mapping[str, Any],
    *,
    project_id: str,
    project_name: str,
    timezone_name: str,
    timezone_info: Optional[dt.tzinfo],
    recorded_at: dt.datetime,
) -> Dict[str, Any]:
    if not isinstance(payload, Mapping):
        raise SchemaError("event must be an object")
    supplied_version = payload.get("schema_version", SCHEMA_VERSION)
    if supplied_version != SCHEMA_VERSION:
        raise SchemaError("unsupported schema version")
    event_type = sanitize_text(str(payload.get("event_type", "")), 80)
    if event_type not in EVENT_TYPES:
        raise SchemaError("invalid event_type")
    occurred = parse_rfc3339(str(payload.get("occurred_at", "")))
    local = occurred.astimezone(timezone_info)
    source = _clean_record(
        payload.get("source"), ("type", "id", "path", "url"), required=("type",)
    )
    authoritative_source_id = bool(source.get("id"))
    correlation = _clean_record(
        payload.get("correlation", {}),
        ("task_id", "session_id", "phase_id", "run_id", "operation_id"),
    )
    operation_id = correlation.get("operation_id")
    if not source.get("id"):
        if operation_id:
            source["id"] = "op_" + _sha256_bytes(
                str(operation_id).encode("utf-8")
            )[:24]
        else:
            stable_source = {
                key: value
                for key, value in payload.items()
                if key not in {"recorded_at", "occurred_at", "schema_version"}
            }
            source["id"] = "src_" + _sha256_bytes(
                _canonical_bytes(sanitize(stable_source))
            )[:24]

    verification = sanitize_text(str(payload.get("verification_status", "")), 40)
    if verification not in VERIFICATION_STATUSES:
        raise SchemaError("invalid verification_status")

    normalized: Dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "project_id": sanitize_text(project_id, 120),
        "project_name": sanitize_text(project_name, 120),
        "event_type": event_type,
        "occurred_at": _utc_rfc3339(occurred),
        "recorded_at": _utc_rfc3339(recorded_at),
        "timezone": sanitize_text(timezone_name, 120),
        "local_date": local.date().isoformat(),
        "iso_week": "%04d-W%02d" % (local.isocalendar()[0], local.isocalendar()[1]),
        "source": source,
        "verification_status": verification,
    }

    if correlation:
        normalized["correlation"] = correlation

    optional_records = {
        "outcome": ("summary", "status", "interpretation"),
        "provenance": (
            "code_commit",
            "config_path",
            "config_hash",
            "dataset",
            "split",
            "seed",
            "checkpoint",
            "artifact_id",
            "job_id",
            "run_id",
            "gpu",
            "execution_environment",
            "verdict",
            "command_summary",
            "exit_code",
            "duration_seconds",
            "hypothesis",
            "prediction",
            "baseline",
            "primary_metric",
            "success_criterion",
            "independent_change",
        ),
    }
    for field, allowed in optional_records.items():
        if field in payload:
            row = _clean_record(payload[field], allowed)
            if row:
                normalized[field] = row

    for field in ("goal", "decision", "blocker", "next_action"):
        if field in payload:
            clean = sanitize(payload[field], field)
            if isinstance(clean, str) and clean:
                normalized[field] = clean

    refs = _clean_records(payload.get("refs"), ("type", "id", "path", "url", "label"))
    if refs:
        normalized["refs"] = refs
    evidence = _clean_records(payload.get("evidence"), ("type", "ref", "label"))
    if evidence:
        normalized["evidence"] = evidence

    metrics = _clean_records(
        payload.get("metrics"),
        (
            "name",
            "value",
            "unit",
            "dataset",
            "split",
            "conditions",
            "evidence_ref",
        ),
    )
    # A metric is a claim. Keep only finite scalar values with an explicit
    # evidence reference, or when the event itself has explicit evidence.
    valid_metrics = []
    for metric in metrics:
        value = metric.get("value")
        scalar = (
            not isinstance(value, bool)
            and isinstance(value, (int, float))
            and (not isinstance(value, float) or math.isfinite(value))
        )
        if metric.get("name") and scalar and (metric.get("evidence_ref") or evidence):
            valid_metrics.append(metric)
    if valid_metrics:
        normalized["metrics"] = valid_metrics

    supersedes = sanitize_text(str(payload.get("supersedes_event_id", "")), 120)
    if supersedes:
        if event_type != "correction":
            raise SchemaError("only correction events may supersede")
        normalized["supersedes_event_id"] = supersedes

    normalized["event_id"] = _event_identity(
        normalized,
        authoritative_source_id=authoritative_source_id,
        operation_id=str(operation_id) if operation_id else None,
    )
    validate_canonical_event(normalized)
    return normalized


def validate_canonical_event(event: Mapping[str, Any]) -> None:
    if event.get("schema_version") != SCHEMA_VERSION:
        raise SchemaError("unsupported schema version")
    for key in (
        "event_id",
        "project_id",
        "project_name",
        "event_type",
        "occurred_at",
        "recorded_at",
        "timezone",
        "local_date",
        "iso_week",
        "source",
        "verification_status",
    ):
        if key not in event:
            raise SchemaError("missing canonical field")
    if event.get("event_type") not in EVENT_TYPES:
        raise SchemaError("invalid canonical event_type")
    if event.get("verification_status") not in VERIFICATION_STATUSES:
        raise SchemaError("invalid canonical verification_status")
    if not re.match(r"^wj_[0-9a-f]{32}$", str(event.get("event_id"))):
        raise SchemaError("invalid canonical event_id")
    parse_rfc3339(str(event.get("occurred_at")))
    parse_rfc3339(str(event.get("recorded_at")))
    source = event.get("source")
    if not isinstance(source, Mapping) or not source.get("type") or not source.get("id"):
        raise SchemaError("invalid canonical source")


def atomic_write_text(path: pathlib.Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=path.name + ".tmp-", dir=str(path.parent))
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(text)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except OSError:
            pass
        raise


def atomic_write_json(path: pathlib.Path, value: Any) -> None:
    atomic_write_text(
        path,
        json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2, allow_nan=False)
        + "\n",
    )


def work_journal_config_path() -> pathlib.Path:
    override = os.environ.get("OMS_WORK_JOURNAL_CONFIG", "").strip()
    if override:
        return pathlib.Path(override).expanduser()
    xdg = os.environ.get("XDG_CONFIG_HOME", "").strip()
    if xdg:
        return pathlib.Path(xdg).expanduser() / "oh-my-setting" / "work-journal.json"
    if os.name == "nt":
        base = os.environ.get("LOCALAPPDATA", "").strip()
        if base:
            return pathlib.Path(base) / "oh-my-setting" / "work-journal.json"
    return pathlib.Path.home() / ".config" / "oh-my-setting" / "work-journal.json"


def load_work_journal_config() -> Dict[str, Any]:
    path = work_journal_config_path()
    if not path.is_file():
        return {}
    try:
        row = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, ValueError, TypeError) as exc:
        raise JournalError("invalid Work Journal configuration") from exc
    if not isinstance(row, Mapping) or row.get("schema_version") != CONFIG_SCHEMA_VERSION:
        raise JournalError("unsupported Work Journal configuration")
    notion = row.get("notion")
    if not isinstance(notion, Mapping):
        raise JournalError("invalid Work Journal Notion configuration")
    return dict(row)


def configured_journal_language() -> str:
    """Language pinned in the config file, or empty when none applies.

    Rendering must never fail over a configuration read, so an unreadable or
    unsupported config resolves like an absent one; the explicit commands that
    write the file still report the error.
    """

    try:
        config = load_work_journal_config()
    except JournalError:
        return ""
    value = str(config.get("language") or "").strip().lower()
    return value if value in _HEADINGS else ""


def set_journal_language(language: str) -> pathlib.Path:
    """Pin the rendered-summary language durably and return the config path."""

    value = str(language or "").strip().lower()
    if value not in _HEADINGS:
        raise JournalError("unsupported journal language: %s" % language)
    path = work_journal_config_path()
    config = dict(load_work_journal_config())
    if not config:
        config = {
            "schema_version": CONFIG_SCHEMA_VERSION,
            "managed_by": "oh-my-setting",
            # A language-only pin still carries the Notion object: the loader
            # rejects a file without one, and every other reader goes through
            # it.
            "notion": {},
        }
    config["language"] = value
    atomic_write_json(path, config)
    try:
        path.chmod(0o600)
    except OSError:
        pass
    return path


def notion_settings() -> Dict[str, Any]:
    config = load_work_journal_config()
    notion = config.get("notion") if isinstance(config.get("notion"), Mapping) else {}
    properties = (
        notion.get("properties")
        if isinstance(notion, Mapping) and isinstance(notion.get("properties"), Mapping)
        else {}
    )

    def configured(name: str, environment: str, default: str = "") -> str:
        value = os.environ.get(environment)
        if value is not None and value.strip():
            return value.strip()
        if isinstance(properties, Mapping) and properties.get(name):
            return str(properties[name]).strip()
        return default

    data_source_id = os.environ.get(
        "OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID", ""
    ).strip()
    database_id = os.environ.get("OMS_WORK_JOURNAL_NOTION_DATABASE_ID", "").strip()
    if not data_source_id and not database_id and isinstance(notion, Mapping):
        data_source_id = str(notion.get("data_source_id") or "").strip()
        database_id = str(notion.get("database_id") or "").strip()
    access_value = os.environ.get("OMS_WORK_JOURNAL_NOTION_TOKEN", "").strip()
    configured_auth = ""
    if isinstance(notion, Mapping):
        configured_auth = str(notion.get("auth_mode") or "").strip().lower()
    auth_mode = "token" if access_value else (
        os.environ.get("OMS_WORK_JOURNAL_NOTION_AUTH", "").strip().lower()
        or configured_auth
    )
    # The transport choice is durable, non-secret config: the docs promise it
    # persists, and hooks run in environments where PATH order and env vars
    # cannot be assumed. Environment overrides remain for one-shot use.
    configured_cli = ""
    configured_keyring = ""
    if isinstance(notion, Mapping):
        configured_cli = str(notion.get("cli_command") or "").strip()
        configured_keyring = str(notion.get("keyring") or "").strip().lower()
    cli_command = (
        os.environ.get("OMS_NOTION_CLI", "").strip() or configured_cli or "ntn"
    )
    keyring = (
        os.environ.get("OMS_NOTION_KEYRING", "").strip().lower()
        or configured_keyring
    )
    return {
        "access_value": access_value,
        "auth_mode": auth_mode,
        "cli_command": cli_command,
        "keyring": keyring,
        "data_source_id": data_source_id,
        "database_id": database_id,
        "title_property": configured(
            "title", "OMS_WORK_JOURNAL_NOTION_TITLE_PROPERTY", "Name"
        ),
        "key_property": configured(
            "key", "OMS_WORK_JOURNAL_NOTION_KEY_PROPERTY", "Work Journal Key"
        ),
        "hash_property": configured(
            "hash", "OMS_WORK_JOURNAL_NOTION_HASH_PROPERTY", "Content Hash"
        ),
        "project_property": configured(
            "project", "OMS_WORK_JOURNAL_NOTION_PROJECT_PROPERTY"
        ),
        "kind_property": configured("kind", "OMS_WORK_JOURNAL_NOTION_KIND_PROPERTY"),
        "period_property": configured(
            "period", "OMS_WORK_JOURNAL_NOTION_PERIOD_PROPERTY"
        ),
        "blocker_property": configured(
            "blocker", "OMS_WORK_JOURNAL_NOTION_BLOCKER_PROPERTY"
        ),
        # Optional columns default to their canonical names: the exporter
        # writes them only when the target database actually carries them, so
        # an unconfigured or older database is never asked to change.
        "sessions_property": configured(
            "sessions", "OMS_WORK_JOURNAL_NOTION_SESSIONS_PROPERTY", "Sessions"
        ),
        "commits_property": configured(
            "commits", "OMS_WORK_JOURNAL_NOTION_COMMITS_PROPERTY", "Commits"
        ),
        "verified_property": configured(
            "verified", "OMS_WORK_JOURNAL_NOTION_VERIFIED_PROPERTY", "Verified"
        ),
        "timeout": float(
            os.environ.get("OMS_WORK_JOURNAL_NOTION_TIMEOUT_SECONDS", "10")
        ),
        "budget_seconds": float(
            os.environ.get("OMS_WORK_JOURNAL_NOTION_BUDGET_SECONDS", "8")
        ),
    }


def notion_auth_available(settings: Mapping[str, Any]) -> bool:
    if settings.get("access_value"):
        return True
    if settings.get("auth_mode") != "ntn":
        return False
    command = str(settings.get("cli_command") or "ntn")
    return bool(shutil.which(command) or pathlib.Path(command).is_file())


def _canonical_repo_path(entry: str) -> str:
    return str(pathlib.Path(str(entry)).expanduser().resolve())


def notion_excluded_repos() -> List[str]:
    """Repos whose journal stays local: the Notion page is the human surface
    for the user's own work, and a repo that was merely cloned does not belong
    on it even when sessions ran there. Kept outside notion_settings() because
    that dict is splatted into NotionJournalExporter.from_config."""
    config = load_work_journal_config()
    notion = config.get("notion") if isinstance(config.get("notion"), Mapping) else {}
    entries: List[str] = []
    raw = notion.get("excluded_repos") if isinstance(notion, Mapping) else None
    if isinstance(raw, list):
        entries.extend(str(item) for item in raw if str(item or "").strip())
    env = os.environ.get("OMS_WORK_JOURNAL_NOTION_EXCLUDE", "")
    entries.extend(part for part in env.split(":") if part.strip())
    resolved: List[str] = []
    for entry in entries:
        try:
            resolved.append(_canonical_repo_path(entry))
        except OSError:
            continue
    return resolved


def notion_repo_excluded(repo: pathlib.Path) -> bool:
    try:
        needle = str(pathlib.Path(repo).resolve())
    except OSError:
        return False
    return needle in set(notion_excluded_repos())


def update_notion_exclusions(
    add: Iterable[str], remove: Iterable[str]
) -> List[str]:
    path = work_journal_config_path()
    config = load_work_journal_config()
    notion = config.get("notion") if isinstance(config.get("notion"), Mapping) else None
    if not path.is_file() or not isinstance(notion, Mapping):
        raise JournalError(
            "Notion is not configured; run configure --discover first"
        )
    result = set()
    raw = notion.get("excluded_repos")
    if isinstance(raw, list):
        for item in raw:
            if str(item or "").strip():
                result.add(_canonical_repo_path(str(item)))
    # Canonical absolute paths make later membership checks plain equality;
    # resolve() tolerates a path that no longer exists, so a deleted clone can
    # still be listed or removed.
    for entry in add:
        result.add(_canonical_repo_path(entry))
    for entry in remove:
        result.discard(_canonical_repo_path(entry))
    updated = dict(notion)
    if result:
        updated["excluded_repos"] = sorted(result)
    else:
        updated.pop("excluded_repos", None)
    new_config = dict(config)
    new_config["notion"] = updated
    atomic_write_json(path, new_config)
    try:
        path.chmod(0o600)
    except OSError:
        pass
    return sorted(result)


def discover_notion_target() -> str:
    settings = notion_settings()
    settings["auth_mode"] = "ntn"
    if not notion_auth_available(settings):
        raise JournalError("Notion CLI is required; install ntn and run ntn login")
    try:
        from notion_journal import (
            NotionCLITransport,
            NotionTransportError,
            discover_work_journal_data_source,
        )
    except ImportError:
        sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
        from notion_journal import (
            NotionCLITransport,
            NotionTransportError,
            discover_work_journal_data_source,
        )
    transport = NotionCLITransport(
        settings["cli_command"],
        "2026-03-11",
        settings.get("keyring") or "",
    )
    try:
        return discover_work_journal_data_source(transport)
    except NotionTransportError as exc:
        raise JournalError(str(exc)) from None


def configure_notion(
    data_source_id: str, *, validate: bool = True, auth_mode: str = ""
) -> pathlib.Path:
    target = str(data_source_id or "").strip()
    if not _NOTION_ID_RE.match(target):
        raise JournalError("invalid Notion data source id")
    settings = notion_settings()
    if auth_mode:
        settings["auth_mode"] = auth_mode
    settings["data_source_id"] = target
    settings["database_id"] = ""
    settings.update(
        {
            "title_property": DEFAULT_NOTION_PROPERTIES["title"],
            "key_property": DEFAULT_NOTION_PROPERTIES["key"],
            "hash_property": DEFAULT_NOTION_PROPERTIES["hash"],
            "project_property": DEFAULT_NOTION_PROPERTIES["project"],
            "kind_property": DEFAULT_NOTION_PROPERTIES["kind"],
            "period_property": DEFAULT_NOTION_PROPERTIES["period"],
            "blocker_property": DEFAULT_NOTION_PROPERTIES["blocker"],
            "sessions_property": DEFAULT_NOTION_PROPERTIES["sessions"],
            "commits_property": DEFAULT_NOTION_PROPERTIES["commits"],
            "verified_property": DEFAULT_NOTION_PROPERTIES["verified"],
        }
    )
    if validate and notion_auth_available(settings):
        try:
            from notion_journal import NotionJournalExporter
        except ImportError:
            sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
            from notion_journal import NotionJournalExporter
        exporter = NotionJournalExporter.from_config(**settings)
        exporter.validate_target()
    path = work_journal_config_path()
    notion_config = {
        "data_source_id": target,
        "properties": dict(DEFAULT_NOTION_PROPERTIES),
        "sync_mode": "finalized",
    }
    # A reconfigure rewrites the whole file, so the exclusion list must be
    # carried over or it silently vanishes on the next configure --discover.
    prior = load_work_journal_config()
    prior_notion = (
        prior.get("notion") if isinstance(prior.get("notion"), Mapping) else {}
    )
    prior_excluded = (
        prior_notion.get("excluded_repos") if isinstance(prior_notion, Mapping) else None
    )
    if isinstance(prior_excluded, list) and prior_excluded:
        notion_config["excluded_repos"] = [str(item) for item in prior_excluded]
    # ntn is a durable, non-secret transport choice. Environment credentials
    # need no persisted marker, and omitting one also avoids implying that an
    # unvalidated target has a usable credential.
    if settings.get("auth_mode") == "ntn":
        notion_config["auth_mode"] = "ntn"
        notion_config["cli_command"] = settings.get("cli_command") or "ntn"
        # A machine without a usable OS keychain authenticates ntn through its
        # file-based store; the hooks must inherit that choice from config,
        # not from whichever shell happened to run configure.
        if settings.get("keyring") == "file":
            notion_config["keyring"] = "file"
    written = {
        "schema_version": CONFIG_SCHEMA_VERSION,
        "managed_by": "oh-my-setting",
        "notion": notion_config,
    }
    # Same reason as the exclusion list: a whole-file rewrite must carry the
    # language pin or reconfiguring Notion silently reverts the journal to the
    # machine locale.
    prior_language = str(prior.get("language") or "").strip().lower()
    if prior_language in _HEADINGS:
        written["language"] = prior_language
    atomic_write_json(path, written)
    try:
        path.chmod(0o600)
    except OSError:
        pass
    return path


def purge_notion_config() -> bool:
    path = work_journal_config_path()
    if not path.is_file():
        return False
    config = load_work_journal_config()
    if config.get("managed_by") != "oh-my-setting":
        return False
    path.unlink()
    try:
        path.parent.rmdir()
    except OSError:
        pass
    return True


def _safe_relpath(repo: pathlib.Path, path: pathlib.Path) -> Optional[str]:
    try:
        resolved_repo = repo.resolve()
        resolved = path.resolve()
        relative = resolved.relative_to(resolved_repo)
        return relative.as_posix()
    except (OSError, ValueError):
        return None


def _event_citation(event: Mapping[str, Any]) -> str:
    parts = [str(event["event_id"])]
    source = event.get("source") or {}
    if source.get("type") and source.get("id"):
        parts.append("source %s:%s" % (source["type"], source["id"]))
    evidence = event.get("evidence") or []
    for item in evidence[:3]:
        if item.get("type") and item.get("ref"):
            parts.append("evidence %s:%s" % (item["type"], item["ref"]))
    return "[" + "; ".join(parts) + "]"


def _outcome_summary(event: Mapping[str, Any]) -> str:
    outcome = event.get("outcome") or {}
    if outcome.get("summary"):
        return str(outcome["summary"])
    return event["event_type"].replace("_", " ")


def _empty_or_lines(lines: Iterable[str]) -> List[str]:
    materialized = list(lines)
    return materialized or [_headings()["empty"]]


def render_daily(period: str, events: Sequence[Mapping[str, Any]]) -> str:
    labels = _headings()
    ordered = sorted(events, key=lambda row: (row["occurred_at"], row["event_id"]))
    project = ordered[0]["project_name"] if ordered else "project"
    progress = [
        "- %s %s" % (_outcome_summary(event), _event_citation(event))
        for event in ordered
        if event["event_type"] not in {"handoff"}
    ]
    work: List[str] = []
    for event in ordered:
        work.append(
            "- %s: %s %s"
            % (labels["work"], _outcome_summary(event), _event_citation(event))
        )
        if event.get("evidence"):
            refs = ", ".join(
                "%s:%s" % (row.get("type"), row.get("ref"))
                for row in event["evidence"]
                if row.get("type") and row.get("ref")
            )
            if refs:
                work.append("  - %s: %s" % (labels["evidence"], refs))
        outcome = event.get("outcome") or {}
        if outcome.get("status"):
            work.append("  - %s: %s" % (labels["result"], outcome["status"]))
        if outcome.get("interpretation"):
            work.append(
                "  - %s: %s" % (labels["interpretation"], outcome["interpretation"])
            )

    verified = [
        "- %s %s" % (_outcome_summary(event), _event_citation(event))
        for event in ordered
        if event["verification_status"] == "passed"
    ]
    unverified = [
        "- %s %s" % (_outcome_summary(event), _event_citation(event))
        for event in ordered
        if event["verification_status"] == "not_verified"
    ]
    failed = []
    for event in ordered:
        status = (event.get("outcome") or {}).get("status")
        if event["verification_status"] in {"failed", "skipped"} or status in {
            "failure",
            "failed",
            "deferred",
            "cancelled",
            "aborted",
        }:
            failed.append("- %s %s" % (_outcome_summary(event), _event_citation(event)))
    decisions = [
        "- %s %s" % (event["decision"], _event_citation(event))
        for event in ordered
        if event.get("decision")
    ]
    blockers = [
        "- %s %s" % (event["blocker"], _event_citation(event))
        for event in ordered
        if event.get("blocker")
    ]
    next_actions = [
        "- %s %s" % (event["next_action"], _event_citation(event))
        for event in ordered
        if event.get("next_action")
    ]
    session_ids: List[str] = []
    for event in ordered:
        sid = str((event.get("correlation") or {}).get("session_id") or "").strip()
        if sid and sid not in session_ids:
            session_ids.append(sid)
    sessions = [
        "- %s (%d event(s))"
        % (
            sid,
            sum(
                1
                for event in ordered
                if str((event.get("correlation") or {}).get("session_id") or "")
                == sid
            ),
        )
        for sid in session_ids
    ]
    if sessions:
        sessions.append("- %s" % labels["sessions_restore"])

    sections: List[str] = [
        "# Daily Work Journal — %s" % period,
        "",
        "## %s" % labels["daily_progress"],
        "",
        *_empty_or_lines(progress),
        "",
        "## %s" % labels["daily_by_project"],
        "",
        "### %s" % project,
        "",
        *_empty_or_lines(work),
        "",
        "## %s" % labels["verified"],
        "",
        *_empty_or_lines(verified),
        "",
        "## %s" % labels["unverified"],
        "",
        *_empty_or_lines(unverified),
        "",
        "## %s" % labels["failed"],
        "",
        *_empty_or_lines(failed),
        "",
        "## %s" % labels["decisions"],
        "",
        *_empty_or_lines(decisions),
        "",
        "## %s" % labels["blockers"],
        "",
        *_empty_or_lines(blockers),
        "",
        "## %s" % labels["next"],
        "",
        *_empty_or_lines(next_actions),
        "",
        "## %s" % labels["sessions"],
        "",
        *_empty_or_lines(sessions),
        "",
    ]
    return "\n".join(sections)


def _metric_signature(metric: Mapping[str, Any]) -> Tuple[str, str, str, str, str]:
    return tuple(
        str(metric.get(key, ""))
        for key in ("name", "unit", "dataset", "split", "conditions")
    )  # type: ignore


def render_weekly(period: str, events: Sequence[Mapping[str, Any]]) -> str:
    labels = _headings()
    ordered = sorted(events, key=lambda row: (row["occurred_at"], row["event_id"]))
    project = ordered[0]["project_name"] if ordered else "project"
    progress = [
        "- %s %s" % (_outcome_summary(event), _event_citation(event)) for event in ordered
    ]
    verified = [
        "- %s %s" % (_outcome_summary(event), _event_citation(event))
        for event in ordered
        if event["verification_status"] == "passed"
    ]
    blocker_groups: Dict[str, List[Mapping[str, Any]]] = {}
    for event in ordered:
        if event.get("blocker"):
            blocker_groups.setdefault(str(event["blocker"]), []).append(event)
    recurring = [
        "- %s (%s) %s"
        % (
            text,
            labels["repeated"] % len(group),
            " ".join(_event_citation(event) for event in group),
        )
        for text, group in sorted(blocker_groups.items())
        if len(group) >= 2
    ]
    decisions = [
        "- %s %s" % (event["decision"], _event_citation(event))
        for event in ordered
        if event.get("decision")
    ]
    next_actions = [
        "- %s %s" % (event["next_action"], _event_citation(event))
        for event in ordered
        if event.get("next_action")
    ]

    metric_groups: Dict[Tuple[str, str, str, str, str], List[Tuple[Any, Mapping[str, Any]]]] = {}
    for event in ordered:
        for metric in event.get("metrics") or []:
            metric_groups.setdefault(_metric_signature(metric), []).append((metric["value"], event))
    trends = []
    for signature, points in sorted(metric_groups.items()):
        if len(points) < 2:
            continue
        name, unit, dataset, split, conditions = signature
        # Missing comparison context is not evidence that two measurements were
        # made under the same conditions. Keep those as separate event results
        # in the progress section instead of presenting a trend.
        if not all((name, unit, dataset, split, conditions)):
            continue
        context = ", ".join(
            item
            for item in (
                ("unit=%s" % unit) if unit else "",
                ("dataset=%s" % dataset) if dataset else "",
                ("split=%s" % split) if split else "",
                ("conditions=%s" % conditions) if conditions else "",
            )
            if item
        )
        values = " → ".join(str(value) for value, _event in points)
        citations = " ".join(_event_citation(event) for _value, event in points)
        trends.append("- %s%s: %s %s" % (name, (" (%s)" % context) if context else "", values, citations))

    sections: List[str] = [
        "# Weekly Work Journal — %s" % period,
        "",
        "## %s" % labels["weekly_by_project"],
        "",
        "### %s" % project,
        "",
        *_empty_or_lines(progress),
        "",
        "## %s" % labels["weekly_verified"],
        "",
        *_empty_or_lines(verified),
        "",
        "## %s" % labels["recurring_blockers"],
        "",
        *_empty_or_lines(recurring),
        "",
        "## %s" % labels["weekly_decisions"],
        "",
        *_empty_or_lines(decisions),
        "",
        "## %s" % labels["trends"],
        "",
        *_empty_or_lines(trends),
        "",
        "## %s" % labels["weekly_next"],
        "",
        *_empty_or_lines(next_actions),
        "",
    ]
    return "\n".join(sections)


def optional_enrichment(
    template: str, enricher: Optional[Callable[[str, str], str]]
) -> str:
    """Return template on any optional enrichment failure.

    No lifecycle currently supplies an enricher; this narrow boundary exists so
    an existing safe prose service could be attached later without changing the
    canonical renderer or its failure semantics.
    """

    if enricher is None:
        return template
    content_hash = _sha256_bytes(template.encode("utf-8"))
    try:
        enriched = enricher(template, content_hash)
    except Exception:
        return template
    return enriched if isinstance(enriched, str) and enriched.strip() else template


class JournalStore:
    def __init__(
        self,
        repo: Union[os.PathLike, str],
        *,
        timezone_name: Optional[str] = None,
        clock: Optional[Callable[[], dt.datetime]] = None,
        project_id: Optional[str] = None,
        project_name: Optional[str] = None,
    ) -> None:
        self.repo = pathlib.Path(repo).resolve()
        self.root = self.repo / ".oms" / "work-journal"
        self.events_path = self.root / "events.jsonl"
        self.project_path = self.root / "project.json"
        self.daily_dir = self.root / "daily"
        self.weekly_dir = self.root / "weekly"
        self.sync_dir = self.root / "sync"
        self.index_path = self.root / "index.json"
        self.index_db_path = self.root / "index.sqlite3"
        self.quarantine_path = self.root / "quarantine" / "index.json"
        self.notion_state_path = self.sync_dir / "notion.json"
        self.digest_state_path = self.root / "digest.json"
        self.distill_state_path = self.root / "distill.json"
        self.clock = clock or (lambda: dt.datetime.now(dt.timezone.utc))
        self.timezone_name, self.timezone_info = resolve_timezone(timezone_name)
        detected_id, detected_name = project_identity(self.repo)
        existing_id, existing_name = self._existing_project_identity()
        self.project_id = project_id or existing_id or detected_id
        self.project_name = project_name or existing_name or detected_name

    def _existing_project_identity(self) -> Tuple[Optional[str], Optional[str]]:
        if self.project_path.is_file():
            try:
                row = json.loads(self.project_path.read_text(encoding="utf-8"))
                if (
                    row.get("schema_version") == 1
                    and isinstance(row.get("project_id"), str)
                    and isinstance(row.get("project_name"), str)
                ):
                    return row["project_id"], row["project_name"]
            except (OSError, ValueError, TypeError):
                pass
        if self.events_path.is_file():
            try:
                records = self.events_path.read_bytes().splitlines()
            except OSError:
                records = []
            for raw in records:
                if not raw.strip():
                    continue
                try:
                    row = json.loads(raw.decode("utf-8"))
                    validate_canonical_event(row)
                    if isinstance(row.get("project_id"), str) and isinstance(
                        row.get("project_name"), str
                    ):
                        return row["project_id"], row["project_name"]
                except (SchemaError, UnicodeError, ValueError, TypeError):
                    continue
        return None, None

    def _ensure_layout(self) -> None:
        oms_dir = self.repo / ".oms"
        oms_dir.mkdir(parents=True, exist_ok=True)
        ignore = oms_dir / ".gitignore"
        if not ignore.exists():
            atomic_write_text(ignore, "*\n")
        self.root.mkdir(parents=True, exist_ok=True)
        if not self.project_path.exists():
            atomic_write_json(
                self.project_path,
                {
                    "schema_version": 1,
                    "project_id": self.project_id,
                    "project_name": self.project_name,
                },
            )

    def _events_fingerprint(self) -> Tuple[int, int]:
        try:
            stat = self.events_path.stat()
        except OSError:
            return 0, 0
        return int(stat.st_size), int(stat.st_mtime_ns)

    @staticmethod
    def _create_index_schema(connection: sqlite3.Connection) -> None:
        connection.executescript(
            """
            CREATE TABLE IF NOT EXISTS metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS events (
                event_id TEXT PRIMARY KEY,
                occurred_at TEXT NOT NULL,
                local_date TEXT NOT NULL,
                iso_week TEXT NOT NULL,
                event_type TEXT NOT NULL,
                source_type TEXT NOT NULL,
                source_id TEXT NOT NULL,
                supersedes_event_id TEXT,
                active INTEGER NOT NULL,
                payload TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS events_day_active
                ON events(local_date, active, occurred_at, event_id);
            CREATE INDEX IF NOT EXISTS events_week_active
                ON events(iso_week, active, occurred_at, event_id);
            CREATE TABLE IF NOT EXISTS dirty_periods (
                kind TEXT NOT NULL,
                period TEXT NOT NULL,
                PRIMARY KEY(kind, period)
            );
            CREATE TABLE IF NOT EXISTS summaries (
                kind TEXT NOT NULL,
                period TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                content TEXT NOT NULL,
                has_blocker INTEGER NOT NULL,
                PRIMARY KEY(kind, period)
            );
            """
        )

    @staticmethod
    def _metadata(connection: sqlite3.Connection) -> Dict[str, str]:
        return {
            str(key): str(value)
            for key, value in connection.execute("SELECT key, value FROM metadata")
        }

    @staticmethod
    def _set_metadata(
        connection: sqlite3.Connection, values: Mapping[str, Any]
    ) -> None:
        connection.executemany(
            "INSERT OR REPLACE INTO metadata(key, value) VALUES (?, ?)",
            [(str(key), str(value)) for key, value in values.items()],
        )

    @staticmethod
    def _mark_period(
        connection: sqlite3.Connection, kind: str, period: str
    ) -> None:
        connection.execute(
            "INSERT OR IGNORE INTO dirty_periods(kind, period) VALUES (?, ?)",
            (kind, period),
        )

    def _rebuild_index_database(
        self, connection: sqlite3.Connection, events: Sequence[Mapping[str, Any]]
    ) -> None:
        connection.executescript(
            """
            DROP TABLE IF EXISTS dirty_periods;
            DROP TABLE IF EXISTS summaries;
            DROP TABLE IF EXISTS events;
            DROP TABLE IF EXISTS metadata;
            """
        )
        self._create_index_schema(connection)
        superseded = {
            str(event["supersedes_event_id"])
            for event in events
            if event.get("supersedes_event_id")
        }
        for event in events:
            source = event.get("source") or {}
            connection.execute(
                """
                INSERT INTO events(
                    event_id, occurred_at, local_date, iso_week, event_type,
                    source_type, source_id, supersedes_event_id, active, payload
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    event["event_id"],
                    event["occurred_at"],
                    event["local_date"],
                    event["iso_week"],
                    event["event_type"],
                    source.get("type", ""),
                    source.get("id", ""),
                    event.get("supersedes_event_id"),
                    0 if event["event_id"] in superseded else 1,
                    _canonical_bytes(event).decode("utf-8"),
                ),
            )
            self._mark_period(connection, "daily", str(event["local_date"]))
            self._mark_period(connection, "weekly", str(event["iso_week"]))
        size, modified = self._events_fingerprint()
        self._set_metadata(
            connection,
            {
                "schema_version": INDEX_SCHEMA_VERSION,
                "events_size": size,
                "events_mtime_ns": modified,
            },
        )

    def _ensure_index_database(self, *, rebuild: bool = False) -> None:
        self._ensure_layout()
        expected_size, expected_modified = self._events_fingerprint()
        valid = False
        if not rebuild and self.index_db_path.is_file():
            try:
                with sqlite3.connect(str(self.index_db_path)) as connection:
                    self._create_index_schema(connection)
                    metadata = self._metadata(connection)
                    valid = (
                        metadata.get("schema_version") == str(INDEX_SCHEMA_VERSION)
                        and metadata.get("events_size") == str(expected_size)
                        and metadata.get("events_mtime_ns")
                        == str(expected_modified)
                    )
            except sqlite3.DatabaseError:
                valid = False
        if valid:
            return

        events = self.load_events()
        try:
            with sqlite3.connect(str(self.index_db_path)) as connection:
                self._rebuild_index_database(connection, events)
        except sqlite3.DatabaseError:
            try:
                self.index_db_path.unlink()
            except OSError:
                pass
            with sqlite3.connect(str(self.index_db_path)) as connection:
                self._rebuild_index_database(connection, events)

    @staticmethod
    def _decode_indexed_event(payload: str) -> Dict[str, Any]:
        event = json.loads(payload)
        validate_canonical_event(event)
        return event

    def _indexed_event(self, event_id: str) -> Optional[Dict[str, Any]]:
        self._ensure_index_database()
        with sqlite3.connect(str(self.index_db_path)) as connection:
            row = connection.execute(
                "SELECT payload FROM events WHERE event_id = ?", (event_id,)
            ).fetchone()
        if row is None:
            return None
        return self._decode_indexed_event(str(row[0]))

    def _write_event(self, event: Mapping[str, Any]) -> bool:
        self._ensure_layout()
        self._ensure_index_database()
        with sqlite3.connect(str(self.index_db_path)) as connection:
            if connection.execute(
                "SELECT 1 FROM events WHERE event_id = ?", (event["event_id"],)
            ).fetchone():
                return False
        encoded = _canonical_bytes(event) + b"\n"
        with open(self.events_path, "a+b") as handle:
            if handle.tell() > 0:
                handle.seek(-1, os.SEEK_END)
                if handle.read(1) != b"\n":
                    handle.seek(0, os.SEEK_END)
                    handle.write(b"\n")
            handle.seek(0, os.SEEK_END)
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        size, modified = self._events_fingerprint()
        source = event.get("source") or {}
        try:
            with sqlite3.connect(str(self.index_db_path)) as connection:
                supersedes = event.get("supersedes_event_id")
                if supersedes:
                    prior = connection.execute(
                        """
                        SELECT local_date, iso_week
                        FROM events
                        WHERE event_id = ?
                        """,
                        (supersedes,),
                    ).fetchone()
                    connection.execute(
                        "UPDATE events SET active = 0 WHERE event_id = ?",
                        (supersedes,),
                    )
                    if prior is not None:
                        self._mark_period(connection, "daily", str(prior[0]))
                        self._mark_period(connection, "weekly", str(prior[1]))
                connection.execute(
                    """
                    INSERT INTO events(
                        event_id, occurred_at, local_date, iso_week, event_type,
                        source_type, source_id, supersedes_event_id, active, payload
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?)
                    """,
                    (
                        event["event_id"],
                        event["occurred_at"],
                        event["local_date"],
                        event["iso_week"],
                        event["event_type"],
                        source.get("type", ""),
                        source.get("id", ""),
                        supersedes,
                        _canonical_bytes(event).decode("utf-8"),
                    ),
                )
                self._mark_period(connection, "daily", str(event["local_date"]))
                self._mark_period(connection, "weekly", str(event["iso_week"]))
                self._set_metadata(
                    connection,
                    {
                        "schema_version": INDEX_SCHEMA_VERSION,
                        "events_size": size,
                        "events_mtime_ns": modified,
                    },
                )
        except sqlite3.DatabaseError:
            # The canonical append already succeeded. Leave recovery to the
            # next call, which detects the stale/corrupt derived fingerprint.
            raise
        return True

    def record_event(
        self, payload: Mapping[str, Any], *, recorded_at: Optional[dt.datetime] = None
    ) -> Tuple[Dict[str, Any], bool]:
        now = recorded_at or self.clock()
        if now.tzinfo is None:
            now = now.replace(tzinfo=dt.timezone.utc)
        event = normalize_event(
            payload,
            project_id=self.project_id,
            project_name=self.project_name,
            timezone_name=self.timezone_name,
            timezone_info=self.timezone_info,
            recorded_at=now,
        )
        created = self._write_event(event)
        if not created:
            indexed = self._indexed_event(str(event["event_id"]))
            if indexed is None:
                raise JournalError("indexed duplicate is unavailable")
            event = indexed
        return event, created

    def capture_head_commit(self) -> bool:
        """Observe the current Git commit without scanning worktree state."""

        try:
            raw = subprocess.check_output(
                [
                    "git",
                    "-C",
                    str(self.repo),
                    "show",
                    "-s",
                    "--format=%H%x00%cI%x00%s",
                    "HEAD",
                ],
                stderr=subprocess.DEVNULL,
            ).decode("utf-8", errors="replace").strip()
        except (OSError, subprocess.CalledProcessError):
            return False
        fields = raw.split("\0", 2)
        if len(fields) != 3 or not fields[0] or not fields[1]:
            return False
        commit_sha, committed_at, subject = fields
        payload = {
            "event_type": "commit",
            "occurred_at": _utc_rfc3339(parse_rfc3339(committed_at)),
            "source": {"type": "git-commit", "id": commit_sha},
            "outcome": {
                "summary": "Commit %s: %s"
                % (commit_sha[:12], sanitize_text(subject, 500)),
                "status": "recorded",
            },
            "verification_status": "not_verified",
            "refs": [{"type": "commit", "id": commit_sha}],
            "evidence": [{"type": "git-commit", "ref": commit_sha}],
            "provenance": {"code_commit": commit_sha},
        }
        _event, created = self.record_event(payload)
        return created

    def load_events(self) -> List[Dict[str, Any]]:
        if not self.events_path.is_file():
            return []
        events: List[Dict[str, Any]] = []
        quarantine = []
        try:
            records = self.events_path.read_bytes().splitlines()
        except OSError:
            records = []
        for line_number, raw in enumerate(records, 1):
            if not raw.strip():
                continue
            try:
                event = json.loads(raw.decode("utf-8"))
                validate_canonical_event(event)
                events.append(event)
            except SchemaError as exc:
                reason = (
                    "unsupported_schema"
                    if "unsupported schema" in str(exc)
                    else "invalid_schema"
                )
                quarantine.append(
                    {
                        "record": line_number,
                        "sha256": _sha256_bytes(raw),
                        "reason": reason,
                    }
                )
            except (OSError, UnicodeError, ValueError, TypeError):
                quarantine.append(
                    {
                        "record": line_number,
                        "sha256": _sha256_bytes(raw),
                        "reason": "malformed",
                    }
                )
        if quarantine:
            atomic_write_json(
                self.quarantine_path,
                {"schema_version": 1, "entries": quarantine},
            )
        elif self.quarantine_path.exists():
            atomic_write_json(
                self.quarantine_path,
                {"schema_version": 1, "entries": []},
            )
        return sorted(events, key=lambda row: (row["occurred_at"], row["event_id"]))

    @staticmethod
    def active_events(events: Sequence[Mapping[str, Any]]) -> List[Dict[str, Any]]:
        superseded = {
            event["supersedes_event_id"]
            for event in events
            if event.get("supersedes_event_id")
        }
        return [dict(event) for event in events if event["event_id"] not in superseded]

    def _remove_stale_views(
        self, directory: pathlib.Path, expected_names: Sequence[str]
    ) -> None:
        if not directory.is_dir():
            return
        expected = set(expected_names)
        for path in directory.glob("*.md"):
            if path.name not in expected:
                path.unlink()

    def _indexed_period_events(
        self, connection: sqlite3.Connection, kind: str, period: str
    ) -> List[Dict[str, Any]]:
        column = "local_date" if kind == "daily" else "iso_week"
        rows = connection.execute(
            """
            SELECT payload
            FROM events
            WHERE active = 1 AND %s = ?
            ORDER BY occurred_at, event_id
            """
            % column,
            (period,),
        )
        return [self._decode_indexed_event(str(row[0])) for row in rows]

    def _index_summary(self, connection: sqlite3.Connection) -> Dict[str, Any]:
        event_count = int(connection.execute("SELECT COUNT(*) FROM events").fetchone()[0])
        active_event_count = int(
            connection.execute(
                "SELECT COUNT(*) FROM events WHERE active = 1"
            ).fetchone()[0]
        )
        daily = [
            str(row[0])
            for row in connection.execute(
                """
                SELECT DISTINCT local_date
                FROM events
                WHERE active = 1
                ORDER BY local_date
                """
            )
        ]
        weekly = [
            str(row[0])
            for row in connection.execute(
                """
                SELECT DISTINCT iso_week
                FROM events
                WHERE active = 1
                ORDER BY iso_week
                """
            )
        ]
        recent_desc = [
            self._decode_indexed_event(str(row[0]))
            for row in connection.execute(
                """
                SELECT payload
                FROM events
                WHERE active = 1
                ORDER BY occurred_at DESC, event_id DESC
                LIMIT ?
                """,
                (INDEX_RECENT_EVENT_LIMIT,),
            )
        ]
        recent = list(reversed(recent_desc))
        return {
            "schema_version": SCHEMA_VERSION,
            "renderer_version": RENDERER_VERSION,
            "project_id": self.project_id,
            "event_count": event_count,
            "active_event_count": active_event_count,
            "events": [
                {
                    "event_id": event["event_id"],
                    "occurred_at": event["occurred_at"],
                    "local_date": event["local_date"],
                    "iso_week": event["iso_week"],
                    "event_type": event["event_type"],
                    "source": event["source"],
                }
                for event in recent
            ],
            "events_truncated": active_event_count > len(recent),
            "daily": daily,
            "weekly": weekly,
        }

    def _render_all(
        self, events: Optional[Sequence[Mapping[str, Any]]] = None
    ) -> Dict[str, Any]:
        if events is not None:
            self._ensure_layout()
            try:
                with sqlite3.connect(str(self.index_db_path)) as connection:
                    self._rebuild_index_database(connection, events)
            except sqlite3.DatabaseError:
                try:
                    self.index_db_path.unlink()
                except OSError:
                    pass
                with sqlite3.connect(str(self.index_db_path)) as connection:
                    self._rebuild_index_database(connection, events)
        else:
            self._ensure_index_database()

        self.daily_dir.mkdir(parents=True, exist_ok=True)
        self.weekly_dir.mkdir(parents=True, exist_ok=True)
        with sqlite3.connect(str(self.index_db_path)) as connection:
            dirty = [
                (str(kind), str(period))
                for kind, period in connection.execute(
                    "SELECT kind, period FROM dirty_periods ORDER BY kind, period"
                )
            ]
            for kind, period in dirty:
                rows = self._indexed_period_events(connection, kind, period)
                directory = self.daily_dir if kind == "daily" else self.weekly_dir
                target = directory / ("%s.md" % period)
                if not rows:
                    try:
                        target.unlink()
                    except FileNotFoundError:
                        pass
                    connection.execute(
                        "DELETE FROM summaries WHERE kind = ? AND period = ?",
                        (kind, period),
                    )
                    continue
                rendered = (
                    render_daily(period, rows)
                    if kind == "daily"
                    else render_weekly(period, rows)
                )
                atomic_write_text(target, rendered)
                export_content = sanitize_multiline(rendered, MAX_EXPORT_BYTES)
                connection.execute(
                    """
                    INSERT OR REPLACE INTO summaries(
                        kind, period, content_hash, content, has_blocker
                    ) VALUES (?, ?, ?, ?, ?)
                    """,
                    (
                        kind,
                        period,
                        _sha256_bytes(export_content.encode("utf-8")),
                        export_content,
                        1 if self._summary_has_blocker(export_content) else 0,
                    ),
                )

            index = self._index_summary(connection)
            atomic_write_json(self.index_path, index)
            connection.execute("DELETE FROM dirty_periods")

        if events is not None:
            self._remove_stale_views(
                self.daily_dir, ["%s.md" % period for period in index["daily"]]
            )
            self._remove_stale_views(
                self.weekly_dir, ["%s.md" % period for period in index["weekly"]]
            )
        return index

    def materialize(self) -> Dict[str, Any]:
        self._ensure_layout()
        return self._render_all()

    def rebuild(self) -> Dict[str, Any]:
        return self._render_all(self.load_events())

    def _distill_marker(self) -> Optional[Tuple[str, str]]:
        if not self.distill_state_path.is_file():
            return None
        try:
            row = json.loads(self.distill_state_path.read_text(encoding="utf-8"))
            through = row.get("through") if isinstance(row, Mapping) else None
            if (
                row.get("schema_version") == 1
                and isinstance(through, Mapping)
                and isinstance(through.get("occurred_at"), str)
                and isinstance(through.get("event_id"), str)
            ):
                return str(through["occurred_at"]), str(through["event_id"])
        except (OSError, UnicodeError, ValueError, TypeError):
            pass
        return None

    @staticmethod
    def _distill_normalized_text(text: str) -> str:
        return re.sub(r"\s+", " ", text).strip().casefold()

    def _shared_memory_text(self) -> str:
        """Normalized project shared memory, for containment dedup.

        Absent or unreadable shared memory means nothing is known to be
        promoted already, so dedup skips nothing instead of blocking the run.
        """

        path = self.repo / ".oms" / "memory" / "shared.md"
        try:
            return self._distill_normalized_text(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError):
            return ""

    def _distill_candidates(
        self, events: Sequence[Mapping[str, Any]], promoted: str = ""
    ) -> Tuple[List[str], int, int]:
        blockers: Dict[str, List[Mapping[str, Any]]] = {}
        candidates: List[Tuple[Tuple[str, str], str, str]] = []
        seen_decisions: set = set()
        for event in events:
            decision = event.get("decision")
            if isinstance(decision, str) and decision:
                # The same decision often re-observes on later days; one lesson
                # per distinct text, or duplicates crowd out the per-run cap.
                decision_key = self._distill_normalized_text(decision)
                if decision_key and decision_key not in seen_decisions:
                    seen_decisions.add(decision_key)
                    text = sanitize_text(decision, 600)
                    candidates.append(
                        (
                            (str(event["occurred_at"]), str(event["event_id"])),
                            "journal-distill: decision %s: %s"
                            % (event["local_date"], text),
                            self._distill_normalized_text(text),
                        )
                    )
            blocker = event.get("blocker")
            if isinstance(blocker, str) and blocker:
                key = self._distill_normalized_text(blocker)
                if key:
                    blockers.setdefault(key, []).append(event)
        for group in blockers.values():
            days = sorted({str(event["local_date"]) for event in group})
            if len(days) < 2:
                continue
            latest = max(
                (str(event["occurred_at"]), str(event["event_id"]))
                for event in group
            )
            text = sanitize_text(str(group[0]["blocker"]), 600)
            candidates.append(
                (
                    latest,
                    "journal-distill: blocker seen %dd %s..%s: %s"
                    % (len(days), days[0], days[-1], text),
                    self._distill_normalized_text(text),
                )
            )
        ordered = sorted(candidates, key=lambda candidate: candidate[0])
        fresh: List[str] = []
        deduped = 0
        for _position, lesson, needle in ordered:
            # Decisions come only from agent-task records, whose close path
            # already appends them to shared memory. Without this containment
            # check the first automatic run would duplicate an existing line in
            # the bounded memory context every peer call carries. Dedup runs
            # ahead of the cap so redundant text cannot consume a promotion slot.
            if promoted and needle and needle in promoted:
                deduped += 1
                continue
            fresh.append(lesson)
        # No silent caps: the marker advances past everything below, so a
        # candidate beyond the cap is dropped for good and must be said aloud.
        return fresh[:5], max(0, len(fresh) - 5), deduped

    def distill(self, *, dry_run: bool = False) -> Tuple[List[str], int, int, int]:
        """Promote new recurring blockers and explicit decisions to shared memory."""

        events = self.active_events(self.load_events())
        marker = self._distill_marker()
        pending = [
            event
            for event in events
            if marker is None
            or (str(event["occurred_at"]), str(event["event_id"])) > marker
        ]
        lessons, dropped, deduped = self._distill_candidates(
            pending, self._shared_memory_text()
        )
        if dry_run:
            return lessons, dropped, 0, deduped
        promoted: List[str] = []
        skipped = 0
        for lesson in lessons:
            # The memory writer applies its own scrubbing and may refuse a
            # lesson (a blocker text can carry anything). One refused lesson
            # must not crash the run or block the others — skip it, say so.
            try:
                subprocess.run(
                    [
                        "bash",
                        str(
                            pathlib.Path(__file__).resolve().parents[1]
                            / "agent-memory.sh"
                        ),
                        "--repo",
                        str(self.repo),
                        "append",
                        "--agent",
                        "journal",
                        "--text",
                        lesson,
                    ],
                    check=True,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
            except (OSError, subprocess.CalledProcessError):
                skipped += 1
                continue
            promoted.append(lesson)
        lessons = promoted
        through = None
        if events:
            latest = max(
                events, key=lambda event: (event["occurred_at"], event["event_id"])
            )
            through = {
                "occurred_at": latest["occurred_at"],
                "event_id": latest["event_id"],
            }
        self._ensure_layout()
        atomic_write_json(
            self.distill_state_path,
            {
                "schema_version": 1,
                "through": through,
                "last_run_date": self._current_periods()[0],
            },
        )
        return lessons, dropped, skipped, deduped

    def distill_due_today(self) -> bool:
        """True when no distill has completed in the current local day.

        The distill owns this day marker instead of reading the digest's: the
        digest advances its own marker only when it actually emits, and
        OMS_WORK_JOURNAL_DIGEST=0 drops the digest from the tick entirely.
        """

        current_day, _current_week = self._current_periods()
        if not self.distill_state_path.is_file():
            return True
        try:
            row = json.loads(self.distill_state_path.read_text(encoding="utf-8"))
            if (
                isinstance(row, Mapping)
                and row.get("schema_version") == 1
                and row.get("last_run_date") == current_day
            ):
                return False
        except (OSError, UnicodeError, ValueError, TypeError):
            pass
        return True

    def status(self) -> Dict[str, Any]:
        self._ensure_index_database()
        size, _modified = self._events_fingerprint()
        with sqlite3.connect(str(self.index_db_path)) as connection:
            summary = self._index_summary(connection)
            dirty_count = int(
                connection.execute("SELECT COUNT(*) FROM dirty_periods").fetchone()[0]
            )
        settings = notion_settings()
        sync_counts = {"synced": 0, "pending": 0, "failed": 0}
        if self.notion_state_path.is_file():
            try:
                sync_state = json.loads(
                    self.notion_state_path.read_text(encoding="utf-8")
                )
                summaries = sync_state.get("summaries", {})
                if isinstance(summaries, Mapping):
                    for value in summaries.values():
                        if not isinstance(value, Mapping):
                            continue
                        state = str(value.get("status") or "")
                        if state in sync_counts:
                            sync_counts[state] += 1
            except (OSError, UnicodeError, ValueError, TypeError):
                sync_counts["failed"] += 1
        detected_id, detected_name = project_identity(self.repo)
        return {
            "repo": str(self.repo),
            "enabled": True,
            "project": {
                "id": self.project_id,
                "name": self.project_name,
                "detected_id": detected_id,
                "detected_name": detected_name,
                "drift": self.project_id != detected_id
                or self.project_name != detected_name,
            },
            "event_count": summary["event_count"],
            "active_event_count": summary["active_event_count"],
            "events_bytes": size,
            "daily_count": len(summary["daily"]),
            "weekly_count": len(summary["weekly"]),
            "dirty_period_count": dirty_count,
            "index_ready": True,
            "notion": {
                "configured": bool(
                    settings["data_source_id"] or settings["database_id"]
                ),
                "excluded": notion_repo_excluded(self.repo),
                "credential_present": notion_auth_available(settings),
                "auth_mode": settings.get("auth_mode") or "",
                "target_kind": "data_source"
                if settings["data_source_id"]
                else ("database" if settings["database_id"] else ""),
                "data_source_id": settings["data_source_id"],
                "sync_mode": "finalized",
                **sync_counts,
            },
        }

    def _current_periods(self) -> Tuple[str, str]:
        now = self.clock()
        if now.tzinfo is None:
            now = now.replace(tzinfo=dt.timezone.utc)
        local = now.astimezone(self.timezone_info)
        calendar = local.isocalendar()
        return local.date().isoformat(), "%04d-W%02d" % (calendar[0], calendar[1])

    @staticmethod
    def _summary_has_blocker(content: str) -> bool:
        in_blockers = False
        for raw_line in content.splitlines():
            line = raw_line.strip()
            if line.startswith("## "):
                heading = line[3:].strip().lower()
                in_blockers = "blocker" in heading
                continue
            if in_blockers and line.startswith("- "):
                return line not in _EMPTY_MARKERS
        return False

    def _summary_rows(
        self, *, include_open: bool = False, include_content: bool = True
    ) -> List[Dict[str, Any]]:
        rows = []
        current_day, current_week = self._current_periods()
        self._ensure_index_database()
        selected = (
            "kind, period, content_hash, content, has_blocker"
            if include_content
            else "kind, period, content_hash, '', has_blocker"
        )
        with sqlite3.connect(str(self.index_db_path)) as connection:
            indexed = list(
                connection.execute(
                    "SELECT %s FROM summaries ORDER BY kind, period" % selected
                )
            )
        for kind_value, period_value, content_hash, content, has_blocker in indexed:
            kind = str(kind_value)
            period = str(period_value)
            if not include_open and (
                (kind == "daily" and period >= current_day)
                or (kind == "weekly" and period >= current_week)
            ):
                continue
            rows.append(
                {
                    "kind": kind,
                    "period": period,
                    "summary_key": "%s:%s:%s" % (self.project_id, kind, period),
                    "title": "%s Work Journal — %s" % (kind.title(), period),
                    "content": str(content),
                    "content_hash": str(content_hash),
                    "project_name": self.project_name,
                    "has_blocker": bool(has_blocker),
                }
            )
        return rows

    def _summary_content(self, kind: str, period: str) -> str:
        self._ensure_index_database()
        with sqlite3.connect(str(self.index_db_path)) as connection:
            row = connection.execute(
                "SELECT content FROM summaries WHERE kind = ? AND period = ?",
                (kind, period),
            ).fetchone()
        if row is None:
            raise JournalError("indexed Work Journal summary is unavailable")
        return str(row[0])

    def _period_aggregates(self, kind: str, period: str) -> Dict[str, Any]:
        """Mirror-facing counts for one daily period: sessions, commits,
        verified. Weekly pages keep their existing properties (a week of
        session ids is a listing, not a column)."""

        if kind != "daily":
            return {}
        self._ensure_index_database()
        with sqlite3.connect(str(self.index_db_path)) as connection:
            rows = connection.execute(
                "SELECT payload FROM events WHERE active = 1 AND local_date = ?"
                " ORDER BY occurred_at, event_id",
                (period,),
            ).fetchall()
        sessions: List[str] = []
        commits = 0
        verified = 0
        for raw in rows:
            event = self._decode_indexed_event(str(raw[0]))
            sid = str(
                (event.get("correlation") or {}).get("session_id") or ""
            ).strip()
            if sid and sid not in sessions:
                sessions.append(sid)
            if event.get("event_type") == "commit":
                commits += 1
            if event.get("verification_status") == "passed":
                verified += 1
        return {"sessions": sessions, "commits": commits, "verified": verified}

    def summary_text(self, kind: str, period: str) -> Optional[str]:
        """Indexed summary content, or None when the period has no events."""

        self._ensure_index_database()
        with sqlite3.connect(str(self.index_db_path)) as connection:
            row = connection.execute(
                "SELECT content FROM summaries WHERE kind = ? AND period = ?",
                (kind, period),
            ).fetchone()
        return None if row is None else str(row[0])

    def recent_events(self, limit: int = 20) -> List[Dict[str, Any]]:
        """Most recent active events, newest first."""

        bounded = max(1, min(int(limit), INDEX_RECENT_EVENT_LIMIT))
        self._ensure_index_database()
        with sqlite3.connect(str(self.index_db_path)) as connection:
            rows = connection.execute(
                """
                SELECT payload
                FROM events
                WHERE active = 1
                ORDER BY occurred_at DESC, event_id DESC
                LIMIT ?
                """,
                (bounded,),
            ).fetchall()
        return [self._decode_indexed_event(str(row[0])) for row in rows]

    def open_annotations(
        self, *, days: int = ANNOTATION_WINDOW_DAYS
    ) -> Dict[str, Any]:
        """Deduplicated blocker/next-action annotations from recent events.

        Annotations have no resolution record, so "open" means observed within
        the window and not superseded — newest occurrence first.
        """

        current_day, _current_week = self._current_periods()
        since = (
            dt.date.fromisoformat(current_day)
            - dt.timedelta(days=max(0, int(days) - 1))
        ).isoformat()
        self._ensure_index_database()
        with sqlite3.connect(str(self.index_db_path)) as connection:
            rows = connection.execute(
                """
                SELECT payload
                FROM events
                WHERE active = 1 AND local_date >= ?
                ORDER BY occurred_at DESC, event_id DESC
                """,
                (since,),
            ).fetchall()
        events = [self._decode_indexed_event(str(row[0])) for row in rows]
        collected: Dict[str, List[Dict[str, Any]]] = {
            "blockers": [],
            "next_actions": [],
        }
        seen: Dict[str, set] = {"blockers": set(), "next_actions": set()}
        for event in events:
            for field, bucket in (("blocker", "blockers"), ("next_action", "next_actions")):
                value = event.get(field)
                if not value or value in seen[bucket]:
                    continue
                seen[bucket].add(value)
                collected[bucket].append(
                    {
                        "text": str(value),
                        "event_id": event["event_id"],
                        "occurred_at": event["occurred_at"],
                        "local_date": event["local_date"],
                    }
                )
        return {"since": since, **collected}

    def _newest_handoff_pointer(self) -> Optional[str]:
        """One line naming the newest recent handoff digest, or None.

        Handoffs are captured manually and loaded manually; the digest is the
        moment a new session would want to know one exists. A pointer only —
        the digest never inlines another artifact's content.
        """

        handoffs = self.repo / ".oms" / "handoffs"
        newest: Optional[pathlib.Path] = None
        newest_mtime = 0.0
        try:
            for path in handoffs.iterdir():
                if path.suffix != ".md" or not path.is_file():
                    continue
                mtime = path.stat().st_mtime
                if mtime > newest_mtime:
                    newest, newest_mtime = path, mtime
        except OSError:
            return None
        if newest is None:
            return None
        now = self.clock()
        if now.tzinfo is None:
            now = now.replace(tzinfo=dt.timezone.utc)
        age = now.timestamp() - newest_mtime
        if age < 0 or age > HANDOFF_POINTER_MAX_AGE_SECONDS:
            return None
        return "Newest handoff (%dh old): oms session-handoff show %s" % (
            max(0, int(age // 3600)),
            sanitize_text(newest.name, 200),
        )

    def prompt_digest(self) -> str:
        """Bounded once-per-local-day catch-up block for prompt-hook stdout.

        Returns an empty string when the digest already fired today or there
        is nothing worth surfacing; the marker is derived state and is only
        advanced when a digest is actually emitted.
        """

        current_day, _current_week = self._current_periods()
        if self.digest_state_path.is_file():
            try:
                marker = json.loads(
                    self.digest_state_path.read_text(encoding="utf-8")
                )
                if (
                    marker.get("schema_version") == DIGEST_SCHEMA_VERSION
                    and marker.get("last_date") == current_day
                ):
                    return ""
            except (OSError, UnicodeError, ValueError, TypeError):
                pass
        recent = self.recent_events(limit=INDEX_RECENT_EVENT_LIMIT)
        if not recent:
            return ""
        annotations = self.open_annotations()
        previous_days = sorted(
            {
                event["local_date"]
                for event in recent
                if event["local_date"] < current_day
            }
        )
        if (
            not annotations["blockers"]
            and not annotations["next_actions"]
            and not previous_days
        ):
            return ""
        lines = ["[work-journal] %s — %s" % (self.project_name, current_day)]
        if annotations["blockers"]:
            lines.append("Open blockers (last %dd):" % ANNOTATION_WINDOW_DAYS)
            lines.extend(
                "- %s [%s]" % (row["text"], row["event_id"])
                for row in annotations["blockers"][:DIGEST_MAX_ITEMS]
            )
        if annotations["next_actions"]:
            lines.append("Next priorities (last %dd):" % ANNOTATION_WINDOW_DAYS)
            lines.extend(
                "- %s [%s]" % (row["text"], row["event_id"])
                for row in annotations["next_actions"][:DIGEST_MAX_ITEMS]
            )
        if previous_days:
            last_day = previous_days[-1]
            day_events = [
                event for event in recent if event["local_date"] == last_day
            ]
            verified = sum(
                1
                for event in day_events
                if event["verification_status"] == "passed"
            )
            lines.append(
                "Last journal day %s: %d events, %d verified."
                % (last_day, len(day_events), verified)
            )
        handoff_pointer = self._newest_handoff_pointer()
        if handoff_pointer:
            lines.append(handoff_pointer)
        lines.append(
            "Details: oms journal show --today | --blockers | --recent 20"
        )
        atomic_write_json(
            self.digest_state_path,
            {"schema_version": DIGEST_SCHEMA_VERSION, "last_date": current_day},
        )
        return "\n".join(lines)

    def sync_notion(self, *, force: bool = False, today_only: bool = False) -> None:
        # Checked before auth so an excluded repo never spends a credential
        # lookup, and the exclusion is testable without one.
        if notion_repo_excluded(self.repo):
            return
        settings = notion_settings()
        if not notion_auth_available(settings) or not (
            settings["data_source_id"] or settings["database_id"]
        ):
            return
        target_fingerprint = _sha256_bytes(
            _canonical_bytes(
                {
                    "kind": "data_source"
                    if settings["data_source_id"]
                    else "database",
                    "id": settings["data_source_id"] or settings["database_id"],
                    "properties": {
                        key: settings[key]
                        for key in (
                            "title_property",
                            "key_property",
                            "hash_property",
                            "project_property",
                            "kind_property",
                            "period_property",
                            "blocker_property",
                            "sessions_property",
                            "commits_property",
                            "verified_property",
                        )
                    },
                }
            )
        )
        try:
            from notion_journal import NotionJournalExporter
        except ImportError:
            # Direct path execution does not necessarily place this directory
            # on sys.path when imported by a unit test.
            sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
            from notion_journal import NotionJournalExporter

        state: Dict[str, Any] = {
            "schema_version": 1,
            "target_fingerprint": target_fingerprint,
            "summaries": {},
        }
        if self.notion_state_path.is_file():
            try:
                loaded = json.loads(self.notion_state_path.read_text(encoding="utf-8"))
                if (
                    loaded.get("schema_version") == 1
                    and loaded.get("target_fingerprint") == target_fingerprint
                    and isinstance(loaded.get("summaries"), dict)
                ):
                    state = loaded
            except (OSError, ValueError, TypeError):
                pass
        exporter = NotionJournalExporter.from_config(**settings)
        try:
            maximum = int(os.environ.get("OMS_WORK_JOURNAL_NOTION_MAX_PER_TICK", "4"))
        except ValueError:
            maximum = 4
        maximum = max(1, min(maximum, 50))
        sync_now = self.clock()
        if sync_now.tzinfo is None:
            sync_now = sync_now.replace(tzinfo=dt.timezone.utc)
        sync_now = sync_now.astimezone(dt.timezone.utc)
        attempted = 0
        rows = self._summary_rows(include_open=force, include_content=False)
        if today_only:
            current_day, _current_week = self._current_periods()
            rows = [
                row
                for row in rows
                if row["kind"] == "daily" and row["period"] == current_day
            ]
        for row in rows:
            previous = state["summaries"].get(row["summary_key"], {})
            if (
                previous.get("status") == "synced"
                and previous.get("content_hash") == row["content_hash"]
            ):
                continue
            if previous.get("next_retry_at"):
                try:
                    if parse_rfc3339(str(previous["next_retry_at"])) > sync_now:
                        continue
                except SchemaError:
                    pass
            if attempted >= maximum:
                break
            attempted += 1
            try:
                content = notion_presentation(
                    self._summary_content(row["kind"], row["period"])
                )
                aggregates = self._period_aggregates(row["kind"], row["period"])
                result = exporter.upsert(
                    row["summary_key"],
                    row["title"],
                    row["content_hash"],
                    content,
                    page_id=previous.get("page_id"),
                    previous_content_hash=previous.get("content_hash")
                    if previous.get("status") == "synced"
                    else None,
                    project_name=row["project_name"],
                    kind=row["kind"],
                    period=row["period"],
                    has_blocker=row["has_blocker"],
                    sessions=aggregates.get("sessions"),
                    commits=aggregates.get("commits"),
                    verified=aggregates.get("verified"),
                )
                state["summaries"][row["summary_key"]] = {
                    "status": "synced",
                    "page_id": result.get("page_id") or previous.get("page_id"),
                    "content_hash": row["content_hash"],
                    "period": row["period"],
                    "kind": row["kind"],
                }
            except Exception as exc:
                retry_after = getattr(exc, "retry_after_seconds", None)
                retry_seconds: Optional[float] = None
                if isinstance(retry_after, (int, float)) and not isinstance(
                    retry_after, bool
                ):
                    candidate = float(retry_after)
                    if math.isfinite(candidate) and candidate > 0:
                        retry_seconds = candidate
                failed_state = {
                    "status": "failed",
                    "page_id": None
                    if getattr(exc, "status_code", None) == 404
                    else previous.get("page_id"),
                    "content_hash": previous.get("content_hash"),
                    "pending_content_hash": row["content_hash"],
                    "period": row["period"],
                    "kind": row["kind"],
                    "error": type(exc).__name__,
                }
                if retry_seconds is not None:
                    failed_state["status"] = "pending"
                    failed_state["next_retry_at"] = _utc_rfc3339(
                        sync_now + dt.timedelta(seconds=retry_seconds)
                    )
                state["summaries"][row["summary_key"]] = failed_state
            atomic_write_json(self.notion_state_path, state)


def observe_fail_open(store: JournalStore, payload: Mapping[str, Any]) -> bool:
    try:
        store.record_event(payload)
        store.materialize()
        return True
    except Exception:
        return False


def _command_summary(value: Any) -> Optional[str]:
    if not isinstance(value, list):
        return None
    parts = [sanitize_text(str(part), 160) for part in value[:12]]
    summary = sanitize_text(" ".join(part for part in parts if part), 500)
    return summary or None


def _source_ref(repo: pathlib.Path, source_path: pathlib.Path, source_id: str) -> str:
    relative = _safe_relpath(repo, source_path)
    return ("%s#%s" % (relative, source_id)) if relative else source_id


def _read_json_source(path: pathlib.Path) -> Dict[str, Any]:
    if path.stat().st_size > MAX_SOURCE_BYTES:
        raise JournalError("source record exceeds Work Journal bound")
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise JournalError("source record is not an object")
    return data


def _last_section_value(text: str, section: str) -> Optional[str]:
    inside = False
    values = []
    for line in text.splitlines():
        if line == section:
            inside = True
            continue
        if inside and line.startswith("## "):
            break
        if inside and line.strip():
            clean = line.strip()
            clean = re.sub(
                r"^-\s+\d{4}-\d\d-\d\dT[0-9:]+Z\s+\[[^\]]+\]\s*", "", clean
            )
            if clean.startswith("- "):
                clean = clean[2:]
            clean = clean.strip()
            values.append(clean)
    return values[-1] if values else None


def _task_payload(
    repo: pathlib.Path,
    path: pathlib.Path,
    *,
    operation: str,
    occurred_at: str,
) -> Dict[str, Any]:
    if path.stat().st_size > MAX_SOURCE_BYTES:
        raise JournalError("Agent State exceeds Work Journal bound")
    text = path.read_text(encoding="utf-8", errors="replace")
    metadata = {}
    for line in text.splitlines():
        if line.startswith("## "):
            break
        match = re.match(r"^-\s+([a-z_]+):\s*(.*)$", line)
        if match:
            metadata[match.group(1)] = match.group(2).strip()
    task_id = metadata.get("task_id") or "task-" + _sha256_bytes(
        text.encode("utf-8")
    )[:16]
    goal = _last_section_value(text, "## Goal")
    result = _last_section_value(text, "## Current State")
    decision = _last_section_value(text, "## Decisions")
    blocker = _last_section_value(text, "## Last Failure")
    next_action = _last_section_value(text, "## Next Step")
    verification_note = _last_section_value(text, "## Verification")
    stable = {
        "task_id": task_id,
        "operation": operation,
        "goal": goal,
        "result": result,
        "decision": decision,
        "blocker": blocker,
        "next": next_action,
        "verification": verification_note,
        "status": metadata.get("status"),
    }
    source_id = "%s:%s:%s" % (
        task_id,
        operation,
        _sha256_bytes(_canonical_bytes(stable))[:16],
    )
    if operation == "verify":
        if metadata.get("status") == "verified":
            verification = "passed"
        elif verification_note and "SKIPPED" in verification_note:
            verification = "skipped"
        else:
            verification = "failed"
    elif operation == "close" and metadata.get("status") == "verified":
        verification = "passed"
    else:
        verification = "not_verified"
    summary = result or goal or ("Agent State %s" % operation)
    payload: Dict[str, Any] = {
        "event_type": "task_outcome" if operation == "close" else "agent_state",
        "occurred_at": occurred_at,
        "source": {"type": "agent-task", "id": source_id},
        "correlation": {"task_id": task_id, "operation_id": source_id},
        "outcome": {"summary": summary, "status": metadata.get("status") or operation},
        "verification_status": verification,
        "evidence": [{"type": "agent-state", "ref": task_id}],
    }
    if goal:
        payload["goal"] = goal
    if decision:
        payload["decision"] = decision
    if blocker:
        payload["blocker"] = blocker
    if next_action:
        payload["next_action"] = next_action
    return payload


def source_payload(
    repo: pathlib.Path,
    source_type: str,
    source_path: pathlib.Path,
    *,
    record_path: Optional[pathlib.Path] = None,
    event_type: Optional[str] = None,
    source_id: Optional[str] = None,
    operation_id: Optional[str] = None,
    occurred_at: Optional[str] = None,
    verification_status: Optional[str] = None,
    outcome: Optional[str] = None,
    outcome_status: Optional[str] = None,
    operation: str = "update",
) -> Dict[str, Any]:
    now = _utc_rfc3339(dt.datetime.now(dt.timezone.utc))
    evidence_path = record_path or source_path
    if source_type == "agent-task":
        payload = _task_payload(
            repo, source_path, operation=operation, occurred_at=occurred_at or now
        )
    elif source_type in {
        "handoff",
        "oms-run",
        "patch-admit",
        "patch-land",
        "peer-review",
        "session-handoff",
    }:
        sid = source_id or source_path.stem
        default_types = {
            "handoff": "handoff",
            "session-handoff": "handoff",
            "oms-run": "phase_outcome",
            "patch-admit": "patch_admit",
            "patch-land": "phase_outcome",
            "peer-review": "patch_review",
        }
        payload = {
            "event_type": event_type or default_types[source_type],
            "occurred_at": occurred_at or now,
            "source": {"type": source_type, "id": sid},
            "outcome": {
                "summary": outcome
                or (
                    "Session handoff captured"
                    if source_type in {"handoff", "session-handoff"}
                    else source_type.replace("-", " ")
                ),
                "status": outcome_status or "recorded",
            },
            "verification_status": verification_status
            or (
                "not_applicable"
                if source_type in {"handoff", "session-handoff"}
                else "not_verified"
            ),
            "evidence": [{"type": source_type, "ref": sid}],
        }
        relative = _safe_relpath(repo, evidence_path)
        if relative:
            payload["refs"] = [{"type": source_type, "path": relative, "id": sid}]
    else:
        row = _read_json_source(source_path)
        if source_type == "run-ledger":
            sid = source_id or str(row.get("id") or "")
            if not sid:
                stable = {
                    key: row.get(key)
                    for key in (
                        "git_sha",
                        "dirty_hash",
                        "cmd",
                        "exit",
                        "duration_s",
                        "note",
                        "slurm_job_id",
                    )
                }
                sid = "legacy-" + _sha256_bytes(_canonical_bytes(stable))[:24]
            exit_code = row.get("exit")
            research_value = row.get("research")
            research: Dict[str, Any] = (
                research_value if isinstance(research_value, dict) else {}
            )
            note = sanitize_text(str(row.get("note") or ""), 800)
            summary = (
                "Research run: %s" % research.get("question")
                if research.get("question")
                else (note or "Command completed with exit %s" % exit_code)
            )
            payload = {
                "event_type": "run",
                "occurred_at": occurred_at or row.get("ts") or now,
                "source": {"type": "run-ledger", "id": sid},
                "correlation": {
                    "run_id": row.get("run_id"),
                    "operation_id": row.get("operation_id"),
                },
                "outcome": {
                    "summary": summary,
                    "status": "success" if exit_code == 0 else "failure",
                },
                "verification_status": "failed"
                if exit_code not in (None, 0)
                else "not_verified",
                "evidence": [
                    {
                        "type": "run-ledger",
                        "ref": _source_ref(repo, evidence_path, sid),
                    }
                ],
                "provenance": {
                    "code_commit": row.get("git_sha"),
                    "job_id": row.get("slurm_job_id"),
                    "command_summary": _command_summary(row.get("cmd")),
                    "exit_code": exit_code,
                    "duration_seconds": row.get("duration_s"),
                    "hypothesis": research.get("hypothesis"),
                    "prediction": research.get("prediction"),
                    "baseline": research.get("baseline"),
                    "primary_metric": research.get("metric"),
                    "success_criterion": research.get("success"),
                    "independent_change": research.get("change"),
                },
            }
            if research.get("question"):
                payload["goal"] = research.get("question")
            metrics = []
            metrics_value = row.get("metrics")
            if isinstance(metrics_value, dict):
                for name, value in list(metrics_value.items())[:MAX_COLLECTION_ITEMS]:
                    metrics.append(
                        {
                            "name": str(name),
                            "value": value,
                            "evidence_ref": sid,
                        }
                    )
            if metrics:
                payload["metrics"] = metrics
        elif source_type == "run-capsule":
            sid = source_id or str(row.get("id") or "")
            result_value = row.get("result")
            capsule_result: Dict[str, Any] = (
                result_value if isinstance(result_value, dict) else {}
            )
            git_value = row.get("git")
            git_info: Dict[str, Any] = (
                git_value if isinstance(git_value, dict) else {}
            )
            env_value = row.get("env")
            env_info: Dict[str, Any] = (
                env_value if isinstance(env_value, dict) else {}
            )
            exit_code = capsule_result.get("exit")
            payload = {
                "event_type": "experiment",
                "occurred_at": occurred_at or row.get("ts") or now,
                "source": {"type": "run-capsule", "id": sid},
                "correlation": {"run_id": sid},
                "outcome": {
                    "summary": row.get("note") or "Experiment run completed",
                    "status": "success" if exit_code == 0 else "failure",
                },
                "verification_status": "failed"
                if exit_code not in (None, 0)
                else "not_verified",
                "evidence": [
                    {
                        "type": "capsule",
                        "ref": _source_ref(repo, evidence_path, sid),
                    }
                ],
                "provenance": {
                    "run_id": sid,
                    "job_id": row.get("slurm_job_id"),
                    "seed": (row.get("seeds") or [None])[0]
                    if isinstance(row.get("seeds"), list)
                    else None,
                    "code_commit": git_info.get("commit")
                    or git_info.get("commit_short"),
                    "command_summary": _command_summary(row.get("command")),
                    "exit_code": exit_code,
                    "duration_seconds": capsule_result.get("duration_s"),
                    "gpu": env_info.get("gpu"),
                    "execution_environment": env_info.get("platform"),
                },
            }
            configs_value = row.get("configs")
            configs: List[Any] = (
                configs_value if isinstance(configs_value, list) else []
            )
            refs = []
            for config in configs[:MAX_COLLECTION_ITEMS]:
                if isinstance(config, dict):
                    refs.append(
                        {
                            "type": "config",
                            "path": config.get("path"),
                            "id": config.get("sha256"),
                        }
                    )
            outputs_value = row.get("outputs")
            outputs: List[Any] = (
                outputs_value if isinstance(outputs_value, list) else []
            )
            for artifact in outputs[:MAX_COLLECTION_ITEMS]:
                if isinstance(artifact, dict):
                    refs.append(
                        {
                            "type": "artifact",
                            "path": artifact.get("path"),
                            "id": artifact.get("sha256"),
                        }
                    )
            if refs:
                payload["refs"] = refs
            if isinstance(capsule_result.get("metrics"), dict):
                payload["metrics"] = [
                    {"name": key, "value": value, "evidence_ref": sid}
                    for key, value in list(capsule_result["metrics"].items())[
                        :MAX_COLLECTION_ITEMS
                    ]
                ]
        elif source_type == "run-reconcile":
            sid = source_id or str(row.get("job_id") or "")
            state = str(row.get("state") or "")
            payload = {
                "event_type": "job",
                "occurred_at": occurred_at or row.get("reconciled_at") or now,
                "source": {"type": "run-reconcile", "id": sid},
                "outcome": {
                    "summary": "Job %s finished: %s" % (sid, state),
                    "status": "success" if state == "COMPLETED" else "failure",
                },
                "verification_status": "not_verified"
                if state == "COMPLETED"
                else "failed",
                "evidence": [
                    {
                        "type": "job-reconcile",
                        "ref": _source_ref(repo, evidence_path, sid),
                    }
                ],
                "provenance": {
                    "job_id": sid,
                    "exit_code": row.get("exit_code"),
                    "duration_seconds": row.get("elapsed"),
                },
            }
        elif source_type == "experiment-board":
            experiment_id = str(row.get("id") or "")
            state = str(row.get("status") or "")
            sid = source_id or "%s:%s" % (experiment_id, state)
            result = row.get("result") or row.get("reason")
            summary = (
                "Experiment %s %s: %s" % (experiment_id, state, result)
                if result
                else "Experiment %s %s" % (experiment_id, state)
            )
            payload = {
                "event_type": "experiment",
                "occurred_at": occurred_at or row.get("ts") or now,
                "source": {"type": "experiment-board", "id": sid},
                "outcome": {"summary": summary, "status": state},
                "verification_status": "not_verified",
                "evidence": [
                    {
                        "type": "experiment-board",
                        "ref": _source_ref(repo, evidence_path, sid),
                    }
                ],
                "provenance": {
                    "job_id": row.get("job"),
                    "verdict": row.get("result"),
                },
            }
            if row.get("reason"):
                payload["blocker"] = row.get("reason")
            if row.get("next"):
                payload["next_action"] = row.get("next")
        elif source_type == "ci-status":
            sid = source_id or "%s:%s" % (row.get("sha"), row.get("conclusion"))
            conclusion = str(row.get("conclusion") or row.get("status") or "")
            failed = conclusion in {
                "failure",
                "timed_out",
                "cancelled",
                "startup_failure",
            }
            if conclusion == "success":
                result_status = "success"
                result_verification = "passed"
            elif failed:
                result_status = "failure"
                result_verification = "failed"
            else:
                result_status = "pending"
                result_verification = "not_verified"
            refs = [
                {"type": "commit", "id": row.get("sha")},
                {"type": "ci-run", "url": row.get("url")},
            ]
            if row.get("pr_number") or row.get("pr_url"):
                refs.append(
                    {
                        "type": "pull_request",
                        "id": row.get("pr_number"),
                        "url": row.get("pr_url"),
                    }
                )
            payload = {
                "event_type": "ci",
                "occurred_at": occurred_at or row.get("ts") or now,
                "source": {"type": "ci-status", "id": sid},
                "outcome": {
                    "summary": "CI %s for commit %s" % (conclusion, row.get("sha")),
                    "status": result_status,
                },
                "verification_status": result_verification,
                "refs": refs,
                "evidence": [
                    {
                        "type": "ci",
                        "ref": _source_ref(repo, evidence_path, sid),
                    }
                ],
            }
        else:
            sid = source_id or str(row.get("id") or row.get("event_id") or "")
            payload = {
                "event_type": event_type or "annotation",
                "occurred_at": occurred_at or row.get("ts") or now,
                "source": {"type": source_type, "id": sid},
                "outcome": {
                    "summary": outcome or row.get("summary") or source_type,
                    "status": outcome_status or row.get("status") or "recorded",
                },
                "verification_status": verification_status or "not_verified",
                "evidence": [{"type": source_type, "ref": sid}],
            }

    if event_type:
        payload["event_type"] = event_type
    if source_id:
        payload.setdefault("source", {})["id"] = source_id
    if operation_id:
        payload.setdefault("correlation", {})["operation_id"] = operation_id
    if occurred_at:
        payload["occurred_at"] = occurred_at
    if verification_status:
        payload["verification_status"] = verification_status
    if outcome:
        payload.setdefault("outcome", {})["summary"] = outcome
    if outcome_status:
        payload.setdefault("outcome", {})["status"] = outcome_status
    return payload


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(add_help=True)
    sub = parser.add_subparsers(dest="command", required=True)
    configure = sub.add_parser("configure")
    target = configure.add_mutually_exclusive_group(required=True)
    target.add_argument("--data-source-id")
    target.add_argument("--discover", action="store_true")
    target.add_argument("--exclude-repo", action="append")
    target.add_argument("--include-repo", action="append")
    # argparse rejects a typo here with a visible message; the __main__ guard
    # deliberately swallows JournalError text, which a human-facing flag
    # cannot afford.
    target.add_argument("--lang", choices=sorted(_HEADINGS))
    configure.add_argument("--no-validate", action="store_true")
    status = sub.add_parser("status")
    status.add_argument("--repo", default=".")
    status.add_argument("--json", action="store_true")
    identity = sub.add_parser("identity")
    identity.add_argument("--repo", default=".")
    identity.add_argument("--json", action="store_true")
    identity.add_argument("--adopt-detected", action="store_true")
    rebuild = sub.add_parser("rebuild")
    rebuild.add_argument("--repo", default=".")
    materialize = sub.add_parser("materialize")
    materialize.add_argument("--repo", default=".")
    disconnect = sub.add_parser("disconnect")
    disconnect.add_argument("--managed", action="store_true")
    observe = sub.add_parser("observe")
    observe.add_argument("--repo", required=True)
    observe.add_argument("--source-type", required=True)
    observe.add_argument("--source-file", required=True)
    observe.add_argument("--record-path")
    observe.add_argument("--event-type")
    observe.add_argument("--source-id")
    observe.add_argument("--operation-id")
    observe.add_argument("--occurred-at")
    observe.add_argument("--verification-status")
    observe.add_argument("--outcome")
    observe.add_argument("--outcome-status")
    observe.add_argument("--operation", default="update")
    tick = sub.add_parser("tick")
    tick.add_argument("--repo", required=True)
    tick.add_argument("--local-only", action="store_true")
    tick.add_argument("--digest", action="store_true")
    tick.add_argument("--autodistill", action="store_true")
    show = sub.add_parser("show")
    show.add_argument("--repo", default=".")
    mode = show.add_mutually_exclusive_group()
    mode.add_argument("--today", action="store_true")
    mode.add_argument("--week", action="store_true")
    mode.add_argument("--period", metavar="YYYY-MM-DD|YYYY-Www")
    mode.add_argument("--blockers", action="store_true")
    mode.add_argument("--recent", type=int, metavar="N")
    show.add_argument("--json", action="store_true")
    sync = sub.add_parser("sync")
    sync.add_argument("--repo", default=".")
    sync.add_argument("--force", action="store_true")
    sync.add_argument("--today", action="store_true")
    distill = sub.add_parser("distill")
    distill.add_argument("--repo", default=".")
    distill.add_argument("--dry-run", action="store_true")
    return parser


def _run_show(store: JournalStore, args: argparse.Namespace) -> int:
    if args.recent is not None:
        events = store.recent_events(limit=args.recent)
        if args.json:
            print(json.dumps({"events": events}, ensure_ascii=False, sort_keys=True))
            return 0
        if not events:
            print("work-journal: no events recorded")
            return 0
        for event in events:
            outcome = event.get("outcome") or {}
            print(
                "- %s %s: %s (%s) [%s]"
                % (
                    event["occurred_at"],
                    event["event_type"],
                    outcome.get("summary") or "",
                    event["verification_status"],
                    event["event_id"],
                )
            )
        return 0
    if args.blockers:
        annotations = store.open_annotations()
        if args.json:
            print(json.dumps(annotations, ensure_ascii=False, sort_keys=True))
            return 0
        for label, bucket in (
            ("Open blockers", "blockers"),
            ("Next priorities", "next_actions"),
        ):
            print("%s (since %s):" % (label, annotations["since"]))
            if not annotations[bucket]:
                print("- none recorded")
                continue
            for row in annotations[bucket]:
                print("- %s (%s) [%s]" % (row["text"], row["local_date"], row["event_id"]))
        return 0
    current_day, current_week = store._current_periods()
    if args.period:
        period = args.period.strip()
        kind = "weekly" if re.match(r"^\d{4}-W\d{2}$", period) else "daily"
    elif args.week:
        kind, period = "weekly", current_week
    else:
        kind, period = "daily", current_day
    content = store.summary_text(kind, period)
    if args.json:
        print(
            json.dumps(
                {"kind": kind, "period": period, "content": content},
                ensure_ascii=False,
                sort_keys=True,
            )
        )
        return 0
    if content is None:
        print("work-journal: no %s summary for %s" % (kind, period))
        return 0
    print(content)
    return 0


def _run_autodistill(store: JournalStore) -> None:
    """Mechanical once-per-local-day distill at the tick boundary.

    Fail-open by contract: the tick's primary result is local materialization,
    and a raised exception here would become the exit code the prompt hook
    reports as degraded, discarding the digest it already produced.
    """

    try:
        if not store.distill_due_today():
            return
        lessons, dropped, skipped, deduped = store.distill()
    except Exception:
        return
    counts: List[str] = []
    if lessons:
        counts.append("%d promoted" % len(lessons))
    if deduped:
        counts.append("%d already in shared memory" % deduped)
    if skipped:
        counts.append("%d refused by the memory writer" % skipped)
    if dropped:
        counts.append("%d beyond the per-run cap" % dropped)
    # Tick stdout becomes agent context, so stay silent on a no-op day and
    # spend at most one bounded line when something actually moved.
    if counts:
        print("[work-journal] distill: %s" % ", ".join(counts))


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = _build_parser().parse_args(argv)
    if args.command == "configure":
        if args.lang:
            path = set_journal_language(args.lang)
            print("journal language: %s (%s)" % (args.lang, path))
            print("run `oms journal rebuild` to re-render existing summaries")
            return 0
        if args.exclude_repo or args.include_repo:
            excluded = update_notion_exclusions(
                args.exclude_repo or [], args.include_repo or []
            )
            if excluded:
                print("notion sync excludes %d repo(s):" % len(excluded))
                for entry in excluded:
                    print("  %s" % entry)
            else:
                print("notion sync excludes no repos")
            return 0
        data_source_id = (
            discover_notion_target() if args.discover else args.data_source_id
        )
        path = configure_notion(
            data_source_id,
            validate=not args.no_validate,
            auth_mode="ntn" if args.discover else "",
        )
        settings = notion_settings()
        print("configured: %s" % path)
        if not notion_auth_available(settings):
            print(
                "Notion access: missing "
                "(run ntn login or set OMS_WORK_JOURNAL_NOTION_TOKEN)"
            )
        else:
            print(
                "Notion access: %s; target schema validated"
                % (settings.get("auth_mode") or "token")
            )
        return 0
    if args.command == "disconnect":
        if not args.managed:
            raise JournalError("disconnect requires --managed")
        print("removed" if purge_notion_config() else "not configured")
        return 0
    store = JournalStore(args.repo)
    if args.command == "status":
        result = store.status()
        if args.json:
            print(json.dumps(result, ensure_ascii=False, sort_keys=True))
        else:
            print("Work Journal")
            print("- repo: %s" % result["repo"])
            project = result.get("project") or {}
            drift_note = ""
            if project.get("drift"):
                drift_note = (
                    " — detected %s; `oms journal identity --adopt-detected` merges"
                    % project.get("detected_name")
                )
            print(
                "- project: %s (%s)%s"
                % (project.get("name"), project.get("id"), drift_note)
            )
            print("- events: %s (%s bytes)" % (result["event_count"], result["events_bytes"]))
            print(
                "- summaries: %s daily, %s weekly"
                % (result["daily_count"], result["weekly_count"])
            )
            print(
                "- notion: %s, credential %s, %s synced, %s pending, %s failed"
                % (
                    "configured" if result["notion"]["configured"] else "not configured",
                    "present"
                    if result["notion"]["credential_present"]
                    else "missing",
                    result["notion"]["synced"],
                    result["notion"]["pending"],
                    result["notion"]["failed"],
                )
            )
        return 0
    if args.command == "rebuild":
        result = store.rebuild()
        print(
            "rebuilt: %s events, %s daily, %s weekly"
            % (result["event_count"], len(result["daily"]), len(result["weekly"]))
        )
        return 0
    if args.command == "identity":
        detected_id, detected_name = project_identity(store.repo)
        pinned_id, pinned_name = store._existing_project_identity()
        drift = pinned_id is not None and (
            pinned_id != detected_id or pinned_name != detected_name
        )
        if args.adopt_detected:
            if pinned_id is not None and not drift:
                print(
                    "identity already canonical: %s (%s)"
                    % (pinned_name, pinned_id)
                )
                return 0
            atomic_write_json(
                store.project_path,
                {
                    "schema_version": 1,
                    "project_id": detected_id,
                    "project_name": detected_name,
                },
            )
            # The mirror is disposable by contract ("the mirror can be
            # discarded and resynchronized"); dropping the mapping makes the
            # next sync find pages by the NEW keys through the remote key
            # lookup instead of patching old-identity pages in place. Events
            # stay append-only: old rows keep their historical identity, and
            # the rebuild re-keys every derived view under the new pin.
            try:
                store.notion_state_path.unlink()
            except FileNotFoundError:
                pass
            rebuilt = JournalStore(args.repo).rebuild()
            print(
                "adopted: %s (%s) — was %s (%s); rebuilt %s events, %s daily, %s weekly"
                % (
                    detected_name,
                    detected_id,
                    pinned_name or "unpinned",
                    pinned_id or "-",
                    rebuilt["event_count"],
                    len(rebuilt["daily"]),
                    len(rebuilt["weekly"]),
                )
            )
            print("next: oms journal sync --force")
            return 0
        row = {
            "pinned_id": pinned_id,
            "pinned_name": pinned_name,
            "detected_id": detected_id,
            "detected_name": detected_name,
            "drift": drift,
        }
        if args.json:
            print(json.dumps(row, ensure_ascii=False, sort_keys=True))
            return 0
        print("pinned:   %s (%s)" % (pinned_name or "(none)", pinned_id or "-"))
        print("detected: %s (%s)" % (detected_name, detected_id))
        if drift:
            print(
                "drift: yes — `oms journal identity --adopt-detected` re-pins,"
                " rebuilds, and resets the Notion mapping"
            )
        else:
            print("drift: no")
        return 0
    if args.command == "materialize":
        store.materialize()
        return 0
    if args.command == "sync":
        store.sync_notion(force=args.force, today_only=args.today)
        return 0
    if args.command == "distill":
        lessons, dropped, skipped, deduped = store.distill(dry_run=args.dry_run)
        if args.dry_run:
            for lesson in lessons:
                print(lesson)
            print("journal distill: %s lesson(s) would be promoted" % len(lessons))
        elif lessons:
            print("journal distill: %s lesson(s) promoted, marker advanced" % len(lessons))
        else:
            print("journal distill: nothing to promote")
        if deduped:
            print(
                "journal distill: %d candidate(s) already in shared memory"
                " and skipped" % deduped
            )
        if skipped:
            print(
                "journal distill: %d lesson(s) refused by the memory writer"
                " and skipped" % skipped
            )
        if dropped:
            print(
                "journal distill: %d candidate(s) beyond the per-run cap were"
                " not promoted" % dropped
            )
        return 0
    if args.command == "show":
        return _run_show(store, args)
    if args.command == "observe":
        payload = source_payload(
            store.repo,
            args.source_type,
            pathlib.Path(args.source_file),
            record_path=pathlib.Path(args.record_path) if args.record_path else None,
            event_type=args.event_type,
            source_id=args.source_id,
            operation_id=args.operation_id,
            occurred_at=args.occurred_at,
            verification_status=args.verification_status,
            outcome=args.outcome,
            outcome_status=args.outcome_status,
            operation=args.operation,
        )
        store.record_event(payload)
    else:
        store.capture_head_commit()
    store.materialize()
    if args.command == "tick" and args.digest:
        digest = store.prompt_digest()
        if digest:
            print(digest)
    if args.command == "tick" and args.autodistill:
        _run_autodistill(store)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (JournalError, OSError, ValueError, TypeError):
        # The shell observer emits one fixed, bounded diagnostic. Avoid echoing
        # exception messages here because source paths or remote errors may be
        # sensitive.
        raise SystemExit(1)
