#!/usr/bin/env bash
set -euo pipefail

# Smoke tests for the hook-surface report: install-claude-hooks.sh
# --print-expected, `doctor.sh --surfaces` comparing source against a live
# settings.json and the harness event stream, the receipt's hooks_schema
# stamp, installer convergence and ownership, registration drift in both doctor
# modes, the displaced-user-config notice in the default doctor run, and the
# gate that forces a schema bump when the expected list changes.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-doctor-surfaces.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

# The expected surface list, recorded. Both literals move together or the gate
# below fails; see test_expected_surface_list_is_bump_gated.
EXPECTED_HOOKS_SCHEMA=11
EXPECTED_SURFACES_SHA256=4bbadf82e8df36c939a76bb4363034c361496021da2088c26d0d8c53b4734f84

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

test_claude_registry_permissions_warn() {
  local home="$TMP/registry-permissions"
  local out

  make_fixture "$home"
  mkdir -p "$home/claude/sessions"
  chmod 700 "$home" "$home/claude"
  chmod 775 "$home/claude/sessions"
  out="$(CLAUDE_CONFIG_DIR="$home/claude" run_default_doctor "$home")"
  case "$out" in
    *"warn: Claude session registry disabled: inbox is not private $home/claude/sessions mode 0775; advisor card lookup, session lineage rebind, and app notify may degrade. Fix with: chmod go-rwx $home/claude/sessions"*) ;;
    *) printf '%s\n' "$out"; fail "writable Claude registry should warn with path, mode, degradation and remedy" ;;
  esac

  chmod 700 "$home/claude/sessions"
  chmod 775 "$home/claude"
  out="$(CLAUDE_CONFIG_DIR="$home/claude" run_default_doctor "$home")"
  case "$out" in
    *"writable inbox path $home/claude mode 0775"*"Fix with: chmod go-w $home/claude"*) ;;
    *) printf '%s\n' "$out"; fail "a writable registry parent should warn with go-w" ;;
  esac

  rmdir "$home/claude/sessions"
  chmod 700 "$home/claude"
  out="$(CLAUDE_CONFIG_DIR="$home/claude" run_default_doctor "$home")"
  case "$out" in
    *"Claude session registry disabled"*) printf '%s\n' "$out"; fail "private Claude registry must not warn" ;;
  esac
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
assert ("PreToolUse", "git-move-guard-hook.sh") in pairs, pairs
assert ("PreToolUse", "command-guard-hook.sh") in pairs, pairs
assert ("UserPromptSubmit", "skill-router.sh") in pairs, pairs
assert ("PreToolUse", "tier-guard-hook.sh") not in pairs, pairs
assert ("PostToolUse", "telemetry-hook.sh") not in pairs, pairs
assert ("SubagentStop", "telemetry-hook.sh") not in pairs, pairs
assert ("SessionStart", "telemetry-hook.sh") in pairs, pairs
assert ("SessionEnd", "telemetry-hook.sh") in pairs, pairs
assert len(pairs) == len(set(pairs)), pairs
for surface in doc["surfaces"]:
    assert surface["command"].endswith("/scripts/" + surface["script"]), surface
PY
}

run_install() {
  local home="$1"

  HOME="$home" \
  XDG_CONFIG_HOME="$home/.config" \
  OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" \
  OMS_LOCK_DIR="$home/.locks" \
    bash "$ROOT/scripts/install-claude-hooks.sh" \
      --settings "$home/.claude/settings.json"
}

run_default_doctor() {
  local home="$1"

  # Other subsystems are absent from a fixture HOME, so only the hook lines
  # are asserted, never the exit status.
  HOME="$home" \
  XDG_CONFIG_HOME="$home/.config" \
  OMS_CLAUDE_SETTINGS="$home/.claude/settings.json" \
  OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" \
  OMS_DOCTOR_PROJECT_DIR="$home/repo" \
  OH_MY_SETTING_MODEL_DOCTOR=0 \
  OH_MY_SETTING_CODEX_PLUGIN=0 \
    bash "$ROOT/scripts/doctor.sh" 2>&1 || true
}

# A directory carrying the source markers an earlier OMS checkout has.
make_old_root() {
  mkdir -p "$1/.agents/plugins" "$1/scripts"
  : > "$1/.agents/plugins/marketplace.json"
  : > "$1/scripts/install-claude-hooks.sh"
}

test_install_converges_owned_rows_only() {
  local home="$TMP/converge"
  local old="$home/old oms"
  local out

  # Rows from a verified earlier checkout whose path has a space, in both the
  # quoted form and the unquoted form earlier installers wrote; retired rows;
  # a deleted timeout, a wrong matcher and a duplicate; and user hooks that
  # merely name an OMS script.
  make_fixture "$home" rows 4
  make_old_root "$old"
  OMS_T_ROOT="$ROOT" python3 - "$home/.claude/settings.json" "$old" "$home" <<'PY'
import json, os, shlex, sys

path, old, home = sys.argv[1:]
root = os.environ["OMS_T_ROOT"]
with open(path, encoding="utf-8") as fh:
    row = json.load(fh)
hooks = row["hooks"]

def cmd(base, script, quote=True):
    target = "%s/scripts/%s" % (base, script)
    return "bash " + (shlex.quote(target) if quote else target)

hooks.setdefault("PreToolUse", []).extend([
    {"matcher": "Edit|Write|MultiEdit|NotebookEdit",
     "hooks": [{"type": "command", "command": cmd(old, "tier-guard-hook.sh"), "timeout": 5}]},
    # An unverifiable root is not evidence of ownership, even for an OMS name.
    {"hooks": [{"type": "command", "command": cmd(home + "/gone", "tier-guard-hook.sh")}]},
])
hooks["PostToolUse"].append(
    {"hooks": [{"type": "command", "command": cmd(old, "telemetry-hook.sh"), "timeout": 5}]})
hooks["PostToolUse"].append({"hooks": [{"type": "command", "command": "echo user-post-hook"}]})
hooks["SubagentStop"] = [{"hooks": [
    {"type": "command", "command": cmd(old, "telemetry-hook.sh", quote=False), "timeout": 5}]}]
for entry in hooks["PostToolUse"]:
    for hook in entry["hooks"]:
        if hook["command"].endswith("/scripts/syntax-guard-hook.sh"):
            entry["matcher"] = "Edit"
            hook["command"] = cmd(old, "syntax-guard-hook.sh", quote=False)
hooks["UserPromptSubmit"][0]["hooks"][0]["command"] = cmd(old, "skill-router.sh")
del hooks["Stop"][0]["hooks"][0]["timeout"]
hooks["Stop"][0].update(matcher="user-only", description="preserve segments")
hooks["Stop"][0]["hooks"][:0] = [{"type": "command", "command": "echo before", "timeout": 9}]
hooks["Stop"][0]["hooks"].append({"type": "command", "command": "echo after", "timeout": 8})
hooks["SessionStart"][0]["hooks"][0]["timeout"] = 99
hooks["Stop"].append({"matcher": "keep-me", "hooks": [
    {"type": "command", "command": "echo user-a"},
    {"type": "command", "command": cmd(old, "turn-guard.sh"), "timeout": 12},
    {"type": "command", "command": "echo user-b"},
]})
hooks["Stop"].append({"hooks": [
    {"type": "command", "command": cmd(home + "/elsewhere", "turn-guard.sh")},
    {"type": "command", "command": "echo turn-guard.sh"},
    {"type": "command", "command": "bash %s/wrap.sh %s" % (home, cmd(root, "turn-guard.sh")[5:])},
]})
with open(path, "w", encoding="utf-8") as fh:
    json.dump(row, fh, indent=2)
PY
  cp "$home/.claude/settings.json" "$TMP/converge-before.json"

  run_install "$home" >/dev/null || fail "install should converge a previous hook surface list"

  OMS_T_ROOT="$ROOT" python3 - "$home/.claude/settings.json" "$TMP/converge-before.json" "$home" \
    <<'PY' || fail "install did not converge exactly the OMS-owned rows"
import json, os, sys

path, before_path, home = sys.argv[1:]
root = os.environ["OMS_T_ROOT"]
hooks = json.load(open(path, encoding="utf-8"))["hooks"]
before = json.load(open(before_path, encoding="utf-8"))["hooks"]
own = lambda script: "bash %s/scripts/lib/python-entry.sh bash %s/scripts/%s" % (root, root, script)
commands = [h.get("command", "") for entries in hooks.values()
            for entry in entries for h in entry.get("hooks", [])]
assert not any("old oms" in c for c in commands), commands
assert "SubagentStop" not in hooks, hooks
# before["PreToolUse"] = [the two current Bash guards, the retired owned row, an unverifiable root]
assert hooks["PreToolUse"] == [
    {"matcher": "Bash",
     "hooks": [{"type": "command", "command": own("git-move-guard-hook.sh"), "timeout": 5}]},
    {"matcher": "Bash",
     "hooks": [{"type": "command", "command": own("command-guard-hook.sh"), "timeout": 5}]},
    before["PreToolUse"][3],
], hooks["PreToolUse"]
assert not any("telemetry-hook.sh" in h["command"]
               for entry in hooks["PostToolUse"] for h in entry["hooks"]), hooks
assert {"hooks": [{"type": "command", "command": "echo user-post-hook"}]} in hooks["PostToolUse"]
guard = [e for e in hooks["PostToolUse"] if e["hooks"][0]["command"] == own("syntax-guard-hook.sh")]
assert guard == [{"matcher": "Edit|Write|MultiEdit",
                  "hooks": [{"type": "command", "command": own("syntax-guard-hook.sh"), "timeout": 5}]}], guard
assert hooks["UserPromptSubmit"][0]["hooks"][0]["command"] == own("skill-router.sh"), hooks
assert hooks["Stop"] == [
    {"matcher": "user-only", "description": "preserve segments",
     "hooks": [{"type": "command", "command": "echo before", "timeout": 9}]},
    {"description": "preserve segments",
     "hooks": [{"type": "command", "command": own("turn-guard.sh"), "timeout": 12}]},
    {"matcher": "user-only", "description": "preserve segments",
     "hooks": [{"type": "command", "command": "echo after", "timeout": 8}]},
    {"matcher": "keep-me", "hooks": [{"type": "command", "command": "echo user-a"},
                                     {"type": "command", "command": "echo user-b"}]},
    before["Stop"][2],
], hooks["Stop"]
PY

  out="$(run_install "$home")" || fail "a converged install should rerun cleanly"
  case "$out" in
    *"already current"*) ;;
    *) printf '%s\n' "$out"; fail "a converged settings.json should be a no-op" ;;
  esac
  out="$(run_surfaces "$home")" ||
    { printf '%s\n' "$out"; fail "a converged settings.json should pass --surfaces"; }
}

