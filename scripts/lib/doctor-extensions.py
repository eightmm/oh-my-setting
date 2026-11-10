#!/usr/bin/env python3
"""Read-only inventory of MCP servers and native plugins across the provider CLIs.

Usage: doctor-extensions.py [--repo PATH] [--json]

Configuration files are parsed as data. No server is started, no provider
command runs, nothing a file names is executed and no network is touched. A
server row carries its name, transport, scope and either the command's base
name or the URL's host; arguments, environment values, headers, URL paths and
credentials are never printed, only how many environment names are set. A file
that is absent is "none"; one that cannot be parsed is "unread", never empty.
"""

import json
import os
import sys
import tomllib
from urllib.parse import urlsplit

OWNED = "oh-my-setting"
MAX_BYTES = 4194304
ROW_CAP = 40


def clean(value, limit=60):
    text = "".join(ch if ch.isprintable() and ch not in "\t" else "?" for ch in str(value))
    return text[:limit]


def load(path, kind):
    """(data, status): status is ok, none (absent) or unread."""
    try:
        if not os.path.isfile(path):
            return None, "none"
        if os.path.getsize(path) > MAX_BYTES:
            return None, "unread"
        with open(path, "rb") as handle:
            raw = handle.read()
        data = tomllib.loads(raw.decode("utf-8")) if kind == "toml" else json.loads(raw.decode("utf-8"))
    except (OSError, ValueError, RecursionError):
        return None, "unread"
    return (data, "ok") if isinstance(data, dict) else (None, "unread")


def server_row(name, spec, scope):
    spec = spec if isinstance(spec, dict) else {}
    command, url = spec.get("command"), spec.get("url") or spec.get("httpUrl") or spec.get("serverUrl")
    kind = spec.get("type") or spec.get("transport")
    if isinstance(url, str) and url:
        try:
            parts = urlsplit(url)
            host = parts.hostname or "?"
            scheme = parts.scheme or "?"
        except ValueError:
            host, scheme = "?", "?"
        transport = clean(kind, 12) if isinstance(kind, str) and kind else "http"
        target = "%s://%s" % (clean(scheme, 8), clean(host))
    elif isinstance(command, str) and command:
        transport, target = "stdio", clean(os.path.basename(command.split()[0]) if command.split() else "?")
    else:
        transport, target = "unknown", "?"
    env = spec.get("env")
    return {"name": clean(name), "transport": transport, "scope": scope, "target": target,
            "env_names": len(env) if isinstance(env, dict) else 0,
            "disabled": spec.get("disabled") is True or spec.get("enabled") is False,
            "owned": name == OWNED}


def servers_from(data, scope, key="mcpServers"):
    table = data.get(key) if isinstance(data, dict) else None
    return [server_row(name, spec, scope) for name, spec in sorted(table.items())] if isinstance(table, dict) else []


def plugin_names(table, scope):
    rows = []
    if isinstance(table, dict):
        for name, state in sorted(table.items()):
            rows.append({"name": clean(name), "scope": scope, "enabled": state is not False,
                         "owned": clean(name).split("@")[0] == OWNED})
    return rows


def subdirs(path):
    try:
        return sorted(name for name in os.listdir(path)
                      if not name.startswith(".") and os.path.isdir(os.path.join(path, name)))
    except OSError:
        return []


def claude(home, repo):
    sources, servers, plugins = [], [], []
    data, status = load(os.path.join(home, ".claude.json"), "json")
    sources.append({"file": "~/.claude.json", "status": status})
    if data:
        servers += servers_from(data, "user")
        projects = data.get("projects")
        if repo and isinstance(projects, dict) and isinstance(projects.get(repo), dict):
            servers += servers_from(projects[repo], "local")
    if repo:
        data, status = load(os.path.join(repo, ".mcp.json"), "json")
        sources.append({"file": ".mcp.json", "status": status})
        servers += servers_from(data, "project") if data else []
    for path, shown, scope in ((os.path.join(home, ".claude", "settings.json"), "~/.claude/settings.json", "user"),
                               (os.path.join(repo, ".claude", "settings.json") if repo else None,
                                ".claude/settings.json", "project")):
        if not path:
            continue
        data, status = load(path, "json")
        sources.append({"file": shown, "status": status})
        plugins += plugin_names(data.get("enabledPlugins"), scope) if data else []
    return {"provider": "claude", "sources": sources, "servers": servers, "plugins": plugins,
            "plugins_note": "enabledPlugins in settings; marketplace contents are not read"}


def codex(home, _repo):
    base = os.environ.get("CODEX_HOME") or os.path.join(home, ".codex")
    data, status = load(os.path.join(base, "config.toml"), "toml")
    return {"provider": "codex", "sources": [{"file": "config.toml (Codex home)", "status": status}],
            "servers": servers_from(data, "user", "mcp_servers") if data else [], "plugins": [],
            "plugins_note": "not read: Codex plugin state is not in a file this report parses"}


