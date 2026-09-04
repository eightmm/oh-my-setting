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
PRINT_EXPECTED=0
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"

# --- expected hook surfaces --------------------------------------------------
#
# The one place that says which Claude Code events this harness registers a
# hook on. The upsert loop below installs exactly these rows, and
# --print-expected emits the same rows as JSON, so a doctor can compare a live
# settings.json against what a given checkout would install rather than against
# a list it carries separately. An installed doctor holding its own copy could
# not see a surface added after it was installed, which is how two hooks stayed
# unregistered while the doctor reported the registration complete.
#
# Rows are event|script|matcher|timeout. An empty matcher or timeout field is
# omitted from the registration; the command is always `bash $ROOT/scripts/
# <script>`. Blank lines and # comments are ignored, so the reason a surface
# exists lives next to the surface.
#
# HOOKS_SCHEMA is bumped whenever a row changes. It is stamped into the install
# receipt at install/update time, which is what lets `doctor --surfaces` say
# "the installed harness predates this list" instead of silently comparing
# against the wrong expectations. tests/doctor-surfaces-smoke.sh hashes the row
# set and fails until the bump and the recorded hash move together.
HOOKS_SCHEMA=4
HOOK_SURFACES='
UserPromptSubmit|skill-router.sh||
Stop|turn-guard.sh||12

# Failed Bash commands feed the shared failure memory and surface what it
# already knows. Matcher-scoped so other tools failures never pay for it; a 5s
# ceiling so a wedged ledger cannot stall the turn.
PostToolUseFailure|fail-ledger-hook.sh|Bash|5

# The same script on the success event closes the loop: a command that passes
# resolves the row its failure wrote, so the ledger stops needing a human sweep
# to stay readable. Same matcher and ceiling; a repo with no failure history
# costs one stat.
PostToolUse|fail-ledger-hook.sh|Bash|5

# A file that does not parse after an edit is reported in the same turn, as
# feedback, never a block. Matcher-scoped to the edit tools (the matcher may
# carry alternatives: the row keeps its first two and last fields, the matcher
# is everything between); a 5s ceiling bounds bash -n on a pathological file.
PostToolUse|syntax-guard-hook.sh|Edit|Write|MultiEdit|5

# Compaction discards transcript detail; snapshot a handoff digest first. The
# hook is best-effort and self-bounded, but a ceiling keeps a huge transcript
# from stalling compaction.
PreCompact|precompact-handoff.sh||30

# A session that ends below the pressure bands and without ever compacting is
# the common case, and it used to capture nothing. Same script, same ceiling:
# it reads the event name off the payload and dedupes against a capture from
# the last few minutes.
SessionEnd|precompact-handoff.sh||30

# A resuming session starts knowing its active task, newest handoff, and open
# failures instead of rediscovering them. The prompt router owns peer warnings.
SessionStart|resume-hook.sh||10

# Only session/subagent lifecycle counters and bounded identifiers are retained.
# Tool-level telemetry used to start a shell and Python process after every tool
# call; the sparse useful fields did not justify that synchronous hot-path cost.
SessionStart|telemetry-hook.sh||5
SubagentStop|telemetry-hook.sh||5
SessionEnd|telemetry-hook.sh||5
'

# --print-expected is a read-only report about this checkout. It answers before
# the install lock and before the canonical-owner check on purpose: the doctor
# that needs the list is usually running from a workspace that does not own the
# install, and that is exactly the case worth reporting on.
case " $* " in
  *" --print-expected "*) PRINT_EXPECTED=1 ;;
esac

if [ "$PRINT_EXPECTED" = "0" ] && [ "${OMS_INSTALL_LOCK_HELD:-0}" != "1" ]; then
  oms_with_file_lock "$(oms_install_receipt_path)" \
    env OMS_INSTALL_LOCK_HELD=1 bash "$ROOT/scripts/install-claude-hooks.sh" "$@"
  exit $?
fi

