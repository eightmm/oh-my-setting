#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-lifecycle-hardening.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
export XDG_CACHE_HOME="$TMP/cache"
mkdir -p "$XDG_CACHE_HOME"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_contains() {
  grep -Fq -- "$2" "$1" || fail "$1 does not contain: $2"
}

# An adopted repo plus a Claude transcript the handoff extractor can read. Two
# user turns, since capture floors trivial sessions at --min-user-turns.
make_handoff_fixture() {
  local repo="$1"
  local claude_home="$2"
  local session="$3"
  local project_dir

  mkdir -p "$repo/.oms"
  printf '*\n' > "$repo/.oms/.gitignore"
  project_dir="$claude_home/projects/$(printf '%s' "$repo" | sed 's#/#-#g')"
  mkdir -p "$project_dir"
  cat > "$project_dir/$session.jsonl" <<EOF
{"type":"user","cwd":"$repo","message":{"role":"user","content":"build the widget"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Widget built and tested."}]}}
{"type":"user","cwd":"$repo","message":{"role":"user","content":"now wire it to the queue"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Wired and verified."}]}}
EOF
}

# One user prompt and nothing else: the session the floor exists to skip.
make_trivial_handoff_fixture() {
  local repo="$1"
  local claude_home="$2"
  local session="$3"
  local project_dir

  mkdir -p "$repo/.oms"
  printf '*\n' > "$repo/.oms/.gitignore"
  project_dir="$claude_home/projects/$(printf '%s' "$repo" | sed 's#/#-#g')"
  mkdir -p "$project_dir"
  cat > "$project_dir/$session.jsonl" <<EOF
{"type":"user","cwd":"$repo","message":{"role":"user","content":"what does this repo do"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"It is a harness."}]}}
EOF
}

# Trailing arguments are extra environment assignments for the hook process.
fire_handoff_hook() {
  local repo="$1"
  local claude_home="$2"
  local session="$3"
  local event="$4"
  shift 4

  printf '{"session_id":"%s","cwd":"%s","hook_event_name":"%s"}' \
    "$session" "$repo" "$event" |
    env OMS_CLAUDE_HOME="$claude_home" "$@" "$ROOT/scripts/precompact-handoff.sh"
}

digest_count() {
  find "$1/.oms/handoffs" -name '*.md' 2>/dev/null | wc -l | tr -d ' '
}

fresh_digest_count() {
  find "$1/.oms/handoffs" -name '*.md' -mmin -5 2>/dev/null | wc -l | tr -d ' '
}

# The digest filename carries a whole-second timestamp, so two captures inside
# the same second land on one name and a file count cannot tell them apart.
# Fractional mtimes can.
digest_state() {
  python3 - "$1/.oms/handoffs" <<'PY'
import os, sys
directory = sys.argv[1]
if os.path.isdir(directory):
    for name in sorted(os.listdir(directory)):
        print(name, os.path.getmtime(os.path.join(directory, name)))
PY
}

# touch -d takes no relative time on BSD, so age the fixtures from Python.
age_digests() {
  python3 - "$1/.oms/handoffs" <<'PY'
import os, sys, time
old = time.time() - 3600
for name in os.listdir(sys.argv[1]):
    path = os.path.join(sys.argv[1], name)
    os.utime(path, (old, old))
PY
}

# A session that ends below the pressure bands and without ever compacting is
# the common case, and it used to leave the next session no continuity pointer
# at all.
test_session_end_captures_a_handoff_digest() {
  local repo="$TMP/sessionend-repo"
  local claude_home="$TMP/sessionend-home"
  local before

  make_handoff_fixture "$repo" "$claude_home" sess-end
  fire_handoff_hook "$repo" "$claude_home" sess-end SessionEnd ||
    fail "session-end handoff hook must exit 0"
  [ "$(digest_count "$repo")" = 1 ] ||
    fail "session end should capture a handoff digest"
  grep -Rlq "auto: session end" "$repo/.oms/handoffs" ||
    fail "the digest should carry the session-end note derived from the event"

  # Dedupe is by recency, not existence: a capture from moments ago (pressure
  # path, or compact-then-exit) makes this run redundant...
  before="$(digest_state "$repo")"
  fire_handoff_hook "$repo" "$claude_home" sess-end SessionEnd ||
    fail "a deduped run must still exit 0"
  [ "$(digest_state "$repo")" = "$before" ] ||
    fail "session end re-captured inside the dedupe window"

  # ...but an aged digest is stale mid-session state, and the session-end
  # capture that supersedes it is exactly what the next session needs.
  age_digests "$repo"
  fire_handoff_hook "$repo" "$claude_home" sess-end SessionEnd ||
    fail "session-end handoff hook must exit 0 after the dedupe window"
  [ "$(fresh_digest_count "$repo")" -ge 1 ] ||
    fail "a stale digest must not suppress the session-end capture"
}

test_session_end_handoff_honors_child_and_opt_out_gates() {
  local repo="$TMP/sessionend-gates"
  local claude_home="$TMP/sessionend-gates-home"

  make_handoff_fixture "$repo" "$claude_home" sess-gate

  # Peer children (peer-common.sh) end their throwaway sessions in the adopted
  # repo; a digest from one would hijack the resume hook's newest pointer.
  fire_handoff_hook "$repo" "$claude_home" sess-gate SessionEnd \
    OMS_HARNESS_CHILD=1 || fail "the harness-child skip must exit 0"
  [ "$(digest_count "$repo")" = 0 ] ||
    fail "a harness child must not write a handoff digest"

  # The pre-existing kill switch covers the new trigger too.
  fire_handoff_hook "$repo" "$claude_home" sess-gate SessionEnd \
    OMS_PRECOMPACT_HANDOFF=0 || fail "the disabled hook must exit 0"
  [ "$(digest_count "$repo")" = 0 ] ||
    fail "OMS_PRECOMPACT_HANDOFF=0 must disable the session-end capture"

  fire_handoff_hook "$repo" "$claude_home" sess-gate SessionEnd \
    OMS_SESSIONEND_HANDOFF=0 || fail "the disabled session-end trigger must exit 0"
  [ "$(digest_count "$repo")" = 0 ] ||
    fail "OMS_SESSIONEND_HANDOFF=0 must disable the session-end capture"

  # That opt-out is trigger-scoped: compaction still captures.
  fire_handoff_hook "$repo" "$claude_home" sess-gate PreCompact \
    OMS_SESSIONEND_HANDOFF=0 || fail "pre-compact handoff hook must exit 0"
  [ "$(digest_count "$repo")" = 1 ] ||
    fail "the session-end opt-out must not disable the pre-compact capture"
  grep -Rlq "auto: pre-compact snapshot" "$repo/.oms/handoffs" ||
    fail "the pre-compact digest should keep its own note"
}

# The hook now fires on every session end, so the floor is what keeps a
# one-prompt session from evicting a substantive digest from the resume hook's
# newest-handoff slot.
test_handoff_capture_floors_trivial_sessions() {
  local repo="$TMP/handoff-floor"
  local claude_home="$TMP/handoff-floor-home"
  local out="$TMP/handoff-floor.out"

  make_trivial_handoff_fixture "$repo" "$claude_home" sess-thin

  # Automatic path: passes nothing, inherits the default floor.
  fire_handoff_hook "$repo" "$claude_home" sess-thin SessionEnd ||
    fail "the hook must exit 0 when the capture is skipped"
  [ "$(digest_count "$repo")" = 0 ] ||
    fail "a one-prompt session must not write a handoff digest"
  # A trivial session is expected noise, not a failure worth a ledger row.
  [ ! -f "$repo/.oms/failures.jsonl" ] ||
    fail "a skipped capture must not file a fail-ledger row"

  # Skipped, not failed, and the reason names the flag that overrides it.
  OMS_CLAUDE_HOME="$claude_home" "$ROOT/scripts/session-handoff.sh" capture \
    --agent claude --cwd "$repo" --session sess-thin \
    > "$TMP/handoff-floor.stdout" 2> "$out" ||
    fail "a skipped capture must exit 0"
  assert_contains "$out" "--min-user-turns"
  [ ! -s "$TMP/handoff-floor.stdout" ] ||
    fail "a skipped capture must not print a digest path"

  # An explicit manual capture can still take it.
  OMS_CLAUDE_HOME="$claude_home" "$ROOT/scripts/session-handoff.sh" capture \
    --agent claude --cwd "$repo" --session sess-thin --min-user-turns 0 \
    >/dev/null || fail "--min-user-turns 0 must capture a one-prompt session"
  [ "$(digest_count "$repo")" = 1 ] ||
    fail "--min-user-turns 0 should have written a digest"

  if OMS_CLAUDE_HOME="$claude_home" "$ROOT/scripts/session-handoff.sh" capture \
      --agent claude --cwd "$repo" --session sess-thin \
      --min-user-turns two >/dev/null 2>&1; then
    fail "--min-user-turns must reject a non-numeric count"
  fi

  # Ordering: the floor must not turn a secret-bearing transcript into a silent
  # skip. Split literal keeps this test source scrubber-clean.
  local project_dir
  project_dir="$claude_home/projects/$(printf '%s' "$repo" | sed 's#/#-#g')"
  printf '{"type":"user","cwd":"%s","message":{"role":"user","content":"use AK%s"}}\n' \
    "$repo" "IAIOSFODNN7EXAMPLE" > "$project_dir/sess-thin.jsonl"
  if OMS_CLAUDE_HOME="$claude_home" "$ROOT/scripts/session-handoff.sh" capture \
      --agent claude --cwd "$repo" --session sess-thin \
      --out "$TMP/handoff-floor-secret.md" >/dev/null 2>&1; then
    fail "a secret-bearing transcript must stay a refusal, floor or not"
  fi
  [ ! -f "$TMP/handoff-floor-secret.md" ] ||
    fail "a refused capture must not persist a digest"
}

test_claude_hooks_register_and_verify_session_end_handoff() {
  local home="$TMP/sessionend-hooks-home"
  local project="$TMP/sessionend-hooks-project"
  local settings="$TMP/sessionend-settings.json"
  local stripped="$TMP/sessionend-settings-stripped.json"
  local receipt="$TMP/sessionend-receipt.json"

  mkdir -p "$home" "$project"
  HOME="$home" OMS_INSTALL_RECEIPT="$receipt" \
    "$ROOT/scripts/install-claude-hooks.sh" --settings "$settings" >/dev/null
  python3 - "$settings" <<'PY' || fail "SessionEnd handoff hook is not registered"
import json, sys

hooks = json.load(open(sys.argv[1], encoding="utf-8"))["hooks"]
ours = [h for entry in hooks["SessionEnd"] for h in entry["hooks"]
        if "precompact-handoff.sh" in h["command"]]
assert len(ours) == 1, ours
assert ours[0]["timeout"] == 30, ours
PY

  # A registration nothing verifies is one settings edit away from vanishing,
  # so the doctor's checklist has to name it.
  HOME="$home" OMS_INSTALL_RECEIPT="$receipt" \
    bash -c '. "$1"; oms_install_write_receipt "$2" "$3"' _ \
      "$ROOT/scripts/lib/install-contract.sh" "$ROOT" "$receipt" >/dev/null
  python3 - "$settings" "$stripped" <<'PY'
import json, sys

settings = json.load(open(sys.argv[1], encoding="utf-8"))
settings["hooks"]["SessionEnd"] = [
    entry for entry in settings["hooks"]["SessionEnd"]
    if not any("precompact-handoff.sh" in h["command"] for h in entry["hooks"])
]
json.dump(settings, open(sys.argv[2], "w", encoding="utf-8"))
PY
  # Only the hook verdict is in scope, so the doctor's overall status (it
  # reports on subsystems this fixture does not install) is discarded.
  run_hook_doctor() {
    ( cd "$project" || exit 2
      HOME="$home" XDG_CONFIG_HOME="$home/.config" \
        OMS_INSTALL_RECEIPT="$receipt" OMS_CLAUDE_SETTINGS="$1" \
        OH_MY_SETTING_REQUIRE_TOOLS=0 OH_MY_SETTING_MODEL_DOCTOR=0 \
        OH_MY_SETTING_CODEX_PLUGIN=0 "$ROOT/scripts/doctor.sh" 2>&1 ) || true
  }
  run_hook_doctor "$stripped" > "$TMP/sessionend-doctor-missing"
  assert_contains "$TMP/sessionend-doctor-missing" \
    "claude hook missing: SessionEnd -> precompact-handoff.sh"
  run_hook_doctor "$settings" > "$TMP/sessionend-doctor-ok"
  assert_contains "$TMP/sessionend-doctor-ok" "ok: claude hooks registered"
}

test_receipt_preserves_snapshot_modes() {
  local repo="$TMP/receipt-repo"
  local receipt="$TMP/config/install.json"
  mkdir -p "$repo/plugins/oh-my-setting/.codex-plugin" "$(dirname "$receipt")"
  printf '0.4.0\n' > "$repo/VERSION"
  printf '{"version":"0.4.0"}\n' > "$repo/plugins/oh-my-setting/.codex-plugin/plugin.json"
  git -C "$repo" init -q
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name Test
  git -C "$repo" add .
  git -C "$repo" commit -qm fixture

  HOME="$TMP/home" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_GENERATE_MACHINE=auto OH_MY_SETTING_GENERATE_SLURM=auto \
    bash -c '. "$1"; oms_install_write_receipt "$2" "$3"' _ \
      "$ROOT/scripts/lib/install-contract.sh" "$repo" "$receipt"
  python3 - "$receipt" <<'PY' || fail "receipt lost snapshot mode"
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert d["component_modes"]["machine_snapshot"] == "auto"
assert d["component_modes"]["slurm_snapshot"] == "auto"
assert d["components"]["machine_snapshot"] is True
assert d["components"]["slurm_snapshot"] is True
PY
}

test_machine_snapshot_cli_and_permissions() {
  local out="$TMP/machine.md"
  "$ROOT/scripts/write-machine-snapshot.sh" --help >/dev/null
  OH_MY_SETTING_MACHINE_SNAPSHOT="$out" "$ROOT/scripts/write-machine-snapshot.sh" --dry-run > "$TMP/machine-dry"
  [ ! -e "$out" ] || fail "machine --dry-run wrote output"
  assert_contains "$TMP/machine-dry" "Schema: 1"
  OH_MY_SETTING_MACHINE_SNAPSHOT="$out" "$ROOT/scripts/write-machine-snapshot.sh" >/dev/null
  [ "$(stat -c '%a' "$out" 2>/dev/null || stat -f '%Lp' "$out")" = 600 ] ||
    fail "machine snapshot is not private"
  OH_MY_SETTING_MACHINE_SNAPSHOT="$out" "$ROOT/scripts/write-machine-snapshot.sh" --check >/dev/null
  printf 'broken\n' > "$out"
  if OH_MY_SETTING_MACHINE_SNAPSHOT="$out" "$ROOT/scripts/write-machine-snapshot.sh" --check >/dev/null 2>&1; then
    fail "machine snapshot check accepted corrupt content"
  fi
}

# The snapshot exists so an agent knows what machine it is on. Presence of the
# labels is not the contract — the content is. A portable rewrite once reduced
# OS to "Linux-6.8.0-x86_64-with-glibc2.39" (the kernel line again, distro
# gone) and CPU to "x86_64", which answers nothing, and every label-shaped
# assertion still passed. Fixtures are injected so this holds on any host.
test_machine_snapshot_names_the_distro_and_the_cpu_model() {
  local out="$TMP/machine-content.md"
  local fixtures="$TMP/machine-fixtures"

  mkdir -p "$fixtures"
  printf 'NAME="Ubuntu"\nPRETTY_NAME="Ubuntu 24.04.4 LTS"\nID=ubuntu\n' \
    > "$fixtures/os-release"
  printf 'processor\t: 0\nmodel name\t: Intel(R) Core(TM) i5-14600KF\n' \
    > "$fixtures/cpuinfo"

  OMS_OS_RELEASE="$fixtures/os-release" OMS_CPUINFO="$fixtures/cpuinfo" \
    OH_MY_SETTING_MACHINE_SNAPSHOT="$out" \
    "$ROOT/scripts/write-machine-snapshot.sh" --dry-run > "$TMP/machine-content"
  assert_contains "$TMP/machine-content" "- OS: Ubuntu 24.04.4 LTS"
  assert_contains "$TMP/machine-content" "- CPU: Intel(R) Core(TM) i5-14600KF"

  # With no distro or cpuinfo to read, the portable answer is the fallback and
  # must still produce a line rather than an empty field.
  OMS_OS_RELEASE="$fixtures/absent" OMS_CPUINFO="$fixtures/absent" \
    OH_MY_SETTING_MACHINE_SNAPSHOT="$out" \
    "$ROOT/scripts/write-machine-snapshot.sh" --dry-run > "$TMP/machine-fallback"
  grep -Eq '^- OS: .+' "$TMP/machine-fallback" ||
    fail "OS must fall back to a portable value"
  grep -Eq '^- CPU: .+' "$TMP/machine-fallback" ||
    fail "CPU must fall back to a portable value"
}

test_slurm_snapshot_cli_and_permissions() {
  local bin="$TMP/slurm-bin"
  local out="$TMP/cluster.md"
  mkdir -p "$bin"
  cat > "$bin/sinfo" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *%P*%a*) printf 'gpu*|up|1-00:00:00|1|gpu:a100:1|64000|8|node1\n' ;;
  *%P*%l*) printf 'gpu*|1-00:00:00|1|gpu:a100:1|8|node1\n' ;;
  *%P*) printf 'gpu*\n' ;;
