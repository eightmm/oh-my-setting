# shellcheck shell=bash
# Shared helpers for multi-agent-ask.sh and multi-agent-review.sh.
# Sourced, not executed. Callers must set before use:
#   MA_KIND              ask | review (artifact headers, messages)
#   MA_SHOW_REPO         1 to include "- repo:" lines (review)
#   MA_QUORUM_FALLBACK   word used in the quorum warning (answer | review)
#   MA_DEBATE_ROLE       advisors | reviewers
#   MA_DEBATE_TOPIC      question | diff
#   MA_DEBATE_SECTIONS   newline-joined section list for debate replies
# plus the per-run globals referenced inside each function.

# shellcheck source=agent-memory-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/agent-memory-common.sh"
# shellcheck source=agent-task-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/agent-task-common.sh"

MA_SAFE_PATHS=(
  .
  ':(top,exclude,glob)local/**'
  ':(top,exclude,glob).env*'
  ':(top,exclude,glob)**/.env*'
  ':(top,exclude,glob).envrc'
  ':(top,exclude,glob)**/.envrc'
  ':(top,exclude,glob)**/*.key'
  ':(top,exclude,glob)**/*.pem'
  ':(top,exclude,glob)**/*.crt'
  ':(top,exclude,glob)**/*.p12'
  ':(top,exclude,glob)**/*.pfx'
  ':(top,exclude,glob)**/id_rsa*'
  ':(top,exclude,glob)**/.aws/**'
  ':(top,exclude,glob)**/.ssh/**'
  ':(top,exclude,glob)**/.netrc'
  ':(top,exclude,glob)**/*credentials*'
  ':(top,exclude,glob)**/*secrets*.yml'
  ':(top,exclude,glob)**/*secrets*.yaml'
  ':(top,exclude)custom-skills/slurm-hpc/references/cluster.generated.md'
)

fail() {
  echo "error: $*" >&2
  exit 2
}

ma_scripts_dir() {
  cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd
}

ma_repo_label() {
  local repo="$1"
  if [ -n "$repo" ]; then
    printf '%s (path omitted)\n' "$(basename "$repo")"
  else
    printf 'repository path omitted\n'
  fi
}

load_user_tool_paths() {
  export PATH="$HOME/.local/bin:$PATH"
  export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"

  if [ -s "$NVM_DIR/nvm.sh" ]; then
    # shellcheck disable=SC1091
    . "$NVM_DIR/nvm.sh"
    nvm use default >/dev/null 2>&1 || true
  fi
}

slugify() {
  printf '%s' "$1" |
    tr '[:upper:]' '[:lower:]' |
    tr -cs '[:alnum:]' '-' |
    sed 's/^-//;s/-$//;s/--*/-/g' |
    cut -c1-48
}

run_with_timeout() {
  if command -v timeout >/dev/null 2>&1; then
    timeout "${OMS_MULTI_AGENT_TIMEOUT:-5m}" "$@"
  else
    "$@"
  fi
}

ma_git_diff_base() {
  local repo="$1"
  if [ -n "${BASE_REF:-}" ]; then
    printf '%s\n' "$BASE_REF"
  elif git -C "$repo" rev-parse --verify HEAD >/dev/null 2>&1; then
    printf 'HEAD\n'
  else
    printf '4b825dc642cb6eb9a060e54bf8d69288fbee4904\n'
  fi
}

contains_sensitive_content() {
  local file="$1"
  # Split terms so this script can safely review its own source diff.
  local secret_re
  secret_re='(^|[^A-Za-z0-9_])((t[o]ken|s[e]cret|passw[o]rd|private_[k]ey|api[-_]?[k]ey|aws_s[e]cret_access_[k]ey)[[:space:]]*[:=]|auth[o]rization:[[:space:]]+[^[:space:]]+|bear[e]r[[:space:]]+[A-Za-z0-9._-]{10,}|gh[p]_[A-Za-z0-9_]+|s[k]-[A-Za-z0-9_-]{10,}|xox[bap]-[A-Za-z0-9-]{10,}|AK[I]A[0-9A-Z]{16}|-----BE[G]IN)'

  grep -E '^\+' "$file" |
    grep -Ev '^\+\+\+ ' |
    grep -Ev '^\+[[:space:]]*secret_re=' |
    grep -Eiq "$secret_re"
}

