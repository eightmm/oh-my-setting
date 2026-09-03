#!/usr/bin/env bash
set -euo pipefail

# The failure-attention split, end to end: fail-ledger rows flow through
# repo-state into inbox and the resume hook, and every surface must agree on
# which rows deserve attention. A hook row seen once is retiring noise on its
# way to TTL retirement (P3, no alarm); a deliberate record or a recurring
# hook failure is actionable (P1); an expired hook row is invisible
# everywhere. The resume hook applies the same read-time predicate — it used
# to re-parse the ledger raw and announce rows every other surface had
# already retired.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-failure-attention.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

make_repo() {
  local dir="$1"
  mkdir -p "$dir/.oms"
  git -C "$dir" init -q
  printf '*\n' > "$dir/.oms/.gitignore"
}

inbox_of() {  # inbox_of REPO -> stdout
  (cd "$1" && "$ROOT/scripts/inbox.sh")
}

resume_of() {  # resume_of REPO -> stdout
  # The graph line is not what this suite measures, and the opt-out keeps the
  # hook from detaching a real project-graph build into every fixture repo.
  printf '{"cwd":"%s"}' "$1" | OMS_GRAPH_AUTOBUILD=0 "$ROOT/scripts/resume-hook.sh"
}

# --- 1. fresh hook x1: retiring, never P1 ----------------------------------
repo="$TMP/one-hook"
make_repo "$repo"
"$ROOT/scripts/fail-ledger.sh" record --repo "$repo" --kind hook \
  --cmd "session-handoff capture --agent claude (session-end)" --exit 1 \
  --summary "capture failed once" >/dev/null
out="$(inbox_of "$repo")"
printf '%s' "$out" | grep -q 'P3 retiring-failures' ||
  fail "a single hook failure must be a P3 retiring item: $out"
if printf '%s' "$out" | grep -q 'P1 open-failures'; then
  fail "a single hook failure must not raise P1: $out"
fi
r="$(resume_of "$repo")"
printf '%s' "$r" | grep -q 'auto-retire on TTL' ||
  fail "resume must label one-shot hook noise as retiring: $r"
ledger_json="$("$ROOT/scripts/fail-ledger.sh" --repo "$repo" list --unresolved --json)"
printf '%s' "$ledger_json" | python3 -c '
import json, sys
rows = json.load(sys.stdin)["failures"]
assert len(rows) == 1, rows
assert rows[0]["attention"] == "retiring", rows
assert rows[0]["actionable"] is False, rows
' || fail "the canonical ledger projection must classify a one-shot hook as retiring: $ledger_json"
runtime_json="$("$ROOT/scripts/runtime.sh" --repo "$repo" envelope show)"
printf '%s' "$runtime_json" | python3 -c '
import json, sys
row = json.load(sys.stdin)
assert not any(action["id"] == "resolve_blocker" for action in row["next_actions"]), row["next_actions"]
assert row["failures"] == [], row["failures"]
' || fail "runtime must not promote retiring hook noise to a blocker: $runtime_json"

# --- 2. hook x2: recurrence is actionable ----------------------------------
"$ROOT/scripts/fail-ledger.sh" record --repo "$repo" --kind hook \
  --cmd "session-handoff capture --agent claude (session-end)" --exit 1 \
  --summary "capture failed again" >/dev/null
out="$(inbox_of "$repo")"
printf '%s' "$out" | grep -q 'P1 open-failures' ||
  fail "a recurring hook failure must be P1 actionable: $out"
printf '%s' "$out" | grep -q 'actionable failure' ||
  fail "the P1 item must speak in actionable terms: $out"
r="$(resume_of "$repo")"
printf '%s' "$r" | grep -q 'failures: 1 actionable' ||
  fail "resume must count the recurring hook failure as actionable: $r"