test_install_quotes_a_root_with_spaces() {
  OMS_T_ROOT="$ROOT" python3 - <<'PY' || fail "a root with spaces must be quoted and still recognized"
import importlib.util, os, shlex, sys

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "hooks", os.path.join(os.environ["OMS_T_ROOT"], "scripts", "lib", "claude-hook-surfaces.py"))
lib = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lib)
root = "/opt/my oms"
command = lib.hook_command(root, "turn-guard.sh")
assert shlex.split(command) == ["bash", root + "/scripts/lib/python-entry.sh",
                                "bash", root + "/scripts/turn-guard.sh"], command
assert lib.owned_script({"type": "command", "command": command}, [root]) == "turn-guard.sh"
plain = lib.hook_command("/opt/oms", "turn-guard.sh")
assert plain == "bash /opt/oms/scripts/lib/python-entry.sh bash /opt/oms/scripts/turn-guard.sh", plain
assert lib.owned_script({"command": "bash /opt/other/scripts/lib/python-entry.sh bash /opt/oms/scripts/turn-guard.sh"},
                        ["/opt/oms"]) is None
assert lib.owned_script({"command": plain + " extra"}, ["/opt/oms"]) is None
for invalid in ("", "0", "\"", "bash", "bash /opt/oms/scripts/lib/python-entry.sh", "bash /opt/oms/scripts/lib/python-entry.sh python3 /opt/oms/scripts/turn-guard.sh"):
    assert lib.owned_script({"command": invalid}, ["/opt/oms"]) is None, invalid
assert lib.owned_script({"command": "bash /opt/oms/scripts/turn-guard.sh"}, ["/opt/oms"]) == "turn-guard.sh"
assert lib.owned_script({"type": "command", "command": "bash /opt/oms/scripts/turn-guard.sh x"},
                        ["/opt/oms"]) is None
assert lib.owned_script({"type": "command", "command": "bash scripts/turn-guard.sh"}, ["/opt/oms"]) is None
PY
}

test_registration_drift_fails_both_doctor_modes() {
  local home="$TMP/drift"
  local out

  make_fixture "$home" rows "$EXPECTED_HOOKS_SCHEMA"
  make_old_root "$home/old"
  OMS_T_ROOT="$ROOT" python3 - "$home/.claude/settings.json" "$home" <<'PY'
import json, os, sys

path, home = sys.argv[1:]
root = os.environ["OMS_T_ROOT"]
with open(path, encoding="utf-8") as fh:
    row = json.load(fh)
hooks = row["hooks"]
del hooks["Stop"][0]["hooks"][0]["timeout"]
del hooks["SessionEnd"][0]["hooks"][0]["type"]
for entry in hooks["PostToolUse"]:
    if "syntax-guard-hook.sh" in entry["hooks"][0]["command"]:
        entry["matcher"] = "Edit"
resume = [e for e in hooks["SessionStart"] if "resume-hook.sh" in e["hooks"][0]["command"]]
# The second registration drifts; the audit checks every registration, not only the first.
duplicate = json.loads(json.dumps(resume[0]))
duplicate["hooks"][0]["timeout"] = 99
hooks["SessionStart"].append(duplicate)
hooks["SubagentStop"] = [{"hooks": [
    {"type": "command", "command": "bash %s/scripts/telemetry-hook.sh" % root, "timeout": 5}]}]
hooks["UserPromptSubmit"][0]["hooks"][0]["command"] = "bash %s/old/scripts/skill-router.sh" % home
# A same-named script outside an OMS checkout does not count as registered.
hooks["PreCompact"][0]["hooks"][0]["command"] = "bash %s/elsewhere/scripts/precompact-handoff.sh" % home
with open(path, "w", encoding="utf-8") as fh:
    json.dump(row, fh, indent=2)
PY

  if out="$(run_surfaces "$home")"; then
    printf '%s\n' "$out"
    fail "--surfaces should fail on registration drift"
  fi
  for line in \
    "REGISTRATION DRIFT: Stop -> turn-guard.sh (timeout absent, expected 12)" \
    "REGISTRATION DRIFT: SessionEnd -> precompact-handoff.sh (type absent, expected command)" \
    "REGISTRATION DRIFT: PostToolUse -> syntax-guard-hook.sh (matcher 'Edit', expected 'Edit|Write|MultiEdit')" \
    "DUPLICATE REGISTRATION: SessionStart -> resume-hook.sh (2 registrations)" \
    "REGISTRATION DRIFT: SessionStart -> resume-hook.sh (timeout 99, expected" \
    "RETIRED REGISTRATION: SubagentStop -> telemetry-hook.sh (still registered)" \
    "REGISTRATION DRIFT: UserPromptSubmit -> skill-router.sh (command" \
    "MISSING REGISTRATION: PreCompact -> precompact-handoff.sh"; do
    case "$out" in
      *"$line"*) ;;
      *) printf '%s\n' "$out"; fail "--surfaces should report: $line" ;;
    esac
  done

  out="$(run_default_doctor "$home")"
  for line in \
    "fail: claude hook drift: Stop -> turn-guard.sh (timeout absent, expected 12)" \
    "fail: claude hook drift: SessionEnd -> precompact-handoff.sh (type absent, expected command)" \
    "fail: claude hook drift: PostToolUse -> syntax-guard-hook.sh (matcher" \
    "fail: claude hook duplicate: SessionStart -> resume-hook.sh (2 registrations)" \
    "fail: claude hook retired: SubagentStop -> telemetry-hook.sh (still registered)" \
    "fail: claude hook drift: UserPromptSubmit -> skill-router.sh (command" \
    "fail: claude hook missing: PreCompact -> precompact-handoff.sh"; do
    case "$out" in
      *"$line"*) ;;
      *) printf '%s\n' "$out"; fail "the default doctor should report: $line" ;;
    esac
  done
  case "$out" in
    *"ok: claude hooks registered"*) printf '%s\n' "$out"; fail "drift must not read as registered" ;;
  esac

  # The installer is the remedy both reports name, and it clears every line.
  run_install "$home" >/dev/null || fail "install should repair registration drift"
  out="$(run_surfaces "$home")" ||
    { printf '%s\n' "$out"; fail "--surfaces should pass after the installer converges"; }
  out="$(run_default_doctor "$home")"
  case "$out" in
    *"ok: claude hooks registered"*) ;;
    *) printf '%s\n' "$out"; fail "the default doctor should pass after the installer converges" ;;
  esac

  # The command guard is registered fail-open unless a person opts in, and the
  # doctor says so as a note beside a healthy registration, never as a failure.
  case "$out" in
    *"note: claude guard PreToolUse -> command-guard-hook.sh is registered fail-open"*) ;;
    *) printf '%s\n' "$out"; fail "the default doctor should note the fail-open command guard" ;;
  esac
  # A guard whose helper is gone lets calls through with a stderr line; the
  # doctor says so read-only. Healthy: silent. Copy of scripts/ owning the
  # receipt with hook_state.py removed: named.
  case "$out" in
    *"cannot evaluate commands"*) printf '%s\n' "$out"; fail "a complete install must not warn that guards cannot evaluate" ;;
  esac
  mkdir -p "$home/gcopy"
  cp -R "$ROOT/scripts" "$home/gcopy/scripts"
  [ ! -f "$ROOT/VERSION" ] || cp "$ROOT/VERSION" "$home/gcopy/VERSION"
  rm -f "$home/gcopy/scripts/lib/hook_state.py"
  OMS_T_ROOT="$ROOT" python3 - "$home" "$home/gcopy" <<'PY'