ma_prompt_has_sensitive_content() {
  local file="$1"
  local tmp

  [ -s "$file" ] || return 1
  tmp="$(mktemp)" || return 1
  grep -Ev '^[[:space:]]*(local[[:space:]]+)?[A-Za-z0-9_]*secret[A-Za-z0-9_]*_?re=' "$file" |
    grep -Ev 'agent_memory_sensitive_re|ma_prompt_has_sensitive_content|contains_sensitive_content' > "$tmp" || true

  if agent_memory_file_has_sensitive_content "$tmp"; then
    rm -f "$tmp"
    return 0
  fi
  rm -f "$tmp"
  return 1
}

ma_validate_outbound_prompt() {
  local prompt="$1"

  if ma_prompt_has_sensitive_content "$prompt"; then
    echo "error: outbound provider context contains sensitive-looking content; external call blocked" >&2
    echo "hint: remove secrets, private keys, absolute machine paths, cluster details, raw logs, datasets, or checkpoints from task/memory/prompt context" >&2
    return 3
  fi
}

ma_write_task_context() {
  local repo="$1"
  agent_task_emit_context "$repo" "$(agent_task_project_file "$repo")" || true
}

ma_write_ml_context() {
  local repo="$1"
  local mode="${OMS_AGENT_ML_CONTEXT:-auto}"
  local scripts_dir

  case "$mode" in
    0|false|off|none) return 0 ;;
    1|true|on|auto) ;;
    *) return 0 ;;
  esac

  scripts_dir="$(ma_scripts_dir)"
  [ -x "$scripts_dir/agent-ml-context.sh" ] || return 0
  if [ "$mode" = "auto" ]; then
    "$scripts_dir/agent-ml-context.sh" --repo "$repo" || true
  else
    "$scripts_dir/agent-ml-context.sh" --repo "$repo" --force || true
  fi
}

ma_safe_status() {
  local repo="$1"
  git -C "$repo" status --short -- "${MA_SAFE_PATHS[@]}"
}

ma_safe_diff() {
  local repo="$1"
  local base
  local tmp
  base="$(ma_git_diff_base "$repo")"
  tmp="$(mktemp)" || return 1

  if ! git -C "$repo" diff "$base" -- "${MA_SAFE_PATHS[@]}" > "$tmp"; then
    rm -f "$tmp"
    return 1
  fi

  if contains_sensitive_content "$tmp"; then
    rm -f "$tmp"
    return 1
  fi

  cat "$tmp"
  rm -f "$tmp"
}

extract_output() {
  awk 'BEGIN{flag=0} /^## Output$/{flag=1;next} /^## Exit$/{flag=0} flag' "$1"
}

