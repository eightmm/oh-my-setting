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
  grep -Fq -- "$text" "$file" || fail "$file does not contain: $text"
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

test_apply_ml_dry_run_has_no_writes() {
  local project="$TMP/ml-dry-run-new"

  OH_MY_SETTING_DRY_RUN=1 "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null

  assert_not_exists "$project"
}

test_apply_ml_scaffolds_docs() {
  local project="$TMP/ml-docs"
  mkdir -p "$project/docs"
  printf 'user content\n' > "$project/docs/DATA.md"

  "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null

  [ -f "$project/docs/MODEL.md" ] || fail "ml docs not scaffolded: MODEL.md"
  [ -f "$project/docs/TRAINING.md" ] || fail "ml docs not scaffolded: TRAINING.md"
  [ -f "$project/docs/REPRODUCIBILITY.md" ] || fail "ml docs not scaffolded: REPRODUCIBILITY.md"
  assert_file_contains "$project/docs/DATA.md" "user content"
  if grep -Fq '## Schema' "$project/docs/DATA.md"; then
    fail "existing docs/DATA.md should not be overwritten"
  fi
}

test_apply_ml_scaffolds_gitignore() {
  local project="$TMP/ml-gitignore"
  mkdir -p "$project"
  printf 'node_modules/\ndata/\n' > "$project/.gitignore"

  "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null

  assert_file_contains "$project/.gitignore" "node_modules/"
  assert_file_contains "$project/.gitignore" "checkpoints/"
  assert_file_contains "$project/.gitignore" "outputs/"
  [ "$(grep -cxF 'data/' "$project/.gitignore")" = "1" ] ||
    fail ".gitignore should not duplicate existing data/ entry"

  local before
  before="$(wc -l < "$project/.gitignore")"
  "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null
  [ "$(wc -l < "$project/.gitignore")" = "$before" ] ||
    fail "re-running apply should not grow .gitignore"
}

test_apply_ml_scaffolds_check_contract() {
  local project="$TMP/ml-check"
  mkdir -p "$project/scripts"
  printf '#!/bin/sh\necho custom\n' > "$project/scripts/check.sh"

  "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null
  assert_file_contains "$project/scripts/check.sh" "custom"

  local project2="$TMP/ml-check-new"
  mkdir -p "$project2"
  "$ROOT/scripts/apply-project-template.sh" ml "$project2" >/dev/null
  [ -x "$project2/scripts/check.sh" ] || fail "check.sh not scaffolded executable"
  assert_file_contains "$project2/scripts/check.sh" "Project verification contract"
}

test_project_doctor_warns_missing_check() {
  local project="$TMP/doctor-no-check"
  mkdir -p "$project"

  "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null
  out="$("$ROOT/scripts/project-doctor.sh" "$project")" || fail "doctor should pass fresh ml apply"
  printf '%s' "$out" | grep -Fq 'verification contract present' ||
    fail "doctor should confirm check.sh presence"

  rm "$project/scripts/check.sh"
  out="$("$ROOT/scripts/project-doctor.sh" "$project")" ||
    fail "missing check.sh should warn, not fail"
  printf '%s' "$out" | grep -Fq 'scripts/check.sh missing' ||
    fail "doctor should warn about missing check.sh"
}

test_job_digest_log_mode() {
  local dir="$TMP/job-digest"
  mkdir -p "$dir"
  printf 'step 1 loss: 0.5\nTraceback (most recent call last):\n  File "t.py", line 1\nRuntimeError: CUDA out of memory\n' > "$dir/run.log"

  out="$("$ROOT/scripts/job-digest.sh" "$dir/run.log")"
  printf '%s' "$out" | grep -Fq 'CUDA out of memory' || fail "digest missing error pattern"
  printf '%s' "$out" | grep -Fq '## Last traceback' || fail "digest missing traceback section"

  if "$ROOT/scripts/job-digest.sh" "$dir/nonexistent" >/dev/null 2>"$dir/error"; then
    fail "digest should reject non-file non-jobid argument"
  fi
  assert_file_contains "$dir/error" 'not a log file or job id'
}

test_run_ledger_records_and_lists() {
  local project="$TMP/run-ledger"
  make_committed_repo "$project"

  if (cd "$project" && "$ROOT/scripts/run-ledger.sh" --note trial -- bash -c 'exit 3' >/dev/null 2>&1); then
    fail "ledger should propagate command exit code"
  fi
  assert_file_contains "$project/docs/EXPERIMENTS.jsonl" '"exit": 3'
  assert_file_contains "$project/docs/EXPERIMENTS.jsonl" '"note": "trial"'

  (cd "$project" && "$ROOT/scripts/run-ledger.sh" -- bash -c 'exit 0' >/dev/null 2>&1) ||
    fail "successful command should exit 0"
  out="$(cd "$project" && "$ROOT/scripts/run-ledger.sh" list 5)"
  printf '%s' "$out" | grep -Fq 'exit=3' || fail "ledger list missing failed run"
}

test_delegate_auto_verify_uses_check_contract() {
  local project="$TMP/delegate-auto-verify"
  local artifact_dir="$project/artifacts"
  local bin_dir="$project/bin"
  local home_dir="$project/home"

  make_committed_repo "$project"
  mkdir -p "$project/scripts" "$bin_dir" "$home_dir"
  printf '#!/usr/bin/env bash\necho check-contract-ran\n' > "$project/scripts/check.sh"
  chmod +x "$project/scripts/check.sh"
  git -C "$project" add scripts/check.sh
  git -C "$project" -c user.email=t@e.c -c user.name=T commit -qm check

  cat > "$bin_dir/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
echo "worker done"
EOF
  chmod +x "$bin_dir/codex"

  HOME="$home_dir" NVM_DIR="$home_dir/.nvm" PATH="$bin_dir:/usr/bin:/bin" \
    "$ROOT/scripts/multi-agent-delegate.sh" \
    --to codex \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --prompt "Auto verify run" >/dev/null

  assert_one_artifact_contains "$artifact_dir" 'codex-auto-verify-run-*.md' 'check-contract-ran'

  HOME="$home_dir" NVM_DIR="$home_dir/.nvm" PATH="$bin_dir:/usr/bin:/bin" \
    "$ROOT/scripts/multi-agent-delegate.sh" \
    --to codex \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --no-verify \
    --prompt "No verify run" >/dev/null

  artifact="$(find "$artifact_dir" -type f -name 'codex-no-verify-run-*.md' | head -n 1)"
  [ -n "$artifact" ] || fail "missing no-verify artifact"
  if grep -Fq '## Verify' "$artifact"; then
    fail "--no-verify should skip verification section"
  fi
}

