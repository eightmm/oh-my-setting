#!/usr/bin/env bash
set -euo pipefail

# model-doctor after de-tiering: it diagnoses provider binaries, their default
# invocation surface, and cached capabilities — it selects no models and knows
# no tiers.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-model-doctor.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

bin="$TMP/bin"
mkdir -p "$bin" "$TMP/cap" "$TMP/adapters"
export PATH="$bin:/usr/bin:/bin"
export OMS_PROVIDER_ADAPTER_DIR="$TMP/adapters"
# Capability refreshes must not touch the real user cache.
export OMS_CAPABILITY_DIR="$TMP/cap"

cat > "$bin/provider-fake" <<'FAKE'
#!/usr/bin/env bash
set -u
provider="$(basename "$0")"
case "$provider:${1:-}" in
  *:--version) echo "$provider 1.0.0" ;;
  codex:exec) exit 0 ;;
  *:--help) echo "usage: $provider" ;;
  *) exit 0 ;;
esac
FAKE
chmod +x "$bin/provider-fake"
ln -s provider-fake "$bin/codex"
ln -s provider-fake "$bin/claude"
ln -s provider-fake "$bin/agy"

DOCTOR="$ROOT/scripts/model-doctor.sh"

# All providers installed and reachable: human report names each provider and
# ends in the ok line; exit is zero.
bash "$DOCTOR" > "$TMP/local.txt" || fail "local doctor should exit 0"
for provider in codex claude antigravity; do
  grep -Eq "^$provider: installed" "$TMP/local.txt" ||
    fail "local report should cover $provider"
done
grep -Fq 'model-doctor: ok' "$TMP/local.txt" || fail "local doctor should pass"

# JSON is the same report, schema-versioned, with no tier vocabulary left.
bash "$DOCTOR" --json > "$TMP/local.json" || fail "json doctor should exit 0"
OMS_T_JSON="$TMP/local.json" python3 - <<'CHECK' || fail "json contract mismatch"
import json, os
x = json.load(open(os.environ["OMS_T_JSON"]))
assert x["schema"] == 2, x["schema"]
assert x["ok"] is True
providers = {p["provider"] for p in x["providers"]}
assert providers == {"codex", "claude", "antigravity"}, providers
for p in x["providers"]:
    assert p["installed"] is True
    assert p["provider_default_reachable"] is True
raw = open(os.environ["OMS_T_JSON"]).read()
for word in ("fast", "balanced", "deep"):
    assert '"%s"' % word not in raw, word
CHECK

# A provider subset restricts the report to what was asked.
bash "$DOCTOR" --providers codex --json > "$TMP/subset.json" ||
  fail "subset doctor should exit 0"
OMS_T_JSON="$TMP/subset.json" python3 - <<'CHECK' || fail "subset mismatch"
import json, os
x = json.load(open(os.environ["OMS_T_JSON"]))
assert [p["provider"] for p in x["providers"]] == ["codex"]
CHECK

# Alias normalization cannot report the same provider twice.
rc=0
bash "$DOCTOR" --providers codex,agy,antigravity > "$TMP/duplicate.txt" 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "agy/antigravity duplicate should be rejected as usage error"

# A missing binary is a warning by default and an error under --require-all.
rm "$bin/agy"
bash "$DOCTOR" > "$TMP/partial.txt" || fail "missing agy should stay ok"
grep -Fq "provider binary 'agy' is not installed" "$TMP/partial.txt" ||
  fail "missing binary should be reported"
rc=0
bash "$DOCTOR" --require-all > "$TMP/require.txt" 2>&1 || rc=$?
[ "$rc" -ne 0 ] || fail "--require-all should fail on a missing binary"
grep -Fq 'model-doctor: FAILED' "$TMP/require.txt" ||
  fail "--require-all failure should print the failed line"

# Strict diversity needs two usable model families, not two binaries.
rm "$bin/claude"
rc=0
bash "$DOCTOR" --providers codex --strict-diversity > "$TMP/strict.txt" 2>&1 || rc=$?
[ "$rc" -ne 0 ] || fail "single family should fail strict diversity"
grep -Fq 'diversity needs at least two usable families' "$TMP/strict.txt" ||
  fail "strict diversity error text missing"

echo "model-doctor-smoke: ok"
