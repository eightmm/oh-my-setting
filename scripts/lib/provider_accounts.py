"""Machine-local provider account registration, health and read-only selection."""

from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import stat
import sys
import time


CODEX_SHARED_ENTRIES = ("AGENTS.md", "config.toml", "hooks.json", ".hook-trust",
                        "plugins", "rules", "skills")
ACCOUNT_READING_FRESH_SECONDS = 10080 * 60
_ACCOUNT_SAFE_UNLINK = (hasattr(os, "O_DIRECTORY") and hasattr(os, "O_NOFOLLOW")
                        and all(call in os.supports_dir_fd for call in (os.stat, os.readlink, os.unlink))
                        and os.stat in os.supports_follow_symlinks)
_ACCOUNT_FD_SYMLINK = os.symlink in os.supports_dir_fd


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
    """Recheck directory ownership and permissions before trusting a profile."""
    try:
        info = Path(entry["home"]).resolve().stat()
        if stat.S_ISDIR(info.st_mode):
            if hasattr(os, "getuid"):
                if info.st_uid != os.getuid():
                    return ("account home must be owned by the current user: " + entry["id"]
                            + "; restore its ownership or choose a user-owned home")
                if info.st_mode & 0o022:
                    return ("account home must not be group/other-writable: " + entry["id"]
                            + "; run chmod go-w on that home")
            return None
    except (OSError, ValueError, RuntimeError):
        pass
    return "account home must be an existing directory: " + entry["id"]


def implicit_default_account(route):
    reason = route.get("account_reason")
    return (route.get("account") == "default" and isinstance(reason, str)
            and reason.endswith("; default current home"))


def route_account_home(route):
    """Resolve only the recorded id, rechecking its home at the execution boundary."""
    provider, ident = route.get("provider"), route.get("account")
    if provider != "codex" or not ident or implicit_default_account(route):
        return None
    config = load_accounts()
    entries = config["providers"].get(provider, {}).get("accounts", []) if config["configured"] else []
    entry = next((entry for entry in entries if entry["id"] == ident), None)
    if entry is None:
        raise ValueError("Codex account is no longer configured: " + ident)
    error = account_home_error(entry)
    if error:
        raise ValueError(error)
    return Path(entry["home"])


def headless_codex_account(model, explicit=False):
    """Select for a shared-runner child; an applied panel route is already pinned."""
    applied = os.environ.get("OMS_CODEX_ACCOUNT_ROUTE")
    if applied:
        try:
            route = json.loads(applied)
        except ValueError as error:
            raise ValueError("invalid applied Codex account route") from error
        if not isinstance(route, dict):
            raise ValueError("invalid applied Codex account route")
        ident = route.get("account") if route.get("account_applied") is True else None
        if ident is not None and (not isinstance(ident, str)
                or not re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", ident)):
            raise ValueError("invalid applied Codex account id")
        return None, ident
    if os.environ.get("OMS_RESUME_SESSION"):
        return None, None
    current = os.environ.get("CODEX_HOME")
    if current and os.path.normcase(str(Path(current).expanduser().resolve())) != os.path.normcase(
            str((Path.home() / ".codex").resolve())):
        return None, None
    try:
        config = load_accounts()
    except (OSError, ValueError, RuntimeError) as error:
        print("warning: Codex account selection skipped: %s; using caller home" % error, file=sys.stderr)
        return None, None
    if not config["configured"] or "codex" not in config["providers"]:
        return None, None
    selected, reason = select_account("codex", model, config=config, explicit=explicit)
    if selected is None:
        raise ValueError("no eligible Codex account: " + reason)
    home = route_account_home({"provider": "codex", "account": selected["id"], "account_reason": reason})
    return (home, selected["id"]) if home is not None else (None, None)


def next_account_route(route, readings=None, now=None):
    """Select once from the remaining eligible accounts in quota order."""
    if route.get("provider") != "codex" or not route.get("account"):
        return None
    config = load_accounts()
    settings = config["providers"].get("codex") if config["configured"] else None
    if not settings:
        return None
    entries = settings["accounts"]
    if not any(entry["id"] == route["account"] for entry in entries):
        return None
    remaining = [entry for entry in entries if entry["id"] != route["account"]]
    if not remaining:
        return None
    config = dict(config, providers={"codex": dict(settings, policy="ordered", accounts=remaining)})
    selected, reason = select_account("codex", route["model"], readings=readings, config=config, now=now)
    return dict(route, account=selected["id"], account_reason="capacity overflow; " + reason) if selected else None


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
    from panel_metrics import _stamp, number, percent
    if not isinstance(row, dict):
        return None
    now = time.time() if now is None else now
    reset = row.get("resets_at")
    if "resets_at" in row and reset is not None and (number(reset) is None or reset <= now):
        return None
    if "observed_at" in row or "timestamp" in row:
        raw = row.get("observed_at") if "observed_at" in row else row.get("timestamp")
        observed = _stamp(raw) if isinstance(raw, str) else number(raw)
        if observed is None or not 0 <= now - observed <= ACCOUNT_READING_FRESH_SECONDS:
            return None
    return percent(row.get("weekly_used"))


def select_account(provider, model, readings=None, running=None, config=None, now=None, explicit=False):
    """Return (account or None, reason) in measured-quota order, then list order.

    Fresh measured weekly usage ranks ascending before unknown readings; spread
    breaks quota ties by live running count before list order. Explicit targets
    use the same quota order and fall back to the current home when none is eligible.
    """
    from panel_metrics import account_readings
    config = load_accounts() if config is None else config
    settings = config["providers"].get(provider)
    if settings is None:
        return None, "no configured accounts for " + provider
    now = time.time() if now is None else now
    readings = account_readings(provider, config=config, now=now) if readings is None else readings
    readings = readings if isinstance(readings, dict) else {}
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
        if model != "provider-default" and models is not None and model not in models:
            skipped.append(ident + ": cached model list lacks " + model)
            continue
        eligible.append((entry, used))
    eligible.sort(key=lambda item: (item[1] if item[1] is not None else 100,
                                    (running or {}).get(item[0]["id"], 0)
                                    if settings["policy"] == "spread" and not explicit else 0))
    reason = ("explicit target" if explicit else settings["policy"])
    reason += "; skipped " + "; ".join(skipped) if skipped else ""
    if explicit and not eligible:
        return default_account(provider), reason + "; default current home"
    return (eligible[0][0] if eligible else None), reason


def _account_paths():
    root = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    if not root.is_absolute():
        root = Path.home() / ".config"
    root = (root / "oh-my-setting").resolve()
    return root / "provider-accounts.json", root / "codex-account-links.json"


def _account_json(path, fallback):
    try:
        info = path.lstat()
    except FileNotFoundError:
        return fallback
    if not stat.S_ISREG(info.st_mode):
        raise ValueError("account state must be a regular non-link file")
    try:
        return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=_unique_fields,
                          parse_constant=_invalid_account_constant)
    except (ValueError, RecursionError) as error:
        raise ValueError("malformed account state; left unchanged") from error


def _invalid_account_constant(_value):
    raise ValueError("non-finite account state value")


def _registration_config(path):
    # Validate owned schema fields without discarding future extensions. The
    # routing reader deliberately remains stricter about unsupported settings.
    value = _account_json(path, {"schema": 1, "providers": {}})
    if not isinstance(value, dict) or type(value.get("schema")) is not int or value["schema"] != 1:
        raise ValueError("unsupported provider accounts schema")
    if not isinstance(value.get("providers"), dict):
        raise ValueError("provider accounts providers must be an object")
    ids, homes = set(), set()
    for settings in value["providers"].values():
        if not isinstance(settings, dict) or settings.get("policy", "ordered") not in ("ordered", "spread"):
            raise ValueError("invalid provider accounts policy")
        entries = settings.get("accounts")
        if not isinstance(entries, list) or not entries:
            raise ValueError("provider accounts must be a non-empty list")
        for entry in entries:
            if not isinstance(entry, dict):
                raise ValueError("invalid provider account entry")
            ident, home = entry.get("id"), entry.get("home")
            if not isinstance(ident, str) or not re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", ident):
                raise ValueError("invalid provider account id")
            if not isinstance(home, str) or not home or "\0" in home or not Path(home).is_absolute():
                raise ValueError("provider account home must be an absolute path")
            canonical = os.path.normcase(str(Path(home).resolve()))
            if ident in ids or canonical in homes:
                raise ValueError("duplicate provider account id or canonical home")
            ids.add(ident)
            homes.add(canonical)
    return value


