#!/usr/bin/env python3
"""Read-only A2A v1 Agent Card and explicit loopback HTTP+JSON bridge."""

from __future__ import annotations

import argparse
import hashlib
import ipaddress
import json
import math
import os
import socket
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from urllib.parse import urlparse

ROOT = Path(__file__).resolve().parents[2]
MAX_REQUEST_BYTES = 64 * 1024
MAX_RESULT_BYTES = 60_000
READ_COMMANDS = {
    "status": ["bash", str(ROOT / "scripts/state.sh"), "--json"],
    "inbox": ["bash", str(ROOT / "scripts/inbox.sh"), "--json"],
    "capabilities": [
        "bash", str(ROOT / "scripts/runtime.sh"), "profile", "current"
    ],
}


class A2AError(RuntimeError):
    def __init__(self, status: int, slug: str, detail: str):
        super().__init__(detail)
        self.status = status
        self.slug = slug
        self.detail = detail


def strict_json(data: bytes) -> object:
    def pairs(values):
        result = {}
        for key, value in values:
            if key in result:
                raise ValueError("duplicate key")
            result[key] = value
        return result

    def constant(_value):
        raise ValueError("non-finite constant")

    try:
        value = json.loads(data.decode("utf-8"), object_pairs_hook=pairs, parse_constant=constant)
    except (UnicodeError, ValueError, RecursionError) as exc:
        raise A2AError(400, "invalid-json", "Request body must be strict UTF-8 JSON.") from exc

    def finite(node):
        if isinstance(node, float) and not math.isfinite(node):
            raise A2AError(400, "invalid-json", "Request body contains a non-finite number.")
        if isinstance(node, list):
            for item in node:
                finite(item)
        elif isinstance(node, dict):
            for item in node.values():
                finite(item)

    finite(value)
    return value


def loopback_host(value: str) -> str:
    try:
        address = ipaddress.ip_address(value)
    except ValueError as exc:
        raise A2AError(2, "invalid-bind", "A2A bridge host must be a loopback IP literal.") from exc
    if not address.is_loopback:
        raise A2AError(2, "invalid-bind", "A2A bridge refuses non-loopback bind addresses.")
    return str(address)


def validate_url(value: str) -> str:
    parsed = urlparse(value)
    try:
        host = loopback_host(parsed.hostname or "")
        port = parsed.port
    except (A2AError, ValueError) as exc:
        raise A2AError(2, "invalid-url", "Agent Card URL must be loopback HTTP with an explicit port.") from exc
    if (
        parsed.scheme != "http"
        or port is None
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or parsed.path not in ("", "/")
    ):
        raise A2AError(2, "invalid-url", "Agent Card URL must be loopback HTTP with an explicit port.")
    rendered_host = "[%s]" % host if ":" in host else host
    return "http://%s:%d" % (rendered_host, port)


def version() -> str:
    try:
        return (ROOT / "VERSION").read_text(encoding="utf-8").strip()
    except OSError:
        return "unknown"


def agent_card(url: str) -> dict:
    return {
        "name": "oh-my-setting",
        "description": (
            "Read-only repository control-plane projections for OMS state, "
            "attention, and capability readiness. No model calls or mutations."
        ),
        "version": version(),
        "supportedInterfaces": [
            {"url": validate_url(url), "protocolBinding": "HTTP+JSON", "protocolVersion": "1.0"}
        ],
        "capabilities": {
            "streaming": False,
            "pushNotifications": False,
            "extendedAgentCard": False,
        },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["application/json"],
        "skills": [
            {
                "id": "oms-repo-state",
                "name": "Repository state",
                "description": "Project canonical task, plan, approval, failure, and evidence projection.",
                "tags": ["repository", "status", "read-only"],
                "examples": ["status"],
                "inputModes": ["text/plain"],
                "outputModes": ["application/json"],
            },
            {
                "id": "oms-inbox",
                "name": "Attention inbox",
                "description": "Ranked read-only attention queue with typed next actions.",
                "tags": ["inbox", "attention", "read-only"],
                "examples": ["inbox"],
                "inputModes": ["text/plain"],
                "outputModes": ["application/json"],
            },
            {
                "id": "oms-capabilities",
                "name": "Capability readiness",
                "description": "Current runtime capability profile and readiness checks.",
                "tags": ["capabilities", "runtime", "read-only"],
                "examples": ["capabilities"],
                "inputModes": ["text/plain"],
                "outputModes": ["application/json"],
            },
        ],
    }


def message_command(body: object) -> tuple[str, str]:
    if not isinstance(body, dict):
        raise A2AError(400, "invalid-message", "SendMessage request must be an object.")
    tenant = body.get("tenant", "")
    if tenant not in (None, ""):
        raise A2AError(400, "unsupported-tenant", "This local bridge has no tenant routing.")
    message = body.get("message")
    if not isinstance(message, dict) or message.get("role") != "ROLE_USER":
        raise A2AError(400, "invalid-message", "message.role must be ROLE_USER.")
    if "taskId" in message:
        raise A2AError(400, "unsupported-task", "This bridge returns synchronous Messages and has no A2A task store.")
    message_id = message.get("messageId")
    if not isinstance(message_id, str) or not message_id or len(message_id.encode("utf-8")) > 256:
        raise A2AError(400, "invalid-message", "message.messageId is required and bounded.")
    parts = message.get("parts")
    if not isinstance(parts, list) or len(parts) != 1 or not isinstance(parts[0], dict):
        raise A2AError(400, "invalid-message", "message must contain exactly one text part.")
    command = parts[0].get("text")
    if not isinstance(command, str):
        raise A2AError(400, "invalid-message", "message part must contain text.")
    command = command.strip()
    if command not in READ_COMMANDS:
        raise A2AError(
            400,
            "unsupported-read-command",
            "Supported read commands are: status, inbox, capabilities.",
        )
    configuration = body.get("configuration")
    if configuration is not None:
        if not isinstance(configuration, dict):
            raise A2AError(400, "invalid-message", "configuration must be an object.")
        modes = configuration.get("acceptedOutputModes", ["application/json"])
        if not isinstance(modes, list) or "application/json" not in modes:
            raise A2AError(406, "output-mode-not-acceptable", "Bridge output mode is application/json.")
    return message_id, command