test_project_doctor_ok_after_apply() {
  local project="$TMP/doctor-ok"
  mkdir -p "$project"

  "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null

  out="$("$ROOT/scripts/project-doctor.sh" "$project")" ||
    fail "project-doctor should pass on freshly applied project: $out"
  printf '%s' "$out" | grep -Fq 'ml block identical' ||
    fail "project-doctor should confirm identical ml blocks"
  printf '%s' "$out" | grep -Fq 'matches current template' ||
    fail "project-doctor should confirm template freshness"
}

test_project_doctor_detects_drift() {
  local project="$TMP/doctor-drift"
  mkdir -p "$project"

  "$ROOT/scripts/apply-project-template.sh" general "$project" >/dev/null
  sed -i 's/Project rules override global defaults./TAMPERED/' "$project/CLAUDE.md"

  if out="$("$ROOT/scripts/project-doctor.sh" "$project")"; then
    fail "project-doctor should fail on drifted CLAUDE.md block"
  fi
  printf '%s' "$out" | grep -Fq 'differs between AGENTS.md and CLAUDE.md' ||
    fail "project-doctor should report block drift"
}

test_project_doctor_detects_missing_block() {
  local project="$TMP/doctor-missing"
  mkdir -p "$project"

  "$ROOT/scripts/apply-project-template.sh" general "$project" >/dev/null
  "$ROOT/scripts/remove-project-template.sh" general "$project" CLAUDE.md >/dev/null

  if out="$("$ROOT/scripts/project-doctor.sh" "$project")"; then
    fail "project-doctor should fail when CLAUDE.md block is missing"
  fi
  printf '%s' "$out" | grep -Fq 'missing in CLAUDE.md' ||
    fail "project-doctor should report missing CLAUDE.md block"
}

test_project_doctor_detects_stale_block() {
  local project="$TMP/doctor-stale"
  mkdir -p "$project"

  "$ROOT/scripts/apply-project-template.sh" general "$project" >/dev/null
  sed -i 's/Project rules override global defaults./OLD RULE/' \
    "$project/AGENTS.md" "$project/CLAUDE.md"

  if out="$("$ROOT/scripts/project-doctor.sh" "$project")"; then
    fail "project-doctor should fail on stale blocks"
  fi
  printf '%s' "$out" | grep -Fq 'stale' ||
    fail "project-doctor should report stale block"
}

test_detect_configs_only_is_general() {
  local project="$TMP/detect-configs-only"
  mkdir -p "$project/configs"
  printf 'port: 8080\n' > "$project/configs/app.yaml"
  printf 'name: app\n' > "$project/config.yaml"

  style="$("$ROOT/scripts/detect-project-style.sh" "$project")"
  [ "$style" = "general" ] || fail "expected general for configs-only project, got $style"
}

test_detect_ml_filename_is_ml() {
  local project="$TMP/detect-ml-filename"
  mkdir -p "$project"
  touch "$project/train.py"

  style="$("$ROOT/scripts/detect-project-style.sh" "$project")"
  [ "$style" = "ml" ] || fail "expected ml for train.py project, got $style"
}

test_detect_ml_code_text_is_ml() {
  local project="$TMP/detect-ml-code"
  mkdir -p "$project"
  printf 'import torch\n' > "$project/main.py"

  style="$("$ROOT/scripts/detect-project-style.sh" "$project")"
  [ "$style" = "ml" ] || fail "expected ml for torch code, got $style"
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

  count="$(find "$artifact_dir" -type f -name '*.md' ! -name '_synthesis-*' | wc -l)"
  [ "$count" = "3" ] || fail "expected three review artifacts, got $count"
  assert_one_artifact_contains "$artifact_dir" 'codex-review-current-diff-*.md' 'DRY RUN'
  assert_one_artifact_contains "$artifact_dir" 'claude-review-current-diff-*.md' 'Question:'
  assert_one_artifact_contains "$artifact_dir" 'antigravity-review-current-diff-*.md' 'Diff:'
  assert_one_artifact_contains "$artifact_dir" '_synthesis-review-current-diff-*.md' 'Multi-agent review synthesis'
  assert_one_artifact_contains "$artifact_dir" '_synthesis-review-current-diff-*.md' '## codex'
}


test_multi_agent_review_base_ref_diff() {
  local project="$TMP/review-base-ref"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project"
  git -C "$project" init >/dev/null
  printf 'one\n' > "$project/file.txt"
  git -C "$project" add file.txt
  git -C "$project" \
    -c user.email=test@example.com \
    -c user.name='Test User' \
    commit -m first >/dev/null
  printf 'two\n' > "$project/file.txt"
  git -C "$project" add file.txt
  git -C "$project" \
    -c user.email=test@example.com \
    -c user.name='Test User' \
    commit -m second >/dev/null

  OH_MY_SETTING_REVIEW_DRY_RUN=1 "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --base HEAD~1 \
    --artifact-dir "$artifact_dir" \
    --providers codex \
    --prompt "Review base ref" >/dev/null

  assert_one_artifact_contains "$artifact_dir" 'codex-review-base-ref-*.md' '-one'
  assert_one_artifact_contains "$artifact_dir" 'codex-review-base-ref-*.md' '+two'
}

