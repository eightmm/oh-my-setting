#!/usr/bin/env bash
set -euo pipefail

# One verb for consulting other agents mid-task. The pieces existed —
# agent-call for a read-only pass, agent-thread for continuity, harness context
# flags, provider routing — but using them together meant composing four tools
# by hand, so in practice an agent working on something just did not ask. This
# picks a peer that is not the caller, attaches the task/memory context and the
# running conversation, asks, records both turns, and prints the answer. With
# --all it asks every installed peer in parallel and keeps every answer in the
# same thread, so the next question starts from what all of them said.
#
# Read-only by design: it never delegates writes. Use peer-delegate (or
# agent-run --mode write) when the peer should produce a patch.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$SCRIPT_DIR/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/peer-common.sh
. "$SCRIPT_DIR/lib/peer-common.sh"

REPO="$PWD"
PROMPT=""
PROMPT_FILE=""
TO=""
ALL=0
THREAD_ID=""
NEW_THREAD=0
TOPIC=""
QUIET=0
MODEL_CLASS="auto"
PASSTHROUGH=()

usage() {
  cat <<'EOF'
Usage: agent-consult.sh (--prompt TEXT | --prompt-file PATH) [options]
       agent-consult.sh "question text" [options]

Ask another agent mid-task and keep the exchange. Picks a peer that is not the
caller, attaches the active task, shared memory, and the current conversation
thread, records question and answer, and prints the answer. Read-only: for a
write task use peer-delegate.sh.

Options:
  --prompt TEXT        The question. A bare argument works too.
  --prompt-file PATH   Read the question from a file.
  --repo PATH          Repo for context and state. Default: PWD.
  --to PROVIDER        Ask this provider instead of the automatic pick.
  --all                Ask every installed peer (not the caller) in parallel
                       and record all answers in the thread.
  --thread ID          Use this thread. Default: the current one, created on
                       first use so a series of consults stays one conversation.
  --new-thread         Start a fresh thread even if one is current.
  --topic TEXT         Topic for a newly created thread.
  --model-class CLASS  auto, fast, balanced, or deep. Default: auto
                       (balanced — a consult is judgement, not a lookup).
  --no-memory          Do not attach shared memory.
  --no-task            Do not attach the active task packet.
  --ml-context         Attach the ML digest as well.
  --quiet              Print only the thread id and artifact paths.
  --dry-run            Compose and record nothing; write prompt artifacts only.
  -h, --help           Show help.

Environment:
  OMS_CONSULT_PROVIDER   Preferred peer (overridden by --to).
  OMS_PEER_TIMEOUT=5m    Provider wall-clock timeout.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

# The artifact agent-call just wrote for this provider, newest first.
latest_artifact_for() {
  local provider="$1"
  ls -t "$REPO/.oms/artifacts/consult/$provider-"*.md 2>/dev/null | sed -n '1p'
}

INCLUDE_MEMORY=1
INCLUDE_TASK=1
DRY_RUN=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prompt) [ "$#" -ge 2 ] || fail "--prompt requires text"; PROMPT="$2"; shift 2 ;;
    --prompt-file) [ "$#" -ge 2 ] || fail "--prompt-file requires a path"; PROMPT_FILE="$2"; shift 2 ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --to) [ "$#" -ge 2 ] || fail "--to requires a provider"; TO="$2"; shift 2 ;;
    --all) ALL=1; shift ;;
    --thread) [ "$#" -ge 2 ] || fail "--thread requires an id"; THREAD_ID="$2"; shift 2 ;;
    --new-thread) NEW_THREAD=1; shift ;;
    --topic) [ "$#" -ge 2 ] || fail "--topic requires text"; TOPIC="$2"; shift 2 ;;
    --model-class) [ "$#" -ge 2 ] || fail "--model-class requires a value"; MODEL_CLASS="$2"; shift 2 ;;
    --no-memory) INCLUDE_MEMORY=0; shift ;;
    --no-task) INCLUDE_TASK=0; shift ;;
    --ml-context) PASSTHROUGH+=(--ml-context); shift ;;
    --quiet) QUIET=1; shift ;;
    --dry-run) DRY_RUN=1; PASSTHROUGH+=(--dry-run); shift ;;
    --model|--fallback-model|--reasoning-effort)
      [ "$#" -ge 2 ] || fail "$1 requires a value"; PASSTHROUGH+=("$1" "$2"); shift 2 ;;
    --no-model-fallback) PASSTHROUGH+=("$1"); shift ;;
    -h|--help) usage; exit 0 ;;
    *)
      [ -z "$PROMPT" ] || fail "unknown argument: $1"
      PROMPT="$1"; shift ;;
  esac