ledger_json="$("$ROOT/scripts/fail-ledger.sh" --repo "$repo" list --unresolved --json)"
printf '%s' "$ledger_json" | python3 -c '
import json, sys
rows = json.load(sys.stdin)["failures"]
assert len(rows) == 1, rows
assert rows[0]["attention"] == "actionable", rows
assert rows[0]["actionable"] is True, rows
' || fail "the canonical ledger projection must classify a recurring hook as actionable: $ledger_json"
runtime_json="$("$ROOT/scripts/runtime.sh" --repo "$repo" envelope show)"
printf '%s' "$runtime_json" | python3 -c '
import json, sys
row = json.load(sys.stdin)
assert any(action["id"] == "resolve_blocker" for action in row["next_actions"]), row["next_actions"]
assert len(row["failures"]) == 1, row["failures"]
' || fail "runtime must promote a recurring hook to one blocker: $runtime_json"

# --- 3. deliberate record x1: actionable immediately ------------------------
repo2="$TMP/deliberate"
make_repo "$repo2"
"$ROOT/scripts/fail-ledger.sh" record --repo "$repo2" \
  --cmd "pytest tests/test_real.py" --exit 1 --summary "assertion failed" >/dev/null
out="$(inbox_of "$repo2")"
printf '%s' "$out" | grep -q 'P1 open-failures' ||
  fail "a deliberate record must be P1 on first sight: $out"
r="$(resume_of "$repo2")"
printf '%s' "$r" | grep -q 'failures: 1 actionable' ||
  fail "resume must surface a deliberate record: $r"

# --- 4. expired hook row: invisible on every surface ------------------------
repo3="$TMP/expired"
make_repo "$repo3"
"$ROOT/scripts/fail-ledger.sh" record --repo "$repo3" --kind hook \
  --cmd "some hook thing" --exit 1 --summary "old noise" >/dev/null
sleep 2
out="$(OMS_HOOK_FAIL_TTL=1 inbox_of "$repo3")"
if printf '%s' "$out" | grep -qE 'open-failures|retiring-failures'; then
  fail "an expired hook row must not appear in inbox: $out"
fi
r="$(OMS_HOOK_FAIL_TTL=1 resume_of "$repo3")"
if printf '%s' "$r" | grep -q 'failures:'; then
  fail "an expired hook row must not appear at session start: $r"
fi
# An invalid TTL must degrade, not crash the surfaces.
OMS_HOOK_FAIL_TTL=invalid "$ROOT/scripts/fail-ledger.sh" list --repo "$repo3" \
  >/dev/null 2>&1 || fail "invalid TTL must not crash the ledger listing"

# --- 5. resolve resets: a later fresh hook row is retiring again ------------
repo4="$TMP/resolve-reset"
make_repo "$repo4"
"$ROOT/scripts/fail-ledger.sh" record --repo "$repo4" \
  --cmd "flaky gate" --exit 1 --summary "first failure" >/dev/null
"$ROOT/scripts/fail-ledger.sh" resolve --repo "$repo4" --cmd "flaky gate" \
  >/dev/null 2>&1 || true
"$ROOT/scripts/fail-ledger.sh" record --repo "$repo4" --kind hook \
  --cmd "flaky gate" --exit 1 --summary "hook saw it once" >/dev/null
out="$(inbox_of "$repo4")"
printf '%s' "$out" | grep -q 'P3 retiring-failures' ||
  fail "post-resolve fresh hook row must be retiring, not haunted by old counts: $out"
if printf '%s' "$out" | grep -q 'P1 open-failures'; then
  fail "resolve must reset the recurrence count: $out"
fi

# --- resolve refuses a fingerprint the ledger never saw ------------------------
if "$ROOT/scripts/fail-ledger.sh" resolve --repo "$repo4" --fingerprint 0123456789abcdef \
    --how "typo" >/dev/null 2>"$TMP/unknown.err"; then
  fail "resolving an unknown fingerprint must be refused"
fi
grep -q 'unknown fingerprint' "$TMP/unknown.err" || fail "the refusal must name the cause: $(cat "$TMP/unknown.err")"
if grep -q '0123456789abcdef' "$repo4/.oms/failures.jsonl"; then
  fail "a refused resolve must not append a phantom row"
fi

# --- 6. one failure on an older commit: stale, not actionable ---------------
repo5="$TMP/stale"
make_repo "$repo5"
"$ROOT/scripts/fail-ledger.sh" record --repo "$repo5" --kind verify \
  --cmd "bash scripts/check.sh" --exit 1 --summary "gate failed" >/dev/null
