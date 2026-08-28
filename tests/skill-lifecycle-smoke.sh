#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-skill-lifecycle.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

make_repo() {
  local repo="$1"
  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" config user.email test@example.invalid
  git -C "$repo" config user.name test
  printf 'fixture\n' > "$repo/README.md"
  git -C "$repo" add README.md
  git -C "$repo" commit -qm init
}

make_skill() {
  local dir="$1" name="$2" body="${3:-Use this skill for deterministic fixture work.}"
  mkdir -p "$dir/references"
  cat > "$dir/SKILL.md" <<EOF
---
name: $name
description: A portable third-party fixture skill with enough routing detail for deterministic lifecycle tests.
---

# Fixture skill

$body
EOF
  printf 'reference-v1\n' > "$dir/references/guide.md"
}

test_skill_eval_is_explicit_repeatable_and_content_free() {
  local repo="$TMP/eval-repo" suite="$TMP/eval-suite.json" router="$TMP/router.py"
  local out first second rc=0
  make_repo "$repo"
  make_skill "$repo/.oms/skills/oms-eval-fixture" oms-eval-fixture

  cat > "$router" <<'PY'
import json, os, sys
prompt = sys.stdin.read()
selected = []
if os.environ.get("OMS_SKILL_EVAL_TREATMENT") == "1" and "deploy" in prompt.lower():
    selected.append("oms-eval-fixture")
print(json.dumps({"selected": selected}, sort_keys=True))
PY
  cat > "$suite" <<EOF
{
  "schema": 1,
  "skill": "oms-eval-fixture",
  "router": ["python3", "$router"],
  "trigger_cases": [
    {"id": "positive", "prompt": "Deploy the fixture", "should_trigger": true},
    {"id": "negative", "prompt": "Explain a poem", "should_trigger": false}
  ],
  "task_cases": [
    {
      "id": "writes-treatment-marker",
      "command": ["python3", "-c", "import os,pathlib; pathlib.Path('result').write_text(os.environ.get('OMS_SKILL_EVAL_TREATMENT',''))"],
      "verify": ["python3", "-c", "import pathlib,sys; sys.exit(0 if pathlib.Path('result').read_text() == '1' else 1)"]
    }
  ]
}
EOF

  out="$("$ROOT/scripts/skill-forge.sh" --repo "$repo" eval oms-eval-fixture \
    --suite "$suite" --json 2>&1)" || rc=$?
  [ "$rc" = 2 ] || fail "task eval without explicit host authority returned $rc: $out"
  [ ! -e "$repo/.oms/runtime/skill-evals.jsonl" ] ||
    fail "refused eval wrote a telemetry row"

  first="$("$ROOT/scripts/skill-forge.sh" --repo "$repo" eval oms-eval-fixture \
    --suite "$suite" --allow-host-commands --json)" || fail "skill eval failed"
  second="$("$ROOT/scripts/skill-forge.sh" --repo "$repo" eval oms-eval-fixture \
    --suite "$suite" --allow-host-commands --json)" || fail "repeat skill eval failed"
  [ "$first" = "$second" ] || fail "skill eval output is not deterministic"
  printf '%s' "$first" | python3 -c '
import json, sys
row = json.load(sys.stdin)
assert row["schema"] == 1 and row["skill"] == "oms-eval-fixture", row
assert row["trigger"]["treatment"] == {"false_negative": 0, "false_positive": 0, "true_negative": 1, "true_positive": 1}, row
assert row["trigger"]["precision"] == 1.0 and row["trigger"]["recall"] == 1.0, row
assert row["task"] == {"baseline_passed": 0, "cases": 1, "pass_delta": 1, "treatment_passed": 1}, row
assert "prompt" not in json.dumps(row) and "output" not in json.dumps(row), row
' || fail "skill eval metrics are wrong: $first"

  "$ROOT/scripts/skill-forge.sh" --repo "$repo" eval oms-eval-fixture \
    --suite "$suite" --allow-host-commands --record --json >/dev/null ||
    fail "recorded skill eval failed"
  [ "$(wc -l < "$repo/.oms/runtime/skill-evals.jsonl" | tr -d ' ')" = 1 ] ||
    fail "recorded skill eval did not append exactly one row"
  ! grep -Fq 'Deploy the fixture' "$repo/.oms/runtime/skill-evals.jsonl" ||
    fail "skill eval ledger retained prompt content"
  "$ROOT/scripts/runtime.sh" --repo "$repo" benchmark show | python3 -c '
import json, sys
row = json.load(sys.stdin)
assert row["skill_evals"] == {
    "count": 1,
    "task_pass_delta_sum": 1,
    "treatment_task_passed": 1,
    "trigger_false_negatives": 0,
    "trigger_false_positives": 0,
    "trigger_true_positives": 1,
}, row
' || fail "runtime benchmark did not project recorded skill evaluation"
}

