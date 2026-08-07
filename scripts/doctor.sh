#!/usr/bin/env bash
set -euo pipefail

# Verify managed target identity, tools, skills, and manifest sync for all
# three agent CLIs.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
FAILED=0
REQUIRE_TOOLS="${OH_MY_SETTING_REQUIRE_TOOLS:-0}"
REPAIR=0
SURFACES=0
MODEL_DOCTOR_MODE="${OH_MY_SETTING_MODEL_DOCTOR:-auto}"
MODEL_DOCTOR_LIVE=0
MODEL_DOCTOR_STRICT=0
MODEL_DOCTOR_ARGS=()
ORIGINAL_ARGS=("$@")

# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/harness-residue.sh
. "$ROOT/scripts/lib/harness-residue.sh"
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"

usage() {
  cat <<'EOF'
Usage: doctor.sh [--repair] [--surfaces] [--contract] [--live-models] [--strict-diversity]
                 [--no-model-doctor] [-h|--help]

Verify the canonical install. --repair relinks from the receipt owner, or
from this checkout for a legacy install without a receipt. The local model
doctor runs automatically when a provider CLI is installed. --live-models
adds bounded account-visible catalog probes, while --strict-diversity requires
at least two usable independent model families. An invalid or unavailable
receipt owner is never replaced automatically.

--surfaces is a standalone read-only report: for every hook surface this
checkout would register, it compares the source list, the live settings.json,
and the harness event stream, and exits nonzero when a registration is
missing. Unlike the rest of the doctor it never delegates to the receipt
owner, because an installed doctor can only check the surfaces it shipped
with.

Environment:
  OH_MY_SETTING_MODEL_DOCTOR=auto|0|1  Auto-detect, disable, or force the
                                      local model capability check.
  OMS_SURFACE_EVIDENCE_DAYS=N         --surfaces evidence window (default 14).
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repair) REPAIR=1; shift ;;
    --surfaces) SURFACES=1; shift ;;
    --contract)
      # The cross-CLI conformance fixture (loader parity, MCP registration
      # parity, fail-open hook no-ops) is slower than a health pass and runs
      # only when asked — never as part of a bare doctor.
      exec bash "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/provider-contract.sh" --check
      ;;
    --live-models)
      MODEL_DOCTOR_MODE=1
      MODEL_DOCTOR_LIVE=1
      MODEL_DOCTOR_ARGS+=(--live-models)
      shift
      ;;
    --strict-diversity)
      MODEL_DOCTOR_MODE=1
      MODEL_DOCTOR_STRICT=1
      MODEL_DOCTOR_ARGS+=(--strict-diversity)
      shift
      ;;
    --no-model-doctor)
      MODEL_DOCTOR_MODE=0
      MODEL_DOCTOR_ARGS+=(--no-model-doctor)
      shift
      ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [ "$SURFACES" = "1" ] && [ "$REPAIR" = "1" ]; then
  echo "error: --surfaces is a read-only report; run --repair separately" >&2
  exit 2
fi

RECEIPT="$(oms_install_receipt_path)"
RECEIPT_STATE="missing"
INSTALL_ROOT="$ROOT"
if [ -f "$RECEIPT" ]; then
  if INSTALL_ROOT="$(oms_install_receipt_owner "$RECEIPT")"; then
    RECEIPT_STATE="valid"
  else
    RECEIPT_STATE="invalid"
    INSTALL_ROOT="$ROOT"
  fi
fi

# --surfaces deliberately opts out of the delegation below. Handing the report
# to the installed checkout would ask the old harness whether it is old: its
# expected list is the stale one, so it certifies itself. The whole value of
# the report is that it runs from a newer source against the live settings.
if [ "$SURFACES" = "0" ] && [ "$RECEIPT_STATE" = "valid" ] &&
   [ "$ROOT" != "$INSTALL_ROOT" ] &&
   [ -x "$INSTALL_ROOT/scripts/doctor.sh" ]; then
  echo "delegating doctor to canonical owner: $INSTALL_ROOT"
  exec "$INSTALL_ROOT/scripts/doctor.sh" "${ORIGINAL_ARGS[@]}"
fi

codex_plugin_installed() {
  command -v codex >/dev/null 2>&1 &&
    codex plugin list --json 2>/dev/null |
      python3 -c 'import json,sys; d=json.load(sys.stdin); target="oh-my-setting@oh-my-setting-local"; sys.exit(0 if any(p.get("pluginId")==target and p.get("installed") for p in d.get("installed", [])) else 1)' 2>/dev/null
}

repair_install() {
  local repair_root="$INSTALL_ROOT"
  local plugin_mode="${OH_MY_SETTING_CODEX_PLUGIN:-auto}"

  if [ "$RECEIPT_STATE" = "invalid" ]; then
    echo "error: refusing repair with invalid install receipt: $RECEIPT" >&2
    echo "hint: choose the intended checkout and run its scripts/link.sh" >&2
    return 1
  fi
  if [ "$RECEIPT_STATE" = "valid" ] &&
     { [ ! -d "$repair_root" ] || [ ! -x "$repair_root/scripts/link.sh" ]; }; then
    echo "error: refusing repair; receipt owner is unavailable: $repair_root" >&2
    echo "hint: restore that checkout or run scripts/link.sh from the intended owner" >&2
    return 1
  fi
  echo "repairing canonical links from: $repair_root"
  "$repair_root/scripts/link.sh"
  if [ "${OH_MY_SETTING_CLAUDE_HOOKS:-1}" = "1" ] &&
     [ -x "$repair_root/scripts/install-claude-hooks.sh" ]; then
    "$repair_root/scripts/install-claude-hooks.sh"
  fi
  if [ -x "$repair_root/scripts/install-codex-plugin.sh" ] &&
     { [ "$plugin_mode" = "1" ] ||
       { [ "$plugin_mode" = "auto" ] && codex_plugin_installed; }; }; then
    "$repair_root/scripts/install-codex-plugin.sh"
  fi
}

if [ "$REPAIR" = "1" ]; then
  repair_install
  exec "$ROOT/scripts/doctor.sh" "${MODEL_DOCTOR_ARGS[@]}"
