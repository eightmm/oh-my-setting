#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-executor-tests.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
fail() { echo "FAIL: $*" >&2; exit 1; }
contains() { grep -Fq -- "$2" "$1" || fail "$1 missing: $2"; }

make_repo() {
  mkdir -p "$1"
  git -C "$1" init -q -b main
  git -C "$1" config user.email test@example.com
  git -C "$1" config user.name test
  printf 'base\n' > "$1/file.txt"
  git -C "$1" add file.txt
  git -C "$1" commit -qm init
}

create_executor() {
  printf '# Specialization\n\n%s\n' "$3" > "$1/$2.proposal.md"
  "$ROOT/scripts/agent-executor.sh" create --repo "$1" --id "$2" \
    --provider codex --strategy implementation-worker --allowed '.' \
    --soul-file "$1/$2.proposal.md" >/dev/null
  "$ROOT/scripts/agent-executor.sh" freeze --repo "$1" --id "$2" >/dev/null
}

test_lifecycle() {
  local repo="$TMP/lifecycle" tool="$ROOT/scripts/agent-executor.sh" hash rc=0
  make_repo "$repo"
  printf '# Specialization\n\nEXECUTOR-CUSTOM-MARKER\n' > "$repo/proposal.md"
  "$tool" create --repo "$repo" --id exec1 --provider codex \
    --strategy implementation-worker --task-id t1 \
    --allowed 'src/,tests/' --forbidden 'src/private/' \
    --verify 'bash tests/smoke.sh' --soul-file "$repo/proposal.md" >/dev/null
  "$tool" validate --repo "$repo" --id exec1 >/dev/null
  "$tool" freeze --repo "$repo" --id exec1 >/dev/null
  "$tool" brief --repo "$repo" --id exec1 > "$repo/brief.md"
  contains "$repo/brief.md" 'EXECUTOR-CUSTOM-MARKER'
  contains "$repo/brief.md" 'allowed_paths: src/, tests/'
  contains "$repo/brief.md" 'forbidden_paths: src/private/'
  contains "$repo/brief.md" 'verify: bash tests/smoke.sh'
  python3 - "$repo/.oms/executors/exec1/meta.json" <<'PY'
import hashlib, json, pathlib, sys
p = pathlib.Path(sys.argv[1]); d = json.loads(p.read_text())
assert d["state"] == "frozen" and d["provider"] == "codex"
assert d["strategy"] == "implementation-worker"
assert d["allowed_paths"] == ["src/", "tests/"]
assert d["forbidden_paths"] == ["src/private/"]
assert d["soul_sha256"] == hashlib.sha256(p.with_name("SOUL.md").read_bytes()).hexdigest()
PY
  hash="$(python3 -c "import json;print(json.load(open('$repo/.oms/executors/exec1/meta.json'))['soul_sha256'])")"
  "$tool" freeze --repo "$repo" --id exec1 >/dev/null
  [ "$hash" = "$(python3 -c "import json;print(json.load(open('$repo/.oms/executors/exec1/meta.json'))['soul_sha256'])")" ] || fail "freeze not idempotent"
  printf '\nTAMPERED\n' >> "$repo/.oms/executors/exec1/SOUL.md"
  "$tool" brief --repo "$repo" --id exec1 >/dev/null 2>"$repo/tamper.err" || rc=$?
  [ "$rc" = 2 ] || fail "tampered executor should exit 2"
  contains "$repo/tamper.err" 'soul hash mismatch'
  rc=0
  "$tool" create --repo "$repo" --id bad --provider codex --strategy implementation-worker \
    --allowed '../escape' --soul-file "$repo/proposal.md" >/dev/null 2>"$repo/path.err" || rc=$?
  [ "$rc" = 2 ] || fail "unsafe scope path should fail"
  contains "$repo/path.err" 'unsafe scope path'

  "$ROOT/scripts/agent-plan.sh" --repo "$repo" init --goal planned >/dev/null
  "$ROOT/scripts/agent-plan.sh" --repo "$repo" add --id p1 --title planned \
    --allowed src/ --verify true --role test-designer >/dev/null
  "$ROOT/scripts/agent-plan.sh" --repo "$repo" claim --id p1 --provider codex >/dev/null
  "$tool" create --repo "$repo" --id planned1 --provider codex --plan-task p1 \
    --soul-file "$repo/proposal.md" >/dev/null
  "$tool" freeze --repo "$repo" --id planned1 >/dev/null
  "$tool" show --repo "$repo" --id planned1 | python3 -c \
    'import json,sys;d=json.load(sys.stdin); assert d["strategy"]=="test-designer" and d["lease_id"].startswith("lease_") and d["allowed_paths"]==["src/"]' ||
    fail "executor should hydrate plan strategy, lease, and scope"

  "$ROOT/scripts/agent-plan.sh" --repo "$repo" add --id unclaimed --title unclaimed \
    --allowed src/ --verify true >/dev/null
  rc=0
  "$tool" create --repo "$repo" --id unclaimed-executor --provider codex \
    --plan-task unclaimed --soul-file "$repo/proposal.md" >/dev/null 2>"$repo/unclaimed.err" || rc=$?
  [ "$rc" = 2 ] || fail "plan executor should require an active claim"
  contains "$repo/unclaimed.err" 'must be claimed'

  "$ROOT/scripts/agent-plan.sh" --repo "$repo" add --id wrong-provider --title wrong-provider \
    --allowed src/ --verify true >/dev/null
  "$ROOT/scripts/agent-plan.sh" --repo "$repo" claim --id wrong-provider --provider claude >/dev/null
  rc=0
  "$tool" create --repo "$repo" --id wrong-provider-executor --provider codex \
    --plan-task wrong-provider --soul-file "$repo/proposal.md" >/dev/null 2>"$repo/provider.err" || rc=$?
  [ "$rc" = 2 ] || fail "plan executor should match the claim provider"
  contains "$repo/provider.err" 'claim provider is claude'

  "$tool" --help > "$repo/executor-help"
  if grep -Eq -- '(^|[[:space:]])--mode([[:space:]=]|$)' "$repo/executor-help"; then
    fail "executor help should not advertise a mode option"
  fi
  rc=0
  "$tool" create --repo "$repo" --id read1 --provider codex --mode read \
    --strategy repo-auditor --soul-file "$repo/proposal.md" >/dev/null 2>"$repo/read-create.err" || rc=$?
  [ "$rc" = 2 ] || fail "read executor creation should be removed"
  contains "$repo/read-create.err" 'read executors were removed'
  [ ! -e "$repo/.oms/executors/read1" ] || fail "removed read executor should not create state"

  # The former default mode remains accepted as an unadvertised compatibility
  # no-op for existing automation, while all newly written metadata is fixed.
  "$tool" create --repo "$repo" --id compat-mode --provider codex --mode worktree-write \
    --strategy implementation-worker --soul-file "$repo/proposal.md" >/dev/null
  "$tool" show --repo "$repo" --id compat-mode | grep -Fq '"mode": "worktree-write"' ||
    fail "legacy worktree-write option changed metadata"

  # Existing read metadata remains inspectable and retireable, but cannot be
  # validated or executed through the write-only executor surface.
  "$tool" freeze --repo "$repo" --id compat-mode >/dev/null
  python3 - "$repo/.oms/executors/compat-mode/meta.json" <<'PY'
import json, sys
p=sys.argv[1]; d=json.load(open(p)); d["mode"]="read"; json.dump(d, open(p,"w"), indent=2)
PY
  "$tool" show --repo "$repo" --id compat-mode >/dev/null
  rc=0
  "$tool" validate --repo "$repo" --id compat-mode >/dev/null 2>"$repo/legacy-read.err" || rc=$?
  [ "$rc" = 2 ] || fail "legacy read executor should not validate"
  contains "$repo/legacy-read.err" 'legacy read executor is unsupported'
  rc=0
  OH_MY_SETTING_DELEGATE_DRY_RUN=1 "$ROOT/scripts/peer-delegate.sh" --repo "$repo" \
    --to codex --executor compat-mode --prompt 'Do not write' >/dev/null 2>"$repo/read-delegate.err" || rc=$?
  [ "$rc" = 2 ] || fail "read executor should be rejected by write delegation"
  contains "$repo/read-delegate.err" 'legacy read executor is unsupported'
  "$tool" fail --repo "$repo" --id compat-mode --reason retired >/dev/null
  "$tool" show --repo "$repo" --id compat-mode | grep -Fq '"state": "failed"' ||
    fail "legacy read executor should remain retireable"

  create_executor "$repo" base1 EXECUTOR-BASE-MARKER
  printf 'next\n' >> "$repo/file.txt"
  git -C "$repo" commit -qam next
  rc=0
  "$tool" brief --repo "$repo" --id base1 >/dev/null 2>"$repo/base.err" || rc=$?
  [ "$rc" = 2 ] || fail "executor should reject a moved base"
  contains "$repo/base.err" 'base sha mismatch'
}

