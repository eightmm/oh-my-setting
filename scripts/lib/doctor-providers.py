#!/usr/bin/env python3
"""Read-only provider x function support matrix from config/provider-capabilities.json.

Usage: doctor-providers.py [--json] [--check] [--table PATH] [--root PATH]

The table is data: nothing runs, nothing is contacted. --check verifies that every
supported/partial cell names an implementation definition and a test word that exist
in the tree (used by tests/provider-registry-smoke.sh); --table and --root point it
at a copy so the test can prove a renamed symbol fails.
"""

import json
import os
import re
import sys

ROOT = os.path.realpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
STATUSES = ("supported", "partial", "unsupported", "unknown")
VERIFIED = ("fixture", "live")
SHORT = {"supported": "sup", "partial": "par", "unsupported": "uns", "unknown": "unk"}
COLUMNS = {"launch_main": "launch", "worker_call": "worker", "observe": "observe", "resume": "resume",
           "send_message": "send", "stop": "stop", "approval": "approve", "usage": "usage",
           "handoff": "handoff"}
MAX_BYTES = 1048576


def load(path):
    with open(path, "rb") as handle:
        raw = handle.read(MAX_BYTES + 1)
    if len(raw) > MAX_BYTES:
        raise ValueError("table too large")
    data = json.loads(raw.decode("utf-8"))
    if not isinstance(data, dict) or data.get("schema") != 1:
        raise ValueError("unsupported table schema")
    return data


def resolve(data):
    """{provider: {function: cell}} with profiles and refs expanded."""
    functions = [row["id"] for row in data["functions"]]
    out = {}
    for name, spec in data["providers"].items():
        cells = dict(data.get("profiles", {}).get(spec.get("profile"), {}))
        for key, cell in spec.get("overrides", {}).items():
            cells[key] = data["refs"][cell["$ref"]] if "$ref" in cell else cell
        for key, cell in spec.items():
            if key in functions:
                cells[key] = cell
        out[name] = {fn: cells.get(fn) for fn in functions}
    return out


def split(entry):
    path, _, word = entry.partition("::")
    return path, word


def has_definition(text, path, symbol):
    quoted = re.escape(symbol)
    if path.endswith(".py"):
        pattern = r"^\s*(?:async\s+)?(?:def|class)\s+%s\b|^%s\s*=" % (quoted, quoted)
    elif path.endswith(".sh"):
        pattern = r"^\s*(?:function\s+)?%s\s*\(\)" % quoted
    else:
        pattern = r"(?<![\w-])%s(?![\w-])" % quoted
    return re.search(pattern, text, re.M) is not None


def has_word(text, word):
    return re.search(r"(?<![\w-])%s(?![\w-])" % re.escape(word), text) is not None


def check(data, root):
    problems = []
    cache = {}

    def body(path):
        if path not in cache:
            try:
                with open(os.path.join(root, path), encoding="utf-8", errors="replace") as handle:
                    cache[path] = handle.read()
            except OSError:
                cache[path] = None
        return cache[path]

    resolved = resolve(data)
    for provider, cells in resolved.items():
        for function, cell in cells.items():
            where = "%s/%s" % (provider, function)
            if not isinstance(cell, dict) or cell.get("status") not in STATUSES:
                problems.append("%s: missing or invalid status" % where)
                continue
            if cell.get("verified") not in VERIFIED:
                problems.append("%s: verified must be fixture or live" % where)
            if cell["status"] in ("supported", "partial"):
                if cell["status"] == "partial" and not cell.get("limit"):
                    problems.append("%s: partial needs a limit" % where)
                for kind in ("impl", "test"):
                    if not cell.get(kind):
                        problems.append("%s: no %s evidence" % (where, kind))
                for entry in cell.get("impl") or []:
                    path, symbol = split(entry)
                    text = body(path)
                    if not symbol or text is None or not has_definition(text, path, symbol):
                        problems.append("%s: implementation not found: %s" % (where, entry))
                for entry in cell.get("test") or []:
                    path, word = split(entry)
                    text = body(path)
                    if not word or text is None or not has_word(text, word):
                        problems.append("%s: test not found: %s" % (where, entry))
    return problems


def clean(value):
    return "".join(ch if ch.isprintable() else "?" for ch in str(value))


def text_report(data):
    resolved = resolve(data)
    functions = [row["id"] for row in data["functions"]]
    lines = ["provider capability matrix (schema 1)",
             "legend: sup=supported par=partial uns=unsupported unk=unknown; /f=fixture-tested /L=live-verified",
             "fixture means not verified against a live CLI; the table is read from config/provider-capabilities.json."]
    width = max(len(name) for name in resolved) + 1
    lines.append("%-*s%s" % (width, "", " ".join("%-8s" % COLUMNS.get(fn, fn[:8]) for fn in functions)))
    notes = {}
    for provider, cells in resolved.items():
        parts = []
        for fn in functions:
            cell = cells[fn]
            mark = "%s/%s" % (SHORT[cell["status"]], "L" if cell["verified"] == "live" else "f")
            parts.append("%-8s" % mark)
            text = cell.get("limit") or cell.get("note")
            if text and cell["status"] != "supported":
                notes.setdefault((fn, cell["status"], clean(text)), []).append(provider)
        lines.append("%-*s%s" % (width, provider, " ".join(parts)))
    lines.append("limits and notes (partial, unsupported and unknown cells):")
    for (fn, status, text), providers in sorted(notes.items()):
        if len(providers) > 6:
            who = "%s and %d more" % (", ".join(providers[:3]), len(providers) - 3)
        else:
            who = ", ".join(providers)
        lines.append("  %s %s [%s]: %s" % (COLUMNS.get(fn, fn), SHORT[status], who, text))
    return "\n".join(lines)


def json_report(data):
    return {"schema": 1, "statuses": data["statuses"],
            "verified": {"fixture": "exercised by fake CLIs or fixtures only, not a live CLI",
                         "live": "repo records a live verification"},
            "functions": data["functions"], "providers": resolve(data)}


def main(argv):
    as_json = do_check = False
    table = os.path.join(ROOT, "config", "provider-capabilities.json")
    root = ROOT
    args = list(argv)
    while args:
        arg = args.pop(0)
        if arg == "--json":
            as_json = True
        elif arg == "--check":
            do_check = True
        elif arg in ("--table", "--root") and args:
            value = args.pop(0)
            if arg == "--table":
                table = value
            else:
                root = value
        else:
            print("error: unknown argument: %s" % arg, file=sys.stderr)
            return 2
    try:
        data = load(table)
        resolve(data)
    except (OSError, ValueError, KeyError, TypeError, RecursionError) as error:
        print("error: provider capability table unreadable: %s" % clean(error), file=sys.stderr)
        return 1
    if do_check:
        problems = check(data, root)
        for problem in problems:
            print("fail: " + problem, file=sys.stderr)
        if not problems:
            print("ok: provider capability evidence found")
        return 1 if problems else 0
    if as_json:
        print(json.dumps(json_report(data), indent=2, sort_keys=True))
    else:
        print(text_report(data))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