esac
EOF
  chmod +x "$bin/sinfo"
  "$ROOT/scripts/generate-slurm-reference.sh" --help >/dev/null
  PATH="$bin:/usr/bin:/bin" OH_MY_SETTING_SLURM_REF="$out" \
    "$ROOT/scripts/generate-slurm-reference.sh" --dry-run > "$TMP/slurm-dry"
  [ ! -e "$out" ] || fail "slurm --dry-run wrote output"
  assert_contains "$TMP/slurm-dry" "Schema: 1"
  PATH="$bin:/usr/bin:/bin" OH_MY_SETTING_SLURM_REF="$out" \
    "$ROOT/scripts/generate-slurm-reference.sh" >/dev/null
  [ "$(stat -c '%a' "$out" 2>/dev/null || stat -f '%Lp' "$out")" = 600 ] ||
    fail "Slurm snapshot is not private"
  OH_MY_SETTING_SLURM_REF="$out" "$ROOT/scripts/generate-slurm-reference.sh" --check >/dev/null

  local default_root="$TMP/slurm-default-root"
  mkdir -p "$default_root/scripts"
  cp "$ROOT/scripts/generate-slurm-reference.sh" "$default_root/scripts/"
  chmod +x "$default_root/scripts/generate-slurm-reference.sh"
  PATH="$bin:/usr/bin:/bin" "$default_root/scripts/generate-slurm-reference.sh" >/dev/null
  [ -f "$default_root/local/slurm.md" ] ||
    fail "default Slurm snapshot should live under local/, outside the skill catalog"
}