run_provider() {
  local provider="$1"
  local prompt_file="$2"
  local artifact="$3"
  local started
  local status

  started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  if ! ma_validate_outbound_prompt "$prompt_file"; then
    {
      printf '# %s %s\n\n' "$provider" "$MA_KIND"
      printf -- '- started: %s\n' "$started"
      printf '## Output\n\n'
      printf 'SKIPPED: outbound provider context contains sensitive-looking content.\n'
      printf 'No prompt content was written to this artifact.\n'
      printf '\n\n## Exit\n\n3\n'
    } > "$artifact"
    echo "blocked: $provider sensitive outbound context -> $artifact"
    return 1
  fi

  {
    printf '# %s %s\n\n' "$provider" "$MA_KIND"
    printf -- '- started: %s\n' "$started"
    if [ "${MA_SHOW_REPO:-0}" = "1" ]; then
      printf -- '- repo: %s\n' "$(ma_repo_label "$REPO")"
    fi
    printf -- '- prompt-file: %s\n\n' "$prompt_file"
    printf '## Prompt\n\n'
    cat "$prompt_file"
    printf '\n\n## Output\n\n'
  } > "$artifact"

  if [ "$DRY_RUN" = "1" ]; then
    printf 'DRY RUN: provider command skipped.\n' >> "$artifact"
    echo "dry-run: $provider -> $artifact"
    return 0
  fi

  local binary="$provider"
  if [ "$provider" = "antigravity" ]; then
    binary="agy"
  fi

  if ! command -v "$binary" >/dev/null 2>&1; then
    printf 'SKIPPED: command not found: %s\n' "$binary" >> "$artifact"
    echo "skipped: $provider missing ($binary) -> $artifact"
    return 1
  fi

  set +e
  case "$provider" in
    codex)
      run_with_timeout codex exec --sandbox read-only - < "$prompt_file" >> "$artifact" 2>&1
      status=$?
      ;;
    claude)
      run_with_timeout claude --permission-mode plan -p < "$prompt_file" >> "$artifact" 2>&1
      status=$?
      ;;
    antigravity|agy)
      run_with_timeout agy --print --sandbox --print-timeout "${OMS_MULTI_AGENT_PRINT_TIMEOUT:-5m}" < "$prompt_file" >> "$artifact" 2>&1
      status=$?
      ;;
    *)
      printf 'SKIPPED: unsupported provider: %s\n' "$provider" >> "$artifact"
      status=1
      ;;
  esac
  set -e

  printf '\n\n## Exit\n\n%s\n' "$status" >> "$artifact"
  if [ "$status" -eq 0 ]; then
    echo "ok: $provider -> $artifact"
  else
    echo "failed: $provider -> $artifact"
  fi
  return "$status"
}

# Round 1: fan out the same prompt to all providers in parallel.
# Sets: ok, total, pids, artifacts, provider_names, alive, last_arts.
ma_run_round1() {
  local provider artifact i
  ok=0
  total=0
  pids=()
  artifacts=()
  provider_names=()

  IFS=',' read -r -a provider_list <<< "$PROVIDERS"
  for provider in "${provider_list[@]}"; do
    provider="$(printf '%s' "$provider" | tr -d '[:space:]')"
    [ -n "$provider" ] || continue
    case "$provider" in
      codex|claude|antigravity|agy) ;;
      *) fail "unsupported provider: $provider" ;;
    esac
    total=$((total + 1))
    artifact="$ARTIFACT_DIR/$provider-$slug-$timestamp.md"
    run_provider "$provider" "$prompt_file" "$artifact" &
    pids+=("$!")
    artifacts+=("$artifact")
    provider_names+=("$provider")
  done

  [ "$total" -gt 0 ] || fail "no providers selected"

  alive=()
  last_arts=()
  for i in "${!pids[@]}"; do
    if wait "${pids[i]}"; then
      ok=$((ok + 1))
      alive[i]=1
    else
      alive[i]=0
    fi
    last_arts[i]="${artifacts[i]}"
  done
}

write_debate_prompt() {
  local output="$1"
  local provider="$2"
  local round="$3"
  local self_artifact="$4"
  shift 4
  # Remaining args: "name:artifact" pairs for the other participants.

  {
    printf 'You are %s, one of several independent %s debating the same %s.\n' \
      "$provider" "${MA_DEBATE_ROLE:-advisors}" "${MA_DEBATE_TOPIC:-question}"
    printf 'This is debate round %s. Critique the other %s with evidence and concrete reasoning.\n' \
      "$round" "${MA_DEBATE_ROLE:-advisors}"
    printf 'Do not converge for the sake of agreement; change your position only where another argument is stronger.\n'
    printf 'Do not modify files.\n\n'
    printf 'Original question:\n%s\n\n' "$PROMPT"
    printf 'Your previous answer:\n'
    extract_output "$self_artifact"
    printf '\nOther %s:\n' "${MA_DEBATE_ROLE:-advisors}"
    local pair name art
    for pair in "$@"; do
      name="${pair%%:*}"
      art="${pair#*:}"
      printf '\n## %s\n' "$name"
      extract_output "$art"
    done
    printf '\nReturn exactly these sections:\n'
    printf '%s\n' "$MA_DEBATE_SECTIONS"
  } > "$output"
}

