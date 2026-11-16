#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
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
