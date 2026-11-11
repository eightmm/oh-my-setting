#!/usr/bin/env python3
"""Read-only report of the permission and sandbox posture of the provider CLIs.

`oms doctor --security` parses each CLI's user-level settings file as data and
prints one row per fact that widens what an agent may do without asking. It
changes no provider file, runs no provider command and executes nothing a
settings file names. The one file it writes is its own: a fingerprint of the
facts it reported, so the next run can say which of them moved.

Rows are findings, not verdicts: `warn` marks a setting that removes a prompt
or a sandbox, `unknown` a file that could not be read as what it claims to be.
A missing file means the CLI's defaults apply and is not a warning.
"""

import hashlib
import json
import os
import shutil
import sys
import tempfile
import time

LIMIT = 1024 * 1024
PRIMITIVES = ("bwrap", "socat", "sandbox-exec", "srt", "docker", "podman")
USERNS = (("kernel.unprivileged_userns_clone", "/proc/sys/kernel/unprivileged_userns_clone"),
          ("kernel.apparmor_restrict_unprivileged_userns",
           "/proc/sys/kernel/apparmor_restrict_unprivileged_userns"))


def clean(value, limit=80):
    """A settings value as one printable line; never more than `limit` characters."""
    text = "".join(ch if ch.isprintable() else "?" for ch in str(value))
    return text if len(text) <= limit else text[:limit - 1] + "…"


def load(path, kind):
    """(document, problem): a mapping parsed from PATH, or why it was not."""
    try:
        if not os.path.exists(path):
            return None, None
        if not os.path.isfile(path):
            return None, "not a regular file"
        if os.path.getsize(path) > LIMIT:
            return None, "larger than %d bytes" % LIMIT
        with open(path, "rb") as handle:
            raw = handle.read()
    except OSError:
        return None, "unreadable"
    try:
        if kind == "toml":
            try:
                import tomllib
            except ImportError:
                return None, "this Python cannot read TOML"
            data = tomllib.loads(raw.decode("utf-8"))
        else:
            data = json.loads(raw.decode("utf-8"))
    except (ValueError, RecursionError):
        return None, "not valid %s" % kind.upper()
    if not isinstance(data, dict):
        return None, "top level is not a table"
    return data, None


def table(value):
    return value if isinstance(value, dict) else {}


def strings(value):
    return [item for item in value if isinstance(item, str)] if isinstance(value, list) else []


def claude(rows, facts, home):
    base = os.environ.get("CLAUDE_CONFIG_DIR") or os.path.join(home, ".claude")
    data, problem = load(os.path.join(base, "settings.json"), "json")
    if problem:
        rows.append(("unknown", "claude: settings.json %s; posture not read" % problem, "claude.settings.json"))
        facts["claude"] = "unknown"
        return
    if data is None:
        rows.append(("info", "claude: no user settings.json; the CLI's defaults apply", "claude.settings.json"))
        facts["claude"] = "absent"
        return
    permissions = table(data.get("permissions"))
    mode = permissions.get("defaultMode")
    allow = strings(permissions.get("allow"))
    broad = sorted(rule for rule in allow if rule in ("Bash", "Bash(*)", "Bash(:*)", "*"))
    sandbox = table(data.get("sandbox"))
    excluded = strings(sandbox.get("excludedCommands"))
    found = {"defaultMode": mode if isinstance(mode, str) else None,
             "skipDangerousModePermissionPrompt": data.get("skipDangerousModePermissionPrompt") is True,
             "broad_allow": broad, "sandbox_enabled": sandbox.get("enabled") is True,
             "sandbox_excluded": len(excluded)}
    facts["claude"] = found
    if found["defaultMode"] == "bypassPermissions":
        rows.append(("warn", "claude: permissions.defaultMode is bypassPermissions; every tool call runs unasked",
                     "claude.permissions.defaultMode"))
    elif found["defaultMode"]:
        rows.append(("ok", "claude: permissions.defaultMode is %s" % clean(found["defaultMode"], 40),
                     "claude.permissions.defaultMode"))
    else:
        rows.append(("ok", "claude: permissions.defaultMode is not set", "claude.permissions.defaultMode"))
    if found["skipDangerousModePermissionPrompt"]:
        rows.append(("warn", "claude: skipDangerousModePermissionPrompt is on; bypass mode starts without its confirmation",
                     "claude.skipDangerousModePermissionPrompt"))
    if broad:
        rows.append(("warn", "claude: permissions.allow grants every shell command (%s)"
                     % ", ".join(clean(rule, 20) for rule in broad), "claude.permissions.allow"))
    else:
        rows.append(("ok", "claude: permissions.allow has no blanket shell rule (%d rule(s))" % len(allow),
                     "claude.permissions.allow"))
    if found["sandbox_enabled"]:
        rows.append(("ok", "claude: sandbox.enabled is on"
                     + ("; %d command(s) excluded from it" % len(excluded) if excluded else ""),
                     "claude.sandbox.enabled"))
    else:
        rows.append(("info", "claude: sandbox.enabled is not on; shell commands run unsandboxed",
                     "claude.sandbox.enabled"))