usage() {
  cat <<'EOF'
Usage: install-claude-hooks.sh [--remove] [--settings PATH] [--print-expected]

Register oh-my-setting's UserPromptSubmit skill-router hook, Stop turn-guard
hook, PostToolUseFailure/PostToolUse fail-ledger hooks, PostToolUse
edit-time syntax-guard hook, PreCompact/SessionEnd handoff-snapshot hooks,
SessionStart resume hook, SessionStart/SubagentStop/SessionEnd telemetry hooks, main usage
HUD, and compact subagent HUD in Claude Code's
settings.json. The merge is additive: existing hooks and user-owned
statusLine/subagentStatusLine entries are preserved, and repeated installs are
idempotent. --remove deletes only oh-my-setting entries.

Options:
  --remove          Remove the oh-my-setting hook entries instead.
  --settings PATH   Settings file (default: ~/.claude/settings.json,
                    override with OMS_CLAUDE_SETTINGS).
  --print-expected  Print this checkout's expected event/script surfaces as
                    JSON and exit, changing nothing.
  -h, --help        Show help.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

# The manifest becomes structured rows in exactly one place. Both the upserts
# below and --print-expected consume this JSON, so the list a doctor compares
# against is the list that would actually be installed.
print_expected_json() {
  OMS_CH_ROOT="$ROOT" OMS_CH_SURFACES="$HOOK_SURFACES" \
    OMS_CH_HOOKS_SCHEMA="$HOOKS_SCHEMA" python3 <<'PY'
import json, os, sys

root = os.environ["OMS_CH_ROOT"]
surfaces = []
for line in os.environ["OMS_CH_SURFACES"].splitlines():
    line = line.strip()
    if not line or line.startswith("#"):
        continue
    parts = line.split("|")
    if len(parts) < 4:
        sys.stderr.write("error: malformed hook surface row: %s\n" % line)
        sys.exit(2)
    # event|script|matcher|timeout, where the matcher itself may be a
    # pipe-separated tool list: the first two and the last field are fixed,
    # the matcher is whatever lies between.
    event, script, timeout = parts[0].strip(), parts[1].strip(), parts[-1].strip()
    matcher = "|".join(part.strip() for part in parts[2:-1])
    if not event or not script:
        sys.stderr.write("error: hook surface row needs an event and a script: %s\n" % line)
        sys.exit(2)
    surfaces.append({
        "event": event,
        "script": script,
        "matcher": matcher or None,
        "timeout": int(timeout) if timeout else None,
        "command": "bash %s/scripts/%s" % (root, script),
    })
json.dump(
    {
        "schema": 1,
        "hooks_schema": int(os.environ["OMS_CH_HOOKS_SCHEMA"]),
        "root": root,
        "surfaces": surfaces,
    },
    sys.stdout,
    indent=2,
    sort_keys=True,
)
sys.stdout.write("\n")
PY
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --remove) REMOVE=1; shift ;;
    --settings) [ "$#" -ge 2 ] || fail "--settings requires a path"; SETTINGS="$2"; shift 2 ;;
    --print-expected) PRINT_EXPECTED=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

if [ "$PRINT_EXPECTED" = "1" ]; then
  print_expected_json
  exit 0
fi

[ -f "$ROOT/scripts/skill-router.sh" ] || fail "skill-router.sh not found under $ROOT"
[ -f "$ROOT/scripts/turn-guard.sh" ] || fail "turn-guard.sh not found under $ROOT"
[ -f "$ROOT/scripts/fail-ledger-hook.sh" ] || fail "fail-ledger-hook.sh not found under $ROOT"
[ -f "$ROOT/scripts/syntax-guard-hook.sh" ] || fail "syntax-guard-hook.sh not found under $ROOT"
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

EXPECTED_JSON="$(print_expected_json)"

OMS_CH_SETTINGS="$SETTINGS" OMS_CH_REMOVE="$REMOVE" \
  OMS_CH_EXPECTED="$EXPECTED_JSON" \
  OMS_CH_STATUS_PATH="$ROOT/scripts/claude-statusline.py" \
  OMS_CH_SUBAGENT_STATUS_PATH="$ROOT/scripts/claude-subagent-statusline.py" python3 <<'PY'
import json, os, shlex, sys, tempfile