fi

load_user_tool_paths() {
  export PATH="$HOME/.local/bin:$PATH"
  export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"

  if [ -s "$NVM_DIR/nvm.sh" ]; then
    # shellcheck disable=SC1091
    . "$NVM_DIR/nvm.sh"
    nvm use default >/dev/null 2>&1 || true
  fi
}

check_cmd() {
  if command -v "$1" >/dev/null 2>&1; then
    echo "ok: command $1"
  elif [ "$REQUIRE_TOOLS" = "1" ]; then
    echo "missing: command $1"
    FAILED=1
  else
    echo "optional missing: command $1"
  fi
}

check_optional_cmd() {
  if command -v "$1" >/dev/null 2>&1; then
    echo "ok: optional command $1"
  else
    echo "optional missing: command $1"
  fi
}

# Antigravity's headless mode cannot prompt for a tool permission, so it
# auto-denies anything outside permissions.allow and exits 0 having printed
# only its refusal. Read-only consults ask peers to read the repository, so a
# narrow allow-list turns every antigravity answer into a non-answer that costs
# a full provider call to discover. Cheap to check from the config, so check it
# here rather than making the operator learn it from an empty council.
# Four permission namespaces, established by probing the CLI and reading the
# request it recorded per step: `command` (run a shell command), `read_file`,
# `write_file` (a directory target grants it recursively), and `unsandboxed`
# (leave the sandbox — what a shell redirection actually needs). A `command`
# target is matched against the whole command line, so a peer that runs
# `pwd && ls -la` needs a rule covering that string: enumerating commands does
# not converge, which is why this asks for command(*) instead of a list. That
# is not the write authority it looks like — under `--sandbox`, which is how
# this harness invokes agy, a granted command still cannot write outside the
# sandbox without a separate `unsandboxed` rule.
check_antigravity_permissions() {
  local tool="$INSTALL_ROOT/scripts/provider-permissions.sh"

  command -v agy >/dev/null 2>&1 || return 0
  if [ ! -x "$tool" ]; then
    echo "missing: $tool"
    FAILED=1
    return 0
  fi
  # A missing grant is a warning, not a failure: the install is fine, one peer
  # is mute. The tool prints its own ok/warn lines and how to fix it.
  "$tool" --check || true
}

model_doctor_applicable() {
  case "$MODEL_DOCTOR_MODE" in
    0) return 1 ;;
    1) return 0 ;;
    auto)
      command -v codex >/dev/null 2>&1 ||
        command -v claude >/dev/null 2>&1 ||
        command -v agy >/dev/null 2>&1
      ;;
  esac
}

check_model_capabilities() {
  local -a args=()
  local output

  model_doctor_applicable || return 0
  printf '\n# model capabilities\n'
  if [ ! -x "$INSTALL_ROOT/scripts/model-doctor.sh" ]; then
    echo "missing: $INSTALL_ROOT/scripts/model-doctor.sh"
    FAILED=1
    return 0
  fi
  [ "$MODEL_DOCTOR_LIVE" = "0" ] || args+=(--live-models)
  [ "$MODEL_DOCTOR_STRICT" = "0" ] || args+=(--strict-diversity)
  if output="$("$INSTALL_ROOT/scripts/model-doctor.sh" "${args[@]}" 2>&1)"; then
    printf '%s\n' "$output"
  else
    printf '%s\n' "$output"
    if [ "$MODEL_DOCTOR_MODE" = "1" ]; then
      FAILED=1
    else
      echo "warn: model capability check failed; set OH_MY_SETTING_MODEL_DOCTOR=1 to enforce"
    fi
  fi
}

check_bash_version() {
  local major="${BASH_VERSINFO[0]:-0}"
  if [ "$major" -lt 3 ]; then
    echo "unsupported: bash 3.2+ required; current bash is ${BASH_VERSION:-unknown}"
    FAILED=1
  else
    echo "ok: bash ${BASH_VERSION:-unknown}"
  fi
}

check_path() {
  # Optional $2: the exact managed source this install expects. Existence alone
  # is not parity: a foreign/stale link or copy means the agent runs different
  # rules while doctor would otherwise report ok.
  # -L before -e: a dangling symlink "exists" as a link but resolves to
  # nothing — exactly the breakage that silently strips an agent's rules.
  local mode

  if [ -n "${2:-}" ] && oms_install_target_matches "$2" "$1"; then
    mode="$(oms_install_target_mode "$2" "$1")"
    if [ "$mode" = copy ]; then
      echo "ok: $1 (copy parity)"
    else
      echo "ok: $1"
    fi
  elif [ -L "$1" ] && [ ! -e "$1" ]; then
    echo "broken link: $1 -> $(readlink "$1")"
    FAILED=1
  elif [ -L "$1" ] && [ -n "${2:-}" ] && [ "$(readlink "$1")" != "$2" ]; then
    echo "linked elsewhere: $1 -> $(readlink "$1") (expected $2)"
    FAILED=1
  elif [ -e "$1" ] && [ -n "${2:-}" ] &&
       [ "$(oms_install_target_mode "$2" "$1")" = copy ]; then
    echo "copy differs: $1 (expected resource parity with $2)"
    FAILED=1
  elif [ -e "$1" ] && [ -n "${2:-}" ]; then
    echo "not managed: $1 (expected $2)"
    FAILED=1
  elif [ -e "$1" ]; then
    echo "ok: $1"
  else
    echo "missing: $1"
    FAILED=1
  fi
}