git -C "$repo5" -c user.name=t -c user.email=t@t commit -q --allow-empty -m base
ledger_json="$("$ROOT/scripts/fail-ledger.sh" --repo "$repo5" list --unresolved --json)"
printf '%s' "$ledger_json" | python3 -c '
import json, sys
rows = json.load(sys.stdin)["failures"]
assert len(rows) == 1 and rows[0]["attention"] == "stale", rows
assert rows[0]["actionable"] is False and rows[0]["retiring"] is False, rows
' || fail "one failure recorded against another commit must be stale: $ledger_json"
r="$(resume_of "$repo5")"
printf '%s' "$r" | grep -q 'failures: 1 stale on an older commit' ||
  fail "resume must count the stale row apart from actionable ones: $r"
out="$(inbox_of "$repo5")"
printf '%s' "$out" | grep -q 'P3 stale-failures' ||
  fail "inbox must file a stale row under P3: $out"
if printf '%s' "$out" | grep -q 'P1 open-failures'; then
  fail "a stale row must not raise P1: $out"
fi
runtime_json="$("$ROOT/scripts/runtime.sh" --repo "$repo5" envelope show)"
printf '%s' "$runtime_json" | python3 -c '
import json, sys
row = json.load(sys.stdin)
assert row["failures"] == [], row["failures"]
assert not any(action["id"] == "resolve_blocker" for action in row["next_actions"]), row["next_actions"]
' || fail "runtime must accept the stale state and not promote it to a blocker: $runtime_json"
"$ROOT/scripts/fail-ledger.sh" record --repo "$repo5" --kind verify \
  --cmd "bash scripts/check.sh" --exit 1 --summary "gate failed again" >/dev/null
r="$(resume_of "$repo5")"
printf '%s' "$r" | grep -q 'failures: 1 actionable' ||
  fail "a recurrence across commits is tree-independent and actionable: $r"
runtime_json="$("$ROOT/scripts/runtime.sh" --repo "$repo5" envelope show)"
printf '%s' "$runtime_json" | python3 -c '
import json, sys
row = json.load(sys.stdin)
assert len(row["failures"]) == 1, row["failures"]
' || fail "runtime must promote the recurrence: $runtime_json"

# --- auto-update attention: one verdict over intent, wiring, outcome --------
# Codex's matrix: enabled+fresh, enabled+failed, enabled+overdue,
# disabled+historical-failure (the false-alarm trap: an opted-out machine
# keeps old status files), enabled+unwired, enabled+no-run, non-owner.
au_receipt="$TMP/au-receipt.json"
au_state="$TMP/au-state"
au_xdg="$TMP/au-xdg"
au_cron="$TMP/au-cron"
mkdir -p "$au_xdg/systemd/user"
: > "$au_cron"

write_au_receipt() {  # write_au_receipt OWNER ENABLED
  python3 - "$au_receipt" "$1" "$2" <<'PY'
import json, sys
json.dump({"schema": 2, "source_root": sys.argv[2], "ref": "main",
           "components": {"auto_update": sys.argv[3] == "true"}},
          open(sys.argv[1], "w"))
PY
}

attention() {  # attention EXTRA_ENV... -> verdict line
  env OMS_INSTALL_RECEIPT="$au_receipt" XDG_CONFIG_HOME="$au_xdg" \
    OH_MY_SETTING_AUTO_UPDATE_CRON_FILE="$au_cron" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$au_state" \
    "$@" "$ROOT/scripts/auto-update.sh" attention
}

write_au_receipt "$ROOT" true
printf 'auto-update.sh apply\n' > "$au_cron"

printf 'last_run=%s\nstatus=up_to_date\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$au_state"
out="$(attention env)"
case "$out" in "attention: ok"*) ;; *) fail "enabled+fresh must be ok: $out" ;; esac

printf 'last_run=%s\nstatus=failed\nmessage=apply failed: error: codex command is required\n' \
  "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$au_state"
out="$(attention env)"
case "$out" in
  "attention: failed"*"codex command is required"*) ;;
  *) fail "enabled+failed must carry the real error: $out" ;;