test_multi_agent_review_invalid_base_fails() {
  local project="$TMP/review-bad-base"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project"
  git -C "$project" init >/dev/null

  if OH_MY_SETTING_REVIEW_DRY_RUN=1 "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --base no-such-ref \
    --artifact-dir "$artifact_dir" \
    --prompt "Review invalid base" >/dev/null 2>"$project/error"; then
    fail "invalid --base ref should fail"
  fi

  assert_file_contains "$project/error" 'invalid --base ref'
}

test_multi_agent_review_synthesize_dry_run() {
  local project="$TMP/review-synthesize"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project"
  git -C "$project" init >/dev/null

  OH_MY_SETTING_REVIEW_DRY_RUN=1 "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --providers codex \
    --no-diff \
    --synthesize \
    --prompt "Review synthesize mode" >/dev/null

  assert_one_artifact_contains "$artifact_dir" '_synthesis-review-synthesize-mode-*.md' '## Synthesis (claude)'
  assert_one_artifact_contains "$artifact_dir" '_synthesis-review-synthesize-mode-*.md' 'DRY RUN: synthesis pass skipped.'
}

test_multi_agent_review_synthesize_provider_override() {
  local project="$TMP/review-synthesize-codex"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project"
  git -C "$project" init >/dev/null

  OH_MY_SETTING_REVIEW_DRY_RUN=1 "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --providers codex \
    --no-diff \
    --synthesize codex \
    --prompt "Review synthesize override" >/dev/null

  assert_one_artifact_contains "$artifact_dir" '_synthesis-review-synthesize-override-*.md' '## Synthesis (codex)'
}

test_multi_agent_review_debate_dry_run() {
  local project="$TMP/review-debate"
  local artifact_dir="$project/artifacts"
  local count

  mkdir -p "$project"
  git -C "$project" init >/dev/null

  OH_MY_SETTING_REVIEW_DRY_RUN=1 "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --providers codex,claude \
    --no-diff \
    --debate 1 \
    --prompt "Review with debate" >/dev/null

  count="$(find "$artifact_dir" -type f -name '*-r2.md' | wc -l)"
  [ "$count" = "2" ] || fail "expected two round-2 review artifacts, got $count"
  assert_one_artifact_contains "$artifact_dir" 'codex-review-with-debate-*-r2.md' 'Other reviewers:'
  assert_one_artifact_contains "$artifact_dir" 'claude-review-with-debate-*-r2.md' 'Remaining disagreements:'
  assert_one_artifact_contains "$artifact_dir" '_synthesis-review-with-debate-*.md' 'debate rounds: 1'
}

test_multi_agent_review_ml_preset() {
  local project="$TMP/review-ml-preset"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project"
  git -C "$project" init >/dev/null

  OH_MY_SETTING_REVIEW_DRY_RUN=1 "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --providers codex \
    --no-diff \
    --ml >/dev/null

  assert_one_artifact_contains "$artifact_dir" 'codex-*.md' 'Data leakage'
  assert_one_artifact_contains "$artifact_dir" 'codex-*.md' 'sampler.set_epoch'
  assert_one_artifact_contains "$artifact_dir" 'codex-*.md' 'silent ML bugs'
}