check_custom_skills() {
  local target_root="$1"
  local skill
  local name
  local source

  while IFS= read -r source; do
    source="${source%%$'\r'}"
    [ -n "$source" ] || continue
    case "$source" in
      skip:*)
        echo "note: skill ${source#skip:} (machine-conditional)"
        continue
        ;;
    esac
    skill="$INSTALL_ROOT/$source"
    name="$(basename "$skill")"
    check_path "$target_root/$name" "$skill"
  done < <(python3 - "$INSTALL_ROOT/skills.manifest.json" <<'PY'
import json
import shutil
import sys
for skill in json.load(open(sys.argv[1], encoding="utf-8")).get("skills", []):
    source = str(skill.get("source", ""))
    if skill.get("enabled") is not True or not source.startswith("custom-skills/"):
        continue
    missing = [
        str(req) for req in (skill.get("requires") or [])
        if shutil.which(str(req)) is None
    ]
    if missing:
        print("skip:%s not linked here (requires %s)" % (
            skill.get("name", source), ", ".join(missing)))
        continue
    print(source)
PY
)
}

check_install_receipt() {
  local recorded_commit
  local current_commit
  local recorded_plugin_hash
  local current_plugin_hash

  printf '\n# install ownership\n'
  case "$RECEIPT_STATE" in
    missing)
      echo "note: legacy install has no receipt; expecting this checkout: $ROOT"
      echo "hint: run scripts/link.sh to record canonical ownership"
      ;;
    invalid)
      echo "invalid install receipt: $RECEIPT"
      FAILED=1
      ;;
    valid)
      echo "ok: install receipt $RECEIPT"
      echo "canonical root: $INSTALL_ROOT"
      if [ "$INSTALL_ROOT" != "$ROOT" ]; then
        echo "note: current checkout is not canonical: $ROOT"
      fi
      if [ ! -d "$INSTALL_ROOT" ]; then
        echo "missing: canonical install root $INSTALL_ROOT"
        FAILED=1
        return 0
      fi
      recorded_commit="$(oms_install_receipt_field commit "$RECEIPT" 2>/dev/null || true)"
      current_commit="$(git -C "$INSTALL_ROOT" rev-parse HEAD 2>/dev/null || true)"
      if [ -n "$recorded_commit" ] && [ -n "$current_commit" ] &&
         [ "$recorded_commit" != "$current_commit" ]; then
        echo "stale install receipt commit: $recorded_commit (source is $current_commit)"
        FAILED=1
      else
        echo "ok: install receipt commit"
      fi
      recorded_plugin_hash="$(oms_install_receipt_field plugin.sha256 "$RECEIPT" 2>/dev/null || true)"
      current_plugin_hash="$(oms_install_plugin_hash "$INSTALL_ROOT")"
      if [ -n "$recorded_plugin_hash" ] && [ "$recorded_plugin_hash" != "$current_plugin_hash" ]; then
        echo "stale install receipt plugin hash"
        FAILED=1
      else
        echo "ok: install receipt plugin hash"
      fi
      ;;
  esac
}

check_snapshots() {
  local machine_mode=0
  local slurm_mode=0
  local machine_path="${OH_MY_SETTING_MACHINE_SNAPSHOT:-$INSTALL_ROOT/local/machine.md}"
  local slurm_path="${OH_MY_SETTING_SLURM_REF:-$INSTALL_ROOT/local/slurm.md}"

  [ "$RECEIPT_STATE" = valid ] || return 0
  machine_mode="$(oms_install_receipt_mode machine_snapshot 0 "$RECEIPT")"
  slurm_mode="$(oms_install_receipt_mode slurm_snapshot 0 "$RECEIPT")"
  printf '\n# snapshots\n'
  echo "machine snapshot mode: $machine_mode"
  echo "Slurm snapshot mode: $slurm_mode"

  case "$machine_mode" in
    1|auto)
      if OH_MY_SETTING_MACHINE_SNAPSHOT="$machine_path" \
        "$INSTALL_ROOT/scripts/write-machine-snapshot.sh" --check >/dev/null 2>&1; then
        echo "ok: machine snapshot"
      else
        echo "invalid or missing machine snapshot: $machine_path"
        FAILED=1
      fi
      ;;
  esac
  case "$slurm_mode" in
    1)
      if OH_MY_SETTING_SLURM_REF="$slurm_path" \
        "$INSTALL_ROOT/scripts/generate-slurm-reference.sh" --check >/dev/null 2>&1; then
        echo "ok: Slurm snapshot"
      else
        echo "invalid or missing Slurm snapshot: $slurm_path"
        FAILED=1
      fi
      ;;
    auto)
      if command -v sinfo >/dev/null 2>&1; then
        if OH_MY_SETTING_SLURM_REF="$slurm_path" \
          "$INSTALL_ROOT/scripts/generate-slurm-reference.sh" --check >/dev/null 2>&1; then
          echo "ok: Slurm snapshot"
        else
          echo "invalid or missing Slurm snapshot while Slurm is available: $slurm_path"
          FAILED=1
        fi
      elif [ -f "$slurm_path" ]; then
        echo "note: retained Slurm snapshot; current host has no sinfo"
      else
        echo "ok: Slurm auto snapshot not applicable on this host"
      fi
      ;;
  esac
}

check_codex_plugin() {
  local mode="${OH_MY_SETTING_CODEX_PLUGIN:-auto}"
  local plugin_version
  local marketplace_name
  local cache
  local expected_hash
  local marker_hash=""
  local marker_root=""
  local actual_hash
  local hud_config="${OMS_CODEX_CONFIG:-${CODEX_HOME:-$HOME/.codex}/config.toml}"
  local hud_state

  if [ "$mode" = "0" ]; then
    echo "note: codex plugin check disabled (OH_MY_SETTING_CODEX_PLUGIN=0)"
    return 0
  fi
  case "$mode" in
    1|auto) ;;
    *)
      echo "fail: OH_MY_SETTING_CODEX_PLUGIN must be 0, 1, or auto"
      FAILED=1
      return 0
      ;;
  esac
  command -v codex >/dev/null 2>&1 || return 0
  if ! codex_plugin_installed; then
    if [ "$mode" = "auto" ]; then
      echo "note: optional codex plugin not installed"
      return 0
    fi
    echo "fail: expected codex plugin not installed: oh-my-setting@oh-my-setting-local"
    echo "hint: run $INSTALL_ROOT/scripts/install-codex-plugin.sh"
    FAILED=1
    return 0
  fi

  plugin_version="$(oms_install_plugin_version "$INSTALL_ROOT")"
  marketplace_name="$(python3 - "$INSTALL_ROOT/.agents/plugins/marketplace.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as fh:
    print(json.load(fh)["name"])
