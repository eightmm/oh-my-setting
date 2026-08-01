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
test_slurm_reference_rename_keeps_compat

echo "state-surfaces-smoke: ok"