def _link_receipt(path):
    rows = _account_json(path, [])
    if not isinstance(rows, list):
        raise ValueError("invalid Codex link ownership receipt")
    for row in rows:
        if (not isinstance(row, dict) or set(row) not in (
                    {"id", "home", "links"}, {"id", "home", "links", "st_dev", "st_ino"})
                or not isinstance(row["id"], str) or not isinstance(row["home"], str)
                or not Path(row["home"]).is_absolute() or not isinstance(row["links"], dict)
                or any(type(row[field]) is not int or row[field] < 0
                       for field in ("st_dev", "st_ino") if field in row)
                or any(name not in CODEX_SHARED_ENTRIES or not isinstance(target, str)
                       or not Path(target).is_absolute() for name, target in row["links"].items())):
            raise ValueError("invalid Codex link ownership receipt")
    return rows


def _outside_repository(home):
    for parent in (home, *home.parents):
        marker = parent / ".git"
        if (marker.is_file() or marker.is_symlink() or (marker / "HEAD").is_file() or ((parent / "HEAD").is_file()
                and (parent / "objects").is_dir() and (parent / "refs").is_dir())):
            raise ValueError("Codex account home must not be inside a repository; use --home PATH outside the repository")


def _matching_account_link(link, target):
    try:
        return link.is_symlink() and link.resolve() == target.resolve()
    except (OSError, RuntimeError):
        return False


def _account_parent_private(parent):
    import shlex
    if not callable(getattr(os, "getuid", None)):
        raise ValueError("cannot verify Codex account ancestor %s ownership; fix: use --home on a platform with ownership checks" %
                         shlex.quote(str(parent)))
    while not parent.exists() and parent != parent.parent:
        parent = parent.parent
    parent = parent.resolve()
    for ancestor in (parent, *parent.parents):
        info = ancestor.stat()
        quoted = shlex.quote(str(ancestor))
        if info.st_uid not in (0, os.getuid()):
            raise ValueError("Codex account ancestor %s is not owned by the current user or root; fix: use --home PATH elsewhere" % quoted)
        if info.st_mode & 0o022 and not info.st_mode & stat.S_ISVTX:
            raise ValueError("unsafe Codex account ancestor %s; fix: chmod go-w %s, or use --home PATH elsewhere" %
                             (quoted, quoted))


def _account_home_private(home, info):
    import shlex
    quoted = shlex.quote(str(home))
    if not callable(getattr(os, "getuid", None)):
        raise ValueError("cannot verify ownership of Codex account home %s; fix: use a platform with ownership checks" % quoted)
    if info.st_uid != os.getuid():
        raise ValueError("Codex account home %s is not owned by the current user; fix: chown %s %s" %
                         (quoted, os.getuid(), quoted))
    if info.st_mode & 0o022:
        raise ValueError("unsafe Codex account home %s; fix: chmod go-w %s" % (quoted, quoted))
    _account_parent_private(home.parent)


def _verify_account_directory(home, descriptor, owner):
    info = os.fstat(descriptor) if descriptor is not None else os.lstat(str(home))
    current = os.lstat(str(home))
    if (not stat.S_ISDIR(current.st_mode) or not stat.S_ISDIR(info.st_mode)
            or (current.st_dev, current.st_ino) != (info.st_dev, info.st_ino)
            or (owner is not None and (owner.get("st_dev"), owner.get("st_ino")) != (info.st_dev, info.st_ino))):
        raise ValueError("Codex account directory identity changed or unverified: %s; choose another home" % home)
    return info


