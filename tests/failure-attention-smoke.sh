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
  printf '{"cwd":"%s"}' "$1" | "$ROOT/scripts/resume-hook.sh"
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

printf 'auto-update.sh apply\n' > "$au_cron"
rm -f "$au_state"
out="$(attention env)"
case "$out" in "attention: no-run"*) ;; *) fail "wired but never run must be no-run: $out" ;; esac

write_au_receipt "$TMP/not-the-owner" true
out="$(attention env)"
case "$out" in "attention: disabled"*) ;; *) fail "a non-owner checkout must read disabled: $out" ;; esac

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
