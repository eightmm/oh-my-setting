"""Machine-local provider account registration, health and read-only selection."""

from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import stat
import time


CODEX_SHARED_ENTRIES = ("AGENTS.md", "config.toml", "hooks.json", ".hook-trust",
                        "plugins", "rules", "skills")
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


def next_account_route(route, readings=None):
    """Select once from eligible accounts following this id in configuration order."""
    if route.get("provider") != "codex" or not route.get("account"):
        return None
    config = load_accounts()
    settings = config["providers"].get("codex") if config["configured"] else None
    if not settings:
        return None
    entries = settings["accounts"]
    index = next((n for n, entry in enumerate(entries) if entry["id"] == route["account"]), len(entries))
    remaining = entries[index + 1:]
    if not remaining:
        return None
    config = dict(config, providers={"codex": dict(settings, policy="ordered", accounts=remaining)})
    selected, reason = select_account("codex", route["model"], readings=readings, config=config)
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
    print("recovery: oms connect-services --codex-account %s --home %s" %
          (shlex.quote(row["id"]), shlex.quote(row["home"])))
    return True


def _account_recovery(ident="ID"):
    import shlex
    main = ident == "default"
    ident = "ID" if main else ident
    command = "oms connect-services --codex-account " + shlex.quote(ident)
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


def connect_codex_account(args):
    from oms_runtime.common import atomic_write_json, file_lock
    ident = args.codex_account
    if ident == "default":
        raise ValueError("Codex account id default is reserved for the main home; choose another id")
    if not re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", ident):
        raise ValueError("invalid provider account id")
    registry, receipt = _account_paths()
    with file_lock(registry):
        value = _registration_config(registry)
        rows = _link_receipt(receipt)
        settings = value["providers"].get("codex")
        entries = settings["accounts"] if settings else []
        existing = next((row for row in entries if row["id"] == ident), None)
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
            existing["home"] if existing else str(Path.home() / (".codex-" + ident)))
        if not raw_home or "\0" in raw_home:
            raise ValueError("Codex account home must not be empty")
        home = Path(raw_home).expanduser().resolve()
        main = Path(default_account("codex")["home"])
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
            home.mkdir(mode=0o700, parents=True, exist_ok=True)
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
    connect.add_argument("--codex-account", required=True)
    connect.add_argument("--home")
    order = connect.add_mutually_exclusive_group()
    order.add_argument("--first", action="store_true")
    order.add_argument("--last", action="store_true")
    action = connect.add_mutually_exclusive_group()
    action.add_argument("--repair", action="store_true")
    action.add_argument("--remove", action="store_true")
    commands.add_parser("doctor")
    cleanup = commands.add_parser("cleanup")
    cleanup.add_argument("--root", type=Path, required=True)
    cleanup.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "connect":
            connect_codex_account(args)
        elif args.command == "doctor":
            doctor_codex_accounts()
        else:
            registry, receipt = _account_paths()
            if not os.path.lexists(str(receipt)):
                _report_account_cleanup(0, {}, 0, args.dry_run)
                return 0
            if args.dry_run:
                _drop_account_links(_link_receipt(receipt), receipt, root=args.root.resolve(), dry_run=True)
            else:
                with file_lock(registry):
                    _drop_account_links(_link_receipt(receipt), receipt, root=args.root.resolve())
    except (OSError, ValueError, RuntimeError, CoreError) as error:
        if args.command == "doctor":
            print("warn: Codex account health unavailable: " + str(error))
            return 0
        print("error: " + str(error), file=__import__("sys").stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(_account_command())
