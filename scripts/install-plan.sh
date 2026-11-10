#!/usr/bin/env bash
[ "${BASH_VERSINFO[0]:-0}" -ge 5 ] || { echo "error: oh-my-setting needs Bash 5 or newer (running ${BASH_VERSION:-unknown}); on macOS run: brew install bash, then rerun" >&2; exit 1; }
set -euo pipefail

# The write-free preview behind `install.sh --plan` and `oms update --plan`.
# It lists, step by step in install order, what an install or update of THIS
# checkout would change for the current HOME. Nothing is created, moved,
# removed, registered, locked or fetched.
#
# Each step is previewed by the step's own code wherever that code has a
# write-free mode (link.sh --plan, install-profile.sh --plan,
# install-autoupdate.sh --dry-run, provider-permissions.sh --print,
# install-claude-hooks.sh --print-expected). A step with no such mode is
# listed as `not-previewed` with the reason; it is never described from a
# second copy of its logic. Every line is `STEP: STATUS: detail`, STATUS one of
# change, current, skip, not-previewed, note.

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
ROOT="$(cd "$script_dir/.." && pwd -P)"

usage() {
  cat <<'EOF'
Usage: install-plan.sh --install | --update

Print what installing (or updating to) this checkout would change, and change
nothing. Reached through `install.sh --plan` and `oms update --plan`, which
resolve the options and the recorded component profile first and pass them in
the environment (OH_MY_SETTING_*, OMS_PLAN_*).

Lines read `STEP: STATUS: detail`:
  change         the step would write this
  current        already as the step would leave it
  skip           the step is turned off for this run
  not-previewed  the step has no write-free mode; it is named, not guessed
  note           context for reading the plan
Exit 0 when the plan was produced, 2 on usage errors. A step whose own preview
fails is reported as not-previewed and does not fail the plan.
EOF
}

KIND=""
case "${1:-}" in
  --install) KIND=install ;;
  --update) KIND=update ;;
  -h|--help) usage; exit 0 ;;
  *) echo "error: install-plan.sh needs --install or --update" >&2; usage >&2; exit 2 ;;
esac
[ "$#" -eq 1 ] || { echo "error: unexpected argument: $2" >&2; exit 2; }

# A preview must not leave bytecode beside the helpers it runs.
export PYTHONDONTWRITEBYTECODE=1

CHANGES=0
CURRENT=0
UNSEEN=0

emit() {  # STEP STATUS DETAIL
  case "$2" in
    change) CHANGES=$((CHANGES + 1)) ;;
    current) CURRENT=$((CURRENT + 1)) ;;
    not-previewed) UNSEEN=$((UNSEEN + 1)) ;;
  esac
  printf '%s: %s: %s\n' "$1" "$2" "$3"
}

# Classify one line a step's own preview printed.
relay() {  # STEP < lines
  local step="$1" line
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    case "$line" in
      "would "*) emit "$step" change "${line#would }" ;;
      "not previewed: "*) emit "$step" not-previewed "${line#not previewed: }" ;;
      *" current: "*|"kept: "*) emit "$step" current "$line" ;;
      *) emit "$step" note "$line" ;;
    esac
  done
}

# Run a step's preview; its failure is a not-previewed line, never a failed plan.
preview() {  # STEP COMMAND...
  local step="$1" out rc=0
  shift
  out="$("$@" 2>&1)" || rc=$?
  if [ "$rc" -ne 0 ]; then
    emit "$step" not-previewed "its preview exited $rc: $(printf '%s\n' "$out" | tail -n 1)"
    return 0
  fi
  relay "$step" <<< "$out"
}

echo "$KIND plan for $ROOT (home $HOME): nothing is written by this command"

# --- checkout ---------------------------------------------------------------
if [ "$KIND" = update ]; then
  emit checkout not-previewed "fetching ${OH_MY_SETTING_REF:-the configured ref} needs the network and writes the checkout; \`oms update --check\` reports whether it moved. The steps below are previewed for the checkout as it is now"
else
  case "${OMS_PLAN_CHECKOUT:-present}" in
    present) emit checkout not-previewed "fetching ${OH_MY_SETTING_REF:-edge} into $ROOT needs the network and may change the files the steps below read" ;;
    entry) emit checkout not-previewed "${OMS_PLAN_DEST:-the install directory} does not exist yet; it would be cloned at ${OH_MY_SETTING_REF:-edge}. The steps below are previewed from this checkout, so source paths name $ROOT rather than ${OMS_PLAN_DEST:-the install directory}" ;;
  esac
fi

# --- python runtime and tools -----------------------------------------------
emit python-runtime not-previewed "python-runtime.sh ensure may download the locked interpreter and precompile the checkout"
if [ "$KIND" = update ]; then
  case "${OMS_PLAN_TOOLS:-providers}" in
    skip) emit tools skip "tool refresh is off (--no-tools)" ;;
    *) emit tools not-previewed "provider and tool refresh runs each tool's own installer" ;;
  esac
