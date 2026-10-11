"""Invocation-scoped loopback gateway; model tools stay in the caller's harness."""

import hmac
import http.client
import json
import queue
import secrets
import socket
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

from codex_auth_broker import BridgeError
from codex_messages import MessageStream, from_messages, json_text, message, subscription_request


MAX_BODY = 16 * 1024 * 1024


class SubscriptionBackend:
    def __init__(self, broker):
        self.broker = broker

    def open(self, body):
        for refresh in (False, True):
            token, account_id = self.broker.credentials(refresh=refresh)
            connection = http.client.HTTPSConnection("chatgpt.com", timeout=90)
            try:
                connection.request("POST", "/backend-api/codex/responses", json_text(body).encode(),
                                   {"Authorization": "Bearer " + token, "ChatGPT-Account-Id": account_id,
                                    "Content-Type": "application/json", "Accept": "text/event-stream",
                                    "OpenAI-Beta": "responses=experimental", "originator": "oms_harness"})
                response = connection.getresponse()
            except (OSError, http.client.HTTPException):
                connection.close()
                raise BridgeError("Cannot connect to the Codex subscription backend", 502) from None
            if response.status == 401 and not refresh:
                connection.close()
                continue
            if response.status != 200:
                status = response.status if response.status in (400, 401, 403, 429) else 502
                connection.close()
                raise BridgeError("Codex subscription backend rejected the request (HTTP %s)" % status, status)
            # The legacy backend can omit Content-Type on a valid SSE response.
            # Validate the bounded event framing and completion in response_events.
            return connection, response
        raise BridgeError("Codex subscription authentication failed", 401)


def response_events(response):
    data = []
    size = 0
    while True:
        raw = response.readline(MAX_BODY + 1)
        if not raw:
            if data:
                raise BridgeError("Codex stream ended inside an event", 502)
            return
        size += len(raw)
        if size > MAX_BODY:
            raise BridgeError("Codex stream event exceeded the size limit", 502)
        line = raw.decode("utf-8").rstrip("\r\n")
        if not line:
            if data:
                payload = "\n".join(data)
                if payload != "[DONE]":
                    event = json.loads(payload)
                    if not isinstance(event, dict):
                        raise BridgeError("Codex returned an invalid stream event", 502)
                    yield event
            else:
                yield None
            data, size = [], 0
        elif line.startswith("data:"):
            data.append(line[5:].lstrip(" "))
        elif not line.startswith(("event:", "id:", "retry:", ":")):
            raise BridgeError("Codex returned an unsupported response format", 502)