test_project_doctor_strict_slurm_contract() {
  local project="$TMP/slurm-project"
  mkdir -p "$project"
  "$ROOT/scripts/apply-project-template.sh" slurm "$project" >/dev/null
  sed -i \
    -e 's/^- State: draft/- State: active/' \
    -e 's/^- Test:$/- Test: bash scripts\/check.sh/' \
    -e 's/^- Success criteria:$/- Success criteria: checks pass/' \
    "$project/PROJECT.md"
  "$ROOT/scripts/project-doctor.sh" "$project" > "$TMP/project-doctor"
  assert_contains "$TMP/project-doctor" "Slurm execution contract fields are empty"
  if "$ROOT/scripts/project-doctor.sh" --strict "$project" >/dev/null 2>&1; then
    fail "strict project doctor accepted empty Slurm contract"
  fi
}

test_tsp_fallback_requires_opt_in() {
  local project="$TMP/tsp-project"
  mkdir -p "$project"
  if (cd "$project" && OMS_TSP_FORCE_FALLBACK=1 PATH="/usr/bin:/bin" \
      "$ROOT/scripts/tsp-queue.sh" enqueue -- bash -c true >/dev/null 2>"$TMP/tsp-denied"); then
    fail "missing tsp started a job without opt-in"
  fi
  assert_contains "$TMP/tsp-denied" "--allow-noqueue"
  (cd "$project" && OMS_TSP_FORCE_FALLBACK=1 OMS_TSP_FALLBACK_DIR="$TMP/tsp-fallback" \
    PATH="/usr/bin:/bin" "$ROOT/scripts/tsp-queue.sh" enqueue --allow-noqueue -- bash -c true >/dev/null)
}