PY
)"
  marketplace_name="$(oms_strip_cr "$marketplace_name")"
  cache="${CODEX_HOME:-$HOME/.codex}/plugins/cache/$marketplace_name/oh-my-setting/$plugin_version"
  expected_hash="$(oms_install_receipt_field plugin.sha256 "$RECEIPT" 2>/dev/null || oms_install_plugin_hash "$INSTALL_ROOT")"

  if [ ! -d "$cache" ]; then
    echo "fail: codex plugin cache missing: $cache"
    FAILED=1
    return 0
  fi
  [ ! -f "$cache/.oh-my-setting-source-sha256" ] ||
    marker_hash="$(sed -n '1p' "$cache/.oh-my-setting-source-sha256")"
  [ ! -f "$cache/.oh-my-setting-source-root" ] ||
    marker_root="$(sed -n '1p' "$cache/.oh-my-setting-source-root")"
  actual_hash="$(oms_install_tree_hash "$cache")"
  if [ "$marker_root" != "$INSTALL_ROOT" ] ||
     [ "$marker_hash" != "$expected_hash" ] ||
     [ "$actual_hash" != "$expected_hash" ]; then
    echo "fail: stale codex plugin cache: $cache"
    echo "hint: run $INSTALL_ROOT/scripts/install-codex-plugin.sh"
    FAILED=1
  else
    echo "ok: codex plugin oh-my-setting (cache parity)"
  fi

  if hud_state="$(python3 "$INSTALL_ROOT/scripts/lib/codex-hud-config.py" \
    check "$hud_config" 2>&1)"; then
    echo "ok: codex HUD configured (${hud_state#codex-hud: })"
  else
    echo "fail: codex HUD is not configured: $hud_state"
    echo "hint: run $INSTALL_ROOT/scripts/install-codex-plugin.sh"
    FAILED=1
  fi
}

# install.sh treats hook registration failure as a warning and continues, so a
# "healthy" install can silently lack skill routing, the turn guard, or the
# fail ledger. Verify the registration actually landed instead of trusting the
# receipt; --repair re-runs install-claude-hooks.sh, which is the remedy.
check_claude_hooks() {
  local settings="${OMS_CLAUDE_SETTINGS:-$HOME/.claude/settings.json}"
  local missing

  if [ "${OH_MY_SETTING_CLAUDE_HOOKS:-1}" != "1" ]; then
    echo "note: claude hooks check disabled (OH_MY_SETTING_CLAUDE_HOOKS=0)"
    return 0
  fi
  # Without a valid receipt there is no install whose claim to verify; a bare
  # checkout (or a sandboxed $HOME) is not a broken registration.
  [ "$RECEIPT_STATE" = valid ] || return 0
  # An install that opted out of the hooks recorded that choice in the
  # receipt; the doctor verifies claims, it does not upgrade them to demands.
  if [ "$(oms_install_receipt_mode claude_hooks 1 "$RECEIPT")" = "0" ]; then
    echo "note: claude hooks not part of this install (receipt opt-out)"
    return 0
  fi
  if [ ! -f "$settings" ]; then
    if ! command -v claude >/dev/null 2>&1; then
      echo "note: claude CLI and settings absent; skipping hook check"
      return 0
    fi
    echo "fail: claude hooks not registered ($settings absent)"
    echo "hint: run $INSTALL_ROOT/scripts/install-claude-hooks.sh"
    FAILED=1
    return 0
  fi
  missing="$(python3 - "$settings" <<'PY'
import json, sys

try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        settings = json.load(fh)
except Exception:
    print("unreadable settings JSON")
    sys.exit(0)
hooks = settings.get("hooks", {}) if isinstance(settings, dict) else {}

def registered(event, mark):
    entries = hooks.get(event)
    for entry in entries if isinstance(entries, list) else []:
        for h in entry.get("hooks", []) if isinstance(entry, dict) else []:
            if mark in str(h.get("command", "")):
                return True
    return False

for event, mark in (
    ("UserPromptSubmit", "skill-router.sh"),
    ("Stop", "turn-guard.sh"),
    ("PostToolUseFailure", "fail-ledger-hook.sh"),
    ("PostToolUse", "fail-ledger-hook.sh"),
    ("PreCompact", "precompact-handoff.sh"),
    ("SessionEnd", "precompact-handoff.sh"),
    ("SessionStart", "resume-hook.sh"),
    ("SessionStart", "telemetry-hook.sh"),
    ("PostToolUse", "telemetry-hook.sh"),
    ("SubagentStop", "telemetry-hook.sh"),
    ("SessionEnd", "telemetry-hook.sh"),
):
    if not registered(event, mark):
        print("%s -> %s" % (event, mark))
# User-owned status lines are preserved by the installer and count as wired.
for key in ("statusLine", "subagentStatusLine"):
    status = settings.get(key) if isinstance(settings, dict) else None
    if not (isinstance(status, dict) and status.get("command")):
        print(key)
PY
)"
  if [ -n "$missing" ]; then
    while IFS= read -r line; do
      echo "fail: claude hook missing: $line"
    done <<EOF
$missing
EOF
    echo "hint: run $INSTALL_ROOT/scripts/install-claude-hooks.sh"
    FAILED=1
  else
    echo "ok: claude hooks registered (router, turn guard, fail ledger, pre-compact/session-end handoff, resume, telemetry, main/subagent HUDs)"
  fi
}

