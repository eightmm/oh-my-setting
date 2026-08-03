#!/usr/bin/env python3
"""Read-only MCP server over the harness state of one repository.

Exposes the shared state another agent surface needs to resume work — the
prioritized inbox, task packet, fail-ledger, handoff digests, Work Journal — as MCP tools, so every
MCP client (Claude Code, Codex, Antigravity, IDEs) reads the same state
without per-CLI hook code. Strictly read-only: each tool maps to a fixed
read-only subcommand argv; nothing here can write harness state.

Transport: stdio, newline-delimited JSON-RPC 2.0 (the MCP stdio framing).
Stdlib only — this runs wherever the harness runs.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FALLBACK_PROTOCOL = "2025-06-18"
# Revisions whose semantics this server actually implements. Echoing an
# arbitrary requested revision back would advertise conformance to behavior
# (newer handshakes, task extensions) it has never implemented.
SUPPORTED_PROTOCOLS = ("2025-06-18", "2025-03-26")
OUTPUT_LIMIT = 60_000


def server_version() -> str:
    try:
        return (ROOT / "VERSION").read_text(encoding="utf-8").strip()
    except OSError:
        return "unknown"


REPO_PROPERTY = {
    "repo": {
        "type": "string",
        "description": (
            "Repository whose harness state to read (default: the server's"
            " working directory)."
        ),
    }
}

TOOLS = [
    {
        "name": "oms_inbox",
        "description": (
            "Prioritized read-only attention queue derived from all shared"
            " harness state, with one exact next command per item."
        ),
        "argv": ["bash", "scripts/inbox.sh", "--json"],
        "properties": REPO_PROPERTY,
    },
    {
        "name": "oms_task_state",
        "description": (
            "Current task packet of the repository: id, status, last"
            " activity, staleness, and whether verification ran."
        ),
        "argv": ["bash", "scripts/agent-task.sh", "status", "--json"],
        "properties": REPO_PROPERTY,
    },
    {
        "name": "oms_fail_ledger",
        "description": (
            "Recorded command failures keyed by fingerprint, with resolved"
            " state — what already failed here, so it is not retried blind."
        ),
        "argv": ["bash", "scripts/fail-ledger.sh", "list", "--json"],
        "properties": REPO_PROPERTY,
    },
    {
        "name": "oms_handoffs",
        "description": (
            "Captured session-handoff digests for the repository, newest"
            " first (paths and sizes; read a digest with oms_handoff_show)."
        ),
        "argv": ["bash", "scripts/session-handoff.sh", "list", "--json"],
        "properties": REPO_PROPERTY,
    },
    {
        "name": "oms_handoff_show",
        "description": "Content of one captured handoff digest by file name.",
        "argv": ["bash", "scripts/session-handoff.sh", "show"],
        "properties": {
            **REPO_PROPERTY,
            "file": {
                "type": "string",
                "description": "Digest file name or path from oms_handoffs.",
            },
        },
        "required": ["file"],
        "positional": "file",
    },
    {
        "name": "oms_journal",
        "description": (
            "Work Journal summary for the repository: today's digest by"
            " default, or open blockers with view='blockers'."
        ),
        "argv": ["bash", "scripts/journal.sh", "show", "--json"],
        "properties": {
            **REPO_PROPERTY,
            "view": {
                "type": "string",
                "enum": ["today", "blockers"],
                "description": "Which journal view to return (default today).",
            },
        },
    },
]


def tool_definitions() -> list[dict]:
    defs = []
    for tool in TOOLS:
        defs.append(
            {
                "name": tool["name"],
                "description": tool["description"],
                "inputSchema": {
                    "type": "object",
                    "properties": tool["properties"],
                    "required": tool.get("required", []),
                },
            }
        )
    return defs


def run_tool(tool: dict, arguments: dict) -> tuple[str, bool]:
    repo = str(arguments.get("repo") or Path.cwd())
    repo_path = Path(repo)
    if not repo_path.is_dir():
        return "error: no such repository directory: %s" % repo, True
    argv = [
        arg if arg == "bash" else str(ROOT / arg) if arg.startswith("scripts/") else arg
        for arg in tool["argv"]
    ]
    if tool["name"] == "oms_journal" and arguments.get("view") == "blockers":
        argv.append("--blockers")
    positional = tool.get("positional")
    if positional:
        value = str(arguments.get(positional, ""))
        # Bare digest names only: this tool reads .oms/handoffs/<name>, and a
        # path here would turn a state reader into an arbitrary-file reader.
        if not value or value.startswith("-") or "/" in value or "\\" in value:
            return "error: %s requires a bare digest file name" % tool["name"], True
        argv.append(value)
    try:
        proc = subprocess.run(
            argv,
            cwd=str(repo_path),
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return "error: %s" % exc, True
    output = proc.stdout if proc.returncode == 0 else (
        proc.stdout + ("\n" if proc.stdout and proc.stderr else "") + proc.stderr
    )
    output = output.strip() or ("exit %d" % proc.returncode)
    if len(output) > OUTPUT_LIMIT:
        output = output[:OUTPUT_LIMIT] + "\n[truncated]"
    return output, proc.returncode != 0


def response(msg_id, result=None, error=None) -> dict:
    if error is not None:
        return {"jsonrpc": "2.0", "id": msg_id, "error": error}
    return {"jsonrpc": "2.0", "id": msg_id, "result": result}


def handle(message: dict):
    method = message.get("method")
    msg_id = message.get("id")
    if method is None or not isinstance(method, str):
        return None
    if msg_id is None:
        return None  # notification (e.g. notifications/initialized)
    if method == "initialize":
        params = message.get("params") or {}
        requested = params.get("protocolVersion")
        if isinstance(requested, str) and requested in SUPPORTED_PROTOCOLS:
            protocol = requested
        else:
            protocol = FALLBACK_PROTOCOL
        return response(
            msg_id,
            {
                "protocolVersion": protocol,
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": "oh-my-setting", "version": server_version()},
            },
        )
    if method == "ping":
        return response(msg_id, {})
    if method == "tools/list":
        return response(msg_id, {"tools": tool_definitions()})
    if method == "tools/call":
        params = message.get("params") or {}
        name = params.get("name")
        arguments = params.get("arguments") or {}
        for tool in TOOLS:
            if tool["name"] == name:
                text, is_error = run_tool(tool, arguments)
                return response(
                    msg_id,
                    {
                        "content": [{"type": "text", "text": text}],
                        "isError": is_error,
                    },
                )
        return response(
            msg_id, error={"code": -32602, "message": "unknown tool: %r" % name}
        )
    return response(
        msg_id, error={"code": -32601, "message": "method not found: %s" % method}
    )


def main() -> int:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            reply = response(
                None, error={"code": -32700, "message": "parse error"}
            )
            print(json.dumps(reply), flush=True)
            continue
        reply = handle(message)
        if reply is not None:
            print(json.dumps(reply, ensure_ascii=False), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
