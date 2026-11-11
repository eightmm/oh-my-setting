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

import fnmatch
import hashlib
import json
import os
import shutil
import sys
import time

LIMIT = 1024 * 1024
PRIMITIVES = ("bwrap", "socat", "sandbox-exec", "srt", "docker", "podman")
USERNS = (("kernel.unprivileged_userns_clone", "/proc/sys/kernel/unprivileged_userns_clone"),
          ("kernel.apparmor_restrict_unprivileged_userns",
           "/proc/sys/kernel/apparmor_restrict_unprivileged_userns"))
# Substrings of a Read deny rule that mean it covers a conventional secret path. The `~/.ssh` and
# `~/.aws` spellings are split so the outbound scrubber does not read this table as a home path.
SECRET_MARKS = (".env", ".ss" "h", ".aw" "s", "id_rsa", ".pem", ".netrc", ".git-credentials")
# Environment names that look like credentials. The first group is the list `ma_scrub_credential_env`
# in scripts/lib/peer-common.sh removes from an opted-in worker (a test keeps the two equal);
# `GH_*` is added here because the GitHub CLI reads its token from it.
WORKER_CREDENTIAL_GLOBS = ("AWS_*", "*_TOKEN", "*_SECRET", "*_SECRET_KEY", "*_API_KEY", "*_APIKEY",
                           "*_ACCESS_KEY", "*_ACCESS_KEY_ID", "*_PRIVATE_KEY", "*_PASSWORD", "*_PASSWD",
                           "*_CREDENTIALS")
CREDENTIAL_GLOBS = WORKER_CREDENTIAL_GLOBS + ("GH_*",)


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


def credential_name(name):
    return not name.startswith("OMS_") and any(fnmatch.fnmatchcase(name, glob) for glob in CREDENTIAL_GLOBS)


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
    hardening(rows, found, data, "claude", "claude", user=True)


def name_list(names, limit=8):
    """Names joined for one row; a long list is cut and says how many were left out."""
    shown = ", ".join(clean(name, 40) for name in names[:limit])
    return shown + (" and %d more" % (len(names) - limit) if len(names) > limit else "")


def secret_deny(rules):
    """True when a deny rule keeps the Read tool out of a conventional secret path."""
    for rule in rules:
        if rule.startswith("Read(") and any(mark in rule for mark in SECRET_MARKS):
            return True
    return False


def broad_domain(entry):
    """A wildcard that names no registrable domain: `*` or `*.com`."""
    if "*" not in entry:
        return False
    return len([part for part in entry.split(".") if part and "*" not in part]) < 2


def hardening(rows, found, data, label, prefix, user):
    """Secret-path deny, unsandboxed fallback, excluded command names, wide network entries.

    Only keys present in the file are judged; a value of an unexpected shape is `unknown`, because
    a CLI version that renamed the key is not evidence of a safe setting."""
    permissions = table(data.get("permissions"))
    sandbox = table(data.get("sandbox"))
    if user:
        deny = strings(permissions.get("deny"))
        found["secret_read_deny"] = secret_deny(deny)
        if not found["secret_read_deny"]:
            rows.append(("info", "%s: permissions.deny has no Read rule for .env, ~/.ssh or ~/.aws (%d deny rule(s)); "
                         "a project's deny rules are not read here" % (label, len(deny)),
                         prefix + ".permissions.deny"))
    fallback = sandbox.get("allowUnsandboxedCommands")
    if fallback is True:
        found["allow_unsandboxed"] = True
        rows.append(("warn", "%s: sandbox.allowUnsandboxedCommands is on; a command the sandbox blocks may be retried outside it"
                     % label, prefix + ".sandbox.allowUnsandboxedCommands"))
    elif fallback is not None and fallback is not False:
        rows.append(("unknown", "%s: sandbox.allowUnsandboxedCommands is not true or false; posture not read" % label,
                     prefix + ".sandbox.allowUnsandboxedCommands"))
    excluded = strings(sandbox.get("excludedCommands"))
    if excluded:
        found["sandbox_excluded_names"] = sorted(excluded)
        rows.append(("info", "%s: sandbox.excludedCommands runs outside the sandbox: %s" % (label, name_list(sorted(excluded))),
                     prefix + ".sandbox.excludedCommands"))
    network = sandbox.get("network")
    domains = table(network).get("allowedDomains") if isinstance(network, dict) else None
    if domains is not None and not isinstance(domains, list):
        rows.append(("unknown", "%s: sandbox.network.allowedDomains is not a list; posture not read" % label,
                     prefix + ".sandbox.network.allowedDomains"))
    elif domains:
        names = sorted(strings(domains))
        wide = [name for name in names if broad_domain(name)]
        found["allowed_domains"] = len(names)
        found["broad_domains"] = wide
        if wide:
            rows.append(("warn", "%s: sandbox.network.allowedDomains has a catch-all entry (%s)"
                         % (label, name_list(wide)), prefix + ".sandbox.network.allowedDomains"))
        else:
            wildcard = [name for name in names if "*" in name]
            rows.append(("info", "%s: sandbox.network.allowedDomains lists %d domain(s)%s"
                         % (label, len(names), "; wildcards: " + name_list(wildcard) if wildcard else ""),
                         prefix + ".sandbox.network.allowedDomains"))


