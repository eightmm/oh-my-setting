#!/usr/bin/env bash
set -euo pipefail

# oms advise --session gives a Codex or agy session what the Claude Code
# native advisor gets for free: the advisor reads the caller's recent history,
# not only the caller's own summary of it. The digest must come from the
# calling agent's real session (pinnable with --session-id when two live
# sessions share one worktree), ride AFTER the decision context, and fail
# hard when it cannot be produced — a silently thinner advisor prompt would
# claim a context it does not have.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-advisor-session.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
mkdir -p "$TMP/home" "$TMP/bin" "$TMP/repo/.oms" "$TMP/locks" "$TMP/codex-home"
export HOME="$TMP/home"
export OMS_LOCK_DIR="$TMP/locks"
export OMS_LOCK_FORCE_MKDIR=1
export OMS_CODEX_HOME="$TMP/codex-home"
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
REPO="$(cd "$TMP/repo" && pwd)"

codex_session() {  # codex_session PATH ID USER_LINE
  mkdir -p "$(dirname "$1")"
  {
    printf '{"type":"session_meta","payload":{"id":"%s","cwd":"%s"}}\n' "$2" "$REPO"
    printf '{"type":"event_msg","payload":{"type":"user_message","message":"%s"}}\n' "$3"
    printf '{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"done"}}\n'
  } > "$1"
}

advise_session() {  # advise_session ARGS... -> prompt artifact path on stdout
  (
    cd "$REPO" &&
      env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT CODEX_SANDBOX=1 OMS_AGENT=codex \
        bash "$ROOT/scripts/advise.sh" --prompt "ship the migration?" \
        --to claude --dry-run "$@"
  ) | sed -n 's/^dry-run: claude -> //p'
}

# 1. --session digests the caller's newest cwd-matching session into the
#    advisor prompt, after the decision context.
mine="$OMS_CODEX_HOME/sessions/2026/08/07/rollout-2026-08-07T01-00-00-cafe0001.jsonl"
codex_session "$mine" cafe0001 "migrate the ledger schema"
touch -t 202608070100 "$mine"
artifact="$(advise_session --session)"
[ -n "$artifact" ] && [ -f "$artifact" ] || fail "--session dry run wrote no artifact"
grep -q 'migrate the ledger schema' "$artifact" ||
  fail "advisor prompt is missing the caller session content"
grep -q '## Caller session history' "$artifact" ||
  fail "advisor prompt is missing the session digest section"
ctx="$(grep -n '## Decision context' "$artifact" | head -1 | cut -d: -f1)"
hist="$(grep -n '## Caller session history' "$artifact" | head -1 | cut -d: -f1)"
[ -n "$ctx" ] && [ -n "$hist" ] && [ "$hist" -gt "$ctx" ] ||
  fail "session digest must ride after the decision context (ctx=$ctx hist=$hist)"
# The digest carries goal/turns/files as evidence. The caller's own concluding
# reasoning stays out: an advisor shown the author's rationale converges on it.
if grep -q '## Last assistant summary' "$artifact"; then
  fail "advisor prompt must not carry the caller's own assistant summary"
fi

# 2. Two live sessions in one worktree: the newest-session default drifts to
#    the other session; --session-id pins the caller's own.
theirs="$OMS_CODEX_HOME/sessions/2026/08/07/rollout-2026-08-07T02-00-00-beef0002.jsonl"
codex_session "$theirs" beef0002 "unrelated peer session doing docs"
touch -t 202608070200 "$theirs"
artifact="$(advise_session --session)"
grep -q 'unrelated peer session doing docs' "$artifact" ||
  fail "expected the newest-session default to pick the peer session (precondition)"
artifact="$(advise_session --session-id cafe0001)"
grep -q 'migrate the ledger schema' "$artifact" ||
  fail "--session-id must pin the named session"
if grep -q 'unrelated peer session doing docs' "$artifact"; then
  fail "--session-id must exclude the other live session"
fi

# 3. Without --session the digest section must not appear.
artifact="$(advise_session)"
if grep -q '## Caller session history' "$artifact"; then
  fail "session digest attached without --session"
fi

# 4. A sensitive-looking session refuses by default (the digest crosses to
#    another provider) and the whole advise call fails hard, not thinner.
sens_home="$TMP/codex-home-sensitive"
vector="AK""IAIOSFODNN7EXAMPLE"
OMS_CODEX_HOME="$sens_home" codex_session \
  "$sens_home/sessions/2026/08/07/rollout-2026-08-07T03-00-00-feed0003.jsonl" \
  feed0003 "deploy with $vector"
if out="$(
  cd "$REPO" &&
    env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT CODEX_SANDBOX=1 OMS_AGENT=codex \
      OMS_CODEX_HOME="$sens_home" \
      bash "$ROOT/scripts/advise.sh" --prompt "ship it?" \
      --to claude --dry-run --session 2>&1
)"; then
  fail "sensitive session digest must fail the advise call by default"
fi
printf '%s' "$out" | grep -qi 'sensitive' ||
  fail "sensitive refusal must say why: $out"

# 4b. --allow-sensitive lifts only session-handoff's refusal; agent-call's
#     outbound scrub is a second, independent gate and must still block the
#     external call (defense in depth, not a bypass).
if out="$(
  cd "$REPO" &&
    env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT CODEX_SANDBOX=1 OMS_AGENT=codex \
      OMS_CODEX_HOME="$sens_home" \
      bash "$ROOT/scripts/advise.sh" --prompt "ship it?" \
      --to claude --dry-run --session --allow-sensitive 2>&1
)"; then
  fail "--allow-sensitive must not bypass the outbound scrub"
fi
printf '%s' "$out" | grep -q 'blocked' ||
  fail "outbound scrub block must be named, not a silent failure: $out"

# 5. --allow-sensitive without --session is a dangling flag, not a no-op.
if (
  cd "$REPO" &&
    env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT CODEX_SANDBOX=1 OMS_AGENT=codex \
      bash "$ROOT/scripts/advise.sh" --prompt "ship it?" \
      --to claude --dry-run --allow-sensitive >/dev/null 2>&1
); then
  fail "--allow-sensitive without --session must fail"
fi

# 6. An unidentifiable caller fails with guidance instead of guessing whose
#    history to attach.
if out="$(
  cd "$REPO" &&
    env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT -u CODEX_SANDBOX -u OMS_AGENT \
      bash "$ROOT/scripts/advise.sh" --prompt "ship it?" \
      --to claude --dry-run --session 2>&1
)"; then
  fail "--session with an unknown caller must fail"
fi
printf '%s' "$out" | grep -q 'OMS_AGENT' ||
  fail "unknown-caller failure must name the fix: $out"

echo "advisor-session-smoke: ok"