test_multi_agent_review_default_prompt_requires_ml() {
  local project="$TMP/review-no-prompt"
  mkdir -p "$project"
  git -C "$project" init >/dev/null

  if OH_MY_SETTING_REVIEW_DRY_RUN=1 "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --artifact-dir "$project/artifacts" \
    --no-diff >/dev/null 2>"$project/error"; then
    fail "review without --prompt and without --ml should fail"
  fi
  assert_file_contains "$project/error" '--prompt is required'
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

  count="$(find "$artifact_dir" -type f -name '*.md' ! -name '_synthesis-*' | wc -l)"
  [ "$count" = "1" ] || fail "expected one review artifact, got $count"
  assert_one_artifact_contains "$artifact_dir" 'antigravity-review-no-diff-mode-*.md' 'Git context omitted by --no-diff.'
  assert_one_artifact_contains "$artifact_dir" '_synthesis-review-no-diff-mode-*.md' '## antigravity'
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

  count="$(find "$artifact_dir" -type f -name '*.md' ! -name '_synthesis-*' | wc -l)"
  [ "$count" = "3" ] || fail "expected three ask artifacts, got $count"
  assert_one_artifact_contains "$artifact_dir" 'codex-compare-two-implementation-options-*.md' 'Repository context: omitted.'
  assert_one_artifact_contains "$artifact_dir" 'claude-compare-two-implementation-options-*.md' 'DRY RUN'
  assert_one_artifact_contains "$artifact_dir" 'antigravity-compare-two-implementation-options-*.md' 'Answer:'
  assert_one_artifact_contains "$artifact_dir" '_synthesis-compare-two-implementation-options-*.md' 'Multi-agent ask synthesis'
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

  count="$(find "$artifact_dir" -type f -name '*.md' ! -name '_synthesis-*' | wc -l)"
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

test_multi_agent_review_print_timeout() {
  local project="$TMP/review-timeout"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project"
  git -C "$project" init >/dev/null

  OH_MY_SETTING_REVIEW_DRY_RUN=1 "$ROOT/scripts/multi-agent-review.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --print-timeout 10m \
    --prompt "Review with custom print timeout" >/dev/null
}

test_multi_agent_ask_print_timeout() {
  local project="$TMP/ask-timeout"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project"

  OH_MY_SETTING_ASK_DRY_RUN=1 "$ROOT/scripts/multi-agent-ask.sh" \
    --artifact-dir "$artifact_dir" \
    --print-timeout 10m \
    --prompt "Ask with custom print timeout" >/dev/null
}

test_multi_agent_ask_debate_dry_run() {
  local project="$TMP/ask-debate"
  local artifact_dir="$project/artifacts"
  local count

  mkdir -p "$project"
  OH_MY_SETTING_ASK_DRY_RUN=1 "$ROOT/scripts/multi-agent-ask.sh" \
    --artifact-dir "$artifact_dir" \
    --debate 1 \
    --prompt "Debate two options" >/dev/null

  count="$(find "$artifact_dir" -type f -name '*.md' ! -name '_synthesis-*' | wc -l)"
  [ "$count" = "6" ] || fail "expected six debate artifacts (3 round1 + 3 round2), got $count"
  assert_one_artifact_contains "$artifact_dir" 'codex-debate-two-options-*-r2.md' 'Your previous answer:'
  assert_one_artifact_contains "$artifact_dir" 'codex-debate-two-options-*-r2.md' 'Other advisors:'
  assert_one_artifact_contains "$artifact_dir" 'claude-debate-two-options-*-r2.md' 'debate round 2'
  assert_one_artifact_contains "$artifact_dir" '_synthesis-debate-two-options-*.md' 'debate rounds: 1'
  assert_one_artifact_contains "$artifact_dir" '_synthesis-debate-two-options-*.md' '_final answer after debate_'
}

test_multi_agent_ask_debate_needs_two_providers() {
  local project="$TMP/ask-debate-single"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project"
  OH_MY_SETTING_ASK_DRY_RUN=1 "$ROOT/scripts/multi-agent-ask.sh" \
    --artifact-dir "$artifact_dir" \
    --providers codex \
    --debate 1 \
    --prompt "Debate alone" >/dev/null 2>"$project/error"

  assert_file_contains "$project/error" 'fewer than two active providers'
  if find "$artifact_dir" -type f -name '*-r2.md' | grep -q .; then
    fail "single-provider debate should not produce round-2 artifacts"
  fi
}

test_multi_agent_ask_rejects_bad_debate_count() {
  local project="$TMP/ask-debate-bad"
  mkdir -p "$project"

  if OH_MY_SETTING_ASK_DRY_RUN=1 "$ROOT/scripts/multi-agent-ask.sh" \
    --artifact-dir "$project/artifacts" \
    --debate 9 \
    --prompt "Debate too long" >/dev/null 2>"$project/error"; then
    fail "--debate 9 should fail"
  fi
  assert_file_contains "$project/error" '--debate must be 1-3'
}

make_committed_repo() {
  local project="$1"
  mkdir -p "$project"
  git -C "$project" init >/dev/null
  printf 'base\n' > "$project/file.txt"
  git -C "$project" add file.txt
  git -C "$project" \
    -c user.email=test@example.com \
    -c user.name='Test User' \
    commit -m init >/dev/null
}

test_delegate_dry_run() {
  local project="$TMP/delegate-dry"
  local artifact_dir="$project/artifacts"

  make_committed_repo "$project"

  OH_MY_SETTING_DELEGATE_DRY_RUN=1 "$ROOT/scripts/multi-agent-delegate.sh" \
    --to codex \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --prompt "Add a helper" >/dev/null

  assert_one_artifact_contains "$artifact_dir" 'codex-add-a-helper-*.md' 'Do not run git commit'
  assert_one_artifact_contains "$artifact_dir" 'codex-add-a-helper-*.md' 'DRY RUN: worker command skipped.'
  local patch
  patch="$(find "$artifact_dir" -type f -name 'codex-add-a-helper-*.patch' | head -n 1)"
  [ -n "$patch" ] || fail "missing delegate patch artifact"
  [ ! -s "$patch" ] || fail "dry-run patch should be empty"
  [ -z "$(git -C "$project" status --porcelain file.txt)" ] || fail "delegate dry-run touched main tree"
}

test_delegate_fake_worker_apply() {
  local project="$TMP/delegate-apply"
  local artifact_dir="$project/artifacts"
  local bin_dir="$project/bin"
  local home_dir="$project/home"

  make_committed_repo "$project"
  mkdir -p "$bin_dir" "$home_dir"
  cat > "$bin_dir/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
printf 'hello from worker\n' > delegated.txt
echo "worker done"
EOF
  chmod +x "$bin_dir/codex"

  HOME="$home_dir" NVM_DIR="$home_dir/.nvm" PATH="$bin_dir:/usr/bin:/bin" \
    "$ROOT/scripts/multi-agent-delegate.sh" \
    --to codex \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --apply \
    --prompt "Create delegated file" >/dev/null

  [ -f "$project/delegated.txt" ] || fail "applied patch should create delegated.txt in main tree"
  assert_file_contains "$project/delegated.txt" "hello from worker"
  assert_one_artifact_contains "$artifact_dir" 'codex-create-delegated-file-*.md' 'worker done'
}

test_delegate_apply_refuses_dirty_tree() {
  local project="$TMP/delegate-dirty"
  local artifact_dir="$project/artifacts"
  local bin_dir="$project/bin"
  local home_dir="$project/home"

  make_committed_repo "$project"
  mkdir -p "$bin_dir" "$home_dir"
  cat > "$bin_dir/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
printf 'x\n' > delegated.txt
EOF
  chmod +x "$bin_dir/codex"
  printf 'dirty\n' >> "$project/file.txt"

  if HOME="$home_dir" NVM_DIR="$home_dir/.nvm" PATH="$bin_dir:/usr/bin:/bin" \
    "$ROOT/scripts/multi-agent-delegate.sh" \
    --to codex \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --apply \
    --prompt "Apply on dirty tree" >/dev/null 2>"$project/error"; then
    fail "--apply on dirty main tree should fail"
  fi

  assert_file_contains "$project/error" 'refusing --apply'
  [ ! -f "$project/delegated.txt" ] || fail "dirty-tree apply should not modify main tree"
}

test_delegate_requires_provider() {
  local project="$TMP/delegate-no-provider"
  make_committed_repo "$project"

  if "$ROOT/scripts/multi-agent-delegate.sh" \
    --repo "$project" \
    --prompt "No provider" >/dev/null 2>"$project/error"; then
    fail "delegate without --to should fail"
  fi
  assert_file_contains "$project/error" '--to is required'
}

test_agent_memory_append_show_and_rejects_sensitive() {
  local project="$TMP/agent-memory"
  local home_dir="$project/home"
  mkdir -p "$project" "$home_dir"

  HOME="$home_dir" "$ROOT/scripts/agent-memory.sh" \
    --repo "$project" append --agent codex --text "Prefer scripts/check.sh fast before done." >/dev/null

  [ -f "$project/.oms/memory/shared.md" ] || fail "project memory file missing"
  [ -f "$project/.oms/memory/summary.md" ] || fail "compact memory summary missing"
  assert_file_contains "$project/.oms/memory/shared.md" "Prefer scripts/check.sh fast before done."
  assert_file_contains "$project/.oms/memory/summary.md" "Prefer scripts/check.sh fast before done."

  HOME="$home_dir" "$ROOT/scripts/agent-memory.sh" \
    --repo "$project" pin --agent codex --text "Current task context should stay compact." >/dev/null
  [ -f "$project/.oms/memory/pins.md" ] || fail "pinned memory file missing"
  assert_file_contains "$project/.oms/memory/pins.md" "Current task context should stay compact."

  HOME="$home_dir" "$ROOT/scripts/agent-memory.sh" \
    --repo "$project" show >"$project/out"
  assert_file_contains "$project/out" "Shared Agent Memory"

  HOME="$home_dir" "$ROOT/scripts/agent-memory.sh" \
    --repo "$project" context >"$project/context"
  assert_file_contains "$project/context" "Compact recent:"
  assert_file_contains "$project/context" "Pinned:"
  if grep -Fq "Shared Agent Memory" "$project/context"; then
    fail "compact context should not include full shared.md header"
  fi

  if HOME="$home_dir" "$ROOT/scripts/agent-memory.sh" \
    --repo "$project" append --agent codex --text "private path /hom""e/jaemin/secret" \
    >"$project/sensitive-out" 2>"$project/sensitive-err"; then
    fail "sensitive-looking memory note should be rejected"
  fi
  assert_file_contains "$project/sensitive-err" "sensitive-looking content"
}


test_agent_task_init_context_and_rejects_sensitive() {
  local project="$TMP/agent-task"
  mkdir -p "$project"

  "$ROOT/scripts/agent-task.sh" \
    --repo "$project" \
    init \
    --agent codex \
    --goal "Implement an ML-focused agent harness" \
    --constraint "Keep provider context compact" \
    --done "agent-task context is available" \
    --verify "bash scripts/check.sh ml-smoke" \
    --decision "Use active task packet before larger task registry" \
    --state "Memory and agent-run already exist" \
    --next "Wire task context into provider calls" >/dev/null

  [ -f "$project/.oms/task/current.md" ] || fail "task file missing"
  assert_file_contains "$project/.oms/task/current.md" "Implement an ML-focused agent harness"
  assert_file_contains "$project/.oms/task/current.md" "Use active task packet before larger task registry"

  "$ROOT/scripts/agent-task.sh" --repo "$project" context >"$project/context"
  assert_file_contains "$project/context" "Active task packet follows"
  assert_file_contains "$project/context" "Wire task context into provider calls"

  if "$ROOT/scripts/agent-task.sh" \
    --repo "$project" append --text "private path /hom""e/jaemin/secret" \
    >"$project/sensitive-out" 2>"$project/sensitive-err"; then
    fail "sensitive-looking task note should be rejected"
  fi
  assert_file_contains "$project/sensitive-err" "sensitive-looking content"
}

test_agent_call_outbound_scrubber_blocks_private_path() {
  local project="$TMP/agent-call-scrub"
  local artifact_dir="$project/artifacts"
  local artifact

  mkdir -p "$project"
  if OH_MY_SETTING_CALL_DRY_RUN=1 "$ROOT/scripts/agent-call.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --to codex \
    --prompt "Assess private path /hom""e/jaemin/secret" \
    >"$project/out" 2>"$project/error"; then
    fail "private-path outbound prompt should be blocked"
  fi

  assert_file_contains "$project/error" "outbound provider context contains sensitive-looking content"
  artifact="$(find "$artifact_dir" -type f -name '*.md' | head -n 1)"
  [ -n "$artifact" ] || fail "blocked call should write safe artifact"
  assert_file_contains "$artifact" "SKIPPED: outbound provider context contains sensitive-looking content"
  if grep -Fq "Assess private path" "$artifact"; then
    fail "blocked artifact should not contain prompt text"
  fi
}

test_agent_ml_context_digest() {
  local project="$TMP/ml-context"
  mkdir -p "$project/scripts" "$project/docs"
  printf 'import torch\n' > "$project/train.py"
  cat > "$project/scripts/check.sh" <<'EOF'
#!/usr/bin/env bash
case "${1:-fast}" in
  fast) echo fast ;;
  ml-smoke) echo ml-smoke ;;
  *) exit 2 ;;