import json, os, sys

home, copy = sys.argv[1:]
receipt = os.path.join(home, ".config", "oh-my-setting", "install.json")
row = json.load(open(receipt, encoding="utf-8"))
row["source_root"] = copy
json.dump(row, open(receipt, "w", encoding="utf-8"))
PY
  out="$(HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_CLAUDE_SETTINGS="$home/.claude/settings.json" \
    OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" OMS_DOCTOR_PROJECT_DIR="$home/repo" \
    OH_MY_SETTING_MODEL_DOCTOR=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
    bash "$home/gcopy/scripts/doctor.sh" 2>&1 || true)"
  case "$out" in
    *"claude guards cannot evaluate commands (missing: hook_state.py)"*) ;;
    *) printf '%s\n' "$out" | grep -i "guard\|claude hook" >&2; fail "doctor should name the missing guard helper" ;;
  esac
  OMS_T_ROOT="$ROOT" python3 - "$home" "$ROOT" <<'PY'
import json, os, sys

home, root = sys.argv[1:]
receipt = os.path.join(home, ".config", "oh-my-setting", "install.json")
row = json.load(open(receipt, encoding="utf-8"))
row["source_root"] = root
json.dump(row, open(receipt, "w", encoding="utf-8"))
PY
  # Opting in writes onFailure only when the installed claude validates the
  # field: a claude that says nothing about a bad value leaves the row as it was.
  mkdir -p "$home/probe-bin"
  printf '%s\n' '#!/bin/sh' 'echo "Claude Code doctor"' > "$home/probe-bin/claude"
  chmod +x "$home/probe-bin/claude"
  out="$(PATH="$home/probe-bin:$PATH" OMS_GUARD_FAIL_CLOSED=1 run_install "$home" 2>&1)" ||
    { printf '%s\n' "$out"; fail "an unsupported fail-closed request must not fail the install"; }
  case "$out" in
    *"OMS_GUARD_FAIL_CLOSED=1 was not applied"*) ;;
    *) printf '%s\n' "$out"; fail "an unsupported fail-closed request should be reported" ;;
  esac
  ! grep -q onFailure "$home/.claude/settings.json" || fail "an unprobed claude must not get the onFailure field"
  cat > "$home/probe-bin/claude" <<'EOF'
#!/bin/sh
[ "$1" = doctor ] || exit 1
grep -q '"onFailure":"oms-probe"' .claude/settings.json || exit 0
echo 'Invalid settings'
echo '- hooks.PreToolUse.0.hooks.0.onFailure: Invalid value. Expected one of: "continue", "block"'
EOF
  PATH="$home/probe-bin:$PATH" OMS_GUARD_FAIL_CLOSED=1 run_install "$home" >/dev/null 2>&1 ||
    fail "the fail-closed opt-in should install"
  python3 - "$home/.claude/settings.json" <<'PY' || fail "only the command guard row should carry onFailure block"
import json, sys
hooks = json.load(open(sys.argv[1], encoding="utf-8"))["hooks"]
marked = [h["command"].rsplit("/", 1)[-1] for entries in hooks.values() for e in entries
          for h in e.get("hooks", []) if "onFailure" in h]
assert marked == ["command-guard-hook.sh"], marked
guard = [h for e in hooks["PreToolUse"] for h in e["hooks"] if h["command"].endswith("command-guard-hook.sh")]
assert guard[0]["onFailure"] == "block" and guard[0]["timeout"] == 5, guard
PY
  out="$(run_default_doctor "$home")"
  case "$out" in
    *"registered fail-open"*) printf '%s\n' "$out"; fail "a fail-closed guard must not be noted as fail-open" ;;
  esac
  case "$out" in
    *"ok: claude hooks registered"*) ;;
    *) printf '%s\n' "$out"; fail "onFailure on the guard row must not read as drift" ;;
  esac
  run_surfaces "$home" >/dev/null || fail "--surfaces should pass with a fail-closed guard"
  # An update that does not repeat the request keeps the choice; =0 undoes it.
  run_install "$home" >/dev/null || fail "a plain reinstall should succeed"
  grep -q '"onFailure": "block"' "$home/.claude/settings.json" || fail "a plain reinstall must keep the fail-closed choice"
  OMS_GUARD_FAIL_CLOSED=0 run_install "$home" >/dev/null || fail "turning fail-closed off should succeed"
  ! grep -q onFailure "$home/.claude/settings.json" || fail "OMS_GUARD_FAIL_CLOSED=0 should remove the field"
  if OMS_GUARD_FAIL_CLOSED=maybe run_install "$home" >/dev/null 2>&1; then
    fail "an unknown OMS_GUARD_FAIL_CLOSED value should be refused"
  fi
  # A probe failure with the opt-in downgrades an existing block (it would
  # contradict the fail-open warning); a hand-written onFailure value is
  # audited and converged away, a block on a non-guard row too.
  local cfg="$home/.claude/settings.json" stub_ok
  stub_ok="$(cat "$home/probe-bin/claude")"
  OMS_GUARD_FAIL_CLOSED=1 PATH="$home/probe-bin:$PATH" run_install "$home" >/dev/null 2>&1 || fail "fail-closed opt-in should install"
  grep -q '"onFailure": "block"' "$cfg" || fail "fixture needs a block"
  printf '%s\n' '#!/bin/sh' 'echo "Claude Code doctor"' > "$home/probe-bin/claude"
  PATH="$home/probe-bin:$PATH" OMS_GUARD_FAIL_CLOSED=1 run_install "$home" >/dev/null 2>&1 || fail "an unprobed opt-in must not fail"
  ! grep -q onFailure "$cfg" || fail "an unprobed opt-in must downgrade an existing block"
  printf '%s\n' "$stub_ok" > "$home/probe-bin/claude"
  python3 - "$cfg" <<'PY'
import json, sys
d = json.load(open(sys.argv[1])); n = 0
for es in d["hooks"].values():
    for e in es:
        for h in e["hooks"]:
            if h["command"].endswith("command-guard-hook.sh"):
                h["onFailure"] = "oms-bogus"
            elif h["command"].endswith("skill-router.sh"):
                h["onFailure"] = "block"
json.dump(d, open(sys.argv[1], "w"))
PY
  out="$(run_default_doctor "$home")"
  case "$out" in *"onFailure 'oms-bogus'"*"onFailure 'block'"*|*"onFailure 'block'"*"onFailure 'oms-bogus'"*) ;; *) printf '%s\n' "$out"; fail "doctor should flag unrequested onFailure values" ;; esac
  run_install "$home" >/dev/null || fail "install should converge onFailure"
  ! grep -q onFailure "$cfg" || fail "converge must drop onFailure the manifest does not ask for"
  # A dead blocking row from a checkout that is gone: doctor names it and the
  # installer replaces it, and removal deletes it even with a bad opt-in value.
  python3 - "$cfg" "$home" <<'PY'
