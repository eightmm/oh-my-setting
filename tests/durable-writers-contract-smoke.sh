#!/usr/bin/env bash
set -euo pipefail

# Durable-writers boundary contract: every path that persists text into .oms
# state (or refuses to) is exercised with the same two sentinels — a
# secret-shaped value that must ALWAYS be refused or absent from the artifact,
# and an absolute home path that must either be normalized to ~ (repo-local
# git-ignored sinks) or refused (the strict shared-memory store). A new
# durable writer belongs in this table; a writer that bypasses the scrubber
# should fail here, not in production. goal-drive's own sinks (progress rows,
# commit subjects) are covered where cheap; its full delegate loop lives in
# tests/autonomy-plan-run-smoke.sh.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-durable-writers.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

export HOME="$TMP/home" TMPDIR="$TMP/tmp"
export NVM_DIR="$HOME/.nvm"
mkdir -p "$HOME" "$TMPDIR"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com

# Sentinels are assembled at runtime so this file itself stays scrubber-clean.
LEAK="AK""IAIOSFODNN7EXAMPLE"
MPATH="/ho""me/sentinel-user/proj/data.txt"

mk_repo() {
  local r="$1"
  mkdir -p "$r"
  git -C "$r" init -q
  git -C "$r" commit -q --allow-empty -m seed
}

# --- fail-ledger: secret refused, home path normalized --------------------
repo="$TMP/ledger"; mk_repo "$repo"
if ( cd "$repo" && bash "$ROOT/scripts/fail-ledger.sh" record \
    --cmd "deploy with $LEAK" --exit 1 >/dev/null 2>&1 ); then
  fail "fail-ledger must refuse a secret-bearing command"
fi
[ ! -s "$repo/.oms/failures.jsonl" ] || fail "refused ledger row must not persist"
( cd "$repo" && bash "$ROOT/scripts/fail-ledger.sh" record \
    --cmd "python $MPATH --epochs 1" --exit 1 >/dev/null 2>&1 ) ||
  fail "fail-ledger must accept a path-bearing command"
grep -Fq 'python ~/proj/data.txt' "$repo/.oms/failures.jsonl" ||
  fail "ledger row must normalize the home path"
if grep -Fq "$MPATH" "$repo/.oms/failures.jsonl"; then
  fail "raw home path leaked into the ledger"
fi

# --- session-handoff: secret refused, home path normalized ----------------
ch="$TMP/claude-home"
proj="$ch/projects/-proj-contract"
mkdir -p "$proj"
printf '{"type":"user","message":{"role":"user","content":"use %s"}}\n' "$LEAK" \
  > "$proj/s1.jsonl"
if OMS_CLAUDE_HOME="$ch" bash "$ROOT/scripts/session-handoff.sh" capture \
    --agent claude --cwd /proj/contract --out "$TMP/d1.md" >/dev/null 2>&1; then
  fail "handoff capture must refuse a secret-bearing transcript"
fi
[ ! -f "$TMP/d1.md" ] || fail "refused digest must not persist"
{ printf '{"type":"user","message":{"role":"user","content":"fix %s"}}\n' "$MPATH"
  printf '{"type":"user","message":{"role":"user","content":"then run the tests"}}\n'
} > "$proj/s1.jsonl"
OMS_CLAUDE_HOME="$ch" bash "$ROOT/scripts/session-handoff.sh" capture \
    --agent claude --cwd /proj/contract --out "$TMP/d2.md" >/dev/null 2>&1 ||
  fail "handoff capture must accept a path-bearing transcript"
grep -Fq 'fix ~/proj/data.txt' "$TMP/d2.md" || fail "digest must normalize the home path"
if grep -Fq "$MPATH" "$TMP/d2.md"; then
  fail "raw home path leaked into the digest body"
fi

# --- agent-memory append: the shared store stays strict on both tiers -----
repo="$TMP/memory"; mk_repo "$repo"
if ( cd "$repo" && bash "$ROOT/scripts/agent-memory.sh" append --agent t \
    --text "token is $LEAK" >/dev/null 2>&1 ); then
  fail "memory append must refuse a secret"
fi
if ( cd "$repo" && bash "$ROOT/scripts/agent-memory.sh" append --agent t \
    --text "see $MPATH" >/dev/null 2>&1 ); then
  fail "the shared memory store keeps the strict tier: home paths refuse"
fi
if [ -f "$repo/.oms/memory/shared.md" ] &&
  grep -Eq "$LEAK|sentinel-user" "$repo/.oms/memory/shared.md"; then
  fail "sentinel leaked into shared memory"
fi

# --- journal distill: a refused lesson skips, never crashes ---------------
repo="$TMP/distill"; mk_repo "$repo"
python3 - "$ROOT/scripts/lib" "$repo" "$LEAK" <<'PY'
import sys
sys.path.insert(0, sys.argv[1])
import work_journal as wj
store = wj.JournalStore(sys.argv[2], timezone_name="UTC")
for source_id, occurred_at in (
    ("b1", "2026-07-30T02:00:00Z"),
    ("b2", "2026-08-01T02:00:00Z"),
):
    store.record_event({
        "event_type": "agent_state",
        "occurred_at": occurred_at,
        "source": {"type": "fixture", "id": source_id},
        "verification_status": "not_verified",
        "blocker": "deploy blocked by %s rotation" % sys.argv[3],
    })
