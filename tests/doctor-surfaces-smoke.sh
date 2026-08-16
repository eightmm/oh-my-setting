#!/usr/bin/env bash
set -euo pipefail

# Smoke tests for the hook-surface report: install-claude-hooks.sh
# --print-expected, `doctor.sh --surfaces` comparing source against a live
# settings.json and the harness event stream, the receipt's hooks_schema
# stamp, the displaced-user-config notice in the default doctor run, and the
# gate that forces a schema bump when the expected list changes.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-doctor-surfaces.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

# The expected surface list, recorded. Both literals move together or the gate
# below fails; see test_expected_surface_list_is_bump_gated.
EXPECTED_HOOKS_SCHEMA=1
EXPECTED_SURFACES_SHA256=d41822723d65c6e3d4bd8b8dd1e0a907ca0e4d83c1114bad4302690bf71280f0

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# A fixture HOME with a receipt this checkout owns, an empty harness repo for
# the event stream, and a settings.json built from --print-expected so the
# tests describe deviations from the real list rather than restating it.
#
# $2 is a python expression run over the surface rows (`rows`) to shape the
# settings file; $3, when given, is the receipt's hooks_schema.
make_fixture() {
  local home="$1"
  local shape="${2:-rows}"
  local schema="${3:-}"

  mkdir -p "$home/.claude" "$home/.config/oh-my-setting" "$home/repo/.oms/hooks"

  OMS_T_HOME="$home" OMS_T_ROOT="$ROOT" OMS_T_SCHEMA="$schema" python3 - "$shape" <<'PY'
import json, os, subprocess, sys

home = os.environ["OMS_T_HOME"]
root = os.environ["OMS_T_ROOT"]
expected = json.loads(subprocess.check_output(
    ["bash", os.path.join(root, "scripts", "install-claude-hooks.sh"),
     "--print-expected"],
    text=True,
))
rows = eval(sys.argv[1], {"rows": expected["surfaces"]})  # noqa: S307 - test fixture

hooks = {}
for row in rows:
    entry = {"hooks": [{"type": "command", "command": row["command"]}]}
    if row["timeout"] is not None:
        entry["hooks"][0]["timeout"] = row["timeout"]
    if row["matcher"] is not None:
        entry["matcher"] = row["matcher"]
    hooks.setdefault(row["event"], []).append(entry)
with open(os.path.join(home, ".claude", "settings.json"), "w", encoding="utf-8") as fh:
    json.dump({"hooks": hooks,
               "statusLine": {"type": "command", "command": "true"},
               "subagentStatusLine": {"type": "command", "command": "true"}}, fh, indent=2)

receipt = {
    "schema": 2,
    "source_root": root,
    "commit": "0" * 40,
    "channel": "main",
    "ref": "main",
    "previous_commit": "",
    "dirty": False,
    "version": "0.0.0-test",
    "profile": "custom",
    "link_mode": "symlink",
    "installed_at": "2026-01-01T00:00:00Z",
    "components": {
        "tools": False, "claude_hooks": True, "codex_plugin": False,
        "auto_update": False, "machine_snapshot": False, "slurm_snapshot": False,
    },
    "component_modes": {"auto_update": "check"},
    "managed_targets": [".claude/CLAUDE.md", ".codex/AGENTS.md", ".gemini/AGENTS.md"],
    "plugin": {"name": "oh-my-setting", "version": "0", "sha256": "0" * 64},
}
schema = os.environ["OMS_T_SCHEMA"]
if schema:
    receipt["hooks_schema"] = int(schema)
with open(os.path.join(home, ".config", "oh-my-setting", "install.json"),
          "w", encoding="utf-8") as fh:
    json.dump(receipt, fh, indent=2, sort_keys=True)
PY
}

run_surfaces() {
  local home="$1"

  HOME="$home" \
  XDG_CONFIG_HOME="$home/.config" \
  OMS_CLAUDE_SETTINGS="$home/.claude/settings.json" \
  OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" \
  OMS_DOCTOR_PROJECT_DIR="$home/repo" \
    bash "$ROOT/scripts/doctor.sh" --surfaces 2>&1
}

