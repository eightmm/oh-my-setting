#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
unset OMS_CAPABILITY_RECEIPT OH_MY_SETTING_TOOL_LOCK
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-profile.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT INT TERM HUP

plan="$("$ROOT/scripts/install-profile.sh" --plan --profile core --primary-provider codex)"
printf '%s' "$plan" | jq -e '.managed_tools == ["node","codex"] and .profiles == ["core"] and .native_runtime == true' >/dev/null || {
  echo "FAIL: core plan mismatch: $plan" >&2; exit 1
}
full="$("$ROOT/scripts/install-profile.sh" --plan --profile full --primary-provider codex)"
printf '%s' "$full" | jq -e '(["node","codex","claude","agy","gh","ntn","uv"] - .managed_tools) | length == 0' >/dev/null || {
  echo "FAIL: full plan mismatch: $full" >&2; exit 1
}
for dependent in codex claude ntn; do
  ordered="$("$ROOT/scripts/install-tools.sh" --plan --only "$dependent,node")"
  printf '%s' "$ordered" | jq -e --arg tool "$dependent" '.managed_tools == ["node",$tool]' >/dev/null || {
    echo "FAIL: Node prerequisite did not precede $dependent: $ordered" >&2; exit 1
  }
done

mkdir -p "$TMP/bin"
cat > "$TMP/bin/codex" <<'EOF_CODEX'
#!/usr/bin/env bash
case "${1:-}" in --version) echo 'codex fixture' ;; esac
exit 0
EOF_CODEX
chmod +x "$TMP/bin/codex"
PATH="$TMP/bin:$PATH" "$ROOT/scripts/install-profile.sh" --check --profile core --primary-provider codex >/dev/null

cat > "$TMP/capabilities.json" <<'EOF_RECEIPT'
{"schema":1,"plan":{"requested":["core","github"],"primary_provider":"codex"},"allow_missing":false}
EOF_RECEIPT
reapply="$("$ROOT/scripts/install-profile.sh" --plan --reapply --receipt "$TMP/capabilities.json")"
printf '%s' "$reapply" | jq -e '.requested == ["core","github"] and .primary_provider == "codex" and .managed_tools == ["node","codex","gh"]' >/dev/null || {
  echo "FAIL: reapply mismatch: $reapply" >&2; exit 1
}

added="$("$ROOT/scripts/install-profile.sh" --plan --add --profile notion --profile github --receipt "$TMP/capabilities.json")"
printf '%s' "$added" | jq -e '.requested == ["core","github","notion"] and .primary_provider == "codex"' >/dev/null || {
  echo "FAIL: add mismatch: $added" >&2; exit 1
}
replacement="$("$ROOT/scripts/install-profile.sh" --plan --profile github --receipt "$TMP/capabilities.json")"
printf '%s' "$replacement" | jq -e '.requested == ["github"]' >/dev/null || {
  echo "FAIL: replacement mismatch: $replacement" >&2; exit 1
}

cat > "$TMP/external.json" <<'EOF_EXTERNAL'
{"schema":1,"plan":{"requested":["hpc"],"primary_provider":"agy"},"allow_missing":true}
EOF_EXTERNAL
HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/xdg-config" "$ROOT/scripts/install-profile.sh" \
  --apply --add --profile remote --receipt "$TMP/external.json" > "$TMP/added.json"
jq -e '.plan.requested == ["hpc","remote"] and .primary_provider == "agy" and .allow_missing == true' "$TMP/external.json" >/dev/null || {
  echo "FAIL: updated external receipt mismatch: $(cat "$TMP/external.json")" >&2; exit 1
}

(
  cd "$TMP"
  HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/xdg-config" OMS_CAPABILITY_RECEIPT=env-capabilities.json \
    "$ROOT/scripts/install-profile.sh" --apply --profile hpc --allow-missing >/dev/null
  OMS_CAPABILITY_RECEIPT=env-capabilities.json \
    "$ROOT/scripts/install-profile.sh" --plan --reapply > "$TMP/env-reapply.json"
)
jq -e '.requested == ["hpc"] and .managed_tools == [] and .allow_missing == true' "$TMP/env-reapply.json" >/dev/null || {
  echo "FAIL: relative environment receipt was not written and reapplied" >&2; exit 1
}
OMS_CAPABILITY_RECEIPT="$TMP/absent.json" "$ROOT/scripts/install-profile.sh" \
  --plan --reapply --receipt "$TMP/capabilities.json" > "$TMP/explicit-reapply.json"
