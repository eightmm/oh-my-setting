#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-land-approval.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
fail() { echo "patch-land-approval: $*" >&2; exit 1; }

export HOME="$TMP/home"
export XDG_STATE_HOME="$TMP/state"
export OMS_LOCK_DIR="$TMP/locks"
mkdir -p "$HOME" "$XDG_STATE_HOME" "$OMS_LOCK_DIR"

repo="$TMP/repo"
mkdir -p "$repo"
git -C "$repo" init -q
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name test
printf 'base\n' > "$repo/file.txt"
git -C "$repo" add file.txt
git -C "$repo" commit -qm base

patch="$TMP/change.patch"
printf 'changed\n' > "$repo/file.txt"
git -C "$repo" diff --binary > "$patch"
git -C "$repo" restore file.txt
other_patch="$TMP/other-change.patch"
printf 'other change\n' > "$repo/file.txt"
git -C "$repo" diff --binary > "$other_patch"
git -C "$repo" restore file.txt

LAND="$ROOT/scripts/patch-land.sh"
APPROVAL="$ROOT/scripts/approval-inbox.sh"

# The one mutating step is parent-only. A delegated worker carries
# OMS_HARNESS_CHILD=1 and works in a throwaway worktree, but git metadata still
# names this checkout, so --repo was all that stood between it and the owner's
# tree, and the authority guard only reports such a write afterwards.
if OMS_HARNESS_CHILD=1 "$LAND" --repo "$repo" --patch "$patch" --verify true \
  >/dev/null 2>&1; then
  fail "a harness child landed onto the owner's tree"
fi
grep -Fxq base "$repo/file.txt" || fail "a child-marked landing still changed the tree"
[ ! -s "$repo/.oms/landings.jsonl" ] || fail "a refused landing wrote a landing row"

# An untracked file can collide with a patch path or be silently included in a
# later commit. Landing therefore requires the complete working tree, not just
# tracked files, to be clean.
printf 'user-owned\n' > "$repo/untracked.txt"
if "$LAND" --repo "$repo" --patch "$patch" --verify true >/dev/null 2>&1; then
  fail "landing ignored an untracked main-tree file"
fi
grep -Fxq base "$repo/file.txt" || fail "untracked-tree refusal still changed the patch target"
rm -f "$repo/untracked.txt"

if OMS_REQUIRE_LANDING_APPROVAL=1 "$LAND" --repo "$repo" --patch "$patch" \
  --verify true >/dev/null 2>&1; then
  fail "strict landing policy accepted an unapproved patch"
fi
grep -Fxq base "$repo/file.txt" || fail "unapproved patch changed the tree"

approval="$(OMS_REQUIRE_LANDING_APPROVAL=1 "$LAND" --repo "$repo" \
  --patch "$patch" --verify true --request-approval)" ||
  fail "could not request an exact landing approval"
case "$approval" in apr_*) ;; *) fail "unexpected approval id: $approval" ;; esac

"$APPROVAL" --repo "$repo" show --approval "$approval" --json > "$TMP/request.json"
python3 - "$TMP/request.json" "$patch" "$repo" <<'PY' || fail "approval request is not bound to the action"
import hashlib, json, subprocess, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
digest = hashlib.sha256(open(sys.argv[2], "rb").read()).hexdigest()
base = subprocess.check_output(["git", "-C", sys.argv[3], "rev-parse", "HEAD"], text=True).strip()
assert row["action"] == "patch-land", row
assert row["object_id"] == "patch:" + digest, row
assert row["patch_sha"] == digest, row
assert row["base_sha"] == base, row
assert row["profile"] == "trusted-local", row
params = row["parameters"]
assert params["admission_contract_schema"] == 1, params
assert params["verify_explicit"] is True, params
assert params["verify_sha256"] == hashlib.sha256(b"true").hexdigest(), params
assert params["ml"] is False, params
assert params["executor_id"] is None, params
assert params["executor_soul_sha256"] is None, params
PY

# Retired bindings must remain part of the approval digest: dropping those
# fields would turn an old Soul-specific grant into a general landing grant.
python3 - "$TMP/request.json" "$APPROVAL" "$LAND" "$repo" "$patch" <<'PY' || fail "legacy Soul approval was replayable"
import json, os, subprocess, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
approval_cmd = [sys.argv[2], "--repo", sys.argv[4]]
params = dict(row["parameters"], executor_id="retired", executor_soul_sha256="a" * 64)
old = subprocess.check_output(approval_cmd + [
    "request", "--action", row["action"], "--object-id", row["object_id"],
    "--summary", "legacy Soul binding", "--base-sha", row["base_sha"],
    "--patch-sha", row["patch_sha"], "--parameters-json", json.dumps(params),
], text=True).strip()
token = subprocess.check_output(approval_cmd + [
    "decide", "--approval", old, "--decision", "approve",
    "--expected-version", "1", "--actor", "operator",
], text=True).strip()
result = subprocess.run([
    sys.argv[3], "--repo", sys.argv[4], "--patch", sys.argv[5], "--verify", "true",
    "--approval", old, "--approval-version", "2", "--approval-token", token,
], env=dict(os.environ, OMS_REQUIRE_LANDING_APPROVAL="1"), capture_output=True, text=True)
assert result.returncode != 0, "legacy grant authorized an unbound landing"
current = json.loads(subprocess.check_output(approval_cmd + ["show", "--approval", old, "--json"], text=True))
assert current["state"] == "approved" and current["version"] == 2, current
PY
grep -Fxq base "$repo/file.txt" || fail "legacy approval changed the patch target"

grant="$($APPROVAL --repo "$repo" decide --approval "$approval" \
  --decision approve --expected-version 1 --actor operator)" || fail "approval failed"

# Verification mode and ML preference change admission semantics. Possession
# of the patch grant must not authorize weakening or changing either field.
if OMS_REQUIRE_LANDING_APPROVAL=1 "$LAND" --repo "$repo" --patch "$patch" \
  --verify false --approval "$approval" --approval-version 2 \
  --approval-token "$grant" >/dev/null 2>&1; then
  fail "approval for one verifier accepted a different verifier"
fi
if OMS_REQUIRE_LANDING_APPROVAL=1 "$LAND" --repo "$repo" --patch "$patch" \
  --verify true --ml --approval "$approval" --approval-version 2 \
  --approval-token "$grant" >/dev/null 2>&1; then
  fail "approval without ML mode accepted ML admission"
fi
"$APPROVAL" --repo "$repo" show --approval "$approval" --json > "$TMP/still-approved.json"
python3 - "$TMP/still-approved.json" <<'PY' || fail "an action mismatch consumed the grant"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "approved" and row["version"] == 2, row
PY

# Python removes assert statements under optimization. Exact-action approval
# validation must still reject a different patch when the interpreter inherits
# PYTHONOPTIMIZE=1; possession of A's grant is not authorization to land B.
if PYTHONOPTIMIZE=1 OMS_REQUIRE_LANDING_APPROVAL=1 \
  "$LAND" --repo "$repo" --patch "$other_patch" --verify true \
  --approval "$approval" --approval-version 2 --approval-token "$grant" \
  >"$TMP/optimized-mismatch.out" 2>&1; then
  fail "optimized Python let one patch's approval land a different patch"
fi
grep -Fxq base "$repo/file.txt" ||
  fail "optimized exact-action refusal still changed the patch target"

if OMS_REQUIRE_LANDING_APPROVAL=1 "$LAND" --repo "$repo" --patch "$patch" \
  --verify true --approval "$approval" --approval-version 2 \
  --approval-token 'grant_wrong.this-token-is-long-enough-but-wrong000000' \
  >/dev/null 2>&1; then
  fail "wrong one-time grant landed the patch"
fi
grep -Fxq base "$repo/file.txt" || fail "wrong grant changed the tree"

OMS_REQUIRE_LANDING_APPROVAL=1 "$LAND" --repo "$repo" --patch "$patch" \
  --verify true --approval "$approval" --approval-version 2 \
  --approval-token "$grant" >/dev/null || fail "approved patch did not land"
grep -Fxq changed "$repo/file.txt" || fail "approved patch was not applied"
"$APPROVAL" --repo "$repo" show --approval "$approval" --json > "$TMP/consumed.json"
python3 - "$TMP/consumed.json" <<'PY' || fail "landing grant was not consumed"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "consumed", row
assert row["version"] == 4, row
PY
git -C "$repo" add file.txt
git -C "$repo" commit -qm 'commit approved landing'
printf 'overlap approved landing\n' > "$repo/file.txt"
git -C "$repo" add file.txt
git -C "$repo" commit -qm 'overlap approved landing'
approved_landing_lines="$(wc -l < "$repo/.oms/landings.jsonl" | tr -d ' ')"
if ! "$LAND" --repo "$repo" --recover >"$TMP/recover-consumed-approval.out" 2>&1; then
  sed -n '1,20p' "$TMP/recover-consumed-approval.out" >&2
  fail "consumed approval did not keep its completed landing closed"
fi
grep -Fq 'already complete' "$TMP/recover-consumed-approval.out" ||
  fail "complete terminal did not validate the consumed approval receipt"
[ "$(wc -l < "$repo/.oms/landings.jsonl" | tr -d ' ')" = "$approved_landing_lines" ] ||
  fail "consumed approval recovery appended another terminal row"

# The approved object is the patch bytes, not the pathname. A verifier can
# legitimately run arbitrary project code, so changing the caller-owned patch
# during admission must not change what is ultimately applied.
race_repo="$TMP/race-repo"
mkdir -p "$race_repo"
git -C "$race_repo" init -q
git -C "$race_repo" config user.email test@example.com
git -C "$race_repo" config user.name test
printf 'base\n' > "$race_repo/file.txt"
git -C "$race_repo" add file.txt
git -C "$race_repo" commit -qm base
approved_patch="$TMP/approved.patch"
replacement_patch="$TMP/replacement.patch"
printf 'approved\n' > "$race_repo/file.txt"
git -C "$race_repo" diff --binary > "$approved_patch"
git -C "$race_repo" restore file.txt
printf 'unapproved\n' > "$race_repo/file.txt"
git -C "$race_repo" diff --binary > "$replacement_patch"
git -C "$race_repo" restore file.txt
race_verify="cp '$replacement_patch' '$approved_patch'"
race_approval="$(OMS_REQUIRE_LANDING_APPROVAL=1 "$LAND" --repo "$race_repo" \
  --patch "$approved_patch" --verify "$race_verify" --request-approval)" ||
  fail "could not request race-safe landing approval"
race_grant="$($APPROVAL --repo "$race_repo" decide --approval "$race_approval" \
  --decision approve --expected-version 1 --actor operator)" ||
  fail "could not approve race-safe landing"
OMS_REQUIRE_LANDING_APPROVAL=1 "$LAND" --repo "$race_repo" \
  --patch "$approved_patch" --verify "$race_verify" \
  --approval "$race_approval" --approval-version 2 --approval-token "$race_grant" \
  >/dev/null || fail "frozen approved patch did not land"