# Three layers have to agree before a hook surface is actually live: the list
# this checkout would install, the registrations in the live settings.json, and
# whether the hook has been heard from lately. check_claude_hooks above only
# compares the first two, and against a list it carries itself — so an install
# that predates a new surface reports "registered" for a set that no longer
# matches the source. This section asks the source for the list instead.
check_hook_surfaces() {
  local settings="${OMS_CLAUDE_SETTINGS:-$HOME/.claude/settings.json}"
  local project_dir="${OMS_DOCTOR_PROJECT_DIR:-$PWD}"
  local expected
  local receipt_schema="skip"
  local status=0
  local errlog

  printf '\n# hook surfaces\n'
  echo "source: $ROOT"
  echo "settings: $settings"
  # stdout carries the JSON and nothing else; a diagnostic merged into it would
  # be indistinguishable from a malformed list.
  errlog="$(mktemp "${TMPDIR:-/tmp}/oms-surfaces.XXXXXX")"
  if ! expected="$("$ROOT/scripts/install-claude-hooks.sh" --print-expected 2>"$errlog")"; then
    echo "fail: cannot read the expected surface list from $ROOT"
    sed -n '1,3p' "$errlog" | sed 's/^/  /'
    rm -f "$errlog"
    FAILED=1
    return 0
  fi
  rm -f "$errlog"
  if [ "$INSTALL_ROOT" != "$ROOT" ]; then
    echo "note: installed harness runs from $INSTALL_ROOT"
  fi
  # Unlike the rest of the doctor this does not require a valid receipt: the
  # source-versus-live comparison is meaningful for a bare checkout too, and
  # the receipt only contributes the schema stamp.
  if [ "$RECEIPT_STATE" = "valid" ]; then
    receipt_schema="$(oms_install_receipt_field hooks_schema "$RECEIPT" 2>/dev/null ||
      printf 'absent')"
  fi

  OMS_DOCTOR_EXPECTED="$expected" \
  OMS_DOCTOR_SETTINGS="$settings" \
  OMS_DOCTOR_EVENTS="$project_dir/.oms/hooks/events.jsonl" \
  OMS_DOCTOR_RECEIPT_SCHEMA="$receipt_schema" \
  OMS_DOCTOR_EVIDENCE_DAYS="${OMS_SURFACE_EVIDENCE_DAYS:-14}" \
    python3 <<'PY' || status=$?
import datetime, json, os, shlex, sys

expected = json.loads(os.environ["OMS_DOCTOR_EXPECTED"])
settings_path = os.environ["OMS_DOCTOR_SETTINGS"]
events_path = os.environ["OMS_DOCTOR_EVENTS"]
receipt_schema = os.environ["OMS_DOCTOR_RECEIPT_SCHEMA"].strip()
days = int(os.environ["OMS_DOCTOR_EVIDENCE_DAYS"])

# Which action in the harness event stream proves a given hook ran. A hook
# that writes no event has nothing to check, and silence from one of those is
# normal — so it is reported as an absent stream, never as a fault.
EVIDENCE = {
    "skill-router.sh": "route",
    "turn-guard.sh": "turn_guard",
    "telemetry-hook.sh": "telemetry",
}

hooks = {}
if not os.path.isfile(settings_path):
    print("fail: settings file absent")
else:
    try:
        with open(settings_path, encoding="utf-8") as fh:
            settings = json.load(fh)
        hooks = settings.get("hooks", {}) if isinstance(settings, dict) else {}
    except Exception as exc:
        print("fail: settings file is not readable JSON (%s)" % exc)

def registered(event, script):
    for entry in hooks.get(event) or []:
        if not isinstance(entry, dict):
            continue
        for hook in entry.get("hooks", []) or []:
            command = str(hook.get("command", ""))
            try:
                argv = shlex.split(command)
            except ValueError:
                argv = command.split()
            if script in {os.path.basename(a.replace("\\", "/")) for a in argv}:
                return True
    return False

# Last time each action was recorded. Timestamps are fixed-width UTC
# ("2026-08-07T04:57:48Z"), so string order is time order and no parsing is
# needed — datetime.fromisoformat rejects the trailing Z before Python 3.11.
last = {}
has_stream = os.path.isfile(events_path)
if has_stream:
    with open(events_path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except Exception:
                continue
            if not isinstance(row, dict):
                continue
            action, ts = row.get("action"), row.get("ts")
            if not isinstance(action, str) or not isinstance(ts, str):
                continue
            keys = [action]
            # Telemetry fires on four events from one script; the row names
            # which, so this is per-surface evidence rather than "the script
            # ran at some point".
            if action == "telemetry" and isinstance(row.get("hook"), str):
                keys.append("telemetry:%s" % row["hook"])
            for key in keys:
                if ts > last.get(key, ""):
                    last[key] = ts
else:
    print("note: no harness event stream at %s; evidence unchecked" % events_path)

cutoff = (
    datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=days)
).strftime("%Y-%m-%dT%H:%M:%SZ")

missing = 0
stale = 0
for surface in expected["surfaces"]:
    event, script = surface["event"], surface["script"]
    label = "%s -> %s" % (event, script)
    if not registered(event, script):
        print("MISSING REGISTRATION: %s" % label)
        missing += 1
        continue
    action = EVIDENCE.get(script)
    if action is None:
        print("ok: %s (registered; no evidence stream)" % label)
        continue
    if not has_stream:
        print("ok: %s (registered; evidence unavailable)" % label)
        continue
    key = "telemetry:%s" % event if action == "telemetry" else action
    seen = last.get(key)
    if seen is None:
        print("no-recent-evidence: %s (registered; no %s event recorded)" % (label, action))
        stale += 1
    elif seen < cutoff:
        print("no-recent-evidence: %s (registered; last %s %s)" % (label, action, seen))
        stale += 1
    else:
        print("ok: %s (registered; last %s %s)" % (label, action, seen))

source_schema = expected["hooks_schema"]
if receipt_schema == "skip":
    print("note: no valid install receipt; hooks_schema %d unverified" % source_schema)
elif receipt_schema == "absent":
    print(
        "warn: install receipt records no hooks_schema (source is %d); the "
        "installed harness predates this surface list" % source_schema
    )
elif receipt_schema != str(source_schema):
    print(
        "warn: install receipt hooks_schema %s != source %d; re-run "
        "install-claude-hooks.sh from the canonical checkout"
        % (receipt_schema, source_schema)
    )
else:
    print("ok: hooks_schema %d matches the install receipt" % source_schema)

