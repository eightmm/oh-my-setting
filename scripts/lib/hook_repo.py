#!/usr/bin/env python3
"""Resolve hook state repositories without importing the routing helper."""

import json
import os
from pathlib import Path
import subprocess


def native_path(raw):
    if not isinstance(raw, str) or not raw:
        return None
    # Payload/environment paths bypass Git Bash's argv conversion for native
    # Windows Python. cygpath also handles mounted roots such as /tmp.
    if os.name == "nt" and raw.startswith("/"):
        try:
            raw = subprocess.check_output(
                ["cygpath", "-m", raw], text=True,
                stderr=subprocess.DEVNULL, timeout=2,
            ).strip()
        except (OSError, subprocess.SubprocessError):
            return None
    return raw or None


def root(raw):
    raw = native_path(raw)
    if raw is None:
        return None
    path = Path(raw).expanduser()
    if not path.is_dir():
        return None
    try:
        proc = subprocess.run(
            ["git", "-C", str(path), "rev-parse", "--show-toplevel"],
            check=False, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True, timeout=2,
        )
        if proc.returncode == 0 and proc.stdout.strip():
            return Path(os.path.realpath(proc.stdout.strip()))
    except Exception:
        pass
    try:
        return path.resolve()
    except OSError:
        return None


def resolve(payload, environ=None, fallback=None):
    environ = os.environ if environ is None else environ
    if environ.get("OMS_STATE_REPO"):
        return root(environ["OMS_STATE_REPO"])
    cwd = None
    if isinstance(payload, dict):
        for key in ("cwd", "currentWorkingDirectory"):
            value = payload.get(key)
            if value is not None and value != "":
                cwd = value
                break
    if cwd is not None:
        return root(cwd)
    return root(fallback or os.getcwd())


def main():
    raw = os.environ.get("OMS_HOOK_PAYLOAD") or "{}"
    if not raw.strip():
        raw = "{}"
    try:
        payload = json.loads(raw)
    except (TypeError, ValueError):
        return
    if not isinstance(payload, dict):
        return
    repo = resolve(payload)
    if repo is not None:
        print(repo.as_posix())


if __name__ == "__main__":
    main()