grep -Fxq approved "$race_repo/file.txt" ||
  fail "patch pathname changed between approval and apply"

# The landing copy itself is private but not an OS isolation boundary: verifier
# code runs as the same user and can discover .oms/landing-patches. If it moves
# those bytes, admission must fail closed before touching the clean tree.
git -C "$race_repo" restore file.txt
frozen_approved_patch="$TMP/frozen-approved.patch"
printf 'approved again\n' > "$race_repo/file.txt"
git -C "$race_repo" diff --binary > "$frozen_approved_patch"
git -C "$race_repo" restore file.txt
frozen_race_verify="for candidate in '$race_repo'/.oms/landing-patches/*.patch; do cp '$replacement_patch' \"\$candidate\"; done"
if "$LAND" --repo "$race_repo" --patch "$frozen_approved_patch" \
  --verify "$frozen_race_verify" >/dev/null 2>&1; then
  fail "same-UID verifier mutation of the frozen patch was admitted"
fi
grep -Fxq base "$race_repo/file.txt" ||
  fail "frozen-patch mutation changed the main tree"

# An approval requested from one lifecycle attempt is not a bearer grant for
# the same patch outside that attempt. A missing caller attempt must fail just
# like a different one; otherwise dropping OMS_ATTEMPT_ID silently weakens the
# action digest that the operator reviewed.
attempt_repo="$TMP/attempt-bound-repo"
mkdir -p "$attempt_repo"
git -C "$attempt_repo" init -q
git -C "$attempt_repo" config user.email test@example.com
git -C "$attempt_repo" config user.name test
printf 'base\n' > "$attempt_repo/file.txt"
git -C "$attempt_repo" add file.txt
git -C "$attempt_repo" commit -qm base
attempt_patch="$TMP/attempt-bound.patch"
printf 'attempt approved\n' > "$attempt_repo/file.txt"
git -C "$attempt_repo" diff --binary > "$attempt_patch"
git -C "$attempt_repo" restore file.txt
attempt_approval="$(OMS_ATTEMPT_ID=att_exact_owner OMS_REQUIRE_LANDING_APPROVAL=1 \
  "$LAND" --repo "$attempt_repo" --patch "$attempt_patch" --verify true \
  --request-approval)" || fail "could not request attempt-bound approval"
"$APPROVAL" --repo "$attempt_repo" show --approval "$attempt_approval" --json \
  > "$TMP/attempt-bound-request.json"
python3 - "$TMP/attempt-bound-request.json" <<'PY' || fail "approval omitted its attempt binding"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["attempt_id"] == "att_exact_owner", row
PY
attempt_grant="$("$APPROVAL" --repo "$attempt_repo" decide \
  --approval "$attempt_approval" --decision approve --expected-version 1 \
  --actor operator)" || fail "could not approve attempt-bound landing"
if OMS_REQUIRE_LANDING_APPROVAL=1 "$LAND" --repo "$attempt_repo" \
  --patch "$attempt_patch" --verify true --approval "$attempt_approval" \
  --approval-version 2 --approval-token "$attempt_grant" >/dev/null 2>&1; then
  fail "attempt-bound approval was consumed with no current attempt"
fi
if OMS_ATTEMPT_ID=att_different_owner OMS_REQUIRE_LANDING_APPROVAL=1 \
  "$LAND" --repo "$attempt_repo" --patch "$attempt_patch" --verify true \
  --approval "$attempt_approval" --approval-version 2 \
  --approval-token "$attempt_grant" >/dev/null 2>&1; then
  fail "attempt-bound approval was consumed by a different attempt"
fi
grep -Fxq base "$attempt_repo/file.txt" ||
  fail "attempt mismatch still changed the patch target"
"$APPROVAL" --repo "$attempt_repo" show --approval "$attempt_approval" --json \
  > "$TMP/attempt-bound-still-approved.json"
python3 - "$TMP/attempt-bound-still-approved.json" <<'PY' || fail "attempt mismatch consumed the grant"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "approved" and row["version"] == 2, row
PY
OMS_ATTEMPT_ID=att_exact_owner OMS_REQUIRE_LANDING_APPROVAL=1 \
  "$LAND" --repo "$attempt_repo" --patch "$attempt_patch" --verify true \
  --approval "$attempt_approval" --approval-version 2 \
  --approval-token "$attempt_grant" >/dev/null ||
  fail "matching attempt could not consume its approval"
grep -Fxq 'attempt approved' "$attempt_repo/file.txt" ||
  fail "matching attempt did not land its approved patch"

PLAN="$ROOT/scripts/agent-plan.sh"

# Old Soul-bound receipts are preserved, not silently converted into authority.
legacy_repo="$TMP/legacy-soul"
mkdir -p "$legacy_repo"
git -C "$legacy_repo" init -q
git -C "$legacy_repo" config user.email test@example.com
git -C "$legacy_repo" config user.name test
printf 'base\n' > "$legacy_repo/file.txt"
git -C "$legacy_repo" add file.txt
git -C "$legacy_repo" commit -qm base
"$PLAN" --repo "$legacy_repo" init --goal legacy >/dev/null
"$PLAN" --repo "$legacy_repo" add --id old --title old --allowed file.txt --verify true >/dev/null
"$PLAN" --repo "$legacy_repo" claim --id old --provider codex >/dev/null
"$PLAN" --repo "$legacy_repo" review --id old --artifact "$TMP/legacy.md" --patch "$patch" >/dev/null
python3 - "$legacy_repo/.oms/plan/tasks.json" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
row = json.loads(path.read_text())
row['tasks']['old'].update(executor_id='retired', executor_soul_sha256='a' * 64)
path.write_text(json.dumps(row))
PY
cp "$legacy_repo/.oms/plan/tasks.json" "$TMP/legacy-before.json"
for tool in peer-delegate plan-run patch-admit patch-land; do
  if "$ROOT/scripts/$tool.sh" --executor retired --repo "$legacy_repo" >"$TMP/legacy-out" 2>&1; then
    fail "$tool accepted a retired executor"
  fi
  grep -Fq 'Soul executors were removed' "$TMP/legacy-out" || fail "$tool did not explain removal"
done
for invocation in 'plan-run --to codex --id old --land --dry-run' \
  'peer-delegate --to codex --plan-task old --prompt legacy --dry-run' \
  'patch-admit --plan-task old' 'patch-land --plan-task old'; do
  # shellcheck disable=SC2086
  set -- $invocation
  legacy_tool="$1"; shift
  case "$legacy_tool" in patch-*) set -- "$@" --patch "$patch" ;; esac
  if "$ROOT/scripts/$legacy_tool.sh" --repo "$legacy_repo" "$@" >"$TMP/legacy-out" 2>&1; then
    fail "$legacy_tool accepted a legacy receipt without its executor"
  fi
  grep -Fq 'Soul executor receipt is retired' "$TMP/legacy-out" ||
    fail "$legacy_tool did not reject the legacy receipt: $(cat "$TMP/legacy-out")"
done
cmp "$TMP/legacy-before.json" "$legacy_repo/.oms/plan/tasks.json" || fail "legacy rejection changed plan evidence"
grep -Fxq base "$legacy_repo/file.txt" || fail "legacy rejection applied a patch"

# A reviewed plan task binds patch bytes, not a caller-selected pathname. A
# goal driver may hand landing a private copy, but it must hash exactly like
# the stored review evidence. Missing, vanished, or unreadable evidence cannot
# authorize a caller-provided replacement.
plan_patch_repo="$TMP/plan-patch-receipt-repo"
mkdir -p "$plan_patch_repo"
git -C "$plan_patch_repo" init -q
git -C "$plan_patch_repo" config user.email test@example.com
git -C "$plan_patch_repo" config user.name test
printf 'base\n' > "$plan_patch_repo/file.txt"
git -C "$plan_patch_repo" add file.txt
git -C "$plan_patch_repo" commit -qm base
stored_review_patch="$TMP/stored-plan-review.patch"
different_review_patch="$TMP/different-plan-review.patch"
copied_review_patch="$TMP/copied-plan-review.patch"
vanished_review_patch="$TMP/vanished-plan-review.patch"
unreadable_review_patch="$TMP/unreadable-plan-review.patch"
relative_review_dir="$plan_patch_repo/.oms/review-patches"
relative_review_patch="$relative_review_dir/review.patch"
printf 'stored review\n' > "$plan_patch_repo/file.txt"
git -C "$plan_patch_repo" diff --binary > "$stored_review_patch"
git -C "$plan_patch_repo" restore file.txt
printf 'different review\n' > "$plan_patch_repo/file.txt"
git -C "$plan_patch_repo" diff --binary > "$different_review_patch"
git -C "$plan_patch_repo" restore file.txt
cp "$stored_review_patch" "$copied_review_patch"
cp "$stored_review_patch" "$vanished_review_patch"
cp "$stored_review_patch" "$unreadable_review_patch"
mkdir -p "$relative_review_dir"
cp "$stored_review_patch" "$relative_review_patch"
printf 'reviewed\n' > "$TMP/plan-patch-artifact.md"

"$PLAN" --repo "$plan_patch_repo" init --goal test >/dev/null
for plan_patch_task in bytes-bound empty-evidence vanished-evidence unreadable-evidence relative-evidence; do
  "$PLAN" --repo "$plan_patch_repo" add --id "$plan_patch_task" \
    --title "$plan_patch_task" --allowed file.txt --verify true >/dev/null
  "$PLAN" --repo "$plan_patch_repo" claim --id "$plan_patch_task" \
    --provider codex >/dev/null
done
"$PLAN" --repo "$plan_patch_repo" add --id empty-verifier \
  --title empty-verifier --allowed file.txt >/dev/null
"$PLAN" --repo "$plan_patch_repo" claim --id empty-verifier \
  --provider codex >/dev/null
bytes_bound_lease="$("$PLAN" --repo "$plan_patch_repo" show --id bytes-bound |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])' | tr -d '\r')"
empty_evidence_lease="$("$PLAN" --repo "$plan_patch_repo" show --id empty-evidence |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])' | tr -d '\r')"
vanished_evidence_lease="$("$PLAN" --repo "$plan_patch_repo" show --id vanished-evidence |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])' | tr -d '\r')"
unreadable_evidence_lease="$("$PLAN" --repo "$plan_patch_repo" show --id unreadable-evidence |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])' | tr -d '\r')"
relative_evidence_lease="$("$PLAN" --repo "$plan_patch_repo" show --id relative-evidence |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])' | tr -d '\r')"
empty_verifier_lease="$("$PLAN" --repo "$plan_patch_repo" show --id empty-verifier |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])' | tr -d '\r')"

"$PLAN" --repo "$plan_patch_repo" review --id bytes-bound \
  --lease-id "$bytes_bound_lease" --artifact "$TMP/plan-patch-artifact.md" \
  --patch "$stored_review_patch" >/dev/null
