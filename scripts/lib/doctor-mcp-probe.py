#!/usr/bin/env python3
"""Opt-in tool-definition probe for ONE operator-named stdio MCP server.

Used by doctor-mcp-pins.py --probe-tools. Starts the server command exactly as
its configuration names it (no shell), speaks just enough MCP over stdio
(initialize, notifications/initialized, tools/list) to read every tool
definition, hashes each complete definition (name, description, inputSchema,
outputSchema, annotations and any other field the server sends) and returns
{tool name: sha256}. Nothing the server says is printed except cleaned tool
names; descriptions and schemas are only hashed.

Bounds: one hard wall-clock deadline for the whole exchange, a cap on bytes
read, on pages and on tool count. The server runs in its own process group and
the group is always terminated (SIGTERM, then SIGKILL) before this returns,
whatever happened. Standard error is discarded. The child gets a minimal
environment (PATH, LANG, TMPDIR) plus the variables its own configuration
names; the parent's environment is not inherited.

Not a sandbox: the server is ordinary code started with the caller's
privileges and can use the network itself. Only the person who names it should
decide to run it.
"""
import hashlib
import json
import os
import select
import signal
import subprocess
import time

PROTOCOL = "2025-06-18"
MAX_BYTES = 4 * 1024 * 1024
MAX_PAGES = 20
MAX_TOOLS = 500


class ProbeError(Exception):
    pass


def definition_hash(tool):
    blob = json.dumps(tool, sort_keys=True, separators=(",", ":"), ensure_ascii=True, default=str)
    return hashlib.sha256(blob.encode("utf-8", "replace")).hexdigest()


class Stdio:
    def __init__(self, proc, deadline):
        self.proc, self.deadline = proc, deadline
        self.fd = proc.stdout.fileno()
        self.buf = b""
        self.total = 0

    def send(self, message):
        try:
            self.proc.stdin.write((json.dumps(message) + "\n").encode("utf-8"))
            self.proc.stdin.flush()
        except (BrokenPipeError, OSError):
            raise ProbeError("server closed its input")

    def line(self):
        while b"\n" not in self.buf:
            left = self.deadline - time.monotonic()
            if left <= 0:
                raise ProbeError("timed out")
            ready, _, _ = select.select([self.fd], [], [], left)
            if not ready:
                continue
            chunk = os.read(self.fd, 65536)
            if not chunk:
                raise ProbeError("server closed its output")
            self.total += len(chunk)
            if self.total > MAX_BYTES:
                raise ProbeError("server output exceeded %d bytes" % MAX_BYTES)
            self.buf += chunk
        raw, self.buf = self.buf.split(b"\n", 1)
        return raw

    def reply(self, wanted):
        """The response to request id `wanted`; other traffic is ignored."""
        while True:
            raw = self.line().strip()
            if not raw:
                continue
            try:
                message = json.loads(raw.decode("utf-8", "replace"))
            except ValueError:
                continue
            if not isinstance(message, dict) or message.get("id") != wanted or "method" in message:
                continue
            if "error" in message:
                raise ProbeError("server answered request %s with an error" % wanted)
            result = message.get("result")
            if not isinstance(result, dict):
                raise ProbeError("server sent a malformed response to request %s" % wanted)
            return result


def stop(proc):
    """Always end the server and anything it started in its process group."""
    try:
        proc.stdin.close()
    except (OSError, ValueError):
        pass
    for sig, wait in ((signal.SIGTERM, 2.0), (signal.SIGKILL, 5.0)):
        try:
            os.killpg(proc.pid, sig)
        except (ProcessLookupError, PermissionError):
            pass
        try:
            proc.wait(timeout=wait)
            break
        except subprocess.TimeoutExpired:
            continue
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass
    try:
        proc.stdout.close()
    except (OSError, ValueError):
        pass


def probe(spec, timeout, cwd):
    """{tool name: definition hash} from a stdio server spec; raises ProbeError."""
    command, args = spec.get("command"), spec.get("args")
    if not isinstance(command, str) or not command.strip():
        raise ProbeError("server has no command")
    if args is None:
        args = []
    if not isinstance(args, list) or not all(isinstance(a, str) for a in args):
        raise ProbeError("server arguments are not a list of strings")
    env = {k: os.environ[k] for k in ("PATH", "LANG", "TMPDIR") if k in os.environ}
    extra = spec.get("env")
    if isinstance(extra, dict):
        env.update({str(k): str(v) for k, v in extra.items()})
    deadline = time.monotonic() + timeout
    try:
        proc = subprocess.Popen([command] + args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL, env=env, cwd=cwd, start_new_session=True)
    except OSError as exc:
        raise ProbeError("could not start the server (%s)" % exc.__class__.__name__)
    try:
        io = Stdio(proc, deadline)
        io.send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                 "params": {"protocolVersion": PROTOCOL, "capabilities": {},
                            "clientInfo": {"name": "oms-doctor-mcp-probe", "version": "1"}}})
        io.reply(1)
        io.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        tools, cursor, request = {}, None, 2
        for _ in range(MAX_PAGES):
            params = {"cursor": cursor} if cursor else {}
            io.send({"jsonrpc": "2.0", "id": request, "method": "tools/list", "params": params})
            result = io.reply(request)
            page = result.get("tools")
            if not isinstance(page, list):
                raise ProbeError("tools/list returned no tools array")
            for tool in page:
                if not isinstance(tool, dict) or not isinstance(tool.get("name"), str):
                    raise ProbeError("tools/list returned a definition without a name")
                tools[tool["name"]] = definition_hash(tool)
            if len(tools) > MAX_TOOLS:
                raise ProbeError("server lists more than %d tools" % MAX_TOOLS)
            cursor = result.get("nextCursor")
            if not isinstance(cursor, str) or not cursor:
                return tools
            request += 1
        raise ProbeError("tools/list paged more than %d times" % MAX_PAGES)
    finally:
        stop(proc)