import json, shlex, sys
d = json.load(open(sys.argv[1])); dead = sys.argv[2] + "/gone-checkout"
for es in d["hooks"].values():
    for e in es:
        for h in e["hooks"]:
            if h["command"].endswith("command-guard-hook.sh"):
                h["command"] = " ".join(shlex.quote(a) for a in (
                    "bash", dead + "/scripts/lib/python-entry.sh", "bash", dead + "/scripts/command-guard-hook.sh"))
                h["onFailure"] = "block"
json.dump(d, open(sys.argv[1], "w"))
PY
  cp "$cfg" "$home/dead-settings.json"
  # A dangling symlink where the checkout was is as dead as a missing one.
  ln -s "$home/nowhere" "$home/gone-checkout"
  out="$(run_default_doctor "$home")"
  case "$out" in *"dead OMS hook"*) ;; *) printf '%s\n' "$out"; fail "doctor should report a dead OMS hook row" ;; esac
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" OMS_LOCK_DIR="$home/.locks" \
    OMS_GUARD_FAIL_CLOSED=maybe bash "$ROOT/scripts/install-claude-hooks.sh" --remove --settings "$home/dead-settings.json" >/dev/null 2>&1 ||
    fail "--remove must ignore an invalid OMS_GUARD_FAIL_CLOSED"
  ! grep -q gone-checkout "$home/dead-settings.json" || fail "--remove must delete a dead OMS row"
  run_install "$home" >/dev/null || fail "install should replace a dead OMS row"
  ! grep -q gone-checkout "$cfg" || fail "install must replace a dead OMS row"
  python3 - "$home/.claude/settings.json" "$home" <<'PY' || fail "the lookalike user hook must survive"
import json, sys
hooks = json.load(open(sys.argv[1], encoding="utf-8"))["hooks"]
commands = [h["command"] for e in hooks["PreCompact"] for h in e["hooks"]]
assert "bash %s/elsewhere/scripts/precompact-handoff.sh" % sys.argv[2] in commands, commands
PY
  # A live launcher-form row from another project without OMS markers is not
  # dead: install and removal leave it alone.
  mkdir -p "$home/foreign/scripts/lib"
  : > "$home/foreign/scripts/lib/python-entry.sh"
  : > "$home/foreign/scripts/command-guard-hook.sh"
  python3 - "$cfg" "$home/foreign" <<'PY'
import json, shlex, sys
d = json.load(open(sys.argv[1])); foreign = sys.argv[2]
d["hooks"].setdefault("PreToolUse", []).append({"matcher": "Bash", "hooks": [{"type": "command", "command": " ".join(
    shlex.quote(a) for a in ("bash", foreign + "/scripts/lib/python-entry.sh", "bash", foreign + "/scripts/command-guard-hook.sh"))}]})
json.dump(d, open(sys.argv[1], "w"))
PY
  run_install "$home" >/dev/null || fail "install should succeed beside a foreign launcher row"
  grep -q "$home/foreign/scripts/command-guard-hook.sh" "$cfg" || fail "install must keep a live foreign launcher row"
  cp "$cfg" "$home/foreign-settings.json"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" OMS_LOCK_DIR="$home/.locks" \
    bash "$ROOT/scripts/install-claude-hooks.sh" --remove --settings "$home/foreign-settings.json" >/dev/null 2>&1 ||
    fail "--remove should succeed beside a foreign launcher row"
  grep -q "$home/foreign/scripts/command-guard-hook.sh" "$home/foreign-settings.json" || fail "--remove must keep a live foreign launcher row"
}

test_surfaces_expect_commands_at_the_install_owner() {
  local home="$TMP/owner"
  local installed="$home/installed"
  local out

  # --surfaces runs from a newer source while settings point at the install
  # owner; that is the expected shape, not command drift.
  make_fixture "$home" rows "$EXPECTED_HOOKS_SCHEMA"
  make_old_root "$installed"
  OMS_T_ROOT="$ROOT" python3 - "$home" "$installed" <<'PY'
import json, os, sys

home, installed = sys.argv[1:]
root = os.environ["OMS_T_ROOT"]
path = os.path.join(home, ".claude", "settings.json")
text = open(path, encoding="utf-8").read().replace(root + "/scripts/", installed + "/scripts/")
open(path, "w", encoding="utf-8").write(text)
receipt = os.path.join(home, ".config", "oh-my-setting", "install.json")
row = json.load(open(receipt, encoding="utf-8"))
row["source_root"] = installed
json.dump(row, open(receipt, "w", encoding="utf-8"))
PY
  out="$(run_surfaces "$home")" ||
    { printf '%s\n' "$out"; fail "commands at the install owner should pass --surfaces"; }
  case "$out" in
    *"DRIFT"*) printf '%s\n' "$out"; fail "the install owner's commands are not drift" ;;
  esac
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
  # Real producers are conditional: routing uses presence rather than the
  # retired route action, and budget blocks use session_budget.
  {
    printf '%s\n' '{"action":"route","hook":"UserPromptSubmit","ts":"2020-01-01T00:00:00Z"}'
    printf '{"action":"presence","hook":"UserPromptSubmit","ts":"%s"}\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '{"action":"session_budget","hook":"Stop","ts":"%s"}\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '%s\n' '{"action":"telemetry","hook":"SessionStart","ts":"2020-01-01T00:00:00Z"}'
  } > "$home/repo/.oms/hooks/events.jsonl"

  out="$(run_surfaces "$home")" ||
    { printf '%s\n' "$out"; fail "stale evidence must not fail the report"; }
  case "$out" in
    *"ok: UserPromptSubmit -> skill-router.sh (registered; last presence"*) ;;
    *) printf '%s\n' "$out"; fail "current prompt presence must count as router evidence" ;;
  esac
  case "$out" in
    *"ok: Stop -> turn-guard.sh (registered; last session_budget"*) ;;
    *) printf '%s\n' "$out"; fail "budget block must count as guard evidence" ;;
  esac
  case "$out" in
    *"no-recent-evidence: SessionStart -> telemetry-hook.sh"*) ;;
    *) printf '%s\n' "$out"; fail "stale lifecycle telemetry must remain visible" ;;
  esac

  # Other event types and ignored children cannot prove these surfaces ran.
  {
    printf '{"action":"presence","hook":"SessionStart","ts":"%s"}\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '{"action":"session_budget","hook":"UserPromptSubmit","ts":"%s"}\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '{"action":"ignored_child","hook":"UserPromptSubmit","status":"route","ts":"%s"}\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '%s\n' '{"action":"presence","hook":"UserPromptSubmit","ts":"2020-01-01T00:00:00Z"}'
  } > "$home/repo/.oms/hooks/events.jsonl"
  out="$(run_surfaces "$home")" || fail "conditional silence must not fail registration"
  case "$out" in
    *"ok: UserPromptSubmit -> skill-router.sh (registered; conditional evidence only;"*"ok: Stop -> turn-guard.sh (registered; conditional evidence only;"*) ;;
    *) printf '%s\n' "$out"; fail "conditional silence must be distinguished from unused hooks" ;;
  esac
  case "$out" in
    *"no-recent-evidence: UserPromptSubmit"*|*"no-recent-evidence: Stop"*)
      printf '%s\n' "$out"; fail "conditional producers must not count as stale hook warnings" ;;
  esac
}

# --- receipt schema stamp ----------------------------------------------------

test_stale_receipt_schema_warns() {
  local home out schema expected
  for schema in 0 ''; do
    home="$TMP/schema-${schema:-absent}"
    if [ -n "$schema" ]; then
      expected="warn: install receipt hooks_schema 0 != source"
    else
      expected="warn: install receipt records no hooks_schema"
    fi
    make_fixture "$home" rows "$schema"
    out="$(run_surfaces "$home")" ||
      { printf '%s\n' "$out"; fail "stale or unstamped hooks_schema is a warning, not a failure"; }
    case "$out" in
      *"$expected"*) ;;
      *) printf '%s\n' "$out"; fail "stale or unstamped hooks_schema should warn: $expected" ;;
    esac
  done
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
      --settings "$home/.claude/settings.json" | grep "already current" > /dev/null ||
    fail "the second install should be a no-op on settings.json"

  recorded="$(python3 -c '
