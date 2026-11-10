#!/usr/bin/env python3
"""Lint config/role-contracts.json against roles/*.md and the panel routing tables.

The contract is descriptive input for later routing work; nothing enforces it at runtime.
"""

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FIELDS = {"read_only", "writes", "returns", "may_delegate"}
WRITES = {"none", "scratch", "assigned-paths", "assigned-docs", "integration-paths"}
READ_ONLY_WRITES = {"none", "scratch"}
# Claude `--tools` belts that scripts/lib/peer-common.sh passes to a read seat. The Bash member is
# the read belt's shell: plan mode and the sandbox bound it, the belt does not, so only tools that
# write or spawn by name are linted. Write launches pass no --tools and keep the default surface.
READ_BELT = ("Read", "Grep", "Glob", "Bash")
WEB_BELT = READ_BELT + ("WebSearch", "WebFetch")
WEB_ROLES = {"researcher"}  # peer-common.sh gives web tools to this role only, under panel dispatch
WRITE_TOOLS = {"Write", "Edit", "MultiEdit", "NotebookEdit", "Agent", "Task"}
BELT_SOURCE = "scripts/lib/peer-common.sh"


def belt_for(name, entry):
    """(belt label, tools or None for the provider default, MCP on) when the role launches with its contract access."""
    if entry.get("read_only") is not True:
        return "default", None, True
    if name in WEB_ROLES:
        return "read+web", WEB_BELT, False
    return "read", READ_BELT, False


def source_belts(root):
    """The `--tools "..."` literals in the launcher, as tuples; None when the file is unreadable."""
    try:
        text = (Path(root) / BELT_SOURCE).read_text(encoding="utf-8")
    except OSError:
        return None
    return [tuple(m.split(",")) for m in re.findall(r'--tools "([^"]*)"', text)]


def surface(contracts):
    """One row per role: what a launch at the contract's own access resolves to."""
    rows = []
    for name, entry in sorted(contracts.items()):
        label, tools, mcp = belt_for(name, entry)
        rows.append({"role": name, "read_only": entry.get("read_only"), "writes": entry.get("writes"),
                     "belt": label, "tools": None if tools is None else len(tools), "mcp": mcp,
                     # peer-delegate.sh defaults to write access; only --read-only narrows it.
                     "delegate_default": "default" if entry.get("read_only") else "same"})
    return rows


def routing_tables(root):
    """(writer roles, role -> board kind) from the live routing module of this checkout."""
    sys.path.insert(0, str(root / "scripts/lib"))
    try:
        import panel_routing
    finally:
        sys.path.pop(0)
    return set(panel_routing.WRITERS), dict(panel_routing.KINDS)


def check(root=ROOT, writers=None, kinds=None):
    root = Path(root)
    errors = []
    try:
        data = json.loads((root / "config/role-contracts.json").read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        return [f"role-contracts.json unreadable: {exc}"]
    if not isinstance(data, dict) or data.get("schema") != 1 or set(data) != {"schema", "roles"} \
            or not isinstance(data["roles"], dict):
        return ["role-contracts.json must be {schema: 1, roles: {...}}"]
    contracts = data["roles"]
    if writers is None or kinds is None:
        writers, kinds = routing_tables(root)
    files = {p.stem: p for p in sorted((root / "roles").glob("*.md"))}
    for name in sorted(set(files) - set(contracts)):
        errors.append(f"role file without contract entry: {name}")
    for name in sorted(set(contracts) - set(files)):
        errors.append(f"contract entry without role file: {name}")
    for name, entry in sorted(contracts.items()):
        if not isinstance(entry, dict) or set(entry) != FIELDS:
            errors.append(f"{name}: fields must be exactly {sorted(FIELDS)}")
            continue
        if type(entry["read_only"]) is not bool:
            errors.append(f"{name}: read_only must be a boolean")
        if entry["writes"] not in WRITES:
            errors.append(f"{name}: writes must be one of {sorted(WRITES)}")
        elif type(entry["read_only"]) is bool and (entry["writes"] in READ_ONLY_WRITES) != entry["read_only"]:
            errors.append(f"{name}: read_only={entry['read_only']} contradicts writes={entry['writes']}")
        if entry["may_delegate"] is not False:
            errors.append(f"{name}: may_delegate must be false")
        returns = entry["returns"]
        if not isinstance(returns, list) or not all(isinstance(r, str) and r for r in returns):
            errors.append(f"{name}: returns must be a list of non-empty strings")
        elif name in files:
            text = files[name].read_text(encoding="utf-8")
            for item in returns:
                if item not in text:
                    errors.append(f"{name}: returns entry not present in the role file: {item}")
        if entry["read_only"] is True and name in writers:
            errors.append(f"{name}: read-only role is listed as a writer in panel routing")
        if entry["read_only"] is False and name in kinds and name not in writers:
            errors.append(f"{name}: writer role is missing from panel routing WRITERS")
        if entry["read_only"] is True:
            _, tools, _ = belt_for(name, entry)
            bad = sorted(set(tools) & WRITE_TOOLS)
            if bad:
                errors.append(f"{name}: read-only role resolves to a belt with write tools: {', '.join(bad)}")
    belts = source_belts(root)
    if belts is None:
        errors.append(f"{BELT_SOURCE} unreadable: tool belts cannot be checked")
    else:
        if sorted(belts) != sorted([READ_BELT, WEB_BELT]):
            errors.append(f"{BELT_SOURCE} --tools belts differ from the table in role_contracts.py: {belts}")
        for belt in belts:
            bad = sorted(set(belt) & WRITE_TOOLS)
            if bad:
                errors.append(f"{BELT_SOURCE}: read belt contains write tools: {', '.join(bad)}")
    return errors


def report(contracts, as_json):
    rows = surface(contracts)
    if as_json:
        return json.dumps({"schema": 1, "roles": rows}, indent=2)
    lines = [f"{'role':<22} {'read_only':<9} {'belt':<9} {'tools':<7} {'mcp':<4} delegate-default"]
    for r in rows:
        tools = "default" if r["tools"] is None else str(r["tools"])
        lines.append(f"{r['role']:<22} {str(r['read_only']).lower():<9} {r['belt']:<9} {tools:<7} "
                     f"{'on' if r['mcp'] else 'off':<4} {r['delegate_default']}")
    lines.append("belt/tools/mcp: claude launch at the contract's own access; delegate-default 'default' marks a "
                 "read-only role that peer-delegate.sh launches with the full surface unless --read-only is passed")
    return "\n".join(lines)


def main(argv):
    if argv and argv[0] in ("--surface", "--surface-json"):
        try:
            data = json.loads((ROOT / "config/role-contracts.json").read_text(encoding="utf-8"))
            print(report(data["roles"], argv[0] == "--surface-json"))
        except (OSError, ValueError, KeyError) as exc:
            print(f"role-contracts: unreadable: {exc}", file=sys.stderr)
            return 1
        return 0
    errors = check()
    for line in errors:
        print(f"role-contracts: {line}", file=sys.stderr)
    if not errors:
        print("role-contracts: ok")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
