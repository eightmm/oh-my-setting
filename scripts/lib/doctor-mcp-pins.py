#!/usr/bin/env python3
"""MCP server pinning: report added, changed and removed servers, and risky launch shapes.

Usage: doctor-mcp-pins.py [--repo PATH] [--json] [--pin] [--probe-tools NAME]

Builds on the no-exec reader in doctor-extensions.py: configuration files are
parsed as data, no server starts, no provider command runs and nothing is
contacted. For each server it hashes (transport, command, arguments, URL,
environment variable NAMES) into a private pin file and compares the current
state with the recorded one. Environment values and headers are never read into
the hash or the output. Tool descriptions are not fetched by default: that
would mean starting the server.

--probe-tools NAME is the only option that starts a process: it starts the ONE
local stdio server NAME names (never a URL server), reads tools/list under a
hard timeout (doctor-mcp-probe.py), always terminates it, and hashes each full
tool definition. With --pin the hashes are stored under the optional "tools"
field of the same pin file; without it they are only compared with the stored
ones. A changed server binary that lists identical tools is not detected.

Without --pin nothing is written. --pin records the servers as they are now; it
is the person's explicit decision, and an observation by itself is not approval.
A provider whose configuration could not be parsed is "unknown": its recorded
pins are neither reported removed nor dropped by --pin.
"""
import hashlib
import importlib.util
import json
import os
import sys
import tempfile
from urllib.parse import urlsplit

HERE = os.path.dirname(os.path.abspath(__file__))
LAUNCHERS = {"npx", "bunx", "pnpx"}
LOOPBACK = {"localhost", "127.0.0.1", "::1"}


