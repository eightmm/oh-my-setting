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
# A worker-style session id fires SessionEnd without ever writing a
# transcript. That capture can never succeed: the hook must skip quietly
# (exit 0, no digest, no ledger row) instead of minting a failure row per
# firing whose "run manually" hint fails the same way. A manual capture of a
# missing session stays a hard error, and a session that EXISTS but carries
# secrets still goes to the ledger through the hook — absence is the only
# thing the policy softens.
test_absent_session_skips_hook_capture_but_fails_manual() {
  local repo="$TMP/handoff-absent"
  local claude_home="$TMP/handoff-absent-home"
  local err="$TMP/handoff-absent.err"
  local vector

  mkdir -p "$repo/.oms" "$claude_home/projects"
  printf '*\n' > "$repo/.oms/.gitignore"

  # Hook path: session id with no transcript anywhere -> quiet skip.
  fire_handoff_hook "$repo" "$claude_home" ghost-worker-1 SessionEnd ||
    fail "the hook must exit 0 for an absent session transcript"
  [ "$(digest_count "$repo")" = 0 ] ||
    fail "an absent session must not produce a digest"
  [ ! -f "$repo/.oms/failures.jsonl" ] ||
    fail "an absent session must not mint a fail-ledger row"

  # Manual path: same absence stays a hard failure naming the session.
  if OMS_CLAUDE_HOME="$claude_home" "$ROOT/scripts/session-handoff.sh" capture \
      --agent claude --cwd "$repo" --session ghost-worker-1 \
      >/dev/null 2>"$err"; then
    fail "a manual capture of a missing session must fail"
  fi
  assert_contains "$err" "claude session not found"

  # Real failures keep the fail-open contract: a sensitive transcript through
  # the hook still records a ledger row (policy touches absence only).
  local project_dir
  project_dir="$claude_home/projects/$(printf '%s' "$repo" | sed 's#/#-#g')"
  mkdir -p "$project_dir"
  vector="AK""IAIOSFODNN7EXAMPLE"
  cat > "$project_dir/sess-secret.jsonl" <<EOF
{"type":"user","cwd":"$repo","message":{"role":"user","content":"deploy with $vector"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Deployed."}]}}
{"type":"user","cwd":"$repo","message":{"role":"user","content":"now roll it back"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Rolled back."}]}}
EOF
  fire_handoff_hook "$repo" "$claude_home" sess-secret SessionEnd ||
    fail "the hook stays exit 0 on a refused capture"
  [ -f "$repo/.oms/failures.jsonl" ] ||
    fail "a refused sensitive capture must still reach the fail-ledger"
  grep -q "session-handoff capture" "$repo/.oms/failures.jsonl" ||
    fail "the ledger row must name the capture"
}

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
  local tool_lock="$TMP/tool-lock.json"
  local fixtures="$TMP/tool-fixtures"
  local npm_prefix="$home/npm-prefix"
  local npm_root="$npm_prefix/lib/node_modules"
  mkdir -p "$bin" "$home/.local/bin" "$fixtures" "$npm_root" "$npm_prefix/bin"
  for cmd in node claude codex agy uv uvx gh; do
    cat > "$bin/$cmd" <<EOF
#!/usr/bin/env bash
if [ "$cmd" = node ] && [ "\${1:-}" = -p ]; then
  echo 24
elif [ "$cmd" = node ] && [ "\${1:-}" = --version ]; then
  echo v24.18.0
elif [ "$cmd" = uv ] || [ "$cmd" = uvx ]; then
  echo 'uv 0.12.3'
elif [ "$cmd" = agy ]; then
  echo 'agy 1.1.11'
elif [ "$cmd" = gh ]; then
  echo 'gh version 2.97.0'
else
  echo '$cmd 1.0'
fi
EOF
    chmod +x "$bin/$cmd"
  done

  # Build tiny npm wrapper/native packages and bind their exact bytes into a
  # copied lock. This exercises the full offline install without a registry or
  # trusting the host npm cache.
  python3 - "$ROOT/tools.lock.json" "$tool_lock" "$fixtures" <<'PY'
import base64
import hashlib
import io
import json
import re
import sys
import tarfile
from pathlib import Path

source, target, fixture_root = map(Path, sys.argv[1:])
row = json.loads(source.read_text(encoding="utf-8"))
fixture_root.mkdir(parents=True, exist_ok=True)
mapping = {}


def package(spec, name, version, install_script=False):
    stem = re.sub(r"[^A-Za-z0-9_.-]+", "-", spec).strip("-")
    path = fixture_root / (stem + ".tgz")
    manifest = json.dumps({"name": name, "version": version}).encode()
    with tarfile.open(path, "w:gz") as archive:
        directory = tarfile.TarInfo("package")
        directory.type = tarfile.DIRTYPE
        directory.mode = 0o755
        archive.addfile(directory)
        item = tarfile.TarInfo("package/package.json")
        item.size = len(manifest)
        archive.addfile(item, io.BytesIO(manifest))
        if install_script:
            script = b"// fixture install script\n"
            item = tarfile.TarInfo("package/install.cjs")
            item.size = len(script)
            archive.addfile(item, io.BytesIO(script))
    integrity = "sha512-" + base64.b64encode(
        hashlib.sha512(path.read_bytes()).digest()
    ).decode()
    mapping[spec] = {"filename": path.name, "integrity": integrity}
    return integrity


for key, value in row["npm"].items():
    spec = "%s@%s" % (value["package"], value["version"])
    value["integrity"] = package(
        spec, value["package"], value["version"], key in ("claude", "ntn")
    )
    for native in value.get("native", {}).values():
        native_spec = "%s@%s" % (native["package"], native["version"])
        if native_spec not in mapping:
            native["integrity"] = package(
                native_spec, native["package"], native["version"]
            )
        else:
            native["integrity"] = mapping[native_spec]["integrity"]

target.write_text(json.dumps(row), encoding="utf-8")
(fixture_root / "mapping.json").write_text(json.dumps(mapping), encoding="utf-8")
PY

  cat > "$bin/npm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$OMS_TEST_NPM_LOG"

case "${1:-}" in
  config)
    [ "${2:-} ${3:-}" = "get prefix" ] && printf '%s\n' "$OMS_TEST_NPM_PREFIX"
    exit 0
    ;;
  prefix)
    printf '%s\n' "$OMS_TEST_NPM_PREFIX"
    exit 0
    ;;
  root)
    printf '%s\n' "$OMS_TEST_NPM_ROOT"
    exit 0
    ;;
  list)
    package=""
    for arg in "$@"; do package="$arg"; done
    python3 - "$OMS_TEST_NPM_ROOT/$package/package.json" "$package" <<'PY'