jq -e '.requested == ["core","github"]' "$TMP/explicit-reapply.json" >/dev/null || {
  echo "FAIL: explicit receipt did not override environment receipt" >&2; exit 1
}
ln -s "$TMP/capabilities.json" "$TMP/linked-receipt.json"
if OMS_CAPABILITY_RECEIPT="$TMP/linked-receipt.json" "$ROOT/scripts/install-profile.sh" \
  --plan --reapply >/dev/null 2>&1; then
  echo "FAIL: environment receipt followed an unsafe leaf symlink" >&2; exit 1
fi

# Every discovered managed tool is a fixture; downloads and real provider calls fail.
profile_home="$TMP/profile-home"
profile_bin="$TMP/profile-bin"
profile_receipt="$profile_home/.config/oh-my-setting/capabilities.json"
mkdir -p "$profile_home/.local/bin" "$profile_home/.local/share/claude/versions" \
  "$profile_home/.codex/packages/standalone/releases/1.0.0/bin" \
  "$profile_home/.config/oh-my-setting" "$profile_home/npm/lib/node_modules" "$profile_bin"
cat > "$TMP/tool-fixture" <<'EOF_TOOL'
#!/bin/sh
case "$*" in
  --version) echo 'fixture 1.0.0' ;;
  update) exit 0 ;;
  *) echo 'unexpected tool fixture invocation' >&2; exit 99 ;;
esac
EOF_TOOL
chmod +x "$TMP/tool-fixture"
for tool in node uv ntn gh; do
  cp "$TMP/tool-fixture" "$profile_bin/$tool"
done
cp "$TMP/tool-fixture" "$profile_home/.local/share/claude/versions/1.0.0"
cp "$TMP/tool-fixture" "$profile_home/.codex/packages/standalone/releases/1.0.0/bin/codex"
cp "$TMP/tool-fixture" "$profile_home/.local/bin/agy"
ln -s "$profile_home/.local/share/claude/versions/1.0.0" "$profile_home/.local/bin/claude"
ln -s "$profile_home/.codex/packages/standalone/releases/1.0.0/bin/codex" "$profile_home/.local/bin/codex"
cat > "$profile_bin/npm" <<'EOF_NPM'
#!/bin/sh
case "$*" in
  'root -g') printf '%s/npm/lib/node_modules\n' "$HOME" ;;
  'prefix -g') printf '%s/npm\n' "$HOME" ;;
  *) echo 'unexpected npm fixture invocation' >&2; exit 99 ;;
