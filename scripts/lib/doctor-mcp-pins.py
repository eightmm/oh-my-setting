#!/usr/bin/env python3
"""MCP server pinning: report added, changed and removed servers, and risky launch shapes.

Usage: doctor-mcp-pins.py [--repo PATH] [--json] [--pin]

Builds on the no-exec reader in doctor-extensions.py: configuration files are
parsed as data, no server starts, no provider command runs and nothing is
contacted. For each server it hashes (transport, command, arguments, URL,
environment variable NAMES) into a private pin file and compares the current
state with the recorded one. Environment values and headers are never read into
the hash or the output. Tool descriptions are not fetched: that would mean
starting the server.

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
        captured.append((row, pin_of(safe), risks_for(safe)))
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
    pins, risks = {}, []
    for row, digest, found in captured:
        key = "%s:%s:%s" % (owner.get(id(row), "?"), row["scope"], row["name"])
        pins[key] = digest
        risks += [{"server": key, "risk": text} for text in found]
    unknown = sorted(p["provider"] for p in report["providers"]
                     if any(s["status"] == "unread" for s in p["sources"]))
    return pins, risks, unknown


def read_pins(path):
    if not os.path.isfile(path):
        return None
    try:
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)
        pins = data["pins"]
        if data.get("schema") == 1 and isinstance(pins, dict):
            return {str(k): str(v) for k, v in pins.items()}
    except (OSError, ValueError, KeyError, TypeError):
        pass
    raise ValueError("pin file exists but is not a schema-1 pin file: " + path)


def write_pins(path, pins):
    parent = os.path.dirname(path) or "."
    os.makedirs(parent, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=".mcp-pins.", dir=parent)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump({"schema": 1, "pins": pins}, fh, indent=2, sort_keys=True)
            fh.write("\n")
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def main(argv):
    repo, as_json, pin, i = "", False, False, 0
    while i < len(argv):
        if argv[i] == "--json":
            as_json = True
        elif argv[i] == "--pin":
            pin = True
        elif argv[i] == "--repo" and i + 1 < len(argv):
            i += 1
            repo = argv[i]
        else:
            print("usage: doctor-mcp-pins.py [--repo PATH] [--json] [--pin]", file=sys.stderr)
            return 2
        i += 1
    if repo:
        if not os.path.isdir(repo):
            print("error: --repo is not a directory", file=sys.stderr)
            return 2
        repo = os.path.realpath(repo)
    path = pin_file()
    try:
        recorded = read_pins(path)
    except ValueError as exc:
        print("error: %s" % exc, file=sys.stderr)
        return 2
    pins, risks, unknown = collect(load_reader(), repo)

    def known(key):
        return key.split(":", 1)[0] not in unknown

    new = changed = removed = []
    if recorded is not None:
        new = sorted(k for k in pins if k not in recorded)
        changed = sorted(k for k in pins if k in recorded and recorded[k] != pins[k])
        removed = sorted(k for k in recorded if k not in pins and known(k))
    if pin:
        merged = {k: v for k, v in (recorded or {}).items() if not known(k)}
        merged.update(pins)
        write_pins(path, merged)
    report = {"schema": 1, "kind": "oms-mcp-pins", "servers": len(pins),
              "baseline": "recorded" if recorded is not None else "none", "new": new, "changed": changed,
              "removed": removed, "unknown_providers": unknown, "risks": risks, "pinned": pin}
    if as_json:
        print(json.dumps(report, indent=2, sort_keys=True))
        return 0
    out = ["mcp pins: %d server(s) read from configuration files (parsed, never run; "
           "tool descriptions are not fetched)" % len(pins)]
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