import json, sys
try:
    version = json.load(open(sys.argv[1], encoding="utf-8"))["version"]
except (OSError, KeyError, json.JSONDecodeError):
    print('{"dependencies": {}}')
else:
    print(json.dumps({"dependencies": {sys.argv[2]: {"version": version}}}))
PY
    exit 0
    ;;
  pack)
    spec="$2"
    destination=""
    shift 2
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --pack-destination ]; then
        destination="$2"
        shift
      fi
      shift
    done
    python3 - "$OMS_TEST_NPM_FIXTURES" "$spec" "$destination" <<'PY'
import json, shutil, sys
from pathlib import Path
root, spec, destination = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
row = json.loads((root / "mapping.json").read_text(encoding="utf-8"))[spec]
shutil.copy2(root / row["filename"], destination / row["filename"])
print(json.dumps([row]))
PY
    exit 0
    ;;
  install)
    package_file=""
    for arg in "$@"; do package_file="$arg"; done
    identity="$(python3 - "$package_file" <<'PY'
import json, sys, tarfile
with tarfile.open(sys.argv[1], "r:gz") as archive:
    manifest = json.load(archive.extractfile("package/package.json"))
print(manifest["name"])
print(manifest["version"])
PY
)"
    package="$(printf '%s\n' "$identity" | sed -n '1p')"
    version="$(printf '%s\n' "$identity" | sed -n '2p')"
    python3 "$OMS_TEST_TOOL_LOCK_HELPER" --lock "$OH_MY_SETTING_TOOL_LOCK" \
      install-npm-payload --file "$package_file" \
      --dest "$OMS_TEST_NPM_ROOT/$package" --root "$OMS_TEST_NPM_ROOT" \
      --name "$package" --version "$version" >/dev/null
    case "$package" in
      @anthropic-ai/claude-code) binary=claude ;;
      @openai/codex) binary=codex ;;
      ntn) binary=ntn ;;
      *) exit 2 ;;
    esac
    mkdir -p "$OMS_TEST_NPM_PREFIX/bin"
    printf '#!/usr/bin/env bash\necho "%s %s"\n' "$binary" "$version" \
      > "$OMS_TEST_NPM_PREFIX/bin/$binary"
    chmod +x "$OMS_TEST_NPM_PREFIX/bin/$binary"
    exit 0
    ;;
esac
exit 2
EOF
  chmod +x "$bin/npm"
  OMS_TEST_NPM_LOG="$TMP/npm.log" OMS_TEST_NPM_PREFIX="$npm_prefix" \
    OMS_TEST_NPM_ROOT="$npm_root" OMS_TEST_NPM_FIXTURES="$fixtures" HOME="$home" \
    OMS_TEST_TOOL_LOCK_HELPER="$ROOT/scripts/lib/tool-lock.py" \
    OH_MY_SETTING_TOOL_LOCK="$tool_lock" \
    NVM_DIR="$home/.nvm" OH_MY_SETTING_UPGRADE_ANTIGRAVITY=0 \
    PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/install-tools.sh" --upgrade >/dev/null
  assert_contains "$TMP/npm.log" "pack @anthropic-ai/claude-code@2.1.226"
  assert_contains "$TMP/npm.log" "pack @openai/codex@0.147.0"
  assert_contains "$TMP/npm.log" "--cache"
  assert_contains "$TMP/npm.log" "--offline"
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
  cp "$ROOT/scripts/lib/install-lifecycle-lock.sh" "$source/scripts/lib/install-lifecycle-lock.sh"
  cp "$ROOT/scripts/lib/file-lock.sh" "$source/scripts/lib/file-lock.sh"
  cp "$ROOT/scripts/lib/poll.sh" "$source/scripts/lib/poll.sh"
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
test_absent_session_skips_hook_capture_but_fails_manual
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