esac

printf 'last_run=2026-01-01T00:00:00Z\nstatus=up_to_date\n' > "$au_state"
out="$(attention env)"
case "$out" in "attention: overdue"*) ;; *) fail "an ancient last_run must be overdue: $out" ;; esac

write_au_receipt "$ROOT" false
printf 'last_run=2026-01-01T00:00:00Z\nstatus=failed\nmessage=old failure\n' > "$au_state"
out="$(attention env)"
case "$out" in "attention: disabled"*) ;; *) fail "opt-out beats a historical failure: $out" ;; esac

write_au_receipt "$ROOT" true
: > "$au_cron"
out="$(attention env)"
case "$out" in "attention: unwired"*) ;; *) fail "enabled without trigger must be unwired: $out" ;; esac

# A unit file is not a trigger: the manager follows the timers.target.wants
# link. A written-but-never-enabled (or later disabled) unit used to read as
# wired here while status.sh called the same file "(disabled)" — and the only
# hint of a dead updater was the overdue heuristic, days later.
au_unit_dir="$au_xdg/systemd/user"
mkdir -p "$au_unit_dir/timers.target.wants"
printf '[Timer]\nOnCalendar=daily\n' > "$au_unit_dir/oh-my-setting-autoupdate.timer"
printf 'last_run=%s\nstatus=up_to_date\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$au_state"
out="$(attention env)"
case "$out" in
  "attention: unwired"*"not enabled"*) ;;
  *) fail "a disabled systemd unit must read unwired: $out" ;;
esac
ln -s ../oh-my-setting-autoupdate.timer \
  "$au_unit_dir/timers.target.wants/oh-my-setting-autoupdate.timer"
out="$(attention env)"
case "$out" in "attention: ok"*) ;; *) fail "an enabled systemd timer must read ok: $out" ;; esac
rm -f "$au_unit_dir/timers.target.wants/oh-my-setting-autoupdate.timer" \
  "$au_unit_dir/oh-my-setting-autoupdate.timer"

printf 'auto-update.sh apply\n' > "$au_cron"
rm -f "$au_state"
out="$(attention env)"
case "$out" in "attention: no-run"*) ;; *) fail "wired but never run must be no-run: $out" ;; esac

write_au_receipt "$TMP/not-the-owner" true
out="$(attention env)"
case "$out" in "attention: disabled"*) ;; *) fail "a non-owner checkout must read disabled: $out" ;; esac

# A state file with no readable status is a torn write or corruption, not a
# green light: an empty file used to fall through to "attention: ok",
# masking a failed run at the exact moment its state was being rewritten.
write_au_receipt "$ROOT" true
printf 'auto-update.sh apply\n' > "$au_cron"
: > "$au_state"
out="$(attention env)"
case "$out" in "attention: unknown"*) ;; *) fail "an empty state file must read unknown, not ok: $out" ;; esac
printf 'garbage without a key\n' > "$au_state"
out="$(attention env)"
case "$out" in "attention: unknown"*) ;; *) fail "an unreadable state file must read unknown: $out" ;; esac

# End to end: the verdict rides repo-state into inbox as a P1 item.
write_au_receipt "$ROOT" true
printf 'auto-update.sh apply\n' > "$au_cron"
printf 'last_run=%s\nstatus=failed\nmessage=apply failed: error: codex command is required\n' \
  "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$au_state"
repo5="$TMP/au-inbox"
make_repo "$repo5"
out="$(cd "$repo5" && env OMS_INSTALL_RECEIPT="$au_receipt" XDG_CONFIG_HOME="$au_xdg" \
  OH_MY_SETTING_AUTO_UPDATE_CRON_FILE="$au_cron" \
  OH_MY_SETTING_AUTO_UPDATE_STATE="$au_state" "$ROOT/scripts/inbox.sh")"
printf '%s' "$out" | grep -q 'P1 auto-update-failed' ||
  fail "a failed updater must be a P1 inbox item: $out"
printf '%s' "$out" | grep -q 'codex command is required' ||
  fail "the inbox item must carry the failure detail: $out"

echo "failure-attention-smoke: ok"
