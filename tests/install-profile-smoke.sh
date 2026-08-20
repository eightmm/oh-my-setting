#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-profile.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT INT TERM HUP

plan="$("$ROOT/scripts/install-profile.sh" --plan --profile core --primary-provider codex)"
printf '%s' "$plan" | python3 -c 'import json,sys; r=json.load(sys.stdin); assert r["managed_tools"] == ["node","codex"]; assert r["notion_optional"]'
full="$("$ROOT/scripts/install-profile.sh" --plan --profile full --primary-provider codex)"
printf '%s' "$full" | python3 -c 'import json,sys; r=json.load(sys.stdin); assert {"node","codex","claude","agy","gh","ntn","uv"} <= set(r["managed_tools"])'

mkdir -p "$TMP/bin"
cat > "$TMP/bin/codex" <<'EOF_CODEX'
#!/usr/bin/env bash
case "$1" in --version) echo 'codex fixture' ;; esac
exit 0
EOF_CODEX
chmod +x "$TMP/bin/codex"
PATH="$TMP/bin:$PATH" "$ROOT/scripts/install-profile.sh" --check --profile core --primary-provider codex >/dev/null

cat > "$TMP/capabilities.json" <<'EOF_RECEIPT'
{"schema":1,"plan":{"requested":["core","github"],"primary_provider":"codex"},"allow_missing":false}
EOF_RECEIPT
reapply="$($ROOT/scripts/install-profile.sh --plan --reapply --receipt "$TMP/capabilities.json")"
printf '%s' "$reapply" | python3 -c 'import json,sys; r=json.load(sys.stdin); assert r["requested"] == ["core","github"]; assert r["primary_provider"] == "codex"; assert r["managed_tools"] == ["node","codex","gh"]'

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
  grep -Fq 'a harness child cannot install or apply capability profiles' "$output" || {
    echo "FAIL: child $label install refusal was not actionable: $(cat "$output")" >&2
    exit 1
  }
}

# The hpc profile has no managed installer payload. Without the child guard it
# would still write a receipt (under this fixture), so the regression exercises
# a real apply without ever downloading or changing host tools.
cat > "$TMP/child-capabilities.json" <<'EOF_CHILD_RECEIPT'
{"schema":1,"plan":{"requested":["hpc"],"primary_provider":"codex"},"allow_missing":true}
EOF_CHILD_RECEIPT
child_receipt="$TMP/blocked-capabilities.json"
assert_child_install_refused default --profile hpc --primary-provider codex \
  --allow-missing --receipt "$child_receipt"
assert_child_install_refused explicit --apply --profile hpc \
  --primary-provider codex --allow-missing --receipt "$child_receipt"
assert_child_install_refused final-apply --dry-run --apply --profile hpc \
  --primary-provider codex --allow-missing --receipt "$child_receipt"
assert_child_install_refused reapply --reapply --receipt \
  "$TMP/child-capabilities.json"
[ ! -e "$child_receipt" ] || {
  echo "FAIL: refused child install wrote a capability receipt" >&2
  exit 1
}

# The final parsed action is authoritative: plans/checks stay observable, and
# an explicit dry-run after --apply must not be misclassified as a mutation.
OMS_HARNESS_CHILD=1 "$ROOT/scripts/install-profile.sh" --plan --profile core \
  --primary-provider codex >/dev/null
OMS_HARNESS_CHILD=1 PATH="$TMP/bin:$PATH" "$ROOT/scripts/install-profile.sh" \
  --check --profile core --primary-provider codex >/dev/null
OMS_HARNESS_CHILD=1 "$ROOT/scripts/install-profile.sh" --apply --dry-run \
  --profile core --primary-provider codex >/dev/null

echo "install-profile-smoke: ok"
