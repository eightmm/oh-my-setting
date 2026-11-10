"""One advisory line before a delegate dispatch when the target provider's last reported window is high.

Reads only what the provider already reported (the Claude HUD cache, the Codex session log); nothing is
derived from tokens, nothing is recorded and no provider is swapped. Silent unless OMS_QUOTA_WARN_PCT is
set, and silent for a reading that is missing, rolled over or older than MAX_AGE_SECONDS."""

import os
from pathlib import Path
import re
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))

from dashboard_projection import mapping, number
from hook_state import env_int
from panel_metrics import _hud_record, codex_weekly_reading, percent

MAX_AGE_SECONDS = 600


def _duration(seconds):
    minutes = int(seconds // 60)
    return "<1m" if minutes < 1 else "%dm" % minutes if minutes < 60 else "%dh%02dm" % divmod(minutes, 60)


def _claude_readings(now):
    """[(window label, percent used, reported at, resets at)] from the newest HUD record."""
    folder = Path(os.environ.get("OMS_HUD_CACHE_DIR") or Path(tempfile.gettempdir()) / "oh-my-setting-hud")
    newest = {}
    try:
        if folder.is_symlink():
            return []
        with os.scandir(folder) as entries:
            for index, entry in enumerate(entries):
                if index >= 4096:
                    return []
                match = re.fullmatch(r"ctx-([0-9a-f]{24})\.json", entry.name)
                if not match:
                    continue
                row = _hud_record(match.group(1) + "0" * 8)
                stamp = number(row.get("ts"))
                if stamp is not None and stamp <= now and (not newest or stamp > newest["ts"]):
                    newest = row
    except OSError:
        return []
    found = []
    for key, label in (("five_hour", "5h"), ("seven_day", "weekly")):
        window = mapping(newest.get(key))
        used, reset = percent(window.get("used_percentage")), number(window.get("resets_at"))
        if used is not None and (reset is None or reset > now):
            found.append((label, used, newest["ts"], reset))
    return found


def _readings(provider, now):
    if provider == "claude":
        return _claude_readings(now)
    if provider == "codex":
        reading = codex_weekly_reading(now, detail=True)
        if reading and number(reading.get("observed_at")) is not None and reading["observed_at"] <= now:
            return [("weekly", reading["used"], reading["observed_at"], reading.get("resets_at"))]
    return []


def alternative(provider, role):
    """The first other provider in the role's configured tier, as "provider model (tier)"; None if absent."""
    try:
        import panel_routing
        policy = panel_routing.policy()
        tier = mapping(policy["roles"].get(role)).get("tier") if role else None
        for route in policy["tiers"].get(tier, []):
            if route["provider"] != provider and route.get("access") != "read" and not route.get("public_only"):
                return "%s %s (%s tier)" % (route["provider"], route["model"], tier)
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        pass
    return None


def hint(provider, role="", now=None):
    line = env_int("OMS_QUOTA_WARN_PCT", 0, minimum=0, maximum=99)
    if not line:
        return ""
    now = time.time() if now is None else now
    high = [r for r in _readings(provider, now) if r[1] >= line and 0 <= now - r[2] <= MAX_AGE_SECONDS]
    if not high:
        return ""
    label, used, reported, reset = max(high, key=lambda r: r[1])
    text = "oms: quota hint: %s %s window %s%% used (last reported %s ago%s)" % (
        provider, label, used, _duration(now - reported),
        ", resets in " + _duration(reset - now) if reset is not None else "")
    other = alternative(provider, role)
    if other:
        text += "; configured alternative: " + other
    return text + "; dispatch continues unchanged"


if __name__ == "__main__":
    try:
        message = hint(sys.argv[1] if len(sys.argv) > 1 else "", sys.argv[2] if len(sys.argv) > 2 else "")
    except Exception:  # advisory only: never fail a dispatch
        message = ""
    if message:
        print(message, file=sys.stderr)