elif [ "${OMS_PLAN_TOOLS_FULL:-0}" = 1 ]; then
  emit tools not-previewed "--full runs install-tools.sh, which installs the whole tool footprint and may add a PATH line to your shell start files"
else
  profile_args=(--plan --primary-provider "${OMS_PLAN_PRIMARY_PROVIDER:-auto}")
  IFS=',' read -r -a plan_capabilities <<< "${OMS_PLAN_CAPABILITIES:-core}"
  for capability in "${plan_capabilities[@]}"; do
    profile_args+=(--profile "$capability")
  done
  tools_plan="" tools_rc=0
  tools_plan="$("$ROOT/scripts/install-profile.sh" "${profile_args[@]}" 2>&1)" || tools_rc=$?
  if [ "$tools_rc" -ne 0 ]; then
    emit tools not-previewed "install-profile.sh --plan exited $tools_rc"
  else
    tools_line="$(printf '%s' "$tools_plan" | python3 -I -c '
import json, sys
try:
    plan = json.load(sys.stdin)
    print("profiles %s; primary provider %s; managed tools: %s" % (
        ",".join(plan.get("expanded") or []) or "-", plan.get("primary_provider") or "-",
        ", ".join(plan.get("managed_tools") or []) or "none"))
except Exception:
    sys.exit(1)
' 2>/dev/null)" || tools_line=""
    if [ -n "$tools_line" ]; then
      emit tools note "$tools_line"
      emit tools not-previewed "whether each managed tool is already installed, and any PATH line its installer adds to a shell start file"
    else
      emit tools not-previewed "install-profile.sh --plan printed a plan this preview could not read"
    fi
  fi
fi

# --- links, rules, skills, dispatcher, receipt ------------------------------
preview links "$ROOT/scripts/link.sh" --plan

# --- services ----------------------------------------------------------------
if [ "$KIND" = install ]; then
  case "${OH_MY_SETTING_CONNECT_SERVICES:-0}" in
    0) emit services skip "no GitHub/Notion login is run" ;;
    *) emit services not-previewed "connect-services.sh (${OH_MY_SETTING_CONNECT_SERVICES}) runs the gh and ntn logins; their credential stores are theirs" ;;
  esac
fi

# --- Claude Code settings ----------------------------------------------------
claude_settings="${OMS_CLAUDE_SETTINGS:-$HOME/.claude/settings.json}"
if [ "${OH_MY_SETTING_CLAUDE_HOOKS:-1}" = 1 ]; then
  hooks_expected="" hooks_rc=0
  hooks_expected="$("$ROOT/scripts/install-claude-hooks.sh" --print-expected 2>/dev/null)" || hooks_rc=$?
  if [ "$hooks_rc" -ne 0 ] || [ -z "$hooks_expected" ]; then
    emit claude-hooks not-previewed "install-claude-hooks.sh --print-expected exited $hooks_rc"
  else
    # The expected rows come from the installer; this only looks each one up in
    # the settings file. The merge itself (ordering, de-duplication, retired
    # rows) is the installer's and is not replayed here.
    hooks_lines="$(OMS_PLAN_EXPECTED="$hooks_expected" python3 -I - "$claude_settings" <<'PY'
import json, os, sys
path = sys.argv[1]
expected = json.loads(os.environ["OMS_PLAN_EXPECTED"])["surfaces"]
state, settings = "missing", {}
try:
    with open(path, encoding="utf-8") as handle:
        settings = json.load(handle)
    state = "read" if isinstance(settings, dict) else "unreadable"
except FileNotFoundError:
    pass
except (OSError, ValueError):
    state = "unreadable"
if state == "unreadable":
    print("not previewed: %s is not readable JSON; the installer decides what it does with it" % path)
    sys.exit(0)
if state == "missing":
    print("would create %s" % path)
have = set()
hooks = settings.get("hooks") if isinstance(settings, dict) else None
if isinstance(hooks, dict):
    for event, groups in hooks.items():
        for group in groups if isinstance(groups, list) else []:
            for hook in (group.get("hooks") if isinstance(group, dict) else None) or []:
                if isinstance(hook, dict) and isinstance(hook.get("command"), str):
                    have.add((event, hook["command"]))
missing = [row for row in expected if (row["event"], row["command"]) not in have]
for row in missing:
    print("would register %s hook %s%s" % (row["event"], row["script"],
                                            " (matcher %s)" % row["matcher"] if row.get("matcher") else ""))
if not missing:
    print("hooks current: all %d expected hook commands are registered in %s" % (len(expected), path))
print("not previewed: the statusLine, subagentStatusLine, modelSettings and attribution keys, "
      "matcher and timeout convergence, and removal of retired hook rows")
PY
)" || hooks_lines="not previewed: could not compare the expected hooks with $claude_settings"
    relay claude-hooks <<< "$hooks_lines"
  fi