done

if [ -n "$PROMPT_FILE" ]; then
  [ -f "$PROMPT_FILE" ] || fail "prompt file not found: $PROMPT_FILE"
elif [ -z "$PROMPT" ]; then
  fail "a question is required (--prompt, --prompt-file, or a bare argument)"
fi
[ -d "$REPO" ] || fail "repo not found: $REPO"
REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
[ "$ALL" -eq 0 ] || [ -z "$TO" ] || fail "--all and --to are mutually exclusive"
case "$MODEL_CLASS" in
  auto) MODEL_CLASS=balanced ;;
  fast|balanced|deep) ;;
  *) fail "--model-class must be auto, fast, balanced, or deep" ;;
esac

provider_cli_available() {
  case "$1" in
    codex) command -v codex >/dev/null 2>&1 ;;
    claude) command -v claude >/dev/null 2>&1 ;;
    antigravity|agy) command -v agy >/dev/null 2>&1 ;;
    *) return 1 ;;
  esac
}

# Peers = installed providers that are not the caller, so a consult is a second
# opinion rather than the same model answering itself. Self-consult is the last
# resort: a fresh context still beats no answer at all.
peers() {
  local caller="${OMS_AGENT:-}" candidate found=0
  for candidate in claude codex antigravity; do
    [ "$candidate" = "$caller" ] && continue
    if provider_cli_available "$candidate"; then
      printf '%s\n' "$candidate"
      found=1
    fi
  done
  if [ "$found" -eq 0 ] && [ -n "$caller" ] && provider_cli_available "$caller"; then
    printf '%s\n' "$caller"
  fi
}

THREAD_SH="$SCRIPT_DIR/agent-thread.sh"

resolve_thread() {
  local id
  if [ -n "$THREAD_ID" ] && [ "$NEW_THREAD" -eq 0 ]; then
    # Create it on demand so --thread names a conversation, not a precondition.
    if ! bash "$THREAD_SH" --repo "$REPO" --id "$THREAD_ID" current >/dev/null 2>&1 &&
      [ ! -f "$REPO/.oms/threads/$THREAD_ID.jsonl" ]; then
      bash "$THREAD_SH" --repo "$REPO" --id "$THREAD_ID" ${TOPIC:+--topic "$TOPIC"} new >/dev/null
    fi
    printf '%s\n' "$THREAD_ID"
    return 0
  fi
  if [ "$NEW_THREAD" -eq 0 ] && id="$(bash "$THREAD_SH" --repo "$REPO" current 2>/dev/null)"; then
    printf '%s\n' "$id"
    return 0
  fi
  bash "$THREAD_SH" --repo "$REPO" ${TOPIC:+--topic "$TOPIC"} new
}

thread="$(resolve_thread)" || fail "could not open a thread"