def read_projection(repo: Path, command: str) -> str:
    argv = list(READ_COMMANDS[command])
    if command in ("status", "inbox"):
        argv[2:2] = ["--repo", str(repo)]
    else:
        argv[2:2] = ["--repo", str(repo)]
    try:
        completed = subprocess.run(
            argv,
            cwd=str(repo),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise A2AError(500, "projection-failed", "Read projection failed.") from exc
    data = completed.stdout
    if len(data) > MAX_RESULT_BYTES:
        raise A2AError(500, "projection-too-large", "Read projection exceeds output limit.")
    # Capability readiness uses a nonzero exit when prerequisites are missing;
    # its JSON remains the authoritative read result. Other commands require a
    # clean command exit.
    if completed.returncode != 0 and command != "capabilities":
        raise A2AError(500, "projection-failed", "Read projection failed.")
    value = strict_json(data)
    if not isinstance(value, (dict, list)):
        raise A2AError(500, "projection-invalid", "Read projection did not return structured JSON.")
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def send_message(repo: Path, body: object) -> dict:
    source_id, command = message_command(body)
    text = read_projection(repo, command)
    digest = hashlib.sha256((source_id + "\0" + command + "\0" + text).encode("utf-8")).hexdigest()[:24]
    return {
        "message": {
            "messageId": "oms-%s" % digest,
            "role": "ROLE_AGENT",
            "parts": [{"text": text}],
        }
    }


class Bridge(HTTPServer):
    allow_reuse_address = False


class Bridge6(Bridge):
    address_family = socket.AF_INET6


class Handler(BaseHTTPRequestHandler):
    server_version = "OMS-A2A/1.0"

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def json_response(self, status: int, value: object, content_type: str) -> None:
        data = (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def problem(self, error: A2AError) -> None:
        self.json_response(
            error.status,
            {
                "type": "https://a2a-protocol.org/errors/%s" % error.slug,
                "title": error.slug.replace("-", " ").title(),
                "status": error.status,
                "detail": error.detail,
            },
            "application/problem+json",
        )

    def do_GET(self) -> None:  # noqa: N802
        if self.path != "/.well-known/agent-card.json":
            self.problem(A2AError(404, "not-found", "Unknown A2A resource."))
            return
        self.json_response(200, self.server.agent_card, "application/json")  # type: ignore[attr-defined]

    def do_POST(self) -> None:  # noqa: N802
        if self.path != "/message:send":
            self.problem(A2AError(404, "not-found", "Unknown A2A operation."))
            return
        content_type = self.headers.get_content_type()
        if content_type not in ("application/a2a+json", "application/json"):
            self.problem(A2AError(415, "unsupported-media-type", "Use application/a2a+json."))
            return
        try:
            length = int(self.headers.get("Content-Length", ""))
        except ValueError:
            length = -1
        if length < 0 or length > MAX_REQUEST_BYTES:
            self.problem(A2AError(413, "request-too-large", "Request body exceeds 64 KiB limit."))
            return
        data = self.rfile.read(length)
        try:
            result = send_message(self.server.repo, strict_json(data))  # type: ignore[attr-defined]
        except A2AError as exc:
            self.problem(exc)
            return
        self.json_response(200, result, "application/a2a+json")


def serve(args: argparse.Namespace) -> int:
    host = loopback_host(args.host)
    repo = Path(os.path.realpath(args.repo))
    if not repo.is_dir():
        raise A2AError(2, "invalid-repo", "Repository is not a directory.")
    if args.port < 0 or args.port > 65535:
        raise A2AError(2, "invalid-port", "Port must be between 0 and 65535.")
    if args.max_requests < 0 or args.max_requests > 100:
        raise A2AError(2, "invalid-limit", "max-requests must be between 0 and 100.")
    server_type = Bridge6 if ":" in host else Bridge
    server = server_type((host, args.port), Handler)
    bound_host, bound_port = server.server_address[:2]
    shown_host = "[%s]" % bound_host if ":" in bound_host else bound_host
    url = "http://%s:%d" % (shown_host, bound_port)
    server.repo = repo  # type: ignore[attr-defined]
    server.agent_card = agent_card(url)  # type: ignore[attr-defined]
    print("a2a-listening: %s" % url, flush=True)
    if args.max_requests:
        for _index in range(args.max_requests):
            server.handle_request()
    else:
        server.serve_forever()
    server.server_close()
    return 0


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    subparsers = result.add_subparsers(dest="action", required=True)
    card = subparsers.add_parser("card")
    card.add_argument("--url", default="http://127.0.0.1:8765")
    card.add_argument("--pretty", action="store_true")
    bridge = subparsers.add_parser("serve")
    bridge.add_argument("--repo", default=".")
    bridge.add_argument("--host", default="127.0.0.1")
    bridge.add_argument("--port", type=int, default=8765)
    bridge.add_argument("--max-requests", type=int, default=0)
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        if args.action == "card":
            print(json.dumps(agent_card(args.url), ensure_ascii=False, sort_keys=True, indent=2 if args.pretty else None))
            return 0
        return serve(args)
    except A2AError as exc:
        print("error: %s" % exc.detail, file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
