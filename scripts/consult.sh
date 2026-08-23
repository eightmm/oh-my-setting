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
# Targets may name an exact model and may repeat a provider, so a panel can be
# several models rather than one per CLI: --to codex:model=a --to
# codex:model=b asks the same CLI twice and records the answers separately.
# Agreement is then reported by model family, because two answers from one
# family are one opinion twice, not two independent opinions.
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
TARGETS_EXPLICIT=()
ALL=0
THREAD_ID=""
NEW_THREAD=0
TOPIC=""
QUIET=0
PASSTHROUGH=()
EXPLICIT_MODEL_OPTION=0

usage() {
  cat <<'EOF'
Usage: consult.sh (--prompt TEXT | --prompt-file PATH) [options]
       consult.sh "question text" [options]

Ask another agent mid-task and keep the exchange. Picks a peer that is not the
caller, attaches the active task and the current conversation thread, records
question and answer, and prints the answer. Shared memory (prior conclusions)
is opt-in via --memory so the second opinion is formed independently.
Read-only: for a write task use peer-delegate.sh.

Options:
  --prompt TEXT        The question. A bare argument works too.
  --prompt-file PATH   Read the question from a file.
  --repo PATH          Repo for context and state. Default: PWD.
  --to TARGET          Ask this target instead of the automatic pick. Repeatable.
                       TARGET is PROVIDER or PROVIDER:model=NAME for an exact
                       model. Repeating a provider with different models is
                       allowed and each answer is recorded separately.
  --all                Ask every installed peer (not the caller) in parallel
                       and record all answers in the thread.
  --model MODEL        Exact model; requires one plain, explicit --to PROVIDER.
  --fallback-model M   One-shot capacity fallback; same target requirement.
  --reasoning-effort E auto, low, medium, high, xhigh, max, or ultra.
  --thread ID          Use this thread. Default: the current one, created on
                       first use so a series of consults stays one conversation.
  --new-thread         Start a fresh thread even if one is current.
  --topic TEXT         Topic for a newly created thread.
  --memory             Attach shared harness memory (prior conclusions).
  --no-memory          Disable --memory (compatibility).
  --no-task            Do not attach the active task packet.
  --ml-context         Attach the ML digest as well.
  --quiet              Print only the thread id and artifact paths.
  --dry-run            Compose and record nothing; write prompt artifacts only.
  -h, --help           Show help.

Environment:
  OMS_CONSULT_PROVIDER   Preferred peer (overridden by --to).
  OMS_PEER_TIMEOUT       Provider wall-clock timeout (default: 20m; consult
                         rides the call verb).
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

# The artifact agent-call just wrote for this provider, newest first.
latest_artifact_for() {
  local provider="$1"
  ls -t "$REPO/.oms/artifacts/consult/$provider-"*.md 2>/dev/null | sed -n '1p'
}

# Shared memory holds prior conclusions (closed tasks, distilled decisions):
# handing them to a consulted peer anchors the second opinion on the first
# one. Opt-in via --memory, matching peer-ask/peer-review/agent-call. The task
# packet stays on: it frames what is being worked on, not what was concluded.
INCLUDE_MEMORY=0
INCLUDE_TASK=1
DRY_RUN=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prompt) [ "$#" -ge 2 ] || fail "--prompt requires text"; PROMPT="$2"; shift 2 ;;
    --prompt-file) [ "$#" -ge 2 ] || fail "--prompt-file requires a path"; PROMPT_FILE="$2"; shift 2 ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --to)
      [ "$#" -ge 2 ] || fail "--to requires a target"
      TARGETS_EXPLICIT+=("$2")
      shift 2 ;;
    --all) ALL=1; shift ;;
    --thread) [ "$#" -ge 2 ] || fail "--thread requires an id"; THREAD_ID="$2"; shift 2 ;;
    --new-thread) NEW_THREAD=1; shift ;;
    --topic) [ "$#" -ge 2 ] || fail "--topic requires text"; TOPIC="$2"; shift 2 ;;
    --memory) INCLUDE_MEMORY=1; shift ;;
    --no-memory) INCLUDE_MEMORY=0; shift ;;
    --no-task) INCLUDE_TASK=0; shift ;;
    --ml-context) PASSTHROUGH+=(--ml-context); shift ;;
    --quiet) QUIET=1; shift ;;
    --dry-run) DRY_RUN=1; PASSTHROUGH+=(--dry-run); shift ;;
    --model|--fallback-model)
      [ "$#" -ge 2 ] || fail "$1 requires a value"
      EXPLICIT_MODEL_OPTION=1
      PASSTHROUGH+=("$1" "$2")
      shift 2 ;;
    --reasoning-effort)
      [ "$#" -ge 2 ] || fail "$1 requires a value"; PASSTHROUGH+=("$1" "$2"); shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *)
      case "$1" in
        -*) fail "unknown option: $1" ;;
      esac
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
[ "$ALL" -eq 0 ] || [ "${#TARGETS_EXPLICIT[@]}" -eq 0 ] ||
  fail "--all and --to are mutually exclusive"