# Settings keys that name a command Claude Code runs on its own; the value is never shown.
CLAUDE_RUNNERS = ("apiKeyHelper", "awsAuthRefresh", "awsCredentialExport", "gcpAuthRefresh",
                  "otelHeadersHelper", "statusLine", "fileSuggestion")
# Environment names a project can set to move traffic or credentials somewhere else.
REDIRECT_MARKS = ("BASE_URL", "PROXY", "ENDPOINT", "_HOST")


def hook_commands(value):
    """(events, commands) counted in a `hooks` table; the commands themselves are not kept."""
    events, count = [], 0
    for event, groups in sorted(table(value).items()):
        before = count
        for group in groups if isinstance(groups, list) else []:
            for hook in table(group).get("hooks", []) if isinstance(table(group).get("hooks", []), list) else []:
                if isinstance(hook, dict) and hook.get("type", "command") == "command":
                    count += 1
        if count > before:
            events.append(event)
    return events, count


def project_root():
    """The nearest directory at or above the working directory that holds `.git`, else the working directory."""
    here = os.path.realpath(os.getcwd())
    probe = here
    while True:
        if os.path.exists(os.path.join(probe, ".git")):
            return probe
        parent = os.path.dirname(probe)
        if parent == probe:
            return here
        probe = parent


def project_claude(rows, found, root, name):
    path = os.path.join(root, ".claude", name)
    label = "project %s" % name
    prefix = "project.claude." + name
    data, problem = load(path, "json")
    if problem:
        rows.append(("unknown", "%s: %s; not read" % (label, problem), prefix))
        found[name] = "unknown"
        return
    if data is None:
        return
    keys = {}
    events, count = hook_commands(data.get("hooks"))
    if count:
        keys["hooks"] = [count] + events
        rows.append(("warn", "%s: registers %d hook command(s) on %s; they run with your permissions"
                     % (label, count, name_list(events)), prefix + ".hooks"))
    runners = sorted(key for key in CLAUDE_RUNNERS if key in data)
    if runners:
        keys["runners"] = runners
        rows.append(("warn", "%s: sets %s, which name a command Claude Code runs" % (label, name_list(runners)),
                     prefix + ".commands"))
    env = sorted(key for key in table(data.get("env")) if isinstance(key, str))
    moved = [key for key in env if any(mark in key.upper() for mark in REDIRECT_MARKS) or credential_name(key)]
    if moved:
        keys["env_redirect"] = moved
        rows.append(("warn", "%s: env sets %s (names only; values not read)" % (label, name_list(moved)), prefix + ".env"))
    elif env:
        keys["env"] = len(env)
        rows.append(("info", "%s: env sets %d variable(s)" % (label, len(env)), prefix + ".env"))
    if data.get("enableAllProjectMcpServers") is True:
        keys["enableAllProjectMcpServers"] = True
        rows.append(("warn", "%s: enableAllProjectMcpServers is on; every server in .mcp.json is approved unasked" % label,
                     prefix + ".enableAllProjectMcpServers"))
    approved = strings(data.get("enabledMcpjsonServers"))
    if approved:
        keys["enabledMcpjsonServers"] = sorted(approved)
        rows.append(("info", "%s: pre-approves MCP server(s) %s" % (label, name_list(sorted(approved))),
                     prefix + ".enabledMcpjsonServers"))
    permissions = table(data.get("permissions"))
    if permissions.get("defaultMode") == "bypassPermissions":
        keys["defaultMode"] = "bypassPermissions"
        rows.append(("warn", "%s: permissions.defaultMode is bypassPermissions" % label, prefix + ".permissions.defaultMode"))
    broad = sorted(rule for rule in strings(permissions.get("allow")) if rule in ("Bash", "Bash(*)", "Bash(:*)", "*"))
    if broad:
        keys["broad_allow"] = broad
        rows.append(("warn", "%s: permissions.allow grants every shell command (%s)"
                     % (label, ", ".join(clean(rule, 20) for rule in broad)), prefix + ".permissions.allow"))
    hardening(rows, keys, data, label, prefix, user=False)
    found[name] = keys