"$PLAN" --repo "$plan_patch_repo" review --id empty-evidence \
  --lease-id "$empty_evidence_lease" --artifact "$TMP/plan-patch-artifact.md" >/dev/null
"$PLAN" --repo "$plan_patch_repo" review --id vanished-evidence \
  --lease-id "$vanished_evidence_lease" --artifact "$TMP/plan-patch-artifact.md" \
  --patch "$vanished_review_patch" >/dev/null
"$PLAN" --repo "$plan_patch_repo" review --id unreadable-evidence \
  --lease-id "$unreadable_evidence_lease" --artifact "$TMP/plan-patch-artifact.md" \
  --patch "$unreadable_review_patch" >/dev/null
"$PLAN" --repo "$plan_patch_repo" review --id relative-evidence \
  --lease-id "$relative_evidence_lease" --artifact "$TMP/plan-patch-artifact.md" \
  --patch .oms/review-patches/review.patch >/dev/null
"$PLAN" --repo "$plan_patch_repo" review --id empty-verifier \
  --lease-id "$empty_verifier_lease" --artifact "$TMP/plan-patch-artifact.md" \
  --patch "$stored_review_patch" >/dev/null
rm -f "$vanished_review_patch"
chmod 000 "$unreadable_review_patch" 2>/dev/null || true

if "$LAND" --repo "$plan_patch_repo" --plan-task bytes-bound \
  --patch "$different_review_patch" --verify true >/dev/null 2>&1; then
  fail "plan review accepted caller-selected patch bytes"
fi
if "$LAND" --repo "$plan_patch_repo" --plan-task bytes-bound \
  --patch "$copied_review_patch" --verify : >/dev/null 2>&1; then
  fail "plan review accepted a different caller verifier"
fi
if "$LAND" --repo "$plan_patch_repo" --plan-task bytes-bound \
  --patch "$copied_review_patch" --executor receiptless-executor \
  --verify true >/dev/null 2>&1; then
  fail "executor-free plan review accepted a caller-added executor"
fi
if "$LAND" --repo "$plan_patch_repo" --plan-task empty-evidence \
  --patch "$copied_review_patch" --verify true >/dev/null 2>&1; then
  fail "plan review without stored patch evidence accepted caller bytes"
fi
if "$LAND" --repo "$plan_patch_repo" --plan-task vanished-evidence \
  --patch "$copied_review_patch" --verify true >/dev/null 2>&1; then
  fail "plan review whose stored patch vanished accepted caller bytes"
fi
if [ ! -r "$unreadable_review_patch" ]; then
  if "$LAND" --repo "$plan_patch_repo" --plan-task unreadable-evidence \
    --patch "$copied_review_patch" --verify true >/dev/null 2>&1; then
    fail "plan review with unreadable stored patch accepted caller bytes"
  fi
fi
if "$LAND" --repo "$plan_patch_repo" --plan-task empty-verifier \
  --patch "$copied_review_patch" --verify true >/dev/null 2>&1; then
  fail "plan review without stored verifier accepted caller verifier"
fi
chmod 600 "$unreadable_review_patch" 2>/dev/null || true
grep -Fxq base "$plan_patch_repo/file.txt" ||
  fail "plan patch evidence mismatch still changed the target"

# Relative review evidence is interpreted against the target repository, not
# whichever directory happens to launch patch-land. Requesting approval is a
# mutation-free proof that an omitted caller path resolves to those bytes.
relative_request="$("$LAND" --repo "$plan_patch_repo" \
  --plan-task relative-evidence --request-approval)" ||
  fail "repo-relative review patch did not resolve"
case "$relative_request" in apr_*) ;; *) fail "relative patch produced bad approval id" ;; esac

# A different path is legitimate when its exact bytes are the reviewed bytes.
"$LAND" --repo "$plan_patch_repo" --plan-task bytes-bound \
  --patch "$copied_review_patch" --verify true >/dev/null ||
  fail "byte-identical private copy did not satisfy plan patch receipt"
grep -Fxq 'stored review' "$plan_patch_repo/file.txt" ||
  fail "byte-identical plan patch copy applied the wrong content"

# Recovery and live landing share the same non-blocking repository lock. A
# concurrent recovery must not inspect and abandon an intent still being
# processed by the live owner.
lock_repo="$TMP/lock-repo"
mkdir -p "$lock_repo/.oms"
git -C "$lock_repo" init -q
git -C "$lock_repo" config user.email test@example.com
git -C "$lock_repo" config user.name test
printf 'base\n' > "$lock_repo/file.txt"
git -C "$lock_repo" add file.txt
git -C "$lock_repo" commit -qm base
lock_patch="$TMP/lock.patch"
printf 'later\n' > "$lock_repo/file.txt"
git -C "$lock_repo" diff --binary > "$lock_patch"
git -C "$lock_repo" restore file.txt
python3 - "$lock_repo/.oms/landings.jsonl" "$lock_patch" "$lock_repo" <<'PY'
import hashlib, json, pathlib, subprocess, sys
path, patch, repo = sys.argv[1:]
row = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-live-owner",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
    "base_sha": subprocess.check_output(
        ["git", "-C", repo, "rev-parse", "HEAD"], text=True
    ).strip(),
}
pathlib.Path(path).write_text(json.dumps(row) + "\n", encoding="utf-8")
PY
cp "$lock_repo/.oms/landings.jsonl" "$TMP/landings-before"
release_marker="$TMP/release-landing-lock"
: > "$release_marker"
(
  . "$ROOT/scripts/lib/file-lock.sh"
  oms_hold_file_lock "$lock_repo/.oms/landings.jsonl" 7 || exit 1
  : > "$TMP/landing-lock-ready"
  while [ -e "$release_marker" ]; do sleep 0.05; done
  oms_release_held_file_lock
) &
holder_pid=$!
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  [ -e "$TMP/landing-lock-ready" ] && break
  sleep 0.05
done
[ -e "$TMP/landing-lock-ready" ] || fail "landing lock holder did not start"
set +e
"$LAND" --repo "$lock_repo" --recover >"$TMP/recover-locked.out" 2>&1
recover_rc=$?
set -e
rm -f "$release_marker"
wait "$holder_pid" || fail "landing lock holder failed"
[ "$recover_rc" -ne 0 ] || fail "recovery ran concurrently with a live landing"
cmp -s "$TMP/landings-before" "$lock_repo/.oms/landings.jsonl" ||
  fail "locked recovery changed the live landing intent"

# If neither forward nor reverse apply is clean, the current tree cannot prove
# whether an interrupted patch was applied and later edited or was never
# applied at all. Recovery must preserve the intent for manual resolution,
# rather than minting a false abandoned/failed receipt.
ambiguous_repo="$TMP/ambiguous-repo"
mkdir -p "$ambiguous_repo/.oms/landing-patches"
git -C "$ambiguous_repo" init -q
git -C "$ambiguous_repo" config user.email test@example.com
git -C "$ambiguous_repo" config user.name test
printf 'base\n' > "$ambiguous_repo/file.txt"
git -C "$ambiguous_repo" add file.txt
git -C "$ambiguous_repo" commit -qm base
ambiguous_patch="$ambiguous_repo/.oms/landing-patches/land-ambiguous.patch"
printf 'landed\n' > "$ambiguous_repo/file.txt"
git -C "$ambiguous_repo" diff --binary > "$ambiguous_patch"
git -C "$ambiguous_repo" restore file.txt
printf 'later-edit\n' > "$ambiguous_repo/file.txt"
python3 - "$ambiguous_repo/.oms/landings.jsonl" "$ambiguous_patch" \
  "$ambiguous_repo" <<'PY'
import hashlib, json, pathlib, subprocess, sys
path, patch, repo = sys.argv[1:]
row = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-ambiguous",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
    "base_sha": subprocess.check_output(
        ["git", "-C", repo, "rev-parse", "HEAD"], text=True
    ).strip(),
}
pathlib.Path(path).write_text(json.dumps(row) + "\n", encoding="utf-8")
PY
set +e
"$LAND" --repo "$ambiguous_repo" --recover >"$TMP/recover-ambiguous.out" 2>&1
ambiguous_rc=$?
set -e
[ "$ambiguous_rc" -ne 0 ] || fail "ambiguous recovery guessed that the patch was not applied"
grep -Fq 'manual recovery' "$TMP/recover-ambiguous.out" ||
  fail "ambiguous recovery did not explain the manual decision"
if ! python3 - "$ambiguous_repo/.oms/landings.jsonl" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
assert [row["event"] for row in rows] == ["intent"], rows
PY
then
  fail "ambiguous recovery wrote a false terminal row"
fi

# Losing the frozen patch is loss of evidence, not proof that no apply
# happened. Recovery must keep the intent outstanding for an operator instead
# of closing it as abandoned.
missing_repo="$TMP/missing-patch-repo"
mkdir -p "$missing_repo/.oms"
git -C "$missing_repo" init -q
git -C "$missing_repo" config user.email test@example.com
git -C "$missing_repo" config user.name test
printf 'base\n' > "$missing_repo/file.txt"
git -C "$missing_repo" add file.txt
git -C "$missing_repo" commit -qm base
printf '%s\n' \
  '{"schema":1,"ts":"2026-08-09T00:00:00Z","landing_id":"land-missing","event":"intent","patch":"/definitely/missing.patch","patch_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}' \
  > "$missing_repo/.oms/landings.jsonl"
set +e
"$LAND" --repo "$missing_repo" --recover >"$TMP/recover-missing.out" 2>&1
missing_rc=$?
set -e
[ "$missing_rc" -ne 0 ] || fail "missing recovery evidence was marked terminal"
grep -Fq 'manual recovery' "$TMP/recover-missing.out" ||
  fail "missing recovery evidence did not request manual recovery"
if grep -Eq '"event": ?"(complete|abandoned)"' "$missing_repo/.oms/landings.jsonl"; then
  fail "missing frozen bytes produced a false terminal landing"
fi

# Terminal rows are appendable evidence, not authority. A provider can append a
# minimal row to shared JSONL, so a forged `complete` must not hide an unapplied
# intent and a forged `abandoned` must not hide an applied one. Recovery must
# converge the external tree/lineage state, append the correct terminal result,
# and become idempotent rather than growing the journal on every retry.
forged_unapplied_repo="$TMP/forged-unapplied-repo"
mkdir -p "$forged_unapplied_repo/.oms/landing-patches"
git -C "$forged_unapplied_repo" init -q
git -C "$forged_unapplied_repo" config user.email test@example.com
git -C "$forged_unapplied_repo" config user.name test
printf 'base\n' > "$forged_unapplied_repo/file.txt"
git -C "$forged_unapplied_repo" add file.txt
git -C "$forged_unapplied_repo" commit -qm base
forged_unapplied_patch="$forged_unapplied_repo/.oms/landing-patches/land-forged-unapplied.patch"
printf 'changed\n' > "$forged_unapplied_repo/file.txt"
git -C "$forged_unapplied_repo" diff --binary > "$forged_unapplied_patch"
git -C "$forged_unapplied_repo" restore file.txt
python3 - "$forged_unapplied_repo/.oms/landings.jsonl" \
  "$forged_unapplied_patch" "$forged_unapplied_repo" <<'PY'
