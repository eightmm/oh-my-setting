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

# Executor identity is part of admission scope. An approval requested under a
# frozen soul cannot be consumed after omitting that executor, even when patch,
# base, verifier, and all allow flags are unchanged.
executor_repo="$TMP/executor-repo"
mkdir -p "$executor_repo"
git -C "$executor_repo" init -q
git -C "$executor_repo" config user.email test@example.com
git -C "$executor_repo" config user.name test
printf 'base\n' > "$executor_repo/file.txt"
git -C "$executor_repo" add file.txt
git -C "$executor_repo" commit -qm base
executor_patch="$TMP/executor.patch"
printf 'executor approved\n' > "$executor_repo/file.txt"
git -C "$executor_repo" diff --binary > "$executor_patch"
git -C "$executor_repo" restore file.txt
printf '# Specialization\n\nLand only the reviewed file.\n' > "$TMP/land-executor-soul.md"
"$ROOT/scripts/agent-executor.sh" create --repo "$executor_repo" \
  --id land-executor --provider codex --strategy implementation-worker \
  --allowed file.txt --verify true --soul-file "$TMP/land-executor-soul.md" >/dev/null
"$ROOT/scripts/agent-executor.sh" freeze --repo "$executor_repo" \
  --id land-executor >/dev/null
executor_approval="$(OMS_REQUIRE_LANDING_APPROVAL=1 "$LAND" --repo "$executor_repo" \
  --patch "$executor_patch" --verify true --executor land-executor \
  --request-approval)" || fail "could not request executor-bound approval"
"$APPROVAL" --repo "$executor_repo" show --approval "$executor_approval" --json \
  > "$TMP/executor-request.json"
python3 - "$TMP/executor-request.json" \
  "$executor_repo/.oms/executors/land-executor/meta.json" <<'PY' || fail "approval omitted executor identity"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
meta = json.load(open(sys.argv[2], encoding="utf-8"))
params = row["parameters"]
assert params["executor_id"] == "land-executor", params
assert params["executor_soul_sha256"] == meta["soul_sha256"], (params, meta)
PY
executor_grant="$($APPROVAL --repo "$executor_repo" decide \
  --approval "$executor_approval" --decision approve --expected-version 1 \
  --actor operator)" || fail "could not approve executor-bound landing"
if OMS_REQUIRE_LANDING_APPROVAL=1 "$LAND" --repo "$executor_repo" \
  --patch "$executor_patch" --verify true --approval "$executor_approval" \
  --approval-version 2 --approval-token "$executor_grant" >/dev/null 2>&1; then
  fail "executor-bound approval was accepted after omitting the executor"
fi
OMS_REQUIRE_LANDING_APPROVAL=1 "$LAND" --repo "$executor_repo" \
  --patch "$executor_patch" --verify true --executor land-executor \
  --approval "$executor_approval" --approval-version 2 \
  --approval-token "$executor_grant" >/dev/null ||
  fail "matching executor-bound approval did not land"
grep -Fxq 'executor approved' "$executor_repo/file.txt" ||
  fail "executor-bound approved patch was not applied"

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
python3 - "$receipt_repo/.oms/landings.jsonl" "$receipt_patch" <<'PY'
import hashlib, json, pathlib, sys
path, patch = sys.argv[1:]
row = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-receipt-failure",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
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
"$ROOT/scripts/oms-run.sh" validate --dir "$receipt_repo/.oms" >/dev/null ||
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
python3 - "$dedupe_repo/.oms/landings.jsonl" "$dedupe_patch" <<'PY'
import hashlib, json, pathlib, sys
path, patch = sys.argv[1:]
row = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-lineage-dedupe",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
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
python3 - "$corrupt_index_repo/.oms/landings.jsonl" "$corrupt_index_patch" <<'PY'
import hashlib, json, pathlib, sys
path, patch = sys.argv[1:]
row = {
    "schema": 1,
    "ts": "2026-08-09T00:00:00Z",
    "landing_id": "land-corrupt-index",
    "event": "intent",
    "patch": patch,
    "patch_sha": hashlib.sha256(pathlib.Path(patch).read_bytes()).hexdigest(),
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
"$ROOT/scripts/agent-plan.sh" --repo "$fence_repo" init --goal test >/dev/null
"$ROOT/scripts/agent-plan.sh" --repo "$fence_repo" add --id t1 --title test >/dev/null
"$ROOT/scripts/agent-plan.sh" --repo "$fence_repo" claim --id t1 --provider codex >/dev/null
"$ROOT/scripts/agent-plan.sh" --repo "$fence_repo" review --id t1 \
  --artifact "$fence_artifact" --patch "$fence_patch" >/dev/null
set +e
"$LAND" --repo "$fence_repo" --plan-task t1 --verify \
  "'$ROOT/scripts/agent-plan.sh' --repo '$fence_repo' release --id t1" \
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

echo "patch-land-approval: ok"
