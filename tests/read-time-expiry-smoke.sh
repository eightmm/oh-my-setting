#!/usr/bin/env bash
set -euo pipefail

# Read-time expiry as the primary retirement mechanism for two state families
# that nothing ever retires by hand: automatic kind=hook fail rows, and plan
# claims held by workers that stopped heartbeating. Both must (a) stop counting
# as open the moment their TTL passes, (b) still be PRINTED, tagged EXPIRED,
# (c) leave the stored rows untouched at read time, and (d) hand gc the same
# predicate so the files eventually shrink. Rows are written through the tools;
# only the timestamps are hand-aged afterwards.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-read-time-expiry.XXXXXX")"
trap '[ "${KEEP_TMP:-0}" = 1 ] || rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

LEDGER_SH="$ROOT/scripts/fail-ledger.sh"
GC_SH="$ROOT/scripts/gc.sh"
PLAN="$ROOT/scripts/agent-plan.sh"
RUN="$ROOT/scripts/plan-run.sh"

TWO_DAYS=172800
FORTY_DAYS=3456000

new_repo() {  # new_repo PATH
  mkdir -p "$1"
  git -C "$1" init -q
  git -C "$1" config user.email test@example.com
  git -C "$1" config user.name Test
  printf 'base\n' > "$1/README.md"
  git -C "$1" add README.md
  git -C "$1" commit -qm base
}

# ---------------------------------------------------------------------------
# PART 1: kind=hook fail rows retire on OMS_HOOK_FAIL_TTL at read time.
# ---------------------------------------------------------------------------
repo="$TMP/repo"
new_repo "$repo"
ledger="$repo/.oms/failures.jsonl"

record() {  # record CMD KIND
  "$LEDGER_SH" --repo "$repo" record --cmd "$1" --exit 1 --kind "$2" 2>&1
}

# Rewrite the ts of every row whose cmd matches, to N seconds ago. The ledger
# is append-only in normal use; this is the one thing a test cannot get by
# waiting, and it is the only field touched.
age_rows() {  # age_rows CMD_SUBSTRING SECONDS_AGO
  OMS_MATCH="$1" OMS_AGE="$2" python3 - "$ledger" <<'PY'
import datetime, json, os, sys
match = os.environ["OMS_MATCH"]
stamp = (datetime.datetime.now(datetime.timezone.utc)
         - datetime.timedelta(seconds=int(os.environ["OMS_AGE"]))
         ).strftime("%Y-%m-%dT%H:%M:%SZ")
out = []
with open(sys.argv[1], encoding="utf-8") as fh:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        row = json.loads(line)
        if match in (row.get("cmd") or ""):
            row["ts"] = stamp
        out.append(json.dumps(row, ensure_ascii=False))
with open(sys.argv[1], "w", encoding="utf-8") as fh:
    fh.write("\n".join(out) + "\n")
PY
}

CHECK_OUT=""
CHECK_RC=0
run_check() {  # run_check CMD [ENV_ASSIGNMENT...] -> sets CHECK_OUT, CHECK_RC
  local cmd="$1"; shift
  CHECK_RC=0
  CHECK_OUT="$(env "$@" "$LEDGER_SH" --repo "$repo" check --cmd "$cmd" 2>&1)" || CHECK_RC=$?
}

record 'oms-test-hook-old --flag' hook >/dev/null
record 'oms-test-hook-recent --flag' hook >/dev/null
record 'oms-test-hook-fresh --flag' hook >/dev/null
record 'oms-test-verify-old --flag' verify >/dev/null
age_rows 'oms-test-hook-old' "$FORTY_DAYS"
age_rows 'oms-test-hook-recent' "$TWO_DAYS"
age_rows 'oms-test-verify-old' "$FORTY_DAYS"

# A hook row past the TTL blocks nothing.
run_check 'oms-test-hook-old --flag'
[ "$CHECK_RC" = 0 ] || fail "expired hook row still blocks a retry (exit $CHECK_RC): $CHECK_OUT"
# ...and it must be the TTL that let it through, not the state-fingerprint escape.
case "$CHECK_OUT" in
  *"git state changed"*) fail "check passed through the git-state escape, not read-time expiry" ;;
esac

# A hook row inside the TTL, and a deliberate non-hook row of any age, still block.
run_check 'oms-test-hook-fresh --flag'
[ "$CHECK_RC" = 3 ] || fail "fresh hook row should still block (exit $CHECK_RC)"
run_check 'oms-test-verify-old --flag'
[ "$CHECK_RC" = 3 ] || fail "a deliberate kind=verify row must never expire (exit $CHECK_RC)"

# The env override moves the clock in both directions.
run_check 'oms-test-hook-fresh --flag' OMS_HOOK_FAIL_TTL=0
[ "$CHECK_RC" = 0 ] || fail "OMS_HOOK_FAIL_TTL=0 did not shorten the TTL (exit $CHECK_RC)"
run_check 'oms-test-hook-old --flag' OMS_HOOK_FAIL_TTL=4000000
[ "$CHECK_RC" = 3 ] || fail "a TTL longer than the row's age did not keep it open (exit $CHECK_RC)"