import hashlib, json, pathlib, subprocess, sys
path, patch, repo = sys.argv[1:]
intent = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-forged-unapplied",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
    "base_sha": subprocess.check_output(
        ["git", "-C", repo, "rev-parse", "HEAD"], text=True
    ).strip(),
}
with open(path, "w", encoding="utf-8") as handle:
    handle.write(json.dumps(intent) + "\n")
    handle.write(json.dumps({
        "landing_id": "land-forged-unapplied", "event": "complete",
    }) + "\n")
PY
"$LAND" --repo "$forged_unapplied_repo" --recover \
  >"$TMP/recover-forged-unapplied.out" 2>&1 ||
  fail "forged complete prevented unapplied recovery"
grep -Fq 'never applied' "$TMP/recover-forged-unapplied.out" ||
  fail "forged complete still hid the unapplied intent"
python3 - "$forged_unapplied_repo/.oms/landings.jsonl" <<'PY' ||
import json, sys
events = [json.loads(line).get("event") for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
assert events == ["intent", "complete", "abandoned"], events
PY
  fail "unapplied recovery did not supersede the forged terminal row"
forged_unapplied_lines="$(wc -l < "$forged_unapplied_repo/.oms/landings.jsonl" | tr -d ' ')"
"$LAND" --repo "$forged_unapplied_repo" --recover >/dev/null 2>&1 ||
  fail "converged unapplied recovery was not idempotent"
[ "$(wc -l < "$forged_unapplied_repo/.oms/landings.jsonl" | tr -d ' ')" = "$forged_unapplied_lines" ] ||
  fail "converged unapplied recovery kept appending terminal rows"

forged_applied_repo="$TMP/forged-applied-repo"
mkdir -p "$forged_applied_repo/.oms/landing-patches"
git -C "$forged_applied_repo" init -q
git -C "$forged_applied_repo" config user.email test@example.com
git -C "$forged_applied_repo" config user.name test
printf 'base\n' > "$forged_applied_repo/file.txt"
git -C "$forged_applied_repo" add file.txt
git -C "$forged_applied_repo" commit -qm base
forged_applied_patch="$forged_applied_repo/.oms/landing-patches/land-forged-applied.patch"
printf 'changed\n' > "$forged_applied_repo/file.txt"
git -C "$forged_applied_repo" diff --binary > "$forged_applied_patch"
git -C "$forged_applied_repo" restore file.txt
python3 - "$forged_applied_repo/.oms/landings.jsonl" \
  "$forged_applied_patch" "$forged_applied_repo" <<'PY'
import hashlib, json, pathlib, subprocess, sys
path, patch, repo = sys.argv[1:]
intent = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-forged-applied",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
    "base_sha": subprocess.check_output(
        ["git", "-C", repo, "rev-parse", "HEAD"], text=True
    ).strip(),
}
with open(path, "w", encoding="utf-8") as handle:
    handle.write(json.dumps(intent) + "\n")
    handle.write(json.dumps({
        "landing_id": "land-forged-applied", "event": "abandoned",
    }) + "\n")
PY
git -C "$forged_applied_repo" apply --binary "$forged_applied_patch"
"$LAND" --repo "$forged_applied_repo" --recover \
  >"$TMP/recover-forged-applied.out" 2>&1 ||
  fail "forged abandoned prevented applied recovery"
grep -Fq 'patch was applied' "$TMP/recover-forged-applied.out" ||
  fail "forged abandoned still hid the applied intent"
[ "$(grep -c '"kind": "patch-land"' "$forged_applied_repo/.oms/artifacts/index.jsonl")" = 1 ] ||
  fail "applied recovery did not converge exact lineage"
python3 - "$forged_applied_repo/.oms/landings.jsonl" <<'PY' ||
import json, sys
events = [json.loads(line).get("event") for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
assert events == ["intent", "abandoned", "complete"], events
PY
  fail "applied recovery did not supersede the forged terminal row"
forged_applied_lines="$(wc -l < "$forged_applied_repo/.oms/landings.jsonl" | tr -d ' ')"
"$LAND" --repo "$forged_applied_repo" --recover >/dev/null 2>&1 ||
  fail "converged applied recovery was not idempotent"
[ "$(wc -l < "$forged_applied_repo/.oms/landings.jsonl" | tr -d ' ')" = "$forged_applied_lines" ] ||
  fail "converged applied recovery kept appending terminal rows"

# Reverse-apply proves only that the bytes are currently present. If HEAD moved
# after an intent was recorded, a different commit may have introduced those
# same bytes. An open intent must not claim that commit as OMS lineage or append
# a terminal receipt; only an exact historical terminal may survive base drift.
base_drift_repo="$TMP/base-drift-repo"
mkdir -p "$base_drift_repo/.oms/landing-patches"
git -C "$base_drift_repo" init -q
git -C "$base_drift_repo" config user.email test@example.com
git -C "$base_drift_repo" config user.name test
printf 'base\n' > "$base_drift_repo/file.txt"
git -C "$base_drift_repo" add file.txt
git -C "$base_drift_repo" commit -qm base
base_drift_base="$(git -C "$base_drift_repo" rev-parse HEAD)"
base_drift_patch="$base_drift_repo/.oms/landing-patches/land-base-drift.patch"
printf 'same bytes from another commit\n' > "$base_drift_repo/file.txt"
git -C "$base_drift_repo" diff --binary > "$base_drift_patch"
git -C "$base_drift_repo" restore file.txt
python3 - "$base_drift_repo/.oms/landings.jsonl" "$base_drift_patch" \
  "$base_drift_base" <<'PY'
import hashlib, json, pathlib, sys

path, patch, base = sys.argv[1:]
row = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-base-drift",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
    "base_sha": base,
    "task": "",
    "lease": "",
    "plan_receipt_sha": "",
    "plan_done_receipt_sha": "",
    "approval": "",
    "approval_version": "",
}
fields = (
    "landing_id", "patch", "patch_sha", "base_sha", "task", "lease",
    "plan_receipt_sha", "plan_done_receipt_sha", "approval", "approval_version",
)
canonical = {"schema": 1}
canonical.update({name: row[name] for name in fields})
row["receipt_sha"] = hashlib.sha256(json.dumps(
    canonical, ensure_ascii=False, sort_keys=True, separators=(",", ":")
).encode()).hexdigest()
pathlib.Path(path).write_text(json.dumps(row) + "\n", encoding="utf-8")
PY
printf 'same bytes from another commit\n' > "$base_drift_repo/file.txt"
git -C "$base_drift_repo" add file.txt
git -C "$base_drift_repo" commit -qm 'independent matching change'
base_drift_head="$(git -C "$base_drift_repo" rev-parse HEAD)"
set +e
"$LAND" --repo "$base_drift_repo" --recover >"$TMP/recover-base-drift.out" 2>&1
base_drift_rc=$?
set -e
[ "$base_drift_rc" -ne 0 ] ||
  fail "open landing intent claimed matching bytes from a different base commit"
grep -Fq 'HEAD no longer matches its exact intent base' "$TMP/recover-base-drift.out" ||
  fail "base-drift recovery did not explain its fail-closed decision"
[ "$(git -C "$base_drift_repo" rev-parse HEAD)" = "$base_drift_head" ] ||
  fail "base-drift recovery changed HEAD"
python3 - "$base_drift_repo/.oms/landings.jsonl" \
  "$base_drift_repo/.oms/artifacts/index.jsonl" <<'PY' ||
import json, pathlib, sys

landings, index = map(pathlib.Path, sys.argv[1:])
rows = [json.loads(line) for line in landings.read_text(encoding="utf-8").splitlines()
        if line.strip()]
assert [row.get("event") for row in rows] == ["intent"], rows
if index.exists():
    lineage = [json.loads(line) for line in index.read_text(encoding="utf-8").splitlines()
               if line.strip()]
    assert not any(row.get("kind") == "patch-land" for row in lineage), lineage
PY
  fail "base-drift recovery wrote false lineage or a terminal receipt"

# Once a complete terminal is bound to its full intent, durable lineage is the
# historical proof. A later commit may overlap the same hunk so neither current
# forward nor reverse apply works; that must not reopen an already completed
# landing transaction.
historical_complete_repo="$TMP/historical-complete-repo"
mkdir -p "$historical_complete_repo"
git -C "$historical_complete_repo" init -q
git -C "$historical_complete_repo" config user.email test@example.com
git -C "$historical_complete_repo" config user.name test
printf 'base\n' > "$historical_complete_repo/file.txt"
git -C "$historical_complete_repo" add file.txt
git -C "$historical_complete_repo" commit -qm base
historical_complete_patch="$TMP/historical-complete.patch"
printf 'first landing\n' > "$historical_complete_repo/file.txt"
git -C "$historical_complete_repo" diff --binary > "$historical_complete_patch"
git -C "$historical_complete_repo" restore file.txt
"$LAND" --repo "$historical_complete_repo" --patch "$historical_complete_patch" \
  --verify true >/dev/null || fail "historical complete fixture did not land"
git -C "$historical_complete_repo" add file.txt
git -C "$historical_complete_repo" commit -qm 'land first patch'
printf 'overlapping later commit\n' > "$historical_complete_repo/file.txt"
git -C "$historical_complete_repo" add file.txt
git -C "$historical_complete_repo" commit -qm 'overlap landed line'
historical_complete_lines="$(wc -l < "$historical_complete_repo/.oms/landings.jsonl" | tr -d ' ')"
"$LAND" --repo "$historical_complete_repo" --recover \
  >"$TMP/recover-historical-complete.out" 2>&1 ||
  fail "later overlapping commit reopened an exact completed landing"
grep -Fq 'already complete' "$TMP/recover-historical-complete.out" ||
  fail "historical complete was not closed from durable receipts"
[ "$(wc -l < "$historical_complete_repo/.oms/landings.jsonl" | tr -d ' ')" = "$historical_complete_lines" ] ||
  fail "historical complete recovery appended another terminal row"