test_repair_and_run() {
  local repo="$TMP/repair" bin="$TMP/bin" capture="$TMP/capture" home="$TMP/home"
  local artifact rc=0
  make_repo "$repo"; mkdir -p "$bin" "$capture" "$home"
  create_executor "$repo" repair1 EXECUTOR-REPAIR-MARKER
  cat > "$bin/codex" <<'EOF'
#!/usr/bin/env bash
prompt="$(cat)"; count=0
[ ! -f "$CAPTURE_DIR/count" ] || count="$(cat "$CAPTURE_DIR/count")"
count=$((count + 1)); printf '%s\n' "$count" > "$CAPTURE_DIR/count"
printf '%s\n' "$prompt" > "$CAPTURE_DIR/$count.prompt"
if [ "$count" -eq 1 ]; then printf 'broken\n' > delegated.txt; else printf 'fixed\n' > delegated.txt; fi
EOF
  chmod +x "$bin/codex"
  CAPTURE_DIR="$capture" HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
    "$ROOT/scripts/peer-delegate.sh" --to codex --repo "$repo" --executor repair1 \
      --artifact-dir "$repo/artifacts" --repair 1 --verify 'grep -q fixed delegated.txt' \
      --prompt 'Create fixed file' >"$repo/out" 2>&1 || rc=$?
  [ "$rc" = 0 ] || fail "executor repair failed: $rc"
  [ "$(cat "$capture/count")" = 2 ] || fail "repair did not invoke twice"
  contains "$capture/1.prompt" 'EXECUTOR-REPAIR-MARKER'; contains "$capture/2.prompt" 'EXECUTOR-REPAIR-MARKER'
  contains "$capture/1.prompt" 'executor_id: repair1'; contains "$capture/2.prompt" 'executor_id: repair1'
  artifact="$(find "$repo/artifacts" -type f -name 'codex-*.md' | head -n 1)"
  contains "$artifact" '## Repair 1'
  contains "$repo/.oms/artifacts/index.jsonl" '"executor_id": "repair1"'
  contains "$repo/.oms/artifacts/index.jsonl" '"soul_sha256"'
  "$ROOT/scripts/artifact-index.sh" --repo "$repo" validate >/dev/null ||
    fail "executor artifact lineage should validate"
  "$ROOT/scripts/agent-executor.sh" show --repo "$repo" --id repair1 | grep -Fq '"state": "done"' || fail "executor not done"

  create_executor "$repo" run1 EXECUTOR-RUN-MARKER
  OH_MY_SETTING_AGENT_RUN_DRY_RUN=1 "$ROOT/scripts/agent-run.sh" --repo "$repo" \
    --to codex --mode write --executor run1 --artifact-dir "$repo/run-artifacts" \
    --prompt 'Implement bounded work' >/dev/null
  artifact="$(find "$repo/run-artifacts" -type f -name 'codex-*.md' | head -n 1)"
  contains "$artifact" 'EXECUTOR-RUN-MARKER'; contains "$artifact" 'executor_id: run1'
  "$ROOT/scripts/agent-executor.sh" show --repo "$repo" --id run1 | grep -Fq '"state": "frozen"' || fail "dry-run changed state"
  "$ROOT/scripts/repo-state.sh" --repo "$repo" --json | python3 -c \
    'import json,sys;d=json.load(sys.stdin); assert any(x["id"]=="run1" and x["state"]=="frozen" for x in d["executors"])' ||
    fail "repo state should surface frozen executors"
  rc=0
  "$ROOT/scripts/agent-run.sh" --repo "$repo" --to codex --mode read --executor run1 \
    --prompt 'Assess work' >/dev/null 2>"$repo/read.err" || rc=$?
  [ "$rc" = 2 ] || fail "read executor should fail"
  contains "$repo/read.err" 'executors require write mode'
}