esac
EOF_NPM
cat > "$profile_bin/curl" <<'EOF_CURL'
#!/bin/sh
echo 'unexpected tool download' >&2
exit 99
EOF_CURL
chmod +x "$profile_bin/npm" "$profile_bin/curl"
profile_run() {
  HOME="$profile_home" CODEX_HOME="$profile_home/.codex" \
    XDG_CONFIG_HOME="$profile_home/.config" XDG_DATA_HOME="$profile_home/.local/share" \
    XDG_STATE_HOME="$profile_home/.local/state" XDG_CACHE_HOME="$profile_home/.cache" \
    OMS_INSTALL_RECEIPT="$profile_home/.config/oh-my-setting/install.json" \
    PATH="$profile_home/.local/bin:$profile_bin:$PATH" "$@"
}
profile_run "$ROOT/scripts/install-tools.sh" >/dev/null
[ ! -e "$profile_receipt" ] || { echo 'FAIL: legacy tool install created a core profile receipt' >&2; exit 1; }
cp "$TMP/capabilities.json" "$profile_receipt"
profile_run "$ROOT/scripts/install-tools.sh" >/dev/null
cmp "$TMP/capabilities.json" "$profile_receipt"
profile_run "$ROOT/scripts/install-tools.sh" --only uv >/dev/null
cmp "$TMP/capabilities.json" "$profile_receipt"
profile_run "$ROOT/scripts/install-tools.sh" --providers >/dev/null
cmp "$TMP/capabilities.json" "$profile_receipt"
profile_run "$ROOT/scripts/install-tools.sh" --only uv --receipt "$TMP/tool-only.json" >/dev/null
[ ! -e "$TMP/tool-only.json" ] || { echo 'FAIL: receipt path alone created profile intent' >&2; exit 1; }
profile_run "$ROOT/scripts/install-tools.sh" --profile hpc --allow-missing >/dev/null
OMS_CAPABILITY_RECEIPT='' profile_run "$ROOT/scripts/install-tools.sh" --reapply --plan > "$TMP/tools-reapply.json"
jq -e '.requested == ["hpc"] and .managed_tools == []' "$TMP/tools-reapply.json" >/dev/null || {
  echo 'FAIL: install-tools reapply expanded beyond its stored profile' >&2; exit 1
}
profile_run "$ROOT/scripts/install-tools.sh" --full >/dev/null
jq -e '.requested == ["full"]' "$profile_receipt" >/dev/null || {
  echo 'FAIL: explicit full tool selection was not recorded' >&2; exit 1
}

printf 'invalid\n' > "$TMP/invalid.json"
if "$ROOT/scripts/install-profile.sh" --plan --add --profile github --receipt "$TMP/invalid.json" >/dev/null 2>&1; then
  echo "FAIL: --add accepted an invalid receipt" >&2; exit 1
fi
if "$ROOT/scripts/install-profile.sh" --plan --add --reapply --receipt "$TMP/capabilities.json" >/dev/null 2>&1; then
  echo "FAIL: --add accepted --reapply" >&2; exit 1
fi

assert_child_install_refused() {
  local label="$1"
  shift
  local output="$TMP/child-$label.out"
  local rc=0
  OMS_HARNESS_CHILD=1 HOME="$TMP/child-home" \
    XDG_CONFIG_HOME="$TMP/child-home/.config" \
    "$ROOT/scripts/install-profile.sh" "$@" \
    >"$output" 2>&1 || rc=$?
  [ "$rc" -eq 2 ] || {
    echo "FAIL: child $label install returned $rc: $(cat "$output")" >&2
    exit 1
  }
  grep -Fq 'a harness child cannot mutate OMS host lifecycle authority' "$output" || {
    echo "FAIL: child $label install refusal was not actionable: $(cat "$output")" >&2
    exit 1
  }
}

cat > "$TMP/child-capabilities.json" <<'EOF_CHILD_RECEIPT'
{"schema":1,"plan":{"requested":["hpc"],"primary_provider":"codex"},"allow_missing":true}
EOF_CHILD_RECEIPT
child_receipt="$TMP/blocked-capabilities.json"
assert_child_install_refused default --profile hpc --primary-provider codex \
  --allow-missing --receipt "$child_receipt"
assert_child_install_refused explicit --apply --profile hpc \
  --primary-provider codex --allow-missing --receipt "$child_receipt"
assert_child_install_refused final-apply --plan --apply --profile hpc \
  --primary-provider codex --allow-missing --receipt "$child_receipt"
assert_child_install_refused reapply --reapply --receipt \
  "$TMP/child-capabilities.json"
[ ! -e "$child_receipt" ] || {
  echo "FAIL: refused child install wrote a capability receipt" >&2
  exit 1
}

OMS_HARNESS_CHILD=1 "$ROOT/scripts/install-profile.sh" --plan --profile core \
  --primary-provider codex >/dev/null
OMS_HARNESS_CHILD=1 PATH="$TMP/bin:$PATH" "$ROOT/scripts/install-profile.sh" \
  --check --profile core --primary-provider codex >/dev/null
OMS_HARNESS_CHILD=1 "$ROOT/scripts/install-profile.sh" --apply --plan \
  --profile core --primary-provider codex >/dev/null

echo "install-profile-smoke: ok"
