#!/usr/bin/env bash
set -euo pipefail

# PostToolUse edit hook: the cheapest feedforward control a coding
# agent can have is being told, in the same turn, that the file it just wrote
# does not parse. Without it the error surfaces one command later, after the
# agent has reasoned on top of the broken file. Scope is syntax only — bash -n,
# a Python AST parse, JSON, and TOML where the stdlib has a parser — and the
# verdict is feedback, never a block: PostToolUse cannot undo the write, and a
# plain stdout line on this event reaches the transcript log rather than the
# model, so the finding rides hookSpecificOutput.additionalContext. Silent on a
# clean file, a tool that is not an edit, a file kind it cannot judge, and an
# unadopted repo. The parse pass resolves the event repo before it imports the
# routing helper. OMS_SYNTAX_GUARD_HOOK=0 disables it.

case "${OMS_SYNTAX_GUARD_HOOK:-1}" in
  0|false|FALSE|no|NO|off|OFF) exit 0 ;;
esac

payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
OMS_SGH_PAYLOAD="$payload" python3 - "$ROOT/scripts/lib" "$BASH" <<'PY' 2>/dev/null || true
import atexit
import ast
import json
import os
from pathlib import Path
import subprocess
import sys

EDIT_TOOLS = {"Edit", "Write", "MultiEdit"}
PATCH_TOOL = "apply_patch"
MAX_BYTES = 2 * 1024 * 1024
MAX_FILES = 12
MESSAGE_LIMIT = 599
# JSON-with-comments files fail a strict parse by design; never nag about them.
JSONC_PREFIXES = ("tsconfig", "jsconfig", "devcontainer")

try:
    row = json.loads(os.environ["OMS_SGH_PAYLOAD"])
except (ValueError, KeyError):
    raise SystemExit(0)
if not isinstance(row, dict):
    raise SystemExit(0)
sys.path.insert(0, sys.argv[1])
from hook_repo import native_path, resolve

repo = resolve(row)
adopted = repo is not None and (repo / ".oms").is_dir()
# A panel main edits in scratch worktrees that carry no .oms; its direct edits
# are counted against the room repository instead.
if not adopted and not os.environ.get("OMS_PANEL_MAIN_ATTEMPT"):
    raise SystemExit(0)
import hook_state

collaboration = hook_state.live_thread_hint(row, repo) if adopted else None
nudge = ""
findings = []


def emit_feedback():
    parts = [collaboration] if collaboration else []
    if nudge:
        parts.append(nudge)
    if findings:
        parts.append(("syntax-guard: " + " | ".join(findings))[:MESSAGE_LIMIT])
    if parts:
        print(json.dumps({"hookSpecificOutput": {"hookEventName": "PostToolUse",
                                                "additionalContext": "\n".join(parts)}}))


atexit.register(emit_feedback)
tool_name = row.get("tool_name")
tool_input = row.get("tool_input")
if not isinstance(tool_input, dict):
    raise SystemExit(0)
cwd = row.get("cwd") or row.get("currentWorkingDirectory")
cwd = native_path(cwd if isinstance(cwd, str) and cwd else os.getcwd())
if cwd is None:
    raise SystemExit(0)


def patch_path(name):
    if not isinstance(name, str) or not name or "\0" in name:
        return None
    # Python 3.13 no longer calls a single-slash Windows path absolute.
    if name.startswith("/") or os.path.isabs(name):
        return None
    if os.name == "nt" and (name.startswith("\\") or os.path.splitdrive(name)[0]):
        return None
    normalized = os.path.normpath(name)
    if normalized in ("", os.curdir, os.pardir):
        return None
    if normalized.startswith(os.pardir + os.sep):
        return None
    return os.path.join(cwd, normalized)


PATCH_LINES = {}
PATCH_DELETED = []


def patch_paths(command):
    names = []
    counts = []
    update_index = None
    body_index = None
    for line in command.splitlines():
        if line.startswith("*** Add File: "):
            names.append(line[len("*** Add File: "):])
            counts.append(0)
            update_index = None
            body_index = len(names) - 1
        elif line.startswith("*** Update File: "):
            names.append(line[len("*** Update File: "):])
            counts.append(0)
            update_index = body_index = len(names) - 1
        elif line.startswith("*** Delete File: "):
            deleted = patch_path(line[len("*** Delete File: "):])
            if deleted is not None and deleted not in PATCH_DELETED and len(PATCH_DELETED) < MAX_FILES:
                PATCH_DELETED.append(deleted)
            update_index = body_index = None
        elif line.startswith("*** Move to: ") and update_index is not None:
            names[update_index] = line[len("*** Move to: "):]
            update_index = None
        elif line.startswith("*** "):
            update_index = body_index = None
        elif body_index is not None and line[:1] in ("+", "-"):
            counts[body_index] += 1

    paths = []
    seen = set()
    for name, count in zip(names, counts):
        path = patch_path(name)
        if path is None:
            continue
        key = os.path.normcase(os.path.normpath(path))
        PATCH_LINES[path] = PATCH_LINES.get(path, 0) + count
        if key in seen:
            continue
        seen.add(key)
        paths.append(path)
        if len(paths) == MAX_FILES:
            break
    return paths