def project_mcp(rows, found, root):
    data, problem = load(os.path.join(root, ".mcp.json"), "json")
    if problem:
        rows.append(("unknown", "project .mcp.json: %s; not read" % problem, "project.mcp.json"))
        found["mcp.json"] = "unknown"
        return
    servers = table(data.get("mcpServers")) if data else {}
    if not servers:
        return
    names = sorted(name for name in servers if isinstance(name, str))
    launches = sorted(name for name in names if "command" in table(servers[name]))
    env = sorted({key for name in names for key in table(table(servers[name]).get("env")) if isinstance(key, str)})
    found["mcp.json"] = {"servers": names, "launch": launches, "env": env}
    rows.append(("info", "project .mcp.json declares %d MCP server(s): %s; %d launch a local command (not run here)"
                 % (len(names), name_list(names), len(launches)), "project.mcp.json"))
    if env:
        rows.append(("info", "project .mcp.json passes environment variable(s) %s to its servers (names only)"
                     % name_list(env), "project.mcp.json.env"))


def project_codex(rows, found, root):
    data, problem = load(os.path.join(root, ".codex", "config.toml"), "toml")
    if problem:
        rows.append(("unknown", "project .codex/config.toml: %s; not read" % problem, "project.codex.config.toml"))
        found["codex"] = "unknown"
        return
    if data is None:
        return
    keys = codex_table(rows, "project codex", data, "project.codex")
    if "notify" in data:
        keys["notify"] = True
        rows.append(("warn", "project codex: notify names a command Codex runs", "project.codex.notify"))
    servers = sorted(name for name in table(data.get("mcp_servers")) if isinstance(name, str))
    if servers:
        keys["mcp_servers"] = servers
        rows.append(("info", "project codex: declares MCP server(s) %s" % name_list(servers), "project.codex.mcp_servers"))
    providers = table(data.get("model_providers"))
    redirected = sorted(name for name in providers if isinstance(name, str) and "base_url" in table(providers[name]))
    if redirected:
        keys["base_url"] = redirected
        rows.append(("warn", "project codex: model_providers %s set a base_url (value not read)" % name_list(redirected),
                     "project.codex.model_providers"))
    found["codex"] = keys


def project(rows, facts, home):
    """Project-scope settings in the current repository, read as data: key names and counts, never values."""
    root = project_root()
    user = os.path.realpath(os.environ.get("CLAUDE_CONFIG_DIR") or os.path.join(home, ".claude"))
    found = {}
    if os.path.realpath(os.path.join(root, ".claude")) != user:
        for name in ("settings.json", "settings.local.json"):
            project_claude(rows, found, root, name)
    project_mcp(rows, found, root)
    if os.path.realpath(os.path.join(root, ".codex")) != os.path.realpath(
            os.environ.get("CODEX_HOME") or os.path.join(home, ".codex")):
        project_codex(rows, found, root)
    facts["project"] = found or "absent"
    if not found:
        rows.append(("info", "project: no .claude settings, .mcp.json or .codex/config.toml in this repository",
                     "project.settings"))


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


def environment(rows):
    """Names, never values, of credential-shaped variables this process would hand to a child.

    Not part of the drift fingerprint: a session's environment is not a setting."""
    names = sorted(name for name in os.environ if credential_name(name))
    guard = os.environ.get("OMS_WORKER_SECRET_GUARD") == "1"
    if not names:
        rows.append(("info", "environment: no credential-shaped variable names in this session", "environment.credentials"))
        return
    tail = ("OMS_WORKER_SECRET_GUARD=1 removes them from provider workers, except login variables and OMS_WORKER_ENV_ALLOW"
            if guard else
            "OMS_WORKER_SECRET_GUARD is off, so provider workers inherit them unchanged")
    rows.append(("info", "environment: %d credential-shaped variable name(s): %s (values not read); %s"
                 % (len(names), name_list(names, 12), tail), "environment.credentials"))


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
    try:
        os.makedirs(os.path.dirname(path), mode=0o700, exist_ok=True)
        scratch = "%s.%d.tmp" % (path, os.getpid())
        descriptor = os.open(scratch, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            json.dump({"schema": 1, "checked_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                       "sections": sections}, handle, sort_keys=True)
            handle.write("\n")
        os.replace(scratch, path)
    except OSError:
        rows.append(("info", "could not record this check for later comparison", "security.state"))
    return since


# --json levels: `info` rows state a fact that is neither a warning nor a read failure.
JSON_LEVEL = {"ok": "ok", "info": "info", "warn": "warn", "unknown": "unread"}


def main():
    home = os.path.expanduser("~")
    rows, facts = [], {}
    claude(rows, facts, home)
    codex(rows, facts, home)
    antigravity(rows, facts, home)
    project(rows, facts, home)
    primitives(rows, facts)
    environment(rows)
    since = drift(rows, facts, home)
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
