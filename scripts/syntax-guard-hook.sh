#!/usr/bin/env bash
set -euo pipefail

# PostToolUse hook on Edit/Write: the cheapest feedforward control a coding
# agent can have is being told, in the same turn, that the file it just wrote
# does not parse. Without it the error surfaces one command later, after the
# agent has reasoned on top of the broken file. Scope is syntax only — bash -n,
# a Python AST parse, JSON, and TOML where the stdlib has a parser — and the
# verdict is feedback, never a block: PostToolUse cannot undo the write, and a
# plain stdout line on this event reaches the transcript log rather than the
# model, so the finding rides hookSpecificOutput.additionalContext. Silent on a
# clean file, a tool that is not an edit, a file kind it cannot judge, and an
# unadopted repo (the same gate as the other hooks: an edit outside the harness
# costs a rev-parse, not a python spawn). OMS_SYNTAX_GUARD_HOOK=0 disables it.

case "${OMS_SYNTAX_GUARD_HOOK:-1}" in
  0|false|FALSE|no|NO|off|OFF) exit 0 ;;
esac

payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

repo="${OMS_STATE_REPO:-$PWD}"
root="$(git -C "$repo" rev-parse --show-toplevel 2>/dev/null || printf '%s' "$repo")"
[ -d "$root/.oms" ] || exit 0

OMS_SGH_PAYLOAD="$payload" python3 - <<'PY' 2>/dev/null || true
import ast
import json
import os
import subprocess
import sys

EDIT_TOOLS = {"Edit", "Write", "MultiEdit"}
MAX_BYTES = 2 * 1024 * 1024
MESSAGE_LIMIT = 600
# JSON-with-comments files fail a strict parse by design; never nag about them.
JSONC_PREFIXES = ("tsconfig", "jsconfig", "devcontainer")

try:
    row = json.loads(os.environ["OMS_SGH_PAYLOAD"])
except (ValueError, KeyError):
    raise SystemExit(0)
if not isinstance(row, dict) or row.get("tool_name") not in EDIT_TOOLS:
    raise SystemExit(0)
tool_input = row.get("tool_input")
path = tool_input.get("file_path") if isinstance(tool_input, dict) else None
if not isinstance(path, str) or not path:
    raise SystemExit(0)
cwd = row.get("cwd") if isinstance(row.get("cwd"), str) and row.get("cwd") else os.getcwd()
if not os.path.isabs(path):
    path = os.path.join(cwd, path)
try:
    if not os.path.isfile(path) or os.path.getsize(path) > MAX_BYTES:
        raise SystemExit(0)
except OSError:
    raise SystemExit(0)

try:
    with open(path, "rb") as handle:
        head = handle.read(256)
except OSError:
    raise SystemExit(0)

lower = os.path.basename(path).lower()
ext = os.path.splitext(lower)[1]
kind = None
if ext in (".sh", ".bash"):
    kind = "bash"
elif ext == ".py":
    kind = "python"
elif ext == ".json":
    if lower.startswith(JSONC_PREFIXES):
        raise SystemExit(0)
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
    raise SystemExit(0)

try:
    shown = os.path.relpath(path, cwd)
except ValueError:
    shown = path
if shown.startswith(".."):
    shown = path


def first_lines(text, limit=3):
    lines = [line.strip() for line in text.strip().splitlines() if line.strip()]
    return " | ".join(lines[:limit])


problem = None
if kind == "bash":
    try:
        proc = subprocess.run(
            ["bash", "-n", path], capture_output=True, text=True, timeout=3
        )
    except (OSError, subprocess.TimeoutExpired):
        raise SystemExit(0)
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
        raise SystemExit(0)
elif kind == "json":
    try:
        with open(path, encoding="utf-8") as handle:
            json.load(handle)
    except json.JSONDecodeError as exc:
        problem = "json: %s (line %d column %d)" % (exc.msg, exc.lineno, exc.colno)
    except (OSError, UnicodeDecodeError):
        raise SystemExit(0)
elif kind == "toml":
    try:
        import tomllib
    except ImportError:
        raise SystemExit(0)
    try:
        with open(path, "rb") as handle:
            tomllib.load(handle)
    except tomllib.TOMLDecodeError as exc:
        problem = "toml: " + first_lines(str(exc), 1)
    except OSError:
        raise SystemExit(0)

if problem is None:
    raise SystemExit(0)
message = "syntax-guard: %s does not parse after this %s — %s" % (
    shown, row.get("tool_name"), problem
)
json.dump(
    {
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": message[:MESSAGE_LIMIT],
        }
    },
    sys.stdout,
)
sys.stdout.write("\n")
PY
exit 0