# --- expected-surface manifest ----------------------------------------------

test_print_expected_emits_the_registration_list() {
  local out="$TMP/expected.json"

  bash "$ROOT/scripts/install-claude-hooks.sh" --print-expected > "$out" ||
    fail "--print-expected should succeed from any checkout"

  OMS_T_OUT="$out" OMS_T_ROOT="$ROOT" python3 - <<'PY' || fail "--print-expected payload is wrong"
import json, os

with open(os.environ["OMS_T_OUT"], encoding="utf-8") as fh:
    doc = json.load(fh)
assert doc["schema"] == 1, doc
assert isinstance(doc["hooks_schema"], int), doc
assert doc["root"] == os.environ["OMS_T_ROOT"], doc

pairs = [(s["event"], s["script"]) for s in doc["surfaces"]]
# The two surfaces whose absence from a live settings.json went unreported are
# named here so a refactor cannot quietly drop them from the expected list.
assert ("SessionEnd", "precompact-handoff.sh") in pairs, pairs
assert ("PostToolUse", "fail-ledger-hook.sh") in pairs, pairs
assert ("UserPromptSubmit", "skill-router.sh") in pairs, pairs
assert len(pairs) == len(set(pairs)), pairs
for surface in doc["surfaces"]:
    assert surface["command"].endswith("/scripts/" + surface["script"]), surface
PY
}

# --- source vs live registrations -------------------------------------------

test_missing_registration_fails_and_is_named() {
  local home="$TMP/missing"
  local out

  # One surface dropped: the SessionEnd handoff, which is exactly what the
  # live machine turned out to be missing while the doctor reported ok.
  make_fixture "$home" \
    "[r for r in rows if not (r['event'] == 'SessionEnd' and r['script'] == 'precompact-handoff.sh')]" \
    1

  if out="$(run_surfaces "$home")"; then
    printf '%s\n' "$out"
    fail "--surfaces should exit nonzero when a registration is missing"
  fi
  case "$out" in
    *"MISSING REGISTRATION: SessionEnd -> precompact-handoff.sh"*) ;;
    *) printf '%s\n' "$out"; fail "the missing surface should be named" ;;
  esac
  case "$out" in
    *"MISSING REGISTRATION: PostToolUse -> precompact"*)
      printf '%s\n' "$out"
      fail "only the dropped surface should be reported missing"
      ;;
  esac
  # Registered-but-silent is not a failure, so the other handoff event still
  # reads ok even though nothing writes an event for it.
  case "$out" in
    *"ok: PreCompact -> precompact-handoff.sh"*) ;;
    *) printf '%s\n' "$out"; fail "a registered surface should stay ok" ;;
  esac
}

test_complete_registration_is_ok() {
  local home="$TMP/complete"
  local out

  make_fixture "$home" rows 1
  out="$(run_surfaces "$home")" ||
    { printf '%s\n' "$out"; fail "a complete settings.json should pass --surfaces"; }
  case "$out" in
    *"MISSING REGISTRATION"*)
      printf '%s\n' "$out"
      fail "nothing should be missing from a complete settings.json"
      ;;
  esac
  case "$out" in
    *"surfaces: ok"*) ;;
    *) printf '%s\n' "$out"; fail "--surfaces should report ok" ;;
  esac
  # No .oms/hooks/events.jsonl in the fixture: absent evidence is a note, and
  # every surface still reads ok. Silence is normal, not a fault.
  case "$out" in
    *"no harness event stream"*) ;;
    *) printf '%s\n' "$out"; fail "an absent event stream should be noted" ;;
  esac
  case "$out" in
    *"no-recent-evidence"*)
      printf '%s\n' "$out"
      fail "an absent event stream must not be reported as stale evidence"
      ;;
  esac
}

