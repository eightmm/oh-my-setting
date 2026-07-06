#!/usr/bin/env bash
set -euo pipefail

# Register (or remove) oh-my-setting's Claude Code hooks in the user's
# ~/.claude/settings.json. Additive merge: existing settings and hooks are
# preserved, our entries are identified by the "oh-my-setting" +
# "skill-router.sh" substring in the command, install is idempotent
# (re-running updates the command path in place), and a one-time backup is
# written next to the file before the first change. Writes are tmp+mv.
#
# Codex/Antigravity have no hook system; this is deliberately Claude-only.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SETTINGS="${OMS_CLAUDE_SETTINGS:-$HOME/.claude/settings.json}"
REMOVE=0

usage() {
  cat <<'EOF'
Usage: install-claude-hooks.sh [--remove] [--settings PATH]

Register oh-my-setting's skill-router UserPromptSubmit hook in Claude Code's
settings.json (additive; existing hooks preserved; idempotent). --remove
deletes only entries whose command points at oh-my-setting's skill-router.

Options:
  --remove          Remove the oh-my-setting hook entries instead.
  --settings PATH   Settings file (default: ~/.claude/settings.json,
                    override with OMS_CLAUDE_SETTINGS).
  -h, --help        Show help.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --remove) REMOVE=1; shift ;;
    --settings) [ "$#" -ge 2 ] || fail "--settings requires a path"; SETTINGS="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

command -v python3 >/dev/null 2>&1 || fail "python3 is required"
[ -f "$ROOT/scripts/skill-router.sh" ] || fail "skill-router.sh not found under $ROOT"

if [ "$REMOVE" = 1 ] && [ ! -f "$SETTINGS" ]; then
  echo "claude-hooks: nothing to remove ($SETTINGS absent)"
  exit 0
fi
mkdir -p "$(dirname "$SETTINGS")"

OMS_CH_SETTINGS="$SETTINGS" OMS_CH_REMOVE="$REMOVE" \
  OMS_CH_CMD="bash $ROOT/scripts/skill-router.sh" python3 <<'PY'
import json, os, sys, tempfile

path = os.environ["OMS_CH_SETTINGS"]
remove = os.environ["OMS_CH_REMOVE"] == "1"
cmd = os.environ["OMS_CH_CMD"]
MARK = "skill-router.sh"

settings = {}
if os.path.isfile(path):
    with open(path, encoding="utf-8") as fh:
        raw = fh.read()
    if raw.strip():
        try:
            settings = json.loads(raw)
        except Exception as e:
            sys.stderr.write("error: %s is not valid JSON (%s); fix it first\n" % (path, e))
            sys.exit(2)
if not isinstance(settings, dict):
    sys.stderr.write("error: %s top level is not an object\n" % path)
    sys.exit(2)

hooks = settings.setdefault("hooks", {})
entries = hooks.setdefault("UserPromptSubmit", [])

def ours(entry):
    for h in entry.get("hooks", []) if isinstance(entry, dict) else []:
        if MARK in str(h.get("command", "")) and "oh-my-setting" in str(h.get("command", "")):
            return True
    return False

before = json.dumps(settings, sort_keys=True)
if remove:
    hooks["UserPromptSubmit"] = [e for e in entries if not ours(e)]
    if not hooks["UserPromptSubmit"]:
        del hooks["UserPromptSubmit"]
    if not hooks:
        del settings["hooks"]
    action = "removed"
else:
    existing = [e for e in entries if ours(e)]
    if existing:
        for e in existing:
            for h in e.get("hooks", []):
                if MARK in str(h.get("command", "")):
                    h["command"] = cmd
        action = "updated"
    else:
        entries.append({"hooks": [{"type": "command", "command": cmd}]})
        action = "installed"

if json.dumps(settings, sort_keys=True) == before:
    print("claude-hooks: already %s (%s)" % ("absent" if remove else "current", path))
    sys.exit(0)

# One-time backup before the first change we make to this file.
bak = path + ".oms-bak"
if os.path.isfile(path) and not os.path.exists(bak):
    with open(path, "rb") as src, open(bak, "wb") as dst:
        dst.write(src.read())

fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path) or ".")
try:
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        json.dump(settings, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    os.replace(tmp, path)
except Exception:
    os.unlink(tmp)
    raise
print("claude-hooks: %s skill-router hook (%s)" % (action, path))
PY