# Debate rounds 2..DEBATE+1. Mutates alive and last_arts.
ma_run_debate_rounds() {
  local round i j k p others debate_prompt artifact
  local r_pids r_idx r_arts active

  for ((round = 2; round <= DEBATE + 1; round++)); do
    active=()
    for i in "${!provider_names[@]}"; do
      [ "${alive[i]}" = 1 ] && active+=("$i")
    done
    if [ "${#active[@]}" -lt 2 ]; then
      echo "debate round $round skipped: fewer than two active providers" >&2
      break
    fi

    r_pids=()
    r_idx=()
    r_arts=()
    for i in "${active[@]}"; do
      p="${provider_names[i]}"
      others=()
      for j in "${active[@]}"; do
        [ "$j" != "$i" ] && others+=("${provider_names[j]}:${last_arts[j]}")
      done
      debate_prompt="$debate_dir/prompt-r$round-$p"
      write_debate_prompt "$debate_prompt" "$p" "$round" "${last_arts[i]}" "${others[@]}"
      artifact="$ARTIFACT_DIR/$p-$slug-$timestamp-r$round.md"
      run_provider "$p" "$debate_prompt" "$artifact" &
      r_pids+=("$!")
      r_idx+=("$i")
      r_arts+=("$artifact")
    done

    for k in "${!r_pids[@]}"; do
      i="${r_idx[k]}"
      if wait "${r_pids[k]}"; then
        last_arts[i]="${r_arts[k]}"
      else
        # Drop failed provider from later rounds; keep its last good answer.
        alive[i]=0
      fi
    done
  done
}

ma_write_synthesis() {
  local synth_file="$1"
  local i
  {
    printf '# Multi-agent %s synthesis\n\n' "$MA_KIND"
    printf -- '- generated: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    if [ "${MA_SHOW_REPO:-0}" = "1" ]; then
      printf -- '- repo: %s\n' "$(ma_repo_label "$REPO")"
    fi
    printf -- '- success: %d/%d providers\n' "$ok" "$total"
    if [ "${DEBATE:-0}" -gt 0 ]; then
      printf -- '- debate rounds: %d\n' "$DEBATE"
    fi
    printf '\n## Prompt\n\n'
    printf '```\n'
    cat "$prompt_file"
    printf '\n```\n\n'
    for i in "${!artifacts[@]}"; do
      printf '## %s\n\n' "${provider_names[i]}"
      if [ "${last_arts[i]}" != "${artifacts[i]}" ]; then
        printf '_final answer after debate_\n\n'
      fi
      extract_output "${last_arts[i]}"
      printf '\n'
    done
  } > "$synth_file"
}

ma_quorum_exit() {
  echo "summary: $ok/$total providers succeeded"
  echo "artifacts: $ARTIFACT_DIR"
  echo "synthesis: $synth_file"
  if [ "$ok" -eq 0 ]; then
    echo "warning: no external $MA_KIND providers succeeded" >&2
    exit 1
  fi
  if [ "$total" -ge 2 ] && [ "$ok" -lt 2 ]; then
    echo "warning: external $MA_KIND quorum not met; synthesize with current-agent local ${MA_QUORUM_FALLBACK:-$MA_KIND}" >&2
    exit 1
  fi
}