test_evidence_window_marks_a_silent_registered_surface() {
  local home="$TMP/evidence"
  local out

  make_fixture "$home" rows 1
  # A route from long before the window, and a fresh turn_guard: the router
  # surface should read stale while the turn guard reads current.
  {
    printf '%s\n' '{"action": "route", "ts": "2020-01-01T00:00:00Z"}'
    printf '{"action": "turn_guard", "ts": "%s"}\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  } > "$home/repo/.oms/hooks/events.jsonl"

  out="$(run_surfaces "$home")" ||
    { printf '%s\n' "$out"; fail "stale evidence must not fail the report"; }
  case "$out" in
    *"no-recent-evidence: UserPromptSubmit -> skill-router.sh"*) ;;
    *) printf '%s\n' "$out"; fail "an old route event should read as no-recent-evidence" ;;
  esac
  case "$out" in
    *"ok: Stop -> turn-guard.sh (registered; last turn_guard"*) ;;
    *) printf '%s\n' "$out"; fail "a fresh turn_guard event should read ok" ;;
  esac
}

# --- receipt schema stamp ----------------------------------------------------

test_stale_receipt_schema_warns() {
  local home="$TMP/stale-schema"
  local out

  make_fixture "$home" rows 0
  out="$(run_surfaces "$home")" ||
    { printf '%s\n' "$out"; fail "a stale hooks_schema is a warning, not a failure"; }
  case "$out" in
    *"warn: install receipt hooks_schema 0 != source"*) ;;
    *) printf '%s\n' "$out"; fail "a stale hooks_schema should warn" ;;
  esac
}

test_absent_receipt_schema_warns() {
  local home="$TMP/absent-schema"
  local out

  # An install that predates the stamp: the receipt is valid and complete, and
  # this warning is the only thing that says its surface list may be older.
  make_fixture "$home" rows ""
  out="$(run_surfaces "$home")" ||
    { printf '%s\n' "$out"; fail "an unstamped receipt is a warning, not a failure"; }
  case "$out" in
    *"warn: install receipt records no hooks_schema"*) ;;
    *) printf '%s\n' "$out"; fail "an unstamped receipt should warn" ;;
  esac
}

test_install_stamps_hooks_schema_into_the_receipt() {
  local home="$TMP/stamp"
  local recorded

  make_fixture "$home" rows ""
  HOME="$home" \
  XDG_CONFIG_HOME="$home/.config" \
  OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" \
  OMS_LOCK_DIR="$home/.locks" \
    bash "$ROOT/scripts/install-claude-hooks.sh" \
      --settings "$home/.claude/settings.json" >/dev/null ||
    fail "installing hooks into a fixture HOME should succeed"

  recorded="$(python3 -c '
import json, sys
with open(sys.argv[1], encoding="utf-8") as fh:
    print(json.load(fh).get("hooks_schema"))
' "$home/.config/oh-my-setting/install.json")"
  [ "$recorded" = "$EXPECTED_HOOKS_SCHEMA" ] ||
    fail "install should stamp hooks_schema=$EXPECTED_HOOKS_SCHEMA (got $recorded)"

  # The stamp has to survive the no-op path too: link.sh rewrites the receipt
  # without it on every update, and most updates change no command paths.
  python3 - "$home/.config/oh-my-setting/install.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as fh:
    row = json.load(fh)
row.pop("hooks_schema", None)
with open(sys.argv[1], "w", encoding="utf-8") as fh:
    json.dump(row, fh, indent=2, sort_keys=True)
PY
  HOME="$home" \
  XDG_CONFIG_HOME="$home/.config" \
  OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" \
  OMS_LOCK_DIR="$home/.locks" \
    bash "$ROOT/scripts/install-claude-hooks.sh" \
      --settings "$home/.claude/settings.json" | grep -q "already current" ||
    fail "the second install should be a no-op on settings.json"

  recorded="$(python3 -c '
import json, sys
with open(sys.argv[1], encoding="utf-8") as fh:
    print(json.load(fh).get("hooks_schema"))
' "$home/.config/oh-my-setting/install.json")"
  [ "$recorded" = "$EXPECTED_HOOKS_SCHEMA" ] ||
    fail "an already-current install should still stamp hooks_schema (got $recorded)"
}

# --- displaced user config ---------------------------------------------------

