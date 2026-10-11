"""Use native Codex credential storage and refresh without copying credentials."""

import base64
import json
import os
import queue
import subprocess
import threading
import time


class BridgeError(Exception):
    def __init__(self, message, status=400):
        super().__init__(message)
        self.status = status


def subscription_env():
    env = dict(os.environ)
    for name in ("OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN"):
        env.pop(name, None)
    return env


class AuthBroker:
    def __init__(self, binary="codex"):
        command = [binary] if isinstance(binary, str) else list(binary)
        self.process = subprocess.Popen(
            command + ["app-server", "--listen", "stdio://"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            env=subscription_env(),
        )
        self.messages = queue.Queue(maxsize=128)
        self.lock = threading.Lock()
        self.ident = 0
        self.cached = None
        self.cached_at = 0
        threading.Thread(target=self._read, daemon=True).start()
        try:
            self.call("initialize", {"clientInfo": {"name": "oms_harness", "version": "1"}})
            self._send({"method": "initialized", "params": {}})
        except BaseException:
            self.close()
            raise

    def _read(self):
        try:
            while True:
                line = self.process.stdout.readline(1024 * 1024 + 1)
                if not line or len(line) > 1024 * 1024:
                    break
                row = json.loads(line)
                if "id" in row:
                    self.messages.put(row, timeout=2)
        except (ValueError, OSError, queue.Full):
            pass
        finally:
            try:
                self.messages.put(None, timeout=2)
            except queue.Full:
                pass

    def _send(self, row):
        try:
            self.process.stdin.write(json.dumps(row).encode() + b"\n")
            self.process.stdin.flush()
        except (OSError, ValueError):
            raise BridgeError("Codex authentication broker disconnected", 503) from None

    def call(self, method, params):
        with self.lock:
            self.ident += 1
            self._send({"id": self.ident, "method": method, "params": params})
            deadline = time.monotonic() + 20
            while True:
                try:
                    row = self.messages.get(timeout=max(0, deadline - time.monotonic()))
                except queue.Empty:
                    raise BridgeError("Codex authentication broker timed out", 503) from None
                if row is None:
                    raise BridgeError("Codex authentication broker disconnected", 503)
                if row.get("id") != self.ident:
                    continue
                if "error" in row:
                    # Native errors may contain credential or configuration details.
                    raise BridgeError("Codex does not support the required authentication RPC: " + method, 503)
                return row.get("result", {})

    def credentials(self, refresh=False):
        if not refresh and self.cached and time.monotonic() - self.cached_at < 60:
            return self.cached
        account = self.call("account/read", {"refreshToken": refresh}).get("account") or {}
        if account.get("type") != "chatgpt":
            raise BridgeError("An existing Codex ChatGPT subscription login is required", 401)
        auth = self.call("getAuthStatus", {"includeToken": True, "refreshToken": False})
        token = auth.get("authToken")
        if auth.get("authMethod") != "chatgpt" or not isinstance(token, str):
            raise BridgeError("Codex cannot export the current subscription credential", 401)
        try:
            encoded = token.split(".")[1]
            claims = json.loads(base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4)))
            account_id = claims["https://api.openai.com/auth"]["chatgpt_account_id"]
            expires = claims.get("exp", 0)
            if not isinstance(account_id, str) or not account_id or any(ord(c) < 32 for c in account_id + token):
                raise ValueError()
            if not isinstance(expires, (int, float)):
                raise ValueError()
        except (ValueError, KeyError, IndexError, TypeError):
            raise BridgeError("Codex returned an unsupported subscription credential", 401) from None
        if expires < time.time() + 120:
            if refresh:
                raise BridgeError("Codex subscription login expired; sign in again in Codex", 401)
            return self.credentials(refresh=True)
        self.cached = token, account_id
        self.cached_at = time.monotonic()
        return self.cached

    def model(self, name):
        cursor = None
        for _ in range(20):
            page = self.call("model/list", {"cursor": cursor, "limit": 100})
            for row in page.get("data", []):
                if row.get("model") == name or row.get("id") == name:
                    return row
            cursor = page.get("nextCursor")
            if not cursor:
                break
        raise BridgeError("The exact model is absent from the installed Codex catalog: " + name)

    def close(self):
        self.cached = None
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=3)
        for stream in (self.process.stdin, self.process.stdout):
            stream.close()