def antigravity(home, _repo):
    sources, servers, plugins = [], [], []
    data, status = load(os.path.join(home, ".gemini", "settings.json"), "json")
    sources.append({"file": "~/.gemini/settings.json", "status": status})
    servers += servers_from(data, "user") if data else []
    root = os.path.join(home, ".gemini", "config", "plugins")
    for name in subdirs(root):
        plugins.append({"name": clean(name), "scope": "user", "enabled": True, "owned": name == OWNED})
        data, status = load(os.path.join(root, name, "mcp_config.json"), "json")
        if status != "none":
            sources.append({"file": "~/.gemini/config/plugins/%s/mcp_config.json" % clean(name), "status": status})
        servers += servers_from(data, "plugin:" + clean(name, 30)) if data else []
    for name in subdirs(os.path.join(home, ".gemini", "extensions")):
        plugins.append({"name": clean(name), "scope": "extension", "enabled": True, "owned": name == OWNED})
    return {"provider": "antigravity", "sources": sources, "servers": servers, "plugins": plugins,
            "plugins_note": "plugin and extension directory names; their contents are not read"}


def mismatches(providers):
    """Servers one CLI has and another lacks, and one name pointing at different targets. A CLI whose
    configuration could not be read is left out of the comparison rather than counted as lacking."""
    readable = [p for p in providers if not any(s["status"] == "unread" for s in p["sources"])]
    names = {}
    for row in readable:
        for server in row["servers"]:
            names.setdefault(server["name"], {}).setdefault(row["provider"], set()).add(server["target"])
    found = []
    everyone = [p["provider"] for p in readable]
    for name in sorted(names):
        have = names[name]
        missing = [p for p in everyone if p not in have]
        if missing and len(everyone) > 1:
            found.append({"server": name, "kind": "missing", "present": sorted(have), "missing": missing})
        targets = sorted({t for values in have.values() for t in values})
        if len(targets) > 1:
            found.append({"server": name, "kind": "target", "targets": targets})
    return found


def build(repo):
    home = os.path.realpath(os.path.expanduser("~"))
    providers = [reader(home, repo) for reader in (claude, codex, antigravity)]
    return {"schema": 1, "kind": "oms-extension-inventory", "repo": repo or None, "providers": providers,
            "mismatches": mismatches(providers),
            "unread": sum(s["status"] == "unread" for p in providers for s in p["sources"])}


def render(report):
    out = ["extensions: MCP servers and native plugins from configuration files (parsed, never run; "
           "arguments, environment values, headers and URL paths are not shown)"]
    for row in report["providers"]:
        out.append("%s: %d server(s), %d plugin(s)" % (row["provider"], len(row["servers"]), len(row["plugins"])))
        for source in row["sources"]:
            if source["status"] == "unread":
                out.append("  %s: %s" % (source["status"], source["file"]))
        for server in row["servers"][:ROW_CAP]:
            marks = [m for m, on in (("oms", server["owned"]), ("disabled", server["disabled"])) if on]
            if server["env_names"]:
                marks.append("%d env name(s)" % server["env_names"])
            out.append("  server %-24s %-8s %-14s %s%s" % (server["name"], server["transport"], server["scope"],
                                                          server["target"], "  [%s]" % ", ".join(marks) if marks else ""))
        if len(row["servers"]) > ROW_CAP:
            out.append("  … %d more server(s)" % (len(row["servers"]) - ROW_CAP))
        for plugin in row["plugins"][:ROW_CAP]:
            marks = [m for m, on in (("oms", plugin["owned"]), ("disabled", not plugin["enabled"])) if on]
            out.append("  plugin %-24s %s%s" % (plugin["name"], plugin["scope"], "  [%s]" % ", ".join(marks) if marks else ""))
        out.append("  plugins: " + row["plugins_note"])
    for item in report["mismatches"][:ROW_CAP]:
        if item["kind"] == "missing":
            out.append("note: server %s is configured for %s but not for %s"
                       % (item["server"], ", ".join(item["present"]), ", ".join(item["missing"])))
        else:
            out.append("note: server %s points at different targets: %s" % (item["server"], ", ".join(item["targets"])))
    out.append("extensions: %d mismatch note(s), %d file(s) unread; an unread file is unknown, not empty; "
               "findings only, nothing was changed" % (len(report["mismatches"]), report["unread"]))
    return "\n".join(out)


def main(argv):
    repo, as_json, i = "", False, 0
    while i < len(argv):
        if argv[i] == "--json":
            as_json = True
        elif argv[i] == "--repo" and i + 1 < len(argv):
            i += 1
            repo = argv[i]
        else:
            print("usage: doctor-extensions.py [--repo PATH] [--json]", file=sys.stderr)
            return 2
        i += 1
    if repo:
        if not os.path.isdir(repo):
            print("error: --repo is not a directory", file=sys.stderr)
            return 2
        repo = os.path.realpath(repo)
    report = build(repo)
    print(json.dumps(report, ensure_ascii=False, indent=2) if as_json else render(report))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
