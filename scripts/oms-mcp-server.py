#!/usr/bin/env python3
"""MCP server over the harness state of one repository.

Exposes the shared state another agent surface needs to resume work — the
prioritized inbox, task packet, fail-ledger, handoff digests, Work Journal — as MCP tools, so every
MCP client (Claude Code, Codex, Antigravity, IDEs) reads the same state
without per-CLI hook code. Every state tool maps to a fixed read-only
subcommand argv.

The write surface is exactly one thing: oms_peer_start launches a peer
consultation (consult, advise, ask), which writes what those verbs always
write — its own artifacts, thread turns, and a run directory under
.oms/artifacts/mcp/. It cannot modify repository files: the peer runs a
read-only pass, and no tool here edits, stages, or commits anything.
Consultations run for minutes, far longer than one tool call may block, so
they are started detached and polled with oms_peer_result. The server keeps
no run state of its own; a poll reads only the filesystem, so any client
process can poll a run another one started — and oms_peer_operations lists
what is on disk, so an id that lived only in a closed conversation is not
how an answer becomes unreachable.

Transport: stdio, newline-delimited JSON-RPC 2.0 (the MCP stdio framing).
Stdlib only — this runs wherever the harness runs.
"""

from __future__ import annotations

import json
import os
import re
import stat
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FALLBACK_PROTOCOL = "2025-06-18"
# Revisions whose semantics this server actually implements. Echoing an
# arbitrary requested revision back would advertise conformance to behavior
# (newer handshakes, task extensions) it has never implemented.
SUPPORTED_PROTOCOLS = ("2026-07-28", "2025-06-18", "2025-03-26")
TASKS_PROTOCOL = "2026-07-28"
TASKS_EXTENSION = "io.modelcontextprotocol/tasks"
# 2026-07-28 dropped the initialize handshake: a modern client names its
# revision on every request instead, so a server that learned the revision
# only from initialize pinned such a client to the legacy fallback for the
# life of the process. The per-request field wins wherever it is present; the
# negotiated session value serves clients that still open with initialize.
META_PROTOCOL = "io.modelcontextprotocol/protocolVersion"
META_CLIENT_CAPABILITIES = "io.modelcontextprotocol/clientCapabilities"
META_SERVER_INFO = "io.modelcontextprotocol/serverInfo"
UNSUPPORTED_PROTOCOL_CODE = -32022
# Freshness hint on cacheable list results; the tool set is fixed per process.
LIST_TTL_MS = 300_000
SESSION_PROTOCOL = FALLBACK_PROTOCOL
OUTPUT_LIMIT = 60_000
MAX_REQUEST_BYTES = 256 * 1024
MAX_PROMPT_BYTES = 64 * 1024
MAX_PATH_BYTES = 4 * 1024
MAX_ARGUMENT_BYTES = 16 * 1024

# Peer consultation: which front-door verb each kind runs, and where that verb
# writes its answer artifacts. Started detached, polled from disk.
PEER_KINDS = {
    "consult": {"script": "scripts/consult.sh", "artifacts": "consult"},
    "advise": {"script": "scripts/advise.sh", "artifacts": "advise"},
    "ask": {"script": "scripts/peer-ask.sh", "artifacts": "ask"},
}
PROVIDER_REGISTRY = ROOT / "scripts/lib/provider-registry.sh"
RUN_ROOT = Path(".oms/artifacts/mcp")
OPERATION_RE = re.compile(r"[a-z]+-[0-9TZ]+-[0-9a-f]+\Z")
LOG_TAIL_LINES = 20
LOG_TAIL_LIMIT = 4_000
OPERATIONS_LIMIT = 20
TITLE_LIMIT = 160
# A thread id reaches the verb as argv, so only the shape thread.sh mints
# gets through, and never one that could read as a flag of its own.
THREAD_RE = re.compile(r"[A-Za-z0-9._][A-Za-z0-9._-]{0,127}\Z")
# The consultation outlives this server: the inner shell is backgrounded so the
# launcher exits at once and the run is reparented to init, and the run's own
# exit code lands in a status file only after the verb returns. That file is the
# completion signal — a verb can fail (no peer CLI installed, bad target) before
# any artifact exists, and waiting on artifacts alone would poll forever.
# The pid of that inner shell is recorded the same way, because a status file
# that never appears is ambiguous: still working, or died without writing one.
LAUNCH_WRAPPER = (
    'status="$1"; pid="$2"; shift 2; '
    '{ rc=0; "$@" || rc=$?; printf "%s\\n" "$rc" > "$status.part" && '
    'mv "$status.part" "$status"; } & '
    'printf "%s\\n" "$!" > "$pid.part" && mv "$pid.part" "$pid"'
)


def server_version() -> str:
    try:
        return (ROOT / "VERSION").read_text(encoding="utf-8").strip()
    except OSError:
        return "unknown"


def tasks_feature_enabled() -> bool:
    return os.environ.get("OMS_MCP_TASKS_EXTENSION") == "1"


def request_meta(params: object) -> dict:
    if isinstance(params, dict):
        meta = params.get("_meta")
        if isinstance(meta, dict):
            return meta
    return {}


def requested_protocol(params: object):
    """The revision a modern client names on this request; None when absent."""
    value = request_meta(params).get(META_PROTOCOL)
    return value if isinstance(value, str) else None


def effective_protocol(params: object) -> str:
    requested = requested_protocol(params)
    if requested in SUPPORTED_PROTOCOLS:
        return requested
    return SESSION_PROTOCOL


def server_info() -> dict:
    return {"name": "oh-my-setting", "version": server_version()}


def server_capabilities(protocol: str) -> dict:
    capabilities = {"tools": {"listChanged": False}}
    if tasks_feature_enabled() and protocol == TASKS_PROTOCOL:
        capabilities["extensions"] = {TASKS_EXTENSION: {}}
    return capabilities


def task_request_capable(params: object) -> bool:
    if not tasks_feature_enabled() or effective_protocol(params) != TASKS_PROTOCOL:
        return False
    capabilities = request_meta(params).get(META_CLIENT_CAPABILITIES)
    if not isinstance(capabilities, dict):
        return False
    extensions = capabilities.get("extensions")
    return isinstance(extensions, dict) and TASKS_EXTENSION in extensions