test_scope() {
  local repo="$TMP/scope" plan="$ROOT/scripts/agent-plan.sh" rc=0
  make_repo "$repo"
  mkdir -p "$repo/src/private"
  printf 'private\n' > "$repo/src/private/secret.txt"
  git -C "$repo" add src/private/secret.txt
  git -C "$repo" commit -qm private
  "$plan" --repo "$repo" init --goal scope >/dev/null
  "$plan" --repo "$repo" add --id t1 --title scope --allowed 'src/' --forbidden 'src/private/' --verify true >/dev/null
  git -C "$repo" checkout -qb scoped; mkdir -p "$repo/src"; printf 'ok\n' > "$repo/src/ok.txt"
  git -C "$repo" add src/ok.txt; git -C "$repo" commit -qm scoped
  git -C "$repo" diff main scoped > "$repo/allowed.patch"; git -C "$repo" checkout -q main; git -C "$repo" branch -qD scoped
  "$ROOT/scripts/patch-admit.sh" --repo "$repo" --patch "$repo/allowed.patch" --plan-task t1 \
    --report "$repo/allowed-report.md" --verify true >/dev/null || fail "allowed patch rejected"
  contains "$repo/allowed-report.md" 'scope: PASS'
  git -C "$repo" checkout -qb escaped; mkdir -p "$repo/src2"
  printf 'deny\n' >> "$repo/src/private/secret.txt"; printf 'outside\n' > "$repo/src2/escape.txt"
  git -C "$repo" add src/private/secret.txt src2/escape.txt; git -C "$repo" commit -qm escaped
  git -C "$repo" diff main escaped > "$repo/escaped.patch"; git -C "$repo" checkout -q main; git -C "$repo" branch -qD escaped
  "$ROOT/scripts/patch-admit.sh" --repo "$repo" --patch "$repo/escaped.patch" --plan-task t1 \
    --report "$repo/escaped-report.md" --verify true >/dev/null 2>&1 || rc=$?
  [ "$rc" = 1 ] || fail "escaped patch should reject"
  contains "$repo/escaped-report.md" 'scope: FAIL'
  contains "$repo/escaped-report.md" 'src/private/secret.txt'
  contains "$repo/escaped-report.md" 'src2/escape.txt'

  git -C "$repo" checkout -qb renamed
  mkdir -p "$repo/src/public"
  git -C "$repo" mv src/private/secret.txt src/public/secret.txt
  git -C "$repo" commit -qm renamed
  git -C "$repo" diff -M main renamed > "$repo/rename.patch"
  git -C "$repo" checkout -q main
  git -C "$repo" branch -qD renamed
  rc=0
  "$ROOT/scripts/patch-admit.sh" --repo "$repo" --patch "$repo/rename.patch" --plan-task t1 \
    --report "$repo/rename-report.md" --verify true >/dev/null 2>&1 || rc=$?
  [ "$rc" = 1 ] || fail "rename from forbidden source should reject"
  contains "$repo/rename-report.md" 'src/private/secret.txt'

  "$plan" --repo "$repo" add --id lease1 --title lease --allowed src/ \
    --verify true --role implementation-worker >/dev/null
  "$plan" --repo "$repo" claim --id lease1 --provider codex >/dev/null
  printf '# Specialization\n\nEXECUTOR-LEASE-MARKER\n' > "$repo/lease.proposal.md"
  "$ROOT/scripts/agent-executor.sh" create --repo "$repo" --id leaseexec --provider codex \
    --plan-task lease1 --soul-file "$repo/lease.proposal.md" >/dev/null
  "$ROOT/scripts/agent-executor.sh" freeze --repo "$repo" --id leaseexec >/dev/null
  "$plan" --repo "$repo" release --id lease1 >/dev/null
  "$plan" --repo "$repo" claim --id lease1 --provider claude >/dev/null
  rc=0
  "$ROOT/scripts/patch-admit.sh" --repo "$repo" --patch "$repo/allowed.patch" \
    --plan-task lease1 --executor leaseexec --verify true >/dev/null 2>"$repo/lease.err" || rc=$?
  [ "$rc" = 2 ] || fail "stale executor lease should fail admission"
  contains "$repo/lease.err" 'executor task lease mismatch'

  "$plan" --repo "$repo" claim --id t1 --provider codex >/dev/null
  "$plan" --repo "$repo" review --id t1 --patch "$repo/escaped.patch" >/dev/null
  rc=0; "$ROOT/scripts/patch-land.sh" --repo "$repo" --plan-task t1 --verify true >/dev/null 2>"$repo/land.err" || rc=$?
  [ "$rc" = 1 ] || fail "land should reject scope escape"; [ ! -e "$repo/src2/escape.txt" ] || fail "escape applied"
  "$plan" --repo "$repo" show --id t1 | grep -Fq '"state": "review"' || fail "review state changed"
}