test_displaced_user_config_is_reported() {
  local home="$TMP/displaced"
  local out

  make_fixture "$home" rows 1
  mkdir -p "$home/.codex" "$home/.gemini"
  printf 'my own rules\n' > "$home/.claude/CLAUDE.md.backup.20260610160231"
  printf 'my own rules\n' > "$home/.codex/AGENTS.md.backup.20260610160231"

  # The default doctor run in a fixture HOME fails for unrelated reasons (no
  # managed targets are linked there), so this asserts on the report line, not
  # on the exit status.
  out="$(HOME="$home" \
    XDG_CONFIG_HOME="$home/.config" \
    OMS_CLAUDE_SETTINGS="$home/.claude/settings.json" \
    OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" \
    OMS_DOCTOR_PROJECT_DIR="$home/repo" \
    OH_MY_SETTING_MODEL_DOCTOR=0 \
    OH_MY_SETTING_CODEX_PLUGIN=0 \
    bash "$ROOT/scripts/doctor.sh" 2>&1 || true)"

  case "$out" in
    *"displaced user config: $home/.claude/CLAUDE.md.backup.20260610160231 (restore: oms uninstall, or merge it)"*) ;;
    *) printf '%s\n' "$out"; fail "a displaced CLAUDE.md backup should be reported" ;;
  esac
  case "$out" in
    *"displaced user config: $home/.codex/AGENTS.md.backup.20260610160231"*) ;;
    *) printf '%s\n' "$out"; fail "a displaced AGENTS.md backup should be reported" ;;
  esac
  case "$out" in
    *".gemini/AGENTS.md.backup"*)
      printf '%s\n' "$out"
      fail "a target with no backup should produce no line"
      ;;
  esac
}

test_clean_home_reports_no_displaced_config() {
  local home="$TMP/undisplaced"
  local out

  make_fixture "$home" rows 1
  out="$(HOME="$home" \
    XDG_CONFIG_HOME="$home/.config" \
    OMS_CLAUDE_SETTINGS="$home/.claude/settings.json" \
    OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" \
    OMS_DOCTOR_PROJECT_DIR="$home/repo" \
    OH_MY_SETTING_MODEL_DOCTOR=0 \
    OH_MY_SETTING_CODEX_PLUGIN=0 \
    bash "$ROOT/scripts/doctor.sh" 2>&1 || true)"

  case "$out" in
    *"displaced user config"*)
      printf '%s\n' "$out"
      fail "a HOME with no backups should say nothing about displaced config"
      ;;
  esac
}

# --- bump enforcement --------------------------------------------------------

# The expected list is the contract between an installed harness and a doctor
# checking it, and hooks_schema is how a doctor learns the two disagree. That
# only works if the number moves whenever the list does, which nothing else
# enforces: adding a surface and forgetting the bump leaves every stale install
# certifying itself. This hashes the list and requires both literals to move
# together.
# A capability receipt names each absence: a selected capability's missing
# tool is a defect, an unselected one is simply not installed (with the add
# command), the chosen core provider is never a mere council seat, and a
# receipt-less home keeps the exact legacy wording.
run_capability_doctor() {  # HOME -> stdout
  env -u NVM_DIR HOME="$1" \
    XDG_CONFIG_HOME="$1/.config" \
    OMS_CLAUDE_SETTINGS="$1/.claude/settings.json" \
    OMS_INSTALL_RECEIPT="$1/.config/oh-my-setting/install.json" \
    OMS_CAPABILITY_RECEIPT="$1/.config/oh-my-setting/capabilities.json" \
    OMS_DOCTOR_PROJECT_DIR="$1/repo" \
    OH_MY_SETTING_MODEL_DOCTOR=0 \
    OH_MY_SETTING_CODEX_PLUGIN=0 \
    OH_MY_SETTING_REQUIRE_TOOLS=0 \
    PATH="/usr/bin:/bin" \
    bash "$ROOT/scripts/doctor.sh" 2>&1 || true
}

