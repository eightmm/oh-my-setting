"""Advisory notes about provider prompt-cache warmth; never a gate, never a record.

Provider prompt caches expire after minutes and are rebuilt when the model or effort of a session changes.
OMS records no per-session last-activity timestamp of its own, so the age here is the newest of what is
already on disk: the native session file's mtime (the provider writes it every turn) and the lifecycle
attempt row's `updated_at`. An unknown age yields no warning, only an "unknown" phrase.
"""

import datetime
import os
from pathlib import Path

DEFAULT_WINDOW = 300


def window():
    """Warm window in seconds from OMS_CACHE_WARM_SECONDS (0 turns the note off); invalid keeps the default."""
    try:
        value = int(os.environ.get("OMS_CACHE_WARM_SECONDS", DEFAULT_WINDOW))
    except ValueError:
        return DEFAULT_WINDOW
    return value if 0 <= value <= 86400 else DEFAULT_WINDOW


def session_mtime(provider, session):
    """Newest mtime (epoch seconds) of the provider's native session file, or None when it is not found."""
    if not isinstance(session, str) or not session or "/" in session or "\\" in session:
        return None
    if provider == "claude":
        base = Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude") / "projects"
        found = base.glob("*/%s.jsonl" % session)
    elif provider == "codex":
        found = (Path(os.environ.get("CODEX_HOME") or Path.home() / ".codex") / "sessions").rglob("*%s*.jsonl" % session)
    else:
        return None
    times = []
    for path in found:
        try:
            times.append(path.stat().st_mtime)
        except OSError:
            continue
    return max(times) if times else None


def _row_time(row):
    stamp = row.get("updated_at") if isinstance(row, dict) else None
    try:
        return datetime.datetime.strptime(stamp, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc).timestamp()
    except (TypeError, ValueError):
        return None


def age_seconds(provider, session, row=None, now=None):
    """Seconds since the session was last active, or None when neither source knows."""
    seen = [t for t in (session_mtime(provider, session), _row_time(row)) if t is not None]
    if not seen:
        return None
    return max(0, int((datetime.datetime.now().timestamp() if now is None else now) - max(seen)))


def human(seconds):
    if seconds < 60:
        return "%ds" % seconds
    if seconds < 3600:
        return "%dm%02ds" % divmod(seconds, 60)
    return "%dh%02dm" % divmod(seconds // 60, 60)


def age_phrase(age):
    return "last active %s ago" % human(age) if age is not None else "last activity unknown"


def rebuild_note(subject, age, change):
    """One advisory line when `change` (what differs, e.g. 'model') hits a session inside the warm window."""
    limit = window()
    if age is None or limit == 0 or age > limit:
        return None
    return ("note: %s was active %s ago (warm window %ds); changing %s rebuilds the provider prompt-cache "
            "prefix. Proceeding." % (subject, human(age), limit, change))