# A known-unapplied patch is not fully recovered until its approval and plan
# receipts are reconciled. If the approval record cannot be read/finalized,
# keep the intent retryable rather than hiding the broken receipt behind an
# abandoned terminal row.
receipt_repo="$TMP/receipt-failure-repo"
mkdir -p "$receipt_repo/.oms/landing-patches"
git -C "$receipt_repo" init -q
git -C "$receipt_repo" config user.email test@example.com
git -C "$receipt_repo" config user.name test
printf 'base\n' > "$receipt_repo/file.txt"
git -C "$receipt_repo" add file.txt
git -C "$receipt_repo" commit -qm base
receipt_patch="$receipt_repo/.oms/landing-patches/land-receipt-failure.patch"
printf 'changed\n' > "$receipt_repo/file.txt"
git -C "$receipt_repo" diff --binary > "$receipt_patch"
git -C "$receipt_repo" restore file.txt
python3 - "$receipt_repo/.oms/landings.jsonl" "$receipt_patch" "$receipt_repo" <<'PY'
import hashlib, json, pathlib, subprocess, sys
path, patch, repo = sys.argv[1:]
row = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-receipt-failure",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
    "base_sha": subprocess.check_output(
        ["git", "-C", repo, "rev-parse", "HEAD"], text=True
    ).strip(),
    "approval": "apr_missing",
    "approval_version": "3",
}
pathlib.Path(path).write_text(json.dumps(row) + "\n", encoding="utf-8")
PY
set +e
"$LAND" --repo "$receipt_repo" --recover >"$TMP/recover-receipt.out" 2>&1
receipt_rc=$?
set -e
[ "$receipt_rc" -ne 0 ] || fail "failed approval receipt was hidden by terminal recovery"
if grep -Eq '"event": ?"(complete|abandoned)"' "$receipt_repo/.oms/landings.jsonl"; then
  fail "failed approval receipt produced a false terminal landing"
fi
grep -Fq '"event": "not-applied-pending-receipt"' "$receipt_repo/.oms/landings.jsonl" ||
  fail "failed approval receipt did not record its retryable state"
"$ROOT/scripts/run.sh" validate --dir "$receipt_repo/.oms" >/dev/null ||
  fail "state validator rejected a retryable not-applied landing receipt"

# A crash can happen after the lineage append and before the terminal landing
# receipt. Recovery must recognize that exact frozen patch row and avoid
# appending a second successful lineage event.
dedupe_repo="$TMP/lineage-dedupe-repo"
mkdir -p "$dedupe_repo/.oms/landing-patches"
git -C "$dedupe_repo" init -q
git -C "$dedupe_repo" config user.email test@example.com
git -C "$dedupe_repo" config user.name test
printf 'base\n' > "$dedupe_repo/file.txt"
git -C "$dedupe_repo" add file.txt
git -C "$dedupe_repo" commit -qm base
dedupe_patch="$dedupe_repo/.oms/landing-patches/land-lineage-dedupe.patch"
printf 'changed\n' > "$dedupe_repo/file.txt"
git -C "$dedupe_repo" diff --binary > "$dedupe_patch"
git -C "$dedupe_repo" restore file.txt
python3 - "$dedupe_repo/.oms/landings.jsonl" "$dedupe_patch" "$dedupe_repo" <<'PY'
import hashlib, json, pathlib, subprocess, sys
path, patch, repo = sys.argv[1:]
row = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-lineage-dedupe",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
    "base_sha": subprocess.check_output(
        ["git", "-C", repo, "rev-parse", "HEAD"], text=True
    ).strip(),
}
pathlib.Path(path).write_text(json.dumps(row) + "\n", encoding="utf-8")
PY
git -C "$dedupe_repo" apply --binary "$dedupe_patch"
(
  . "$ROOT/scripts/lib/peer-common.sh"
  ma_append_artifact_index "$dedupe_repo" patch-land "" 0 "" "$dedupe_patch"
)
"$LAND" --repo "$dedupe_repo" --recover >"$TMP/recover-dedupe.out" 2>&1 ||
  fail "lineage-dedupe recovery did not finish"
[ "$(grep -c '"kind": "patch-land"' "$dedupe_repo/.oms/artifacts/index.jsonl")" = 1 ] ||
  fail "recovery duplicated an existing exact patch-land lineage row"

# A corrupt artifact index could hide the exact receipt recovery is looking
# for. Do not append beside unknown state and call the landing complete.
corrupt_index_repo="$TMP/corrupt-index-recovery-repo"
mkdir -p "$corrupt_index_repo/.oms/landing-patches" "$corrupt_index_repo/.oms/artifacts"
git -C "$corrupt_index_repo" init -q
git -C "$corrupt_index_repo" config user.email test@example.com
git -C "$corrupt_index_repo" config user.name test
printf 'base\n' > "$corrupt_index_repo/file.txt"
git -C "$corrupt_index_repo" add file.txt
git -C "$corrupt_index_repo" commit -qm base
corrupt_index_patch="$corrupt_index_repo/.oms/landing-patches/land-corrupt-index.patch"
printf 'changed\n' > "$corrupt_index_repo/file.txt"
git -C "$corrupt_index_repo" diff --binary > "$corrupt_index_patch"
git -C "$corrupt_index_repo" restore file.txt
python3 - "$corrupt_index_repo/.oms/landings.jsonl" "$corrupt_index_patch" \
  "$corrupt_index_repo" <<'PY'
import hashlib, json, pathlib, subprocess, sys
path, patch, repo = sys.argv[1:]
row = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-corrupt-index",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
    "base_sha": subprocess.check_output(
        ["git", "-C", repo, "rev-parse", "HEAD"], text=True
    ).strip(),
}
pathlib.Path(path).write_text(json.dumps(row) + "\n", encoding="utf-8")
PY
git -C "$corrupt_index_repo" apply --binary "$corrupt_index_patch"
printf 'not-json\n' > "$corrupt_index_repo/.oms/artifacts/index.jsonl"
set +e
"$LAND" --repo "$corrupt_index_repo" --recover >"$TMP/recover-corrupt-index.out" 2>&1
corrupt_index_rc=$?
set -e
[ "$corrupt_index_rc" -ne 0 ] || fail "recovery accepted a corrupt lineage index"
if grep -Eq '"event": ?"(complete|abandoned)"' "$corrupt_index_repo/.oms/landings.jsonl"; then
  fail "corrupt lineage index produced a false terminal landing"
fi
[ "$(cat "$corrupt_index_repo/.oms/artifacts/index.jsonl")" = not-json ] ||
  fail "recovery appended beside a corrupt lineage index"

# A same-lease repair can publish a new reviewed receipt while the old patch is
# still inside its admission verifier. The final review -> landing transition
# must compare-and-set the exact receipt that admission checked; otherwise the
# old patch can land over the newer review simply because its lease did not
# change. A rejected stale fence must preserve the replacement review.
receipt_cas_repo="$TMP/plan-receipt-cas-repo"
receipt_cas_barrier="$TMP/plan-receipt-cas-barrier"
mkdir -p "$receipt_cas_repo" "$receipt_cas_barrier"
git -C "$receipt_cas_repo" init -q
git -C "$receipt_cas_repo" config user.email test@example.com
git -C "$receipt_cas_repo" config user.name test
printf 'base\n' > "$receipt_cas_repo/file.txt"
git -C "$receipt_cas_repo" add file.txt
git -C "$receipt_cas_repo" commit -qm base
receipt_cas_patch_a="$TMP/plan-receipt-cas-a.patch"
receipt_cas_patch_b="$TMP/plan-receipt-cas-b.patch"
receipt_cas_artifact_a="$TMP/plan-receipt-cas-a.md"
receipt_cas_artifact_b="$TMP/plan-receipt-cas-b.md"
receipt_cas_repair_artifact="$TMP/plan-receipt-cas-repair.md"
printf 'review A\n' > "$receipt_cas_repo/file.txt"
git -C "$receipt_cas_repo" diff --binary > "$receipt_cas_patch_a"
git -C "$receipt_cas_repo" restore file.txt
printf 'review B\n' > "$receipt_cas_repo/file.txt"
git -C "$receipt_cas_repo" diff --binary > "$receipt_cas_patch_b"
git -C "$receipt_cas_repo" restore file.txt
printf 'review A\n' > "$receipt_cas_artifact_a"
printf 'review B\n' > "$receipt_cas_artifact_b"
printf 'superseded during admission\n' > "$receipt_cas_repair_artifact"
receipt_cas_verify="mkdir -p '$receipt_cas_barrier'; : > '$receipt_cas_barrier/ready'; while [ ! -f '$receipt_cas_barrier/release' ]; do sleep 0.05; done; true"
"$PLAN" --repo "$receipt_cas_repo" init --goal test >/dev/null
"$PLAN" --repo "$receipt_cas_repo" add --id t1 --title test \
  --allowed file.txt --verify "$receipt_cas_verify" >/dev/null
"$PLAN" --repo "$receipt_cas_repo" claim --id t1 --provider codex >/dev/null
receipt_cas_lease="$("$PLAN" --repo "$receipt_cas_repo" show --id t1 |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])' | tr -d '\r')"
"$PLAN" --repo "$receipt_cas_repo" review --id t1 \
  --lease-id "$receipt_cas_lease" --artifact "$receipt_cas_artifact_a" \
  --patch "$receipt_cas_patch_a" >/dev/null
"$PLAN" --repo "$receipt_cas_repo" show --id t1 > "$TMP/plan-receipt-cas-review-a.json"
receipt_cas_review_a_sha="$(python3 -c '
import hashlib,json,sys
d=json.load(sys.stdin)
for name in ("state", "updated", "claim_expired", "claim_age_s"):
    d.pop(name, None)
print(hashlib.sha256(json.dumps(
 d,sort_keys=True,separators=(",",":"),ensure_ascii=False).encode()).hexdigest())
' < "$TMP/plan-receipt-cas-review-a.json" | tr -d '\r')"
"$LAND" --repo "$receipt_cas_repo" --plan-task t1 \
  >"$TMP/plan-receipt-cas.out" 2>&1 &
receipt_cas_pid=$!
receipt_cas_ready=0
receipt_cas_wait=0
while [ "$receipt_cas_wait" -lt 200 ]; do
  if [ -f "$receipt_cas_barrier/ready" ]; then
    receipt_cas_ready=1
    break
  fi
  sleep 0.05
  receipt_cas_wait=$((receipt_cas_wait + 1))
done
if [ "$receipt_cas_ready" != 1 ]; then
  : > "$receipt_cas_barrier/release"
  wait "$receipt_cas_pid" >/dev/null 2>&1 || true
  fail "receipt CAS verifier did not reach its barrier"
fi
"$PLAN" --repo "$receipt_cas_repo" repair --id t1 \
  --lease-id "$receipt_cas_lease" --artifact "$receipt_cas_repair_artifact" >/dev/null
"$PLAN" --repo "$receipt_cas_repo" review --id t1 \
  --lease-id "$receipt_cas_lease" --artifact "$receipt_cas_artifact_b" \
  --patch "$receipt_cas_patch_b" >/dev/null
: > "$receipt_cas_barrier/release"
set +e
wait "$receipt_cas_pid"
receipt_cas_rc=$?
set -e
[ "$receipt_cas_rc" -ne 0 ] || fail "a superseded same-lease review patch landed"
grep -Fxq base "$receipt_cas_repo/file.txt" ||
  fail "stale reviewed bytes changed the main tree"
[ -z "$(git -C "$receipt_cas_repo" status --porcelain --untracked-files=all)" ] ||
  fail "stale review fence left the main tree dirty"