esac
EOF
  chmod +x "$project/scripts/check.sh"
  printf '%s\n' '{"ts":"2026-06-10T00:00:00Z","git_sha":"abc1234","dirty":0,"exit":0,"duration_s":2,"note":"smoke","cmd":["python","train.py"]}' > "$project/docs/EXPERIMENTS.jsonl"

  "$ROOT/scripts/agent-ml-context.sh" --repo "$project" >"$project/context"
  assert_file_contains "$project/context" "ML Agent Context Digest"
  assert_file_contains "$project/context" "train.py"
  assert_file_contains "$project/context" "preferred ML smoke: bash scripts/check.sh ml-smoke"
  assert_file_contains "$project/context" "exit=0"
  [ "$(head -n 1 "$project/context")" = "# ML Agent Context Digest" ] ||
    fail "digest header must be the first output line (ledger rows leaked before header)"
  awk '/## Recent Experiments/{f=1} f && /exit=0/{found=1} END{exit !found}' "$project/context" ||
    fail "ledger row must appear inside Recent Experiments section"
}

test_delegate_auto_verify_prefers_ml_smoke() {
  local project="$TMP/delegate-ml-smoke"
  local artifact_dir="$project/artifacts"
  local bin_dir="$project/bin"
  local home_dir="$project/home"

  make_committed_repo "$project"
  mkdir -p "$project/scripts" "$bin_dir" "$home_dir"
  printf 'import torch\n' > "$project/train.py"
  cat > "$project/scripts/check.sh" <<'EOF'
#!/usr/bin/env bash
case "${1:-fast}" in
  fast) echo fast-ran ;;
  ml-smoke) echo ml-smoke-ran ;;
  *) exit 2 ;;