test_assets() {
  local sums hook_root="$TMP/hook-root"
  [ ! -e "$ROOT/custom-skills/.gitkeep" ] || fail "custom-skills gitkeep remains"
  [ ! -e "$ROOT/templates/AGENTS.md" ] || fail "legacy template remains"
  [ ! -e "$ROOT/prompts/decision-context.md" ] || fail "merged prompt remains"
  [ -f "$ROOT/prompts/executor-soul.md" ] || fail "executor prompt missing"
  [ ! -e "$ROOT/workflows" ] || fail "deprecated workflows directory remains"
  [ ! -e "$ROOT/scripts/multi-agent-ask.sh" ] || fail "multi-agent ask shim remains"
  [ ! -e "$ROOT/scripts/multi-agent-review.sh" ] || fail "multi-agent review shim remains"
  [ ! -e "$ROOT/scripts/multi-agent-delegate.sh" ] || fail "multi-agent delegate shim remains"
  [ -f "$ROOT/plugins/oh-my-setting/scripts/harness-hook.sh" ] || fail "shared hook missing"
  [ ! -e "$ROOT/plugins/oh-my-setting/scripts/skill-router-hook.sh" ] || fail "skill hook duplicate remains"
  [ ! -e "$ROOT/plugins/oh-my-setting/scripts/turn-guard-hook.sh" ] || fail "guard hook duplicate remains"
  mkdir -p "$hook_root/scripts"
  printf '#!/usr/bin/env bash\ncat > "$HOOK_CAPTURE"\n' > "$hook_root/scripts/skill-router.sh"
  printf '#!/usr/bin/env bash\ncat > "$HOOK_CAPTURE"\n' > "$hook_root/scripts/turn-guard.sh"
  chmod +x "$hook_root/scripts/skill-router.sh" "$hook_root/scripts/turn-guard.sh"
  printf 'router-payload' | HOOK_CAPTURE="$TMP/router.capture" OH_MY_SETTING_DIR="$hook_root" \
    "$ROOT/plugins/oh-my-setting/scripts/harness-hook.sh" skill-router
  printf 'guard-payload' | HOOK_CAPTURE="$TMP/guard.capture" OH_MY_SETTING_DIR="$hook_root" \
    "$ROOT/plugins/oh-my-setting/scripts/harness-hook.sh" turn-guard
  contains "$TMP/router.capture" 'router-payload'
  contains "$TMP/guard.capture" 'guard-payload'
  sums="$("$ROOT/scripts/gen-checksums.sh")"
  printf '%s\n' "$sums" | grep -Fq scripts/agent-executor.sh || fail "executor not checksummed"
  printf '%s\n' "$sums" | grep -Fq prompts/executor-soul.md || fail "executor prompt not checksummed"
  if printf '%s\n' "$sums" | grep -Fq prompts/decision-context.md; then
    fail "removed prompt checksummed"
  fi
}