"$PLAN" --repo "$receipt_cas_repo" show --id t1 > "$TMP/plan-receipt-cas-task.json"
python3 - "$TMP/plan-receipt-cas-task.json" "$receipt_cas_patch_b" \
  "$receipt_cas_artifact_b" "$receipt_cas_verify" "$receipt_cas_lease" <<'PY' ||
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "review", row
assert row["patch"] == sys.argv[2], row
assert row["artifact"] == sys.argv[3], row
assert row["verify"] == sys.argv[4], row
assert row["lease_id"] == sys.argv[5] == row["review_lease_id"], row
assert row["repair_count"] == 1, row
PY
  fail "stale fence did not preserve the superseding review receipt"
python3 - "$receipt_cas_repo/.oms/landings.jsonl" <<'PY' ||
import json, sys
events = [json.loads(line)["event"] for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
assert events == ["intent", "abandoned"], events
PY
  fail "stale receipt fence was not closed by an abandoned landing receipt"

# The same receipt CAS applies after a crash. An old unapplied intent for review
# A must not release a same-lease repair review B merely because task and lease
# still match. Recovery may abandon A, but every field of B's review authority
# must survive unchanged.
receipt_cas_recovery_patch="$receipt_cas_repo/.oms/landing-patches/land-stale-recovery.patch"
cp "$receipt_cas_patch_a" "$receipt_cas_recovery_patch"
python3 - "$receipt_cas_repo/.oms/landings.jsonl" \
  "$receipt_cas_recovery_patch" "$receipt_cas_repo" "$receipt_cas_lease" \
  "$receipt_cas_review_a_sha" "$TMP/plan-receipt-cas-review-a.json" <<'PY'
import hashlib, json, pathlib, subprocess, sys
path, patch, repo, lease, receipt_sha, review_path = sys.argv[1:]
review = json.load(open(review_path, encoding="utf-8"))
review["patch"] = patch
for name in ("state", "updated", "claim_expired", "claim_age_s"):
    review.pop(name, None)
done_receipt_sha = hashlib.sha256(json.dumps(
    review, sort_keys=True, separators=(",", ":"), ensure_ascii=False
).encode()).hexdigest()
row = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-stale-recovery",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
    "base_sha": subprocess.check_output(
        ["git", "-C", repo, "rev-parse", "HEAD"], text=True
    ).strip(),
    "task": "t1",
    "lease": lease,
    "plan_receipt_sha": receipt_sha,
    "plan_done_receipt_sha": done_receipt_sha,
}
with open(path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(row) + "\n")
PY
"$LAND" --repo "$receipt_cas_repo" --recover \
  >"$TMP/plan-receipt-cas-recover.out" 2>&1 ||
  fail "stale unapplied receipt recovery did not converge"
"$PLAN" --repo "$receipt_cas_repo" show --id t1 > "$TMP/plan-receipt-cas-recovered-task.json"
python3 - "$TMP/plan-receipt-cas-recovered-task.json" "$receipt_cas_patch_b" \
  "$receipt_cas_artifact_b" "$receipt_cas_verify" "$receipt_cas_lease" <<'PY' ||
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "review", row
assert row["patch"] == sys.argv[2], row
assert row["artifact"] == sys.argv[3], row
assert row["verify"] == sys.argv[4], row
assert row["lease_id"] == sys.argv[5] == row["review_lease_id"], row
assert row["repair_count"] == 1, row
PY
  fail "stale recovery released or rewrote the superseding review receipt"
python3 - "$receipt_cas_repo/.oms/landings.jsonl" <<'PY' ||
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
events = [row["event"] for row in rows if row.get("landing_id") == "land-stale-recovery"]
assert events == ["intent", "abandoned"], events
PY
  fail "stale recovery did not close only the old unapplied intent"

# Superseding review B may itself have crossed its exact review -> landing CAS
# before recovery sees stale intent A. The differing frozen review receipt, not
# the shared lease or state name, proves that B owns `landing`; preserve it just
# like a newer review.
receipt_cas_landing_patch="$receipt_cas_repo/.oms/landing-patches/land-stale-newer-landing.patch"
cp "$receipt_cas_patch_a" "$receipt_cas_landing_patch"
python3 - "$receipt_cas_repo/.oms/landings.jsonl" \
  "$receipt_cas_landing_patch" "$receipt_cas_repo" "$receipt_cas_lease" \
  "$receipt_cas_review_a_sha" "$TMP/plan-receipt-cas-review-a.json" <<'PY'
import hashlib, json, pathlib, subprocess, sys
path, patch, repo, lease, receipt_sha, review_path = sys.argv[1:]
review = json.load(open(review_path, encoding="utf-8"))
review["patch"] = patch
for name in ("state", "updated", "claim_expired", "claim_age_s"):
    review.pop(name, None)
done_receipt_sha = hashlib.sha256(json.dumps(
    review, sort_keys=True, separators=(",", ":"), ensure_ascii=False
).encode()).hexdigest()
row = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-stale-newer-landing",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
    "base_sha": subprocess.check_output(
        ["git", "-C", repo, "rev-parse", "HEAD"], text=True
    ).strip(),
    "task": "t1",
    "lease": lease,
    "plan_receipt_sha": receipt_sha,
    "plan_done_receipt_sha": done_receipt_sha,
}
with open(path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(row) + "\n")
PY
receipt_cas_patch_b_sha="$(
  . "$ROOT/scripts/lib/oms-common.sh"
  oms_sha256_file "$receipt_cas_patch_b"
)"
"$PLAN" --repo "$receipt_cas_repo" land --id t1 \
  --lease-id "$receipt_cas_lease" \
  --expected-review-patch "$receipt_cas_patch_b" \
  --expected-review-patch-sha256 "$receipt_cas_patch_b_sha" \
  --expected-review-verify "$receipt_cas_verify" \
  --expected-review-executor-id "" \
  --expected-review-executor-soul-sha256 "" \
  --expected-review-lease-id "$receipt_cas_lease" >/dev/null
"$LAND" --repo "$receipt_cas_repo" --recover \
  >"$TMP/plan-receipt-cas-newer-landing.out" 2>&1 ||
  fail "stale recovery did not preserve a superseding landing receipt"
"$PLAN" --repo "$receipt_cas_repo" show --id t1 \
  > "$TMP/plan-receipt-cas-newer-landing-task.json"
python3 - "$TMP/plan-receipt-cas-newer-landing-task.json" "$receipt_cas_patch_b" \
  "$receipt_cas_artifact_b" "$receipt_cas_verify" "$receipt_cas_lease" <<'PY' ||
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "landing", row
assert row["patch"] == sys.argv[2], row
assert row["artifact"] == sys.argv[3], row
assert row["verify"] == sys.argv[4], row
assert row["lease_id"] == sys.argv[5] == row["review_lease_id"], row
assert row["repair_count"] == 1, row
PY
  fail "stale recovery released or rewrote the superseding landing receipt"
python3 - "$receipt_cas_repo/.oms/landings.jsonl" <<'PY' ||
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
events = [row["event"] for row in rows if row.get("landing_id") == "land-stale-newer-landing"]
assert events == ["intent", "abandoned"], events
PY
  fail "stale recovery did not close only the old intent beside newer landing"

# A superseding landing can already have applied B before its own journal is
# recovered. If stale intent A produces the same result bytes, reverse-apply is
# not enough to identify the owner: recovery must compare the exact landing
# receipt before calling finish, preserve B, and close only A as superseded.
receipt_cas_stale_landing_patch="$receipt_cas_repo/.oms/landing-patches/land-stale-same-bytes-landing.patch"
receipt_cas_stale_landing_review_patch="$TMP/plan-receipt-cas-stale-same-bytes-landing-review.patch"
receipt_cas_stale_landing_artifact="$TMP/plan-receipt-cas-stale-same-bytes-landing.md"
cp "$receipt_cas_patch_b" "$receipt_cas_stale_landing_patch"
cp "$receipt_cas_patch_b" "$receipt_cas_stale_landing_review_patch"
printf 'older same-byte review A\n' > "$receipt_cas_stale_landing_artifact"
python3 - "$receipt_cas_repo/.oms/landings.jsonl" \
  "$receipt_cas_stale_landing_patch" "$receipt_cas_stale_landing_review_patch" \
  "$receipt_cas_stale_landing_artifact" "$receipt_cas_repo" \
  "$receipt_cas_lease" "$TMP/plan-receipt-cas-newer-landing-task.json" <<'PY'
import hashlib, json, pathlib, subprocess, sys

path, patch, review_patch, artifact, repo, lease, review_path = sys.argv[1:]
review = json.load(open(review_path, encoding="utf-8"))
review["patch"] = review_patch
review["artifact"] = artifact
review["repair_count"] = 0
review["repair_artifact"] = ""

def receipt(row):
    row = dict(row)
    for name in ("state", "updated", "claim_expired", "claim_age_s"):
        row.pop(name, None)
    raw = json.dumps(row, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(raw.encode()).hexdigest()

done = dict(review)
done["patch"] = patch
row = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-stale-same-bytes-landing",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
    "base_sha": subprocess.check_output(
        ["git", "-C", repo, "rev-parse", "HEAD"], text=True
    ).strip(),
    "task": "t1",
    "lease": lease,
    "plan_receipt_sha": receipt(review),
    "plan_done_receipt_sha": receipt(done),
    "approval": "",
    "approval_version": "",
}
with open(path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(row) + "\n")
PY
receipt_cas_stale_landing_receipt_sha="$(python3 - \
  "$receipt_cas_repo/.oms/landings.jsonl" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
row = next(row for row in rows
           if row.get("landing_id") == "land-stale-same-bytes-landing"
           and row.get("event") == "intent")
print(row["plan_receipt_sha"])
PY
)"
if "$PLAN" --repo "$receipt_cas_repo" finish --id t1 \
  --lease-id "$receipt_cas_lease" --patch "$receipt_cas_stale_landing_patch" \
  --expected-landing-receipt-sha256 "$receipt_cas_stale_landing_receipt_sha" \
  >/dev/null 2>&1; then
  fail "stale exact landing receipt finished a superseding same-lease review"
fi
"$PLAN" --repo "$receipt_cas_repo" show --id t1 \
  > "$TMP/plan-receipt-cas-stale-finish-rejected.json"
python3 - "$TMP/plan-receipt-cas-newer-landing-task.json" \
  "$TMP/plan-receipt-cas-stale-finish-rejected.json" <<'PY' ||
import json, sys
before = json.load(open(sys.argv[1], encoding="utf-8"))
after = json.load(open(sys.argv[2], encoding="utf-8"))
assert after == before, (before, after)
PY
  fail "atomic landing receipt rejection changed the superseding task"
git -C "$receipt_cas_repo" apply --binary "$receipt_cas_patch_b"
"$LAND" --repo "$receipt_cas_repo" --recover \
  >"$TMP/plan-receipt-cas-superseded-landing.out" 2>&1 ||
  fail "same-byte superseding landing receipt did not converge"
"$PLAN" --repo "$receipt_cas_repo" show --id t1 \
  > "$TMP/plan-receipt-cas-superseded-landing-task.json"
