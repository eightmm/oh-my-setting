#!/usr/bin/env bash
set -euo pipefail

# Both single-pick consult paths fall back to the caller's own CLI when no
# other provider is installed. That is defensible — a fresh context beats no
# answer — but silence made every advise/consult on a one-CLI machine look like
# an outside read, which is the failure the caller-detection fix set out to
# remove. The fallback must announce itself on stderr, and only then.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-self-advice.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
mkdir -p "$TMP/home" "$TMP/bin" "$TMP/repo/.oms" "$TMP/locks"
export HOME="$TMP/home"
export OMS_LOCK_DIR="$TMP/locks"
export OMS_LOCK_FORCE_MKDIR=1
unset OMS_ADVISOR_PROVIDER OMS_CONSULT_PROVIDER OMS_AGENT 2>/dev/null || true

NOTE="note: no independent provider available; answering from the caller's own family (self-advice)"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

stub() {
  printf '%s\n' '#!/usr/bin/env bash' 'exit 0' > "$TMP/bin/$1"
  chmod +x "$TMP/bin/$1"
}

git -C "$TMP/repo" init -q
git -C "$TMP/repo" config user.name t
git -C "$TMP/repo" config user.email t@example.com

# A fixture PATH rather than a prepended one: the point of this test is which
# provider binaries exist, and the real PATH has all three. /usr/bin:/bin keeps
# git, sed, and mktemp reachable without reaching any agent CLI.
run() {  # run SCRIPT ARGS... -> stdout; stderr in $TMP/err, exit code in $rc
  rc=0
  (
    cd "$TMP/repo" &&
      env -u CLAUDE_CODE_ENTRYPOINT -u CODEX_SANDBOX \
        PATH="$TMP/bin:/usr/bin:/bin" HOME="$HOME" \
        OMS_LOCK_DIR="$OMS_LOCK_DIR" OMS_LOCK_FORCE_MKDIR=1 \
        CLAUDECODE=1 bash "$@"
  ) 2> "$TMP/err" || rc=$?
}

# 1. Only the caller's own CLI is installed: the fallback fires and says so,
#    while stdout still carries the provider the caller would parse.
stub claude

run "$ROOT/scripts/advise.sh" --prompt "self-advice probe" --dry-run > "$TMP/out"
[ "$rc" -eq 0 ] || fail "advise self-fallback should still succeed (exit $rc): $(cat "$TMP/err")"
grep -Fq "$NOTE" "$TMP/err" ||
  fail "advise self-fallback did not disclose self-advice: $(cat "$TMP/err")"
grep -q '^dry-run: claude ' "$TMP/out" ||
  fail "advise stdout contract changed: $(cat "$TMP/out")"
grep -Fq "$NOTE" "$TMP/out" &&
  fail "the disclosure must stay off stdout: $(cat "$TMP/out")"

run "$ROOT/scripts/agent-consult.sh" --prompt "self-advice probe" --dry-run --quiet > "$TMP/out"
[ "$rc" -eq 0 ] || fail "consult self-fallback should still succeed (exit $rc): $(cat "$TMP/err")"
grep -Fq "$NOTE" "$TMP/err" ||
  fail "consult self-fallback did not disclose self-advice: $(cat "$TMP/err")"
grep -Eq '(^|/)claude-' "$TMP/out" ||
  fail "consult stdout contract changed: $(cat "$TMP/out")"
grep -Fq "$NOTE" "$TMP/out" &&
  fail "the disclosure must stay off stdout: $(cat "$TMP/out")"
# One note per run, not one per peers() call site.
[ "$(grep -Fc "$NOTE" "$TMP/err")" -eq 1 ] ||
  fail "consult disclosed self-advice more than once: $(cat "$TMP/err")"

# --all reaches the same fallback: a one-CLI "panel" is the caller alone.
run "$ROOT/scripts/agent-consult.sh" --prompt "panel probe" --all --dry-run --quiet > "$TMP/out"
grep -Fq "$NOTE" "$TMP/err" ||
  fail "consult --all did not disclose a caller-only panel: $(cat "$TMP/err")"

# 2. A second provider exists: the pick is independent, so there is nothing to
#    disclose and the note must not appear.
stub codex

run "$ROOT/scripts/advise.sh" --prompt "independent probe" --dry-run > "$TMP/out"
[ "$rc" -eq 0 ] || fail "advise with a peer failed (exit $rc): $(cat "$TMP/err")"
grep -Fq "$NOTE" "$TMP/err" &&
  fail "advise disclosed self-advice while codex was installed: $(cat "$TMP/err")"
grep -q '^dry-run: codex ' "$TMP/out" ||
  fail "advise should have picked codex: $(cat "$TMP/out")"

run "$ROOT/scripts/agent-consult.sh" --prompt "independent probe" --dry-run --quiet > "$TMP/out"
[ "$rc" -eq 0 ] || fail "consult with a peer failed (exit $rc): $(cat "$TMP/err")"
grep -Fq "$NOTE" "$TMP/err" &&
  fail "consult disclosed self-advice while codex was installed: $(cat "$TMP/err")"
grep -Eq '(^|/)codex-' "$TMP/out" ||
  fail "consult should have picked codex: $(cat "$TMP/out")"

# 3. An explicit --to is the caller's own choice, not a silent fallback, so it
#    stays quiet even when it names the caller's family.
run "$ROOT/scripts/agent-consult.sh" --prompt "explicit probe" --to claude --dry-run --quiet > "$TMP/out"
[ "$rc" -eq 0 ] || fail "explicit consult failed (exit $rc): $(cat "$TMP/err")"
grep -Fq "$NOTE" "$TMP/err" &&
  fail "an explicit --to must not be reported as a fallback: $(cat "$TMP/err")"

echo "self-advice-disclosure-smoke: ok"
