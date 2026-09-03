#!/usr/bin/env bash
set -euo pipefail

# The seat-reliability contract, in three parts. One: the peer wall clock
# defaults per verb, because the single 5m default killed healthy seats doing
# long reasoning (a review seat died at the wall three times, a consult needed
# twenty minutes) while the wall only ever bounds a silent seat. Two: the
# failure ledger's seat rows are readable ignoring git state, because a
# provider CLI does not recover when the repo gains a commit. Three: the
# history is consumed and cleared at the call site — a call on a seat with
# unresolved no-answer rows warns before spending its wall, and a seat that
# answers again resolves its own rows so the warning cannot become permanent
# noise.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-seat-reliability.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "seat-reliability-smoke: $*" >&2; exit 1; }

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com

FL="$ROOT/scripts/fail-ledger.sh"

make_repo() {
  local dir="$1"
  mkdir -p "$dir"
  git -C "$dir" init -q -b main
  printf 'base\n' > "$dir/file.txt"
  git -C "$dir" add file.txt
  git -C "$dir" commit -q -m init
}

# --- 1. Verb-scoped wall defaults, explicit override stays authoritative ----

default_for() {  # default_for MA_KIND_VALUE_OR_EMPTY
  if [ -n "$1" ]; then
    MA_KIND="$1" bash -c '. "$1/scripts/lib/peer-common.sh"; ma_peer_timeout_default' _ "$ROOT"
  else
    bash -c '. "$1/scripts/lib/peer-common.sh"; ma_peer_timeout_default' _ "$ROOT"
  fi
}

[ "$(default_for ask)" = 15m ] || fail "ask default should be 15m"
[ "$(default_for call)" = 20m ] || fail "call default should be 20m"
[ "$(default_for review)" = 20m ] || fail "review default should be 20m"
[ "$(default_for delegate)" = 30m ] || fail "delegate default should be 30m"
[ "$(default_for '')" = 20m ] || fail "an unset verb rides call and should default to 20m"
[ "$(default_for weird)" = 5m ] || fail "an unknown verb should keep the conservative 5m"

wall="$(MA_KIND=review bash -c \
  '. "$1/scripts/lib/peer-common.sh"; ma_run_bounded() { printf "%s\n" "$1"; }; run_with_timeout ignored' \
  _ "$ROOT")"
[ "$wall" = 20m ] || fail "run_with_timeout should use the verb default, got $wall"

wall="$(MA_KIND=review OMS_PEER_TIMEOUT=7m bash -c \
  '. "$1/scripts/lib/peer-common.sh"; ma_run_bounded() { printf "%s\n" "$1"; }; run_with_timeout ignored' \
  _ "$ROOT")"
[ "$wall" = 7m ] || fail "an explicit OMS_PEER_TIMEOUT must override the verb default, got $wall"

# --- 2. check --ignore-state: seat history survives repo changes ------------

ledger_repo="$TMP/ledger-repo"
make_repo "$ledger_repo"
"$FL" --repo "$ledger_repo" record --kind cmd --cmd "peer-ask seat codex" --exit 124 \
  --summary "codex ask seat returned no answer (exit 124)" \
  --next "check the provider CLI, raise OMS_PEER_TIMEOUT, or drop the seat" \
  >/dev/null 2>&1 || fail "seat record failed"

rc=0; "$FL" --repo "$ledger_repo" check --cmd "peer-ask seat codex" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 3 ] || fail "unchanged state should report the unresolved seat, got $rc"

printf 'change\n' >> "$ledger_repo/file.txt"
git -C "$ledger_repo" add file.txt
git -C "$ledger_repo" commit -q -m change

rc=0; "$FL" --repo "$ledger_repo" check --cmd "peer-ask seat codex" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 0 ] || fail "the plain repo-command read should clear on git change, got $rc"

rc=0; "$FL" --repo "$ledger_repo" check --cmd "peer-ask seat codex" --ignore-state \
  >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 3 ] || fail "--ignore-state must keep seat history visible across git changes, got $rc"

rc=0; "$FL" --repo "$ledger_repo" list --ignore-state >/dev/null 2>&1 || rc=$?
[ "$rc" -ne 0 ] || fail "--ignore-state outside check must be rejected"

# --- 3. The call site consumes and clears the history -----------------------

proj="$TMP/seat"
make_repo "$proj"
mkdir -p "$proj/home" "$proj/bin"
cat > "$proj/bin/codex" <<'EOF'
#!/usr/bin/env bash
cat > /dev/null
echo "Verdict: the seat is healthy again. Evidence: this answer arrived well"
echo "inside the wall. Risk: none observed. Next: clear the seat's history."
EOF
chmod +x "$proj/bin/codex"

"$FL" --repo "$proj" record --kind cmd --cmd "peer-ask seat codex" --exit 124 \
  --summary "codex ask seat returned no answer (exit 124)" \
  --next "check the provider CLI, raise OMS_PEER_TIMEOUT, or drop the seat" \
  >/dev/null 2>&1 || fail "seat record failed"

run_ask() {
  local seat_home="$proj/home"
  HOME="$seat_home" NVM_DIR="$seat_home/.nvm" PATH="$proj/bin:/usr/bin:/bin" \
    OMS_PEER_TIMEOUT=60 OMS_PEER_KILL_AFTER=1 \
    "$ROOT/scripts/peer-ask.sh" --repo "$proj" --artifact-dir "$proj/artifacts" \
    --providers codex --no-memory --no-task --no-ml-context \
    --prompt "Seat reliability check" > "$proj/out" 2> "$proj/err"
}

run_ask || fail "one healthy stub seat should succeed: $(cat "$proj/err")"
grep -q "codex ask seat has unresolved no-answer history" "$proj/err" ||
  fail "a call on a seat with unresolved history must warn first: $(cat "$proj/err")"
grep -q "seat-health: unresolved no-answer history" "$proj/artifacts"/*.md ||
  fail "the artifact must carry the pre-call seat-health line"

rc=0; "$FL" --repo "$proj" check --cmd "peer-ask seat codex" --ignore-state \
  >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 0 ] || fail "a seat that answered must auto-resolve its history, got $rc"

run_ask || fail "the recovered seat should keep answering: $(cat "$proj/err")"
! grep -q "unresolved no-answer history" "$proj/err" ||
  fail "a recovered seat must not keep warning"

resolves="$(grep -c '"event": "resolved"' "$proj/.oms/failures.jsonl")"
[ "$resolves" -eq 1 ] ||
  fail "routine successes must not append resolve rows, got $resolves"

echo "seat-reliability-smoke: OK"