call_one() {
  local provider="$1"
  local args

  args=("$SCRIPT_DIR/agent-call.sh" --to "$provider" --repo "$REPO"
        --artifact-dir "$REPO/.oms/artifacts/consult"
        --model-class "$MODEL_CLASS" --thread "$thread")
  [ "$INCLUDE_MEMORY" -eq 1 ] && args+=(--memory)
  [ "$INCLUDE_TASK" -eq 1 ] && args+=(--task)
  if [ -n "$PROMPT_FILE" ]; then
    args+=(--prompt-file "$PROMPT_FILE")
  else
    args+=(--prompt "$PROMPT")
  fi
  args+=(${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"})
  bash "${args[@]}"
}

# One consult should not die because the first CLI is broken, unauthenticated,
# or answers nothing. Only for an automatic pick: a pinned --to is the caller's
# explicit choice, and a scrubber block (exit 3) is a problem with the prompt,
# not the provider, so neither is retried. Read-only only — never for writes.
consult_with_failover() {
  local first="$1"
  local rc=0 quality next artifact
  shift

  call_one "$first" || rc=$?
  artifact="$(latest_artifact_for "$first")"
  quality="ok"
  [ -z "$artifact" ] || quality="$(ma_answer_quality "$artifact")"
  if [ "$rc" -eq 0 ] && [ "$quality" = "ok" ]; then
    return 0
  fi
  if [ "$rc" -eq 3 ]; then
    echo "consult: $first blocked by the outbound gate; not retrying another peer" >&2
    return "$rc"
  fi
  next=""
  for candidate in "$@"; do
    [ "$candidate" = "$first" ] && continue
    next="$candidate"
    break
  done
  if [ -z "$next" ]; then
    [ "$quality" = "ok" ] ||
      echo "consult: $first did not really answer ($quality) and no other peer is installed" >&2
    return "$rc"
  fi
  if [ "$rc" -ne 0 ]; then
    echo "consult: $first failed (exit $rc); asking $next instead" >&2
  else
    echo "consult: $first did not really answer ($quality); asking $next instead" >&2
  fi
  # The question is already in the thread from the first attempt; a failover
  # must not duplicate it.
  export OMS_THREAD_QUESTION_RECORDED=1
  rc=0
  call_one "$next" || rc=$?
  artifact="$(latest_artifact_for "$next")"
  if [ "$rc" -eq 0 ] && [ -n "$artifact" ]; then
    quality="$(ma_answer_quality "$artifact")"
    [ "$quality" = "ok" ] ||
      echo "consult: $next also did not really answer ($quality)" >&2
  fi
  return "$rc"
}

targets=()
if [ -n "$TO" ]; then
  targets=("$TO")
elif [ "$ALL" -eq 1 ]; then
  while IFS= read -r p; do [ -z "$p" ] || targets+=("$p"); done <<EOF
$(peers)
EOF
else
  first="$(peers | sed -n '1p')"
  [ -n "$first" ] || fail "no peer agent CLI found (codex, claude, agy)"
  targets=("$first")
fi
[ "${#targets[@]}" -gt 0 ] || fail "no peer agent CLI found (codex, claude, agy)"

# One consult per peer, in parallel: waiting for three serial calls is the
# other reason mid-task consultation does not happen.
status=0
usable=0
if [ "${#targets[@]}" -gt 1 ]; then
  question_turn="$(agent_memory_mktemp)" || question_turn=""
  if [ -n "$question_turn" ]; then
    if [ -n "$PROMPT_FILE" ]; then cat "$PROMPT_FILE" > "$question_turn"
    else printf '%s\n' "$PROMPT" > "$question_turn"; fi
    bash "$THREAD_SH" --repo "$REPO" --id "$thread" append --role question \
      --text-file "$question_turn" >/dev/null 2>&1 &&
      export OMS_THREAD_QUESTION_RECORDED=1
    rm -f "$question_turn"
  fi
fi
if [ "${#targets[@]}" -eq 1 ]; then
  if [ -n "$TO" ]; then
    call_one "${targets[0]}" || status=$?
  else
    # Auto-picked: fall back to one other peer if this one cannot answer.
    peer_list=()
    while IFS= read -r peer_name; do
      [ -z "$peer_name" ] || peer_list+=("$peer_name")
    done <<EOF
$(peers)
EOF
    consult_with_failover "${targets[0]}" ${peer_list[@]+"${peer_list[@]}"} || status=$?
  fi
else
  pids=()
  logs=()
  tmpdir="$(mktemp -d)" || fail "mktemp failed"
  trap 'rm -rf "$tmpdir"' EXIT
  for p in "${targets[@]}"; do
    call_one "$p" > "$tmpdir/$p.log" 2>&1 &
    pids+=("$!")
    logs+=("$tmpdir/$p.log")
  done
  i=0
  for pid in "${pids[@]}"; do
    wait "$pid" || status=1
    [ "$QUIET" -eq 1 ] || cat "${logs[i]}"
    i=$((i + 1))
  done
  # A council is only as good as the answers in it: say how many peers actually
  # answered, so a non-answer is not silently counted as agreement.
  for p in "${targets[@]}"; do
    art="$(latest_artifact_for "$p")"
    [ -n "$art" ] || continue
    q="$(ma_answer_quality "$art")"
    if [ "$q" = "ok" ]; then
      usable=$((usable + 1))
    else
      echo "consult: $p did not really answer ($q)" >&2
    fi
  done
  echo "consult: ${usable}/${#targets[@]} peer(s) answered"
fi

echo "thread: $thread"
if [ "$QUIET" -eq 0 ] && [ "$DRY_RUN" -eq 0 ]; then
  echo "---"
  bash "$THREAD_SH" --repo "$REPO" --id "$thread" --turns 4 context || true
fi
exit "$status"