def codex_table(rows, label, config, prefix):
    found = {}
    mode = config.get("sandbox_mode")
    policy = config.get("approval_policy")
    network = table(config.get("sandbox_workspace_write")).get("network_access")
    if isinstance(mode, str):
        found["sandbox_mode"] = mode
        if mode == "danger-full-access":
            rows.append(("warn", "%s: sandbox_mode is danger-full-access; commands run with no sandbox" % label,
                         prefix + ".sandbox_mode"))
        else:
            rows.append(("ok", "%s: sandbox_mode is %s" % (label, clean(mode, 40)), prefix + ".sandbox_mode"))
    if isinstance(policy, str):
        found["approval_policy"] = policy
        if policy == "never":
            rows.append(("warn", "%s: approval_policy is never; nothing asks before a command runs" % label,
                         prefix + ".approval_policy"))
        else:
            rows.append(("ok", "%s: approval_policy is %s" % (label, clean(policy, 40)), prefix + ".approval_policy"))
    if network is True:
        found["network_access"] = True
        rows.append(("warn", "%s: sandbox_workspace_write.network_access is on; sandboxed commands reach the network" % label,
                     prefix + ".sandbox_workspace_write.network_access"))
    return found


def codex(rows, facts, home):
    base = os.environ.get("CODEX_HOME") or os.path.join(home, ".codex")
    data, problem = load(os.environ.get("OMS_CODEX_CONFIG") or os.path.join(base, "config.toml"), "toml")
    if problem:
        rows.append(("unknown", "codex: config.toml %s; posture not read" % problem, "codex.config.toml"))
        facts["codex"] = "unknown"
        return
    if data is None:
        rows.append(("info", "codex: no config.toml; the CLI's defaults apply", "codex.config.toml"))
        facts["codex"] = "absent"
        return
    before = len(rows)
    found = {"top": codex_table(rows, "codex", data, "codex")}
    profiles = table(data.get("profiles"))
    for name in sorted(profiles)[:20]:
        if isinstance(name, str) and isinstance(profiles[name], dict):
            inner = codex_table(rows, "codex profile %s" % clean(name, 30), profiles[name],
                                "codex.profiles.%s" % clean(name, 30))
            if inner:
                found["profile:" + name] = inner
    skipped = max(0, len(profiles) - 20)
    if skipped:
        found["profiles_skipped"] = skipped
        rows.append(("unknown", "codex: %d profile(s) skipped; posture incomplete" % skipped,
                     "codex.profiles"))
    if len(rows) == before:
        rows.append(("ok", "codex: config.toml sets no sandbox or approval override", "codex.config.toml"))
    facts["codex"] = found