python3 - "$TMP/plan-receipt-cas-newer-landing-task.json" \
  "$TMP/plan-receipt-cas-superseded-landing-task.json" \
  "$receipt_cas_repo/.oms/landings.jsonl" \
  "$receipt_cas_repo/.oms/artifacts/index.jsonl" \
  "$receipt_cas_stale_landing_patch" "$receipt_cas_repo" <<'PY' ||
import hashlib, json, pathlib, sys

before_path, after_path, landings_path, index_path, stale_patch, repo = sys.argv[1:]
before = json.load(open(before_path, encoding="utf-8"))
after = json.load(open(after_path, encoding="utf-8"))
assert after == before, (before, after)
rows = [json.loads(line) for line in open(landings_path, encoding="utf-8") if line.strip()]
stale = [row for row in rows if row.get("landing_id") == "land-stale-same-bytes-landing"]
assert [row.get("event") for row in stale] == ["intent", "abandoned"], stale
assert stale[-1].get("reason") == "superseded-landing", stale[-1]
fields = (
    "landing_id", "patch", "patch_sha", "base_sha", "task", "lease",
    "plan_receipt_sha", "plan_done_receipt_sha", "approval", "approval_version",
)
canonical = {"schema": 1}
canonical.update({name: stale[0].get(name, "") for name in fields})
digest = hashlib.sha256(json.dumps(
    canonical, ensure_ascii=False, sort_keys=True, separators=(",", ":")
).encode()).hexdigest()
assert stale[-1].get("receipt_sha") == digest, stale[-1]
index = pathlib.Path(index_path)
if index.exists():
    lineage = [json.loads(line) for line in index.read_text(encoding="utf-8").splitlines()
               if line.strip()]
    assert not any(row.get("kind") == "patch-land" and row.get("patch") ==
                   str(pathlib.Path(stale_patch).relative_to(pathlib.Path(repo)))
                   for row in lineage), lineage
PY
  fail "stale same-byte intent rewrote B or minted A lineage/terminal authority"
receipt_cas_landing_lines="$(wc -l < "$receipt_cas_repo/.oms/landings.jsonl" | tr -d ' ')"
"$LAND" --repo "$receipt_cas_repo" --recover \
  >"$TMP/plan-receipt-cas-superseded-landing-skip.out" 2>&1 ||
  fail "canonical superseded landing terminal did not remain closed"
[ "$(wc -l < "$receipt_cas_repo/.oms/landings.jsonl" | tr -d ' ')" = \
  "$receipt_cas_landing_lines" ] ||
  fail "historical superseded landing appended another terminal row"
git -C "$receipt_cas_repo" restore file.txt

# Once review B finishes under the same lease, its different exact done receipt
# is the durable winner. An older A intent with the same effective patch bytes
# must not borrow B's generic `done` state to mint A lineage/complete; and the
# already-canonical abandoned A intents above must remain historically closed.
git -C "$receipt_cas_repo" apply --binary "$receipt_cas_patch_b"
receipt_cas_landing_b_sha="$(python3 - \
  "$TMP/plan-receipt-cas-newer-landing-task.json" <<'PY'
import hashlib, json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
for name in ("state", "updated", "claim_expired", "claim_age_s"):
    row.pop(name, None)
raw = json.dumps(row, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
print(hashlib.sha256(raw.encode()).hexdigest())
PY
)"
"$PLAN" --repo "$receipt_cas_repo" finish --id t1 \
  --lease-id "$receipt_cas_lease" --patch "$receipt_cas_patch_b" \
  --expected-landing-receipt-sha256 "$receipt_cas_landing_b_sha" >/dev/null
"$PLAN" --repo "$receipt_cas_repo" show --id t1 \
  > "$TMP/plan-receipt-cas-done-b.json"
receipt_cas_stale_done_patch="$receipt_cas_repo/.oms/landing-patches/land-stale-same-bytes-done.patch"
cp "$receipt_cas_patch_b" "$receipt_cas_stale_done_patch"
python3 - "$receipt_cas_repo/.oms/landings.jsonl" \
  "$receipt_cas_stale_done_patch" "$receipt_cas_repo" "$receipt_cas_lease" \
  "$TMP/plan-receipt-cas-newer-landing-task.json" <<'PY'
import hashlib, json, pathlib, subprocess, sys

path, patch, repo, lease, review_path = sys.argv[1:]
review = json.load(open(review_path, encoding="utf-8"))

def receipt(row):
    row = dict(row)
    for name in ("state", "updated", "claim_expired", "claim_age_s"):
        row.pop(name, None)
    raw = json.dumps(row, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(raw.encode()).hexdigest()

expected_done = dict(review)
expected_done["patch"] = patch
row = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-stale-same-bytes-done",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
    "base_sha": subprocess.check_output(
        ["git", "-C", repo, "rev-parse", "HEAD"], text=True
    ).strip(),
    "task": "t1",
    "lease": lease,
    "plan_receipt_sha": receipt(review),
    "plan_done_receipt_sha": receipt(expected_done),
    "approval": "",
    "approval_version": "",
}
with open(path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(row) + "\n")
PY
"$LAND" --repo "$receipt_cas_repo" --recover \
  >"$TMP/plan-receipt-cas-superseded-done.out" 2>&1 ||
  fail "superseding done receipt did not close stale same-lease intents"
"$PLAN" --repo "$receipt_cas_repo" show --id t1 \
  > "$TMP/plan-receipt-cas-done-b-after.json"
python3 - "$TMP/plan-receipt-cas-done-b.json" \
  "$TMP/plan-receipt-cas-done-b-after.json" \
  "$receipt_cas_repo/.oms/landings.jsonl" \
  "$receipt_cas_repo/.oms/artifacts/index.jsonl" \
  "$receipt_cas_stale_done_patch" "$receipt_cas_repo" <<'PY' ||
import hashlib, json, pathlib, sys

before_path, after_path, landings_path, index_path, stale_patch, repo = sys.argv[1:]
before = json.load(open(before_path, encoding="utf-8"))
after = json.load(open(after_path, encoding="utf-8"))
assert after == before, (before, after)
rows = [json.loads(line) for line in open(landings_path, encoding="utf-8") if line.strip()]
stale = [row for row in rows if row.get("landing_id") == "land-stale-same-bytes-done"]
assert [row.get("event") for row in stale] == ["intent", "abandoned"], stale
assert stale[-1].get("reason") == "superseded-done", stale[-1]
fields = (
    "landing_id", "patch", "patch_sha", "base_sha", "task", "lease",
    "plan_receipt_sha", "plan_done_receipt_sha", "approval", "approval_version",
)
canonical = {"schema": 1}
canonical.update({name: stale[0].get(name, "") for name in fields})
digest = hashlib.sha256(json.dumps(
    canonical, ensure_ascii=False, sort_keys=True, separators=(",", ":")
).encode()).hexdigest()
assert stale[-1].get("receipt_sha") == digest, stale[-1]
index = pathlib.Path(index_path)
if index.exists():
    lineage = [json.loads(line) for line in index.read_text(encoding="utf-8").splitlines()
               if line.strip()]
    assert not any(row.get("kind") == "patch-land" and row.get("patch") ==
                   str(pathlib.Path(stale_patch).relative_to(pathlib.Path(repo)))
                   for row in lineage), lineage
PY
  fail "superseding done recovery changed B or forged A lineage/terminal authority"

# Exact abandoned terminals plus the durable superseding plan outcome remain
# closed after later commits make their old patches ambiguous.
receipt_cas_landing_lines="$(wc -l < "$receipt_cas_repo/.oms/landings.jsonl" | tr -d ' ')"
printf 'later overlapping commit\n' > "$receipt_cas_repo/file.txt"
git -C "$receipt_cas_repo" add file.txt
git -C "$receipt_cas_repo" commit -qm 'overlap abandoned patches'
"$LAND" --repo "$receipt_cas_repo" --recover \
  >"$TMP/plan-receipt-cas-abandoned-skip.out" 2>&1 ||
  fail "later overlapping commit reopened an exact abandoned landing"
[ "$(wc -l < "$receipt_cas_repo/.oms/landings.jsonl" | tr -d ' ')" = "$receipt_cas_landing_lines" ] ||
  fail "historical abandoned recovery appended another terminal row"

# Admission may pass while the reviewed task's lease/state changes. The
# durable intent must exist before the final review -> landing fence, so a
# crash or failed fence never leaves an unexplained task stuck in `landing`.
fence_repo="$TMP/plan-fence-repo"
mkdir -p "$fence_repo"
git -C "$fence_repo" init -q
git -C "$fence_repo" config user.email test@example.com
git -C "$fence_repo" config user.name test
printf 'base\n' > "$fence_repo/file.txt"
git -C "$fence_repo" add file.txt
git -C "$fence_repo" commit -qm base
fence_patch="$TMP/plan-fence.patch"
fence_artifact="$TMP/plan-fence.md"
printf 'changed\n' > "$fence_repo/file.txt"
git -C "$fence_repo" diff --binary > "$fence_patch"
git -C "$fence_repo" restore file.txt
printf 'reviewed\n' > "$fence_artifact"
fence_verify="'$ROOT/scripts/agent-plan.sh' --repo '$fence_repo' release --id t1"
"$ROOT/scripts/agent-plan.sh" --repo "$fence_repo" init --goal test >/dev/null
"$ROOT/scripts/agent-plan.sh" --repo "$fence_repo" add --id t1 --title test \
  --verify "$fence_verify" >/dev/null
"$ROOT/scripts/agent-plan.sh" --repo "$fence_repo" claim --id t1 --provider codex >/dev/null
"$ROOT/scripts/agent-plan.sh" --repo "$fence_repo" review --id t1 \
  --artifact "$fence_artifact" --patch "$fence_patch" >/dev/null
set +e
"$LAND" --repo "$fence_repo" --plan-task t1 --verify "$fence_verify" \
  >"$TMP/plan-fence.out" 2>&1