test_skill_bundle_preview_import_update_and_rollback() {
  local repo="$TMP/bundle-repo" source="$TMP/bundle-source"
  local preview first second current out
  make_repo "$repo"
  make_skill "$source" oms-vendor-fixture

  preview="$("$ROOT/scripts/skill-forge.sh" --repo "$repo" preview \
    --source "$source" --json)" || fail "bundle preview failed"
  first="$(printf '%s' "$preview" | python3 -c 'import json,sys; print(json.load(sys.stdin)["bundle_sha256"])')"
  [ "${#first}" = 64 ] || fail "preview omitted the bundle digest: $preview"
  [ ! -e "$repo/.oms/skill-store" ] || fail "preview wrote project state"

  mkdir -p "$repo/.agents/skills"
  ln -s "$TMP" "$repo/.agents/skills/oms-vendor-fixture"
  out="$("$ROOT/scripts/skill-forge.sh" --repo "$repo" import \
    --source "$source" --expected-bundle-sha256 "$first" --apply 2>&1)" &&
    fail "import replaced a foreign project skill link: $out"
  [ "$(readlink "$repo/.agents/skills/oms-vendor-fixture")" = "$TMP" ] ||
    fail "foreign project skill link was changed"
  rm "$repo/.agents/skills/oms-vendor-fixture"

  "$ROOT/scripts/skill-forge.sh" --repo "$repo" import \
    --source "$source" --expected-bundle-sha256 "$first" --apply >/dev/null ||
    fail "bundle import failed"
  [ -f "$repo/.oms/skill-store/oms-vendor-fixture/lock.json" ] ||
    fail "bundle lock receipt missing"
  [ -L "$repo/.agents/skills/oms-vendor-fixture" ] ||
    fail "Codex skill link missing after import"
  [ -L "$repo/.claude/skills/oms-vendor-fixture" ] ||
    fail "Claude skill link missing after import"
  grep -Fq 'reference-v1' "$repo/.agents/skills/oms-vendor-fixture/references/guide.md" ||
    fail "imported bundle resource missing"

  printf 'reference-v2\n' > "$source/references/guide.md"
  preview="$("$ROOT/scripts/skill-forge.sh" --repo "$repo" preview \
    --source "$source" --json)"
  second="$(printf '%s' "$preview" | python3 -c 'import json,sys; print(json.load(sys.stdin)["bundle_sha256"])')"
  [ "$first" != "$second" ] || fail "bundle digest did not change"
  out="$("$ROOT/scripts/skill-forge.sh" --repo "$repo" update \
    --source "$source" --expected-current-sha256 "$(printf '0%.0s' $(seq 1 64))" \
    --expected-bundle-sha256 "$second" --apply 2>&1)" &&
    fail "update accepted stale current provenance: $out"
  grep -Fq 'reference-v1' "$repo/.agents/skills/oms-vendor-fixture/references/guide.md" ||
    fail "failed update changed active bytes"

  "$ROOT/scripts/skill-forge.sh" --repo "$repo" update \
    --source "$source" --expected-current-sha256 "$first" \
    --expected-bundle-sha256 "$second" --apply >/dev/null || fail "bundle update failed"
  grep -Fq 'reference-v2' "$repo/.agents/skills/oms-vendor-fixture/references/guide.md" ||
    fail "updated bundle resource missing"

  "$ROOT/scripts/skill-forge.sh" --repo "$repo" rollback oms-vendor-fixture \
    --to "$first" --expected-current-sha256 "$second" --apply >/dev/null ||
    fail "bundle rollback failed"
  grep -Fq 'reference-v1' "$repo/.agents/skills/oms-vendor-fixture/references/guide.md" ||
    fail "rollback did not restore exact revision"
  current="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["bundle_sha256"])' \
    "$repo/.oms/skill-store/oms-vendor-fixture/lock.json")"
  [ "$current" = "$first" ] || fail "rollback provenance is wrong"

  ln -s "$TMP" "$source/references/escape"
  out="$("$ROOT/scripts/skill-forge.sh" --repo "$repo" preview \
    --source "$source" --json 2>&1)" && fail "preview accepted a symlink: $out"
  rm "$source/references/escape"

  printf 'ghp_abcdefghijklmnopqrstuvwxyz0123456789\n' > "$source/.env"
  out="$("$ROOT/scripts/skill-forge.sh" --repo "$repo" preview \
    --source "$source" --json 2>&1)" && fail "preview accepted a credential file: $out"
  printf '%s' "$out" | grep -Fq 'sensitive' ||
    fail "credential bundle refusal was not explicit: $out"
}

