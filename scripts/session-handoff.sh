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
# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT/scripts/lib/work-journal.sh"

CLAUDE_HOME="${OMS_CLAUDE_HOME:-$HOME/.claude}"
CODEX_HOME="${OMS_CODEX_HOME:-$HOME/.codex}"
GEMINI_HOME="${OMS_GEMINI_HOME:-$HOME/.gemini}"

AGENT=""
SESSION=""
CWD="$PWD"
OUT=""
NOTE=""
TURNS="${OMS_HANDOFF_TURNS:-6}"
# A session with fewer real user turns than this has nothing to hand off, and
# capturing it would only push a substantive digest out of the resume hook's
# newest-handoff slot. The automatic hooks inherit this default; a deliberate
# manual capture can lower it.
MIN_USER_TURNS=2

# What to do when the named session has no transcript on disk at all. The
# automatic hook passes skip: worker-style session ids fire SessionEnd
# without ever writing a transcript, so capture can never succeed, retrying
# is meaningless, and a ledger row per firing is pure noise. Everything that
# is a real capture failure — sensitive refusal, parse errors, unreadable
# files — is unaffected by this policy and still fails.
MISSING_POLICY=fail

usage() {
  cat <<'EOF'
Usage: session-handoff.sh <capture|list|show> [options]

Distill an agent CLI session into a portable handoff digest.

Subcommands:
  capture   Read a session and write a digest to the project's .oms/handoffs/
            (the repo containing --cwd; the Work Journal digest points there).
  list      List captured handoff digests (newest first); --json for machines.
  show FILE Print a captured digest to stdout.

capture options:
  --agent NAME     claude | codex | antigravity (default: claude).
  --session ID     Session id / file. Default: most recent for --cwd.
  --cwd PATH       Project dir to match (default: current dir).
  --note TEXT      Free-text note added to the digest header.
  --out FILE       Write digest here instead of the default handoffs dir.
  --min-user-turns N  Skip the capture when the session has fewer than N real
                   user turns (default: 2; 0 captures anything).
  --missing-session-policy skip|fail
                   What to do when the named session has no transcript on
                   disk at all (default: fail, exit 2). The automatic hook
                   passes skip: worker-style session ids fire hooks without
                   ever writing a transcript, so the capture becomes a quiet
                   no-op instead of a daily ledger row. Real capture
                   failures (sensitive refusal, parse errors) always fail.
  --allow-sensitive  Write the digest even if it looks sensitive (default:
                   refuse, since the digest is meant for another agent).

Notes:
  - Extraction is mechanical; no model is called.
  - A session below --min-user-turns is skipped, not failed: it writes no
    digest, says so on stderr, and exits 0, so the automatic hooks stay quiet
    instead of reporting a failure for every trivial session.
  - antigravity stores only user prompts in history.jsonl (assistant output
    lives in opaque protobuf), so its digest is prompts-only (best-effort).
  - Digests are local artifacts; loading one into another agent is your step.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

# Absence is its own exit (3), distinct from failure (2), so cmd_capture can
# apply --missing-session-policy to "the transcript simply is not there"
# without ever softening a real failure.
absent() {
  echo "error: $*" >&2
  exit 3
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
    [ -f "$path" ] || absent "claude session not found: $path"
    printf '%s\n' "$path"
    return 0
  fi
  [ -d "$dir" ] || absent "no claude sessions for cwd: $cwd ($dir)"
  newest_file "$dir"/*.jsonl
}

resolve_codex_session() {
  local cwd="$1"
  local id="$2"
  if [ -n "$id" ]; then
    local hit
    hit="$(find "$CODEX_HOME/sessions" "$CODEX_HOME/archived_sessions" \
      -type f -name "*$id*.jsonl" 2>/dev/null | head -n 1)"
    [ -n "$hit" ] || absent "codex session not found for id: $id"
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
  absent "no codex session matched cwd: $cwd"
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
  [ -f "$hist" ] || absent "no antigravity history: $hist"
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

# A digest is what the next session resumes from, and a checkpoint is state
# PLUS its verifier: the active packet's Verify command rides along so the
# resumed session inherits a machine-checkable gate, and a review round that
# ended split rides along as open dissents the next session must acknowledge
# (agree, override with reasons, or escalate) instead of silently re-deriving
# consensus. Mechanical and fail-open, like the rest of capture.
append_resume_contract() {
  local repo="$1"
  local out="$2"
  local verify_cmd=""
  local dissent_seats=""
  local task_file="$repo/.oms/task/current.md"

  if [ -s "$task_file" ]; then
    verify_cmd="$(awk '
      $0 == "## Verify" { inside = 1; next }
      inside && /^## / { exit }
      inside && NF { print }
    ' "$task_file" 2>/dev/null || true)"
  fi
  # The gate records its seats on the review-outcome row; consume that typed
  # object. Rendered review prose is never machine state here. An open
  # dissent is the latest GENUINE split (exit and overall both 1 — an
  # incomplete round is not a disagreement) whose event no
  # artifact-resolution receipt has retired; a later non-split gate must not
  # bury a split nobody acknowledged, and a resolved one must stop nagging.
  # A failed probe must stay distinguishable from "no dissent": absence of
  # the section is read downstream as consensus, which is exactly the burial
  # this function exists to prevent. Probe failures print a sentinel and the
  # digest says UNAVAILABLE instead of saying nothing.
  local review_json="" dissent_probe_failed=0
  review_json="$(python3 - "$repo" <<'PY' 2>/dev/null || printf '__OMS_DISSENT_PROBE_FAILED__'
import json, os, sys

index = os.path.join(sys.argv[1], ".oms", "artifacts", "index.jsonl")
resolved = set()
rows = []
try:
    with open(index, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if not isinstance(row, dict):
                continue
            if row.get("kind") == "artifact-resolution":
                target = row.get("resolves_event_id")
                if isinstance(target, str) and target:
                    resolved.add(target)
            elif row.get("kind") == "review-outcome" and isinstance(row.get("review"), dict):
                rows.append(row)
except OSError:
    raise SystemExit(0)

best = None
for row in rows:
    review = row["review"]
    seats = [s for s in review.get("seats", []) if isinstance(s, dict)]
    verdicts = [s.get("verdict") for s in seats]
    if "pass" not in verdicts or "fail" not in verdicts:
        continue
    if row.get("exit") != 1 or review.get("overall") != 1:
        continue
    event_id = row.get("event_id")
    if isinstance(event_id, str) and event_id in resolved:
        continue
    best = {"overall": 1, "seats": seats,
            "event_id": event_id if isinstance(event_id, str) else ""}
if best is not None:
    print(json.dumps(best, ensure_ascii=False, separators=(",", ":")))
PY
)"
  case "$review_json" in
    *__OMS_DISSENT_PROBE_FAILED__*) dissent_probe_failed=1; review_json="" ;;
  esac
  if [ -z "$review_json" ] && [ "$dissent_probe_failed" -eq 0 ] &&
    [ -d "$repo/.oms/artifacts/review" ]; then
    # Artifacts that predate the typed outcome row are stored user state;
    # parse them through the one parser peer-review owns, still consuming
    # JSON rather than grepping rendered lines.
    review_json="$("$ROOT/scripts/peer-review.sh" verdicts --json \
      "$repo/.oms/artifacts/review" 2>/dev/null || printf '__OMS_DISSENT_PROBE_FAILED__')"
    case "$review_json" in
      *__OMS_DISSENT_PROBE_FAILED__*) dissent_probe_failed=1; review_json="" ;;
    esac
  fi
  [ -z "$review_json" ] ||
    dissent_seats="$(OMS_HANDOFF_VERDICTS_JSON="$review_json" python3 - <<'PY' 2>/dev/null || printf '__OMS_DISSENT_PROBE_FAILED__'
import json, os

try:
    data = json.loads(os.environ.get("OMS_HANDOFF_VERDICTS_JSON", ""))
except ValueError:
    raise SystemExit(0)
if not isinstance(data, dict):
    raise SystemExit(0)
seats = [s for s in data.get("seats", []) if isinstance(s, dict)]
verdicts = [s.get("verdict") for s in seats]
if data.get("overall") != 1:
    raise SystemExit(0)
if "pass" in verdicts and "fail" in verdicts:
    for seat in seats:
        line = "%s: %s" % (seat.get("provider", "?"), seat.get("verdict", "?"))
        confidence = seat.get("confidence")
        if isinstance(confidence, (int, float)):
            line += " (confidence %s)" % confidence
        print(line)
    event_id = data.get("event_id")
    if isinstance(event_id, str) and event_id:
        print("event: %s" % event_id)
        print('resolve: oms artifact-index resolve --event-id %s --reason "<how it was settled>"'
              % event_id)
PY
)"
  {
    if [ -n "$verify_cmd" ]; then
      printf '\n## Resume contract\n\n'
      printf 'Run before building on this handoff:\n\n'
      printf '```bash\n%s\n```\n' "$verify_cmd"
    fi
    case "$dissent_seats" in
      *__OMS_DISSENT_PROBE_FAILED__*) dissent_probe_failed=1; dissent_seats="" ;;
    esac
    if [ -n "$dissent_seats" ]; then
      printf '\n## Open dissents\n\n'
      printf 'The last review round ended split. Acknowledge each verdict —\n'
      printf 'agree, override with reasons, or escalate — before landing\n'
      printf 'related work; do not silently re-derive consensus.\n\n'
      printf '%s\n' "$dissent_seats"
    elif [ "$dissent_probe_failed" -eq 1 ]; then
      printf '\n## Open dissents\n\n'
      printf 'UNAVAILABLE: the dissent projection failed while capturing this\n'
      printf 'handoff. Do not read absence as consensus; inspect with:\n'
      printf '`oms peer-review verdicts --json .oms/artifacts/review`\n'
    fi
  } >> "$out" || echo "warning: resume contract/dissent section could not be appended to $out" >&2
}

cmd_capture() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --agent) [ "$#" -ge 2 ] || fail "--agent requires a value"; AGENT="$2"; shift 2 ;;
      --session) [ "$#" -ge 2 ] || fail "--session requires a value"; SESSION="$2"; shift 2 ;;
      --cwd) [ "$#" -ge 2 ] || fail "--cwd requires a path"; CWD="$2"; shift 2 ;;
      --note) [ "$#" -ge 2 ] || fail "--note requires text"; NOTE="$2"; shift 2 ;;
      --out) [ "$#" -ge 2 ] || fail "--out requires a path"; OUT="$2"; shift 2 ;;
      --min-user-turns)
        [ "$#" -ge 2 ] || fail "--min-user-turns requires a count"
        case "$2" in
          ''|*[!0-9]*) fail "--min-user-turns requires a non-negative integer: $2" ;;
        esac
        MIN_USER_TURNS="$2"; shift 2
        ;;
      --allow-sensitive) ALLOW_SENSITIVE=1; shift ;;
      --missing-session-policy)
        [ "$#" -ge 2 ] || fail "--missing-session-policy requires skip or fail"
        case "$2" in
          skip|fail) MISSING_POLICY="$2" ;;
          *) fail "--missing-session-policy must be skip or fail: $2" ;;
        esac
        shift 2
        ;;
      *) fail "unknown capture argument: $1" ;;
    esac
  done
  AGENT="${AGENT:-claude}"
  CWD="$(cd "$CWD" 2>/dev/null && pwd || printf '%s' "$CWD")"

  # Resolver exit 3 means the session transcript is absent. Under skip the
  # capture becomes a quiet no-op (exit 0, note, no digest, no ledger row);
  # under fail (manual callers) it stays the hard error it always was.
  session_absent_check() {
    [ "$1" -ne 0 ] || return 0
    if [ "$1" -eq 3 ] && [ "$MISSING_POLICY" = skip ]; then
      echo "session-handoff: skipped $AGENT capture: session transcript absent (--missing-session-policy skip)" >&2
      exit 0
    fi
    exit 2
  }

  local source="" session_id="$SESSION" extract ts handoff_hash rc
  ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  case "$AGENT" in
    claude)
      rc=0
      source="$(resolve_claude_session "$CWD" "$SESSION")" || rc=$?
      session_absent_check "$rc"
      session_id="$(basename "$source" .jsonl)"
      extract="$(digest_claude "$source")"
      ;;
    codex)
      rc=0
      source="$(resolve_codex_session "$CWD" "$SESSION")" || rc=$?
      session_absent_check "$rc"
      session_id="${SESSION:-$(basename "$source" .jsonl)}"
      extract="$(digest_codex "$source")"
      ;;
    antigravity|agy)
      AGENT="antigravity"
      source="$GEMINI_HOME/antigravity-cli/history.jsonl"
      rc=0
      extract="$(digest_antigravity "$CWD" "$SESSION")" || rc=$?
      session_absent_check "$rc"
      session_id="${SESSION:-latest}"
      ;;
    *)
      fail "unknown agent: $AGENT (use claude|codex|antigravity)"
      ;;
  esac

  local out="$OUT"
  if [ -z "$out" ]; then
    # Digests are project artifacts: the Work Journal's newest-handoff pointer
    # scans the project's .oms/handoffs, so writing anywhere else (e.g. the
    # harness checkout) makes every capture invisible to the daily digest.
    local repo_root
    repo_root="$(oms_repo_root "$CWD" 2>/dev/null || true)"
    if [ -z "$repo_root" ] || [ ! -d "$repo_root" ]; then
      fail "cannot resolve a state repo for --cwd: $CWD (pass --out)"
    fi
    local dir="$repo_root/.oms/handoffs"
    agent_memory_ensure_oms_ignore_for_path "$dir" 2>/dev/null || true
    mkdir -p "$dir"
    local stamp
    stamp="$(printf '%s' "$ts" | tr -c 'A-Za-z0-9' '-')"
    out="$dir/$AGENT-$(slug_id "$session_id")-$stamp.md"
  else
    mkdir -p "$(dirname "$out")"
  fi

  # Decide sensitivity on the USER CONTENT (transcript turns + note) — not the
  # rendered digest, whose header carries our own machine paths (cwd/source)
  # and would otherwise self-block every real capture. Block by default since
  # the digest is meant to be loaded into another, possibly external, agent.
  SCAN_FILE="$(mktemp)" || fail "mktemp failed"
  { printf '%s\n' "$extract"; printf '%s\n' "$NOTE"; } > "$SCAN_FILE"
  if agent_memory_file_has_secret_content "$SCAN_FILE"; then
    rm -f "$SCAN_FILE"; SCAN_FILE=""
    if [ "${ALLOW_SENSITIVE:-0}" = 1 ]; then
      echo "warning: handoff digest looks sensitive; emitting under --allow-sensitive" >&2
    else
      fail "session content looks sensitive; refusing to write the digest. Re-run with --allow-sensitive to override."
    fi
  elif agent_memory_file_has_sensitive_content "$SCAN_FILE"; then
    # Machine tier only (home paths, cluster fields): every real Linux session
    # trips it, and a digest that cannot cite a path is no digest at all. The
    # file is repo-local and git-ignored, so normalize the paths and write.
    # Anything leaving toward a peer re-scans with the strict tier regardless.
    rm -f "$SCAN_FILE"; SCAN_FILE=""
    extract="$(printf '%s\n' "$extract" | agent_memory_normalize_machine_paths)"
    NOTE="$(printf '%s' "$NOTE" | agent_memory_normalize_machine_paths)"
  else
    rm -f "$SCAN_FILE"; SCAN_FILE=""
  fi

  # Trivial sessions (one prompt, then /clear) would otherwise take over the
  # resume hook's newest-handoff slot from a digest that says something. The
  # floor sits after the sensitivity scan on purpose: a secret-bearing
  # transcript stays a loud refusal instead of becoming a silent skip. Skipping
  # exits 0 rather than failing, so the automatic hook path does not file a
  # fail-ledger row for every one-prompt session.
  local user_count
  user_count="$(printf '%s\n' "$extract" |
    awk -F'\t' '$1 == "USER_COUNT" { print $2; exit }')"
  case "$user_count" in
    ''|*[!0-9]*) user_count=0 ;;
  esac
  if [ "$user_count" -lt "$MIN_USER_TURNS" ]; then
    echo "session-handoff: skipped $AGENT $session_id: $user_count user turn(s), below the --min-user-turns floor of $MIN_USER_TURNS (pass --min-user-turns 0 to capture anyway)" >&2
    return 0
  fi

  printf '%s\n' "$extract" |
    render_digest "$AGENT" "$source" "$session_id" "$CWD" "$NOTE" "$ts" > "$out"
  append_resume_contract "$CWD" "$out" || true
  handoff_hash="$(
    printf '%s\n%s\n' "$extract" "$NOTE" |
      oms_sha256_stream 2>/dev/null || printf 'unhashed'
  )"
  work_journal_observe "$CWD" session-handoff "$out" \
    --source-id "$AGENT:$session_id:$handoff_hash" --occurred-at "$ts"
  printf '%s\n' "$out"
}

slug_id() {
  printf '%s' "$1" | tr -c 'A-Za-z0-9' '-' | cut -c1-24 | sed 's/-*$//'
}

cmd_list() {
  local as_json=0
  if [ "${1:-}" = "--json" ]; then as_json=1; shift; fi
  [ "$#" -eq 0 ] || fail "list takes no arguments"
  local dir
  dir="$(oms_repo_root "${OMS_STATE_REPO:-$PWD}")/.oms/handoffs"
  if [ ! -d "$dir" ]; then
    if [ "$as_json" -eq 1 ]; then
      echo '{"schema": 1, "handoffs": []}'
    else
      echo "no handoffs captured"
    fi
    return 0
  fi
  if [ "$as_json" -eq 1 ]; then
    OMS_SH_DIR="$dir" python3 -c '
import glob, json, os
rows = []
for path in sorted(glob.glob(os.path.join(os.environ["OMS_SH_DIR"], "*.md")),
                   key=os.path.getmtime, reverse=True):
    rows.append({"path": path, "bytes": os.path.getsize(path),
                 "mtime": int(os.path.getmtime(path))})
print(json.dumps({"schema": 1, "handoffs": rows}, ensure_ascii=False))
'
    return 0
  fi
  find "$dir" -maxdepth 1 -type f -name '*.md' -exec ls -1t {} + 2>/dev/null
}

cmd_show() {
  [ "$#" -eq 1 ] || fail "show requires exactly one file"
  local f="$1"
  [ -f "$f" ] ||
    f="$(oms_repo_root "${OMS_STATE_REPO:-$PWD}")/.oms/handoffs/$1"
  [ -f "$f" ] || fail "no such handoff: $1"
  cat "$f"
}

case "${1:-}" in
  capture) shift; cmd_capture "$@" ;;
  list) shift; cmd_list "$@" ;;
  show) shift; cmd_show "$@" ;;
  -h|--help) usage ;;
  "") usage >&2; exit 2 ;;
  *) fail "unknown subcommand: $1" ;;
esac