import json, sys
with open(sys.argv[1], encoding="utf-8") as fh:
    print(json.load(fh).get("hooks_schema"))
' "$home/.config/oh-my-setting/install.json")"
  [ "$recorded" = "$EXPECTED_HOOKS_SCHEMA" ] ||
    fail "an already-current install should still stamp hooks_schema (got $recorded)"

  # Alternate targets never certify or unstamp the canonical registration.
  python3 - "$ROOT" "$home" <<'PYBIND'
import json, os, subprocess, sys
from pathlib import Path
root, home = map(Path, sys.argv[1:])
receipt = home / ".config/oh-my-setting/install.json"
env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"),
           OMS_INSTALL_RECEIPT=str(receipt), OMS_LOCK_DIR=str(home / ".locks"))
installer = ["bash", str(root / "scripts/install-claude-hooks.sh")]

def hooks(path, remove=False):
    result = subprocess.run(installer + ["--settings", str(path)] + (["--remove"] if remove else []),
                            env=env, capture_output=True, text=True)
    assert result.returncode == 0, (result.stdout, result.stderr)
    return result

before = receipt.read_bytes()
alternate = home / "alternate/settings.json"
hooks(alternate)
assert receipt.read_bytes() == before, "alternate install stamped main receipt"
hooks(alternate, True)
assert receipt.read_bytes() == before, "alternate remove unstamped main receipt"
alias = home / "physical-alias"
alias.symlink_to(home / ".claude", target_is_directory=True)
hooks(alias / "settings.json")
assert receipt.read_bytes() == before
# The unchanged legacy owner decoder accepts the producer's additive field.
producer = 'source "$1/scripts/lib/install-contract.sh"; oms_install_write_receipt "$1"; oms_install_receipt_owner'
result = subprocess.run(["bash", "-c", producer, "receipt-fixture", str(root)],
                        env=env, capture_output=True, text=True)
assert result.returncode == 0 and result.stdout.strip() == str(root.resolve()), result
row = json.loads(receipt.read_text())
assert row["claude_settings_path"] == str(home / ".claude/settings.json")
hooks(home / ".claude/settings.json")
hooks(home / ".claude/settings.json", True)
assert "hooks_schema" not in json.loads(receipt.read_text())
# Unknown or retargeted bindings cannot attest either target.
for binding in (None, 0, "relative/settings.json", str(alias / "settings.json")):
    row["claude_settings_path"] = binding
    row.pop("hooks_schema", None)
    receipt.write_text(json.dumps(row))
    before = receipt.read_bytes()
    hooks(home / ".claude/settings.json")
    assert receipt.read_bytes() == before
# A legacy writer may drop the field. Only the actual default is eligible.
row.pop("claude_settings_path")
receipt.write_text(json.dumps(row))
before = receipt.read_bytes()
hooks(alternate)
assert receipt.read_bytes() == before
hooks(home / ".claude/settings.json")
assert "hooks_schema" in json.loads(receipt.read_text())
PYBIND
}

# --- displaced user config ---------------------------------------------------

test_displaced_user_config_is_reported() {
  local home="$TMP/displaced"
  local out

  make_fixture "$home" rows 1
  out="$(run_default_doctor "$home")"
  case "$out" in
    *"displaced user config"*)
      printf '%s\n' "$out"
      fail "a HOME with no backups should say nothing about displaced config"
      ;;
  esac
  mkdir -p "$home/.codex" "$home/.gemini"
  printf 'my own rules\n' > "$home/.claude/CLAUDE.md.backup.20260610160231"
  printf 'my own rules\n' > "$home/.codex/AGENTS.md.backup.20260610160231"

  # The default doctor run in a fixture HOME fails for unrelated reasons (no
  # managed targets are linked there), so this asserts on the report line, not
  # on the exit status.
  out="$(run_default_doctor "$home")"

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
    *"capability notion not installed: command ntn (add it: oms install-profile --apply --add --profile notion)"*) ;;
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

test_apparmor_userns_restriction_warns_only_on_linux() {
  local home="$TMP/userns"
  local setting="$TMP/apparmor_restrict_unprivileged_userns"
  local out

  make_fixture "$home" rows 1
  printf '1\n' > "$setting"
  out="$(env -u NVM_DIR HOME="$home" XDG_CONFIG_HOME="$home/.config" \
    OMS_CLAUDE_SETTINGS="$home/.claude/settings.json" \
    OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" \
    OMS_DOCTOR_PROJECT_DIR="$home/repo" OH_MY_SETTING_MODEL_DOCTOR=0 \
    OH_MY_SETTING_CODEX_PLUGIN=0 OH_MY_SETTING_REQUIRE_TOOLS=0 \
    OMS_DOCTOR_USERNS_SYSCTL_PATH="$setting" OMS_PLATFORM_KERNEL=Linux \
    PATH="/usr/bin:/bin" bash "$ROOT/scripts/doctor.sh" 2>&1 || true)"
  case "$out" in
    *"warn: AppArmor restricts unprivileged user namespaces; Codex sandboxed runs may fail (bwrap)."*) ;;
    *) printf '%s\n' "$out"; fail "restricted user namespaces should warn about Codex sandboxed runs" ;;
  esac
  case "$out" in
    *"Allow userns for bwrap in an AppArmor profile"*"kernel.apparmor_restrict_unprivileged_userns=0"*) ;;
    *) printf '%s\n' "$out"; fail "warning should name both standard remedies" ;;
  esac

  printf '0\n' > "$setting"
  out="$(env -u NVM_DIR HOME="$home" XDG_CONFIG_HOME="$home/.config" \
    OMS_CLAUDE_SETTINGS="$home/.claude/settings.json" \
    OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" \
    OMS_DOCTOR_PROJECT_DIR="$home/repo" OH_MY_SETTING_MODEL_DOCTOR=0 \
    OH_MY_SETTING_CODEX_PLUGIN=0 OH_MY_SETTING_REQUIRE_TOOLS=0 \
    OMS_DOCTOR_USERNS_SYSCTL_PATH="$setting" OMS_PLATFORM_KERNEL=Linux \
    PATH="/usr/bin:/bin" bash "$ROOT/scripts/doctor.sh" 2>&1 || true)"
  case "$out" in *"AppArmor restricts unprivileged user namespaces"*) fail "setting 0 must not warn" ;; esac

  printf '1\n' > "$setting"
  out="$(env -u NVM_DIR HOME="$home" XDG_CONFIG_HOME="$home/.config" \
    OMS_CLAUDE_SETTINGS="$home/.claude/settings.json" \
    OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" \
    OMS_DOCTOR_PROJECT_DIR="$home/repo" OH_MY_SETTING_MODEL_DOCTOR=0 \
    OH_MY_SETTING_CODEX_PLUGIN=0 OH_MY_SETTING_REQUIRE_TOOLS=0 \
    OMS_DOCTOR_USERNS_SYSCTL_PATH="$setting" OMS_PLATFORM_KERNEL=Darwin \
    PATH="/usr/bin:/bin" bash "$ROOT/scripts/doctor.sh" 2>&1 || true)"
  case "$out" in *"AppArmor restricts unprivileged user namespaces"*) fail "non-Linux systems must not report this Linux restriction" ;; esac
}