if [ "$EXPLICIT_MODEL_OPTION" -eq 1 ]; then
  if [ "$ALL" -ne 0 ] || [ "${#TARGETS_EXPLICIT[@]}" -ne 1 ]; then
    fail "--model/--fallback-model requires exactly one explicit --to PROVIDER"
  fi
  case "${TARGETS_EXPLICIT[0]}" in
    *:model=*) fail "--model/--fallback-model conflicts with PROVIDER:model=NAME" ;;
  esac
fi

oms_require_peer_owner || exit $?

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
  # Same exclusion contract as advise.sh: detected caller, not OMS_AGENT
  # alone, or a claude session's "second opinion" panel includes claude.
  local caller candidate found=0
  caller="$(oms_peer_caller)"
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

# A target is provider[:class] or provider[:model=NAME]. Splitting it here
# keeps every downstream step (call, artifact, thread turn, family accounting)
# talking about the same resolved pair.
target_provider() { printf '%s\n' "${1%%:*}"; }

target_model() {
  local spec="${1#*:}"
  [ "$spec" != "$1" ] || { printf '\n'; return 0; }
  case "$spec" in
    model=*) printf '%s\n' "${spec#model=}" ;;
    *) printf '\n' ;;
  esac
}

validate_target() {
  local spec="${1#*:}"
  local provider
  provider="$(target_provider "$1")"
  case "$provider" in
    codex|claude|antigravity|agy) ;;
    *) fail "unknown provider in target: $1" ;;
  esac
  [ "$spec" = "$1" ] && return 0
  case "$spec" in
    model=?*) ;;
    *) fail "target must be PROVIDER or PROVIDER:model=NAME: $1" ;;
  esac
}

THREAD_SH="$SCRIPT_DIR/thread.sh"

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