class Gateway(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, backend, model, effort):
        super().__init__(("127.0.0.1", 0), Handler)
        self.backend, self.model, self.effort = backend, model, effort
        self.capability = secrets.token_urlsafe(32)
        self.slots = threading.BoundedSemaphore(8)
        self.connections = set()
        self.connection_lock = threading.Lock()

    @property
    def url(self):
        return "http://127.0.0.1:%s" % self.server_port

    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.slots.release()

    def close(self):
        self.shutdown()
        with self.connection_lock:
            for connection in self.connections:
                try:
                    connection.sock.shutdown(socket.SHUT_RDWR)
                except (OSError, AttributeError):
                    pass
                connection.close()
            self.connections.clear()
        self.server_close()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def setup(self):
        super().setup()
        self.connection.settimeout(20)

    def log_message(self, *_args):
        pass

    def authorized(self):
        host = self.headers.get("Host", "")
        expected = "127.0.0.1:%s" % self.server.server_port
        auth = self.headers.get("Authorization", "")
        key = self.headers.get("x-api-key", "")
        return (host == expected and not self.headers.get("Origin") and
                (hmac.compare_digest(auth.encode(), ("Bearer " + self.server.capability).encode()) or
                 hmac.compare_digest(key.encode(), self.server.capability.encode())))

    def send_json(self, status, row):
        data = json_text(row).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        if status >= 400:
            self.send_header("x-should-retry", "false")
        self.end_headers()
        self.wfile.write(data)

    def do_HEAD(self):
        self.send_response(200 if self.authorized() else 403)
        self.end_headers()

    def do_POST(self):
        if not self.authorized():
            self.send_json(403, {"error": {"type": "authentication_error", "message": "Invalid local bridge authentication"}})
            return
        path = urlsplit(self.path).path
        if path not in ("/v1/messages", "/v1/responses"):
            self.send_json(404, {"error": {"type": "not_found_error", "message": "Unsupported bridge endpoint"}})
            return
        streaming = False
        connection = None
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if self.headers.get("Transfer-Encoding") or length <= 0 or length > MAX_BODY:
                raise BridgeError("Request body must have a bounded Content-Length", 413)
            data = self.rfile.read(length)
            if len(data) != length:
                raise BridgeError("Incomplete request body")
            body = json.loads(data, parse_constant=lambda _value: (_ for _ in ()).throw(ValueError()))
            if not isinstance(body, dict) or not isinstance(body.get("stream", False), bool):
                raise BridgeError("Request must be an object with a boolean stream flag")
            wants_stream = body.get("stream", False)
            translated = from_messages(body) if path == "/v1/messages" else body
            outbound = subscription_request(translated, self.server.model, self.server.effort)
            connection, response = self.server.backend.open(outbound)
            with self.server.connection_lock:
                self.server.connections.add(connection)
            events = queue.Queue(maxsize=16)
            cancelled = threading.Event()

            def enqueue(row):
                while not cancelled.is_set():
                    try:
                        events.put(row, timeout=0.5)
                        return
                    except queue.Full:
                        pass

            def reader():
                try:
                    for row in response_events(response):
                        enqueue(row)
                        if cancelled.is_set() or (row and row.get("type") in (
                                "response.completed", "response.incomplete", "response.failed", "error")):
                            break
                except (OSError, ValueError, AttributeError, http.client.HTTPException, BridgeError):
                    # HTTPResponse may lose its file pointer during concurrent shutdown.
                    enqueue(BridgeError("Codex response stream failed", 502))
                finally:
                    enqueue(StopIteration)

            threading.Thread(target=reader, daemon=True).start()
            converter = MessageStream(self.server.model) if path == "/v1/messages" else None
            completed = False
            if wants_stream:
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Cache-Control", "no-store")
                self.end_headers()
                streaming = True
            try:
                while True:
                    try:
                        event = events.get(timeout=10)
                    except queue.Empty:
                        event = None
                    if event is StopIteration:
                        if not completed:
                            raise BridgeError("Codex response ended before completion", 502)
                        break
                    if isinstance(event, BridgeError):
                        raise event
                    if event is None:
                        if streaming:
                            self.sse("ping", {})
                        continue
                    kind = event.get("type", "")
                    if kind in ("error", "response.failed"):
                        raise BridgeError("Codex inference failed", 502)
                    terminal = kind in ("response.completed", "response.incomplete")
                    if terminal:
                        expected = "completed" if kind == "response.completed" else "incomplete"
                        if not isinstance(event.get("response"), dict) or event["response"].get("status") != expected:
                            raise BridgeError("Codex returned an inconsistent completion event", 502)
                    if terminal and path == "/v1/messages":
                        final_message = message(event["response"], self.server.model, body.get("stop_sequences", []))
                    if streaming:
                        rows = converter.feed(event) if converter else [(kind, event)]
                        for event_name, payload in rows:
                            self.sse(event_name, payload)
                    if terminal:
                        completed = True
                        if not streaming:
                            self.send_json(200, final_message if converter else event["response"])
                        break
            finally:
                cancelled.set()
        except BridgeError as exc:
            error = {"error": {"type": "api_error", "message": str(exc)}}
            if streaming:
                self.sse("error", error)
            else:
                self.send_json(exc.status, error)
        except (ValueError, TypeError, KeyError, UnicodeError):
            error = {"error": {"type": "invalid_request_error", "message": "Invalid bridge request or response"}}
            if streaming:
                self.sse("error", error)
            else:
                self.send_json(400, error)
        except (OSError, http.client.HTTPException):
            pass
        finally:
            if connection:
                with self.server.connection_lock:
                    self.server.connections.discard(connection)
                connection.close()

    def sse(self, name, row):
        row = dict(row)
        row.setdefault("type", name)
        self.wfile.write(("event: %s\ndata: %s\n\n" % (name, json_text(row))).encode())
        self.wfile.flush()
