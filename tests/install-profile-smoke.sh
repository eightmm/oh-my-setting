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

echo "install-profile-smoke: ok"