# The scheduled/manual refresh audit is a separate path from a full doctor:
# it must exercise management interfaces, honor opt-outs and avoid inference.
test_post_refresh_integration_audit() {
  local home="$TMP/provider-health" bin="$TMP/provider-health/bin" out listing expected indent
  make_fixture "$home" rows "$EXPECTED_HOOKS_SCHEMA"
  mkdir -p "$bin" "$home/.gemini/config/plugins/oh-my-setting"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OH_MY_SETTING_CODEX_PLUGIN=0 \
    OH_MY_SETTING_MCP=0 OH_MY_SETTING_AGY_PLUGIN=0 bash -c \
    '. "$1/scripts/lib/install-contract.sh"; oms_install_write_receipt "$1"' _ "$ROOT"
  cat > "$bin/claude" <<'EOF_CLI'
#!/usr/bin/env bash
[ "$*" = 'mcp get oh-my-setting' ] || exit 73
printf '%s/scripts/oms-mcp-server.py\n' "${OMS_TEST_MCP_ROOT:-/wrong}"
EOF_CLI
  cat > "$bin/agy" <<'EOF_AGY'
#!/usr/bin/env bash
[ "$*" = 'plugin list' ] || exit 73
cat "$HOME/agy-plugin-list.txt"
EOF_AGY
  printf '%s\n' 'Installed plugins:' '- oh-my-setting (enabled, 0.7.0)' \
    '      Source:     antigravity' '      Components: 1 MCP server' > "$home/agy-plugin-list.txt"
  chmod +x "$bin/claude" "$bin/agy"
  run_health() {
    # NVM_BIN/NVM_DIR would put the host's provider CLIs back on PATH.
    env -u NVM_BIN -u NVM_DIR HOME="$home" XDG_CONFIG_HOME="$home/.config" PATH="${OMS_TEST_HEALTH_PATH:-$bin:/usr/bin:/bin}" \
      OMS_CLAUDE_SETTINGS="$home/.claude/settings.json" \
      OMS_INSTALL_RECEIPT="$home/.config/oh-my-setting/install.json" \
      bash -c '. "$1/scripts/lib/install-contract.sh"; oms_install_check_provider_health "$1"' _ "$ROOT" 2>&1
  }
  out="$(run_health)" || { printf '%s\n' "$out"; fail "receipt opt-outs failed integration audit"; }
  out="$(OH_MY_SETTING_MCP=1 OH_MY_SETTING_AGY_PLUGIN=auto OMS_TEST_HEALTH_PATH=/usr/bin:/bin run_health)" ||
    { printf '%s\n' "$out"; fail "provider-free integration audit failed"; }
  python3 - "$home/.gemini/config/plugins/oh-my-setting/mcp_config.json" "$ROOT" <<'PY_CONFIG'
import json, sys
json.dump({"mcpServers": {"oh-my-setting": {"args": [sys.argv[2] + "/scripts/oms-mcp-server.py"]}}}, open(sys.argv[1], "w"))
PY_CONFIG
  out="$(OH_MY_SETTING_MCP=1 OH_MY_SETTING_AGY_PLUGIN=auto OMS_TEST_MCP_ROOT="$ROOT" run_health)" ||
    { printf '%s\n' "$out"; fail "healthy provider interfaces failed integration audit"; }
  for listing in disabled absent empty unknown truncated duplicate; do
    case "$listing" in
      disabled)
        printf '%s\n' 'Installed plugins:' '- oh-my-setting (disabled, 0.7.0)' > "$home/agy-plugin-list.txt"
        expected='Antigravity plugin is disabled' ;;
      absent)
        printf '%s\n' 'Installed plugins:' '- oh-my-setting-other (enabled, 0.7.0)' > "$home/agy-plugin-list.txt"
        expected='Antigravity plugin is not installed' ;;
      empty)
        printf '%s\n' 'No plugins installed.' > "$home/agy-plugin-list.txt"
        expected='Antigravity plugin is not installed' ;;
      unknown)
        printf '%s\n' 'Installed plugins:' '- oh-my-setting (enabled, 0.7.0)' 'unexpected output' > "$home/agy-plugin-list.txt"
        expected='unrecognized Antigravity plugin list format' ;;
      truncated)
        printf '%s\n' 'Installed plugins:' > "$home/agy-plugin-list.txt"
        expected='unrecognized Antigravity plugin list format' ;;
      duplicate)
        printf '%s\n' 'Installed plugins:' '- oh-my-setting (enabled, 0.7.0)' \
          '- oh-my-setting (disabled, 0.7.0)' > "$home/agy-plugin-list.txt"
        expected='unrecognized Antigravity plugin list format' ;;
    esac
    if out="$(OH_MY_SETTING_AGY_PLUGIN=auto run_health)"; then fail "$listing Antigravity state passed audit"; fi
    case "$out" in *"$expected"*) ;; *) fail "$listing Antigravity failure was not named: $out" ;; esac
  done
  for indent in "" "  " $'\t'; do
    printf 'Installed plugins:\r\n%s- oh-my-setting (enabled, 0.7.0)\r\n' "$indent" > "$home/agy-plugin-list.txt"
    printf '%s\r\n' '      Source:     antigravity' '      Components: 1 MCP server' >> "$home/agy-plugin-list.txt"
    out="$(OH_MY_SETTING_AGY_PLUGIN=auto run_health)" || fail "CRLF/indentation Antigravity list failed: $out"
  done
  if out="$(OH_MY_SETTING_MCP=1 run_health)"; then fail "MCP drift passed the integration audit"; fi
  case "$out" in *"MCP registration does not point at this install"*) ;; *) fail "MCP drift was not named: $out" ;; esac
  printf '{}\n' > "$home/.gemini/config/plugins/oh-my-setting/mcp_config.json"
  if out="$(OH_MY_SETTING_AGY_PLUGIN=auto run_health)"; then fail "Antigravity drift passed the integration audit"; fi
  rm -f "$home/.claude/settings.json"
  if out="$(run_health)"; then fail "missing hooks passed the integration audit"; fi
  case "$out" in *"claude hooks not registered"*) ;; *) fail "hook drift was not named: $out" ;; esac
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
EXPECTED_SURFACES_SHA256 too; command-form changes also require a schema bump.
  EXPECTED_HOOKS_SCHEMA in tests/doctor-surfaces-smoke.sh -> $source_schema
EOF
    exit 1
  fi
}

