#!/usr/bin/env bash
set -euo pipefail

# CI state has to be keyed by (branch, HEAD sha) or it lies in the reassuring
# direction. Two failures are covered here: an unauthenticated gh that used to
# print "no runs for <branch>" and exit 0, and a run for an earlier commit that
# used to be presented as a stale-but-current row — a nag with no action, while
# the actionable truth (the commits were never pushed) went unsaid.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-ci-status.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

git_init() {
  local repo="$1"
  mkdir -p "$repo"
  # -b main so the fixture is branch-deterministic wherever it runs.
  git -C "$repo" init -q -b main
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name Test
  printf 'base\n' > "$repo/file.txt"
  git -C "$repo" add file.txt
  git -C "$repo" commit -qm base
  mkdir -p "$repo/.oms"
  printf '*\n' > "$repo/.oms/.gitignore"
}

# A local bare remote is the whole point: without an upstream there is no such
# thing as "unpushed", and the scripts fall back to the older comparison.
make_pushed_repo() {
  local repo="$1"
  git_init "$repo"
  git -C "$repo" init -q --bare "$repo.git" 2>/dev/null || git init -q --bare "$repo.git"
  git -C "$repo" remote add origin "$repo.git"
  git -C "$repo" push -q -u origin main
}

commit_more() {
  local repo="$1" count="$2" i=0
  while [ "$i" -lt "$count" ]; do
    i=$((i + 1))
    printf 'change %s\n' "$i" >> "$repo/file.txt"
    git -C "$repo" commit -qam "change $i"
  done
}

# One stub, driven by the environment at call time: OMS_T_GH_MODE=authfail is
# an unauthenticated gh, and `pr view` answers the way the real CLI does for a
# branch with no PR — which must stay benign.
write_stub_gh() {
  local path="$1"
  mkdir -p "$(dirname "$path")"
  cat > "$path" <<'EOF'
#!/usr/bin/env bash
if [ "${OMS_T_GH_MODE:-run}" = "authfail" ]; then
  echo "gh: To get started with GitHub CLI, please run: gh auth login." >&2
  exit 4
fi
case "${1:-} ${2:-}" in
  "run list")
    printf '[{"status":"%s","conclusion":"%s","workflowName":"test","headSha":"%s","url":"https://example.invalid/run/1"}]\n' \
      "${OMS_T_GH_STATUS:-completed}" "${OMS_T_GH_CONCLUSION:-success}" \
      "${OMS_T_GH_SHA:-0000000000000000000000000000000000000000}"
    ;;
  "pr view")
    if [ "${OMS_T_GH_PR_MODE:-none}" = "authfail" ]; then
      echo "gh: To get started with GitHub CLI, please run: gh auth login." >&2
      exit 4
    fi
    echo 'no pull requests found for branch "main"' >&2
    exit 1
    ;;
  *) exit 2 ;;
esac
EOF
  chmod +x "$path"
}

ci_json_state() {
  bash "$ROOT/scripts/state.sh" --repo "$1" --json |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["ci"]["state"])'
}

# --- FIX 1: an unusable gh is reported, never read as "no CI here" ----------

test_unauthenticated_gh_is_reported_on_the_explicit_surfaces() {
  local repo="$TMP/authfail"
  local gh="$TMP/authfail-bin/gh"
  local out

  make_pushed_repo "$repo"
  write_stub_gh "$gh"

  if out="$( (cd "$repo" && OMS_T_GH_MODE=authfail OMS_GH_BIN="$gh" \
    bash "$ROOT/scripts/ci-status.sh" main) 2>&1 )"; then
    fail "an unauthenticated gh must not exit 0: $out"
  fi
  grep -Fq "gh not authenticated or unreachable" <<<"$out" ||
    fail "the failure should name the cause: $out"
  grep -Fq "gh auth login" <<<"$out" ||
    fail "the failure should name the fix: $out"
  # The regression itself: silence read as a clean branch.
  if grep -Fq "no runs for" <<<"$out"; then
    fail "an unanswerable query must never read as 'no runs': $out"
  fi

  if (cd "$repo" && OMS_T_GH_MODE=authfail OMS_GH_BIN="$gh" \
    bash "$ROOT/scripts/ci-status.sh" record main) >/dev/null 2>&1; then
    fail "record must fail loudly when gh cannot answer"
  fi
  [ ! -f "$repo/.oms/ci.jsonl" ] ||
    fail "a query that never succeeded must not write a CI row"
}

test_unauthenticated_gh_leaves_the_tick_path_silent() {
  local repo="$TMP/authfail-tick"
  local gh="$TMP/authfail-tick-bin/gh"
  local out

  make_pushed_repo "$repo"
  write_stub_gh "$gh"

  # Documented design: a hook must not fail an agent turn because gh is
  # unavailable, so the loud path above stops at the explicit surfaces.
  out="$( (cd "$repo" && OMS_T_GH_MODE=authfail OMS_GH_BIN="$gh" \
    OMS_LOCK_DIR="$TMP/tick-locks" bash "$ROOT/scripts/ci-status.sh" tick main) 2>&1 )" ||
    fail "tick must stay fail-open when gh is unauthenticated: $out"
  [ -z "$out" ] || fail "tick must stay silent, got: $out"
}