test_tool_upgrade_refreshes_existing_clis() {
  local bin="$TMP/tool-bin"
  local home="$TMP/tool-home"
  mkdir -p "$bin" "$home/.local/bin"
  for cmd in node claude codex agy uv gh; do
    cat > "$bin/$cmd" <<EOF
#!/usr/bin/env bash
if [ "$cmd" = node ] && [ "\${1:-}" = -p ]; then echo 22; else echo '$cmd 1.0'; fi
EOF
    chmod +x "$bin/$cmd"
  done
  cat > "$bin/npm" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$OMS_TEST_NPM_LOG"
[ "$1 $2 $3" = "config get prefix" ] && printf '%s\n' "$OMS_TEST_NPM_PREFIX"
exit 0
EOF
  cat > "$bin/curl" <<'EOF'
#!/usr/bin/env bash
output=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = -o ]; then
    output="$2"
    shift
  fi
  shift
done
if [ -n "$output" ]; then
  : > "$output"
else
  printf '{"tag_name":"v1.0.0"}\n'
fi
EOF
  cat > "$bin/tar" <<'EOF'
#!/usr/bin/env bash
dest=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = -C ]; then
    dest="$2"
    shift
  fi
  shift
done
mkdir -p "$dest/gh_1.0.0_linux_amd64/bin"
cat > "$dest/gh_1.0.0_linux_amd64/bin/gh" <<'GH'
#!/usr/bin/env bash
if [ "${1:-}" = auth ]; then exit 1; fi
echo 'gh version 1.0.0'
GH
chmod +x "$dest/gh_1.0.0_linux_amd64/bin/gh"
EOF
  chmod +x "$bin/npm" "$bin/curl" "$bin/tar"
  OMS_TEST_NPM_LOG="$TMP/npm.log" OMS_TEST_NPM_PREFIX="$home" HOME="$home" \
    NVM_DIR="$home/.nvm" OH_MY_SETTING_UPGRADE_ANTIGRAVITY=0 \
    PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/install-tools.sh" --upgrade >/dev/null
  assert_contains "$TMP/npm.log" "install -g @anthropic-ai/claude-code"
  assert_contains "$TMP/npm.log" "install -g @openai/codex"
}