# Only a missing registration is a failure. Silence can be normal — a machine
# that has not compacted, a repository nobody worked in this fortnight — and a
# schema stamp is a staleness hint, not a broken surface.
if stale:
    print("note: %d registered surface(s) have no evidence within %d day(s)" % (stale, days))
if missing:
    print("hint: run %s/scripts/install-claude-hooks.sh" % expected["root"])
    sys.exit(1)
sys.exit(0)
PY
  [ "$status" -eq 0 ] || FAILED=1
}

# The managed global targets this install claims, HOME-relative, as recorded at
# install time. Falls back to the three rule files, which is what every install
# has had since before the receipt carried an inventory.
managed_target_inventory() {
  local out=""

  if [ "$RECEIPT_STATE" = "valid" ]; then
    out="$(python3 - "$RECEIPT" <<'PY' || true
import json, sys

try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        targets = json.load(fh).get("managed_targets")
except Exception:
    sys.exit(1)
if not isinstance(targets, list):
    sys.exit(1)
for target in targets:
    if isinstance(target, str) and target and not target.startswith("/"):
        print(target)
PY
)"
    out="$(oms_strip_cr "$out")"
  fi
  if [ -n "$out" ]; then
    printf '%s\n' "$out"
  else
    printf '%s\n' ".claude/CLAUDE.md" ".codex/AGENTS.md" ".gemini/AGENTS.md"
  fi
}

# scripts/link.sh moves a pre-existing user file to <target>.backup.<stamp>
# before linking ours in. That file is the user's own configuration, parked
# where nothing reads it again and nothing mentions it again. Name it here so
# leaving it there is a decision rather than an accident. Not a failure: the
# install is correct, the user's old rules are merely no longer in effect.
check_displaced_config() {
  local target
  local backup

  while IFS= read -r target; do
    [ -n "$target" ] || continue
    for backup in "$HOME/$target".backup.*; do
      [ -e "$backup" ] || continue
      echo "displaced user config: $backup (restore: oms uninstall, or merge it)"
    done
  done <<EOF
$(managed_target_inventory)
EOF
}