# list prints the retired row as EXPIRED instead of dropping it.
list_out="$("$LEDGER_SH" --repo "$repo" list)"
line="$(printf '%s\n' "$list_out" | grep -F 'oms-test-hook-old' || true)"
[ -n "$line" ] || fail "list dropped the expired hook row: $list_out"
case "$line" in *EXPIRED*) ;; *) fail "expired hook row is not tagged EXPIRED: $line" ;; esac
printf '%s\n' "$list_out" | grep -F 'oms-test-hook-fresh' | grep -Fq OPEN ||
  fail "fresh hook row lost its OPEN tag"
printf '%s\n' "$list_out" | grep -F 'oms-test-verify-old' | grep -Fq OPEN ||
  fail "aged non-hook row lost its OPEN tag"
printf '%s\n' "$list_out" | grep -Fq 'retired:' || fail "list did not explain the retirement"

# --unresolved answers "what is still failing", so a retired row is not there.
un_out="$("$LEDGER_SH" --repo "$repo" list --unresolved)"
if printf '%s\n' "$un_out" | grep -Fq 'oms-test-hook-old'; then
  fail "retired hook row still counted as unresolved"
fi
printf '%s\n' "$un_out" | grep -Fq 'oms-test-hook-fresh' || fail "--unresolved lost the live hook row"
printf '%s\n' "$un_out" | grep -Fq 'oms-test-verify-old' || fail "--unresolved lost the live verify row"

"$LEDGER_SH" --repo "$repo" list --json | python3 -c '
import json, sys
rows = {r["cmd"]: r for r in json.load(sys.stdin)["failures"]}
old = rows["oms-test-hook-old --flag"]
assert old["expired"] is True and old["count"] == 0, old
assert rows["oms-test-hook-fresh --flag"]["expired"] is False
assert rows["oms-test-verify-old --flag"]["expired"] is False
' || fail "list --json does not carry the retirement verdict"

# Reads never rewrite the ledger.
[ "$(wc -l < "$ledger" | tr -d ' ')" = 4 ] || fail "a read path rewrote the ledger"

# The repeat-failure advisor hint counts the same way: one live failure after
# an expired one is a first failure, not a pattern.
record 'oms-test-hint --flag' hook >/dev/null
age_rows 'oms-test-hint' "$FORTY_DAYS"
hint_out="$(record 'oms-test-hint --flag' hook)"
printf '%s\n' "$hint_out" | grep -Fq 'recorded' || fail "second hint row was not recorded"
if printf '%s\n' "$hint_out" | grep -Fq 'has failed'; then
  fail "the advise hint counted a retired hook row: $hint_out"
fi

# ---------------------------------------------------------------------------
# PART 2: gc reclaims exactly what reads retired, on its own retention floor.
# ---------------------------------------------------------------------------
gc_out="$("$GC_SH" --repo "$repo" --days 30 --apply)"
printf '%s\n' "$gc_out" | grep -Fq 'failures: compact' || fail "gc did not compact the ledger: $gc_out"
if grep -Fq 'oms-test-hook-old' "$ledger"; then
  fail "gc kept a hook row that is both expired and past the retention floor"
fi
grep -Fq 'oms-test-hook-recent' "$ledger" ||
  fail "gc dropped an expired hook row that is still inside the --days window"
grep -Fq 'oms-test-hook-fresh' "$ledger" || fail "gc dropped a live hook row"
grep -Fq 'oms-test-verify-old' "$ledger" ||
  fail "gc dropped an aged but unresolved non-hook row"

# ---------------------------------------------------------------------------
# PART 3: plan claims read as expired, and plan-run frees them pre-flight.
# ---------------------------------------------------------------------------
prepo="$TMP/plan-repo"
home="$TMP/home"
bin="$TMP/bin"
mkdir -p "$home" "$bin"
new_repo "$prepo"
mkdir -p "$prepo/scripts"
cat > "$prepo/scripts/check.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$prepo/scripts/check.sh"
git -C "$prepo" add scripts/check.sh
git -C "$prepo" commit -qm tooling

# Keep provider discovery hermetic even when the invoking shell exports a real
# NVM_DIR; peer-delegate intentionally loads that directory before execution.
export NVM_DIR="$home/.nvm"
cat > "$bin/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
echo worker-ok
EOF
chmod +x "$bin/codex"
cat > "$TMP/stub-delegate" <<'EOF'
#!/usr/bin/env bash
echo "stub-delegate: ok"
EOF
chmod +x "$TMP/stub-delegate"

plan_file="$prepo/.oms/plan/tasks.json"
"$PLAN" --repo "$prepo" init --goal 'read-time expiry' >/dev/null
"$PLAN" --repo "$prepo" add --id t1 --title dead-worker \
  --allowed delegated.txt --verify 'bash scripts/check.sh' >/dev/null