esac
EOF
  chmod +x "$project/scripts/check.sh"
  git -C "$project" add train.py scripts/check.sh
  git -C "$project" -c user.email=t@e.c -c user.name=T commit -qm ml-smoke

  cat > "$bin_dir/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
echo "worker done"
EOF
  chmod +x "$bin_dir/codex"

  HOME="$home_dir" NVM_DIR="$home_dir/.nvm" PATH="$bin_dir:/usr/bin:/bin" \
    "$ROOT/scripts/multi-agent-delegate.sh" \
    --to codex \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --prompt "Auto verify ml smoke" >/dev/null

  assert_one_artifact_contains "$artifact_dir" 'codex-auto-verify-ml-smoke-*.md' 'command: bash scripts/check.sh ml-smoke'
  assert_one_artifact_contains "$artifact_dir" 'codex-auto-verify-ml-smoke-*.md' 'ml-smoke-ran'
}


test_agent_call_dry_run_attaches_shared_memory() {
  local project="$TMP/agent-call"
  local home_dir="$project/home"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project" "$home_dir"
  HOME="$home_dir" "$ROOT/scripts/agent-memory.sh" \
    --repo "$project" append --agent codex --text "Prefer narrow verification commands." >/dev/null

  HOME="$home_dir" OH_MY_SETTING_CALL_DRY_RUN=1 "$ROOT/scripts/agent-call.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --to codex \
    --prompt "Assess this plan" >/dev/null

  assert_one_artifact_contains "$artifact_dir" 'codex-assess-this-plan-*.md' 'compact mode'
  assert_one_artifact_contains "$artifact_dir" 'codex-assess-this-plan-*.md' 'Prefer narrow verification commands.'
  assert_one_artifact_contains "$artifact_dir" 'codex-assess-this-plan-*.md' 'DRY RUN: provider command skipped.'
}


test_agent_run_auto_read_routes_to_call() {
  local project="$TMP/agent-run-read"
  local home_dir="$project/home"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project" "$home_dir"
  HOME="$home_dir" "$ROOT/scripts/agent-memory.sh" \
    --repo "$project" append --agent codex --text "Prefer narrow verification commands." >/dev/null

  HOME="$home_dir" OH_MY_SETTING_AGENT_RUN_DRY_RUN=1 "$ROOT/scripts/agent-run.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --to claude \
    --prompt "Assess this plan" >/dev/null

  assert_one_artifact_contains "$artifact_dir" 'claude-assess-this-plan-*.md' 'independent read-only pass'
  assert_one_artifact_contains "$artifact_dir" 'claude-assess-this-plan-*.md' 'Compact recent:'
  assert_one_artifact_contains "$artifact_dir" 'claude-assess-this-plan-*.md' 'Prefer narrow verification commands.'
}


test_agent_run_auto_write_routes_to_delegate() {
  local project="$TMP/agent-run-write"
  local home_dir="$project/home"
  local artifact_dir="$project/artifacts"
  local patch

  make_committed_repo "$project"
  mkdir -p "$home_dir"

  HOME="$home_dir" OH_MY_SETTING_AGENT_RUN_DRY_RUN=1 "$ROOT/scripts/agent-run.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --to codex \
    --prompt "Implement a helper" >/dev/null

  assert_one_artifact_contains "$artifact_dir" 'codex-implement-a-helper-*.md' 'delegated worker agent'
  assert_one_artifact_contains "$artifact_dir" 'codex-implement-a-helper-*.md' 'DRY RUN: worker command skipped.'
  patch="$(find "$artifact_dir" -type f -name 'codex-implement-a-helper-*.patch' | head -n 1)"
  [ -n "$patch" ] || fail "agent-run write mode should create patch artifact"
  [ ! -s "$patch" ] || fail "dry-run write patch should be empty"
}

test_scrubber_passes_harness_sources() {
  local dir="$TMP/scrubber-self"
  local bundle="$dir/bundle"

  mkdir -p "$dir"
  # Self-review regression gate: anything that can enter diff/prompt context
  # must pass the outbound scrubber. Env examples and generated cluster refs
  # are excluded here exactly like MA_SAFE_PATHS excludes them from diffs.
  (cd "$ROOT" && git ls-files -z -- '*.sh' '*.md' '*.json' ':(exclude).env*' | xargs -0 cat) > "$bundle"
  if bash -c ". '$ROOT/scripts/lib/agent-memory-common.sh'; agent_memory_file_has_sensitive_content '$bundle'"; then
    bash -c ". '$ROOT/scripts/lib/agent-memory-common.sh'; grep -Ein \"\$(agent_memory_sensitive_re)\" '$bundle' | head -n 5" >&2 || true
    fail "harness sources must pass the outbound scrubber (self-review regression)"
  fi
}

test_scrubber_blocks_env_style_token() {
  local project="$TMP/scrub-env-token"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project"
  if OH_MY_SETTING_CALL_DRY_RUN=1 "$ROOT/scripts/agent-call.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --to codex \
    --prompt "Use GITHUB_TOK""EN=ghx-not-real-1234 for CI" \
    >"$project/out" 2>"$project/error"; then
    fail "env-style token outbound prompt should be blocked"
  fi
  assert_file_contains "$project/error" "outbound provider context contains sensitive-looking content"
}