test_executor_gc() {
  local repo="$TMP/executor-gc" tool="$ROOT/scripts/agent-executor.sh"
  make_repo "$repo"
  create_executor "$repo" old1 EXECUTOR-OLD-MARKER
  "$tool" start --repo "$repo" --id old1 >/dev/null
  "$tool" "done" --repo "$repo" --id old1 >/dev/null
  create_executor "$repo" keep1 EXECUTOR-KEEP-MARKER
  touch -t 202001010000 "$repo/.oms/executors/old1/meta.json" "$repo/.oms/executors/keep1/meta.json"
  "$tool" gc --repo "$repo" --days 1 --dry-run >/dev/null
  [ -d "$repo/.oms/executors/old1" ] || fail "executor gc dry-run removed state"
  "$tool" gc --repo "$repo" --days 1 --apply >/dev/null
  [ ! -e "$repo/.oms/executors/old1" ] || fail "terminal executor was not collected"
  [ -d "$repo/.oms/executors/keep1" ] || fail "frozen executor must be retained"

  create_executor "$repo" orphan1 EXECUTOR-ORPHAN-MARKER
  "$tool" start --repo "$repo" --id orphan1 >/dev/null
  mkdir -p "$repo/.oms/delegations"
  printf '%s\n' '{"schema":2,"id":"dead","provider":"codex","pid":999999,"executor_id":"orphan1","task_id":"","lease_id":""}' \
    > "$repo/.oms/delegations/dead.json"
  "$ROOT/scripts/gc.sh" --repo "$repo" --days 30 --apply >/dev/null
  [ ! -e "$repo/.oms/delegations/dead.json" ] || fail "orphan marker was not collected"
  "$tool" show --repo "$repo" --id orphan1 | grep -Fq '"state": "failed"' ||
    fail "dead delegation should fail its running executor"
}

test_lifecycle; test_repair_and_run; test_scope; test_assets; test_executor_gc
echo "executor-smoke: ok"
