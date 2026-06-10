#!/usr/bin/env bash
set -euo pipefail

REPO="$PWD"
PROMPT=""
PROVIDERS="codex,claude,antigravity"
ARTIFACT_DIR=""
NO_DIFF=0
BASE_REF=""
DRY_RUN="${OH_MY_SETTING_REVIEW_DRY_RUN:-0}"

REVIEW_PATHS=(
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

usage() {
  cat <<'EOF'
Usage: multi-agent-review.sh [options] --prompt TEXT

Ask the same review question to Codex, Claude Code, and Antigravity, then persist
each answer as an artifact.

Options:
  --prompt TEXT        Review question/task. Required.
  --repo PATH          Git repo to review. Default: current directory.
  --base REF           Diff base ref. Default: HEAD (staged + unstaged changes).
                       Use e.g. --base origin/main for branch/PR review.
  --providers LIST     Comma list: codex,claude,antigravity. Default: all three.
  --artifact-dir PATH  Artifact directory. Default: REPO/.omc/artifacts/review.
  --no-diff            Do not attach git diff/status context.
  --dry-run            Write prompts as artifacts without CLI calls.
  -h, --help           Show this help.

Environment:
  OH_MY_SETTING_REVIEW_DRY_RUN=1   Same as --dry-run.
  OMS_MULTI_AGENT_TIMEOUT=5m       Per-provider wall-clock timeout (GNU timeout).
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
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

git_diff_base() {
  local repo="$1"
  if [ -n "$BASE_REF" ]; then
    printf '%s\n' "$BASE_REF"
  elif git -C "$repo" rev-parse --verify HEAD >/dev/null 2>&1; then
    printf 'HEAD\n'
  else
    printf '4b825dc642cb6eb9a060e54bf8d69288fbee4904\n'
  fi
}

run_with_timeout() {
  if command -v timeout >/dev/null 2>&1; then
    timeout "${OMS_MULTI_AGENT_TIMEOUT:-5m}" "$@"
  else
    "$@"
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

safe_review_status() {
  local repo="$1"
  git -C "$repo" status --short -- "${REVIEW_PATHS[@]}"
}

safe_review_diff() {
  local repo="$1"
  local base
  local tmp
  base="$(git_diff_base "$repo")"
  tmp="$(mktemp)" || return 1

  if ! git -C "$repo" diff "$base" -- "${REVIEW_PATHS[@]}" > "$tmp"; then
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

write_prompt() {
  local output="$1"
  local repo="$2"
  local question="$3"
  local diff_file="$4"
  local status_file="$5"

  {
    printf 'You are one of three independent reviewers: Codex, Claude Code, and Antigravity.\n'
    printf 'Answer the same question from your own perspective. Do not modify files.\n'
    printf 'Find bugs, regressions, missing tests, unclear contracts, and unsafe operations.\n'
    printf 'Tie every finding to file/line evidence, diff evidence, commands, or docs.\n'
    printf 'If there are no actionable findings, say "No findings".\n\n'
    printf 'Question:\n%s\n\n' "$question"
    printf 'Repository:\n%s\n\n' "$repo"
    if [ "$NO_DIFF" -eq 0 ]; then
      printf 'Git status:\n'
      cat "$status_file"
      printf '\nDiff:\n'
      cat "$diff_file"
      printf '\n'
    else
      printf 'Git context omitted by --no-diff.\n'
    fi
    printf '\nReturn exactly these sections:\n'
    printf 'Findings:\n'
    printf 'Risks:\n'
    printf 'Missing tests:\n'
    printf 'Recommendation:\n'
  } > "$output"
}

run_provider() {
  local provider="$1"
  local prompt_file="$2"
  local artifact="$3"
  local started
  local status

  started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  {
    printf '# %s review\n\n' "$provider"
    printf -- '- started: %s\n' "$started"
    printf -- '- repo: %s\n' "$REPO"
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

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prompt)
      [ "$#" -ge 2 ] || fail "--prompt requires text"
      PROMPT="$2"
      shift 2
      ;;
    --repo)
      [ "$#" -ge 2 ] || fail "--repo requires path"
      REPO="$2"
      shift 2
      ;;
    --base)
      [ "$#" -ge 2 ] || fail "--base requires git ref"
      BASE_REF="$2"
      shift 2
      ;;
    --providers)
      [ "$#" -ge 2 ] || fail "--providers requires list"
      PROVIDERS="$2"
      shift 2
      ;;
    --artifact-dir)
      [ "$#" -ge 2 ] || fail "--artifact-dir requires path"
      ARTIFACT_DIR="$2"
      shift 2
      ;;
    --no-diff)
      NO_DIFF=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      if [ -z "$PROMPT" ]; then
        PROMPT="$1"
        shift
      else
        fail "unknown argument: $1"
      fi
      ;;
  esac
done

[ -n "$PROMPT" ] || fail "--prompt is required"
REPO="$(cd "$REPO" && pwd)"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"
if [ -n "$BASE_REF" ]; then
  git -C "$REPO" rev-parse --verify --quiet "${BASE_REF}^{commit}" >/dev/null ||
    fail "invalid --base ref: $BASE_REF"
fi
ARTIFACT_DIR="${ARTIFACT_DIR:-$REPO/.omc/artifacts/review}"

load_user_tool_paths
mkdir -p "$ARTIFACT_DIR"

status_file="$(mktemp)" || fail "mktemp failed"
diff_file="$(mktemp)" || fail "mktemp failed"
prompt_file="$(mktemp)" || fail "mktemp failed"
cleanup() {
  rm -f "$status_file" "$diff_file" "$prompt_file"
}
trap cleanup EXIT

if [ "$NO_DIFF" -eq 0 ]; then
  safe_review_status "$REPO" > "$status_file"
  if ! safe_review_diff "$REPO" > "$diff_file"; then
    echo "external review skipped: sensitive-looking diff content detected" >&2
    exit 3
  fi
else
  : > "$diff_file"
fi

write_prompt "$prompt_file" "$REPO" "$PROMPT" "$diff_file" "$status_file"

timestamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
slug="$(slugify "$PROMPT")"
[ -n "$slug" ] || slug="review"
ok=0
total=0
declare -a pids artifacts provider_names

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

for i in "${!pids[@]}"; do
  if wait "${pids[i]}"; then
    ok=$((ok + 1))
  fi
done

synth_file="$ARTIFACT_DIR/_synthesis-$slug-$timestamp.md"
{
  printf '# Multi-agent review synthesis\n\n'
  printf -- '- generated: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf -- '- repo: %s\n' "$REPO"
  printf -- '- success: %d/%d providers\n\n' "$ok" "$total"
  printf '## Prompt\n\n'
  printf '```\n'
  cat "$prompt_file"
  printf '\n```\n\n'
  for i in "${!artifacts[@]}"; do
    printf '## %s\n\n' "${provider_names[i]}"
    awk 'BEGIN{flag=0} /^## Output$/{flag=1;next} /^## Exit$/{flag=0} flag' "${artifacts[i]}"
    printf '\n'
  done
} > "$synth_file"

echo "summary: $ok/$total providers succeeded"
echo "artifacts: $ARTIFACT_DIR"
echo "synthesis: $synth_file"
if [ "$ok" -eq 0 ]; then
  echo "warning: no external review providers succeeded" >&2
  exit 1
fi
if [ "$total" -ge 2 ] && [ "$ok" -lt 2 ]; then
  echo "warning: external review quorum not met; synthesize with current-agent local review" >&2
  exit 1
fi
