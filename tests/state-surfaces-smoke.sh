#!/usr/bin/env bash
set -euo pipefail

# Smoke tests for the harness-state surfaces added around the MCP server:
# the read-only oms MCP server itself, its claude/codex registration, the
# Antigravity plugin packaging, the router's state-conditional hints, and
# the generate-slurm-reference rename.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-state-surfaces.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

make_repo() {
  local dir="$1"
  mkdir -p "$dir"
  git -C "$dir" init -q
  mkdir -p "$dir/.oms"
  printf '*\n' > "$dir/.oms/.gitignore"
}

# --- MCP server protocol ----------------------------------------------------

test_mcp_server_protocol() {
  local repo="$TMP/mcp-repo"
  local out="$TMP/mcp-out"

  make_repo "$repo"
  {
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}'
    printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
    printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
    printf '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"oms_task_state","arguments":{"repo":"%s"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"oms_handoff_show","arguments":{"repo":"%s","file":"../escape"}}}\n' "$repo"
    printf '%s\n' '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope","arguments":{}}}'
  } | python3 "$ROOT/scripts/oms-mcp-server.py" > "$out"

  OMS_T_OUT="$out" python3 - <<'PY' || fail "MCP protocol exchange did not match the contract"
import json, os

by_id = {}
with open(os.environ["OMS_T_OUT"], encoding="utf-8") as fh:
    for line in fh:
        msg = json.loads(line)
        by_id[msg.get("id")] = msg

init = by_id[1]["result"]
assert init["protocolVersion"] == "2026-03-26", init
assert init["serverInfo"]["name"] == "oh-my-setting", init
tools = {t["name"] for t in by_id[2]["result"]["tools"]}
assert {"oms_task_state", "oms_fail_ledger", "oms_handoffs",
        "oms_handoff_show", "oms_journal"} <= tools, tools
task = by_id[3]["result"]
assert not task["isError"], task
assert json.loads(task["content"][0]["text"])["schema"] == 1, task
guard = by_id[4]["result"]
assert guard["isError"], guard
assert "bare digest file name" in guard["content"][0]["text"], guard
assert by_id[5]["error"]["code"] == -32602, by_id[5]
PY
}

# --- claude/codex registration ---------------------------------------------

test_install_mcp_registers_and_is_idempotent() {
  local bin="$TMP/mcp-bin"
  local log="$TMP/mcp-log"

  mkdir -p "$bin"
  : > "$log"
  # Stubs: `mcp get` succeeds (printing the registered command) only after a
  # marker that `mcp add` writes — the real CLIs' visible contract.
  local cli
  for cli in claude codex; do
    cat > "$bin/$cli" <<EOF
#!/usr/bin/env bash
echo "$cli \$*" >> "$log"
case "\$1 \$2" in
  "mcp get")
    [ -f "$TMP/$cli.registered" ] || exit 1
    cat "$TMP/$cli.registered"
    ;;
  "mcp add")
    printf '%s\n' "\$*" > "$TMP/$cli.registered"
    ;;
  "mcp remove") rm -f "$TMP/$cli.registered" ;;
esac
exit 0
EOF
    chmod +x "$bin/$cli"
  done

  PATH="$bin:/usr/bin:/bin" bash "$ROOT/scripts/install-mcp.sh" > "$TMP/mcp-first" ||
    fail "install-mcp should succeed with both CLIs present"
  grep -Fq "mcp: claude registered" "$TMP/mcp-first" || fail "claude was not registered"
  grep -Fq "mcp: codex registered" "$TMP/mcp-first" || fail "codex was not registered"
  grep -Fq "scripts/oms-mcp-server.py" "$TMP/claude.registered" ||
    fail "claude registration does not point at the server"

  PATH="$bin:/usr/bin:/bin" bash "$ROOT/scripts/install-mcp.sh" > "$TMP/mcp-second" ||
    fail "re-registration should succeed"
  grep -Fq "mcp: claude already registered" "$TMP/mcp-second" ||
    fail "second run should detect the existing claude registration"

  PATH="$bin:/usr/bin:/bin" bash "$ROOT/scripts/install-mcp.sh" --remove >/dev/null ||
    fail "removal should succeed"
  [ ! -f "$TMP/claude.registered" ] || fail "claude registration was not removed"
  [ ! -f "$TMP/codex.registered" ] || fail "codex registration was not removed"

  # No CLIs on PATH: notes, exit 0.
  PATH="/usr/bin:/bin" bash "$ROOT/scripts/install-mcp.sh" > "$TMP/mcp-none" ||
    fail "missing CLIs must be notes, not failures"
  grep -Fq "claude CLI absent" "$TMP/mcp-none" || fail "missing claude should be noted"
}

