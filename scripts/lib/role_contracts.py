#!/usr/bin/env python3
"""Lint config/role-contracts.json against roles/*.md and the panel routing tables.

The contract is descriptive input for later routing work; the only runtime reader is
peer-delegate.sh --role (via --role-access), which defaults a bundled read-only role to read access.
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
                     # peer-delegate.sh --role launches a bundled read-only role with read access
                     # unless OMS_DELEGATE_ROLE_WRITE=1 (or --apply) asks for the old write default.
                     "delegate_default": "read" if entry.get("read_only") is True else "write"})
    return rows
# Role files with no entry of their own in config/panel-routing.json `roles`: the panel never
# allocates them by name, so they take the tier of the routing key they dispatch through
# (custom-skills/oms-agent-harness/references/roles.md, "Panel front door"; for the worker
# profiles panel_routing.command picks the file by purpose). A routing key with no role file
# is allowed only as the target of one of these aliases.
ROUTING_ALIASES = {
    "analysis-worker": "worker",
    "implementation-worker": "worker",
    "patch-reviewer": "reviewer",
    "repo-auditor": "researcher",
    "test-designer": "advisor",
    "decision-advisor": "advisor",
}


def routing_tables(root):
    """(writer roles, role -> board kind) from the live routing module of this checkout."""
    sys.path.insert(0, str(root / "scripts/lib"))
    try:
        import panel_routing
    finally:
        sys.path.pop(0)
    return set(panel_routing.WRITERS), dict(panel_routing.KINDS)


def routing_names(root):
    """Role names keyed in config/panel-routing.json, or None when the file is unreadable."""
    try:
        roles = json.loads((root / "config/panel-routing.json").read_text(encoding="utf-8"))["roles"]
    except (OSError, ValueError, KeyError, TypeError):
        return None
    return set(roles) if isinstance(roles, dict) else None


def check_routing(files, routing, aliases):
    errors = []
    for name in sorted(set(files) - routing):
        if name not in aliases:
            errors.append(f"role file without routing entry or alias: {name}")
    for name, target in sorted(aliases.items()):
        if name not in files:
            errors.append(f"routing alias without role file: {name}")
        if name in routing:
            errors.append(f"routing alias also has a routing entry: {name}")
        if target not in routing:
            errors.append(f"routing alias {name} targets a key missing from routing roles: {target}")
    for name in sorted(routing - set(files)):
        if name not in set(aliases.values()):
            errors.append(f"routing entry without role file or alias: {name}")
    return errors


def check(root=ROOT, writers=None, kinds=None, aliases=None):
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
    routing = routing_names(root)
    if routing is None:
        errors.append("panel-routing.json roles unreadable")
    else:
        errors += check_routing(files, routing, ROUTING_ALIASES if aliases is None else aliases)
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
    lines.append("belt/tools/mcp: claude launch at the contract's own access; delegate-default is the access "
                 "peer-delegate.sh --role gives a bundled role when no access flag is passed (read for read-only "
                 "roles; OMS_DELEGATE_ROLE_WRITE=1 restores write)")
    return "\n".join(lines)


def role_access(name, path=None):
    """'read' when the contract marks the role read_only, else 'write'. Raises on an unreadable contract."""
    data = json.loads((Path(path) if path else ROOT / "config/role-contracts.json").read_text(encoding="utf-8"))
    entry = data["roles"].get(name)
    return "read" if isinstance(entry, dict) and entry.get("read_only") is True else "write"


def main(argv):
    if len(argv) == 2 and argv[0] == "--role-access":
        try:
            print(role_access(argv[1]))
        except (OSError, ValueError, KeyError, AttributeError) as exc:
            print(f"role-contracts: unreadable: {exc}", file=sys.stderr)
            return 1
        return 0
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