path = os.environ["OMS_CH_SETTINGS"]
remove = os.environ["OMS_CH_REMOVE"] == "1"
expected = json.loads(os.environ["OMS_CH_EXPECTED"])["surfaces"]
status_cmd = "python3 %s" % shlex.quote(os.environ["OMS_CH_STATUS_PATH"])
subagent_status_cmd = "python3 %s" % shlex.quote(
    os.environ["OMS_CH_SUBAGENT_STATUS_PATH"]
)
MARKS = (
    "skill-router.sh", "turn-guard.sh", "fail-ledger-hook.sh",
    "syntax-guard-hook.sh", "tier-guard-hook.sh", "precompact-handoff.sh",
    "resume-hook.sh", "telemetry-hook.sh",
)
expected_pairs = {(row["event"], row["script"]) for row in expected}

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

def managed_mark(hook):
    command = str(hook.get("command", "")) if isinstance(hook, dict) else ""
    return next((mark for mark in MARKS if mark in command), None)

def prune_obsolete_surfaces():
    """Converge old OMS rows without touching hooks the user owns."""
    for event in list(hooks):
        entries = hooks.get(event)
        if not isinstance(entries, list):
            continue
        kept_entries = []
        for entry in entries:
            if not isinstance(entry, dict) or not isinstance(entry.get("hooks"), list):
                kept_entries.append(entry)
                continue
            kept_hooks = []
            removed = False
            for hook in entry["hooks"]:
                mark = managed_mark(hook)
                if mark is not None and (event, mark) not in expected_pairs:
                    removed = True
                    continue
                kept_hooks.append(hook)
            if kept_hooks or not removed:
                if removed:
                    entry = dict(entry, hooks=kept_hooks)
                kept_entries.append(entry)
        if kept_entries:
            hooks[event] = kept_entries
        else:
            del hooks[event]

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
    prune_obsolete_surfaces()
    for surface in expected:
        upsert(
            surface["event"], surface["script"], surface["command"],
            matcher=surface["matcher"], timeout=surface["timeout"],
        )
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

# Record which surface list this registration came from. link.sh rewrites the
# whole receipt on every install and update and knows nothing about hooks, and
# this script always runs after it, so the stamp has to happen here and on
# every non-remove run — including the "already current" path, which is the
# common case for an update that changes no command paths.
#
# Without it, an installed harness whose expected list predates the source's
# looks identical to one that is current, which is precisely the drift that let
# two hooks stay unregistered while the doctor reported ok.
stamp_receipt_hooks_schema() {
  local receipt
  local owner

  receipt="$(oms_install_receipt_path)"
  [ -f "$receipt" ] || return 0
  # Only the canonical owner may speak for the install; another checkout
  # running --remove is not evidence about what the owner registered.
  owner="$(oms_install_receipt_owner "$receipt" 2>/dev/null)" || return 0
  [ "$owner" = "$ROOT" ] || return 0

  OMS_CH_HOOKS_SCHEMA="$HOOKS_SCHEMA" OMS_CH_REMOVE="$REMOVE" \
    python3 - "$receipt" <<'PY'
import json, os, sys, tempfile

receipt = sys.argv[1]
value = None if os.environ["OMS_CH_REMOVE"] == "1" else int(
    os.environ["OMS_CH_HOOKS_SCHEMA"]
)
try:
    with open(receipt, encoding="utf-8") as fh:
        row = json.load(fh)
except Exception:
    sys.exit(1)
if not isinstance(row, dict):
    sys.exit(1)
if value is None:
    if "hooks_schema" not in row:
        sys.exit(0)
    row.pop("hooks_schema")
else:
    if row.get("hooks_schema") == value:
        sys.exit(0)
    row["hooks_schema"] = value

parent = os.path.dirname(receipt) or "."
fd, tmp = tempfile.mkstemp(prefix=".install.", suffix=".tmp", dir=parent)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        json.dump(row, fh, ensure_ascii=False, indent=2, sort_keys=True)
        fh.write("\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, receipt)
except Exception:
    try:
        os.unlink(tmp)
    except FileNotFoundError:
        pass
    raise
PY
}

# Best-effort: a receipt this script could not stamp is a stale doctor warning,
# not a failed hook registration, and install.sh already treats this script's
# exit status as pass/fail for the registration itself.
stamp_receipt_hooks_schema ||
  echo "warning: could not record hooks_schema in the install receipt" >&2