@contextmanager
def _account_directory(home, owner=None):
    descriptor = None
    try:
        initial = _verify_account_directory(home, None, owner)
        _account_home_private(home, initial)
        if _ACCOUNT_SAFE_UNLINK:
            descriptor = os.open(str(home), os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        expected = owner if owner is not None else {"st_dev": initial.st_dev, "st_ino": initial.st_ino}
        info = _verify_account_directory(home, descriptor, expected)
        _account_home_private(home, info)
        yield descriptor, info
    finally:
        if descriptor is not None:
            os.close(descriptor)


def _account_entry(home, descriptor, name):
    try:
        if descriptor is not None:
            return os.stat(name, dir_fd=descriptor, follow_symlinks=False)
        return os.lstat(str(home / name))
    except FileNotFoundError:
        return None


def _unlink_account_entry(home, descriptor, owner, name, target, expected, dry_run=False):
    if descriptor is None:
        print("skipped: platform lacks safe unlink: " + str(home / name))
        return False
    try:
        info = _verify_account_directory(home, descriptor, owner)
        _account_home_private(home, info)
        current = _account_entry(home, descriptor, name)
        if (current is None or not stat.S_ISLNK(current.st_mode)
                or (current.st_dev, current.st_ino) != (expected.st_dev, expected.st_ino)
                or os.readlink(name, dir_fd=descriptor) != target):
            raise ValueError("entry type, identity or target changed")
        # Recheck after readlink: it can race a rename of either the entry or
        # the home. The descriptor never redirects an unlink into a new home.
        after = _account_entry(home, descriptor, name)
        if (after is None or not stat.S_ISLNK(after.st_mode)
                or (after.st_dev, after.st_ino) != (current.st_dev, current.st_ino)):
            raise ValueError("entry type or identity changed")
        _verify_account_directory(home, descriptor, owner)
    except (OSError, ValueError) as error:
        print("warn: kept Codex account entry %s: %s" % (home / name, error))
        return False
    if not dry_run:
        # The verified home belongs to this user and is not group/other-writable;
        # only the same user (or trusted root) can race this final unlink.
        os.unlink(name, dir_fd=descriptor)
    return True


def _recorded_account_link(home, descriptor, owner, name, target, expected):
    try:
        _verify_account_directory(home, descriptor, owner)
        info = _account_entry(home, descriptor, name)
        if (info is not None and stat.S_ISLNK(info.st_mode)
                and (info.st_dev, info.st_ino) == (expected.st_dev, expected.st_ino)
                and os.readlink(name, dir_fd=descriptor) == target):
            return info
    except (OSError, ValueError):
        pass
    return None


def _account_link_progress(rows, receipt, row, name, target=None):
    from oms_runtime.common import atomic_write_json
    if target is None:
        row["links"].pop(name, None)
    else:
        row["links"][name] = target
    atomic_write_json(receipt, rows)


def _retire_stale_account_home(rows, receipt, row):
    import shlex
    from oms_runtime.common import atomic_write_json
    try:
        info = os.lstat(row["home"])
    except FileNotFoundError:
        reason = "home missing"
    except OSError:
        return False
    else:
        if (stat.S_ISDIR(info.st_mode)
                and (row.get("st_dev"), row.get("st_ino")) == (info.st_dev, info.st_ino)):
            return False
        reason = "directory identity changed or unverified"
    # Explicit unregister forgets stale authority; it never adopts the entries
    # of a different directory at the same path.
    rows.remove(row)
    atomic_write_json(receipt, rows)
    print("warn: Codex account %s %s; stale receipt removed, links kept" % (row["id"], reason))
    print("recovery: oms connect-services --account codex:%s --home %s" %
          (shlex.quote(row["id"]), shlex.quote(row["home"])))
    return True


def _account_recovery(ident="ID"):
    import shlex
    main = ident == "default"
    ident = "ID" if main else ident
    command = "oms connect-services --account " + shlex.quote("codex:" + ident)
    print("recovery: " + ("restore the main Codex home; for other accounts: " if main else "") +
          command + " --remove; or " + command + " --repair")


def _report_account_cleanup(removed, kept, rows_kept, dry_run):
    reasons = ", ".join("%s: %s" % (reason, count) for reason, count in sorted(kept.items()) if count)
    print("Codex account cleanup: %s %s links; %s %s links%s; %s %s receipt rows; registry %s" % (
        "would remove" if dry_run else "removed", removed,
        "would keep" if dry_run else "kept", sum(kept.values()),
        " (" + reasons + ")" if reasons else "",
        "would keep" if dry_run else "kept", rows_kept,
        "would be kept" if dry_run else "kept"))
    if sum(kept.values()) or rows_kept:
        _account_recovery()


def _drop_account_links(rows, receipt, ident=None, root=None, dry_run=False):
    from collections import Counter
    from oms_runtime.common import atomic_write_json
    main = Path(default_account("codex")["home"])
    removed_count, kept = 0, Counter()
    rows_kept = sum(ident is None or row["id"] == ident for row in rows)
    for row in list(rows):
        if ident is not None and row["id"] != ident:
            continue
        home = Path(row["home"])
        pending = set(row["links"])
        remaining = set(pending)
        if ident is not None and not dry_run and _retire_stale_account_home(rows, receipt, row):
            rows_kept -= 1
            continue
        if not _ACCOUNT_SAFE_UNLINK:
            print("skipped: platform lacks safe unlink: " + str(home))
            kept["safe unlink unavailable"] += len(pending)
            continue
        try:
            with _account_directory(home, row) as (descriptor, _info):
                for name, target in list(row["links"].items()):
                    _verify_account_directory(home, descriptor, row)
                    current = _account_entry(home, descriptor, name)
                    recorded = (_recorded_account_link(home, descriptor, row, name, target, current)
                                if current is not None else None)
                    if recorded is None:
                        if current is not None:
                            print("warn: kept changed or unmanaged Codex account entry: " + str(home / name))
                            kept["changed or unmanaged"] += 1
                        remaining.discard(name)
                        pending.discard(name)
                        if not dry_run:
                            _account_link_progress(rows, receipt, row, name)
                        continue
                    allowed = root is None or Path(target) == main / name or root in Path(target).resolve().parents
                    if not allowed:
                        print("warn: kept changed or unmanaged Codex account entry: " + str(home / name))
                        kept["outside cleanup targets"] += 1
                        pending.discard(name)
                        continue
                    if dry_run:
                        if _unlink_account_entry(home, descriptor, row, name, target, recorded, True):
                            print("would remove Codex account link: " + name)
                            removed_count += 1
                            remaining.discard(name)
                        else:
                            kept["unlink refused"] += 1
                        pending.discard(name)
                        continue
                    # Retire authority before unlink so a crash cannot authorize
                    # deleting a later replacement at this name.
                    _account_link_progress(rows, receipt, row, name)
                    removed = False
                    try:
                        removed = _unlink_account_entry(home, descriptor, row, name, target, recorded)
                        if removed:
                            print("removed Codex account link: " + name)
                            removed_count += 1
                    except OSError as error:
                        print("warn: kept Codex account entry %s: %s" % (home / name, error))
                    finally:
                        if not removed and _recorded_account_link(home, descriptor, row, name, target, recorded) is not None:
                            _account_link_progress(rows, receipt, row, name, target)
                    if not removed:
                        kept["unlink refused"] += 1
                    pending.discard(name)
        except (OSError, ValueError) as error:
            if ident is not None and not dry_run and _retire_stale_account_home(rows, receipt, row):
                rows_kept -= 1
                continue
            print("warn: kept Codex account home %s: directory identity or safety check failed: %s" % (home, error))
            kept["directory identity or safety check failed"] += len(pending)
            continue
        if dry_run and not remaining:
            rows_kept -= 1
        if not dry_run and not row["links"]:
            rows.remove(row)
            atomic_write_json(receipt, rows)
            rows_kept -= 1
    if root is not None:
        _report_account_cleanup(removed_count, kept, rows_kept, dry_run)


# Registration is independent of the legacy routing capability table above.
REGISTERED_ACCOUNT_PROVIDERS = frozenset(("codex", "claude"))
CLAUDE_SHARED_ENTRIES = {"CLAUDE.md": "rules/global-AGENTS.md",
                         "skills": "custom-skills",
                         "output-styles/oms-korean.md": "output-styles/oms-korean.md"}


def _claude_paths():
    parent = _account_paths()[0].parent
    return parent / "provider-accounts.claude.json", parent / "provider-account-effects.json"


def _private_state(path, fallback):
    try:
        descriptor = os.open(str(path), os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        return fallback
    with os.fdopen(descriptor, "rb") as stream:
        info = os.fstat(stream.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_mode & 0o022):
            raise ValueError("account state must be a user-owned regular non-link file: " + str(path))
        try:
            return json.load(stream, object_pairs_hook=_unique_fields,
                             parse_constant=_invalid_account_constant)
        except (ValueError, RecursionError) as error:
            raise ValueError("malformed account state; left unchanged: " + str(path)) from error


def _valid_ident(ident):
    return isinstance(ident, str) and re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", ident) is not None


def _physical_path(value):
    return (isinstance(value, str) and bool(value) and "\0" not in value
            and Path(value).is_absolute() and str(Path(value).resolve()) == value)


def _claude_registry():
    path, _ = _claude_paths()
    value = _private_state(path, {"schema": 1, "provider": "claude", "accounts": []})
    if (not isinstance(value, dict) or set(value) != {"schema", "provider", "accounts"}
            or type(value["schema"]) is not int or value["schema"] != 1
            or value["provider"] != "claude" or not isinstance(value["accounts"], list)):
        raise ValueError("invalid claude account registry; restore provider-accounts.claude.json")
    ids, homes = set(), set()
    for row in value["accounts"]:
        if (not isinstance(row, dict) or set(row) != {"id", "home"}
                or not _valid_ident(row["id"]) or row["id"] == "default"
                or not _physical_path(row["home"])):
            raise ValueError("invalid claude account entry")
        if row["id"] in ids or row["home"] in homes:
            raise ValueError("duplicate claude account id or canonical home")
        ids.add(row["id"])
        homes.add(row["home"])
    return value


def _identity(info):
    return {"st_uid": info.st_uid, "st_dev": info.st_dev, "st_ino": info.st_ino}


def _valid_identity(value):
    return (isinstance(value, dict) and set(value) == {"st_uid", "st_dev", "st_ino"}
            and all(type(n) is int and n >= 0 for n in value.values()))


def _source_root(root):
    if not isinstance(root, (str, Path)):
        raise ValueError("invalid Claude account source root")
    root = Path(root)
    if (not _physical_path(str(root)) or not (root / "scripts/install-claude-hooks.sh").is_file()
            or not (root / "rules/global-AGENTS.md").is_file()):
        raise ValueError("unverified Claude account source root; restore its OMS checkout")
    return root


def _claude_effects():
    _, path = _claude_paths()
    value = _private_state(path, {"schema": 1, "accounts": []})
    if (not isinstance(value, dict) or set(value) != {"schema", "accounts"}
            or type(value["schema"]) is not int or value["schema"] != 1
            or not isinstance(value["accounts"], list)):
        raise ValueError("invalid Claude account effects; preserve state and restore its receipt")
    homes = set()
    for row in value["accounts"]:
        keys = {"provider", "id", "home", "parent", "parent_identity", "identity", "source_root",
                "settings", "hooks_receipt", "status", "links"}
        if (not isinstance(row, dict) or set(row) != keys or row["provider"] != "claude"
                or not _valid_ident(row["id"]) or row["id"] == "default"
                or not _physical_path(row["home"]) or row["home"] in homes
                or not _physical_path(row["parent"]) or not _valid_identity(row["parent_identity"])
                or (row["identity"] is not None and not _valid_identity(row["identity"]))
                or row["status"] not in ("pending", "installed", "removing")
                or row["settings"] != str(Path(row["home"]) / "settings.json")
                or row["hooks_receipt"] != str(Path(row["home"]) / ".oms-claude-hooks.json")
                or not isinstance(row["links"], dict)):
            raise ValueError("invalid Claude account effects; preserve state for recovery")
        if row["status"] == "installed" and row["identity"] is None:
            raise ValueError("installed Claude effects require a bound home identity")
        source = _source_root(row["source_root"])
        if Path(row["parent"]) not in Path(row["home"]).parents:
            raise ValueError("invalid Claude reservation parent")
        for name, link in row["links"].items():
            if (name not in CLAUDE_SHARED_ENTRIES or not isinstance(link, dict)
                    or set(link) != {"target", "identity", "status"}
                    or not isinstance(link["target"], str)
                    or not _physical_path(link["target"])
                    or link["status"] not in ("pending", "installed", "removing")
                    or (link["identity"] is not None and not _valid_identity(link["identity"]))):
                raise ValueError("invalid Claude account link ownership")
            if row["status"] == "installed" and (link["identity"] is None or link["status"] != "installed"):
                raise ValueError("installed Claude effects require finalized link identities")
            target = Path(link["target"])
            link_root = target
            for _ in Path(CLAUDE_SHARED_ENTRIES[name]).parts:
                link_root = link_root.parent
            if target != _source_root(str(link_root)) / CLAUDE_SHARED_ENTRIES[name]:
                raise ValueError("unverified Claude account link source")
        homes.add(row["home"])
    return value


def _registered_locked(legacy=None):
    legacy = load_accounts() if legacy is None else legacy
    if "claude" in legacy["providers"]:
        raise ValueError("legacy Claude accounts in provider-accounts.json are unsupported; preserve the file and restore its Codex-only configuration")
    claude = _claude_registry()
    effects = _claude_effects()
    for row in effects["accounts"]:
        if row["identity"] is not None:
            _hook_receipt(Path(row["settings"]), Path(row["hooks_receipt"]), row["source_root"], binding=row)
    providers = dict(legacy["providers"])
    providers["claude"] = {"policy": "ordered", "accounts": claude["accounts"]}
    homes = {}
    for provider, settings in providers.items():
        for row in settings["accounts"]:
            home = str(Path(row["home"]).resolve())
            key = (provider, row["id"])
            if home in homes and homes[home] != key:
                raise ValueError("duplicate provider canonical home; preserve both states and choose separate homes")
            homes[home] = key
    reservations = list(effects["accounts"])
    for row in _link_receipt(_account_paths()[1]):
        reservations.append(dict(row, provider="codex"))
    for row in reservations:
        key = (row["provider"], row["id"])
        home = str(Path(row["home"]).resolve())
        if home in homes and homes[home] != key:
            raise ValueError("account home reserved by pending effects; repair or remove that account")
        homes[home] = key
    return {"schema": 1, "configured": bool(legacy.get("configured") or claude["accounts"]),
            "providers": providers, "configured_providers": {
                "codex": legacy.get("configured", bool(legacy["providers"])),
                "claude": bool(claude["accounts"])}, "reservations": reservations}


def load_registered_accounts():
    """Consistent lifecycle snapshot; passive Codex readers keep load_accounts."""
    from oms_runtime.common import file_lock
    with file_lock(_account_paths()[0]):
        return _registered_locked()


def _account_publish(path, value):
    from oms_runtime.common import atomic_write_json
    _account_parent_private(path.parent)
    missing = []
    parent = path.parent
    while not parent.exists():
        missing.append(parent)
        parent = parent.parent
    for parent in reversed(missing):
        parent.mkdir(mode=0o700)
    _account_home_private(path.parent, path.parent.stat())
    if _private_state(path, None) != value:
        atomic_write_json(path, value)


def _claude_main():
    return Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude").expanduser().resolve()


def _claude_collision(view, home, ident, provider):
    for other, settings in view["providers"].items():
        for entry in settings["accounts"]:
            if (other, entry["id"]) != (provider, ident) and Path(entry["home"]).resolve() == home:
                raise ValueError("duplicate provider canonical home")
    for row in view["reservations"]:
        if (row["provider"], row["id"]) != (provider, ident) and Path(row["home"]).resolve() == home:
            raise ValueError("account home reserved by pending Claude effects")
    if home in (_claude_main(), Path(default_account("codex")["home"])):
        raise ValueError("account home must differ from provider main homes")


def _effect_home(row):
    parent = Path(row["parent"])
    if _identity(parent.stat()) != row["parent_identity"]:
        raise ValueError("Claude reservation parent identity changed; preserve effects for recovery")
    _account_parent_private(parent)
    home = Path(row["home"])
    if row["identity"] is None:
        raise ValueError("Claude home creation interrupted; preserve the home and reservation for recovery")
    info = home.lstat()
    if not stat.S_ISDIR(info.st_mode) or _identity(info) != row["identity"]:
        raise ValueError("Claude account directory identity changed; preserve effects for recovery")
    _account_home_private(home, info)
    return home


def _claude_link_parent(row, name, create=False):
    home = _effect_home(row)
    parent = (home / name).parent
    if parent != home:
        if not os.path.lexists(str(parent)) and create:
            with _account_directory(home, row["identity"]) as (descriptor, _):
                if descriptor is None or os.mkdir not in os.supports_dir_fd:
                    raise ValueError("platform lacks safe Claude integration directory creation")
                os.mkdir(parent.name, mode=0o700, dir_fd=descriptor)
        info = parent.lstat()
        if not stat.S_ISDIR(info.st_mode):
            raise ValueError("Claude account integration parent must not be a link")
        _account_home_private(parent, info)
    return parent


def _claude_link_apply(row, effects, repair, source_root=None):
    _, path = _claude_paths()
    source = _source_root(source_root or row["source_root"])
    for name, relative in CLAUDE_SHARED_ENTRIES.items():
        parent = _claude_link_parent(row, name, create=True)
        link = parent / Path(name).name
        target = str(source / relative)
        recorded = row["links"].get(name)
        current = link.lstat() if os.path.lexists(str(link)) else None
        if recorded and current is not None:
            if (recorded["identity"] is None or _identity(current) != recorded["identity"]
                    or not stat.S_ISLNK(current.st_mode) or os.readlink(str(link)) != recorded["target"]):
                raise ValueError("changed or ambiguous Claude account link; preserve it and restore recorded ownership: " + name)
            if recorded["target"] == target and recorded["status"] == "installed":
                continue
        elif current is not None:
            print("warn: kept existing unmanaged Claude account entry " + name)
            continue
        row["status"] = "pending"
        if current is not None:
            if not repair:
                raise ValueError("Claude account links need --repair")
            recorded["status"] = "removing"
            _account_publish(path, effects)
            with _account_directory(parent) as (descriptor, _):
                if not _unlink_account_entry(parent, descriptor, None, link.name,
                                             recorded["target"], current):
                    raise ValueError("Claude account link removal refused")
        row["links"][name] = {"target": target, "identity": None, "status": "pending"}
        _account_publish(path, effects)
        with _account_directory(parent) as (descriptor, _):
            _effect_home(row)
            if descriptor is None or not _ACCOUNT_FD_SYMLINK:
                raise ValueError("platform lacks safe Claude account link creation")
            os.symlink(target, link.name, dir_fd=descriptor)
            info = _account_entry(parent, descriptor, link.name)
            row["links"][name].update(identity=_identity(info), status="installed")
            _account_publish(path, effects)


def _claude_hooks(row, remove=False, source_root=None):
    import subprocess
    _effect_home(row)
    root = _source_root(source_root or row["source_root"])
    command = ["bash", str(root / "scripts/install-claude-hooks.sh"), "--settings", row["settings"],
               "--ownership-receipt", row["hooks_receipt"]]
    if remove:
        command.append("--remove")
    # Keep the caller's HOME and lock domain. Account mode probes no provider.
    result = subprocess.run(command, stdin=subprocess.DEVNULL, check=False,
                            env=dict(os.environ, OMS_ACCOUNT_REGISTRY_LOCK_HELD="1"))
    if result.returncode:
        raise ValueError("Claude account settings ownership pending; restore the sidecar/settings binding and retry --repair or --remove")


def _claude_remove_row(row, effects, dry_run=False):
    _, path = _claude_paths()
    if row["identity"] is None and not os.path.lexists(row["home"]):
        parent = Path(row["parent"])
        if _identity(parent.stat()) != row["parent_identity"] or row["links"]:
            raise ValueError("Claude pending reservation identity changed; preserve it for recovery")
        _account_parent_private(parent)
        print("Claude pending reservation has no applied effects: " + row["id"])
        return
    home = _effect_home(row)
    if dry_run:
        _hook_receipt(Path(row["settings"]), Path(row["hooks_receipt"]), row["source_root"])
        print("would remove owned Claude account integrations: " + row["id"])
        return
    row["status"] = "removing"
    _account_publish(path, effects)
    _claude_hooks(row, remove=True)
    for name, recorded in list(row["links"].items()):
        parent = _claude_link_parent(row, name)
        link = parent / Path(name).name
        with _account_directory(parent) as (descriptor, _):
            info = _account_entry(parent, descriptor, link.name)
            if info is not None:
                if (recorded["identity"] is None or _identity(info) != recorded["identity"]
                        or not stat.S_ISLNK(info.st_mode) or os.readlink(str(link)) != recorded["target"]):
                    raise ValueError("changed Claude account link kept; restore it before --remove: " + name)
                recorded["status"] = "removing"
                _account_publish(path, effects)
                _effect_home(row)
                if not _unlink_account_entry(parent, descriptor, None, link.name, recorded["target"], info):
                    raise ValueError("Claude account cleanup refused; reservation kept")
            del row["links"][name]
            _account_publish(path, effects)
    print("Claude account integrations removed: " + row["id"] + "; home, login and sessions kept")


def _verify_claude(row):
    import subprocess
    _effect_home(row)
    if any(name.startswith(("ANTHROPIC_", "CLAUDE_CODE_USE_", "CLAUDE_CODE_OAUTH_"))
           for name in os.environ):
        raise ValueError("Claude verification refuses global auth environment overrides")
    # Only diagnostic process plumbing is inherited; no caller auth overrides.
    names = ("HOME", "PATH", "LANG", "LC_ALL", "TMPDIR", "SYSTEMROOT")
    env = {name: os.environ[name] for name in names if name in os.environ}
    env["CLAUDE_CONFIG_DIR"] = row["home"]
    try:
        result = subprocess.run(["claude", "auth", "status"], env=env, stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=15, check=False)
        status = json.loads(result.stdout, object_pairs_hook=_unique_fields,
                            parse_constant=_invalid_account_constant)
        good = (result.returncode == 0 and isinstance(status, dict)
                and status.get("authMethod") == "claude.ai"
                and isinstance(status.get("configDirectory"), str)
                and Path(status["configDirectory"]).is_absolute()
                and Path(status["configDirectory"]).resolve() == Path(row["home"]))
    except (OSError, ValueError, RecursionError, subprocess.TimeoutExpired):
        good = False
    if not good:
        raise ValueError("Claude account status unverified; independent authentication isolation is unsupported")
    raise ValueError("Claude status selected the requested config, but credential origin is not attested; isolation verification unsupported")


def _create_claude_home(row, effects):
    from contextlib import ExitStack
    parent, home = Path(row["parent"]), Path(row["home"])
    with ExitStack() as stack:
        if not _ACCOUNT_SAFE_UNLINK or os.mkdir not in os.supports_dir_fd or os.open not in os.supports_dir_fd:
            raise ValueError("platform lacks safe Claude account directory creation")
        _account_parent_private(parent)
        descriptor = os.open(str(parent), os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        stack.callback(os.close, descriptor)
        info = _verify_account_directory(parent, descriptor, row["parent_identity"])
        current = parent
        for name in home.relative_to(parent).parts:
            _verify_account_directory(current, descriptor, _identity(info))
            os.mkdir(name, mode=0o700, dir_fd=descriptor)
            descriptor = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=descriptor)
            stack.callback(os.close, descriptor)
            current = current / name
            info = _verify_account_directory(current, descriptor, None)
            _account_home_private(current, info)
        row["identity"] = _identity(info)
        _account_publish(_claude_paths()[1], effects)


def connect_claude_account(args):
    from oms_runtime.common import file_lock
    ident = args.account.split(":", 1)[1]
    if not _valid_ident(ident) or ident == "default":
        raise ValueError("invalid Claude account id; default is reserved for the main config")
    registry, effects_path = _claude_paths()
    with file_lock(_account_paths()[0]):
        view = _registered_locked(_registration_config(_account_paths()[0]))
        value, effects = _claude_registry(), _claude_effects()
        existing = next((entry for entry in value["accounts"] if entry["id"] == ident), None)
        owned = [row for row in effects["accounts"] if row["id"] == ident]
        if existing and not any(row["home"] == existing["home"] for row in owned):
            raise ValueError("Claude account ownership missing; preserve registry and restore effects for recovery")
        if args.verify:
            row = next((row for row in owned if existing and row["home"] == existing["home"]), None)
            if row is None or row["status"] != "installed":
                raise ValueError("Claude account is not registered and installed: " + ident)
            _verify_claude(row)
            return
        if args.remove:
            for row in owned:
                _claude_remove_row(row, effects)
            if existing:
                value["accounts"].remove(existing)
                _account_publish(registry, value)
            effects["accounts"][:] = [row for row in effects["accounts"] if row not in owned]
            if owned:
                _account_publish(effects_path, effects)
            return
        raw = args.home if args.home is not None else (existing["home"] if existing else str(_claude_main() / "accounts" / ident))
        if not raw or "\0" in raw:
            raise ValueError("Claude account home must not be empty")
        home = Path(raw).expanduser().resolve()
        _claude_collision(view, home, ident, "claude")
        _outside_repository(home)
        _account_parent_private(home.parent)
        row = next((row for row in owned if row["home"] == str(home)), None)
        if row is None:
            if home.exists():
                _account_home_private(home, home.lstat())
                if not home.is_dir():
                    raise ValueError("Claude account home must be a directory")
            parent = home.parent
            while not parent.exists():
                parent = parent.parent
            root = _source_root(str(Path(__file__).resolve().parents[2]))
            row = {"provider": "claude", "id": ident, "home": str(home), "parent": str(parent),
                   "parent_identity": _identity(parent.stat()), "identity": _identity(home.stat()) if home.exists() else None,
                   "source_root": str(root), "settings": str(home / "settings.json"),
                   "hooks_receipt": str(home / ".oms-claude-hooks.json"), "status": "pending", "links": {}}
            effects["accounts"].append(row)
            _account_publish(effects_path, effects)
            if row["identity"] is None:
                _create_claude_home(row, effects)
        _effect_home(row)
        for old in owned:
            if old is not row:
                _claude_remove_row(old, effects)
                # Old reservation remains until the new registry row is published.
        current_root = str(_source_root(str(Path(__file__).resolve().parents[2])))
        _claude_link_apply(row, effects, args.repair, source_root=current_root)
        _claude_hooks(row, source_root=current_root)
        row["source_root"] = current_root
        entry = {"id": ident, "home": str(home)}
        value["accounts"][:] = [entry for entry in value["accounts"] if entry["id"] != ident]
        value["accounts"].insert(0, entry) if args.first else value["accounts"].append(entry)
        _account_publish(registry, value)
        row["status"] = "installed"
        effects["accounts"][:] = [item for item in effects["accounts"] if item["id"] != ident or item is row]
        _account_publish(effects_path, effects)
        import shlex
        print("Claude account " + ident + ": registered, login pending; registration does not verify authentication")
        print("CLAUDE_CONFIG_DIR=" + shlex.quote(str(home)) + " claude auth login")


def _settings_snapshot(path):
    import hashlib
    try:
        descriptor = os.open(str(path), os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        return None, None, {}
    with os.fdopen(descriptor, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
            raise ValueError("Claude settings must be a user-owned regular non-link file")
        data = stream.read()
    value = json.loads(data, object_pairs_hook=_unique_fields, parse_constant=_invalid_account_constant)
    if not isinstance(value, dict):
        raise ValueError("Claude settings must be a JSON object")
    return _identity(info), hashlib.sha256(data).hexdigest(), value


def _hook_lib(root):
    import importlib.util
    spec = importlib.util.spec_from_file_location("oms_account_hooks", str(Path(root) / "scripts/lib/claude-hook-surfaces.py"))
    lib = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(lib)
    return lib


_LEGACY_ATTRIBUTION = {"commit": "", "pr": "", "sessionUrl": False}
_LEGACY_LEAVES = {("attribution",)}


def _managed_leaf_values(root):
    lib = _hook_lib(root)
    return {
        ("statusLine",): {"type": "command", "command": lib.entry_command(root, "python3", str(Path(root) / "scripts/claude-statusline.py"))},
        ("subagentStatusLine",): {"type": "command", "command": lib.entry_command(root, "python3", str(Path(root) / "scripts/claude-subagent-statusline.py"))},
        ("modelSettings", "claude-opus-5-5", "effortLevel"): "high",
        # Legacy: OMS no longer writes it, but receipts from older installs
        # record it as an owned leaf and must still validate and be removable.
        ("attribution",): dict(_LEGACY_ATTRIBUTION),
    }


def _hook_receipt(settings, receipt, root, binding=None):
    home = settings.parent
    if (settings.name != "settings.json" or receipt != home / ".oms-claude-hooks.json"
            or home.resolve() != home):
        raise ValueError("account hook receipt must be beside its physical settings.json")
    if binding is None:
        _account_home_private(home, home.lstat())
        identity, digest, value = _settings_snapshot(settings)
    else:
        identity, digest, value = None, None, {}
    row = _private_state(receipt, None)
    if row is None:
        return None, identity, digest, value
    keys = {"schema", "settings", "home_identity", "source_root", "hooks_schema", "status",
            "settings_identity", "settings_digest", "leaves", "hooks", "containers"}
    if (not isinstance(row, dict) or set(row) != keys or type(row["schema"]) is not int or row["schema"] != 1
            or row["settings"] != str(settings) or not _valid_identity(row["home_identity"])
            or row["home_identity"] != (binding["identity"] if binding else _identity(home.lstat()))
            or type(row["hooks_schema"]) is not int or row["hooks_schema"] < 1
            or row["status"] not in ("pending", "installed", "removed")
            or (row["settings_identity"] is not None and not _valid_identity(row["settings_identity"]))
            or (row["settings_digest"] is not None and (not isinstance(row["settings_digest"], str)
                or not re.fullmatch(r"[0-9a-f]{64}", row["settings_digest"])))
            or not isinstance(row["leaves"], list) or not isinstance(row["hooks"], list)
            or not isinstance(row["containers"], list)):
        raise ValueError("invalid account hooks ownership receipt; preserve settings for recovery")
    for parts in row["containers"]:
        if parts not in (["hooks"], ["modelSettings"], ["modelSettings", "claude-opus-5-5"]):
            raise ValueError("invalid account settings container ownership")
    source = str(_source_root(row["source_root"]))
    allowed = _managed_leaf_values(source)
    for leaf in row["leaves"]:
        if (not isinstance(leaf, dict) or set(leaf) != {"path", "value"} or not isinstance(leaf["path"], list)
                or any(not isinstance(part, str) for part in leaf["path"])
                or tuple(leaf["path"]) not in allowed or leaf["value"] != allowed[tuple(leaf["path"])]):
            raise ValueError("invalid account hook leaf ownership")
    lib = _hook_lib(source)
    for hook in row["hooks"]:
        if (not isinstance(hook, dict) or set(hook) != {"event", "entry"} or not isinstance(hook["event"], str)
                or not isinstance(hook["entry"], dict) or set(hook["entry"]) - {"hooks", "matcher"}
                or not isinstance(hook["entry"].get("hooks"), list) or len(hook["entry"]["hooks"]) != 1
                or lib.owned_script(hook["entry"]["hooks"][0], [source]) is None):
            raise ValueError("invalid account hook command ownership")
        hook_value = hook["entry"]["hooks"][0]
        script = lib.owned_script(hook_value, [source])
        if (set(hook_value) - {"type", "command", "timeout"} or hook_value.get("type") != "command"
                or hook_value.get("command") != lib.hook_command(source, script)
                or ("timeout" in hook_value and (type(hook_value["timeout"]) is not int or hook_value["timeout"] < 1))):
            raise ValueError("unverified account hook command identity")
    if binding is None and row["status"] == "pending":
        raise ValueError("interrupted account settings write (pending); ownership ambiguous, preserve settings and sidecar for recovery")
    if binding is None and row["settings_identity"] != identity:
        raise ValueError("Claude settings identity changed; preserve settings and sidecar for recovery")
    return row, identity, digest, value


def _leaf_get(settings, parts):
    cursor = settings
    for part in parts:
        if not isinstance(cursor, dict) or part not in cursor:
            return False, None
        cursor = cursor[part]
    return True, cursor


def _leaf_set(settings, parts, value, remove=False):
    cursor = settings
    for part in parts[:-1]:
        if part not in cursor:
            if remove:
                return False
            cursor[part] = {}
        if not isinstance(cursor[part], dict):
            return False
        cursor = cursor[part]
    if remove:
        cursor.pop(parts[-1], None)
    else:
        cursor[parts[-1]] = value
    return True


def validate_account_hook_reservation(settings, receipt):
    settings = Path(os.path.abspath(settings))
    receipt = Path(os.path.abspath(receipt))
    view = _registered_locked(_registration_config(_account_paths()[0]))
    row = next((row for row in view["reservations"] if row["provider"] == "claude" and row["settings"] == str(settings)
                and row["hooks_receipt"] == str(receipt)), None)
    if row is None:
        raise ValueError("account hooks require an existing validated Claude reservation")
    _effect_home(row)


def run_account_hook_installer(arguments):
    import subprocess
    from oms_runtime.common import file_lock
    with file_lock(_account_paths()[0]):
        root = Path(__file__).resolve().parents[2]
        return subprocess.call(["bash", str(root / "scripts/install-claude-hooks.sh")] + arguments,
                               env=dict(os.environ, OMS_ACCOUNT_REGISTRY_LOCK_HELD="1"))


def _write_account_settings(path, data, identity, digest, home_identity):
    import hashlib
    import uuid
    with _account_directory(path.parent, home_identity) as (descriptor, _):
        if descriptor is None:
            raise ValueError("platform lacks safe account settings replacement")
        name = ".oms-settings-" + uuid.uuid4().hex
        output = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=descriptor)
        try:
            with os.fdopen(output, "wb") as stream:
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
                written = _identity(os.fstat(stream.fileno()))
            _verify_account_directory(path.parent, descriptor, home_identity)
            if _settings_snapshot(path)[:2] != (identity, digest):
                raise ValueError("concurrent Claude settings edit; pending receipt kept for recovery")
            os.replace(name, path.name, src_dir_fd=descriptor, dst_dir_fd=descriptor)
            try:
                os.fsync(descriptor)
            except OSError:
                pass
            current, current_digest, _ = _settings_snapshot(path)
            if current != written or current_digest != hashlib.sha256(data).hexdigest():
                raise ValueError("Claude settings replaced after write; pending ownership kept for recovery")
            return current, current_digest
        finally:
            try:
                os.unlink(name, dir_fd=descriptor)
            except FileNotFoundError:
                pass


def account_hook_settings(settings, receipt, root, manifest, remove=False):
    """Called under the install lock; the registry lock is acquired by the caller.

    Pending receipts deliberately require recovery after a cross-file crash:
    matching bytes cannot prove who wrote a previously unowned value.
    """
    import copy
    settings, receipt = Path(os.path.abspath(settings)), Path(os.path.abspath(receipt))
    root = str(_source_root(root))
    old, identity, digest, current = _hook_receipt(settings, receipt, root)
    updated = copy.deepcopy(current)
    wanted = _managed_leaf_values(root)
    owned_leaves, owned_hooks = [], []
    containers = {tuple(parts) for parts in old["containers"]} if old else set()
    original = copy.deepcopy(current)
    old_leaves = {tuple(row["path"]): row["value"] for row in old["leaves"]} if old else {}
    for parts, value in wanted.items():
        exists, actual = _leaf_get(updated, parts)
        if parts in old_leaves:
            if not exists:
                continue
            if actual != old_leaves[parts]:
                print("recovery: kept user-modified Claude setting " + ".".join(parts))
                continue
            # A legacy leaf is dropped on update/repair too, never re-owned.
            legacy = parts in _LEGACY_LEAVES
            _leaf_set(updated, parts, value, remove=remove or legacy)
            if not remove and not legacy:
                owned_leaves.append({"path": list(parts), "value": value})
        elif parts in _LEGACY_LEAVES:
            continue
        elif not remove and not exists and _leaf_set(updated, parts, value):
            owned_leaves.append({"path": list(parts), "value": value})
            for length in range(1, len(parts)):
                if not _leaf_get(original, parts[:length])[0]:
                    containers.add(parts[:length])
    hooks = updated.get("hooks", {})
    if not isinstance(hooks, dict):
        raise ValueError("Claude settings hooks must be an object")
    if any(not isinstance(entries, list) for entries in hooks.values()):
        raise ValueError("Claude hook events must be lists")
    # Remove only the single recorded hook in its recorded matcher. User hook
    # siblings and entry metadata survive; ambiguous duplicates stay untouched.
    for row in old["hooks"] if old else []:
        event, entry = row["event"], row["entry"]
        entries = hooks.get(event, [])
        matches = [(candidate, i) for candidate in entries if isinstance(candidate, dict)
                   and candidate.get("matcher") == entry.get("matcher")
                   and isinstance(candidate.get("hooks"), list)
                   for i, hook in enumerate(candidate["hooks"]) if hook == entry["hooks"][0]]
        if len(matches) != 1:
            print("recovery: kept changed or ambiguous Claude hook " + event)
            continue
        candidate, index = matches[0]
        del candidate["hooks"][index]
        if not candidate["hooks"] and not set(candidate) - {"hooks", "matcher"}:
            entries.remove(candidate)
        if not entries:
            hooks.pop(event, None)
    if not remove:
        # Converge only newly constructed rows through the existing merger.
        # Preexisting user registrations, including identical OMS commands,
        # remain outside this account's deletion authority.
        lib = _hook_lib(root)
        desired = {}
        lib.converge(desired, manifest["surfaces"], root, [root])
        for event, entries in desired.items():
            current_entries = hooks.setdefault(event, [])
            if not isinstance(current_entries, list):
                raise ValueError("Claude hooks event must be a list: " + event)
            for entry in entries:
                if any(isinstance(candidate, dict) and candidate.get("matcher") == entry.get("matcher")
                       and isinstance(candidate.get("hooks"), list)
                       and entry["hooks"][0] in candidate["hooks"] for candidate in current_entries):
                    continue
                current_entries.append(entry)
                owned_hooks.append({"event": event, "entry": entry})
    if hooks or "hooks" in current:
        updated["hooks"] = hooks
        if not remove and "hooks" not in original:
            containers.add(("hooks",))
    if remove:
        for parts in sorted(containers, key=len, reverse=True):
            exists, value = _leaf_get(updated, parts)
            if exists and value == {}:
                _leaf_set(updated, parts, None, remove=True)
    if remove and old is None:
        print("claude-settings: no account-owned settings to remove")
        return
    row = {"schema": 1, "settings": str(settings), "home_identity": _identity(settings.parent.lstat()),
           "source_root": root, "hooks_schema": manifest["hooks_schema"], "status": "pending",
           "settings_identity": identity, "settings_digest": digest, "leaves": owned_leaves, "hooks": owned_hooks,
           "containers": [] if remove else [list(parts) for parts in sorted(containers)]}
    if old and old["status"] == ("removed" if remove else "installed") and updated == current:
        stable = dict(row, status=old["status"])
        if stable == old:
            print("claude-settings: already current (account ownership verified)")
            return
    if _settings_snapshot(settings)[:2] != (identity, digest):
        raise ValueError("concurrent Claude settings edit; retry after inspecting user changes")
    _account_publish(receipt, row)
    # Intent is durable before the settings write. There is no backup rollback.
    if _identity(settings.parent.lstat()) != row["home_identity"]:
        raise ValueError("Claude settings parent changed; preserve pending receipt")
    if _settings_snapshot(settings)[:2] != (identity, digest):
        raise ValueError("concurrent Claude settings edit; pending receipt kept for recovery")
    if updated != current:
        data = (json.dumps(updated, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
        identity, digest = _write_account_settings(settings, data, identity, digest, row["home_identity"])
    elif _settings_snapshot(settings)[:2] != (identity, digest):
        raise ValueError("concurrent Claude settings edit; pending ownership kept for recovery")
    row["settings_identity"], row["settings_digest"] = identity, digest
    row["status"] = "removed" if remove else "installed"
    _account_publish(receipt, row)
    print("claude-settings: " + row["status"] + " account-owned hooks and defaults")


def doctor_registered_accounts():
    doctor_codex_accounts()
    try:
        view = load_registered_accounts()
    except (OSError, ValueError, RuntimeError) as error:
        print("warn: claude account health unavailable: " + str(error))
        return
    entries = view["providers"]["claude"]["accounts"]
    for row in view["reservations"]:
        if row["provider"] != "claude":
            continue
        prefix = "claude account " + row["id"] + " "
        try:
            _effect_home(row)
            if row["status"] != "installed" or not any(entry["home"] == row["home"] for entry in entries):
                raise ValueError("pending effects; retry --repair or --remove")
            hook, _, digest, _ = _hook_receipt(Path(row["settings"]), Path(row["hooks_receipt"]), row["source_root"])
            if hook is None or hook["status"] != "installed":
                raise ValueError("settings ownership missing or stale")
            if digest != hook["settings_digest"] or row["source_root"] != str(Path(__file__).resolve().parents[2]):
                raise ValueError("settings or source changed; inspect user edits and rerun --repair")
            source_text = (Path(row["source_root"]) / "scripts/install-claude-hooks.sh").read_text(encoding="utf-8")
            schema = re.search(r"^HOOKS_SCHEMA=([0-9]+)$", source_text, re.MULTILINE)
            if schema is None or hook["hooks_schema"] != int(schema.group(1)):
                raise ValueError("settings schema stale; inspect and rerun --repair")
            for name in CLAUDE_SHARED_ENTRIES:
                link = row["links"].get(name)
                if link is None:
                    print("warn: " + prefix + "unmanaged integration kept: " + name)
                    continue
                path = Path(row["home"]) / name
                if (_identity(path.lstat()) != link["identity"] or not path.is_symlink()
                        or not path.exists() or os.readlink(str(path)) != link["target"]):
                    raise ValueError("link identity or target changed: " + name)
            print("ok: " + prefix + "integrations installed; login unverified (explicit --verify only)")
        except (OSError, ValueError, RuntimeError) as error:
            print("warn: " + prefix + str(error))
            print("recovery: oms connect-services --account claude:" + row["id"] + " --repair or --remove; preserve ambiguous user data")
    for entry in entries:
        if not any(row["provider"] == "claude" and row["home"] == entry["home"] for row in view["reservations"]):
            print("warn: claude account " + entry["id"] + " ownership missing; preserve its config for recovery")


def refresh_registered_accounts():
    from oms_runtime.common import file_lock
    with file_lock(_account_paths()[0]):
        view = _registered_locked(_registration_config(_account_paths()[0]))
        effects = _claude_effects()
        if any(not any(row["home"] == entry["home"] for row in effects["accounts"])
               for entry in view["providers"]["claude"]["accounts"]):
            raise ValueError("Claude account ownership missing; preserve registry and restore effects for recovery")
        current = str(Path(__file__).resolve().parents[2])
        for row in effects["accounts"]:
            if row["status"] != "installed":
                raise ValueError("Claude account refresh pending; repair or remove " + row["id"])
            _effect_home(row)
            row["status"] = "pending"
            _account_publish(_claude_paths()[1], effects)
            _claude_link_apply(row, effects, True, source_root=current)
            _claude_hooks(row, source_root=current)
            row["source_root"] = current
            row["status"] = "installed"
            _account_publish(_claude_paths()[1], effects)


def cleanup_registered_accounts(root, dry_run=False):
    from oms_runtime.common import file_lock
    with file_lock(_account_paths()[0]):
        view = _registered_locked(_registration_config(_account_paths()[0]))
        effects = _claude_effects()
        if any(not any(row["home"] == entry["home"] for row in effects["accounts"])
               for entry in view["providers"]["claude"]["accounts"]):
            raise ValueError("Claude account ownership missing; uninstall stopped; restore effects for recovery")
        for row in effects["accounts"]:
            if row["identity"] is not None:
                _effect_home(row)
                _hook_receipt(Path(row["settings"]), Path(row["hooks_receipt"]), row["source_root"])
        for row in list(effects["accounts"]):
            _claude_remove_row(row, effects, dry_run)
            if not dry_run:
                # Keep the registry until cleanup succeeded, then release the
                # reservation last. An interrupted cleanup remains discoverable.
                value = _claude_registry()
                value["accounts"][:] = [entry for entry in value["accounts"] if entry["home"] != row["home"]]
                _account_publish(_claude_paths()[0], value)
                effects["accounts"].remove(row)
                _account_publish(_claude_paths()[1], effects)
        registry, receipt = _account_paths()
        if not os.path.lexists(str(receipt)):
            _report_account_cleanup(0, {}, 0, dry_run)
            return
        _drop_account_links(_link_receipt(receipt), receipt, root=root, dry_run=dry_run)
        if _link_receipt(receipt) and not dry_run:
            raise ValueError("Codex account cleanup incomplete; preserve the install and retry account recovery")


def connect_codex_account(args):
    from oms_runtime.common import atomic_write_json, file_lock
    ident = args.codex_account
    if ident == "default" and not getattr(args, "verify", False):
        raise ValueError("Codex account id default is reserved for the main home; choose another id")
    if not re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", ident):
        raise ValueError("invalid provider account id")
    registry, receipt = _account_paths()
    with file_lock(registry):
        value = _registration_config(registry)
        if not getattr(args, "verify", False):
            modern = _registered_locked(value)
        rows = _link_receipt(receipt)
        settings = value["providers"].get("codex")
        entries = settings["accounts"] if settings else []
        existing = next((row for row in entries if row["id"] == ident), None)
        if getattr(args, "verify", False):
            import subprocess
            if existing is None:
                raise ValueError("Codex account is not registered: " + ident)
            failed = False
            main = default_account("codex")
            verify_entries = list(entries)
            if not any(Path(entry["home"]).resolve() == Path(main["home"]) for entry in entries):
                main["id"] = "main"
                verify_entries.insert(0, main)
            for entry in verify_entries:
                error = account_home_error(entry)
                if error:
                    failed = True
                    print("warn: codex account " + entry["id"] + " home unavailable")
                    continue
                env = dict(os.environ, CODEX_HOME=entry["home"])
                try:
                    result = subprocess.run(["codex", "login", "status"], env=env,
                                            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                            stderr=subprocess.DEVNULL, timeout=15, check=False)
                    logged_in = result.returncode == 0
                except (OSError, subprocess.TimeoutExpired):
                    logged_in = False
                print(("ok: " if logged_in else "warn: ") + "codex account " + entry["id"] +
                      (" logged in" if logged_in else " login unverified"))
                failed |= not logged_in
            if failed:
                raise ValueError("Codex account login verification failed")
            return
        if args.remove:
            _drop_account_links(rows, receipt, ident=ident)
            if existing:
                entries.remove(existing)
                if not entries:
                    del value["providers"]["codex"]
                atomic_write_json(registry, value)
            print("Codex account removed: " + ident + "; home, login and sessions kept")
            return
        raw_home = args.home if args.home is not None else (
            existing["home"] if existing else str(Path(default_account("codex")["home"]) / "accounts" / ident))
        if not raw_home or "\0" in raw_home:
            raise ValueError("Codex account home must not be empty")
        home = Path(raw_home).expanduser().resolve()
        main = Path(default_account("codex")["home"])
        if home != main:
            _claude_collision(modern, home, ident, "codex")
        if home == main:
            raise ValueError("Codex account home must differ from the main home")
        _outside_repository(home)
        if home.exists() and not home.is_dir():
            raise ValueError("Codex account home must be a directory")
        for provider, other in value["providers"].items():
            for row in other["accounts"]:
                if provider == "codex" and row["id"] == ident:
                    continue
                if row["id"] == ident or os.path.normcase(str(Path(row["home"]).resolve())) == os.path.normcase(str(home)):
                    raise ValueError("duplicate provider account id or canonical home")
        owner = next((row for row in rows if row["id"] == ident and row["home"] == str(home)), None)
        if not _ACCOUNT_SAFE_UNLINK:
            print("skipped: platform lacks safe unlink: " + str(home))
        if owner is None:
            _account_parent_private(home.parent)
            missing = []
            parent = home.parent
            while not parent.exists():
                missing.append(parent)
                parent = parent.parent
            for parent in reversed(missing):
                parent.mkdir(mode=0o700, exist_ok=True)
            _account_parent_private(home.parent)
            home.mkdir(mode=0o700, exist_ok=True)
        with _account_directory(home, owner) as (descriptor, info):
            if owner is None:
                owner = {"id": ident, "home": str(home), "links": {}, "st_dev": info.st_dev, "st_ino": info.st_ino}
                rows.append(owner)
                atomic_write_json(receipt, rows)
            for name in CODEX_SHARED_ENTRIES:
                link, target = home / name, str(main / name)
                current = _account_entry(home, descriptor, name)
                if current is not None and stat.S_ISLNK(current.st_mode):
                    if _matching_account_link(link, main / name):
                        continue
                    if not args.repair:
                        print("warn: kept conflicting link " + name + "; rerun with --repair")
                        continue
                    try:
                        old_target = (os.readlink(name, dir_fd=descriptor) if descriptor is not None
                                      else os.readlink(str(link)))
                    except OSError:
                        print("warn: kept Codex account entry %s: entry type or target changed" % link)
                        continue
                    if not _unlink_account_entry(home, descriptor, owner, name, old_target, current):
                        continue
                elif current is not None:
                    print("warn: kept existing non-link " + name)
                    continue
                _verify_account_directory(home, descriptor, owner)
                # Intent binds to the open directory before any new link exists.
                owner["links"][name] = target
                atomic_write_json(receipt, rows)
                _verify_account_directory(home, descriptor, owner)
                if descriptor is not None and _ACCOUNT_FD_SYMLINK:
                    os.symlink(target, name, dir_fd=descriptor,
                               target_is_directory=name in ("plugins", "rules", "skills"))
                else:
                    link.symlink_to(target, target_is_directory=name in ("plugins", "rules", "skills"))
            _verify_account_directory(home, descriptor, owner)
        if settings is None:
            entries = [default_account("codex")]
            settings = {"policy": "ordered", "accounts": entries}
            value["providers"]["codex"] = settings
        entry = dict(existing or {}, id=ident, home=str(home))
        entries[:] = [row for row in entries if row["id"] != ident]
        if args.first:
            entries.insert(0, entry)
        else:
            entries.append(entry)
        atomic_write_json(registry, value)
        if (home / "auth.json").is_file():
            print("Codex account registered: " + ident)
        else:
            import shlex
            print("Codex account " + ident + ": registered, login pending")
            print("CODEX_HOME=" + shlex.quote(str(home)) + " codex login")


def doctor_codex_accounts():
    registry, receipt = _account_paths()
    value = _registration_config(registry)
    main = Path(default_account("codex")["home"])
    entries = value["providers"].get("codex", {}).get("accounts", [])
    homes = {os.path.normcase(str(Path(entry["home"]).resolve())) for entry in entries}
    for entry in entries:
        home = Path(entry["home"])
        prefix = "codex account " + entry["id"] + " "
        print(("ok: " if home.is_dir() else "warn: ") + prefix +
              ("home exists" if home.is_dir() else "home missing"))
        if not home.is_dir():
            _account_recovery(entry["id"])
        try:
            physical = home.resolve()
            _account_home_private(physical, physical.stat())
            print("ok: " + prefix + "ancestor chain safe")
        except (OSError, ValueError, RuntimeError) as error:
            print("warn: " + prefix + "home safety: " + str(error))
        for name in CODEX_SHARED_ENTRIES:
            link, source = home / name, main / name
            try:
                good = (home == main or link.is_symlink()) and source.exists() and link.resolve() == source.resolve()
            except (OSError, RuntimeError):
                good = False
            print(("ok: " if good else "warn: ") + prefix + "shared " + name +
                  (" resolves" if good else " missing or differs from main home"))
        for name in ("auth.json", "models_cache.json"):
            present = (home / name).is_file()
            print(("ok: " if present else "warn: ") + prefix + name + (" present" if present else " missing"))
    for row in _link_receipt(receipt):
        if os.path.normcase(str(Path(row["home"]).resolve())) not in homes:
            print("warn: codex account " + row["id"] + " receipt home is not registered")
            _account_recovery(row["id"])


def _account_command():
    import argparse
    from oms_runtime.common import CoreError, file_lock
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    connect = commands.add_parser("connect")
    connect.add_argument("--account", required=True)
    connect.add_argument("--home")
    order = connect.add_mutually_exclusive_group()
    order.add_argument("--first", action="store_true")
    order.add_argument("--last", action="store_true")
    action = connect.add_mutually_exclusive_group()
    action.add_argument("--repair", action="store_true")
    action.add_argument("--remove", action="store_true")
    action.add_argument("--verify", action="store_true")
    commands.add_parser("doctor")
    commands.add_parser("refresh")
    cleanup = commands.add_parser("cleanup")
    cleanup.add_argument("--root", type=Path, required=True)
    cleanup.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "connect":
            if args.account.count(":") != 1 or args.account.split(":")[0] not in REGISTERED_ACCOUNT_PROVIDERS:
                raise ValueError("account must have the form codex:ID or claude:ID; antigravity and other providers are unsupported")
            if args.verify and (args.home is not None or args.first or args.last):
                raise ValueError("--verify cannot be combined with --home, --first or --last")
            if args.account.startswith("claude:"):
                connect_claude_account(args)
            else:
                args.codex_account = args.account[len("codex:"):]
                connect_codex_account(args)
        elif args.command == "doctor":
            doctor_registered_accounts()
        elif args.command == "refresh":
            refresh_registered_accounts()
        else:
            cleanup_registered_accounts(args.root.resolve(), args.dry_run)
    except (OSError, ValueError, RuntimeError, CoreError) as error:
        if args.command == "doctor":
            print("warn: Codex account health unavailable: " + str(error))
            return 0
        print("error: " + str(error), file=__import__("sys").stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(_account_command())