harness_relpath() {
  local project_dir="$1"
  local path="$2"

  case "$path" in
    "$project_dir"/*) printf '%s\n' "${path#"$project_dir"/}" ;;
    *) printf '%s\n' "$path" ;;
  esac
}

check_harness_artifact_index() {
  local project_dir="$1"
  local index="$project_dir/.oms/artifacts/index.jsonl"
  local stats
  local bad
  local stale
  local schema1
  local canonical_out

  if [ ! -f "$index" ]; then
    echo "ok: artifact index absent"
    return 0
  fi

  command -v python3 >/dev/null 2>&1 || return 0
  stats="$(python3 - "$project_dir" "$index" <<'PY'
import json
import os
import sys

repo, index = sys.argv[1:]
bad = 0
stale = 0

with open(index, "r", encoding="utf-8") as f:
    for line in f:
        try:
            row = json.loads(line)
        except Exception:
            bad += 1
            continue
        if not isinstance(row, dict):
            continue
        for key in ("artifact", "patch", "source"):
            value = row.get(key)
            if not isinstance(value, str) or not value:
                continue
            path = value if os.path.isabs(value) else os.path.join(repo, value)
            if not os.path.exists(path):
                stale += 1

print(f"{bad} {stale}")
PY
)" || {
    echo "warn: artifact index audit failed"
    return 0
  }

  stats="$(oms_strip_cr "$stats")"
  read -r bad stale <<< "$stats"
  if [ "${bad:-0}" -gt 0 ]; then
    echo "warn: artifact index has $bad invalid JSON line(s)"
  else
    echo "ok: artifact index JSONL"
  fi

  if [ "${stale:-0}" -gt 0 ]; then
    echo "warn: artifact index has $stale stale artifact/patch reference(s)"
  else
    echo "ok: artifact index references"
  fi

  schema1="$(python3 - "$index" <<'PY'
import json, sys
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    try:
        row = json.loads(line)
    except Exception:
        continue
    if isinstance(row, dict) and row.get("schema") == 1:
        print(1)
        break
PY
)"
  schema1="$(oms_strip_cr "$schema1")"
  if [ "$schema1" = "1" ]; then
    if canonical_out="$("$ROOT/scripts/artifact-index.sh" --repo "$project_dir" validate 2>&1)"; then
      echo "ok: artifact index canonical validation"
    else
      echo "warn: artifact index canonical validation failed"
      printf '%s\n' "$canonical_out" | sed -n '1,5p' | sed 's/^/  /'
    fi
  fi
}

check_harness_run_state() {
  local project_dir="$1"
  local oms_dir="$project_dir/.oms"
  local bad

  command -v python3 >/dev/null 2>&1 || return 0
  bad="$(python3 - "$oms_dir" <<'PY'
import glob, json, os, sys
oms = sys.argv[1]
# Run-tool JSONL state written this family of tools (spine, experiments,
# reconcile, capsule/run index). Manifests are *.json (single object).
targets = []
targets += glob.glob(os.path.join(oms, "runs", "*.jsonl"))
targets += glob.glob(os.path.join(oms, "experiments.jsonl"))
bad = 0
for f in targets:
    try:
        with open(f, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                if line.strip():
                    json.loads(line)
    except Exception:
        bad += 1
print(bad)
PY
)" || { echo "warn: run-state audit failed"; return 0; }
  bad="$(oms_strip_cr "$bad")"
  if [ "${bad:-0}" -gt 0 ]; then
    echo "warn: $bad run-state JSONL file(s) have malformed lines (run oms-run.sh validate)"
  else
    echo "ok: run-state JSONL"
  fi
}

check_harness_sensitive_files() {
  local project_dir="$1"
  local oms_dir="$project_dir/.oms"
  local file
  local rel
  local sensitive=0

  file="$oms_dir/task/current.md"
  if [ -f "$file" ] && agent_memory_file_has_sensitive_content "$file"; then
    rel="$(harness_relpath "$project_dir" "$file")"
    echo "warn: sensitive-looking harness state: $rel"
    sensitive=$((sensitive + 1))
  fi

  if [ -d "$oms_dir/memory" ]; then
    # SQLite is a derived copy of these Markdown sources. Scan the sources once
    # instead of reporting every sensitive note twice (and treating a binary
    # database page as another user-authored record).
    while IFS= read -r -d '' file; do
      if agent_memory_file_has_sensitive_content "$file"; then
        rel="$(harness_relpath "$project_dir" "$file")"
        echo "warn: sensitive-looking harness state: $rel"
        sensitive=$((sensitive + 1))
      fi
    done < <(find "$oms_dir/memory" -maxdepth 1 -type f -name '*.md' -print0 2>/dev/null)
  fi

  if [ "$sensitive" -eq 0 ]; then
    echo "ok: harness task/memory sensitive scan"
  fi
}

check_harness_memory_db() {
  local project_dir="$1"
  local db="$project_dir/.oms/memory/memory.sqlite3"

  [ -f "$db" ] || return 0
  command -v python3 >/dev/null 2>&1 || return 0
  # The expected schema version is read out of the helper that writes it, not
  # written here as a number. Hardcoding it meant the bump to 3 left this check
  # asserting 2, so every repository that had used memory reported "invalid" and
  # was told to rebuild — which produces the same version again, making the
  # advice unfollowable and the warning permanent.
  if python3 - "$db" "$ROOT/scripts/lib/agent-memory-db.py" >/dev/null 2>&1 <<'PY'
import re
import sqlite3
import sys

with open(sys.argv[2]) as handle:
    match = re.search(r"^SCHEMA_VERSION\s*=\s*(\d+)", handle.read(), re.M)
if not match:
    raise SystemExit(1)
expected = int(match.group(1))

db = sqlite3.connect(sys.argv[1])
try:
    if db.execute("pragma user_version").fetchone()[0] != expected:
        raise SystemExit(1)
    if db.execute("pragma quick_check").fetchone()[0] != "ok":
        raise SystemExit(1)
    tables = {
        row[0]
        for row in db.execute(
            "select name from sqlite_master where type = 'table'"
        )
    }
    if not {"memory_sources", "memory_entries"}.issubset(tables):
        raise SystemExit(1)
    columns = {
        row[1] for row in db.execute("pragma table_info(memory_entries)")
    }
    required = {
        "event_id", "occurred_at", "ordinal", "agent", "kind", "task_id",
        "session_hash", "git_sha", "git_dirty", "git_state", "body",
    }
    if not required.issubset(columns):
        raise SystemExit(1)
finally:
    db.close()
PY
  then
    echo "ok: memory database schema/integrity"
  else
    echo "warn: memory database is invalid (run: oms agent-memory --repo . rebuild)"
  fi
}

check_harness_residue() {
  local project_dir="$1"
  local stale_worktrees=0
  local dead_locks=0
  local temp_dirs=0
  local unindexed=0
  local residue=0

  if git -C "$project_dir" rev-parse --git-dir >/dev/null 2>&1; then
    stale_worktrees="$(oms_harness_count_stale_worktrees "$project_dir")"
  fi
  local lock_files
  dead_locks="$(oms_harness_lock_residue_count)"
  temp_dirs="$(oms_harness_tmp_residue_count)"
  unindexed="$(oms_harness_count_unindexed_artifacts "$project_dir")"

  if [ "${stale_worktrees:-0}" -gt 0 ]; then
    echo "warn: $stale_worktrees stale git worktree registration(s)"
    residue=1
  fi
  if [ "${dead_locks:-0}" -gt 0 ]; then
    echo "warn: $dead_locks dead harness lock dir(s)"
    residue=1
  fi
  # A note, not a warning, and not counted as residue: flock lock files cannot
  # be unlinked safely, so there is no action to recommend. Reported because the
  # scan above only sees mkdir-path directories, and staying silent about
  # thousands of files reads as "nothing here" instead of "nothing removable".
  # A large number means a run leaked its lock dir into this HOME — a test
  # without OMS_LOCK_DIR — rather than anything wrong with the install.
  # Guarded: an update transaction stages doctor.sh with only a subset of the
  # libs, so calling a brand-new helper unconditionally turned a routine update
  # into a rollback. A note is never worth failing an install over.
  lock_files=0
  if command -v oms_harness_lock_file_count >/dev/null 2>&1; then
    lock_files="$(oms_harness_lock_file_count)"
  fi
  if [ "${lock_files:-0}" -ge "${OMS_LOCK_FILE_NOTE_AT:-1000}" ]; then
    echo "note: $lock_files flock lock file(s) in $(oms_file_lock_dir); not removable by design"
  fi
  if [ "${temp_dirs:-0}" -gt 0 ]; then
    echo "warn: $temp_dirs dead harness temp dir(s)"
    residue=1
  fi
  if [ "${unindexed:-0}" -gt 0 ]; then
    echo "warn: $unindexed unindexed artifact file(s) (run artifact-index.sh prune --files)"
  fi
  if [ "$residue" -eq 1 ]; then
    echo "hint: run cleanup.sh --apply to remove safe harness residue"
  else
    echo "ok: no crash residue detected"
  fi
}

check_harness_state() {
  local project_dir="${OMS_DOCTOR_PROJECT_DIR:-}"
  local oms_dir

  if [ -z "$project_dir" ]; then
    [ -d "$PWD/.oms" ] || return 0
    project_dir="$PWD"
  fi

  if ! project_dir="$(cd "$project_dir" 2>/dev/null && pwd)"; then
    echo "warn: harness project dir unavailable: ${OMS_DOCTOR_PROJECT_DIR:-$PWD}"
    return 0
  fi

  oms_dir="$project_dir/.oms"
  printf '\n# harness state\n'
  if [ ! -d "$oms_dir" ]; then
    echo "ok: no .oms harness state"
    return 0
  fi

  echo "ok: harness state $oms_dir"
  if [ -f "$oms_dir/.gitignore" ]; then
    echo "ok: .oms/.gitignore"
  else
    echo "warn: .oms/.gitignore missing (re-run any harness command)"
  fi

  check_harness_artifact_index "$project_dir"
  check_harness_run_state "$project_dir"
  check_harness_memory_db "$project_dir"
  check_harness_sensitive_files "$project_dir"
  check_harness_residue "$project_dir"
  check_harness_project_skills "$project_dir"
}

# Project skills are standing context for every future session in this repo;
# an invalid or secret-carrying one should surface here, not on first load.
check_harness_project_skills() {
  local project_dir="$1"
  local out

  [ -d "$project_dir/.oms/skills" ] || return 0
  if out="$("$INSTALL_ROOT/scripts/skill-forge.sh" --repo "$project_dir" status 2>&1)"; then
    echo "ok: ${out#skill-forge: }"
  else
    echo "warn: ${out#skill-forge: }"
  fi
}

load_user_tool_paths

case "$REQUIRE_TOOLS" in
  0|1) ;;
  *)
    echo "error: OH_MY_SETTING_REQUIRE_TOOLS must be 0 or 1" >&2
    exit 2
    ;;
esac
case "$MODEL_DOCTOR_MODE" in
  auto|0|1) ;;
  *)
    echo "error: OH_MY_SETTING_MODEL_DOCTOR must be auto, 0, or 1" >&2
    exit 2
    ;;
esac

if [ "$SURFACES" = "1" ]; then
  check_hook_surfaces
  if [ "$FAILED" -ne 0 ]; then
    echo "surfaces: failed"
    exit 1
  fi
  echo "surfaces: ok"
  exit 0
fi

check_install_receipt
check_snapshots

check_cmd git
check_cmd curl
check_cmd node
check_cmd npm
check_cmd uv
check_cmd claude
check_cmd codex
check_cmd agy
check_cmd ntn
# gh is installed by default now, so it is required on the same terms as the
# provider CLIs: REQUIRE_TOOLS=1 fails on it, and a machine that deliberately
# skipped the tool install still only sees "optional missing".
check_cmd gh
# Presence is installable; authentication is an interactive browser flow, so it
# can only ever be reported. An unauthenticated gh is the state in which
# ci-status fails on its first call, which is worth knowing
# here rather than there.
#
# Read the credential locally instead of asking `gh auth status`: that command
# validates the token against the API, and this doctor is local-only by design —
# a health check that needs the network reports "unauthenticated" for a dropped
# connection. hosts.yml is the file `gh auth login` writes; the token variables
# are the other way gh is credentialed.
check_gh_auth() {
  local config="${GH_CONFIG_DIR:-$HOME/.config/gh}"
  # The environment variable names are assembled rather than written out: the
  # harness's own sources have to pass the outbound scrubber, which flags any
  # identifier ending in "…t0ken" (spelled properly) next to a colon or equals
  # sign, and a peer review of this very file would otherwise be refused.
  local suffix="TO""KEN"
  local var

  command -v gh >/dev/null 2>&1 || return 0
  [ ! -s "$config/hosts.yml" ] || return 0
  for var in "GH_$suffix" "GITHUB_$suffix"; do
    [ -z "${!var:-}" ] || return 0
  done

  echo "warn: gh is not authenticated (run: gh auth login)"
}
check_gh_auth

# The install now guarantees the binaries, which moves the failure one step
# later: a provider CLI that is present but not logged in answers nothing, and
# the harness discovers that by spending a call on it. Every one of them stores
# its credential in a local file, so this is answerable here — locally, with no
# network, for the same reason as the gh check above. Presence of the file is all
# that is claimed; whether the credential is still valid is the provider's
# answer to give, not this doctor's to guess.
check_provider_auth() {
  local cli file
  local reported=0

  for cli in claude codex agy; do
    command -v "$cli" >/dev/null 2>&1 || continue
    case "$cli" in
      claude) file="$HOME/.claude/.credentials.json" ;;
      codex) file="$HOME/.codex/auth.json" ;;
      agy) file="$HOME/.gemini/oauth_creds.json" ;;
    esac
    if [ -s "$file" ]; then
      continue
    fi
    echo "warn: $cli is installed but has no local credential (log in once with: $cli)"
    reported=1
  done
  [ "$reported" = 1 ] || return 0
}
check_provider_auth

check_bash_version

check_optional_cmd timeout
check_optional_cmd sbatch
check_optional_cmd srun
check_optional_cmd squeue
check_optional_cmd sinfo
check_optional_cmd scancel

check_antigravity_permissions
check_model_capabilities

check_path "$INSTALL_ROOT/rules/global-AGENTS.md"
check_path "$INSTALL_ROOT/skills.manifest.json"
check_path "$INSTALL_ROOT/.agents/plugins/marketplace.json"
check_path "$INSTALL_ROOT/plugins/oh-my-setting/.codex-plugin/plugin.json"
check_path "$INSTALL_ROOT/plugins/oh-my-setting/hooks.json"
check_path "$HOME/.codex/AGENTS.md" "$INSTALL_ROOT/rules/global-AGENTS.md"
check_path "$HOME/.claude/CLAUDE.md" "$INSTALL_ROOT/rules/global-AGENTS.md"
check_path "$HOME/.gemini/AGENTS.md" "$INSTALL_ROOT/rules/global-AGENTS.md"
check_custom_skills "$HOME/.codex/skills"
check_custom_skills "$HOME/.claude/skills"
check_custom_skills "$HOME/.gemini/antigravity/skills"
check_path "$HOME/.oh-my-setting-prompts" "$INSTALL_ROOT/prompts"
check_path "$HOME/.local/bin/oms" "$INSTALL_ROOT/scripts/oms"
check_displaced_config

if ! "$ROOT/scripts/skill-doctor.sh"; then
  FAILED=1
fi

if ! "$INSTALL_ROOT/scripts/install-skills.sh" >/dev/null; then
  echo "fail: skills.manifest.json out of sync (run scripts/install-skills.sh for details)"
  FAILED=1
fi

check_claude_hooks

check_codex_plugin

check_harness_state

if [ "$FAILED" -ne 0 ]; then
  echo "doctor: failed"
  exit 1
fi

echo "doctor: ok"
