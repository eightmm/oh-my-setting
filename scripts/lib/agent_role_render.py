#!/usr/bin/env python3
"""Read-only renderer: one role file -> a provider-native subagent definition on stdout.

The role markdown stays the single source. Nothing is written or installed;
the caller redirects the output if it wants a file. Access is never broader
than the role: only the bundled write roles below get edit tools or a
workspace-write sandbox. Every other role, and any project or global override,
renders read-only.
"""

import argparse
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import panel_routing  # noqa: E402

PROVIDERS = ("claude", "codex")
# Bundled roles whose prose grants editing; panel WRITERS plus the plain implementation role.
WRITE_ROLES = frozenset(panel_routing.WRITERS | {"implementation-worker"})
CLAUDE_READ_TOOLS = "Read, Grep, Glob"
CLAUDE_WRITE_TOOLS = "Read, Grep, Glob, Edit, Write, Bash"


def description(text, name):
    """First Mandate paragraph or bullet, joined to one line; the title as a last resort."""
    match = re.search(r"^## Mandate\s*\n(.*?)(?=^## |\Z)", text, re.S | re.M)
    if match:
        block = match.group(1).strip()
    else:
        # No Mandate heading: first prose paragraph that is not the title or the NAME-STRATEGY marker.
        paras = [p.strip() for p in re.split(r"\n\s*\n", text) if p.strip()]
        block = next((p for p in paras if not p.startswith("#") and not re.fullmatch(r"[A-Z0-9-]+-STRATEGY", p)), "")
    first = re.split(r"\n\s*\n", block)[0] if block else ""
    line = " ".join(part.strip() for part in first.splitlines()).lstrip("- ").strip()
    return line or "OMS %s role" % name


def route_for(policy, role, provider):
    row = policy["roles"].get(role)
    if not row:
        return None
    for route in policy["tiers"][row["tier"]]:
        if route["provider"] == provider:
            return route
    return None


def toml_multiline(value):
    escaped = value.replace("\\", "\\\\").replace('"""', '\\"\\"\\"')
    if re.search(r"[\x00-\x08\x0b-\x1f\x7f]", escaped):
        raise ValueError("role text holds a control character")
    return '"""\n' + escaped.rstrip("\n") + '\n"""'


def render(provider, role, text, writable, policy):
    name = "oms-" + role
    desc = description(text, role)
    body = text.rstrip("\n") + "\n"
    route = route_for(policy, role, provider)
    if provider == "claude":
        lines = ["---", "name: " + name, "description: " + json.dumps(desc, ensure_ascii=False),
                 "tools: " + (CLAUDE_WRITE_TOOLS if writable else CLAUDE_READ_TOOLS)]
        if route:
            lines.append("model: " + route["model"])
        return "\n".join(lines + ["---", "", body])
    lines = ["name = " + json.dumps(name), "description = " + json.dumps(desc, ensure_ascii=False)]
    if route:
        lines += ["model = " + json.dumps(route["model"]),
                  "model_reasoning_effort = " + json.dumps(route["effort"])]
    lines.append("sandbox_mode = " + json.dumps("workspace-write" if writable else "read-only"))
    lines.append("developer_instructions = " + toml_multiline(body))
    return "\n".join(lines) + "\n"


def main(argv):
    parser = argparse.ArgumentParser(prog="agent-role render")
    parser.add_argument("--provider", required=True, choices=PROVIDERS)
    parser.add_argument("--role", required=True)
    parser.add_argument("--file", required=True)
    parser.add_argument("--bundled", action="store_true", help="the file is the bundled role, not an override")
    args = parser.parse_args(argv)
    text = Path(args.file).read_text(encoding="utf-8")
    writable = args.bundled and args.role in WRITE_ROLES
    sys.stdout.write(render(args.provider, args.role, text, writable, panel_routing.policy()))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