PY
out="$(bash "$ROOT/scripts/journal.sh" distill --repo "$repo" 2>&1)" ||
  fail "distill must not crash on a scrubber-refused lesson: $out"
printf '%s\n' "$out" | grep -Fq 'refused by the memory writer' ||
  fail "distill must report the skipped lesson: $out"
if [ -f "$repo/.oms/memory/shared.md" ] &&
  grep -Fq "$LEAK" "$repo/.oms/memory/shared.md"; then
  fail "secret leaked through distill into shared memory"
fi
second="$(bash "$ROOT/scripts/journal.sh" distill --repo "$repo" 2>&1)"
printf '%s\n' "$second" | grep -Fq 'nothing to promote' ||
  fail "distill marker must advance past the refused lesson: $second"

# --- agent-plan: secret in goal/accept text refuses; runtime acceptance ---
# output never lands raw in receipts (digest only).
repo="$TMP/accept"; mk_repo "$repo"
if ( cd "$repo" && bash "$ROOT/scripts/agent-plan.sh" init --goal g \
    --accept "curl -H 'x: $LEAK' https://x" >/dev/null 2>&1 ); then
  fail "init must refuse a secret-bearing acceptance command"
fi
[ ! -f "$repo/.oms/plan/tasks.json" ] || fail "refused plan must not persist"
# The runtime vector: the command text scans clean but PRINTS a secret.
( cd "$repo" && bash "$ROOT/scripts/agent-plan.sh" init --goal g \
    --accept "printf 'AK%s\n' IAIOSFODNN7EXAMPLE; false" >/dev/null )
if ( cd "$repo" && bash "$ROOT/scripts/agent-plan.sh" accept >/dev/null 2>&1 ); then
  fail "failing acceptance must exit nonzero"
fi
grep -Fq '"kind": "acceptance"' "$repo/.oms/plan/progress.jsonl" ||
  fail "acceptance receipt missing"
if grep -Fq "$LEAK" "$repo/.oms/plan/progress.jsonl"; then
  fail "acceptance output leaked raw into progress receipts"
fi

# --- draft-pr intent: verifier text is durable and therefore strict --------
repo="$TMP/draft-pr"; mk_repo "$repo"
git -C "$repo" branch -M main
printf '/.oms/\n' > "$repo/.gitignore"
git -C "$repo" add .gitignore
git -C "$repo" commit -qm ignore
bare="$TMP/draft-pr.git"
git init -q --bare "$bare"
git -C "$repo" remote add origin "$bare"
git -C "$repo" push -q -u origin main
git -C "$repo" checkout -qb codex/durable-writer
git -C "$repo" commit -q --allow-empty -m 'feat: fixture'
real_git="$(command -v git)"
mkdir -p "$TMP/draft-bin"
cat > "$TMP/draft-bin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *' remote get-url --push --all origin') echo 'git@github.com:eightmm/oh-my-setting.git'; exit 0 ;;
  *' remote get-url --all origin') echo 'git@github.com:eightmm/oh-my-setting.git'; exit 0 ;;
esac
translated=()
for arg in "\$@"; do
  [ "\$arg" != 'git@github.com:eightmm/oh-my-setting.git' ] || arg="$bare"
  translated+=("\$arg")
done
exec "$real_git" "\${translated[@]}"
EOF
cat > "$TMP/draft-bin/gh" <<'EOF'
#!/usr/bin/env bash
case "${1:-} ${2:-}" in
  'auth status') exit 0 ;;
  'repo view') printf '{"nameWithOwner":"eightmm/oh-my-setting","viewerPermission":"WRITE"}\n' ;;
  'api --hostname') printf 'fixture-user\n' ;;
  *) exit 9 ;;
esac
EOF
chmod +x "$TMP/draft-bin/git" "$TMP/draft-bin/gh"
if OMS_GIT_BIN="$TMP/draft-bin/git" OMS_GH_BIN="$TMP/draft-bin/gh" \
    bash "$ROOT/scripts/draft-pr.sh" --repo "$repo" prepare --base main \
      --verify "printf '%s' '$LEAK' >/dev/null" >/dev/null 2>&1; then
  fail "draft-pr must refuse a secret-bearing durable verifier"
fi
if OMS_GIT_BIN="$TMP/draft-bin/git" OMS_GH_BIN="$TMP/draft-bin/gh" \
    bash "$ROOT/scripts/draft-pr.sh" --repo "$repo" prepare --base main \
      --verify "printf '%s' '$MPATH' >/dev/null" >/dev/null 2>&1; then
  fail "draft-pr must refuse a machine-path-bearing durable verifier"
fi
if [ -d "$repo/.oms/publish" ] && grep -R -Eq "$LEAK|sentinel-user" "$repo/.oms/publish"; then
  fail "sentinel leaked into a Draft PR publication intent"
fi

echo "durable-writers-contract-smoke: ok"
