#!/usr/bin/env python3
"""Lint config/role-contracts.json against roles/*.md and the panel routing tables.

The contract is descriptive input for later routing work; nothing enforces it at runtime.
"""

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FIELDS = {"read_only", "writes", "returns", "may_delegate"}
WRITES = {"none", "scratch", "assigned-paths", "assigned-docs", "integration-paths"}
READ_ONLY_WRITES = {"none", "scratch"}
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
    return errors


def main(argv):
    errors = check()
    for line in errors:
        print(f"role-contracts: {line}", file=sys.stderr)
    if not errors:
        print("role-contracts: ok")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
