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
process can poll a run another one started.

Transport: stdio, newline-delimited JSON-RPC 2.0 (the MCP stdio framing).
Stdlib only — this runs wherever the harness runs.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FALLBACK_PROTOCOL = "2025-06-18"
# Revisions whose semantics this server actually implements. Echoing an
# arbitrary requested revision back would advertise conformance to behavior
# (newer handshakes, task extensions) it has never implemented.
SUPPORTED_PROTOCOLS = ("2025-06-18", "2025-03-26")
OUTPUT_LIMIT = 60_000

# Peer consultation: which front-door verb each kind runs, and where that verb
# writes its answer artifacts. Started detached, polled from disk.
PEER_KINDS = {
    "consult": {"script": "scripts/agent-consult.sh", "artifacts": "consult"},
    "advise": {"script": "scripts/advise.sh", "artifacts": "advise"},
    "ask": {"script": "scripts/peer-ask.sh", "artifacts": "ask"},
}
PROVIDERS = ("codex", "claude", "antigravity", "agy")
RUN_ROOT = Path(".oms/artifacts/mcp")
OPERATION_RE = re.compile(r"[a-z]+-[0-9TZ]+-[0-9a-f]+\Z")
LOG_TAIL_LINES = 20
LOG_TAIL_LIMIT = 4_000
# The consultation outlives this server: the inner shell is backgrounded so the
# launcher exits at once and the run is reparented to init, and the run's own
# exit code lands in a status file only after the verb returns. That file is the
# completion signal — a verb can fail (no peer CLI installed, bad target) before
# any artifact exists, and waiting on artifacts alone would poll forever.
LAUNCH_WRAPPER = (
    'status="$1"; shift; '
    '{ rc=0; "$@" || rc=$?; printf "%s\\n" "$rc" > "$status.part" && '
    'mv "$status.part" "$status"; } &'
)


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
    # Action tools: no argv, dispatched through ACTIONS below.
    {
        "name": "oms_peer_start",
        "description": (
            "Start a consultation with another agent CLI (codex, claude,"
            " antigravity) in the background and return immediately with an"
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
                    "Comma list of targets (codex, claude, antigravity), each"
                    " optionally pinned as PROVIDER:model=NAME. Default: the"
                    " verb's own pick, which excludes the calling agent."
                    " advise takes one target."
                ),
            },
        },
        "required": ["kind", "prompt"],
    },
    {
        "name": "oms_peer_result",
        "description": (
            "Read a consultation started by oms_peer_start. While the peer is"
            " still working this returns status='running' with the tail of its"
            " log; when it finishes, status='done' with the answer, the exit"
            " code, and the artifact paths. Polling is cheap and never blocks,"
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


def resolve_repo(arguments: dict) -> tuple[Path, str]:
    repo = Path(str(arguments.get("repo") or Path.cwd()))
    if not repo.is_dir():
        return repo, "error: no such repository directory: %s" % repo
    return repo, ""


def ensure_oms_ignore(repo: Path) -> None:
    # Same contract as agent_memory_ensure_oms_ignore: .oms is local state, and
    # a run directory created here must not be the thing that commits it.
    oms_dir = repo / ".oms"
    oms_dir.mkdir(parents=True, exist_ok=True)
    ignore = oms_dir / ".gitignore"
    if not ignore.exists():
        ignore.write_text("*\n", encoding="utf-8")


def peer_targets(raw) -> tuple[list[str], str]:
    """Validate PROVIDER[:model=NAME] entries before they become argv.

    A target reaches the verb as an argument, so an unchecked value could pass
    a flag of its own; only the shapes the verbs document get through.
    """
    targets = []
    for entry in str(raw or "").split(","):
        entry = entry.strip()
        if not entry:
            continue
        provider, sep, spec = entry.partition(":")
        model = spec[len("model="):] if spec.startswith("model=") else ""
        if provider not in PROVIDERS or (
            sep and not re.fullmatch(r"[A-Za-z0-9._-]+", model)
        ):
            return [], (
                "error: target must be PROVIDER or PROVIDER:model=NAME with"
                " PROVIDER in %s: %r" % ("|".join(PROVIDERS), entry)
            )
        targets.append(entry)
    return targets, ""


def peer_command(kind, script, repo, prompt, prompt_file, targets):
    argv = ["bash", str(ROOT / script), "--repo", str(repo)]
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
    repo, err = resolve_repo(arguments)
    if err:
        return err, True
    kind = str(arguments.get("kind") or "")
    spec = PEER_KINDS.get(kind)
    if spec is None:
        return "error: kind must be one of: %s" % ", ".join(sorted(PEER_KINDS)), True
    prompt = str(arguments.get("prompt") or "").strip()
    if not prompt:
        return "error: prompt is required: a consultation needs a question", True
    targets, err = peer_targets(arguments.get("providers"))
    if err:
        return err, True

    operation = "%s-%s-%s" % (
        kind,
        time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()),
        os.urandom(4).hex(),
    )
    run_dir = repo / RUN_ROOT / operation
    log = run_dir / "run.log"
    status = run_dir / "status"
    prompt_file = run_dir / "prompt.txt"
    argv, err = peer_command(kind, spec["script"], repo, prompt, prompt_file, targets)
    if err:
        return err, True
    try:
        ensure_oms_ignore(repo)
        run_dir.mkdir(parents=True, exist_ok=True)
        prompt_file.write_text(prompt + "\n", encoding="utf-8")
        with open(log, "wb") as log_handle, open(os.devnull, "rb") as devnull:
            launcher = subprocess.Popen(
                ["bash", "-c", LAUNCH_WRAPPER, operation, str(status), *argv],
                cwd=str(repo),
                stdin=devnull,
                stdout=log_handle,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
    except OSError as exc:
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
            "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
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
    "ok: PROVIDER -> PATH"; reading the log is how agent-consult itself learns
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


def peer_result(arguments: dict) -> tuple[str, bool]:
    repo, err = resolve_repo(arguments)
    if err:
        return err, True
    operation = str(arguments.get("operation") or "")
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

    status = run_dir / "status"
    if not status.is_file():
        payload["status"] = "running"
        payload["log_tail"] = log_tail(log)
        payload["next"] = (
            "Still running. Do other work and poll oms_peer_result again;"
            " a consultation can take many minutes."
        )
        return json.dumps(payload, ensure_ascii=False, indent=2), False

    try:
        code = int(status.read_text(encoding="utf-8").strip())
    except (OSError, ValueError):
        code = 1
    artifacts = artifact_paths(log)
    sections = []
    for path in artifacts:
        answer, artifact_exit = artifact_answer(path)
        if not answer:
            continue
        if len(artifacts) > 1:
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
    if code != 0 or not answer:
        payload["log_tail"] = log_tail(log)
    return json.dumps(payload, ensure_ascii=False, indent=2), code != 0


ACTIONS = {"oms_peer_start": start_peer, "oms_peer_result": peer_result}


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
                # State tools are one fixed argv; action tools run their own
                # handler, because starting a peer is not a subcommand call.
                if "argv" in tool:
                    text, is_error = run_tool(tool, arguments)
                else:
                    text, is_error = ACTIONS[tool["name"]](arguments)
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