def load_reader():
    spec = importlib.util.spec_from_file_location("oms_doctor_extensions", os.path.join(HERE, "doctor-extensions.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def pin_file():
    explicit = os.environ.get("OMS_MCP_PIN_FILE")
    if explicit:
        return explicit
    base = os.environ.get("XDG_CONFIG_HOME") or os.path.join(os.path.expanduser("~"), ".config")
    return os.path.join(base, "oh-my-setting", "mcp-pins.json")


def package_unpinned(args):
    """True when the first package-like argument carries no version."""
    for arg in args:
        if not isinstance(arg, str) or arg.startswith("-"):
            continue
        name = arg[1:] if arg.startswith("@") else arg
        return "@" not in name or name.endswith("@latest")
    return False


def risks_for(spec):
    found = []
    command = spec.get("command") if isinstance(spec.get("command"), str) else ""
    args = spec.get("args") if isinstance(spec.get("args"), list) else []
    base = os.path.basename(command.split()[0]) if command.split() else ""
    if base in LAUNCHERS and package_unpinned(args):
        found.append("%s runs a package with no pinned version" % base)
    if base == "uvx" and package_unpinned([a for a in args if isinstance(a, str)]) and not any(
            isinstance(a, str) and "==" in a for a in args):
        found.append("uvx runs a package with no pinned version")
    url = spec.get("url") or spec.get("httpUrl") or spec.get("serverUrl")
    if isinstance(url, str):
        try:
            parts = urlsplit(url)
            if parts.scheme == "http" and (parts.hostname or "") not in LOOPBACK:
                found.append("plain http to a non-loopback host")
        except ValueError:
            pass
    return found


def pin_of(spec):
    env = spec.get("env")
    material = {
        "type": spec.get("type") or spec.get("transport"),
        "command": spec.get("command"),
        "args": spec.get("args"),
        "url": spec.get("url") or spec.get("httpUrl") or spec.get("serverUrl"),
        "env_names": sorted(env) if isinstance(env, dict) else [],
    }
    blob = json.dumps(material, sort_keys=True, separators=(",", ":"), default=str)
    return hashlib.sha256(blob.encode("utf-8", "replace")).hexdigest()


def collect(reader, repo):
    """(pins, risks, unknown providers). Keys are provider:scope:name."""
    captured = []
    original = reader.server_row

    def capture(name, spec, scope):
        row = original(name, spec, scope)
        safe = spec if isinstance(spec, dict) else {}
        captured.append((row, pin_of(safe), risks_for(safe), safe))
        return row

    reader.server_row = capture
    try:
        report = reader.build(repo)
    finally:
        reader.server_row = original
    owner = {}
    for provider in report["providers"]:
        for server in provider["servers"]:
            owner[id(server)] = provider["provider"]
    pins, risks, specs = {}, [], {}
    for row, digest, found, spec in captured:
        key = "%s:%s:%s" % (owner.get(id(row), "?"), row["scope"], row["name"])
        pins[key] = digest
        specs[key] = spec
        risks += [{"server": key, "risk": text} for text in found]
    unknown = sorted(p["provider"] for p in report["providers"]
                     if any(s["status"] == "unread" for s in p["sources"]))
    return pins, risks, unknown, specs


def read_store(path):
    """(pins, tool pins) or None when no file. Tool pins are the optional
    {server key: {tool name: definition hash}} field of the same schema-1 file."""
    if not os.path.isfile(path):
        return None
    try:
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)
        pins = data["pins"]
        tools = data.get("tools", {})
        if data.get("schema") == 1 and isinstance(pins, dict) and isinstance(tools, dict):
            return ({str(k): str(v) for k, v in pins.items()},
                    {str(k): {str(n): str(h) for n, h in v.items()}
                     for k, v in tools.items() if isinstance(v, dict)})
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        pass
    raise ValueError("pin file exists but is not a schema-1 pin file: " + path)


def read_pins(path):
    store = read_store(path)
    return None if store is None else store[0]


def write_pins(path, pins, tools=None):
    parent = os.path.dirname(path) or "."
    os.makedirs(parent, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=".mcp-pins.", dir=parent)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            data = {"schema": 1, "pins": pins}
            if tools:
                data["tools"] = tools
            json.dump(data, fh, indent=2, sort_keys=True)
            fh.write("\n")
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def load_probe():
    spec = importlib.util.spec_from_file_location("oms_doctor_mcp_probe", os.path.join(HERE, "doctor-mcp-probe.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def probe_timeout():
    try:
        return min(120.0, max(1.0, float(os.environ.get("OMS_MCP_PROBE_TIMEOUT", "15"))))
    except ValueError:
        return 15.0


def pick_server(name, specs):
    """(key, spec) for the one stdio server NAME names, else (None, reason)."""
    keys = sorted(k for k in specs if k == name or k.split(":", 2)[-1] == name)
    if not keys:
        return None, "no configured server matches %r" % name
    if len(keys) > 1:
        return None, "%r is ambiguous; name one of: %s" % (name, ", ".join(keys))
    spec = specs[keys[0]]
    kind = spec.get("type") or spec.get("transport")
    if spec.get("url") or spec.get("httpUrl") or spec.get("serverUrl") or kind not in (None, "stdio"):
        return None, "%s is a remote or network server; only a local stdio server is ever started" % keys[0]
    if not isinstance(spec.get("command"), str) or not spec["command"].strip():
        return None, "%s has no command to start" % keys[0]
    return keys[0], spec


def diff_tools(old, new):
    return (sorted(n for n in new if n not in old),
            sorted(n for n in new if n in old and old[n] != new[n]),
            sorted(n for n in old if n not in new))


def main(argv):
    repo, as_json, pin, probe_name, i = "", False, False, "", 0
    while i < len(argv):
        if argv[i] == "--json":
            as_json = True
        elif argv[i] == "--pin":
            pin = True
        elif argv[i] == "--repo" and i + 1 < len(argv):
            i += 1
            repo = argv[i]
        elif argv[i] == "--probe-tools" and i + 1 < len(argv):
            i += 1
            probe_name = argv[i]
        else:
            print("usage: doctor-mcp-pins.py [--repo PATH] [--json] [--pin] [--probe-tools NAME]", file=sys.stderr)
            return 2
        i += 1
    if repo:
        if not os.path.isdir(repo):
            print("error: --repo is not a directory", file=sys.stderr)
            return 2
        repo = os.path.realpath(repo)
    path = pin_file()
    try:
        store = read_store(path)
    except ValueError as exc:
        print("error: %s" % exc, file=sys.stderr)
        return 2
    recorded, recorded_tools = (None, {}) if store is None else store
    pins, risks, unknown, specs = collect(load_reader(), repo)

    def known(key):
        return key.split(":", 1)[0] not in unknown

    # Opt-in: the only place a server is started, for one named stdio server.
    probed = None
    if probe_name:
        key, spec = pick_server(probe_name, specs)
        if key is None:
            print("error: --probe-tools: %s" % spec, file=sys.stderr)
            return 2
        module = load_probe()
        try:
            hashes = module.probe(spec, probe_timeout(), repo or os.getcwd())
        except module.ProbeError as exc:
            print("probe failed: %s: %s; nothing was recorded" % (key, exc))
            return 1
        names = {n: "".join(c if c.isprintable() else "?" for c in n)[:80] for n in hashes}
        old = recorded_tools.get(key)
        added, altered, gone = diff_tools(old or {}, hashes) if old is not None else ([], [], [])
        probed = {"server": key, "tools": len(hashes), "baseline": "recorded" if old is not None else "none",
                  "added": [names[n] for n in added], "changed": [names[n] for n in altered],
                  "removed": ["".join(c if c.isprintable() else "?" for c in n)[:80] for n in gone]}

    new = changed = removed = []
    if recorded is not None:
        new = sorted(k for k in pins if k not in recorded)
        changed = sorted(k for k in pins if k in recorded and recorded[k] != pins[k])
        removed = sorted(k for k in recorded if k not in pins and known(k))
    if pin:
        merged = {k: v for k, v in (recorded or {}).items() if not known(k)}
        merged.update(pins)
        kept = {k: v for k, v in recorded_tools.items() if k in merged}
        if probed:
            kept[probed["server"]] = hashes
        write_pins(path, merged, kept)
    report = {"schema": 1, "kind": "oms-mcp-pins", "servers": len(pins),
              "baseline": "recorded" if recorded is not None else "none", "new": new, "changed": changed,
              "removed": removed, "unknown_providers": unknown, "risks": risks, "pinned": pin}
    if probed:
        report["probe"] = probed
    if as_json:
        print(json.dumps(report, indent=2, sort_keys=True))
        return 0
    out = ["mcp pins: %d server(s) read from configuration files (%s)" % (
        len(pins), "one server was started by --probe-tools, the rest parsed, never run" if probed
        else "parsed, never run; tool descriptions are not fetched")]
    if probed:
        out.append("probe: %s: %d tool definition(s) read" % (probed["server"], probed["tools"]))
        if probed["baseline"] == "none" and not pin:
            out.append("note: no tool pin recorded for %s; run `oms doctor --mcp --probe-tools %s --pin` to record"
                       % (probed["server"], probe_name))
        out += ["tool %s: %s: %s" % (label, probed["server"], name) + (
            " (tool definition changed since pin)" if label == "changed" else "")
            for label in ("added", "changed", "removed") for name in probed[label]]
        if probed["baseline"] == "recorded" and not (probed["added"] or probed["changed"] or probed["removed"]):
            out.append("ok: every tool definition of %s matches its pin" % probed["server"])
    if recorded is None and not pin:
        out.append("note: no baseline recorded; run `oms doctor --mcp --pin` to record these servers")
    for label, keys in (("new", new), ("changed", changed), ("removed", removed)):
        out += ["%s: %s" % (label, key) for key in keys]
    out += ["warn: %s: %s" % (r["server"], r["risk"]) for r in risks]
    out += ["unknown: %s configuration could not be parsed; its pins are neither compared nor changed" % p
            for p in unknown]
    if pin:
        out.append("pinned: %d server(s) recorded in a private file (mode 600); this is your approval, "
                   "not something the report inferred" % len(merged))
    elif recorded is not None and not (new or changed or removed):
        out.append("ok: every server matches its pin")
    print("\n".join(out))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