def antigravity(rows, facts, home):
    path = os.environ.get("OMS_ANTIGRAVITY_SETTINGS") or os.path.join(
        home, ".gemini", "antigravity-cli", "settings.json")
    data, problem = load(path, "json")
    if problem:
        rows.append(("unknown", "antigravity: settings.json %s; posture not read" % problem, "antigravity.settings.json"))
        facts["antigravity"] = "unknown"
        return
    if data is None:
        rows.append(("info", "antigravity: no settings.json; the CLI's defaults apply", "antigravity.settings.json"))
        facts["antigravity"] = "absent"
        return
    allow = strings(table(data.get("permissions")).get("allow"))
    broad = sorted(rule for rule in allow if rule in ("mcp(*)", "unsandboxed(*)", "*"))
    found = {"broad_allow": broad, "unsandboxed_rules": sum(rule.startswith("unsandboxed(") for rule in allow),
             "enableTerminalSandbox": data.get("enableTerminalSandbox")
             if isinstance(data.get("enableTerminalSandbox"), bool) else None}
    facts["antigravity"] = found
    if broad:
        rows.append(("warn", "antigravity: permissions.allow has a blanket rule (%s)"
                     % ", ".join(clean(rule, 20) for rule in broad), "antigravity.permissions.allow"))
    else:
        rows.append(("ok", "antigravity: permissions.allow has no blanket rule (%d rule(s), %d unsandboxed)"
                     % (len(allow), found["unsandboxed_rules"]), "antigravity.permissions.allow"))
    if found["enableTerminalSandbox"] is False:
        rows.append(("warn", "antigravity: enableTerminalSandbox is off; terminal commands run unsandboxed",
                     "antigravity.enableTerminalSandbox"))
    elif found["enableTerminalSandbox"] is True:
        rows.append(("ok", "antigravity: enableTerminalSandbox is on", "antigravity.enableTerminalSandbox"))


def primitives(rows, facts):
    present = sorted(name for name in PRIMITIVES if shutil.which(name))
    facts["primitives"] = present
    rows.append(("info", "sandbox tools on PATH: %s (not executed)" % (", ".join(present) if present else "none"),
                 "system.sandbox-tools"))
    for name, path in USERNS:
        try:
            with open(path, encoding="ascii") as handle:
                value = handle.read(16).strip()
        except (OSError, ValueError):
            continue
        facts[name] = value
        rows.append(("info", "%s = %s" % (name, clean(value, 16)), "system." + name))


PROMPT_LEDGER = os.path.join(".oms", "hooks", "events.jsonl")
PROMPT_LEDGER_LIMIT = 8 * 1024 * 1024
PROMPT_HINT_DEFAULT = 20
PROMPT_DAYS = 7


def prompt_ledger(start):
    """The nearest enclosing project's hook ledger, or None; nothing is created."""
    here = os.path.realpath(start)
    while True:
        path = os.path.join(here, PROMPT_LEDGER)
        if os.path.isfile(path) and not os.path.islink(path):
            return path
        parent = os.path.dirname(here)
        if parent == here:
            return None
        here = parent


def permission_prompts(rows):
    """Count recorded `permission_prompt` Notification rows per session and per UTC day.

    Read-only and advisory: it only reads the project hook ledger, is not part of
    the drift fingerprint, and never edits a permission or allow rule. The ledger
    keeps no tool or command text, so there is no per-tool breakdown.
    """
    path = prompt_ledger(os.getcwd())
    if path is None:
        rows.append(("info", "permission prompts: no project hook ledger here; run from an adopted project",
                     "security.prompts"))
        return
    sessions, days = {}, {}
    try:
        with open(path, "rb") as handle:
            size = os.fstat(handle.fileno()).st_size
            if size > PROMPT_LEDGER_LIMIT:
                handle.seek(size - PROMPT_LEDGER_LIMIT)
                handle.readline()
            for raw in handle:
                if b"permission_prompt" not in raw:
                    continue
                try:
                    row = json.loads(raw.decode("utf-8", "replace"))
                except ValueError:
                    continue
                if not isinstance(row, dict) or row.get("action") != "permission_prompt":
                    continue
                session, when = row.get("session"), row.get("ts")
                if not isinstance(session, str) or not isinstance(when, str) or len(when) < 10:
                    continue
                sessions[session] = sessions.get(session, 0) + 1
                days[when[:10]] = days.get(when[:10], 0) + 1
    except OSError:
        rows.append(("unknown", "permission prompts: hook ledger could not be read", "security.prompts"))
        return
    if not sessions:
        rows.append(("info", "permission prompts: none recorded in this project's hook ledger", "security.prompts"))
        return
    try:
        limit = max(1, int(os.environ.get("OMS_PERMISSION_PROMPT_HINT", "") or PROMPT_HINT_DEFAULT))
    except ValueError:
        limit = PROMPT_HINT_DEFAULT
    total = sum(sessions.values())
    busiest = max(sessions.items(), key=lambda item: (item[1], item[0]))
    recent = sorted(days.items())[-PROMPT_DAYS:]
    rows.append(("info", "permission prompts: %d in %d session(s) over %d day(s); busiest session %s had %d"
                 % (total, len(sessions), len(days), clean(busiest[0][:8], 12), busiest[1]), "security.prompts"))
    rows.append(("info", "permission prompts per day (last %d with any): %s"
                 % (len(recent), ", ".join("%s=%d" % (day, count) for day, count in recent)),
                 "security.prompts.days"))
    peak = max(max(sessions.values()), max(days.values()))
    if peak >= limit:
        rows.append(("info", "permission prompts reached %d in one session or day (hint at %d): frequent approvals "
                     "wear down review; consider narrow permissions.allow rules or the sandbox instead of "
                     "bypassPermissions (this report never edits permissions)" % (peak, limit),
                     "security.prompts.hint"))