if tool_name in EDIT_TOOLS:
    path = native_path(tool_input.get("file_path"))
    paths = [path] if path else []
    if paths and not os.path.isabs(paths[0]):
        paths[0] = os.path.join(cwd, paths[0])
elif tool_name == PATCH_TOOL:
    command = tool_input.get("command")
    paths = patch_paths(command) if isinstance(command, str) else []
else:
    raise SystemExit(0)
if not paths and not PATCH_DELETED:
    raise SystemExit(0)


def text_lines(value):
    return len(value.splitlines()) or (1 if value else 0) if isinstance(value, str) else 0


def edit_lines():
    if tool_name == "Write":
        return text_lines(tool_input.get("content"))
    if tool_name == "Edit":
        return max(text_lines(tool_input.get("old_string")), text_lines(tool_input.get("new_string")), 1)
    if tool_name == "MultiEdit":
        edits = tool_input.get("edits")
        return sum(max(text_lines(e.get("old_string")), text_lines(e.get("new_string")), 1)
                   for e in edits if isinstance(e, dict)) if isinstance(edits, list) else 1
    return None


if os.environ.get("OMS_PANEL_MAIN_ATTEMPT"):
    counted = []
    for counted_path in paths + PATCH_DELETED:
        name = hook_state.panel_countable(cwd, counted_path)
        if name is not None:
            lines = edit_lines()
            # A deletion counts as a file even though its removed lines are unknown.
            counted.append((name, PATCH_LINES.get(counted_path, 0) if lines is None else lines))
    if counted:
        nudge = hook_state.panel_direct_edit_hint(row, counted)
if not adopted:
    raise SystemExit(0)


def first_lines(text, limit=3):
    lines = [line.strip() for line in text.strip().splitlines() if line.strip()]
    return " | ".join(lines[:limit])


def syntax_problem(path):
    try:
        if not os.path.isfile(path) or os.path.getsize(path) > MAX_BYTES:
            return None
        with open(path, "rb") as handle:
            head = handle.read(256)
    except OSError:
        return None

    lower = os.path.basename(path).lower()
    ext = os.path.splitext(lower)[1]
    kind = None
    if ext in (".sh", ".bash"):
        kind = "bash"
    elif ext == ".py":
        kind = "python"
    elif ext == ".json":
        if lower.startswith(JSONC_PREFIXES):
            return None
        kind = "json"
    elif ext == ".toml":
        kind = "toml"
    elif ext == "" and head.startswith(b"#!"):
        shebang = head.split(b"\n", 1)[0]
        if b"bash" in shebang or shebang.rstrip().endswith(b"sh"):
            kind = "bash"
        elif b"python" in shebang:
            kind = "python"
    if kind is None:
        return None

    try:
        shown = os.path.relpath(path, cwd)
    except ValueError:
        shown = path
    if shown.startswith(".."):
        shown = path

    problem = None
    if kind == "bash":
        try:
            # Reuse the host shell; Windows executable search can find WSL Bash.
            proc = subprocess.run(
                [sys.argv[2], "-n", path], capture_output=True, text=True, timeout=3
            )
        except (OSError, subprocess.TimeoutExpired):
            return None
        if proc.returncode != 0:
            detail = (proc.stderr or "syntax error").replace(path, shown)
            problem = "bash -n: " + first_lines(detail)
    elif kind == "python":
        try:
            with open(path, "rb") as handle:
                ast.parse(handle.read(), filename=path)
        except SyntaxError as exc:
            problem = "python: %s (line %s)" % (exc.msg, exc.lineno)
        except (OSError, ValueError, RecursionError):
            return None
    elif kind == "json":
        try:
            with open(path, encoding="utf-8") as handle:
                json.load(handle)
        except json.JSONDecodeError as exc:
            problem = "json: %s (line %d column %d)" % (exc.msg, exc.lineno, exc.colno)
        except (OSError, UnicodeDecodeError):
            return None
    elif kind == "toml":
        try:
            import tomllib
        except ImportError:
            return None
        try:
            with open(path, "rb") as handle:
                tomllib.load(handle)
        except tomllib.TOMLDecodeError as exc:
            problem = "toml: " + first_lines(str(exc), 1)
        except OSError:
            return None

    if problem is None:
        return None
    return shown, problem


for path in paths:
    finding = syntax_problem(path)
    if finding is not None:
        shown, problem = finding
        findings.append("%s does not parse after this %s — %s" % (
            shown, tool_name, problem
        ))
PY
exit 0