test_branch_receipt_auto_update_uses_transaction() {
  local repo="$TMP/auto-repo"
  local home="$TMP/auto-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  mkdir -p "$repo/scripts" "$repo/local" "$(dirname "$receipt")"
  cp "$ROOT/scripts/auto-update.sh" "$repo/scripts/auto-update.sh"
  cat > "$repo/scripts/update.sh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" > "$OMS_TEST_UPDATE_MARKER"
commit="$(git rev-parse HEAD)"
echo "update-check: up_to_date $commit"
EOF
  chmod +x "$repo/scripts/auto-update.sh" "$repo/scripts/update.sh"
  git -C "$repo" init -q
  git -C "$repo" checkout -qb main
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name Test
  git -C "$repo" add .
  git -C "$repo" commit -qm fixture
  python3 - "$receipt" "$repo" <<'PY'
import json, sys
json.dump({"schema": 2, "source_root": sys.argv[2], "ref": "edge"}, open(sys.argv[1], "w"))
PY
  HOME="$home" OMS_INSTALL_RECEIPT="$receipt" OMS_TEST_UPDATE_MARKER="$TMP/update.marker" \
    "$repo/scripts/auto-update.sh" check >/dev/null
  assert_contains "$TMP/update.marker" "--check"
}

test_update_refreshes_snapshot_policy() {
  local source="$TMP/update-source"
  local remote="$TMP/update-remote.git"
  local installed="$TMP/update-installed"
  local home="$TMP/update-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  local first

  mkdir -p "$source/scripts/lib" "$(dirname "$receipt")"
  cp "$ROOT/scripts/update.sh" "$source/scripts/update.sh"
  cp "$ROOT/scripts/lib/install-contract.sh" "$source/scripts/lib/install-contract.sh"
  for script in install-claude-hooks install-codex-plugin install-autoupdate uninstall-autoupdate doctor install-mcp install-agy-plugin; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$source/scripts/$script.sh"
    chmod +x "$source/scripts/$script.sh"
  done
  cat > "$source/scripts/link.sh" <<'EOF'
#!/usr/bin/env bash
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ROOT/scripts/lib/install-contract.sh"
oms_install_write_receipt "$ROOT" "$(oms_install_receipt_path)" >/dev/null
EOF
  chmod +x "$source/scripts/link.sh"
  cat > "$source/scripts/write-machine-snapshot.sh" <<'EOF'
#!/usr/bin/env bash
printf 'machine\n' >> "$OMS_TEST_SNAPSHOT_LOG"
EOF
  cat > "$source/scripts/generate-slurm-reference.sh" <<'EOF'
#!/usr/bin/env bash
printf 'slurm\n' >> "$OMS_TEST_SNAPSHOT_LOG"
EOF
  chmod +x "$source/scripts/write-machine-snapshot.sh" "$source/scripts/generate-slurm-reference.sh"
  printf 'base\n' > "$source/README.md"
  git -C "$source" init -q
  git -C "$source" checkout -qb main
  git -C "$source" config user.email test@example.com
  git -C "$source" config user.name Test
  git -C "$source" add .
  git -C "$source" commit -qm base
  first="$(git -C "$source" rev-parse HEAD)"
  git clone -q --bare "$source" "$remote"
  git -C "$source" remote add origin "$remote"
  git clone -q "$remote" "$installed"
  printf 'next\n' >> "$source/README.md"
  git -C "$source" add README.md
  git -C "$source" commit -qm next
  git -C "$source" push -q origin main
  python3 - "$receipt" "$installed" "$first" <<'PY'
import json, sys
json.dump({
  "schema": 2, "source_root": sys.argv[2], "commit": sys.argv[3],
  "channel": "main", "dirty": False, "version": "0.4.0", "profile": "full",
  "ref": "edge", "previous_commit": "", "installed_at": "2026-07-15T00:00:00Z",
  "components": {"tools": False, "claude_hooks": False, "codex_plugin": False,
    "auto_update": False, "machine_snapshot": True, "slurm_snapshot": True},
  "component_modes": {"machine_snapshot": "auto", "slurm_snapshot": "auto"},
  "managed_targets": [],
  "plugin": {"name": "oh-my-setting", "version": "0.4.0", "sha256": "x" * 64}
}, open(sys.argv[1], "w", encoding="utf-8"))
PY
  HOME="$home" OMS_INSTALL_RECEIPT="$receipt" OMS_TEST_SNAPSHOT_LOG="$TMP/snapshot.log" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --no-tools --no-doctor >/dev/null
  assert_contains "$TMP/snapshot.log" "machine"
  if grep -Fq slurm "$TMP/snapshot.log"; then
    fail "Slurm auto snapshot ran without sinfo"
  fi
  : > "$TMP/snapshot.log"
  HOME="$home" OMS_INSTALL_RECEIPT="$receipt" OMS_TEST_SNAPSHOT_LOG="$TMP/snapshot.log" \
    OH_MY_SETTING_GENERATE_MACHINE=0 OH_MY_SETTING_GENERATE_SLURM=1 \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --no-tools --no-doctor >/dev/null
  assert_contains "$TMP/snapshot.log" "slurm"
  if grep -Fq machine "$TMP/snapshot.log"; then
    fail "explicit machine snapshot disable did not override receipt auto mode"
  fi
  [ "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["previous_commit"])' "$receipt")" = "$first" ] ||
    fail "no-op reconciliation replaced the last real rollback commit"
}

test_session_end_captures_a_handoff_digest
test_session_end_handoff_honors_child_and_opt_out_gates
test_handoff_capture_floors_trivial_sessions
test_claude_hooks_register_and_verify_session_end_handoff
test_receipt_preserves_snapshot_modes
test_machine_snapshot_cli_and_permissions
test_machine_snapshot_names_the_distro_and_the_cpu_model
test_slurm_snapshot_cli_and_permissions
test_project_doctor_strict_slurm_contract
test_tsp_fallback_requires_opt_in
test_tool_upgrade_refreshes_existing_clis
test_branch_receipt_auto_update_uses_transaction
test_update_refreshes_snapshot_policy
echo "lifecycle-hardening-smoke: ok"