def state_path(home):
    root = os.environ.get("XDG_STATE_HOME") or os.path.join(home, ".local", "state")
    return os.path.join(root, "oh-my-setting", "doctor-security.json")


def drift(rows, facts, home):
    """Compare with the previous run's facts and record this run's; a harness child records nothing."""
    path = state_path(home)
    sections = {key: hashlib.sha256(json.dumps(value, sort_keys=True).encode("utf-8")).hexdigest()
                for key, value in facts.items()}
    previous = None
    try:
        with open(path, encoding="utf-8") as handle:
            previous = json.load(handle)
    except (OSError, ValueError):
        previous = None
    old = table(table(previous).get("sections"))
    since = {"baseline": True, "checked_at": None, "changed": []}
    if not old:
        rows.append(("info", "no earlier security check recorded; this one is the baseline", "security.drift"))
    else:
        moved = sorted(key for key in set(sections) | set(old) if sections.get(key) != old.get(key))
        when = clean(table(previous).get("checked_at") or "an earlier check", 32)
        since = {"baseline": False, "checked_at": when, "changed": [clean(key, 40) for key in moved]}
        if moved:
            rows.append(("warn", "changed since %s: %s" % (when, ", ".join(clean(key, 40) for key in moved)),
                         "security.drift"))
        else:
            rows.append(("ok", "unchanged since %s" % when, "security.drift"))
    if os.environ.get("OMS_HARNESS_CHILD") == "1":
        return since
    scratch = None
    try:
        os.makedirs(os.path.dirname(path), mode=0o700, exist_ok=True)
        descriptor, scratch = tempfile.mkstemp(prefix=".doctor-security.", dir=os.path.dirname(path))
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            json.dump({"schema": 1, "checked_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                       "sections": sections}, handle, sort_keys=True)
            handle.write("\n")
        os.replace(scratch, path)
        scratch = None
    except OSError:
        rows.append(("info", "could not record this check for later comparison", "security.state"))
    finally:
        if scratch is not None:
            try:
                os.unlink(scratch)
            except OSError:
                pass
    return since


# --json levels: `info` rows state a fact that is neither a warning nor a read failure.
JSON_LEVEL = {"ok": "ok", "info": "info", "warn": "warn", "unknown": "unread"}


def main():
    home = os.path.expanduser("~")
    rows, facts = [], {}
    claude(rows, facts, home)
    codex(rows, facts, home)
    antigravity(rows, facts, home)
    primitives(rows, facts)
    since = drift(rows, facts, home)
    permission_prompts(rows)
    warned = sum(row[0] == "warn" for row in rows)
    unread = sum(row[0] == "unknown" for row in rows)
    if "--json" in sys.argv[1:]:
        print(json.dumps({
            "schema": 1, "kind": "oms-doctor-security",
            "findings": [{"id": ident, "level": JSON_LEVEL[level], "message": text}
                         for level, text, ident in rows],
            "changed_since": since, "warnings": warned, "unread": unread, "exit": 0,
        }, ensure_ascii=False, sort_keys=True))
        return 0
    for level, text, _ in rows:
        print("%s: %s" % (level, text))
    print("security: %d warning(s), %d unread; findings only, nothing was changed" % (warned, unread))
    return 0


if __name__ == "__main__":
    sys.exit(main())