# `doctor --security` reads each provider's user settings as data and reports
# what widens an agent's reach. Findings only: the exit status stays 0, a file
# it cannot read is unknown rather than ok, values from a settings file are
# never executed, and the only file it writes is its own fingerprint.
test_security_report_names_risky_posture_read_only() {
  local home="$TMP/security" out before
  mkdir -p "$home/.claude" "$home/.codex" "$home/.gemini/antigravity-cli" "$home/bin"
  printf '#!/bin/sh\ntouch "%s/executed"\n' "$home" > "$home/bin/codex"
  cp "$home/bin/codex" "$home/bin/claude"
  chmod +x "$home/bin/codex" "$home/bin/claude"
  security() {
    HOME="$home" XDG_STATE_HOME="$home/state" PATH="$home/bin:/usr/bin:/bin" \
      bash "$ROOT/scripts/doctor.sh" --security 2>&1
  }

  # No settings at all: defaults apply, nothing warns, and a baseline is recorded.
  out="$(security)" || fail "--security must exit 0 on a home without settings: $out"
  contains "$out" "info: claude: no user settings.json; the CLI's defaults apply" || fail "missing claude default row: $out"
  contains "$out" "security: 0 warning(s), 0 unread" || fail "an empty home should have no warnings: $out"
  contains "$out" "this one is the baseline" || fail "the first check should say it is the baseline: $out"
  [ "$(stat -c %a "$home/state/oh-my-setting/doctor-security.json" 2>/dev/null ||
       stat -f %Lp "$home/state/oh-my-setting/doctor-security.json")" = 600 ] ||
    fail "the recorded fingerprint must be private to the user"

  printf '%s\n' '{"permissions":{"defaultMode":"bypassPermissions","allow":["Bash(*)","Read","$(touch '"$home"'/executed)"]},"skipDangerousModePermissionPrompt":true}' \
    > "$home/.claude/settings.json"
  printf 'sandbox_mode = "danger-full-access"\napproval_policy = "never"\n[profiles.safe]\nsandbox_mode = "read-only"\n[sandbox_workspace_write]\nnetwork_access = true\n' \
    > "$home/.codex/config.toml"
  printf '%s\n' '{"permissions":{"allow":["mcp(*)","unsandboxed(npm)"]},"enableTerminalSandbox":false}' \
    > "$home/.gemini/antigravity-cli/settings.json"
  before="$(cat "$home/.claude/settings.json" "$home/.codex/config.toml" "$home/.gemini/antigravity-cli/settings.json")"
  out="$(security)" || fail "warnings are findings and must not change the exit status: $out"
  local row
  for row in \
    "warn: claude: permissions.defaultMode is bypassPermissions" \
    "warn: claude: skipDangerousModePermissionPrompt is on" \
    "warn: claude: permissions.allow grants every shell command (Bash(*))" \
    "info: claude: sandbox.enabled is not on" \
    "warn: codex: sandbox_mode is danger-full-access" \
    "warn: codex: approval_policy is never" \
    "warn: codex: sandbox_workspace_write.network_access is on" \
    "ok: codex profile safe: sandbox_mode is read-only" \
    "warn: antigravity: permissions.allow has a blanket rule (mcp(*))" \
    "warn: antigravity: enableTerminalSandbox is off" \
    "warn: changed since " \
    "security: 9 warning(s), 0 unread; findings only, nothing was changed"; do
    contains "$out" "$row" || fail "--security should report: $row
$out"
  done
  contains "$out" "claude, codex, antigravity" || contains "$out" "antigravity, claude, codex" ||
    fail "the drift row should name the sections that moved: $out"
  [ "$before" = "$(cat "$home/.claude/settings.json" "$home/.codex/config.toml" "$home/.gemini/antigravity-cli/settings.json")" ] ||
    fail "--security must not change a provider file"
  [ ! -e "$home/executed" ] || fail "--security must not run a provider CLI or anything a settings file names"

  # The same posture again is unchanged; a safer one clears its rows.
  out="$(security)"
  contains "$out" "ok: unchanged since " || fail "an unchanged posture should say so: $out"

  # --json carries the same findings with stable ids, the same exit status, and
  # nothing the text mode does not print.
  local json
  json="$(HOME="$home" XDG_STATE_HOME="$home/state" PATH="$home/bin:/usr/bin:/bin" \
    bash "$ROOT/scripts/doctor.sh" --security --json 2>/dev/null)" || fail "--security --json must exit like the text mode: $json"
  printf '%s' "$json" | python3 -c '
import json, sys
doc = json.load(sys.stdin)
assert doc["schema"] == 1 and doc["kind"] == "oms-doctor-security" and doc["exit"] == 0, doc
rows = {f["id"]: f for f in doc["findings"]}
assert len(rows) == len(doc["findings"]), "ids must be unique"
assert rows["claude.permissions.defaultMode"]["level"] == "warn", rows
assert rows["claude.sandbox.enabled"]["level"] == "info", rows  # an info row keeps its own level and is not a warning
assert rows["codex.sandbox_mode"]["level"] == "warn", rows
assert rows["codex.approval_policy"]["message"].startswith("codex: approval_policy is never"), rows
assert rows["codex.profiles.safe.sandbox_mode"]["level"] == "ok", rows
assert rows["antigravity.enableTerminalSandbox"]["level"] == "warn", rows
assert rows["security.drift"]["message"].startswith("unchanged since "), rows
assert doc["changed_since"]["baseline"] is False and doc["changed_since"]["changed"] == [], doc
assert doc["warnings"] == sum(f["level"] == "warn" for f in doc["findings"]) and doc["unread"] == 0, doc
assert "touch" not in json.dumps(doc), "a settings value must not be echoed beyond the text rows"
' || fail "--security --json findings are wrong: $json"
  [ ! -e "$home/executed" ] || fail "--security --json must not run anything a settings file names"
  printf '%s\n' '{"permissions":{"defaultMode":"default","allow":["Read"]},"sandbox":{"enabled":true,"excludedCommands":["docker"]}}' \
    > "$home/.claude/settings.json"
  out="$(security)"
  contains "$out" "ok: claude: permissions.allow has no blanket shell rule (1 rule(s))" || fail "a narrow allow list is ok: $out"
  contains "$out" "ok: claude: sandbox.enabled is on; 1 command(s) excluded from it" || fail "an enabled sandbox is ok: $out"
  contains "$out" "warn: changed since " || fail "a changed posture should be flagged once: $out"
  contains "$out" ": claude" || fail "only the claude section moved: $out"

  # Unreadable is unknown, never ok; control characters in a value cannot reach the terminal.
  printf '{not json' > "$home/.claude/settings.json"
  printf 'sandbox_mode = "x\\u001b[31mred"\n' > "$home/.codex/config.toml"
  out="$(security)" || fail "an unreadable settings file must not fail the report: $out"
  contains "$out" "unknown: claude: settings.json not valid JSON; posture not read" || fail "invalid JSON is unknown: $out"
  contains "$out" "1 unread" || fail "the summary should count the unread file: $out"
  json="$(HOME="$home" XDG_STATE_HOME="$home/state" PATH="$home/bin:/usr/bin:/bin" \
    bash "$ROOT/scripts/doctor.sh" --security --json 2>/dev/null)" || fail "unread settings must not fail --json: $json"
  printf '%s' "$json" | python3 -c '
import json, sys
doc = json.load(sys.stdin)
rows = {f["id"]: f for f in doc["findings"]}
assert rows["claude.settings.json"]["level"] == "unread" and doc["unread"] == 1, doc
assert doc["changed_since"]["baseline"] is False, doc  # the text run just before recorded this state
' || fail "an unreadable file must be level unread in --json: $json"
  case "$json" in *$'\033'*) fail "--json must not carry an escape either" ;; esac
  case "$out" in *$'\033'*) fail "a settings value must not carry an escape to the terminal" ;; esac

  # Profiles beyond the inspection cap must make the result incomplete.
  python3 - "$home/.codex/config.toml" <<'PYPROFILES'
import sys
with open(sys.argv[1], "w") as out:
    for i in range(21):
        out.write("[profiles.p%02d]\n" % i)
        if i == 20:
            out.write('sandbox_mode = "danger-full-access"\n')
PYPROFILES
  out="$(security)"
  contains "$out" "unknown: codex: 1 profile(s) skipped" || fail "skipped profiles must be reported: $out"
  ! contains "$out" "sets no sandbox or approval override" || fail "incomplete inspection cannot be clean: $out"

  # A predictable planted scratch name must not be opened or truncated.
  python3 - "$ROOT/scripts/lib/doctor-security.py" "$home" <<'PYATOMIC' || fail "scratch file was not created safely"
import importlib.util, os, pathlib, sys
spec = importlib.util.spec_from_file_location("security", sys.argv[1])
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
home = pathlib.Path(sys.argv[2]); os.environ["XDG_STATE_HOME"] = str(home / "state")
os.environ.pop("OMS_HARNESS_CHILD", None)
target = home / "target"; target.write_text("preserved")
scratch = pathlib.Path(m.state_path(str(home)) + ".%d.tmp" % os.getpid())
scratch.symlink_to(target)
m.drift([], {}, str(home))
assert target.read_text() == "preserved"
assert scratch.is_symlink()
assert pathlib.Path(m.state_path(str(home))).is_file()
PYATOMIC

  # A harness child may read the report but records nothing.
  rm -f "$home/state/oh-my-setting/doctor-security.json"
  out="$(OMS_HARNESS_CHILD=1 security)" || fail "a harness child may read the report: $out"
  [ ! -e "$home/state/oh-my-setting/doctor-security.json" ] || fail "a harness child must not record a fingerprint"

  # Permission-prompt frequency: counted from the project hook ledger, read-only,
  # informational (no warning), with a hint at the threshold and no ledger written.
  local proj="$TMP/security-proj"
  mkdir -p "$proj/.oms/hooks" "$proj/sub"
  : > "$proj/.oms/hooks/events.jsonl"
  out="$(cd "$proj/sub" && security)" || fail "an empty ledger must not fail: $out"
  contains "$out" "info: permission prompts: none recorded" || fail "an empty ledger says none: $out"
  {
    for n in 1 2 3; do printf '%s\n' '{"action":"permission_prompt","hook":"Notification","session":"aaaaaaaa11","ts":"2026-11-10T01:00:0'"$n"'Z"}'; done
    printf '%s\n' '{"action":"permission_prompt","hook":"Notification","session":"bbbbbbbb22","ts":"2026-11-11T02:00:00Z"}'
    printf '%s\n' '{"action":"telemetry","hook":"Stop","session":"cccccccc33","ts":"2026-11-11T03:00:00Z"}' 'not json'
  } > "$proj/.oms/hooks/events.jsonl"
  before="$(cat "$proj/.oms/hooks/events.jsonl")"
  out="$(cd "$proj/sub" && OMS_PERMISSION_PROMPT_HINT=3 security)" || fail "prompt summary must not fail the report: $out"
  contains "$out" "info: permission prompts: 4 in 2 session(s) over 2 day(s); busiest session aaaaaaaa had 3" ||
    fail "prompt totals are wrong: $out"
  contains "$out" "2026-11-10=3, 2026-11-11=1" || fail "per-day counts are wrong: $out"
  contains "$out" "reached 3 in one session or day (hint at 3)" || fail "the threshold hint is missing: $out"
  contains "$out" "never edits permissions" || fail "the hint must say it edits nothing: $out"
  out="$(cd "$proj/sub" && security)"
  case "$out" in *"hint at"*) fail "no hint below the default threshold: $out" ;; esac
  [ "$before" = "$(cat "$proj/.oms/hooks/events.jsonl")" ] || fail "the prompt summary must not touch the ledger"
  json="$(cd "$proj/sub" && HOME="$home" XDG_STATE_HOME="$home/state" PATH="$home/bin:/usr/bin:/bin" \
    bash "$ROOT/scripts/doctor.sh" --security --json 2>/dev/null)" || fail "--json with prompts failed: $json"
  printf '%s' "$json" | python3 -c '
