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
PROVIDERS = ("codex", "claude", "antigravity", "agy")
RUN_ROOT = Path(".oms/artifacts/mcp")
OPERATION_RE = re.compile(r"[a-z]+-[0-9TZ]+-[0-9a-f]+\Z")
LOG_TAIL_LINES = 20
LOG_TAIL_LIMIT = 4_000
OPERATIONS_LIMIT = 20
TITLE_LIMIT = 160
# A thread id reaches the verb as argv, so only the shape agent-thread.sh mints
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
        "argv": ["bash", "scripts/repo-state.sh", "--json"],
        "properties": REPO_PROPERTY,
        "annotations": READ_ONLY,
    },
    {
        "name": "oms_agent_operations",
        "description": (
            "Durable lifecycle projection for all supervised and direct agent"
            " attempts, including state, usage, lineage, and budgets."
        ),
        "argv": ["bash", "scripts/agent-events.sh", "list", "--json"],
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
        ),
        "argv": ["bash", "scripts/fail-ledger.sh", "list", "--json"],
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
        if provider not in PROVIDERS or (
            sep and not re.fullmatch(r"[A-Za-z0-9._-]+", model)
        ):
            return [], (
                "error: target must be PROVIDER or PROVIDER:model=NAME with"
                " PROVIDER in %s: %r" % ("|".join(PROVIDERS), entry)
            )
        targets.append(entry)
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
    thread = log_thread(log)
    if thread:
        # The follow-up address: pass it back as oms_peer_start thread=ID and
        # the next question starts from what this peer already said.
        payload["thread"] = thread
    if code != 0 or not answer:
        payload["log_tail"] = log_tail(log)
    return json.dumps(payload, ensure_ascii=False, indent=2), code != 0


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
    if method == "initialize":
        params = message.get("params")
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
        params = message.get("params")
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