test_reviewed_draft_is_inert_and_source_bound() {
  local repo="$TMP/derive-repo" thread
  local out draft
  thread="$repo/.oms/threads/th-fixture.jsonl"
  make_repo "$repo"
  mkdir -p "$(dirname "$thread")"
  cat > "$thread" <<'EOF'
{"schema":1,"ts":"2026-08-27T00:00:00Z","thread":"th-fixture","seq":1,"role":"user","agent":"test","text":"When the fixture fails, inspect the bounded status report and record the exact failing check."}
{"schema":1,"ts":"2026-08-27T00:01:00Z","thread":"th-fixture","seq":2,"role":"assistant","agent":"test","text":"Run the focused verifier, fix only the confirmed boundary, then rerun the focused verifier before the final gate."}
EOF

  out="$("$ROOT/scripts/skill-forge.sh" --repo "$repo" derive --from thread \
    --id th-fixture --name oms-derived-fixture --json)" || fail "draft preview failed"
  printf '%s' "$out" | python3 -c '
import json,sys
row=json.load(sys.stdin)
assert row["status"] == "preview" and row["source"]["kind"] == "thread", row
assert len(row["source"]["sha256"]) == 64, row
' || fail "draft preview contract is wrong: $out"
  [ ! -e "$repo/.oms/drafts" ] || fail "draft preview mutated the repo"

  out="$("$ROOT/scripts/skill-forge.sh" --repo "$repo" derive --from thread \
    --id th-fixture --name oms-derived-fixture --apply --json)" || fail "draft write failed"
  draft="$(printf '%s' "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin)["draft_path"])')"
  [ -f "$repo/$draft/SKILL.md" ] || fail "derived SKILL.md missing"
  [ -f "$repo/$draft/REVIEW.md" ] || fail "derived review receipt missing"
  [ ! -e "$repo/.oms/skills/oms-derived-fixture" ] ||
    fail "derive activated an unreviewed skill"
  [ ! -e "$repo/.agents/skills/oms-derived-fixture" ] ||
    fail "derive linked an unreviewed skill"

  out="$("$ROOT/scripts/skill-forge.sh" --repo "$repo" derive --from attempt \
    --id missing --name oms-missing --json 2>&1)" &&
    fail "derive fabricated a draft from missing attempt evidence: $out"
  printf '%s' "$out" | grep -Fq 'insufficient-source' ||
    fail "insufficient source refusal is not typed: $out"
}

test_skill_eval_is_explicit_repeatable_and_content_free
test_skill_bundle_preview_import_update_and_rollback
test_reviewed_draft_is_inert_and_source_bound

echo "skill-lifecycle-smoke: ok"