test_pr_lookup_distinguishes_no_pr_from_an_unusable_gh() {
  local repo="$TMP/pr-lookup"
  local gh="$TMP/pr-lookup-bin/gh"
  local head out

  make_pushed_repo "$repo"
  head="$(git -C "$repo" rev-parse HEAD)"
  write_stub_gh "$gh"

  # A branch with no PR is the ordinary case and stays quiet.
  out="$( (cd "$repo" && OMS_T_GH_SHA="$head" OMS_GH_BIN="$gh" \
    bash "$ROOT/scripts/ci-status.sh" record main) 2>&1 )" ||
    fail "a branch without a PR must not fail record: $out"
  if grep -Fq "not authenticated" <<<"$out"; then
    fail "'no pull requests found' is not an outage: $out"
  fi

  # Any other gh failure is the same outage, reported — but only after the CI
  # row is written, so a usable answer is never thrown away with the error.
  rm -f "$repo/.oms/ci.jsonl"
  if out="$( (cd "$repo" && OMS_T_GH_SHA="$head" OMS_T_GH_PR_MODE=authfail \
    OMS_T_GH_CONCLUSION=success OMS_GH_BIN="$gh" \
    bash "$ROOT/scripts/ci-status.sh" record main) 2>&1 )"; then
    fail "an unusable gh must not leave record exiting 0: $out"
  fi
  grep -Fq "gh not authenticated or unreachable" <<<"$out" ||
    fail "the pr lookup failure should be reported: $out"
  grep -Fq "\"sha\": \"$head\"" "$repo/.oms/ci.jsonl" ||
    fail "the CI row should survive a failed PR lookup"
}

# --- FIX 2: keyed by (branch, sha) ------------------------------------------

test_run_for_a_prior_sha_is_history_not_a_current_result() {
  local repo="$TMP/prior-sha"
  local gh="$TMP/prior-sha-bin/gh"
  local old out

  make_pushed_repo "$repo"
  old="$(git -C "$repo" rev-parse HEAD)"
  commit_more "$repo" 1
  git -C "$repo" push -q origin main
  write_stub_gh "$gh"

  out="$( (cd "$repo" && OMS_T_GH_SHA="$old" OMS_GH_BIN="$gh" \
    bash "$ROOT/scripts/ci-status.sh" record main) 2>&1 )" ||
    fail "a green run on an earlier commit is not a failure: $out"
  grep -Fq "no run yet for HEAD" <<<"$out" ||
    fail "HEAD has no run of its own and should say so: $out"
  grep -Fq "history:" <<<"$out" ||
    fail "the earlier run should be labelled history: $out"

  [ "$(ci_json_state "$repo")" = pending ] ||
    fail "a pushed HEAD with no run of its own is pending"
  out="$(bash "$ROOT/scripts/state.sh" --repo "$repo")"
  grep -Fq "unknown/pending" <<<"$out" ||
    fail "state should report the missing run for HEAD: $out"
  grep -Fq "history:" <<<"$out" ||
    fail "the recorded row belongs under history: $out"
  if grep -Fq "STALE" <<<"$out"; then
    fail "a prior-SHA row must not render as a stale current row: $out"
  fi

  out="$(bash "$ROOT/scripts/inbox.sh" --repo "$repo" --json)"
  if grep -Fq '"ci-stale"' <<<"$out"; then
    fail "the generic staleness nag is replaced by this model: $out"
  fi
}

test_unpushed_head_is_named_as_unpushed_everywhere() {
  local repo="$TMP/unpushed"
  local gh="$TMP/unpushed-bin/gh"
  local pushed out

  make_pushed_repo "$repo"
  mkdir -p "$repo/.github/workflows"
  printf 'name: ci\n' > "$repo/.github/workflows/ci.yml"
  git -C "$repo" add .github
  git -C "$repo" commit -qm workflows
  git -C "$repo" push -q origin main
  pushed="$(git -C "$repo" rev-parse HEAD)"
  commit_more "$repo" 2

  # A repo that has CI says what to do before anything has been recorded.
  [ "$(ci_json_state "$repo")" = unpushed ] || fail "two local commits are unpushed"
  out="$(bash "$ROOT/scripts/state.sh" --repo "$repo")"
  grep -Fq "unpushed: 2 commit(s) ahead of origin/main" <<<"$out" ||
    fail "state should count the unpushed commits: $out"
  grep -Fq "push to get CI" <<<"$out" ||
    fail "state should name the action: $out"

  write_stub_gh "$gh"
  out="$( (cd "$repo" && OMS_T_GH_SHA="$pushed" OMS_GH_BIN="$gh" \
    bash "$ROOT/scripts/ci-status.sh" record main) 2>&1 )" ||
    fail "an unpushed HEAD is not a CI failure: $out"
  grep -Fq "unpushed (2 commits ahead" <<<"$out" ||
    fail "ci-status should report the push state: $out"
  grep -Fq "history:" <<<"$out" ||
    fail "the last pushed commit's run is history: $out"
  grep -Fq "\"sha\": \"$pushed\"" "$repo/.oms/ci.jsonl" ||
    fail "the run that did happen should still be recorded"

  out="$(bash "$ROOT/scripts/state.sh" --repo "$repo")"
  grep -Fq "unpushed: 2 commit(s)" <<<"$out" ||
    fail "a recorded prior run must not displace the push state: $out"
  grep -Fq "history:" <<<"$out" || fail "the prior run is history: $out"
  if grep -Fq "STALE" <<<"$out"; then
    fail "unpushed work is not a stale record: $out"
  fi

  out="$(bash "$ROOT/scripts/inbox.sh" --repo "$repo" --json)"
  grep -Fq '"unpushed-head"' <<<"$out" ||
    fail "the inbox should carry the push item: $out"
  grep -Fq '"summary": "2 commit(s) not pushed' <<<"$out" ||
    fail "the push item should count the commits: $out"
  if grep -Fq '"ci-stale"' <<<"$out"; then
    fail "unpushed work must not also raise the staleness nag: $out"
  fi
  OMS_T_INBOX="$out" python3 - <<'PY' || fail "the push item should be P2 with a push command"
import json, os
items = {i["code"]: i for i in json.loads(os.environ["OMS_T_INBOX"])["items"]}
item = items["unpushed-head"]
assert item["priority"] == "P2", item
assert item["command"] == "git push", item
PY
}

