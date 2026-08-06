#!/usr/bin/env bash
set -euo pipefail

# Register (or remove) oh-my-setting's Claude Code hooks and HUDs in the user's
# ~/.claude/settings.json. Additive merge: existing settings and hooks are
# preserved, our entries are identified by the "oh-my-setting" script names,
# install is idempotent (re-running updates command paths in place), and a
# one-time backup is written next to the file before the first change.
#
# Codex gets equivalent hooks through install-codex-plugin.sh; this file only
# manages Claude Code settings.json.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
SETTINGS="${OMS_CLAUDE_SETTINGS:-$HOME/.claude/settings.json}"
REMOVE=0
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"

if [ "${OMS_INSTALL_LOCK_HELD:-0}" != "1" ]; then
  oms_with_file_lock "$(oms_install_receipt_path)" \
    env OMS_INSTALL_LOCK_HELD=1 bash "$ROOT/scripts/install-claude-hooks.sh" "$@"
  exit $?
fi

usage() {
  cat <<'EOF'
Usage: install-claude-hooks.sh [--remove] [--settings PATH]

Register oh-my-setting's UserPromptSubmit skill-router hook, Stop turn-guard
hook, PostToolUseFailure/PostToolUse fail-ledger hooks, PreCompact/SessionEnd
handoff-snapshot hooks, SessionStart resume hook,
SessionStart/PostToolUse/SubagentStop/SessionEnd telemetry hooks, main usage
HUD, and compact subagent HUD in Claude Code's
settings.json. The merge is additive: existing hooks and user-owned
statusLine/subagentStatusLine entries are preserved, and repeated installs are
idempotent. --remove deletes only oh-my-setting entries.

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
[ -f "$ROOT/scripts/turn-guard.sh" ] || fail "turn-guard.sh not found under $ROOT"
[ -f "$ROOT/scripts/fail-ledger-hook.sh" ] || fail "fail-ledger-hook.sh not found under $ROOT"
[ -f "$ROOT/scripts/precompact-handoff.sh" ] || fail "precompact-handoff.sh not found under $ROOT"
[ -f "$ROOT/scripts/resume-hook.sh" ] || fail "resume-hook.sh not found under $ROOT"
[ -f "$ROOT/scripts/telemetry-hook.sh" ] || fail "telemetry-hook.sh not found under $ROOT"
[ -f "$ROOT/scripts/claude-statusline.py" ] || fail "claude-statusline.py not found under $ROOT"
[ -f "$ROOT/scripts/claude-subagent-statusline.py" ] || fail "claude-subagent-statusline.py not found under $ROOT"

if [ "$REMOVE" != "1" ] && [ -f "$(oms_install_receipt_path)" ]; then
  owner="$(oms_install_receipt_owner "$(oms_install_receipt_path)" 2>/dev/null)" ||
    fail "invalid install receipt: $(oms_install_receipt_path)"
  [ "$owner" = "$ROOT" ] ||
    fail "this checkout is not the canonical install owner: $ROOT (owner: $owner)"
fi

if [ "$REMOVE" = 1 ] && [ ! -f "$SETTINGS" ]; then
  echo "claude-hooks: nothing to remove ($SETTINGS absent)"
  exit 0
fi
mkdir -p "$(dirname "$SETTINGS")"

OMS_CH_SETTINGS="$SETTINGS" OMS_CH_REMOVE="$REMOVE" \
  OMS_CH_SKILL_CMD="bash $ROOT/scripts/skill-router.sh" \
  OMS_CH_GUARD_CMD="bash $ROOT/scripts/turn-guard.sh" \
  OMS_CH_FAIL_CMD="bash $ROOT/scripts/fail-ledger-hook.sh" \
  OMS_CH_PRECOMPACT_CMD="bash $ROOT/scripts/precompact-handoff.sh" \
  OMS_CH_RESUME_CMD="bash $ROOT/scripts/resume-hook.sh" \
  OMS_CH_TELEMETRY_CMD="bash $ROOT/scripts/telemetry-hook.sh" \
  OMS_CH_STATUS_PATH="$ROOT/scripts/claude-statusline.py" \
  OMS_CH_SUBAGENT_STATUS_PATH="$ROOT/scripts/claude-subagent-statusline.py" python3 <<'PY'
import json, os, shlex, sys, tempfile

path = os.environ["OMS_CH_SETTINGS"]
remove = os.environ["OMS_CH_REMOVE"] == "1"
skill_cmd = os.environ["OMS_CH_SKILL_CMD"]
guard_cmd = os.environ["OMS_CH_GUARD_CMD"]
fail_cmd = os.environ["OMS_CH_FAIL_CMD"]
precompact_cmd = os.environ["OMS_CH_PRECOMPACT_CMD"]
resume_cmd = os.environ["OMS_CH_RESUME_CMD"]
telemetry_cmd = os.environ["OMS_CH_TELEMETRY_CMD"]
status_cmd = "python3 %s" % shlex.quote(os.environ["OMS_CH_STATUS_PATH"])
subagent_status_cmd = "python3 %s" % shlex.quote(
    os.environ["OMS_CH_SUBAGENT_STATUS_PATH"]
)
MARKS = (
    "skill-router.sh", "turn-guard.sh", "fail-ledger-hook.sh",
    "precompact-handoff.sh", "resume-hook.sh", "telemetry-hook.sh",
)

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

def ours(entry):
    for h in entry.get("hooks", []) if isinstance(entry, dict) else []:
        cmd = str(h.get("command", ""))
        if any(mark in cmd for mark in MARKS):
            return True
    return False

def upsert(event, mark, cmd, matcher=None, timeout=None):
    entries = hooks.setdefault(event, [])
    existing = [e for e in entries if any(
        mark in str(h.get("command", ""))
        for h in e.get("hooks", []) if isinstance(e, dict)
    )]
    if existing:
        for e in existing:
            for h in e.get("hooks", []):
                if mark in str(h.get("command", "")):
                    h["command"] = cmd
    else:
        hook = {"type": "command", "command": cmd}
        if timeout is not None:
            hook["timeout"] = timeout
        entry = {"hooks": [hook]}
        if matcher is not None:
            entry["matcher"] = matcher
        entries.append(entry)

def status_ours(value, command, script_name):
    if not isinstance(value, dict):
        return False
    if value.get("command") == command:
        return True
    try:
        argv = shlex.split(str(value.get("command", "")))
    except ValueError:
        return False
    if not (
        len(argv) == 2
        and argv[0] == "python3"
        and argv[1].replace("\\", "/").rsplit("/", 1)[-1] == script_name
    ):
        return False
    # A previous canonical checkout can move. Recognize it only when the
    # command still points into an actual OMS checkout; a user's unrelated
    # script with the same basename remains user-owned.
    candidate_root = os.path.dirname(os.path.dirname(os.path.realpath(argv[1])))
    return (
        os.path.isfile(os.path.join(candidate_root, ".agents", "plugins", "marketplace.json"))
        and os.path.isfile(os.path.join(candidate_root, "scripts", "install-claude-hooks.sh"))
    )

before = json.dumps(settings, sort_keys=True)
if remove:
    for event in list(hooks):
        entries = hooks.get(event)
        if isinstance(entries, list):
            hooks[event] = [e for e in entries if not ours(e)]
            if not hooks[event]:
                del hooks[event]
    if not hooks:
        del settings["hooks"]
    if status_ours(settings.get("statusLine"), status_cmd, "claude-statusline.py"):
        del settings["statusLine"]
    if status_ours(
        settings.get("subagentStatusLine"),
        subagent_status_cmd,
        "claude-subagent-statusline.py",
    ):
        del settings["subagentStatusLine"]
    action = "removed"
else:
    upsert("UserPromptSubmit", "skill-router.sh", skill_cmd)
    upsert("Stop", "turn-guard.sh", guard_cmd, timeout=12)
    # Failed Bash commands feed the shared failure memory and surface what it
    # already knows. Matcher-scoped so other tools' failures never pay for it;
    # a 5s ceiling so a wedged ledger cannot stall the turn.
    upsert(
        "PostToolUseFailure", "fail-ledger-hook.sh", fail_cmd,
        matcher="Bash", timeout=5,
    )
    # The same script on the success event closes the loop: a command that
    # passes resolves the row its failure wrote, so the ledger stops needing a
    # human sweep to stay readable. Same matcher and ceiling; a repo with no
    # failure history costs one stat.
    upsert(
        "PostToolUse", "fail-ledger-hook.sh", fail_cmd,
        matcher="Bash", timeout=5,
    )
    # Compaction discards transcript detail; snapshot a handoff digest first.
    # The hook is best-effort and self-bounded, but a ceiling keeps a huge
    # transcript from stalling compaction.
    upsert("PreCompact", "precompact-handoff.sh", precompact_cmd, timeout=30)
    # A session that ends below the pressure bands and without ever compacting
    # is the common case, and it used to capture nothing. Same script, same
    # ceiling: it reads the event name off the payload and dedupes against a
    # capture from the last few minutes.
    upsert("SessionEnd", "precompact-handoff.sh", precompact_cmd, timeout=30)
    # A resuming session starts knowing its active task, newest handoff,
    # open failures, and live-peer status instead of rediscovering them.
    upsert("SessionStart", "resume-hook.sh", resume_cmd, timeout=10)
    # Only content-free counters and bounded identifiers are retained. Keep
    # every event under the recommended five-second synchronous hook budget.
    for event in ("SessionStart", "PostToolUse", "SubagentStop", "SessionEnd"):
        upsert(event, "telemetry-hook.sh", telemetry_cmd, timeout=5)
    if "statusLine" not in settings:
        settings["statusLine"] = {"type": "command", "command": status_cmd}
    elif status_ours(
        settings.get("statusLine"), status_cmd, "claude-statusline.py"
    ):
        settings["statusLine"]["type"] = "command"
        settings["statusLine"]["command"] = status_cmd
    if "subagentStatusLine" not in settings:
        settings["subagentStatusLine"] = {
            "type": "command",
            "command": subagent_status_cmd,
        }
    elif status_ours(
        settings.get("subagentStatusLine"),
        subagent_status_cmd,
        "claude-subagent-statusline.py",
    ):
        settings["subagentStatusLine"]["type"] = "command"
        settings["subagentStatusLine"]["command"] = subagent_status_cmd
    action = "installed"

if json.dumps(settings, sort_keys=True) == before:
    print("claude-settings: already %s (%s)" % ("absent" if remove else "current", path))
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
print("claude-settings: %s oh-my-setting hooks/HUDs (%s)" % (action, path))
PY
