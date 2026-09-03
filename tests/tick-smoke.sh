#!/usr/bin/env bash
set -euo pipefail

# `oms tick` against fixture repos under TMP with an XDG home of its own and a
# systemctl stub, so nothing here reaches the real user manager, crontab,
# journal config, or Codex. Covers the registry, the sweep receipt, the idle
# thread threshold, gc staying off by default, and install/uninstall ownership.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-tick.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
TICK="$ROOT/scripts/tick.sh"
export XDG_CONFIG_HOME="$TMP/xdg" XDG_RUNTIME_DIR="$TMP/rt" OMS_INSTALL_RECEIPT="$TMP/no-receipt.json" \
  OMS_WORK_JOURNAL_CONFIG="$TMP/no-journal.json" OMS_TICK_THREAD_IDLE_DAYS=7
mkdir -p "$XDG_RUNTIME_DIR" "$TMP/bin"
cat > "$TMP/bin/systemctl" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$TMP/systemctl.log"
[ "\$2" != show ] || printf '%s\n' "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.timer"
EOF
chmod +x "$TMP/bin/systemctl"
export PATH="$TMP/bin:$PATH"

fail() { echo "FAIL: $*" >&2; exit 1; }
make_repo() {
  mkdir -p "$1/.oms"; git -C "$1" init -q; printf '*\n' > "$1/.oms/.gitignore"
}

# --- registry -----------------------------------------------------------------
a="$TMP/a"; b="$TMP/b"; make_repo "$a"; make_repo "$b"
"$TICK" register --repo "$a" | grep -q '^registered' || fail "register must add a repo"
"$TICK" register --repo "$a" | grep -q 'already registered' || fail "register must dedupe"
out="$("$TICK" register --repo "$TMP" 2>&1 || true)"
printf '%s' "$out" | grep -q 'no .oms' || fail "an unadopted dir must be refused: $out"
[ "$(wc -l < "$XDG_CONFIG_HOME/oh-my-setting/tick-repos.txt")" -eq 1 ] || fail "registry must hold one line"

# --- sweep: idle thread closed, fresh one kept, gc off, receipt written --------
THREAD="$ROOT/scripts/thread.sh"
"$THREAD" new --repo "$a" --id old-thread --topic old >/dev/null
"$THREAD" append --repo "$a" --id old-thread --role note --text hello >/dev/null
"$THREAD" new --repo "$a" --id new-thread --topic new >/dev/null
touch -d '10 days ago' "$a/.oms/threads/old-thread.jsonl"
out="$("$TICK" run --repo "$a")"
printf '%s' "$out" | grep -q 'threads_closed=1' || fail "the idle thread must be closed: $out"
"$THREAD" list --repo "$a" | grep -q 'new-thread' || fail "a fresh thread must stay open"
if "$THREAD" list --repo "$a" | grep -q 'old-thread'; then fail "the idle thread must not stay open"; fi
python3 - "$a/.oms/tick/last.json" <<'PY' || fail "sweep receipt is wrong: $(cat "$a/.oms/tick/last.json")"
import json, sys
r = json.load(open(sys.argv[1]))
assert r["gc"] == "skipped" and r["threads_closed"] == ["old-thread"], r
assert isinstance(r["journal_rc"], int) and isinstance(r["reconcile_rc"], int), r
PY
"$TICK" run | grep -q "swept $a" || fail "run without --repo must use the registry"
"$TICK" run --repo "$b" --dry-run | grep -q "would sweep $b" || fail "dry-run must not sweep"
[ ! -f "$b/.oms/tick/last.json" ] || fail "dry-run must not write a receipt"

# --- install / status / uninstall through the stub --------------------------
(cd "$b" && "$TICK" install --method systemd --dry-run) | grep -q 'would install' || fail "install dry-run must print"
[ ! -f "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.timer" ] || fail "dry-run must not write units"
grep -Fxq "$b" "$XDG_CONFIG_HOME/oh-my-setting/tick-repos.txt" && fail "a dry-run install must not register the cwd"
(cd "$b" && "$TICK" install --method systemd) | grep -q 'systemd timer installed' || fail "install must report"
grep -Fxq "$b" "$XDG_CONFIG_HOME/oh-my-setting/tick-repos.txt" || fail "install must register the adopted cwd"
grep -q 'OnCalendar=hourly' "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.timer" || fail "timer must be hourly"
grep -q "tick.sh\" run" "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.service" || fail "service must run the tick"
grep -q 'enable --now oh-my-setting-tick.timer' "$TMP/systemctl.log" || fail "install must enable the timer"
st="$("$TICK" status)"
printf '%s' "$st" | grep -q 'timer: systemd (owned)' || fail "status must see the owned timer: $st"
printf '%s' "$st" | grep -q "$a  last:" || fail "status must show the last sweep: $st"
"$TICK" uninstall | grep -q 'removed' || fail "uninstall must report"
[ ! -f "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.timer" ] || fail "uninstall must remove the timer"
grep -q 'disable --now oh-my-setting-tick.timer' "$TMP/systemctl.log" || fail "uninstall must disable the owned timer"
st="$("$TICK" status)"
printf '%s' "$st" | grep -q 'timer: none' || fail "status must report no timer: $st"
"$TICK" unregister --repo "$a" >/dev/null
"$TICK" unregister --repo "$b" >/dev/null
"$TICK" run | grep -q 'nothing registered' || fail "an empty registry must say so"

echo "tick-smoke: ok"