test_completed_run_for_head_is_the_current_result() {
  local repo="$TMP/head-run"
  local gh="$TMP/head-run-bin/gh"
  local head out

  make_pushed_repo "$repo"
  head="$(git -C "$repo" rev-parse HEAD)"
  write_stub_gh "$gh"

  out="$( (cd "$repo" && OMS_T_GH_SHA="$head" OMS_GH_BIN="$gh" \
    bash "$ROOT/scripts/ci-status.sh" record main) 2>&1 )" ||
    fail "a green run for HEAD should exit 0: $out"
  grep -Fq "completed success" <<<"$out" ||
    fail "HEAD's own run is the reported result: $out"

  [ "$(ci_json_state "$repo")" = current ] || fail "HEAD's run should read as current"
  out="$(bash "$ROOT/scripts/state.sh" --repo "$repo")"
  grep -Fq "completed success" <<<"$out" || fail "state should show it: $out"
  if grep -Eq "STALE|unpushed|pending" <<<"$out"; then
    fail "a run for HEAD is neither stale, unpushed, nor pending: $out"
  fi
  out="$(bash "$ROOT/scripts/inbox.sh" --repo "$repo" --json)"
  if grep -Fq '"ci-' <<<"$out"; then
    fail "a green current run is not an inbox item: $out"
  fi

  # Red for the same commit stays the loud P1 it always was.
  if (cd "$repo" && OMS_T_GH_SHA="$head" OMS_T_GH_CONCLUSION=failure OMS_GH_BIN="$gh" \
    bash "$ROOT/scripts/ci-status.sh" record main) >/dev/null 2>&1; then
    fail "a red run for HEAD must exit nonzero"
  fi
  out="$(bash "$ROOT/scripts/inbox.sh" --repo "$repo" --json)"
  grep -Fq '"ci-failed"' <<<"$out" ||
    fail "a red run for HEAD stays a P1 item: $out"
}

test_repo_without_upstream_keeps_the_recorded_vs_head_signal() {
  local repo="$TMP/no-upstream"
  local out

  # No remote at all: whether HEAD reached CI is unknowable, so the older
  # recorded-vs-HEAD comparison remains the honest answer rather than a
  # guess in either direction.
  git_init "$repo"
  printf '{"schema":1,"ts":"2026-08-01T00:00:00Z","branch":"main","sha":"%s","status":"completed","conclusion":"success"}\n' \
    "0000000000000000000000000000000000000001" > "$repo/.oms/ci.jsonl"

  [ "$(ci_json_state "$repo")" = stale ] ||
    fail "without an upstream the recorded row is only known to be stale"
  out="$(bash "$ROOT/scripts/state.sh" --repo "$repo")"
  grep -Fq "STALE" <<<"$out" ||
    fail "the pre-existing signal should survive: $out"
  grep -Fq "oms state --refresh-ci" <<<"$out" ||
    fail "state should still name the refresh: $out"
  out="$(bash "$ROOT/scripts/inbox.sh" --repo "$repo" --json)"
  grep -Fq '"ci-stale"' <<<"$out" ||
    fail "the staleness item still applies where push state is unknown: $out"
}

test_unauthenticated_gh_is_reported_on_the_explicit_surfaces
test_unauthenticated_gh_leaves_the_tick_path_silent
test_pr_lookup_distinguishes_no_pr_from_an_unusable_gh
test_run_for_a_prior_sha_is_history_not_a_current_result
test_unpushed_head_is_named_as_unpushed_everywhere
test_completed_run_for_head_is_the_current_result
test_repo_without_upstream_keeps_the_recorded_vs_head_signal

echo "ci-status-smoke: ok"