"$PLAN" --repo "$prepo" add --id t2 --title live-worker \
  --allowed delegated2.txt --verify 'bash scripts/check.sh' >/dev/null
"$PLAN" --repo "$prepo" claim --id t1 --provider codex >/dev/null
"$PLAN" --repo "$prepo" claim --id t2 --provider codex >/dev/null
# t2's worker is alive and has just heartbeat; t1's stopped two days ago.
"$PLAN" --repo "$prepo" touch --id t2 >/dev/null
OMS_AGE="$TWO_DAYS" python3 - "$plan_file" <<'PY'
import datetime, json, os, sys
stamp = (datetime.datetime.now(datetime.timezone.utc)
         - datetime.timedelta(seconds=int(os.environ["OMS_AGE"]))
         ).strftime("%Y-%m-%dT%H:%M:%SZ")
with open(sys.argv[1], encoding="utf-8") as fh:
    plan = json.load(fh)
plan["tasks"]["t1"]["claimed_at"] = stamp
with open(sys.argv[1], "w", encoding="utf-8") as fh:
    json.dump(plan, fh, ensure_ascii=False, indent=2)
PY

cp "$plan_file" "$TMP/tasks.before"
t1_lease="$("$PLAN" --repo "$prepo" show --id t1 |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["lease_id"])')"
[ -n "$t1_lease" ] || fail "claim did not issue a lease"

# list tags the dead claim and leaves the live one alone.
plan_list="$("$PLAN" --repo "$prepo" list)"
printf '%s\n' "$plan_list" | grep -E '^t1 ' | grep -Fq EXPIRED ||
  fail "expired claim is not tagged in list: $plan_list"
if printf '%s\n' "$plan_list" | grep -E '^t2 ' | grep -Fq EXPIRED; then
  fail "a claim with a fresh heartbeat was reported expired"
fi
printf '%s\n' "$plan_list" | grep -E '^t1 ' | grep -Fq claimed ||
  fail "list stopped reporting the stored state"

# show reports the verdict without inventing a state.
"$PLAN" --repo "$prepo" show --id t1 | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert d["state"] == "claimed", d["state"]
assert d["claim_expired"] is True, d
assert d["claim_age_s"] >= 3600, d
' || fail "show does not present the expired claim"
"$PLAN" --repo "$prepo" show --id t2 | python3 -c '
import json, sys
assert json.load(sys.stdin)["claim_expired"] is False
' || fail "show expired a live claim"

# ready / next / status offer the task again.
ready_out="$("$PLAN" --repo "$prepo" ready 2>/dev/null)"
[ "$ready_out" = t1 ] || fail "ready did not offer the expired claim (got: $ready_out)"
next_out="$("$PLAN" --repo "$prepo" next 2>&1)"
printf '%s\n' "$next_out" | grep -Fq 'Task t1' || fail "next did not select the expired claim"
printf '%s\n' "$next_out" | grep -Fq EXPIRED || fail "next brief hid the expiry"
"$PLAN" --repo "$prepo" status | grep -Fq 'expired claim t1' ||
  fail "status did not report the expired claim"
"$PLAN" --repo "$prepo" status | grep -Fq 'ready now: t1' ||
  fail "status did not count the expired claim as actionable"

# None of those reads may touch the stored plan.
cmp -s "$plan_file" "$TMP/tasks.before" || fail "a read path rewrote tasks.json"

# plan-run's pre-flight frees the dead claim so --id works without a separate
# reclaim call; without it plan-run dies with "task t1 is claimed, not ready".
run_rc=0
OMS_PLAN_RUN_DELEGATE="$TMP/stub-delegate" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$prepo" --to codex --id t1 >"$TMP/run.out" 2>&1 || run_rc=$?
[ "$run_rc" = 0 ] || fail "plan-run did not recover the expired claim (exit $run_rc): $(cat "$TMP/run.out")"
grep -Fq 'pre-flight' "$TMP/run.out" || fail "plan-run did not report the pre-flight reclaim"
grep -Fq 'reclaimed t1' "$TMP/run.out" || fail "pre-flight did not reclaim t1"

"$PLAN" --repo "$prepo" show --id t1 | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert d["state"] == "claimed", d["state"]
assert d["claim_expired"] is False, d
' || fail "plan-run left t1 outside a live claim"
new_lease="$("$PLAN" --repo "$prepo" show --id t1 |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["lease_id"])')"
[ "$new_lease" != "$t1_lease" ] || fail "the dead worker's lease was not fenced"

# The live claim survived the pre-flight untouched.
"$PLAN" --repo "$prepo" show --id t2 | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert d["state"] == "claimed" and d["claim_expired"] is False, d
' || fail "pre-flight reclaim stole a live claim"

echo "read-time-expiry-smoke: ok"
