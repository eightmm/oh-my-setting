# shellcheck shell=bash

ma_conversation_boundary() {
  ma_write_post_attempt_guard "$1" || return 2
  # ma_provider_attempt owns this optional exclusive snapshot and its recovery.
  if [ -n "$3" ]; then
    ma_authority_state_snapshot "$2" "$4" || return 2
    cmp -s "$3" "$4" || return 2
  fi
}

# One routed attempt, several native processes, one explicitly bound session.
ma_conversation_run() {
  local provider="$1" access="$2" prompt="$3" output="$4" workdir="$5"
  local origin="$6" state_repo="$7" call_id="$8"
  local exclusive_before="$9" exclusive_after="${10}"
  shift 10
  local helper
  helper="$(ma_scripts_dir)/lib/peer-conversation.py"
  local scratch session_id="" turn=0 status=0 next_status=0 pending_denials=0
  local -a base=("$@") cmd
  scratch="$(mktemp -d "${TMPDIR:-/tmp}/oms-conversation.XXXXXX")" || return 2
  conversation_scratch="$scratch"
  chmod 700 "$scratch" || return 2
  : > "$output"
  while :; do
    # Recheck after the operator wait as well as immediately after each worker.
    if ! ma_conversation_boundary "$access" "$state_repo" "$exclusive_before" "$exclusive_after"; then
      export OMS_WORKER_AUTHORITY_VIOLATION=1
      status=125
      break
    fi
    cmd=("${base[@]}")
    if [ -n "$session_id" ]; then
      if [ "$provider" = claude ]; then cmd+=(--resume "$session_id")
      else cmd+=(--conversation "$session_id"); fi
    fi
    [ "$provider" != antigravity ] || cmd+=(--print "$(cat "$prompt")")
    turn=$((turn + 1))
    (
      exec 3>&- 4<&-
      unset OMS_PEER_INTERACTIVE OMS_CONVERSATION_MAX_TURNS OMS_CONVERSATION_IDLE_SECONDS
      ma_export_child_env "$provider" "$origin" "$state_repo" "$call_id" "$access"
      cd "$workdir" || exit 1
      run_with_timeout "${cmd[@]}" < "$prompt"
    ) > "$scratch/native" 2>&1 &
    local pid="$!"
    if wait "$pid"; then status=0; else status=$?; fi
    if ! ma_conversation_boundary "$access" "$state_repo" "$exclusive_before" "$exclusive_after"; then
      export OMS_WORKER_AUTHORITY_VIOLATION=1
      status=125
    fi
    [ "$status" -eq 0 ] || break
    if ! python3 -B "$helper" result "$provider" "$scratch/native" "$session_id" > "$scratch/result"; then
      status=2
      break
    fi
    session_id="$(python3 -B "$helper" id "$scratch/result" | tr -d '\r')" || { status=2; break; }
    pending_denials="$(python3 -B "$helper" denials "$scratch/result" | tr -d '\r')" || { status=2; break; }
    printf '\nConversation turn %s (not yet verified):\n' "$turn" >> "$output"
    python3 -B "$helper" text "$scratch/result" >> "$output" || { status=2; break; }
    if oms_model_is_policy_decline_output "$output" "$prompt" ||
      { [ "$pending_denials" -eq 0 ] && [ "$(ma_answer_quality "$output")" = blocked ]; }; then
      status=4
      break
    fi
    python3 -B "$helper" emit "$scratch/result" "$turn" "$workdir" >&3 || { status=2; break; }
    next_status=0
    python3 -B "$helper" input "$OMS_CONVERSATION_IDLE_SECONDS" <&4 > "$scratch/prompt" || next_status=$?
    if [ "$next_status" -eq 10 ]; then
      if [ "$pending_denials" -gt 0 ]; then
        echo "error: resolve denied tools in a follow-up before finishing" >&2
        status=4
      fi
      break
    fi
    if [ "$next_status" -ne 0 ]; then status="$next_status"; break; fi
    if [ "$turn" -ge "$OMS_CONVERSATION_MAX_TURNS" ]; then
      echo "error: conversation turn limit reached; send finish instead of another prompt" >&2
      status=2
      break
    fi
    ma_validate_outbound_prompt "$scratch/prompt" || { status=2; break; }
    printf '\nController follow-up %s (within the original delegation scope):\n' "$((turn + 1))" >> "$output"
    sed 's/^/> /' "$scratch/prompt" >> "$output"
    printf '\n' >> "$output"
    prompt="$scratch/prompt"
  done
  if [ "$status" -ne 0 ]; then
    printf '\nBLOCKED: conversation stopped before explicit successful finish (exit %s).\n' "$status" >> "$output"
    if [ "$status" -ne 125 ] && [ -f "$scratch/native" ]; then
      printf '\nBounded native diagnostics (untrusted, sanitized):\n' >> "$output"
      tail -c 4096 "$scratch/native" | ma_sanitize_quoted_output >> "$output" || true
    fi
  fi
  rm -rf "$scratch"
  # The peer-delegate EXIT trap owns this cleanup pointer.
  # shellcheck disable=SC2034
  conversation_scratch=""
  return "$status"
}