def complete_result(value: dict, params: object = None) -> dict:
    """Add the post-2026 result discriminator without changing old clients."""
    if effective_protocol(params) != TASKS_PROTOCOL or "resultType" in value:
        return value
    return {"resultType": "complete", **value}


def cacheable(value: dict, params: object) -> dict:
    """List results carry a freshness hint only where the revision defines one."""
    if effective_protocol(params) != TASKS_PROTOCOL:
        return value
    return {**value, "ttlMs": LIST_TTL_MS, "cacheScope": "private"}


REPO_PROPERTY = {
    "repo": {
        "type": "string",
        "description": (
            "Repository whose harness state to read (default: the server's"
            " working directory)."
        ),
    }
}

# Tool annotations are hints a client uses to decide what to auto-approve;
# they are not enforcement, and this server's own refusals stay authoritative.
# Every reader here runs one fixed read-only subcommand against local state.
READ_ONLY = {
    "readOnlyHint": True,
    "destructiveHint": False,
    "idempotentHint": True,
    "openWorldHint": False,
}
# Starting a consultation spends another agent's wall clock and money and
# writes artifacts, and calling it twice starts two runs. It reaches outside
# this machine's state through the provider CLI, but it destroys nothing.
START_PEER = {
    "readOnlyHint": False,
    "destructiveHint": False,
    "idempotentHint": False,
    "openWorldHint": True,
}
# Graph ids are structured, not bare names: a project-graph node carries its
# path and qualified name (symbol:scripts/plan-run.sh::main), so the bare-name
# rule below would refuse every real id. This is the widest shape such an id
# takes, and nothing more — it cannot begin with a dash, and check_positional
# refuses a '..' segment inside it.
GRAPH_ID_PATTERN = r"^[A-Za-z0-9][A-Za-z0-9._:/@-]{0,511}$"
# A search query may carry spaces between words; it still cannot lead with a
# dash or climb a path, which check_positional enforces for every positional.
GRAPH_QUERY_PATTERN = r"^[A-Za-z0-9][A-Za-z0-9._:/@ -]{0,511}$"