test_scrubber_no_function_name_bypass() {
  local project="$TMP/scrub-bypass"
  local artifact_dir="$project/artifacts"

  mkdir -p "$project"
  # Regression: the old line-level exclusion skipped any line mentioning
  # scrubber symbols, letting secrets ride along on those lines.
  if OH_MY_SETTING_CALL_DRY_RUN=1 "$ROOT/scripts/agent-call.sh" \
    --repo "$project" \
    --artifact-dir "$artifact_dir" \
    --to codex \
    --prompt "contains_sensitive_content notes /hom""e/jaemin/secret" \
    >"$project/out" 2>"$project/error"; then
    fail "scrubber symbol on the same line must not bypass the block"
  fi
  assert_file_contains "$project/error" "outbound provider context contains sensitive-looking content"
}

test_agent_run_read_priority_routing() {
  local project="$TMP/agent-run-read-priority"
  local home_dir="$project/home"
  local artifact_dir="$project/artifacts"

  make_committed_repo "$project"
  mkdir -p "$home_dir"

  HOME="$home_dir" OH_MY_SETTING_AGENT_RUN_DRY_RUN=1 "$ROOT/scripts/agent-run.sh" \
    --repo "$project" --artifact-dir "$artifact_dir" --to claude \
    --prompt "Review the latest fix and report findings" >/dev/null 2>"$project/route1"
  assert_file_contains "$project/route1" "resolved=read"

  HOME="$home_dir" OH_MY_SETTING_AGENT_RUN_DRY_RUN=1 "$ROOT/scripts/agent-run.sh" \
    --repo "$project" --artifact-dir "$artifact_dir" --to claude \
    --prompt "Summarize committed changes in this update" >/dev/null 2>"$project/route2"
  assert_file_contains "$project/route2" "resolved=read"

  HOME="$home_dir" OH_MY_SETTING_AGENT_RUN_DRY_RUN=1 "$ROOT/scripts/agent-run.sh" \
    --repo "$project" --artifact-dir "$artifact_dir" --to codex \
    --prompt "Refactor the parser module" >/dev/null 2>"$project/route3"
  assert_file_contains "$project/route3" "resolved=write"
}


test_link_and_unlink_with_home_override() {
  local home_dir="$TMP/link-home"
  mkdir -p "$home_dir/.codex/skills" "$home_dir/.agents/skills" \
    "$home_dir/.pi/agent/skills" "$home_dir/.gemini" "$home_dir/old-skills"
  ln -s "$home_dir/old-skills/multi-agent-ask" \
    "$home_dir/.codex/skills/multi-agent-ask"
  ln -s "$home_dir/old-skills/multi-agent-review" \
    "$home_dir/.codex/skills/multi-agent-review.backup.legacy"
  ln -s "$ROOT/custom-skills" "$home_dir/.codex/skills/oh-my-setting"
  ln -s "$ROOT/custom-skills/multi-agent-ask" \
    "$home_dir/.agents/skills/multi-agent-ask"
  ln -s "$ROOT/custom-skills/multi-agent-review" \
    "$home_dir/.pi/agent/skills/multi-agent-review"
  ln -s "$ROOT/AGENTS.md" "$home_dir/.gemini/GEMINI.md"

  HOME="$home_dir" "$ROOT/scripts/link.sh" >/dev/null

  [ -L "$home_dir/.codex/AGENTS.md" ] || fail "codex AGENTS.md not linked"
  [ -L "$home_dir/.claude/CLAUDE.md" ] || fail "claude CLAUDE.md not linked"
  [ -L "$home_dir/.gemini/AGENTS.md" ] || fail "gemini AGENTS.md not linked"
  [ -L "$home_dir/.gemini/antigravity/skills/spec-interview" ] ||
    fail "antigravity skills not linked"
  [ "$(readlink "$home_dir/.codex/skills/multi-agent-ask")" = \
    "$ROOT/custom-skills/multi-agent-ask" ] ||
    fail "stale skill symlink not replaced"
  if find "$home_dir/.codex/skills" -maxdepth 1 -name "*.backup.*" | grep -q .; then
    fail "backup skill symlink not cleaned"
  fi
  [ ! -e "$home_dir/.codex/skills/oh-my-setting" ] ||
    fail "legacy grouped skill symlink not removed"
  [ ! -e "$home_dir/.agents/skills/multi-agent-ask" ] ||
    fail "legacy .agents skill symlink not removed"
  [ ! -e "$home_dir/.pi/agent/skills/multi-agent-review" ] ||
    fail "legacy pi skill symlink not removed"
  [ ! -e "$home_dir/.gemini/GEMINI.md" ] ||
    fail "legacy gemini file not removed"

  HOME="$home_dir" "$ROOT/scripts/unlink.sh" >/dev/null

  [ ! -e "$home_dir/.gemini/AGENTS.md" ] || fail "gemini AGENTS.md not unlinked"
  [ ! -e "$home_dir/.gemini/antigravity/skills/spec-interview" ] ||
    fail "antigravity skills not unlinked"
}


test_skill_doctor_detects_duplicate_names() {
  local home_dir="$TMP/skill-doctor-dup"
  mkdir -p "$home_dir/.codex/skills/a" "$home_dir/.codex/skills/b"

  cat > "$home_dir/.codex/skills/a/SKILL.md" <<'EOF'
---
name: duplicate-skill
---
EOF
  cat > "$home_dir/.codex/skills/b/SKILL.md" <<'EOF'
---
name: duplicate-skill
---
EOF

  if HOME="$home_dir" "$ROOT/scripts/skill-doctor.sh" >"$home_dir/out" 2>&1; then
    fail "skill-doctor should fail on duplicate names"
  fi
  assert_file_contains "$home_dir/out" "duplicate skill name: duplicate-skill"
}