# Runs one target and writes the artifact path it reported to $2, so a panel
# that asks one provider twice still tracks each answer exactly instead of
# guessing from the newest file on disk.
call_one() {
  local target="$1"
  local artifact_out="${2:-}"
  local provider model args rc=0 log

  provider="$(target_provider "$target")"
  model="$(target_model "$target")"

  args=("$SCRIPT_DIR/agent-call.sh" --to "$provider" --repo "$REPO"
        --artifact-dir "$REPO/.oms/artifacts/consult"
        --thread "$thread")
  [ -z "$model" ] || args+=(--model "$model")
  [ "$INCLUDE_MEMORY" -eq 1 ] && args+=(--memory)
  [ "$INCLUDE_TASK" -eq 1 ] && args+=(--task)
  if [ -n "$PROMPT_FILE" ]; then
    args+=(--prompt-file "$PROMPT_FILE")
  else
    args+=(--prompt "$PROMPT")
  fi
  args+=(${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"})

  log="$(agent_memory_mktemp)" || log=""
  if [ -z "$log" ]; then
    bash "${args[@]}"
    return $?
  fi
  bash "${args[@]}" > "$log" 2>&1 || rc=$?
  cat "$log"
  if [ -n "$artifact_out" ]; then
    sed -n 's/^artifact: //p' "$log" | sed -n '1p' > "$artifact_out"
  fi
  rm -f "$log"
  return "$rc"
}

# One consult should not die because the first CLI is broken, unauthenticated,
# or answers nothing. Only for an automatic pick: a pinned --to is the caller's
# explicit choice, and a scrubber block (exit 3) is a problem with the prompt,
# not the provider, so neither is retried. Read-only only — never for writes.
consult_with_failover() {
  local first="$1"
  local rc=0 quality next artifact out
  shift

  # Judge the exact artifact this call reported, never the newest file on
  # disk: detached runs (the MCP server backgrounds consults) can overlap on
  # one provider, and mtime would quality-score the other run's in-flight
  # answer. The side file lives in artifact_dir_tmp, which the outer trap owns.
  out="$artifact_dir_tmp/failover"
  call_one "$first" ${out:+"$out"} || rc=$?
  artifact=""
  [ -z "$out" ] || artifact="$(sed -n 1p "$out" 2>/dev/null || true)"
  [ -n "$artifact" ] && [ -f "$artifact" ] || artifact="$(latest_artifact_for "$first")"
  quality="ok"
  [ -z "$artifact" ] || quality="$(ma_answer_quality "$artifact")"
  if [ "$rc" -eq 0 ] && [ "$quality" = "ok" ]; then
    return 0
  fi
  if [ "$rc" -eq 3 ]; then
    echo "consult: $first blocked by the outbound gate; not retrying another peer" >&2
    return "$rc"
  fi
  if [ "$rc" -eq 4 ]; then
    echo "consult: $first declined the request by policy; not retrying another peer" >&2
    return "$rc"
  fi
  next=""
  for candidate in "$@"; do
    [ "$candidate" = "$first" ] && continue
    next="$candidate"
    break
  done
  if [ -z "$next" ]; then
    if [ "$quality" != "ok" ]; then
      echo "consult: $first did not really answer ($quality) and no other peer is installed" >&2
      [ "$quality" != blocked ] ||
        echo "consult: $first said: $(ma_answer_block_reason "$artifact")" >&2
    fi
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
  : > "$out"
  call_one "$next" "$out" || rc=$?
  artifact="$(sed -n 1p "$out" 2>/dev/null || true)"
  [ -n "$artifact" ] && [ -f "$artifact" ] || artifact="$(latest_artifact_for "$next")"
  if [ "$rc" -eq 0 ] && [ -n "$artifact" ]; then
    quality="$(ma_answer_quality "$artifact")"
    [ "$quality" = "ok" ] ||
      echo "consult: $next also did not really answer ($quality)" >&2
  fi
  return "$rc"
}

targets=()
if [ "${#TARGETS_EXPLICIT[@]}" -gt 0 ]; then
  for spec in "${TARGETS_EXPLICIT[@]}"; do
    validate_target "$spec"
    targets+=("$spec")
  done
elif [ "$ALL" -eq 1 ]; then
  while IFS= read -r p; do
    [ -n "$p" ] || continue
    targets+=("$p")
  done <<EOF
$(peers)
EOF
else
  first="$(peers | sed -n '1p')"
  [ -n "$first" ] || fail "no peer agent CLI found (codex, claude, agy)"
  targets=("$first")
fi
[ "${#targets[@]}" -gt 0 ] || fail "no peer agent CLI found (codex, claude, agy)"

# The exclusion loop in peers() can never emit the caller, so an automatic pick
# that names the caller means the self-consult fallback fired. Disclose it once
# here rather than inside peers(): that runs in several command substitutions,
# and call_one folds its child's stderr into the answer on stdout.
if [ "${#TARGETS_EXPLICIT[@]}" -eq 0 ]; then
  consult_caller="$(oms_peer_caller)"
  if [ -n "$consult_caller" ] && [ "${targets[0]}" = "$consult_caller" ]; then
    echo "note: no independent provider available; answering from the caller's own family (self-advice)" >&2
  fi
fi

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
artifact_dir_tmp="$(mktemp -d)" || fail "mktemp failed"
trap 'rm -rf "$artifact_dir_tmp"' EXIT

if [ "${#targets[@]}" -eq 1 ]; then
  if [ "${#TARGETS_EXPLICIT[@]}" -gt 0 ]; then
    call_one "${targets[0]}" "$artifact_dir_tmp/0" || status=$?
    # A pinned target is not retried, but a non-answer is still not an answer:
    # exiting 0 over a refusal or banner-only artifact tells the caller a
    # consultation happened when none did.
    if [ "$status" -eq 0 ] && [ "$DRY_RUN" -eq 0 ]; then
      art="$(sed -n 1p "$artifact_dir_tmp/0" 2>/dev/null || true)"
      if [ -n "$art" ] && [ -f "$art" ]; then
        q="$(ma_answer_quality "$art")"
        if [ "$q" != "ok" ]; then
          echo "consult: ${targets[0]} did not really answer ($q)" >&2
          [ "$q" != blocked ] ||
            echo "consult: ${targets[0]} said: $(ma_answer_block_reason "$art")" >&2
          status=1
        fi
      fi
    fi
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
  trap 'rm -rf "$tmpdir" "$artifact_dir_tmp"' EXIT
  index=0
  for p in "${targets[@]}"; do
    # Index the side files, not the target name: a panel repeats providers.
    call_one "$p" "$artifact_dir_tmp/$index" > "$tmpdir/$index.log" 2>&1 &
    pids+=("$!")
    logs+=("$tmpdir/$index.log")
    index=$((index + 1))
  done
  i=0
  for pid in "${pids[@]}"; do
    wait "$pid" || status=1
    [ "$QUIET" -eq 1 ] || cat "${logs[i]}"
    i=$((i + 1))
  done
  # A council is only as good as the answers in it, and two answers from one
  # model family are one opinion twice. Report both: how many targets really
  # answered, and how many independent families those answers came from.
  families=""
  index=0
  for p in "${targets[@]}"; do
    art=""
    [ ! -f "$artifact_dir_tmp/$index" ] || art="$(cat "$artifact_dir_tmp/$index")"
    index=$((index + 1))
    [ -n "$art" ] && [ -f "$art" ] || continue
    q="$(ma_answer_quality "$art")"
    if [ "$q" != "ok" ]; then
      echo "consult: $p did not really answer ($q)" >&2
      [ "$q" != blocked ] ||
        echo "consult: $p said: $(ma_answer_block_reason "$art")" >&2
      continue
    fi
    usable=$((usable + 1))
    selected="$(sed -n 's/^model-result: selected=\(.*\) effort=.*/\1/p' "$art" | sed -n '1p')"
    family="$(oms_provider_model_family "$(target_provider "$p")" "$selected" 2>/dev/null || printf 'unknown')"
    case "
$families
" in
      *"
$family
"*) ;;
      *) families="${families:+$families
}$family" ;;
    esac
  done
  family_count=0
  [ -z "$families" ] || family_count="$(printf '%s\n' "$families" | grep -c .)"
  echo "consult: ${usable}/${#targets[@]} target(s) answered, ${family_count} independent model family(ies)"
  if [ "$usable" -gt 1 ] && [ "$family_count" -lt 2 ]; then
    echo "consult: every answer came from one model family; treat agreement as one opinion, not corroboration" >&2
  fi
  # A panel where nothing answered is a failed consultation, whatever each
  # seat's exit code claimed.
  if [ "$DRY_RUN" -eq 0 ] && [ "$usable" -eq 0 ]; then
    echo "consult: no target produced a usable answer" >&2
    status=1
  fi
fi

echo "thread: $thread"
if [ "$QUIET" -eq 0 ] && [ "$DRY_RUN" -eq 0 ]; then
  echo "---"
  bash "$THREAD_SH" --repo "$REPO" --id "$thread" --turns 4 context || true
fi
exit "$status"
