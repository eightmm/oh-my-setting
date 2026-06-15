#!/usr/bin/env bash
set -euo pipefail

# Read one agent CLI's session transcript and distill it into a compact,
# portable handoff digest that another agent can load as context. Extraction
# is purely mechanical (no model call): deterministic and free. Full
# transcripts are huge and tool-noisy, so we capture goal, recent user turns,
# files touched, and the last assistant summary instead of the raw log.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

CLAUDE_HOME="${OMS_CLAUDE_HOME:-$HOME/.claude}"
CODEX_HOME="${OMS_CODEX_HOME:-$HOME/.codex}"
GEMINI_HOME="${OMS_GEMINI_HOME:-$HOME/.gemini}"

AGENT=""
SESSION=""
CWD="$PWD"
OUT=""
NOTE=""
TURNS="${OMS_HANDOFF_TURNS:-6}"

usage() {
  cat <<'EOF'
Usage: session-handoff.sh <capture|list|show> [options]

Distill an agent CLI session into a portable handoff digest.

Subcommands:
  capture   Read a session and write a digest to .oms/handoffs/.
  list      List captured handoff digests (newest first).
  show FILE Print a captured digest to stdout.

capture options:
  --agent NAME     claude | codex | antigravity (default: claude).
  --session ID     Session id / file. Default: most recent for --cwd.
  --cwd PATH       Project dir to match (default: current dir).
  --note TEXT      Free-text note added to the digest header.
  --out FILE       Write digest here instead of the default handoffs dir.
  --allow-sensitive  Write the digest even if it looks sensitive (default:
                   refuse, since the digest is meant for another agent).

Notes:
  - Extraction is mechanical; no model is called.
  - antigravity stores only user prompts in history.jsonl (assistant output
    lives in opaque protobuf), so its digest is prompts-only (best-effort).
  - Digests are local artifacts; loading one into another agent is your step.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

claude_project_dir() {
  # Claude encodes the project cwd by replacing every '/' with '-'.
  local cwd="$1"
  printf '%s/projects/%s\n' "$CLAUDE_HOME" "$(printf '%s' "$cwd" | sed 's#/#-#g')"
}

newest_file() {
  # Print the most recently modified file among the args, or nothing.
  local newest=""
  local f
  for f in "$@"; do
    [ -f "$f" ] || continue
    if [ -z "$newest" ] || [ "$f" -nt "$newest" ]; then
      newest="$f"
    fi
  done
  [ -n "$newest" ] && printf '%s\n' "$newest"
}

resolve_claude_session() {
  local cwd="$1"
  local id="$2"
  local dir
  dir="$(claude_project_dir "$cwd")"
  if [ -n "$id" ]; then
    local path="$dir/$id.jsonl"
    [ -f "$path" ] || fail "claude session not found: $path"
    printf '%s\n' "$path"
    return 0
  fi
  [ -d "$dir" ] || fail "no claude sessions for cwd: $cwd ($dir)"
  newest_file "$dir"/*.jsonl
}

resolve_codex_session() {
  local cwd="$1"
  local id="$2"
  if [ -n "$id" ]; then
    local hit
    hit="$(find "$CODEX_HOME/sessions" "$CODEX_HOME/archived_sessions" \
      -type f -name "*$id*.jsonl" 2>/dev/null | head -n 1)"
    [ -n "$hit" ] || fail "codex session not found for id: $id"
    printf '%s\n' "$hit"
    return 0
  fi
  # No id: pick the newest rollout whose session_meta/turn_context cwd matches.
  local f
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    if OMS_MATCH_CWD="$cwd" python3 - "$f" <<'PY'
import json, os, sys
want = os.environ["OMS_MATCH_CWD"]
path = sys.argv[1]
try:
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            try:
                obj = json.loads(line)
            except Exception:
                continue
            p = obj.get("payload", {})
            if not isinstance(p, dict):
                continue
            if p.get("cwd") == want or obj.get("cwd") == want:
                sys.exit(0)
    sys.exit(1)
except OSError:
    sys.exit(1)
PY
    then
      printf '%s\n' "$f"
      return 0
    fi
  done <<EOF
$(find "$CODEX_HOME/sessions" "$CODEX_HOME/archived_sessions" -type f -name 'rollout-*.jsonl' -exec ls -1t {} + 2>/dev/null)
EOF
  fail "no codex session matched cwd: $cwd"
}

digest_claude() {
  local path="$1"
  OMS_TURNS="$TURNS" python3 - "$path" <<'PY'
import json, os, sys

path = sys.argv[1]
turns = int(os.environ.get("OMS_TURNS", "6"))
users, assistants, files = [], [], {}

def text_of(content):
    if isinstance(content, str):
        return content.strip()
    if isinstance(content, list):
        out = []
        for it in content:
            if isinstance(it, dict) and it.get("type") == "text" and it.get("text"):
                out.append(it["text"])
        return "\n".join(out).strip()
    return ""

import re

def is_noise(txt):
    # Slash-command wrappers, caveats, and auto-injected continuations are not
    # real user intent.
    if not txt:
        return True
    if txt.startswith("<local-command-caveat>") or txt.startswith("<command-"):
        return True
    if "<command-name>" in txt or "<command-message>" in txt:
        return True
    if txt in ("Continue from where you left off.",):
        return True
    return False

def clean(txt):
    # Drop system-reminder / caveat tag blocks that wrap real prose.
    txt = re.sub(r"<system-reminder>.*?</system-reminder>", " ", txt, flags=re.S)
    txt = re.sub(r"<local-command-[^>]*>.*?</local-command-[^>]*>", " ", txt, flags=re.S)
    return txt.strip()

with open(path, encoding="utf-8") as fh:
    for line in fh:
        try:
            obj = json.loads(line)
        except Exception:
            continue
        t = obj.get("type")
        msg = obj.get("message")
        if t == "user" and isinstance(msg, dict):
            content = msg.get("content")
            # Skip tool_result-only turns (not real user prose).
            if isinstance(content, list) and content and all(
                isinstance(i, dict) and i.get("type") == "tool_result" for i in content
            ):
                continue
            txt = text_of(content)
            if is_noise(txt):
                continue
            txt = clean(txt)
            if txt:
                users.append(txt)
        elif t == "assistant" and isinstance(msg, dict):
            content = msg.get("content")
            txt = text_of(content)
            if txt:
                assistants.append(txt)
            if isinstance(content, list):
                for it in content:
                    if isinstance(it, dict) and it.get("type") == "tool_use":
                        name = it.get("name", "")
                        if name in ("Edit", "Write", "MultiEdit", "NotebookEdit"):
                            fp = (it.get("input") or {}).get("file_path")
                            if fp:
                                files[fp] = files.get(fp, 0) + 1

def trim(s, n=1200):
    s = s.strip()
    return s if len(s) <= n else s[:n] + " …(truncated)"

print("GOAL\t" + (trim(users[0], 600) if users else "(no user message found)"))
print("USER_COUNT\t%d" % len(users))
for u in users[-turns:]:
    print("USER\t" + trim(u, 400).replace("\n", " "))
for fp, c in sorted(files.items(), key=lambda kv: -kv[1]):
    print("FILE\t%s\t%d" % (fp, c))
print("LAST_ASSISTANT\t" + (trim(assistants[-1]) if assistants else "(none)"))
PY
}

digest_codex() {
  local path="$1"
  OMS_TURNS="$TURNS" python3 - "$path" <<'PY'
import json, os, sys

path = sys.argv[1]
turns = int(os.environ.get("OMS_TURNS", "6"))
users, lasts, cwd = [], [], ""

with open(path, encoding="utf-8") as fh:
    for line in fh:
        try:
            obj = json.loads(line)
        except Exception:
            continue
        p = obj.get("payload", {})
        if not isinstance(p, dict):
            continue
        if obj.get("type") == "session_meta" and p.get("cwd"):
            cwd = p["cwd"]
        pt = p.get("type")
        if pt == "user_message" and p.get("message"):
            users.append(p["message"].strip())
        elif pt == "task_complete" and p.get("last_agent_message"):
            lasts.append(p["last_agent_message"].strip())

def trim(s, n=1200):
    s = s.strip()
    return s if len(s) <= n else s[:n] + " …(truncated)"

if cwd:
    print("CWD\t" + cwd)
print("GOAL\t" + (trim(users[0], 600) if users else "(no user message found)"))
print("USER_COUNT\t%d" % len(users))
for u in users[-turns:]:
    print("USER\t" + trim(u, 400).replace("\n", " "))
print("LAST_ASSISTANT\t" + (trim(lasts[-1]) if lasts else "(none)"))
PY
}

digest_antigravity() {
  local cwd="$1"
  local id="$2"
  local hist="$GEMINI_HOME/antigravity-cli/history.jsonl"
  [ -f "$hist" ] || fail "no antigravity history: $hist"
  OMS_TURNS="$TURNS" OMS_MATCH_CWD="$cwd" OMS_MATCH_ID="$id" python3 - "$hist" <<'PY'
import json, os, sys

path = sys.argv[1]
turns = int(os.environ.get("OMS_TURNS", "6"))
want_cwd = os.environ.get("OMS_MATCH_CWD", "")
want_id = os.environ.get("OMS_MATCH_ID", "")
rows = []
with open(path, encoding="utf-8") as fh:
    for line in fh:
        try:
            obj = json.loads(line)
        except Exception:
            continue
        if want_cwd and obj.get("workspace") != want_cwd:
            continue
        if want_id and obj.get("conversationId") != want_id:
            continue
        if obj.get("display"):
            rows.append(obj)

if not rows:
    print("GOAL\t(no antigravity prompts matched)")
    sys.exit(0)

# Default to the most recent conversation when no id is pinned.
if not want_id:
    last_conv = rows[-1].get("conversationId")
    rows = [r for r in rows if r.get("conversationId") == last_conv]

def trim(s, n=400):
    s = (s or "").strip()
    return s if len(s) <= n else s[:n] + " …(truncated)"

conv = rows[-1].get("conversationId", "")
if conv:
    print("CONVERSATION\t" + conv)
print("GOAL\t" + trim(rows[0]["display"], 600))
print("USER_COUNT\t%d" % len(rows))
for r in rows[-turns:]:
    print("USER\t" + trim(r["display"]).replace("\n", " "))
print("LAST_ASSISTANT\t(antigravity assistant output not available from history.jsonl)")
PY
}

render_digest() {
  # Read TAB-separated extractor lines on stdin, emit markdown.
  local agent="$1"
  local source="$2"
  local session_id="$3"
  local cwd="$4"
  local note="$5"
  local ts="$6"

  printf '# Session handoff: %s %s\n\n' "$agent" "$session_id"
  printf -- '- captured: %s\n' "$ts"
  printf -- '- agent: %s\n' "$agent"
  printf -- '- cwd: %s\n' "$cwd"
  printf -- '- source: %s\n' "$source"
  [ -n "$note" ] && printf -- '- note: %s\n' "$note"
  printf '\n'

  local goal="" last="" user_count=""
  local -a users=()
  local -a files=()
  local key rest
  while IFS=$'\t' read -r key rest; do
    case "$key" in
      GOAL) goal="$rest" ;;
      USER) users+=("$rest") ;;
      USER_COUNT) user_count="$rest" ;;
      FILE) files+=("$rest") ;;
      LAST_ASSISTANT) last="$rest" ;;
      CWD|CONVERSATION) ;; # informational; already have cwd
    esac
  done

  printf '## Goal (first user turn)\n\n%s\n\n' "$goal"
  printf '## Recent user turns'
  [ -n "$user_count" ] && printf ' (last %d of %s)' "${#users[@]}" "$user_count"
  printf '\n\n'
  if [ "${#users[@]}" -gt 0 ]; then
    local u
    for u in "${users[@]}"; do
      printf -- '- %s\n' "$u"
    done
  else
    printf '(none)\n'
  fi
  printf '\n'
  if [ "${#files[@]}" -gt 0 ]; then
    printf '## Files touched\n\n'
    local fl path count
    for fl in "${files[@]}"; do
      path="${fl%$'\t'*}"
      count="${fl##*$'\t'}"
      printf -- '- %s (%s edits)\n' "$path" "$count"
    done
    printf '\n'
  fi
  printf '## Last assistant summary\n\n%s\n' "$last"
}

cmd_capture() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --agent) [ "$#" -ge 2 ] || fail "--agent requires a value"; AGENT="$2"; shift 2 ;;
      --session) [ "$#" -ge 2 ] || fail "--session requires a value"; SESSION="$2"; shift 2 ;;
      --cwd) [ "$#" -ge 2 ] || fail "--cwd requires a path"; CWD="$2"; shift 2 ;;
      --note) [ "$#" -ge 2 ] || fail "--note requires text"; NOTE="$2"; shift 2 ;;
      --out) [ "$#" -ge 2 ] || fail "--out requires a path"; OUT="$2"; shift 2 ;;
      --allow-sensitive) ALLOW_SENSITIVE=1; shift ;;
      *) fail "unknown capture argument: $1" ;;
    esac
  done
  AGENT="${AGENT:-claude}"
  CWD="$(cd "$CWD" 2>/dev/null && pwd || printf '%s' "$CWD")"

  local source="" session_id="$SESSION" extract ts
  ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  case "$AGENT" in
    claude)
      source="$(resolve_claude_session "$CWD" "$SESSION")"
      session_id="$(basename "$source" .jsonl)"
      extract="$(digest_claude "$source")"
      ;;
    codex)
      source="$(resolve_codex_session "$CWD" "$SESSION")"
      session_id="${SESSION:-$(basename "$source" .jsonl)}"
      extract="$(digest_codex "$source")"
      ;;
    antigravity|agy)
      AGENT="antigravity"
      source="$GEMINI_HOME/antigravity-cli/history.jsonl"
      extract="$(digest_antigravity "$CWD" "$SESSION")"
      session_id="${SESSION:-latest}"
      ;;
    *)
      fail "unknown agent: $AGENT (use claude|codex|antigravity)"
      ;;
  esac

  local out="$OUT"
  if [ -z "$out" ]; then
    local dir="$ROOT/.oms/handoffs"
    agent_memory_ensure_oms_ignore_for_path "$dir" 2>/dev/null || true
    mkdir -p "$dir"
    local stamp
    stamp="$(printf '%s' "$ts" | tr -c 'A-Za-z0-9' '-')"
    out="$dir/$AGENT-$(slug_id "$session_id")-$stamp.md"
  else
    mkdir -p "$(dirname "$out")"
  fi

  printf '%s\n' "$extract" |
    render_digest "$AGENT" "$source" "$session_id" "$CWD" "$NOTE" "$ts" > "$out"

  if agent_memory_file_has_sensitive_content "$out"; then
    # The digest is meant to be loaded into another (possibly external) agent,
    # and transcripts carry pasted secrets. Block by default — mirror the
    # agent-memory append posture — unless explicitly overridden.
    if [ "${ALLOW_SENSITIVE:-0}" = 1 ]; then
      echo "warning: handoff digest looks sensitive; emitted under --allow-sensitive" >&2
    else
      rm -f "$out"
      fail "handoff digest looks sensitive; refusing to write. Re-run with --allow-sensitive to override."
    fi
  fi
  printf '%s\n' "$out"
}

slug_id() {
  printf '%s' "$1" | tr -c 'A-Za-z0-9' '-' | cut -c1-24 | sed 's/-*$//'
}

cmd_list() {
  local dir="$ROOT/.oms/handoffs"
  [ -d "$dir" ] || { echo "no handoffs captured"; return 0; }
  find "$dir" -maxdepth 1 -type f -name '*.md' -exec ls -1t {} + 2>/dev/null
}

cmd_show() {
  [ "$#" -eq 1 ] || fail "show requires exactly one file"
  local f="$1"
  [ -f "$f" ] || f="$ROOT/.oms/handoffs/$1"
  [ -f "$f" ] || fail "no such handoff: $1"
  cat "$f"
}

case "${1:-}" in
  capture) shift; cmd_capture "$@" ;;
  list) shift; [ "$#" -eq 0 ] || fail "list takes no arguments"; cmd_list ;;
  show) shift; cmd_show "$@" ;;
  -h|--help) usage ;;
  "") usage >&2; exit 2 ;;
  *) fail "unknown subcommand: $1" ;;
esac