elif [ "$KIND" = update ]; then
  emit claude-hooks not-previewed "hooks are off for this install; update runs install-claude-hooks.sh --remove, which drops only OMS rows from $claude_settings"
else
  emit claude-hooks skip "OH_MY_SETTING_CLAUDE_HOOKS=0"
fi

# --- Codex, MCP, Antigravity -------------------------------------------------
codex_mode="${OH_MY_SETTING_CODEX_PLUGIN:-auto}"
if [ "$codex_mode" = auto ]; then
  if command -v codex >/dev/null 2>&1; then codex_mode=1; else codex_mode=0; fi
fi
# Later steps read the resolved value, as they do in a real install.
export OH_MY_SETTING_CODEX_PLUGIN="$codex_mode"
if [ "$codex_mode" = 1 ]; then
  emit codex-plugin not-previewed "the marketplace and plugin registration and the managed block in the Codex config.toml; its own preview (OH_MY_SETTING_DRY_RUN=1 $ROOT/scripts/install-codex-plugin.sh) runs the codex CLI, which this plan does not start"
else
  emit codex-plugin skip "codex plugin registration is off or codex is not on PATH"
fi
if [ "${OH_MY_SETTING_MCP:-1}" = 0 ]; then
  emit mcp skip "OH_MY_SETTING_MCP=0"
else
  emit mcp not-previewed "install-mcp.sh registers the harness MCP server through the claude and codex CLIs"
fi
case "${OH_MY_SETTING_AGY_PLUGIN:-auto}" in
  0) emit agy-plugin skip "OH_MY_SETTING_AGY_PLUGIN=0" ;;
  *)
    if command -v agy >/dev/null 2>&1 || [ "${OH_MY_SETTING_AGY_PLUGIN:-auto}" = 1 ]; then
      emit agy-plugin not-previewed "install-agy-plugin.sh imports the plugin through the agy CLI"
    else
      emit agy-plugin skip "agy is not on PATH"
    fi
    ;;
esac
if [ "${OH_MY_SETTING_PEER_PERMISSIONS:-1}" = 1 ] && command -v agy >/dev/null 2>&1; then
  permission_rules="" permission_rc=0
  permission_rules="$("$ROOT/scripts/provider-permissions.sh" --print --profile delegate --allow-toolchains 2>&1)" || permission_rc=$?
  if [ "$permission_rc" -gt 1 ]; then
    emit permissions not-previewed "provider-permissions.sh --print exited $permission_rc"
  else
    permission_count="$(printf '%s\n' "$permission_rules" | grep -c '(' || true)"
    if [ "$permission_count" -gt 0 ]; then
      emit permissions change "add $permission_count Antigravity permission rule(s); list them with: oms provider-permissions --print --profile delegate --allow-toolchains"
    else
      emit permissions current "no Antigravity permission rule is missing"
    fi
  fi
else
  emit permissions skip "permission grants are off or agy is not on PATH"
fi

# --- snapshots, doctor, auto-update -----------------------------------------
case "${OH_MY_SETTING_GENERATE_SLURM:-0}" in
  1) emit slurm-snapshot change "write the Slurm reference under $ROOT/local" ;;
  auto)
    if command -v sinfo >/dev/null 2>&1; then
      emit slurm-snapshot change "write the Slurm reference under $ROOT/local"
    else
      emit slurm-snapshot skip "sinfo is not on PATH"
    fi
    ;;
  *) emit slurm-snapshot skip "OH_MY_SETTING_GENERATE_SLURM=0" ;;
esac
case "${OH_MY_SETTING_GENERATE_MACHINE:-auto}" in
  0) emit machine-snapshot skip "OH_MY_SETTING_GENERATE_MACHINE=0" ;;
  *) emit machine-snapshot change "rewrite the private machine snapshot $ROOT/local/machine.md (see it with: $ROOT/scripts/write-machine-snapshot.sh --dry-run)" ;;
esac
emit doctor note "doctor runs after the steps above and changes nothing"

case "${OH_MY_SETTING_AUTO_UPDATE:-1}" in
  1)
    autoupdate_args=(--dry-run)
    [ "${OH_MY_SETTING_AUTO_UPDATE_MODE:-}" != check ] || autoupdate_args+=(--check-only)
    preview auto-update "$ROOT/scripts/install-autoupdate.sh" "${autoupdate_args[@]}"
    ;;
  *)
    if [ "$KIND" = update ]; then
      emit auto-update not-previewed "the trigger is off for this install; update runs uninstall-autoupdate.sh, which removes an OMS timer or cron block if one is left"
    else
      emit auto-update skip "OH_MY_SETTING_AUTO_UPDATE=0"
    fi
    ;;
esac

echo "plan: $CHANGES change(s), $CURRENT already current, $UNSEEN step part(s) not previewed; nothing was written"
