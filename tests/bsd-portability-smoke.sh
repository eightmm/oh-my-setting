#!/usr/bin/env bash
set -euo pipefail

# Tools whose behavior depends on the userland, exercised end to end.
#
# These lived as inline steps in the macOS CI job, which meant they could not
# be run locally and vanished silently when that job was reworked. They cover
# the one class of breakage a Linux-only gate cannot see: sed, awk, date, sort,
# and stat differ between GNU and BSD, and every script below leans on them.
# Run here on every platform, and on macOS in CI, where the difference is real.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-bsd.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
# Normalize away the "//" a trailing-slash TMPDIR leaves in the template.
TMP="$(cd "$TMP" && pwd -P)"

export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null
export GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t
export GIT_AUTHOR_EMAIL=t@example.com
export GIT_COMMITTER_NAME=t
export GIT_COMMITTER_EMAIL=t@example.com

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

test_detect_project_style_reads_imports() {
  local dir="$TMP/detect/app"
  local style

  mkdir -p "$dir"
  printf 'import torch\n' > "$dir/model.py"
  style="$("$ROOT/scripts/detect-project-style.sh" "$dir")"
  [ "$style" = ml ] || fail "detect-project-style should report ml, got: $style"
}

test_template_applies_and_passes_project_doctor() {
  local project="$TMP/template/project"

  "$ROOT/scripts/apply-project-template.sh" general "$project" >/dev/null ||
    fail "apply-project-template should succeed"
  "$ROOT/scripts/project-doctor.sh" "$project" >/dev/null ||
    fail "project-doctor should accept a freshly applied template"
}

test_job_digest_extracts_the_failure() {
  local dir="$TMP/digest"

  mkdir -p "$dir"
  printf 'step 1 loss: 0.5\nTraceback (most recent call last):\n  File "t.py", line 1\nRuntimeError: CUDA out of memory\n' \
    > "$dir/run.log"
  "$ROOT/scripts/job-digest.sh" "$dir/run.log" > "$dir/digest.md"
  grep -Fq 'CUDA out of memory' "$dir/digest.md" ||
    fail "job-digest lost the error line"
  grep -Fq '## Last traceback' "$dir/digest.md" ||
    fail "job-digest lost the traceback section"
}

test_run_ledger_lists_a_recorded_command() {
  local project="$TMP/ledger/project"

  mkdir -p "$project/docs"
  printf '{"ts":"2026-06-12T00:00:00Z","exit":0,"duration_s":1,"git_sha":"abc1234","dirty":0,"cmd":["echo","ok"]}\n' \
    > "$project/docs/EXPERIMENTS.jsonl"
  (cd "$project" && "$ROOT/scripts/run-ledger.sh" list 1) > "$project/list.txt"
  grep -Fq 'echo ok' "$project/list.txt" ||
    fail "run-ledger did not render the recorded command"
}

test_run_tools_survive_a_bsd_userland() {
  local project="$TMP/run-tools/p"
  local id

  mkdir -p "$project"
  (
    cd "$project"
    git init -b main -q .
    echo base > f
    git add f
    git commit -qm init

    export OMS_RUNS_DIR="$project/.oms/runs"
    export OMS_RUN_INDEX="$project/.oms/runs/spine.jsonl"
    id="$("$ROOT/scripts/run.sh" new)"
    export OMS_RUN_ID="$id"

    printf 'fake\n' > ckpt.pt
    "$ROOT/scripts/run-capsule.sh" run --output ckpt.pt --no-ledger \
      -- bash -c 'echo trained' >/dev/null
    "$ROOT/scripts/run-capsule.sh" whence ckpt.pt >/dev/null
    "$ROOT/scripts/run.sh" show "$id" | grep -Fq run-capsule ||
      { echo "FAIL: run spine lost the capsule step" >&2; exit 1; }

    printf 'a\nb\nc\n' > train.txt
    printf 'd\na\n' > val.txt
    OMS_MANIFEST_DIR="$project/.oms/manifests" "$ROOT/scripts/data-manifest.sh" \
      create --name ds --split train=train.txt --split val=val.txt >/dev/null
    if OMS_MANIFEST_DIR="$project/.oms/manifests" \
       "$ROOT/scripts/data-manifest.sh" leakage --name ds >/dev/null 2>&1; then
      echo "FAIL: overlapping splits must be reported as leakage" >&2
      exit 1
    fi

    OMS_EXPERIMENT_BOARD="$project/.oms/experiments.jsonl" \
      "$ROOT/scripts/experiment-board.sh" claim --id e --hypothesis h >/dev/null
    "$ROOT/scripts/run.sh" validate --dir "$project/.oms" >/dev/null
  ) || fail "run-tools fixture failed"
}

test_detect_project_style_reads_imports
test_template_applies_and_passes_project_doctor
test_job_digest_extracts_the_failure
test_run_ledger_lists_a_recorded_command
test_run_tools_survive_a_bsd_userland

echo "bsd-portability: ok"
