"""Machine-local provider accounts and read-only selection."""

import json
import os
from pathlib import Path
import re
import stat
import time


# Enable a provider only after its CLI's independent home behavior is verified.
PROVIDER_HOMES = {"codex": ("CODEX_HOME", True),
                  "claude": ("CLAUDE_CONFIG_DIR", False),
                  "pi": ("PI_CODING_AGENT_DIR", False)}


def _unique_fields(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate provider accounts configuration key")
        result[key] = value
    return result


def default_account(provider):
    variable, enabled = PROVIDER_HOMES[provider]
    if not enabled:
        raise ValueError("accounts for %s are not supported yet" % provider)
    home = Path(os.environ.get(variable) or Path.home() / ("." + provider)).expanduser().resolve()
    return {"id": "default", "home": str(home)}


def account_home_error(entry):
    """Recheck the directory so a vanished home cannot become unknown quota."""
    try:
        if Path(entry["home"]).is_dir():
            return None
    except (OSError, ValueError):
        pass
    return "account home must be an existing directory: " + entry["id"]


def load_accounts(path=None):
    """Validate the user-local schema; unavailable homes remain per-account errors."""
    from panel_routing import policy
    if path is None:
        root = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
        if not root.is_absolute():
            root = Path.home() / ".config"
        path = root / "oh-my-setting/provider-accounts.json"
    path = Path(path)
    if not path.is_absolute():
        raise ValueError("provider accounts configuration path must be absolute")
    try:
        value = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=_unique_fields)
    except FileNotFoundError:
        return {"schema": 1, "configured": False, "providers": {
            "codex": {"policy": "ordered", "accounts": [default_account("codex")]}}}
    except OSError as error:
        raise ValueError("invalid provider accounts configuration: " + type(error).__name__) from error
    except (ValueError, RecursionError) as error:
        raise ValueError("invalid provider accounts configuration: " + str(error)) from error
    if not isinstance(value, dict) or set(value) - {"schema", "providers"}:
        raise ValueError("unknown provider accounts configuration keys")
    if type(value.get("schema")) is not int or value["schema"] != 1:
        raise ValueError("unsupported provider accounts schema")
    providers = value.get("providers")
    if not isinstance(providers, dict):
        raise ValueError("provider accounts providers must be an object")
    routes = policy()
    known = {row["provider"] for tier in routes["tiers"].values() for row in tier}
    known.update(routes["main"])
    ids, homes, normalized = set(), set(), {}
    for provider, settings in providers.items():
        if provider not in known:
            raise ValueError("unknown provider in accounts configuration: " + provider)
        if provider not in PROVIDER_HOMES or not PROVIDER_HOMES[provider][1]:
            raise ValueError("accounts for %s are not supported yet" % provider)
        if not isinstance(settings, dict) or set(settings) - {"policy", "accounts"}:
            raise ValueError("unknown provider accounts configuration keys: " + provider)
        if settings.get("policy", "ordered") not in ("ordered", "spread"):
            raise ValueError("unsupported provider accounts policy: " + provider)
        entries = settings.get("accounts")
        if not isinstance(entries, list) or not entries:
            raise ValueError("provider accounts must be a non-empty list: " + provider)
        accounts = []
        for entry in entries:
            if not isinstance(entry, dict) or set(entry) != {"id", "home"}:
                raise ValueError("unknown or missing provider account fields")
            ident = entry["id"]
            if not isinstance(ident, str) or not re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", ident):
                raise ValueError("invalid provider account id")
            raw_home = entry["home"]
            if (not isinstance(raw_home, str) or not raw_home or "\0" in raw_home
                    or not Path(raw_home).is_absolute()):
                raise ValueError("provider account home must be an absolute path")
            try:
                home = Path(raw_home).resolve()
            except (OSError, ValueError, RuntimeError) as error:
                raise ValueError("invalid provider account home: " + ident) from error
            canonical = os.path.normcase(str(home))
            if ident in ids or canonical in homes:
                raise ValueError("duplicate provider account id or canonical home")
            ids.add(ident)
            homes.add(canonical)
            account = dict(entry, home=str(home))
            error = account_home_error(account)
            if error:
                account["error"] = error
            accounts.append(account)
        normalized[provider] = {"policy": settings.get("policy", "ordered"), "accounts": accounts}
    return {"schema": 1, "providers": normalized, "configured": True}


def model_slugs(home):
    """Only model names leave the bounded local cache reader; unreadable is unknown."""
    try:
        fd = os.open(str(Path(home) / "models_cache.json"), os.O_RDONLY
                     | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
                     | getattr(os, "O_BINARY", 0))
        with os.fdopen(fd, "rb") as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_size > 4 * 1024 * 1024:
                return None
            body = stream.read(4 * 1024 * 1024 + 1)
            if len(body) > 4 * 1024 * 1024:
                return None
            data = json.loads(body)
        models = data.get("models") if isinstance(data, dict) else None
        if not isinstance(models, list) or any(not isinstance(row, dict)
                or not isinstance(row.get("slug"), str) or not row["slug"] for row in models):
            return None
        return {row["slug"] for row in models}
    except (OSError, ValueError, RecursionError):
        return None


def account_weekly_used(row, now=None):
    from panel_metrics import percent
    if not isinstance(row, dict):
        return None
    reset = row.get("resets_at")
    if isinstance(reset, (int, float)) and reset <= (time.time() if now is None else now):
        return None
    return percent(row.get("weekly_used"))


def select_account(provider, model, readings=None, running=None, config=None, now=None, explicit=False):
    """Return (account or None, reason), preserving list order for equal choices.

    Unknown quota ranks after measured quota for spread. Running counts are a
    best-effort observation, never a reservation between simultaneous callers.
    Explicit targets pick the first eligible account and fall back to the current home.
    """
    from panel_metrics import account_readings
    config = load_accounts() if config is None else config
    settings = config["providers"].get(provider)
    if settings is None:
        return None, "no configured accounts for " + provider
    readings = account_readings(provider, config=config) if readings is None else readings
    readings = readings if isinstance(readings, dict) else {}
    now = time.time() if now is None else now
    eligible, skipped = [], []
    for entry in settings["accounts"]:
        ident = entry["id"]
        error = account_home_error(entry)
        if error:
            skipped.append(ident + ": " + error)
            continue
        used = account_weekly_used(readings.get(ident, {}), now)
        if used is not None and used >= 90:
            skipped.append(ident + ": <=10% weekly quota left")
            continue
        models = model_slugs(entry["home"]) if provider == "codex" else None
        if models is not None and model not in models:
            skipped.append(ident + ": cached model list lacks " + model)
            continue
        eligible.append((entry, used))
    if settings["policy"] == "spread" and not explicit:
        eligible.sort(key=lambda item: (item[1] if item[1] is not None else 100,
                                       (running or {}).get(item[0]["id"], 0)))
    reason = ("explicit target" if explicit else settings["policy"])
    reason += "; skipped " + "; ".join(skipped) if skipped else ""
    if explicit and not eligible:
        return default_account(provider), reason + "; default current home"
    return (eligible[0][0] if eligible else None), reason
