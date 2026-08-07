#!/usr/bin/env bash
set -euo pipefail

# The advisor exists for an outside read, so its default routing must exclude
# the calling agent's own family. It used to trust OMS_AGENT alone; interactive
# sessions rarely export it, and an empty caller made the exclusion loop a
# no-op — a Claude session asking for independent advice got Claude.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-advisor-routing.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
mkdir -p "$TMP/home" "$TMP/bin" "$TMP/repo/.oms" "$TMP/locks"
export HOME="$TMP/home"
export OMS_LOCK_DIR="$TMP/locks"
export OMS_LOCK_FORCE_MKDIR=1
unset OMS_ADVISOR_PROVIDER OMS_AGENT 2>/dev/null || true

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

for cli in codex claude agy; do
  printf '%s\n' '#!/usr/bin/env bash' 'exit 0' > "$TMP/bin/$cli"
  chmod +x "$TMP/bin/$cli"
done
export PATH="$TMP/bin:$PATH"

git -C "$TMP/repo" init -q
git -C "$TMP/repo" config user.name t
git -C "$TMP/repo" config user.email t@example.com

pick() {  # pick ENV_ASSIGNMENTS... -> provider the dry run selected
  (
    cd "$TMP/repo" &&
      env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT -u CODEX_SANDBOX "$@" \
        bash "$ROOT/scripts/advise.sh" --prompt "routing probe" --dry-run
  ) | sed -n 's/^dry-run: \([a-z]*\) .*/\1/p'
}

[ "$(pick CLAUDECODE=1)" = codex ] ||
  fail "a claude session must not get a claude advisor by default"
[ "$(pick CODEX_SANDBOX=1)" = claude ] ||
  fail "a codex session must not get a codex advisor by default"
[ "$(pick OMS_AGENT=codex)" = claude ] ||
  fail "an explicit OMS_AGENT caller must be excluded"
out="$(
  cd "$TMP/repo" &&
    env CLAUDECODE=1 bash "$ROOT/scripts/advise.sh" \
      --prompt "routing probe" --to claude --dry-run
)"
printf '%s' "$out" | grep -q '^dry-run: claude ' ||
  fail "--to must still override the exclusion: $out"

# consult shares the exclusion contract: a claude session's automatic
# consult must not go to claude.
out="$(
  cd "$TMP/repo" &&
    env -u CLAUDE_CODE_ENTRYPOINT -u CODEX_SANDBOX CLAUDECODE=1 \
      bash "$ROOT/scripts/consult.sh" --prompt "routing probe" \
      --dry-run --quiet 2>&1
)" || fail "consult dry-run failed: $out"
if printf '%s' "$out" | grep -Eq '(^|/)claude-'; then
  fail "a claude session's automatic consult picked claude: $out"
fi
printf '%s' "$out" | grep -Eq '(^|/)(codex|antigravity)-' ||
  fail "consult dry-run should have picked a non-claude peer: $out"

echo "advisor-routing-smoke: ok"
