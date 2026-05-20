#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d /tmp/oms-tests.XXXXXX)"

cleanup() {
  rm -rf "$TMP"
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_file_contains() {
  local file="$1"
  local text="$2"
  grep -Fq "$text" "$file" || fail "$file does not contain: $text"
}

assert_not_exists() {
  local path="$1"
  [ ! -e "$path" ] || fail "$path should not exist"
}

assert_one_artifact_contains() {
  local dir="$1"
  local pattern="$2"
  local text="$3"
  local file

  file="$(find "$dir" -type f -name "$pattern" | head -n 1)"
  [ -n "$file" ] || fail "missing artifact matching: $pattern"
  assert_file_contains "$file" "$text"
}

test_apply_dry_run_has_no_writes() {
  local project="$TMP/dry-run-new"

  OH_MY_SETTING_DRY_RUN=1 "$ROOT/scripts/apply-project-template.sh" general "$project" >/dev/null

  assert_not_exists "$project"
}

test_apply_rejects_unclosed_managed_block() {
  local project="$TMP/apply-malformed"
  mkdir -p "$project"
  cat > "$project/AGENTS.md" <<'EOF'
before
<!-- oh-my-setting:general:begin -->
managed
after
EOF

  if "$ROOT/scripts/apply-project-template.sh" general "$project" AGENTS.md >/dev/null 2>"$project/error"; then
    fail "apply should reject unclosed managed block"
  fi

  assert_file_contains "$project/AGENTS.md" "after"
  assert_file_contains "$project/error" "missing managed block end"
}

test_remove_rejects_unclosed_managed_block() {
  local project="$TMP/remove-malformed"
  mkdir -p "$project"
  cat > "$project/AGENTS.md" <<'EOF'
before
<!-- oh-my-setting:general:begin -->
managed
after
EOF

  if "$ROOT/scripts/remove-project-template.sh" general "$project" AGENTS.md >/dev/null 2>"$project/error"; then
    fail "remove should reject unclosed managed block"
  fi

  assert_file_contains "$project/AGENTS.md" "after"
  assert_file_contains "$project/error" "missing managed block end"
}

test_detect_ignores_common_generated_dirs() {
  local project="$TMP/generated-dirs"
  mkdir -p "$project/.venv" "$project/node_modules/pkg" "$project/backups/old"
  touch "$project/.venv/train.py"
  touch "$project/node_modules/pkg/model.py"
  touch "$project/backups/old/config.yaml"

  style="$("$ROOT/scripts/detect-project-style.sh" "$project")"
  [ "$style" = "general" ] || fail "expected general, got $style"
}

test_apply_and_remove_valid_block() {
  local project="$TMP/valid"
  mkdir -p "$project"

  "$ROOT/scripts/apply-project-template.sh" general "$project" >/dev/null
  assert_file_contains "$project/AGENTS.md" "<!-- oh-my-setting:general:begin -->"
  assert_file_contains "$project/CLAUDE.md" "<!-- oh-my-setting:general:begin -->"
  assert_file_contains "$project/PROJECT.md" "State: draft"
  local legacy_agent_file="GE""MINI.md"
  assert_not_exists "$project/$legacy_agent_file"

  "$ROOT/scripts/remove-project-template.sh" all "$project" >/dev/null
  if grep -Fq "oh-my-setting:general:begin" "$project/AGENTS.md"; then
    fail "managed block should be removed"
  fi
}

test_multi_agent_review_dry_run_artifacts() {
  local project="$TMP/review"
  local artifact_dir="$project/artifacts"
  local count

  mkdir -p "$project"
  git -C "$project" init >/dev/null
  printf 'before\n' > "$project/file.txt"
  git -C "$project" add file.txt
  git -C "$project" \
    -c user.email=test@example.com \
    -c user.name='Test User' \
    commit -m init >/dev/null
  printf 'after\n' > "$project/file.txt"

  OH_MY_SETTING_REVIEW_DRY_RUN=1 "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --prompt "Review current diff" >/dev/null

  count="$(find "$artifact_dir" -type f -name '*.md' | wc -l)"
  [ "$count" = "3" ] || fail "expected three review artifacts, got $count"
  assert_one_artifact_contains "$artifact_dir" 'codex-review-current-diff-*.md' 'DRY RUN'
  assert_one_artifact_contains "$artifact_dir" 'claude-review-current-diff-*.md' 'Question:'
  assert_one_artifact_contains "$artifact_dir" 'antigravity-review-current-diff-*.md' 'Diff:'
}


test_multi_agent_review_excludes_private_status() {
  local project="$TMP/review-private-status"
  local artifact_dir="$project/artifacts"
  local artifact

  mkdir -p "$project"
  git -C "$project" init >/dev/null
  printf 'tracked\n' > "$project/file.txt"
  git -C "$project" add file.txt
  git -C "$project" \
    -c user.email=test@example.com \
    -c user.name='Test User' \
    commit -m init >/dev/null
  printf 'changed\n' > "$project/file.txt"
  printf 'API_%s=not-real\n' 'KEY' > "$project/.env.local"

  OH_MY_SETTING_REVIEW_DRY_RUN=1 "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --providers codex \
    --prompt "Review status filtering" >/dev/null

  artifact="$(find "$artifact_dir" -type f -name 'codex-review-status-filtering-*.md' | head -n 1)"
  [ -n "$artifact" ] || fail "missing codex artifact"
  assert_file_contains "$artifact" 'file.txt'
  if grep -Fq '.env.local' "$artifact"; then
    fail "private .env.local path leaked into review artifact"
  fi
}

test_multi_agent_review_secret_diff_skips_external() {
  local project="$TMP/review-secret-diff"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project"
  git -C "$project" init >/dev/null
  printf 'safe\n' > "$project/file.txt"
  git -C "$project" add file.txt
  git -C "$project" \
    -c user.email=test@example.com \
    -c user.name='Test User' \
    commit -m init >/dev/null
  printf 'api_%s = "not-real"\n' 'key' >> "$project/file.txt"

  if OH_MY_SETTING_REVIEW_DRY_RUN=1 "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --prompt "Review secret gate" >/dev/null 2>"$project/error"; then
    fail "secret-like diff should skip external review"
  fi

  assert_file_contains "$project/error" 'sensitive-looking diff content detected'
  [ ! -d "$artifact_dir" ] || [ -z "$(find "$artifact_dir" -type f -name '*.md' -print -quit)" ] ||
    fail "secret-like diff should not write provider artifacts"
}

test_multi_agent_review_no_diff_provider_subset() {
  local project="$TMP/review-no-diff"
  local artifact_dir="$project/artifacts"
  local count

  mkdir -p "$project"
  git -C "$project" init >/dev/null
  OH_MY_SETTING_REVIEW_DRY_RUN=1 "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --providers antigravity \
    --no-diff \
    --prompt "Review no diff mode" >/dev/null

  count="$(find "$artifact_dir" -type f -name '*.md' | wc -l)"
  [ "$count" = "1" ] || fail "expected one review artifact, got $count"
  assert_one_artifact_contains "$artifact_dir" 'antigravity-review-no-diff-mode-*.md' 'Git context omitted by --no-diff.'
}


test_multi_agent_review_rejects_unknown_provider() {
  local project="$TMP/review-bad-provider"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project"
  git -C "$project" init >/dev/null

  if OH_MY_SETTING_REVIEW_DRY_RUN=1 "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --providers '../outside' \
    --no-diff \
    --prompt "Review bad provider" >/dev/null 2>"$project/error"; then
    fail "unknown provider should fail"
  fi

  assert_file_contains "$project/error" 'unsupported provider'
  [ ! -e "$project/outside-review-bad-provider" ] || fail "provider path traversal should not write outside artifact dir"
}

test_multi_agent_review_single_provider_failure_exits() {
  local project="$TMP/review-provider-failure"
  local artifact_dir="$project/artifacts"
  local bin_dir="$project/bin"
  local home_dir="$project/home"

  mkdir -p "$project" "$bin_dir" "$home_dir"
  git -C "$project" init >/dev/null
  cat > "$bin_dir/codex" <<'EOF'
#!/usr/bin/env bash
exit 42
EOF
  chmod +x "$bin_dir/codex"

  if HOME="$home_dir" PATH="$bin_dir:/usr/bin:/bin" "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --providers codex \
    --no-diff \
    --prompt "Review failing provider" >/dev/null 2>"$project/error"; then
    fail "single provider failure should fail"
  fi

  assert_file_contains "$project/error" 'no external review providers succeeded'
  assert_one_artifact_contains "$artifact_dir" 'codex-review-failing-provider-*.md' '## Exit'
}


test_multi_agent_ask_dry_run_no_repo() {
  local project="$TMP/ask-no-repo"
  local artifact_dir="$project/artifacts"
  local count

  mkdir -p "$project"
  OH_MY_SETTING_ASK_DRY_RUN=1 "$ROOT/scripts/multi-agent-ask.sh" \
    --artifact-dir "$artifact_dir" \
    --prompt "Compare two implementation options" >/dev/null

  count="$(find "$artifact_dir" -type f -name '*.md' | wc -l)"
  [ "$count" = "3" ] || fail "expected three ask artifacts, got $count"
  assert_one_artifact_contains "$artifact_dir" 'codex-compare-two-implementation-options-*.md' 'Repository context: omitted.'
  assert_one_artifact_contains "$artifact_dir" 'claude-compare-two-implementation-options-*.md' 'DRY RUN'
  assert_one_artifact_contains "$artifact_dir" 'antigravity-compare-two-implementation-options-*.md' 'Answer:'
}

test_multi_agent_ask_repo_context_subset() {
  local project="$TMP/ask-repo-context"
  local artifact_dir="$project/artifacts"
  local count

  mkdir -p "$project"
  git -C "$project" init >/dev/null
  printf 'before\n' > "$project/file.txt"
  git -C "$project" add file.txt
  git -C "$project" \
    -c user.email=test@example.com \
    -c user.name='Test User' \
    commit -m init >/dev/null
  printf 'after\n' > "$project/file.txt"
  printf 'API_%s=not-real\n' 'KEY' > "$project/.env.local"

  OH_MY_SETTING_ASK_DRY_RUN=1 "$ROOT/scripts/multi-agent-ask.sh" \
    --repo "$project" \
    --repo-context \
    --artifact-dir "$artifact_dir" \
    --providers codex \
    --prompt "Assess repo state" >/dev/null

  count="$(find "$artifact_dir" -type f -name '*.md' | wc -l)"
  [ "$count" = "1" ] || fail "expected one ask artifact, got $count"
  assert_one_artifact_contains "$artifact_dir" 'codex-assess-repo-state-*.md' 'Git status:'
  assert_one_artifact_contains "$artifact_dir" 'codex-assess-repo-state-*.md' 'file.txt'
  if grep -R -Fq '.env.local' "$artifact_dir"; then
    fail "private .env.local path leaked into ask artifact"
  fi
}

test_multi_agent_ask_secret_diff_skips_external() {
  local project="$TMP/ask-secret-diff"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project"
  git -C "$project" init >/dev/null
  printf 'safe\n' > "$project/file.txt"
  git -C "$project" add file.txt
  git -C "$project" \
    -c user.email=test@example.com \
    -c user.name='Test User' \
    commit -m init >/dev/null
  printf 'api_%s = "not-real"\n' 'key' >> "$project/file.txt"

  if OH_MY_SETTING_ASK_DRY_RUN=1 "$ROOT/scripts/multi-agent-ask.sh" \
    --repo "$project" \
    --diff \
    --artifact-dir "$artifact_dir" \
    --prompt "Explain this diff" >/dev/null 2>"$project/error"; then
    fail "secret-like ask diff should skip external ask"
  fi

  assert_file_contains "$project/error" 'sensitive-looking diff content detected'
  [ ! -d "$artifact_dir" ] || [ -z "$(find "$artifact_dir" -type f -name '*.md' -print -quit)" ] ||
    fail "secret-like ask diff should not write provider artifacts"
}

test_apply_dry_run_has_no_writes
test_apply_rejects_unclosed_managed_block
test_remove_rejects_unclosed_managed_block
test_detect_ignores_common_generated_dirs
test_apply_and_remove_valid_block
test_multi_agent_review_dry_run_artifacts
test_multi_agent_review_excludes_private_status
test_multi_agent_review_secret_diff_skips_external
test_multi_agent_review_no_diff_provider_subset
test_multi_agent_review_rejects_unknown_provider
test_multi_agent_review_single_provider_failure_exits
test_multi_agent_ask_dry_run_no_repo
test_multi_agent_ask_repo_context_subset
test_multi_agent_ask_secret_diff_skips_external

echo "scripts-smoke: ok"
