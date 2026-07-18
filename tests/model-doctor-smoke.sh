#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-model-doctor-smoke.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

bin="$TMP/bin"
mkdir -p "$bin"
export PATH="$bin:/usr/bin:/bin"
export HOME="$TMP/home"
mkdir -p "$HOME"

cat > "$bin/provider-fake" <<'FAKE'
#!/usr/bin/env bash
set -u
provider="$(basename "$0")"
case "$provider:$*" in
  codex:--version)
    echo 'codex-cli 9.9.0'
    ;;
  codex:'exec --help')
    printf '%s\n' 'Usage: codex exec [--model MODEL] [--sandbox MODE]'
    ;;
  claude:--version)
    echo 'claude-code 9.9.0'
    ;;
  claude:--help)
    if [ "${CLAUDE_HELP_MISSING_EFFORT:-0}" = 1 ]; then
      printf '%s\n' 'Usage: claude --model MODEL --permission-mode MODE'
    else
      printf '%s\n' 'Usage: claude --model MODEL --permission-mode MODE --effort LEVEL'
    fi
    ;;
  agy:--version)
    echo 'agy 9.9.0'
    ;;
  agy:--help)
    printf '%s\n' 'Usage: agy --model MODEL --print --sandbox --print-timeout DUR'
    ;;
  agy:models)
    cat <<'MODELS'
Gemini 3.5 Flash (Low)
Gemini 3.5 Flash (Medium)
Gemini 3.5 Flash (High)
Gemini 3.1 Pro (Low)
Gemini 3.1 Pro (High)
Claude Sonnet 4.6 (Thinking)
Claude Opus 4.6 (Thinking)
GPT-OSS 120B (Medium)
MODELS
    ;;
  *)
    echo "unexpected fake invocation: $provider $*" >&2
    exit 9
    ;;
esac
FAKE
chmod +x "$bin/provider-fake"
ln -s provider-fake "$bin/codex"
ln -s provider-fake "$bin/claude"
ln -s provider-fake "$bin/agy"

DOCTOR="$ROOT/scripts/model-doctor.sh"

# Local-only inspection checks the command contract without making model-list calls.
bash "$DOCTOR" > "$TMP/local.txt"
grep -Fq 'model-doctor: ok' "$TMP/local.txt" || fail "local doctor should pass"
grep -Fq 'fast: independent' "$TMP/local.txt" || fail "default fast quorum should be independent"
grep -Fq 'availability=unverified' "$TMP/local.txt" || fail "local routes should remain unverified"

# Machine-readable output keeps the same result contract.
bash "$DOCTOR" --json > "$TMP/local.json"
python3 - "$TMP/local.json" <<'PY' || fail "JSON result contract invalid"
import json, sys
result = json.load(open(sys.argv[1], encoding="utf-8"))
assert result["schema"] == 1
assert result["ok"] is True
assert [item["status"] for item in result["diversity"]] == ["independent"] * 3
assert result["providers"][0]["routes"]["fast"]["model"] == "gpt-5.6-luna"
PY

# Live probing verifies models where the provider offers an official catalog command.
bash "$DOCTOR" --live-models > "$TMP/live.txt"
grep -Fq 'Gemini 3.1 Pro (High) [family=google, effort=high, availability=available]' "$TMP/live.txt" ||
  fail "Antigravity live model should be available"
grep -Fq 'codex: no stable model-list probe is registered' "$TMP/live.txt" ||
  fail "unsupported live catalog should be explicit"

# A configured model missing from the account-visible catalog fails closed.
rc=0
OMS_MODEL_ANTIGRAVITY_DEEP='Gemini 9 Missing (High)' \
  bash "$DOCTOR" --live-models > "$TMP/missing-model.txt" 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "missing live model should fail"
grep -Fq 'configured deep model is not in the live catalog' "$TMP/missing-model.txt" ||
  fail "missing live model diagnostic absent"

# Quorum independence follows the underlying model family, not only the CLI name.
rc=0
OMS_MODEL_ANTIGRAVITY_BALANCED='Claude Sonnet 4.6 (Thinking)' \
  bash "$DOCTOR" > "$TMP/duplicate-family.txt" 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "duplicate underlying model family should fail"
grep -Fq 'anthropic is used by claude, antigravity' "$TMP/duplicate-family.txt" ||
  fail "duplicate model-family diagnostic absent"

# Installed but incompatible CLI versions fail before a worker call is attempted.
rc=0
CLAUDE_HELP_MISSING_EFFORT=1 bash "$DOCTOR" > "$TMP/missing-flag.txt" 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "missing required CLI flag should fail"
grep -Fq 'installed CLI is missing required flags: --effort' "$TMP/missing-flag.txt" ||
  fail "missing flag diagnostic absent"

# Partial installs remain supported by default; strict environments can require all three.
rm "$bin/agy"
bash "$DOCTOR" > "$TMP/partial.txt"
grep -Fq "provider binary 'agy' is not installed" "$TMP/partial.txt" ||
  fail "partial install warning absent"
rc=0
bash "$DOCTOR" --require-all > "$TMP/require-all.txt" 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "--require-all should fail on a missing provider"

# A single selected provider is valid for diagnosis but cannot prove diversity.
bash "$DOCTOR" --providers codex > "$TMP/single-provider.txt"
grep -Fq 'fast: insufficient' "$TMP/single-provider.txt" ||
  fail "single-provider diversity should be insufficient"

# Alias normalization cannot create a duplicate quorum entry.
rc=0
bash "$DOCTOR" --providers codex,agy,antigravity > "$TMP/duplicate-provider.txt" 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "agy/antigravity duplicate should be rejected as usage error"

echo 'model-doctor-smoke: ok'