TOOLS = [
    {
        "name": "oms_inbox",
        "description": (
            "Prioritized read-only attention queue derived from all shared"
            " harness state, with one exact next command per item."
        ),
        "argv": ["bash", "scripts/inbox.sh", "--json"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_repo_state",
        "description": (
            "Complete read-only repository control-plane projection, including"
            " attempts, approvals, plans, runs, artifacts, CI, and recovery."
        ),
        "argv": ["bash", "scripts/state.sh", "--json"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_agent_operations",
        "description": (
            "Durable lifecycle projection of recent supervised and direct"
            " agent attempts (most recent 40), including state, usage,"
            " lineage, and budgets."
        ),
        # The lifecycle ledger is append-only and never pruned; unbounded, the
        # projection outgrows OUTPUT_LIMIT and the character cut below turns
        # every call into unparseable JSON. Recent attempts are the question
        # this tool answers.
        "argv": ["bash", "scripts/agent-events.sh", "list", "--json", "--limit", "40"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_approvals",
        "description": (
            "Public projection of pending one-time approvals; grant secrets"
            " and private hashes are never returned."
        ),
        "argv": ["bash", "scripts/approval-inbox.sh", "list", "--pending", "--json"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_task_state",
        "description": (
            "Current task packet of the repository: id, status, last"
            " activity, staleness, and whether verification ran."
        ),
        "argv": ["bash", "scripts/agent-task.sh", "status", "--json"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_fail_ledger",
        "description": (
            "Recorded command failures keyed by fingerprint, with resolved"
            " state — what already failed here, so it is not retried blind."
            " The 30 most recently active fingerprints; the JSON says how"
            " many older ones were omitted."
        ),
        "argv": ["bash", "scripts/fail-ledger.sh", "list", "--json", "--limit", "30"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_handoffs",
        "description": (
            "Captured session-handoff digests for the repository, newest"
            " first (paths and sizes; read a digest with oms_handoff_show)."
        ),
        "argv": ["bash", "scripts/session-handoff.sh", "list", "--json"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
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
        "annotations": READ_ONLY,
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
        "annotations": READ_ONLY,
    },
    # Typed runtime readers: fixed argv onto the runtime front door, structured
    # failures passed through unchanged, and no mutation, promotion, apply, or
    # landing surface — runtime state is projection or evidence only.
    {
        "name": "oms_runtime_release",
        "description": (
            "Advisory update-channel projection from the checkout's own"
            " manifest: stable and edge by exact commit, readiness, and the"
            " apply command. Read-only; nothing here applies an update."
        ),
        "argv": ["bash", "scripts/runtime.sh", "release", "status"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_runtime_profile",
        "description": (
            "Current capability-profile selection and readiness: requested"
            " profiles, required and missing commands, and whether the"
            " selection is configured by a receipt."
        ),
        "argv": ["bash", "scripts/runtime.sh", "profile", "current"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_runtime_failures",
        "description": (
            "Canonical failure taxonomy: every failure code with its recovery"
            " action, retryability, and escalation flags, as fail-ledger rows"
            " and typed refusals use them."
        ),
        "argv": ["bash", "scripts/runtime.sh", "failure", "catalog"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    # Action tools: no argv, dispatched through ACTIONS below.
    {
        "name": "oms_peer_start",
        "description": (
            "Start a consultation with a registered local agent CLI in the"
            " background and return immediately with an"
            " operation id. kind='consult' asks a peer mid-task and keeps the"
            " exchange in a thread; kind='advise' is an adversarial review of"
            " a decision; kind='ask' puts the same question to every installed"
            " peer. The peer keeps running after this call returns and often"
            " takes 5-25 minutes. Read the answer with oms_peer_result; do"
            " other work between polls instead of waiting."
        ),
        "properties": {
            **REPO_PROPERTY,
            "kind": {
                "type": "string",
                "enum": sorted(PEER_KINDS),
                "description": (
                    "consult (ask a peer mid-task), advise (adversarial review"
                    " of a decision), or ask (every peer answers)."
                ),
            },
            "prompt": {
                "type": "string",
                "description": (
                    "The question. Include the decision, the evidence, and the"
                    " alternatives — the peer sees no conversation but this."
                ),
            },
            "providers": {
                "type": "string",
                "description": (
                    "Comma list of registered targets, each"
                    " optionally pinned as PROVIDER:model=NAME. Default: the"
                    " verb's own pick, which excludes the calling agent."
                    " advise takes one target."
                ),
            },
            "thread": {
                "type": "string",
                "description": (
                    "Continue this conversation: the thread id a finished"
                    " oms_peer_result reported. Default: the repository's"
                    " current thread, so follow-ups keep their context."
                ),
            },
            "new_thread": {
                "type": "boolean",
                "description": (
                    "Start a fresh conversation instead of continuing the"
                    " current one — a new topic, not a follow-up."
                    " kind='consult' only."
                ),
            },
        },
        "required": ["kind", "prompt"],
        "annotations": START_PEER,
    },
    {
        "name": "oms_peer_result",
        "description": (
            "Read a consultation started by oms_peer_start. While the peer is"
            " still working this returns status='running' with the tail of its"
            " log; when it finishes, status='done' with the answer, the exit"
            " code, the artifact paths, and the thread id to continue from."
            " status='stalled' means the run's process is gone without an"
            " exit: no answer is coming. Polling is cheap and never blocks,"
            " but the run takes minutes — do other work between polls."
        ),
        "properties": {
            **REPO_PROPERTY,
            "operation": {
                "type": "string",
                "description": "Operation id returned by oms_peer_start.",
            },
        },
        "required": ["operation"],
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_peer_operations",
        "description": (
            "Consultations this repository has started, newest first, with"
            " status, kind, targets, question, and thread. The operation id"
            " lives on disk, not in one conversation: this is how a later"
            " session — or another client entirely — finds a run started"
            " elsewhere and reads its answer with oms_peer_result."
        ),
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    # Graph readers: the project graph answers what depends on what, the
    # execution graph answers where a run stands and which route is legal
    # next. Both are fixed read-only subcommands onto the graph front door;
    # nothing here builds, rebuilds, or advances anything.
    {
        "name": "oms_project_graph_map",
        "description": (
            "Read-only project-graph overview: counts by kind and language,"
            " hubs, module groups (requires a prior `oms graph project"
            " build`)."
        ),
        "argv": ["bash", "scripts/graph.sh", "project", "map", "--json"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_project_graph_query",
        "description": "Find project-graph nodes by name, path, or qualified name.",
        "argv": ["bash", "scripts/graph.sh", "project", "find", "--json", "--limit", "40"],
        "properties": {
            **REPO_PROPERTY,
            "query": {
                "type": "string",
                "description": (
                    "Name, path, or qualified-name fragment to match, such as"
                    " plan-run or scripts/graph.sh."
                ),
            },
        },
        "required": ["query"],
        "positional": "query",
        "positional_pattern": GRAPH_QUERY_PATTERN,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_project_graph_trace",
        "description": (
            "Trace one project-graph node's dependency edges two hops"
            " outward: what it reaches, and through which edges."
        ),
        "argv": [
            "bash", "scripts/graph.sh", "project", "trace", "--json",
            "--depth", "2", "--direction", "out",
        ],
        "properties": {
            **REPO_PROPERTY,
            "node": {
                "type": "string",
                "description": (
                    "Project-graph node id from oms_project_graph_query, such"
                    " as symbol:scripts/plan-run.sh::main."
                ),
            },
        },
        "required": ["node"],
        "positional": "node",
        "positional_pattern": GRAPH_ID_PATTERN,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_project_graph_blast",
        "description": "Dependency blast radius of the working tree's changed files.",
        "argv": ["bash", "scripts/graph.sh", "project", "blast", "--json"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_execution_graph_status",
        "description": "Latest execution-graph run: projection and next legal route.",
        "argv": ["bash", "scripts/graph.sh", "exec", "status", "--json"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_execution_graph_route",
        "description": (
            "Next legal route of one execution-graph run: which transitions"
            " its current state allows, and what each requires."
        ),
        "argv": ["bash", "scripts/graph.sh", "exec", "route", "--json", "--run"],
        "properties": {
            **REPO_PROPERTY,
            "run": {
                "type": "string",
                "description": "Run id from oms_execution_graph_status.",
            },
        },
        "required": ["run"],
        "positional": "run",
        "positional_pattern": GRAPH_ID_PATTERN,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_execution_graph_events",
        "description": (
            "Execution-graph events of one run, most recent 40: the"
            " transitions it actually took, in order."
        ),
        "argv": [
            "bash", "scripts/graph.sh", "exec", "events", "--json",
            "--limit", "40", "--run",
        ],
        "properties": {
            **REPO_PROPERTY,
            "run": {
                "type": "string",
                "description": "Run id from oms_execution_graph_status.",
            },
        },
        "required": ["run"],
        "positional": "run",
        "positional_pattern": GRAPH_ID_PATTERN,
        "annotations": READ_ONLY,
    },
]


def tool_definitions() -> list[dict]:
    defs = []
    for tool in TOOLS:
        definition = {
            "name": tool["name"],
            "description": tool["description"],
            "inputSchema": {
                "type": "object",
                "properties": tool["properties"],
                "required": tool.get("required", []),
            },
        }
        annotations = tool.get("annotations")
        if annotations:
            definition["annotations"] = dict(annotations)
        defs.append(definition)
    return defs


def text_argument(
    arguments: dict, name: str, limit: int = MAX_ARGUMENT_BYTES
) -> tuple[str, str]:
    """Read one bounded UTF-8 string without invoking arbitrary __str__."""
    value = arguments.get(name)
    if value is None:
        return "", ""
    if not isinstance(value, str):
        return "", "error: %s must be a string" % name
    try:
        size = len(value.encode("utf-8"))
    except UnicodeEncodeError:
        return "", "error: %s must be valid UTF-8 text" % name
    if size > limit:
        return "", "error: %s exceeds %d-byte limit" % (name, limit)
    if "\x00" in value:
        return "", "error: %s contains a NUL byte" % name
    return value, ""


def check_positional(tool: dict, value: str) -> str:
    """Refusal text for a positional that must not become argv, else "".

    Every positional here is appended to a fixed subcommand, so one rule holds
    for all of them: it may never read as a flag of its own, and it may never
    climb out of the repository. What varies is the shape underneath. The
    default is a bare name — that is what keeps oms_handoff_show a state
    reader instead of an arbitrary-file reader — and a tool whose ids are
    structured declares the exact pattern it accepts instead. text_argument
    has already refused a NUL byte and an oversized value before this runs.
    """
    pattern = tool.get("positional_pattern")
    requirement = (
        "an id matching %s" % pattern if pattern else "a bare digest file name"
    )
    if not value or value.startswith("-"):
        return "error: %s requires %s" % (tool["name"], requirement)
    if any(segment == ".." for segment in re.split(r"[/\\]", value)):
        return "error: %s requires %s" % (tool["name"], requirement)
    if pattern:
        # fullmatch, not match: a '$'-anchored pattern still admits a trailing
        # newline, and a newline has no business in argv.
        if not re.fullmatch(pattern, value):
            return "error: %s requires %s" % (tool["name"], requirement)
    elif "/" in value or "\\" in value:
        return "error: %s requires %s" % (tool["name"], requirement)
    return ""


def run_tool(tool: dict, arguments: dict) -> tuple[str, bool]:
    repo_path, err = resolve_repo(arguments)
    if err:
        return err, True
    argv = [
        arg if arg == "bash" else str(ROOT / arg) if arg.startswith("scripts/") else arg
        for arg in tool["argv"]
    ]
    if tool["name"] == "oms_journal" and arguments.get("view") == "blockers":
        argv.append("--blockers")
    positional = tool.get("positional")
    if positional:
        value, err = text_argument(arguments, positional, MAX_PATH_BYTES)
        if err:
            return err, True
        err = check_positional(tool, value)
        if err:
            return err, True
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


def resolve_repo(arguments: dict) -> tuple[Path, str]:
    raw, err = text_argument(arguments, "repo", MAX_PATH_BYTES)
    if err:
        return Path("."), err
    try:
        repo = Path(raw) if raw else Path.cwd()
        if not repo.is_dir():
            return repo, "error: no such repository directory: %s" % repo
    except (OSError, ValueError) as exc:
        return Path("."), "error: invalid repository path: %s" % exc
    return repo, ""


def ensure_oms_ignore(repo: Path) -> None:
    # Same contract as agent_memory_ensure_oms_ignore: .oms is local state, and
    # a run directory created here must not be the thing that commits it.
    oms_dir = repo / ".oms"
    oms_dir.mkdir(parents=True, exist_ok=True)
    ignore = oms_dir / ".gitignore"
    if not ignore.exists():
        ignore.write_text("*\n", encoding="utf-8")


def normalize_peer_provider(provider: str) -> str:
    """Ask the shell registry to normalize built-ins, aliases, and adapters."""
    try:
        result = subprocess.run(
            [
                "bash",
                "-c",
                '. "$1"; oms_provider_normalize "$2"',
                "oms-provider-normalize",
                str(PROVIDER_REGISTRY),
                provider,
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=3,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return ""
    normalized = result.stdout.strip()
    if result.returncode != 0 or not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,63}", normalized):
        return ""
    return normalized


def peer_targets(raw: str) -> tuple[list[str], str]:
    """Validate PROVIDER[:model=NAME] entries before they become argv.

    A target reaches the verb as an argument, so an unchecked value could pass
    a flag of its own; only the shapes the verbs document get through.
    """
    targets = []
    for entry in raw.split(","):
        entry = entry.strip()
        if not entry:
            continue
        provider, sep, spec = entry.partition(":")
        model = spec[len("model="):] if spec.startswith("model=") else ""
        normalized = normalize_peer_provider(provider)
        if not normalized or (
            sep
            and (
                not spec.startswith("model=")
                or not re.fullmatch(r"[^\x00-\x1f\x7f,]{1,160}", model)
            )
        ):
            return [], (
                "error: target must be PROVIDER or PROVIDER:model=NAME with"
                " a registered provider: %r" % entry
            )
        targets.append(normalized + ((":model=" + model) if sep else ""))
    return targets, ""


def peer_command(kind, script, repo, prompt, prompt_file, targets, thread, new_thread):
    argv = ["bash", str(ROOT / script), "--repo", str(repo)]
    if thread:
        argv += ["--thread", thread]
    if new_thread:
        argv.append("--new-thread")
    if kind == "ask":
        # peer-ask takes the question inline and one comma list of providers.
        argv += ["--prompt", prompt]
        if targets:
            argv += ["--providers", ",".join(targets)]
        return argv, ""
    argv += ["--prompt-file", str(prompt_file)]
    if kind == "advise":
        if len(targets) > 1:
            return [], "error: advise asks one advisor; name a single provider"
        if targets:
            argv += ["--to", targets[0]]
        return argv, ""
    for target in targets:
        argv += ["--to", target]
    return argv, ""


def start_peer(arguments: dict) -> tuple[str, bool]:
    # A provider CLI spawned by this harness inherits OMS_HARNESS_CHILD (and
    # this server inherits it from that CLI). Workers cannot widen authority
    # or delegate recursively — that decision belongs to the owner — and no
    # provider CLI offers a per-invocation switch to withhold this tool
    # (codex loads plugin MCP servers unconditionally; probed 2026-08-18).
    # Refusing here covers every provider at once, server-side.
    if os.environ.get("OMS_HARNESS_CHILD") == "1":
        return (
            "error: a delegated worker cannot start peer consultations;"
            " recursive delegation is the owner's decision — report the need"
            " in your answer instead",
            True,
        )
    repo, err = resolve_repo(arguments)
    if err:
        return err, True
    kind, err = text_argument(arguments, "kind", 64)
    if err:
        return err, True
    spec = PEER_KINDS.get(kind)
    if spec is None:
        return "error: kind must be one of: %s" % ", ".join(sorted(PEER_KINDS)), True
    raw_prompt = arguments.get("prompt")
    if not isinstance(raw_prompt, str):
        return "error: prompt must be a string", True
    try:
        prompt_bytes = raw_prompt.encode("utf-8")
    except UnicodeEncodeError:
        return "error: prompt must be valid UTF-8 text", True
    if len(prompt_bytes) > MAX_PROMPT_BYTES:
        return "error: prompt exceeds %d-byte limit" % MAX_PROMPT_BYTES, True
    prompt = raw_prompt.strip()
    if not prompt:
        return "error: prompt is required: a consultation needs a question", True
    raw_targets, err = text_argument(arguments, "providers")
    if err:
        return err, True
    targets, err = peer_targets(raw_targets)
    if err:
        return err, True
    thread, err = text_argument(arguments, "thread", 128)
    if err:
        return err, True
    if thread and not THREAD_RE.match(thread):
        return (
            "error: thread must be an id a previous run reported (letters,"
            " digits, dot, underscore, dash): %r" % thread
        ), True
    raw_new_thread = arguments.get("new_thread")
    if raw_new_thread is None:
        new_thread = False
    elif isinstance(raw_new_thread, bool):
        new_thread = raw_new_thread
    else:
        return "error: new_thread must be a boolean", True
    # advise and ask take a thread but cannot mint one, and naming a thread
    # while asking for a fresh one is two different conversations at once.
    if new_thread and kind != "consult":
        return (
            "error: new_thread applies to kind='consult'; advise and ask join"
            " the thread they are given or the current one"
        ), True
    if new_thread and thread:
        return (
            "error: thread and new_thread are exclusive: continue that thread"
            " or start a fresh one"
        ), True

    operation = "%s-%s-%s" % (
        kind,
        time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()),
        os.urandom(4).hex(),
    )
    run_dir = repo / RUN_ROOT / operation
    log = run_dir / "run.log"
    status = run_dir / "status"
    pid_file = run_dir / "pid"
    prompt_file = run_dir / "prompt.txt"
    started_at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    argv, err = peer_command(
        kind, spec["script"], repo, prompt, prompt_file, targets, thread, new_thread
    )
    if err:
        return err, True
    meta = {
        "operation": operation,
        "kind": kind,
        "targets": targets,
        "started_at": started_at,
        # One line of the question, so a later listing says what was asked
        # without reading the prompt file of every run.
        "title": " ".join(prompt.split())[:TITLE_LIMIT],
    }
    if thread:
        meta["thread"] = thread
    try:
        ensure_oms_ignore(repo)
        run_dir.mkdir(parents=True, exist_ok=True)
        prompt_file.write_text(prompt + "\n", encoding="utf-8")
        (run_dir / "meta.json").write_text(
            json.dumps(meta, ensure_ascii=False) + "\n", encoding="utf-8"
        )
        with open(log, "wb") as log_handle, open(os.devnull, "rb") as devnull:
            launcher = subprocess.Popen(
                ["bash", "-c", LAUNCH_WRAPPER, operation, str(status), str(pid_file), *argv],
                cwd=str(repo),
                stdin=devnull,
                stdout=log_handle,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
    except (OSError, UnicodeError) as exc:
        return "error: %s" % exc, True
    try:
        launcher.wait(timeout=30)
    except subprocess.TimeoutExpired:
        pass  # the launcher only forks the run and exits; a slow fork is not a failure
    return json.dumps(
        {
            "operation": operation,
            "kind": kind,
            "started": True,
            "started_at": started_at,
            "targets": targets,
            "artifact_dir": str(repo / ".oms" / "artifacts" / spec["artifacts"]),
            "run_dir": str(run_dir),
            "log": str(log),
            "next": (
                "The peer is running detached. Read it with oms_peer_result"
                " operation=%s. It usually takes several minutes: do other work"
                " and poll again rather than waiting on it." % operation
            ),
        },
        ensure_ascii=False,
        indent=2,
    ), False


def log_tail(log: Path) -> str:
    try:
        text = log.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""
    tail = "\n".join(text.splitlines()[-LOG_TAIL_LINES:])
    if len(tail) > LOG_TAIL_LIMIT:
        tail = "[truncated]\n" + tail[-LOG_TAIL_LIMIT:]
    return tail


def artifact_paths(log: Path) -> list[str]:
    """Artifact paths the run reported, in the order it reported them.

    agent-call prints "artifact: PATH" and run_provider prints
    "ok: PROVIDER -> PATH"; reading the log is how consult itself learns
    which artifact belongs to which target.
    """
    paths: list[str] = []
    try:
        text = log.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return paths
    for line in text.splitlines():
        match = re.match(r"artifact: (\S.*)\Z", line) or re.match(
            r"(?:ok|failed|skipped|blocked|dry-run|exported): \S+ -> (\S.*)\Z", line
        )
        if match is None:
            continue
        path = match.group(1).strip()
        if path not in paths and Path(path).is_file():
            paths.append(path)
    return paths


def artifact_answer(path: str) -> tuple[str, str]:
    """The Output section and recorded exit of one artifact.

    Same sections as extract_output in peer-common.sh — the answer sits between
    the Output and Exit headings — but matched from the end. An artifact quotes
    the whole composed prompt, and a prompt that dictates a reply format brings
    its own "## Output" heading with it, so the first match is the advisor's
    template rather than what the advisor said.
    """
    try:
        lines = Path(path).read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError as exc:
        return "error: %s" % exc, ""
    try:
        end = len(lines) - 1 - lines[::-1].index("## Exit")
    except ValueError:
        return "", ""  # no exit marker: this artifact is still being written
    exit_code = next((line.strip() for line in lines[end + 1:] if line.strip()), "")
    head = lines[:end]
    try:
        start = len(head) - 1 - head[::-1].index("## Output")
    except ValueError:
        return "", exit_code
    return "\n".join(head[start + 1:]).strip(), exit_code


def run_meta(run_dir: Path) -> dict:
    """Start facts recorded for a run, or {} for runs that predate the file."""
    try:
        data = json.loads((run_dir / "meta.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return data if isinstance(data, dict) else {}


def operation_start(operation: str) -> tuple[str, str]:
    """Kind and start time carried by the id itself: kind-YYYYmmddTHHMMSSZ-hex.

    A run directory older than meta.json still lists, because the id the
    server minted has always spelled both of these out.
    """
    parts = operation.split("-")
    kind = parts[0]
    stamp = parts[1] if len(parts) > 2 else ""
    if len(stamp) == 16 and stamp[8] == "T":
        return kind, "%s-%s-%sT%s:%s:%sZ" % (
            stamp[0:4], stamp[4:6], stamp[6:8], stamp[9:11], stamp[11:13], stamp[13:15]
        )
    return kind, ""


def run_alive(run_dir: Path) -> bool:
    """False only on positive evidence that the run's process is gone.

    Absence of evidence keeps a run 'running': a slow peer must never be
    reported dead, because the caller's answer to that is to spend another
    25-minute provider call. Only POSIX gets the probe — on Windows os.kill
    terminates the target instead of testing it — and a recycled pid reads
    as alive, which is exactly the behavior this had before the pid file.
    """
    if os.name != "posix":
        return True
    try:
        pid = int((run_dir / "pid").read_text(encoding="utf-8").strip())
    except (OSError, ValueError):
        return True
    if pid <= 0:
        return True
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        return True  # alive but not ours to signal
    return True


def run_status(run_dir: Path) -> tuple[str, int | None]:
    """running, done with its exit code, or stalled — died writing no exit."""
    status = run_dir / "status"
    if status.is_file():
        try:
            return "done", int(status.read_text(encoding="utf-8").strip())
        except (OSError, ValueError):
            return "done", 1
    if not run_alive(run_dir):
        return "stalled", None
    return "running", None


def run_age(run_dir: Path) -> int | None:
    for candidate in (run_dir / "prompt.txt", run_dir):
        try:
            return int(time.time() - candidate.stat().st_mtime)
        except OSError:
            continue
    return None


def log_thread(log: Path) -> str:
    """The thread id consult prints when it finishes; empty while it runs."""
    thread = ""
    try:
        text = log.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""
    for line in text.splitlines():
        match = re.match(r"thread: (\S+)\Z", line)
        if match:
            thread = match.group(1)
    return thread


def peer_operations(arguments: dict) -> tuple[str, bool]:
    repo, err = resolve_repo(arguments)
    if err:
        return err, True
    # A pure reader: a repository that has never consulted anyone has no run
    # root, and listing must not be the thing that creates one.
    entries = []
    try:
        for child in (repo / RUN_ROOT).iterdir():
            if child.is_dir() and OPERATION_RE.match(child.name):
                entries.append(child)
    except OSError:
        entries = []
    entries.sort(key=lambda path: (path.name.split("-")[1], path.name), reverse=True)

    rows = []
    for run_dir in entries[:OPERATIONS_LIMIT]:
        meta = run_meta(run_dir)
        kind, started = operation_start(run_dir.name)
        status, code = run_status(run_dir)
        row = {
            "operation": run_dir.name,
            "kind": meta.get("kind") or kind,
            "status": status,
            "started_at": meta.get("started_at") or started,
        }
        age = run_age(run_dir)
        if age is not None:
            row["age_seconds"] = age
        if code is not None:
            row["exit"] = code
        targets = meta.get("targets")
        if isinstance(targets, list) and targets:
            row["targets"] = [t for t in targets if isinstance(t, str)][:8]
        title = meta.get("title")
        if isinstance(title, str) and title:
            row["title"] = title[:TITLE_LIMIT]
        thread = log_thread(run_dir / "run.log") or meta.get("thread")
        if isinstance(thread, str) and thread:
            row["thread"] = thread
        rows.append(row)

    payload = {
        "operations": rows,
        "shown": len(rows),
        "total": len(entries),
        "next": (
            "Read one with oms_peer_result operation=ID. status='stalled' means"
            " the run's process is gone with no exit recorded: that answer is"
            " never arriving, so start a new consultation instead of polling."
        ),
    }
    if len(entries) > len(rows):
        payload["truncated"] = True
    return json.dumps(payload, ensure_ascii=False, indent=2), False


def peer_result(arguments: dict) -> tuple[str, bool]:
    repo, err = resolve_repo(arguments)
    if err:
        return err, True
    operation, err = text_argument(arguments, "operation", 128)
    if err:
        return err, True
    # The id names a directory under .oms/artifacts/mcp. Accepting anything but
    # the generated shape would turn a run reader into an arbitrary-path reader.
    if not OPERATION_RE.match(operation):
        return "error: operation must be an id returned by oms_peer_start", True
    run_dir = repo / RUN_ROOT / operation
    if not run_dir.is_dir():
        return "error: unknown operation: %s" % operation, True
    log = run_dir / "run.log"
    payload = {"operation": operation, "log": str(log)}
    try:
        elapsed = time.time() - (run_dir / "prompt.txt").stat().st_mtime
        payload["elapsed_seconds"] = int(elapsed)
    except OSError:
        pass

    status, code = run_status(run_dir)
    if status == "running":
        payload["status"] = "running"
        payload["log_tail"] = log_tail(log)
        payload["next"] = (
            "Still running. Do other work and poll oms_peer_result again;"
            " a consultation can take many minutes."
        )
        return json.dumps(payload, ensure_ascii=False, indent=2), False
    if status == "stalled":
        payload["status"] = "stalled"
        payload["log_tail"] = log_tail(log)
        payload["next"] = (
            "The run's process is gone and it recorded no exit, so no answer"
            " is coming. The log tail says how far it got; start a new"
            " consultation if the question still needs one."
        )
        return json.dumps(payload, ensure_ascii=False, indent=2), True

    artifacts = artifact_paths(log)
    sections = []
    # Each seat gets an equal slice of the budget: joining before cutting let
    # the first artifact spend it all and silently dropped the later seats.
    per_artifact = OUTPUT_LIMIT // max(1, len(artifacts))
    for path in artifacts:
        answer, artifact_exit = artifact_answer(path)
        if not answer:
            continue
        if len(answer) > per_artifact:
            answer = answer[:per_artifact] + "\n[truncated]"
        # The recorded exit is part of the answer's meaning: a nonzero seat's
        # text is a partial, and dropping the label on single-artifact runs
        # (the common case) hid exactly that.
        if len(artifacts) > 1 or str(artifact_exit or "0") != "0":
            answer = "--- %s (exit %s) ---\n%s" % (
                Path(path).name,
                artifact_exit or "?",
                answer,
            )
        sections.append(answer)
    answer = "\n\n".join(sections)
    if len(answer) > OUTPUT_LIMIT:
        answer = answer[:OUTPUT_LIMIT] + "\n[truncated]"
    payload["status"] = "done"
    payload["exit"] = code
    payload["artifacts"] = artifacts
    payload["answer"] = answer
    thread = log_thread(log)
    if thread:
        # The follow-up address: pass it back as oms_peer_start thread=ID and
        # the next question starts from what this peer already said.
        payload["thread"] = thread
    if code != 0 or not answer:
        payload["log_tail"] = log_tail(log)
    if not answer and code == 0:
        payload["note"] = (
            "the run exited 0 but no answer text could be extracted from its"
            " artifacts; judge by the log tail, not by the clean exit"
        )
    return json.dumps(payload, ensure_ascii=False, indent=2), code != 0


def task_run_dir(task_id: object) -> tuple[Path | None, str]:
    if not isinstance(task_id, str) or not OPERATION_RE.fullmatch(task_id):
        return None, "taskId must be an operation id returned by oms_peer_start"
    current = Path.cwd()
    components = (".oms", "artifacts", "mcp", task_id)
    try:
        for component in components:
            current = current / component
            info = current.lstat()
            attributes = getattr(info, "st_file_attributes", 0)
            reparse_flag = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
            if not stat.S_ISDIR(info.st_mode) or attributes & reparse_flag:
                return None, "unknown or unsafe taskId: %s" % task_id
    except OSError:
        return None, "unknown taskId: %s" % task_id
    return current, ""


def iso_mtime(paths: list[Path], fallback: str) -> str:
    stamps = []
    for path in paths:
        try:
            stamps.append(path.stat().st_mtime)
        except OSError:
            pass
    if not stamps:
        return fallback
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(max(stamps)))


def cancellation_is_valid(run_dir: Path) -> bool:
    path = run_dir / "cancel-request.json"
    try:
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > 4096:
            return False
        with path.open("r", encoding="utf-8") as handle:
            row = json.load(handle)
    except (OSError, UnicodeError, ValueError):
        return False
    return (
        isinstance(row, dict)
        and row.get("schema") == 1
        and row.get("kind") == "mcp-task-cancel-request"
        and row.get("task_id") == run_dir.name
    )


def task_snapshot(run_dir: Path) -> dict:
    meta = run_meta(run_dir)
    _kind, inferred_start = operation_start(run_dir.name)
    created = meta.get("started_at") if isinstance(meta.get("started_at"), str) else ""
    created = created or inferred_start or "1970-01-01T00:00:00Z"
    base = {
        "resultType": "complete",
        "taskId": run_dir.name,
        "createdAt": created,
        "lastUpdatedAt": iso_mtime(
            [run_dir / "status", run_dir / "cancel-request.json", run_dir / "run.log"],
            created,
        ),
        "ttlMs": None,
        "pollIntervalMs": 5000,
    }
    if cancellation_is_valid(run_dir):
        return {**base, "status": "cancelled", "statusMessage": "Cancellation requested by the client."}
    status, code = run_status(run_dir)
    if status == "running":
        return {**base, "status": "working", "statusMessage": "Peer consultation is running."}
    if status == "stalled":
        return {
            **base,
            "status": "failed",
            "statusMessage": "Peer process ended without a durable exit status.",
            "error": {"code": -32603, "message": "peer operation stalled"},
        }
    text, is_error = peer_result({"repo": str(Path.cwd()), "operation": run_dir.name})
    return {
        **base,
        "status": "completed",
        "statusMessage": "Peer consultation completed with exit %s." % code,
        "result": {
            "resultType": "complete",
            "content": [{"type": "text", "text": text}],
            "isError": is_error,
        },
    }


def write_cancel_request(run_dir: Path) -> None:
    path = run_dir / "cancel-request.json"
    row = {
        "schema": 1,
        "kind": "mcp-task-cancel-request",
        "task_id": run_dir.name,
        "requested_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    data = (json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
    temp = run_dir / (".cancel-request.%d.part" % os.getpid())
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    fd = os.open(str(temp), flags, 0o600)
    try:
        os.write(fd, data)
        os.fsync(fd)
    finally:
        os.close(fd)
    try:
        os.replace(str(temp), str(path))
    finally:
        try:
            temp.unlink()
        except OSError:
            pass


ACTIONS = {
    "oms_peer_start": start_peer,
    "oms_peer_result": peer_result,
    "oms_peer_operations": peer_operations,
}


def response(msg_id, result=None, error=None) -> dict:
    if error is not None:
        return {"jsonrpc": "2.0", "id": msg_id, "error": error}
    return {"jsonrpc": "2.0", "id": msg_id, "result": result}


def handle(message: dict):
    global SESSION_PROTOCOL
    if not isinstance(message, dict):
        return response(
            None, error={"code": -32600, "message": "invalid request: expected object"}
        )
    method = message.get("method")
    msg_id = message.get("id")
    if isinstance(msg_id, (bool, dict, list)):
        return response(
            None, error={"code": -32600, "message": "invalid request: invalid id"}
        )
    if isinstance(msg_id, str):
        try:
            id_size = len(msg_id.encode("utf-8"))
        except UnicodeEncodeError:
            return response(
                None, error={"code": -32600, "message": "invalid request: invalid id"}
            )
        if id_size > 1024:
            return response(
                None, error={"code": -32600, "message": "invalid request: id is too long"}
            )
    if method is None or not isinstance(method, str):
        return response(
            msg_id, error={"code": -32600, "message": "invalid request: method must be a string"}
        )
    try:
        method_size = len(method.encode("utf-8"))
    except UnicodeEncodeError:
        return response(
            msg_id, error={"code": -32600, "message": "invalid request: invalid method"}
        )
    if method_size > 128:
        return response(
            msg_id, error={"code": -32600, "message": "invalid request: method is too long"}
        )
    if msg_id is None:
        return None  # notification (e.g. notifications/initialized)
    params = message.get("params")
    requested = requested_protocol(params)
    if requested is not None and requested not in SUPPORTED_PROTOCOLS:
        # A named-but-unknown revision is a modern client to retry with one
        # of ours, never a legacy client to serve on the fallback.
        return response(
            msg_id,
            error={
                "code": UNSUPPORTED_PROTOCOL_CODE,
                "message": "Unsupported protocol version",
                "data": {
                    "supported": list(SUPPORTED_PROTOCOLS),
                    "requested": requested[:64],
                },
            },
        )
    if method == "server/discover":
        # Stateless by design: the probe a modern client sends first must
        # not pin the process the way initialize does.
        if params is not None and not isinstance(params, dict):
            return response(
                msg_id, error={"code": -32602, "message": "invalid params: expected object"}
            )
        return response(
            msg_id,
            {
                "resultType": "complete",
                "supportedVersions": list(SUPPORTED_PROTOCOLS),
                "capabilities": server_capabilities(TASKS_PROTOCOL),
                "_meta": {META_SERVER_INFO: server_info()},
                "ttlMs": LIST_TTL_MS,
                "cacheScope": "private",
            },
        )
    if method == "initialize":
        if params is None:
            params = {}
        if not isinstance(params, dict):
            return response(
                msg_id, error={"code": -32602, "message": "invalid params: expected object"}
            )
        requested = params.get("protocolVersion")
        if isinstance(requested, str) and requested in SUPPORTED_PROTOCOLS:
            protocol = requested
        else:
            protocol = FALLBACK_PROTOCOL
        SESSION_PROTOCOL = protocol
        return response(
            msg_id,
            complete_result({
                "protocolVersion": protocol,
                "capabilities": server_capabilities(protocol),
                "serverInfo": server_info(),
            }, params),
        )
    if method == "ping":
        return response(msg_id, complete_result({}, params))
    if method == "tools/list":
        return response(
            msg_id,
            complete_result(cacheable({"tools": tool_definitions()}, params), params),
        )
    if method in ("tasks/get", "tasks/update", "tasks/cancel"):
        if not tasks_feature_enabled() or effective_protocol(params) != TASKS_PROTOCOL:
            return response(
                msg_id, error={"code": -32601, "message": "method not found: %s" % method}
            )
        if not isinstance(params, dict):
            return response(
                msg_id, error={"code": -32602, "message": "invalid params: expected object"}
            )
        if not task_request_capable(params):
            return response(
                msg_id,
                error={
                    "code": -32003,
                    "message": "missing required client capability: %s" % TASKS_EXTENSION,
                },
            )
        run_dir, err = task_run_dir(params.get("taskId"))
        if err:
            return response(msg_id, error={"code": -32602, "message": err})
        assert run_dir is not None
        snapshot = task_snapshot(run_dir)
        if method == "tasks/get":
            return response(msg_id, snapshot)
        if method == "tasks/update":
            return response(
                msg_id,
                error={
                    "code": -32602,
                    "message": "task has no outstanding input requests",
                },
            )
        if snapshot["status"] != "working":
            return response(
                msg_id,
                error={"code": -32602, "message": "task is already terminal"},
            )
        try:
            write_cancel_request(run_dir)
        except OSError as exc:
            return response(
                msg_id,
                error={"code": -32603, "message": "could not record cancellation: %s" % exc},
            )
        return response(msg_id, {"resultType": "complete"})
    if method == "tools/call":
        if params is None:
            params = {}
        if not isinstance(params, dict):
            return response(
                msg_id, error={"code": -32602, "message": "invalid params: expected object"}
            )
        name = params.get("name")
        try:
            name_size = len(name.encode("utf-8")) if isinstance(name, str) else -1
        except UnicodeEncodeError:
            name_size = -1
        if name_size < 0 or name_size > 128:
            return response(
                msg_id, error={"code": -32602, "message": "invalid tool name"}
            )
        arguments = params.get("arguments")
        if arguments is None:
            arguments = {}
        if not isinstance(arguments, dict):
            return response(
                msg_id,
                error={"code": -32602, "message": "invalid arguments: expected object"},
            )
        for tool in TOOLS:
            if tool["name"] == name:
                # State tools are one fixed argv; action tools run their own
                # handler, because starting a peer is not a subcommand call.
                if "argv" in tool:
                    text, is_error = run_tool(tool, arguments)
                else:
                    text, is_error = ACTIONS[tool["name"]](arguments)
                ordinary = {
                    "content": [{"type": "text", "text": text}],
                    "isError": is_error,
                }
                if (
                    name == "oms_peer_start"
                    and not is_error
                    and task_request_capable(params)
                ):
                    repo, repo_err = resolve_repo(arguments)
                    try:
                        same_repo = not repo_err and repo.resolve() == Path.cwd().resolve()
                    except OSError:
                        same_repo = False
                    if same_repo:
                        try:
                            payload = json.loads(text)
                        except ValueError:
                            payload = {}
                        operation = payload.get("operation") if isinstance(payload, dict) else None
                        run_dir, task_err = task_run_dir(operation)
                        if not task_err and run_dir is not None:
                            task = task_snapshot(run_dir)
                            task["resultType"] = "task"
                            return response(msg_id, task)
                return response(
                    msg_id,
                    complete_result(ordinary, params),
                )
        return response(
            msg_id, error={"code": -32602, "message": "unknown tool: %r" % name}
        )
    return response(
        msg_id, error={"code": -32601, "message": "method not found: %s" % method}
    )


def main() -> int:
    stream = sys.stdin.buffer
    while True:
        raw = stream.readline(MAX_REQUEST_BYTES + 1)
        if not raw:
            break
        if len(raw) > MAX_REQUEST_BYTES:
            # Drain only the remainder of this record in bounded chunks, then
            # resume at the next JSON-RPC line. Never parse, echo, or retain the
            # oversized payload.
            while raw and not raw.endswith(b"\n"):
                raw = stream.readline(MAX_REQUEST_BYTES + 1)
            reply = response(
                None,
                error={
                    "code": -32600,
                    "message": "request exceeds %d-byte limit" % MAX_REQUEST_BYTES,
                },
            )
            print(json.dumps(reply), flush=True)
            continue
        try:
            line = raw.decode("utf-8").strip()
        except UnicodeDecodeError:
            reply = response(None, error={"code": -32700, "message": "parse error"})
            print(json.dumps(reply), flush=True)
            continue
        if not line:
            continue
        try:
            message = json.loads(line)
        except (ValueError, RecursionError):
            reply = response(
                None, error={"code": -32700, "message": "parse error"}
            )
            print(json.dumps(reply), flush=True)
            continue
        try:
            reply = handle(message)
        except (OSError, RecursionError, UnicodeError, ValueError):
            # A malformed field must fail only its own request. Filesystem path
            # limits vary by platform, and deeply shaped values can exceed
            # Python's recursion limit even inside the overall byte bound.
            msg_id = message.get("id") if isinstance(message, dict) else None
            reply = response(
                msg_id,
                error={"code": -32602, "message": "invalid or oversized request field"},
            )
        if reply is not None:
            print(json.dumps(reply, ensure_ascii=False), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