fence_rc=$?
set -e
[ "$fence_rc" -ne 0 ] || fail "a concurrently released review task landed"
grep -Fxq base "$fence_repo/file.txt" || fail "failed plan fence still applied the patch"
python3 - "$fence_repo/.oms/landings.jsonl" <<'PY' ||
import json, sys
events = [json.loads(line)["event"] for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
assert events == ["intent", "abandoned"], events
PY
  fail "plan fence failure was not bracketed by a durable intent and terminal receipt"

# Freeze plan lineage with the reviewed task before admission. A verifier can
# replace only the top-level lineage while leaving the same task/lease receipt
# in place; landing is then still mechanically valid, but every durable row
# from this operation must retain the old lineage and stay inert for the new
# plan's same textual task id.
lineage_swap_repo="$TMP/patch-land-lineage-swap-repo"
lineage_swap_patch="$TMP/patch-land-lineage-swap.patch"
lineage_swap_artifact="$TMP/patch-land-lineage-swap.md"
mkdir -p "$lineage_swap_repo"
git -C "$lineage_swap_repo" init -q
git -C "$lineage_swap_repo" config user.email test@example.com
git -C "$lineage_swap_repo" config user.name test
printf 'base\n' > "$lineage_swap_repo/file.txt"
git -C "$lineage_swap_repo" add file.txt
git -C "$lineage_swap_repo" commit -qm base
printf 'lineage landed\n' > "$lineage_swap_repo/file.txt"
git -C "$lineage_swap_repo" diff --binary > "$lineage_swap_patch"
git -C "$lineage_swap_repo" restore file.txt
printf 'reviewed\n' > "$lineage_swap_artifact"
"$PLAN" --repo "$lineage_swap_repo" init --goal lineage >/dev/null
lineage_old_plan_id="$(python3 - "$lineage_swap_repo/.oms/plan/tasks.json" <<'PY' | tr -d '\r'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["plan_id"])
PY
)"
lineage_new_plan_id="plan_dddddddddddddddddddddddddddddddd"
lineage_swap_verify="OMS_SWAP_PLAN_FILE='$lineage_swap_repo/.oms/plan/tasks.json' OMS_SWAP_PLAN_ID='$lineage_new_plan_id' python3 -c 'import json,os,pathlib; p=pathlib.Path(os.environ[\"OMS_SWAP_PLAN_FILE\"]); d=json.loads(p.read_text(encoding=\"utf-8\")); d[\"plan_id\"]=os.environ[\"OMS_SWAP_PLAN_ID\"]; p.write_text(json.dumps(d,ensure_ascii=False,indent=2),encoding=\"utf-8\")'"
"$PLAN" --repo "$lineage_swap_repo" add --id same-task --title lineage \
  --allowed file.txt --verify "$lineage_swap_verify" >/dev/null
"$PLAN" --repo "$lineage_swap_repo" claim --id same-task --provider codex >/dev/null
"$PLAN" --repo "$lineage_swap_repo" review --id same-task \
  --artifact "$lineage_swap_artifact" --patch "$lineage_swap_patch" >/dev/null
"$LAND" --repo "$lineage_swap_repo" --plan-task same-task \
  --verify "$lineage_swap_verify" >/dev/null ||
  fail "same-task landing failed after a top-level plan lineage replacement"
python3 - "$lineage_swap_repo/.oms/plan/tasks.json" \
  "$lineage_swap_repo/.oms/landings.jsonl" \
  "$lineage_swap_repo/.oms/artifacts/index.jsonl" \
  "$lineage_old_plan_id" "$lineage_new_plan_id" <<'PY' ||
import json, sys
plan = json.load(open(sys.argv[1], encoding="utf-8"))
landings = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
artifacts = [json.loads(line) for line in open(sys.argv[3], encoding="utf-8") if line.strip()]
assert plan["plan_id"] == sys.argv[5], plan
assert [row["event"] for row in landings] == ["intent", "complete"], landings
assert all(row.get("plan_id") == sys.argv[4] for row in landings), landings
row = next(row for row in reversed(artifacts)
           if row.get("kind") == "patch-land" and row.get("task_id") == "same-task")
assert row.get("plan_id") == sys.argv[4], row
assert row["plan_id"] != plan["plan_id"], (row, plan)
PY
  fail "patch landing borrowed a replacement plan lineage"
"$ROOT/scripts/runtime.sh" --repo "$lineage_swap_repo" envelope show \
  > "$TMP/patch-land-lineage-swap-envelope.json"
python3 - "$TMP/patch-land-lineage-swap-envelope.json" <<'PY' ||
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
statuses = {item["id"]: item["status"] for item in row["criteria"]}
assert statuses["plan-task-same-task"] == "missing", statuses
PY
  fail "old-lineage landing evidence leaked into the replacement plan"
# Recreate the crash window after apply/plan completion but before lineage and
# the terminal journal row. Recovery must source the old id from the intent,
# never from the replacement plan that is current when --recover runs.
python3 - "$lineage_swap_repo/.oms/landings.jsonl" \
  "$lineage_swap_repo/.oms/artifacts/index.jsonl" <<'PY'
import json, pathlib, sys
landings, artifacts = map(pathlib.Path, sys.argv[1:])
landing_rows = [json.loads(line) for line in landings.read_text(encoding="utf-8").splitlines()
                if line.strip()]
artifact_rows = [json.loads(line) for line in artifacts.read_text(encoding="utf-8").splitlines()
                 if line.strip()]
landings.write_text(json.dumps(landing_rows[0]) + "\n", encoding="utf-8")
artifacts.write_text("".join(json.dumps(row) + "\n" for row in artifact_rows
                             if row.get("kind") != "patch-land"), encoding="utf-8")
PY
"$LAND" --repo "$lineage_swap_repo" --recover >/dev/null ||
  fail "recovery could not replay frozen landing lineage"
python3 - "$lineage_swap_repo/.oms/landings.jsonl" \
  "$lineage_swap_repo/.oms/artifacts/index.jsonl" "$lineage_old_plan_id" <<'PY' ||
import json, sys
landings = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
artifacts = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
assert [row["event"] for row in landings] == ["intent", "complete"], landings
assert all(row.get("plan_id") == sys.argv[3] for row in landings), landings
row = next(row for row in artifacts if row.get("kind") == "patch-land")
assert row.get("plan_id") == sys.argv[3], row
PY
  fail "recovery borrowed the current plan lineage instead of its intent"

# A plan applied from a reviewed proposal carries a plan-level project_contract,
# which `agent-plan show` mixes into the read view landing hashes, while finish
# hashes the stored task. Two projections of one receipt is a deadlock, not a
# check: the patch lands and then cannot be finished, the task is stuck in
# `landing`, and `--recover` repeats the same rejection forever. Every
# spec-bound (autopilot) landing goes through here, so this drives the real
# binaries end to end rather than asserting a digest by hand.
contract_repo="$TMP/contract-bound-landing-repo"
contract_patch="$TMP/contract-bound-landing.patch"
contract_artifact="$TMP/contract-bound-landing.md"
mkdir -p "$contract_repo"
git -C "$contract_repo" init -q
git -C "$contract_repo" config user.email test@example.com
git -C "$contract_repo" config user.name test
printf 'base\n' > "$contract_repo/file.txt"
cat > "$contract_repo/PROJECT.md" <<'EOF'
# Contract-bound landing fixture

- State: confirmed
EOF
git -C "$contract_repo" add file.txt PROJECT.md
git -C "$contract_repo" commit -qm base
printf 'contract reviewed\n' > "$contract_repo/file.txt"
git -C "$contract_repo" diff --binary > "$contract_patch"
git -C "$contract_repo" restore file.txt
printf 'reviewed\n' > "$contract_artifact"

"$PLAN" --repo "$contract_repo" init --goal test >/dev/null
"$PLAN" --repo "$contract_repo" add --id t1 --title contract-bound \
  --allowed file.txt --verify true >/dev/null
"$PLAN" --repo "$contract_repo" add --id t2 --title contract-drift \
  --allowed file.txt --verify true >/dev/null
# The contract is written the way apply-proposal writes it; this suite fences
# the landing receipt, not the proposal path that mints the contract.
python3 - "$contract_repo/.oms/plan/tasks.json" "$contract_repo/PROJECT.md" <<'PY'
import hashlib, json, pathlib, sys
path = pathlib.Path(sys.argv[1])
plan = json.loads(path.read_text(encoding="utf-8"))
plan["project_contract"] = {
    "schema": 1,
    "spec_sha256": hashlib.sha256(pathlib.Path(sys.argv[2]).read_bytes()).hexdigest(),
    "allowed_envelope": ["file.txt"],
    "acceptance_files": [],
    "acceptance_manifest": [],
}
path.write_text(json.dumps(plan, ensure_ascii=False, indent=2), encoding="utf-8")
PY
"$PLAN" --repo "$contract_repo" show --id t1 |
  python3 -c 'import json,sys; assert isinstance(json.load(sys.stdin).get("project_contract"), dict)' ||
  fail "the contract fixture is not visible in the task read view"

"$PLAN" --repo "$contract_repo" claim --id t1 --provider codex >/dev/null
"$PLAN" --repo "$contract_repo" review --id t1 \
  --artifact "$contract_artifact" --patch "$contract_patch" >/dev/null
"$LAND" --repo "$contract_repo" --plan-task t1 --verify true >/dev/null ||
  fail "a contract-bound plan review did not land"
grep -Fxq 'contract reviewed' "$contract_repo/file.txt" ||
  fail "contract-bound landing applied the wrong patch"
"$PLAN" --repo "$contract_repo" show --id t1 |
  python3 -c 'import json,sys;d=json.load(sys.stdin);assert d["state"]=="done",d' ||
  fail "a contract-bound landing applied its patch but could not finish the task"
python3 - "$contract_repo/.oms/landings.jsonl" <<'PY' ||
import json, sys
events = [json.loads(line)["event"] for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
assert events == ["intent", "complete"], events
PY
  fail "contract-bound landing did not close its durable receipt"
"$LAND" --repo "$contract_repo" --recover > "$TMP/contract-recover.out" 2>&1 ||
  fail "recovery over a settled contract-bound landing failed"
grep -Fq "0 recovered, 0 abandoned, 0 need manual recovery" "$TMP/contract-recover.out" ||
  fail "a settled contract-bound landing still looked unrecovered: $(cat "$TMP/contract-recover.out")"

# One projection must not mean a weaker fence: a landing task whose stored
# record moved after its receipt was taken still cannot be finished.
contract_drift_lease="$("$PLAN" --repo "$contract_repo" claim --id t2 \
  --provider codex >/dev/null; "$PLAN" --repo "$contract_repo" show --id t2 |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])' | tr -d '\r')"
"$PLAN" --repo "$contract_repo" review --id t2 \
  --lease-id "$contract_drift_lease" --artifact "$contract_artifact" \
  --patch "$contract_patch" >/dev/null
"$PLAN" --repo "$contract_repo" land --id t2 --lease-id "$contract_drift_lease" \
  --expected-review-patch "$contract_patch" \
  --expected-review-patch-sha256 "$(python3 -c 'import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$contract_patch")" \
  --expected-review-verify true \
  --expected-review-lease-id "$contract_drift_lease" \
  --expected-review-executor-id "" \
  --expected-review-executor-soul-sha256 "" >/dev/null ||
  fail "a contract-bound review could not be fenced into landing"
if "$PLAN" --repo "$contract_repo" finish --id t2 \
  --lease-id "$contract_drift_lease" --patch "$contract_patch" \
  --expected-landing-receipt-sha256 "$(printf '0%.0s' $(seq 64))" \
  >/dev/null 2>&1; then
  fail "a stale landing receipt finished a contract-bound task"
fi
"$PLAN" --repo "$contract_repo" show --id t2 |
  python3 -c 'import json,sys;d=json.load(sys.stdin);assert d["state"]=="landing",d' ||
  fail "the rejected stale finish still moved the contract-bound task"

echo "patch-land-approval: ok"