import json, sys
doc = json.load(sys.stdin)
rows = {f["id"]: f for f in doc["findings"]}
assert rows["security.prompts"]["level"] == "info" and "4 in 2 session(s)" in rows["security.prompts"]["message"], rows
assert "security.prompts.hint" not in rows, rows
' || fail "--security --json prompt rows are wrong: $json"

  # It is a standalone mode.
  if out="$(HOME="$home" bash "$ROOT/scripts/doctor.sh" --security --repair 2>&1)"; then
    fail "--security --repair must be refused"
  fi
  contains "$out" "standalone read-only report" || fail "the refusal should say why: $out"
  if out="$(HOME="$home" bash "$ROOT/scripts/doctor.sh" --security --json --repair 2>&1)"; then
    fail "--security --json --repair must be refused"
  fi
  if out="$(HOME="$home" bash "$ROOT/scripts/doctor.sh" --security --remediation-plan 2>&1)"; then
    fail "--security --remediation-plan must be refused"
  fi
}

# `doctor --sandbox` reports host sandbox facts from files under a test-only
# root prefix and warns when a provider sandbox setting cannot take effect. It
# starts no process unless --probe is given.
test_sandbox_report_states_facts_and_cross_checks_settings() {
  local home="$TMP/sandbox" root="$TMP/sandbox/root" out
  mkdir -p "$home/.claude" "$home/bin" "$root/proc/sys/kernel" "$root/proc/sys/user" "$root/proc/self" "$root/sys/kernel/security"
  printf '#!/bin/sh\ntouch "%s/bwrap-ran"\necho "bwrap: setting up uid map: Permission denied" >&2\nexit 1\n' "$home" > "$home/bin/bwrap"
  chmod +x "$home/bin/bwrap"
  printf '31000\n' > "$root/proc/sys/user/max_user_namespaces"
  printf '1\n' > "$root/proc/sys/kernel/apparmor_restrict_unprivileged_userns"
  printf 'lockdown,capability,yama\n' > "$root/sys/kernel/security/lsm"
  printf 'Name:\tx\nSeccomp:\t2\n' > "$root/proc/self/status"
  printf '6.8.0-test\n' > "$root/proc/sys/kernel/osrelease"
  printf '%s\n' '{"sandbox":{"enabled":true}}' > "$home/.claude/settings.json"
  sandbox() {
    HOME="$home" PATH="${SB_PATH:-$home/bin:/usr/bin:/bin}" OMS_SANDBOX_ROOT="$root" OMS_SANDBOX_OS="${SB_OS:-Linux}" \
      bash "$ROOT/scripts/doctor.sh" --sandbox "$@" 2>&1
  }

  out="$(sandbox)" || fail "--sandbox must exit 0: $out"
  local row
  for row in \
    "available: bwrap: found at $home/bin/bwrap (not executed)" \
    "restricted: apparmor_restrict_userns: value is 1 [/proc/sys/kernel/apparmor_restrict_unprivileged_userns]" \
    "unavailable: landlock: not listed in the LSM list; kernel 6.8.0-test; ABI unknown" \
    "available: seccomp: Seccomp: 2" \
    "warn: claude: sandbox.enabled is on but the sandbox helper is unusable here (apparmor_restrict_userns is restricted)"; do
    contains "$out" "$row" || fail "--sandbox should report: $row
$out"
  done
  [ ! -e "$home/bwrap-ran" ] || fail "--sandbox must not run bwrap without --probe"

  # Restriction off, Landlock listed, no bwrap: the warning is about the missing helper.
  printf '0\n' > "$root/proc/sys/kernel/apparmor_restrict_unprivileged_userns"
  printf 'landlock,yama\n' > "$root/sys/kernel/security/lsm"
  # The system directories may hold a real bwrap: shadow them without it.
  mkdir -p "$home/nobwrap"
  local tool
  for tool in /usr/bin/* /bin/*; do
    [ "${tool##*/}" = bwrap ] || [ -e "$home/nobwrap/${tool##*/}" ] || ln -s "$tool" "$home/nobwrap/${tool##*/}"
  done
  out="$(SB_PATH="$home/nobwrap" sandbox)" || fail "$out"
  contains "$out" "unavailable: bwrap: not on PATH" || fail "missing bwrap: $out"
  contains "$out" "available: landlock: listed in the LSM list" || fail "landlock listed: $out"
  contains "$out" "warn: claude: sandbox.enabled is on but the sandbox helper is unusable here (bwrap is not on PATH)" ||
    fail "a missing bwrap should warn for Claude: $out"
  out="$(sandbox)"
  case "$out" in *"warn: claude"*) fail "a usable helper must not warn: $out" ;; esac

  # --probe is the only thing that runs bwrap, and a failure is a state, not an error.
  out="$(sandbox --probe)" || fail "a failing probe must not change the exit status: $out"
  contains "$out" "unavailable: probe: fails: bwrap: setting up uid map: Permission denied" || fail "probe failure: $out"
  [ -e "$home/bwrap-ran" ] || fail "--probe should run bwrap"
  contains "$out" "warn: claude: sandbox.enabled is on but the sandbox helper is unusable here (the --probe run failed)" ||
    fail "a failed probe should warn: $out"

  # macOS and other systems.
  out="$(SB_OS=Darwin sandbox)" || fail "$out"
  contains "$out" "unavailable: sandbox-exec: not on PATH" || fail "Darwin without sandbox-exec: $out"
  out="$(SB_OS=Plan9 sandbox)" || fail "$out"
  contains "$out" "unavailable: os: unsupported operating system: Plan9" || fail "other OS: $out"

  out="$(sandbox --json)" || fail "$out"
  python3 -I -c 'import json,sys; d=json.loads(sys.argv[1]); assert d["schema"]==1 and d["kind"]=="oms-doctor-sandbox" and d["items"]' "$out" ||
    fail "--json should be schema 1 kind oms-doctor-sandbox: $out"

  # Usage errors exit 2.
  local code=0
  HOME="$home" bash "$ROOT/scripts/doctor.sh" --probe >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "--probe without --sandbox must exit 2, got $code"
  code=0
  HOME="$home" bash "$ROOT/scripts/doctor.sh" --sandbox --security >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "--sandbox --security must exit 2, got $code"
}

test_print_expected_emits_the_registration_list
test_install_converges_owned_rows_only
test_security_report_names_risky_posture_read_only
test_sandbox_report_states_facts_and_cross_checks_settings
test_install_quotes_a_root_with_spaces
test_registration_drift_fails_both_doctor_modes
test_surfaces_expect_commands_at_the_install_owner
test_missing_registration_fails_and_is_named
test_complete_registration_is_ok
test_evidence_window_marks_a_silent_registered_surface
test_stale_receipt_schema_warns
test_install_stamps_hooks_schema_into_the_receipt
test_displaced_user_config_is_reported
test_post_refresh_integration_audit
test_expected_surface_list_is_bump_gated
test_capability_receipt_names_tool_absences
test_receiptless_home_keeps_legacy_wording
test_apparmor_userns_restriction_warns_only_on_linux
test_claude_registry_permissions_warn

echo "doctor-surfaces-smoke: ok"