test_capability_receipt_names_tool_absences() {
  local home="$TMP/capability"
  local out

  make_fixture "$home" rows 1
  cat > "$home/.config/oh-my-setting/capabilities.json" <<'EOF_CAP'
{"schema":1,"requested":["core","research"],"primary_provider":"codex"}
EOF_CAP
  out="$(run_capability_doctor "$home")"

  case "$out" in
    *"missing: command uv (selected capability research)"*) ;;
    *) printf '%s\n' "$out"; fail "a selected capability's absent tool must be a named defect" ;;
  esac
  case "$out" in
    *"missing: command codex (selected core provider)"*) ;;
    *) printf '%s\n' "$out"; fail "the chosen core provider's absence must be a defect, not a council note" ;;
  esac
  case "$out" in
    *"capability notion not installed: command ntn (add it: oms install-profile --apply --profile notion)"*) ;;
    *) printf '%s\n' "$out"; fail "an unselected capability must be reported as not installed, with the add command" ;;
  esac
  case "$out" in
    *"capability council not installed: command claude"*) ;;
    *) printf '%s\n' "$out"; fail "a non-primary provider seat must read as the uninstalled council" ;;
  esac
}

test_receiptless_home_keeps_legacy_wording() {
  local home="$TMP/receiptless"
  local out

  make_fixture "$home" rows 1
  out="$(run_capability_doctor "$home")"

  case "$out" in
    *"optional missing: command ntn"*) ;;
    *) printf '%s\n' "$out"; fail "a receipt-less home must keep the legacy optional-missing wording" ;;
  esac
  case "$out" in
    *"capability "*) printf '%s\n' "$out"; fail "no capability wording may appear without a receipt" ;;
  esac
}

test_expected_surface_list_is_bump_gated() {
  local computed
  local source_schema

  computed="$(bash "$ROOT/scripts/install-claude-hooks.sh" --print-expected | python3 -c '
import hashlib, json, sys

doc = json.load(sys.stdin)
rows = [
    {key: surface[key] for key in ("event", "script", "matcher", "timeout")}
    for surface in doc["surfaces"]
]
print(doc["hooks_schema"])
print(hashlib.sha256(json.dumps(rows, sort_keys=True).encode("utf-8")).hexdigest())
')"
  source_schema="$(printf '%s\n' "$computed" | sed -n '1p')"
  computed="$(printf '%s\n' "$computed" | sed -n '2p')"

  if [ "$computed" != "$EXPECTED_SURFACES_SHA256" ]; then
    cat >&2 <<EOF
FAIL: the expected hook surface list changed.
  recorded sha256: $EXPECTED_SURFACES_SHA256
  computed sha256: $computed
Update all three, in the same commit:
  1. HOOKS_SCHEMA in scripts/install-claude-hooks.sh -> $((EXPECTED_HOOKS_SCHEMA + 1))
  2. EXPECTED_HOOKS_SCHEMA in tests/doctor-surfaces-smoke.sh -> $((EXPECTED_HOOKS_SCHEMA + 1))
  3. EXPECTED_SURFACES_SHA256 in tests/doctor-surfaces-smoke.sh -> $computed
EOF
    exit 1
  fi

  if [ "$source_schema" != "$EXPECTED_HOOKS_SCHEMA" ]; then
    cat >&2 <<EOF
FAIL: HOOKS_SCHEMA is $source_schema but this test records $EXPECTED_HOOKS_SCHEMA.
The schema and the surface list move together. If the list changed, update
EXPECTED_SURFACES_SHA256 too; if it did not, revert the bump.
  EXPECTED_HOOKS_SCHEMA in tests/doctor-surfaces-smoke.sh -> $source_schema
EOF
    exit 1
  fi
}

test_print_expected_emits_the_registration_list
test_missing_registration_fails_and_is_named
test_complete_registration_is_ok
test_evidence_window_marks_a_silent_registered_surface
test_stale_receipt_schema_warns
test_absent_receipt_schema_warns
test_install_stamps_hooks_schema_into_the_receipt
test_displaced_user_config_is_reported
test_clean_home_reports_no_displaced_config
test_expected_surface_list_is_bump_gated
test_capability_receipt_names_tool_absences
test_receiptless_home_keeps_legacy_wording

echo "doctor-surfaces-smoke: ok"