test_cleanup_dry_run_and_apply() {
  local home_dir="$TMP/cleanup-home"
  mkdir -p "$home_dir/.codex/skills" "$home_dir/.agents/skills" \
    "$home_dir/.pi/agent/skills" "$home_dir/.gemini"
  ln -s "$ROOT/custom-skills/multi-agent-ask" \
    "$home_dir/.codex/skills/multi-agent-ask.backup.legacy"
  ln -s "$ROOT/custom-skills/multi-agent-review" \
    "$home_dir/.agents/skills/multi-agent-review"
  ln -s "$ROOT/custom-skills/spec-interview" \
    "$home_dir/.pi/agent/skills/spec-interview"
  ln -s "$ROOT/AGENTS.md" "$home_dir/.gemini/GEMINI.md"

  HOME="$home_dir" "$ROOT/scripts/cleanup.sh" --dry-run >"$home_dir/dry-run"
  assert_file_contains "$home_dir/dry-run" "cleanup: 4 removable item(s) found"
  [ -e "$home_dir/.codex/skills/multi-agent-ask.backup.legacy" ] ||
    fail "dry-run removed backup symlink"

  HOME="$home_dir" "$ROOT/scripts/cleanup.sh" --apply >"$home_dir/apply"
  [ ! -e "$home_dir/.codex/skills/multi-agent-ask.backup.legacy" ] ||
    fail "cleanup did not remove backup symlink"
  [ ! -e "$home_dir/.agents/skills/multi-agent-review" ] ||
    fail "cleanup did not remove legacy .agents skill"
  [ ! -e "$home_dir/.pi/agent/skills/spec-interview" ] ||
    fail "cleanup did not remove legacy pi skill"
  [ ! -e "$home_dir/.gemini/GEMINI.md" ] ||
    fail "cleanup did not remove legacy gemini file"
  assert_file_contains "$home_dir/apply" "skill-doctor: ok"
}

test_update_help_runs() {
  "$ROOT/scripts/update.sh" --help >/dev/null
}

test_uninstall_help_runs() {
  "$ROOT/scripts/uninstall.sh" --help >/dev/null
}

test_uninstall_dry_run_no_changes() {
  local home_dir="$TMP/uninstall-home"
  mkdir -p "$home_dir"
  HOME="$home_dir" OH_MY_SETTING_DRY_RUN=1 \
    "$ROOT/scripts/uninstall.sh" --dry-run --yes >/dev/null

  [ -z "$(find "$home_dir" -mindepth 1 -print -quit 2>/dev/null)" ] ||
    fail "uninstall dry-run created files under $home_dir"
}

test_version_file_present() {
  [ -f "$ROOT/VERSION" ] || fail "VERSION file missing"
  local version
  version="$(head -n 1 "$ROOT/VERSION")"
  case "$version" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) fail "VERSION not semver-like: $version" ;;
  esac
}

test_status_shows_version() {
  local out
  out="$("$ROOT/scripts/status.sh" 2>/dev/null)"
  printf '%s' "$out" | grep -Eq '^- version: [0-9]' || fail "status.sh missing version line"
}

test_apply_dry_run_has_no_writes
test_apply_ml_dry_run_has_no_writes
test_apply_ml_scaffolds_docs
test_apply_ml_scaffolds_gitignore
test_apply_ml_scaffolds_check_contract
test_project_doctor_warns_missing_check
test_job_digest_log_mode
test_run_ledger_records_and_lists
test_delegate_auto_verify_uses_check_contract
test_project_doctor_ok_after_apply
test_project_doctor_detects_drift
test_project_doctor_detects_missing_block
test_project_doctor_detects_stale_block
test_apply_rejects_unclosed_managed_block
test_remove_rejects_unclosed_managed_block
test_detect_configs_only_is_general
test_detect_ml_filename_is_ml
test_detect_ml_code_text_is_ml
test_detect_ignores_common_generated_dirs
test_apply_and_remove_valid_block
test_multi_agent_review_dry_run_artifacts
test_multi_agent_review_base_ref_diff
test_multi_agent_review_invalid_base_fails
test_multi_agent_review_synthesize_dry_run
test_multi_agent_review_synthesize_provider_override
test_multi_agent_review_ml_preset
test_multi_agent_review_default_prompt_requires_ml
test_multi_agent_review_debate_dry_run
test_multi_agent_review_excludes_private_status
test_multi_agent_review_secret_diff_skips_external
test_multi_agent_review_no_diff_provider_subset
test_multi_agent_review_rejects_unknown_provider
test_multi_agent_review_single_provider_failure_exits
test_multi_agent_ask_dry_run_no_repo
test_multi_agent_ask_repo_context_subset
test_multi_agent_ask_secret_diff_skips_external
test_multi_agent_review_print_timeout
test_multi_agent_ask_print_timeout
test_multi_agent_ask_debate_dry_run
test_multi_agent_ask_debate_needs_two_providers
test_multi_agent_ask_rejects_bad_debate_count
test_delegate_dry_run
test_delegate_fake_worker_apply
test_delegate_apply_refuses_dirty_tree
test_delegate_requires_provider
test_agent_memory_append_show_and_rejects_sensitive
test_agent_task_init_context_and_rejects_sensitive
test_agent_call_outbound_scrubber_blocks_private_path
test_agent_ml_context_digest
test_delegate_auto_verify_prefers_ml_smoke
test_agent_call_dry_run_attaches_shared_memory
test_agent_run_auto_read_routes_to_call
test_agent_run_auto_write_routes_to_delegate
test_scrubber_passes_harness_sources
test_scrubber_blocks_env_style_token
test_scrubber_no_function_name_bypass
test_agent_run_read_priority_routing
test_link_and_unlink_with_home_override
test_skill_doctor_detects_duplicate_names
test_cleanup_dry_run_and_apply
test_update_help_runs
test_uninstall_help_runs
test_uninstall_dry_run_no_changes
test_version_file_present
test_status_shows_version

echo "scripts-smoke: ok"