# --- antigravity plugin -----------------------------------------------------

test_install_agy_plugin_bakes_absolute_paths() {
  local bin="$TMP/agy-bin"
  local log="$TMP/agy-log"
  local capture="$TMP/agy-plugin-copy"

  mkdir -p "$bin" "$capture"
  cat > "$bin/agy" <<EOF
#!/usr/bin/env bash
echo "agy \$*" >> "$log"
if [ "\$1 \$2" = "plugin install" ]; then
  cp "\$3"/* "$capture/"
fi
exit 0
EOF
  chmod +x "$bin/agy"

  PATH="$bin:/usr/bin:/bin" bash "$ROOT/scripts/install-agy-plugin.sh" > "$TMP/agy-out" ||
    fail "agy plugin install should succeed with agy present"
  grep -Fq "agy-plugin: installed" "$TMP/agy-out" || fail "install was not reported"
  [ -f "$capture/plugin.json" ] || fail "plugin.json was not built"
  [ -f "$capture/mcp_config.json" ] || fail "mcp_config.json was not built"
  OMS_T_DIR="$capture" OMS_T_ROOT="$ROOT" python3 - <<'PY' || fail "generated plugin content is wrong"
import json, os
d = os.environ["OMS_T_DIR"]
root = os.environ["OMS_T_ROOT"]
plugin = json.load(open(os.path.join(d, "plugin.json")))
assert plugin["name"] == "oh-my-setting", plugin
assert plugin["version"] not in ("", "0.0.0"), plugin
mcp = json.load(open(os.path.join(d, "mcp_config.json")))
server = mcp["mcpServers"]["oh-my-setting"]
assert server["command"] == "python3", server
assert server["args"] == [os.path.join(root, "scripts", "oms-mcp-server.py")], server
PY

  PATH="$bin:/usr/bin:/bin" bash "$ROOT/scripts/install-agy-plugin.sh" --remove >/dev/null ||
    fail "plugin removal should succeed"
  grep -Fq "agy plugin uninstall oh-my-setting" "$log" ||
    fail "removal should call agy plugin uninstall"

  PATH="$bin:/usr/bin:/bin" OH_MY_SETTING_AGY_PLUGIN=0 \
    bash "$ROOT/scripts/install-agy-plugin.sh" > "$TMP/agy-skip" ||
    fail "explicit skip should exit 0"
  grep -Fq "skipped" "$TMP/agy-skip" || fail "skip should be reported"

  PATH="/usr/bin:/bin" bash "$ROOT/scripts/install-agy-plugin.sh" > "$TMP/agy-absent" ||
    fail "absent agy in auto mode must be a note"
  grep -Fq "agy CLI absent" "$TMP/agy-absent" || fail "absent agy should be noted"
}

# --- router state hints -----------------------------------------------------

test_router_state_hint_on_unresolved_failures() {
  local repo="$TMP/hint-repo"
  local payload out

  make_repo "$repo"
  ( cd "$repo" &&
    bash "$ROOT/scripts/fail-ledger.sh" record --cmd "make test" --exit 1 >/dev/null &&
    OMS_ADVISE_AFTER_FAILURES=0 bash "$ROOT/scripts/fail-ledger.sh" record \
      --cmd "python train.py" --exit 2 >/dev/null )

  payload='{"prompt":"continue the work","session_id":"s","turn_id":"t"}'
  out="$(printf '%s' "$payload" |
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  printf '%s' "$out" | grep -Fq "unresolved fail-ledger rows" ||
    fail "two unresolved failures should produce a state hint: $out"

  # Same day: the marker suppresses a second hint.
  out="$(printf '%s' "$payload" |
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  if printf '%s' "$out" | grep -Fq "unresolved fail-ledger rows"; then
    fail "the state hint must fire at most once per day"
  fi

  # Opt-out silences it even on a fresh day marker.
  rm -f "$repo/.oms/hooks/state-hint."*
  out="$(printf '%s' "$payload" | OMS_STATE_REPO="$repo" OMS_STATE_HINTS=0 \
    TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  if printf '%s' "$out" | grep -Fq "unresolved fail-ledger rows"; then
    fail "OMS_STATE_HINTS=0 must disable the hint"
  fi
}

test_router_state_hint_skips_unadopted_repo() {
  local repo="$TMP/hint-plain"
  mkdir -p "$repo"
  local out
  out="$(printf '{"prompt":"hello","session_id":"s","turn_id":"t"}' |
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  [ ! -d "$repo/.oms" ] || fail "the hint must not create .oms in a plain directory"
}

# --- machine-conditional skills ---------------------------------------------

# A deterministic PATH: real tools by symlink, nothing else — so a command's
# absence can be asserted even on machines that actually have it.
make_toolbox() {
  local dir="$1"
  local cmd path

  mkdir -p "$dir"
  for cmd in bash sh python3 git ln mkdir rm rmdir mv cp ls basename dirname \
    grep sed find sort tr cat printf echo date mktemp chmod readlink id \
    uname wc head tail awk cmp touch env cut cksum sha256sum flock xargs \
    sleep; do
    path="$(command -v "$cmd" 2>/dev/null)" || continue
    ln -sf "$path" "$dir/$cmd"
  done
}

test_conditional_skills_link_only_where_required_commands_exist() {
  local toolbox="$TMP/cond-toolbox"
  local stub="$TMP/cond-stub"
  local home="$TMP/cond-home"
  local out

  make_toolbox "$toolbox"
  mkdir -p "$stub" "$home"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$stub/sinfo"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$stub/nvidia-smi"
  chmod +x "$stub/sinfo" "$stub/nvidia-smi"

  # Without the commands: base skills link, conditional skills do not.
  PATH="$toolbox" HOME="$home" bash "$ROOT/scripts/link.sh" >/dev/null
  [ -e "$home/.codex/skills/agent-harness" ] || fail "base skill missing"
  [ ! -e "$home/.codex/skills/slurm" ] ||
    fail "slurm skill linked without sinfo"
  [ ! -e "$home/.codex/skills/gpu-workstation" ] ||
    fail "gpu-workstation linked without nvidia-smi"

  # With the commands: both conditional skills appear.
  PATH="$stub:$toolbox" HOME="$home" bash "$ROOT/scripts/link.sh" >/dev/null
  [ -e "$home/.codex/skills/slurm" ] || fail "slurm skill not linked with sinfo"
  [ -e "$home/.codex/skills/gpu-workstation" ] ||
    fail "gpu-workstation not linked with nvidia-smi"

  # Machine lost the commands: the next link converges by removing them.
  out="$(PATH="$toolbox" HOME="$home" bash "$ROOT/scripts/link.sh")"
  [ ! -e "$home/.codex/skills/slurm" ] ||
    fail "slurm link should be removed once sinfo is gone"
  printf '%s' "$out" | grep -Fq "unlinked disabled skill" ||
    fail "removal should be reported: $out"
}

test_router_skips_conditional_skill_without_command() {
  local toolbox="$TMP/router-toolbox"
  local stub="$TMP/router-stub"
  local repo="$TMP/router-cond-repo"
  local payload out

  make_toolbox "$toolbox"
  mkdir -p "$stub" "$repo"
  git -C "$repo" init -q
  printf '#!/usr/bin/env bash\nexit 0\n' > "$stub/sinfo"
  chmod +x "$stub/sinfo"
  payload='{"prompt":"sbatch로 잡 제출하고 squeue 봐줘","session_id":"s","turn_id":"t"}'

  out="$(printf '%s' "$payload" | PATH="$stub:$toolbox" \
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  printf '%s' "$out" | grep -Fq "slurm" ||
    fail "router should suggest slurm when sinfo exists: $out"

  out="$(printf '%s' "$payload" | PATH="$toolbox" \
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  if printf '%s' "$out" | grep -Fq "slurm"; then
    fail "router must not suggest slurm without sinfo: $out"
  fi
}

# --- project skill forge ----------------------------------------------------

test_skill_forge_stores_links_and_hides() {
  local repo="$TMP/forge repo"
  local original="$TMP/original-skill"
  local out

  make_repo "$repo"
  cat <<'EOF' | bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" add --name build-quirks
---
name: build-quirks
description: How this repository actually builds and tests, including the nonstandard invocations discovered by inspection rather than guessed.
---

# Build Quirks

- Tests run with `make check-fast`, not pytest directly.
EOF
  [ -f "$repo/.oms/skills/build-quirks/SKILL.md" ] || fail "skill not stored"
  [ -L "$repo/.agents/skills/build-quirks" ] || fail "not linked for codex/agy"
  [ -L "$repo/.claude/skills/build-quirks" ] || fail "not linked for claude"
  grep -Fq ".agents/skills/build-quirks" "$repo/.git/info/exclude" ||
    fail "codex/agy link not hidden from git"
  grep -Fq ".claude/skills/build-quirks" "$repo/.git/info/exclude" ||
    fail "claude link not hidden from git"

  bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" status | grep -Fq "1 project skill(s) valid" ||
    fail "status should count the valid skill"

  cp "$repo/.oms/skills/build-quirks/SKILL.md" "$original"
  if printf -- '---\nname: build-quirks\ndescription: short\n---\nbody\n' |
    bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" add --name build-quirks \
      >/dev/null 2>&1; then
    fail "add must refuse to overwrite an existing project skill"
  fi
  cmp -s "$original" "$repo/.oms/skills/build-quirks/SKILL.md" ||
    fail "rejected replacement destroyed the existing project skill"

  # Invalidate the stored skill: the next link pass must withdraw the links
  # and status must fail loudly.
  printf 'no frontmatter\n' > "$repo/.oms/skills/build-quirks/SKILL.md"
  bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" link >/dev/null 2>&1
  [ ! -e "$repo/.agents/skills/build-quirks" ] ||
    fail "invalid skill must be unlinked"
  if bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" status >/dev/null 2>&1; then
    fail "status must be nonzero with an invalid skill"
  fi

  bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" remove build-quirks >/dev/null
  [ ! -d "$repo/.oms/skills/build-quirks" ] || fail "remove left the skill"
}

test_skill_forge_rejects_thin_and_sensitive() {
  local repo="$TMP/forge-reject"
  local protected="$repo/.oms/protected"
  local vector

  make_repo "$repo"
  if printf -- '---\nname: bad\ndescription: short\n---\nbody\n' |
    bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" add --name bad 2>/dev/null; then
    fail "a thin description must be rejected"
  fi
  [ ! -d "$repo/.oms/skills/bad" ] || fail "rejected skill must not be stored"

  # Assembled at runtime so this test file stays scrubber-clean.
  vector="AK""IAIOSFODNN7EXAMPLE"
  if printf -- '---\nname: leaky\ndescription: a sufficiently long description that satisfies the routing-quality floor\n---\nkey: %s\n' "$vector" |
    bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" add --name leaky 2>/dev/null; then
    fail "secret-shaped content must be rejected"
  fi
  [ ! -d "$repo/.oms/skills/leaky" ] || fail "sensitive skill must not be stored"

  mkdir -p "$protected"
  printf 'must survive\n' > "$protected/SKILL.md"
  if bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" show ../protected \
    >/dev/null 2>&1; then
    fail "show must reject a traversal name"
  fi
  if bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" remove ../protected \
    >/dev/null 2>&1; then
    fail "remove must reject a traversal name"
  fi
  [ -f "$protected/SKILL.md" ] ||
    fail "traversal remove escaped the project skills directory"
}

test_task_close_hints_at_forging_learned_lessons() {
  local repo="$TMP/forge-hint"
  local out

  make_repo "$repo"
  ( cd "$repo" &&
    OMS_ADVISE_AFTER_FAILURES=0 bash "$ROOT/scripts/fail-ledger.sh" record --cmd "make flaky" --exit 1 >/dev/null &&
    OMS_ADVISE_AFTER_FAILURES=0 bash "$ROOT/scripts/fail-ledger.sh" record --cmd "make flaky" --exit 1 >/dev/null &&
    bash "$ROOT/scripts/fail-ledger.sh" resolve --cmd "make flaky" >/dev/null )
  bash "$ROOT/scripts/agent-task.sh" --repo "$repo" init --goal "lesson" >/dev/null
  out="$(bash "$ROOT/scripts/agent-task.sh" --repo "$repo" close 2>&1 || true)"
  printf '%s' "$out" | grep -Fq "skill-forge add" ||
    fail "close should hint at promoting a resolved repeated failure: $out"
}

test_template_style_switch_retires_the_old_block() {
  local project="$TMP/style-switch"

  mkdir -p "$project"
  git -C "$project" init -q 2>/dev/null || true
  bash "$ROOT/scripts/apply-project-template.sh" general "$project" >/dev/null 2>&1 ||
    fail "general template apply failed"
  grep -Fq "oh-my-setting:general:begin" "$project/AGENTS.md" ||
    fail "general block missing after apply"

  bash "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null 2>&1 ||
    fail "ml template apply failed"
  grep -Fq "oh-my-setting:ml:begin" "$project/AGENTS.md" ||
    fail "ml block missing after switch"
  # Two loaders with different rules is the bug this guards: the retired
  # style must be gone from every managed file, not merely superseded.
  local f
  for f in AGENTS.md CLAUDE.md; do
    if grep -Fq "oh-my-setting:general:begin" "$project/$f"; then
      fail "stale general block survived the switch in $f"
    fi
  done

  # The slurm overlay is additive and follows machine detection, so a
  # re-apply from a machine without Slurm must not strip cluster rules a
  # project deliberately carries. Only an explicit remove does that.
  bash "$ROOT/scripts/apply-project-template.sh" slurm "$project" >/dev/null 2>&1 ||
    fail "slurm overlay apply failed"
  grep -Fq "oh-my-setting:slurm:begin" "$project/AGENTS.md" ||
    fail "slurm overlay missing after apply"
  bash "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null 2>&1 ||
    fail "re-applying ml failed"
  grep -Fq "oh-my-setting:slurm:begin" "$project/AGENTS.md" ||
    fail "re-applying a base style must not strip the slurm overlay"
}

test_ml_template_installs_project_skills() {
  local project="$TMP/ml-skills-proj"

  mkdir -p "$project"
  git -C "$project" init -q 2>/dev/null || true
  bash "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null 2>&1 ||
    fail "ml template apply failed"
  [ -f "$project/.oms/skills/ml-experiment/SKILL.md" ] ||
    fail "ml-experiment project skill not installed"
  [ -f "$project/.oms/skills/dataset-safety/SKILL.md" ] ||
    fail "dataset-safety project skill not installed"
  [ -L "$project/.agents/skills/ml-experiment" ] ||
    fail "ml-experiment not linked for native discovery"
  bash "$ROOT/scripts/skill-forge.sh" --repo "$project" status |
    grep -Fq "2 project skill(s) valid" || fail "installed skills should validate"

  # Idempotent: a second apply must not fail on the existing skills.
  bash "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null 2>&1 ||
    fail "re-applying the ml template should be idempotent"

  # A general project gets none.
  local plain="$TMP/general-skills-proj"
  mkdir -p "$plain"
  bash "$ROOT/scripts/apply-project-template.sh" general "$plain" >/dev/null 2>&1 ||
    fail "general template apply failed"
  [ ! -d "$plain/.oms/skills" ] || fail "general template must not install ML skills"
}

# --- slurm generator rename -------------------------------------------------

test_slurm_reference_rename_keeps_compat() {
  bash "$ROOT/scripts/generate-slurm-reference.sh" --help >/dev/null ||
    fail "generate-slurm-reference --help failed"
  bash "$ROOT/scripts/generate-slurm-skill.sh" --help >/dev/null ||
    fail "compat shim generate-slurm-skill --help failed"
  grep -Fq 'generate-slurm-reference' "$ROOT/scripts/oms" ||
    fail "oms allowlist should carry the new name"
  grep -Fq 'generate-slurm-skill' "$ROOT/scripts/oms" ||
    fail "oms allowlist should keep the old name as an alias"
}

test_mcp_server_protocol
test_install_mcp_registers_and_is_idempotent
test_install_agy_plugin_bakes_absolute_paths
test_router_state_hint_on_unresolved_failures
test_router_state_hint_skips_unadopted_repo
test_conditional_skills_link_only_where_required_commands_exist
test_router_skips_conditional_skill_without_command
test_skill_forge_stores_links_and_hides
test_skill_forge_rejects_thin_and_sensitive
test_task_close_hints_at_forging_learned_lessons
test_ml_template_installs_project_skills
test_template_style_switch_retires_the_old_block
test_slurm_reference_rename_keeps_compat

echo "state-surfaces-smoke: ok"
