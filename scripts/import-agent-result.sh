#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/peer-common.sh
. "$ROOT/scripts/lib/peer-common.sh"

REPO="$PWD"
KIND=""
PROVIDER=""
RESULT_FILE=""
PROMPT_FILE=""
ARTIFACT_DIR=""
EXIT_CODE=0

usage() {
  cat <<'EOF'
Usage: import-agent-result.sh --kind KIND --provider PROVIDER --file PATH [options]

Import an externally run agent answer into the same artifact/index format used
by the multi-agent harness. Use this after an --export-only prompt is answered
by a registered or external agent outside the current policy boundary.

Options:
  --kind KIND          ask, review, delegate, or call. Required.
  --provider NAME      Registered agent transport. Required.
  --file PATH          File containing the provider answer. Required.
  --prompt-file PATH   Exported prompt artifact or prompt text file to include.
  --repo PATH          Repo/directory for artifact index. Default: PWD.
  --artifact-dir PATH  Destination artifact dir. Default: REPO/.oms/artifacts/KIND.
  --exit N             Imported provider exit code. Default: 0.
  -h, --help           Show help.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --kind)
      [ "$#" -ge 2 ] || fail "--kind requires value"
      KIND="$2"
      shift 2
      ;;
    --provider)
      [ "$#" -ge 2 ] || fail "--provider requires value"
      PROVIDER="$2"
      shift 2
      ;;
    --file)
      [ "$#" -ge 2 ] || fail "--file requires path"
      RESULT_FILE="$2"
      shift 2
      ;;
    --prompt-file)
      [ "$#" -ge 2 ] || fail "--prompt-file requires path"
      PROMPT_FILE="$2"
      shift 2
      ;;
    --repo)
      [ "$#" -ge 2 ] || fail "--repo requires path"
      REPO="$2"
      shift 2
      ;;
    --artifact-dir)
      [ "$#" -ge 2 ] || fail "--artifact-dir requires path"
      ARTIFACT_DIR="$2"
      shift 2
      ;;
    --exit)
      [ "$#" -ge 2 ] || fail "--exit requires number"
      EXIT_CODE="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

case "$KIND" in
  ask|review|delegate|call) ;;
  "") fail "--kind is required" ;;
  *) fail "--kind must be ask, review, delegate, or call" ;;
esac
[ -n "$PROVIDER" ] || fail "--provider is required"
PROVIDER="$(oms_provider_normalize "$PROVIDER")" || exit $?
[ -n "$RESULT_FILE" ] || fail "--file is required"
[ -f "$RESULT_FILE" ] || fail "result file not found: $RESULT_FILE"
# An empty paste would become an artifact with a harness-written exit 0,
# indistinguishable downstream from a clean answer. Refuse it here; a
# deliberately failed run is imported with its real --exit instead.
[ -s "$RESULT_FILE" ] || fail "result file is empty: $RESULT_FILE"
if [ -n "$PROMPT_FILE" ]; then
  [ -f "$PROMPT_FILE" ] || fail "prompt file not found: $PROMPT_FILE"
fi
case "$EXIT_CODE" in
  *[!0-9]*|"") fail "--exit must be a non-negative integer" ;;
esac
if agent_memory_file_has_sensitive_content "$RESULT_FILE"; then
  echo "warning: imported result contains sensitive-looking content; review before sharing or promoting to memory" >&2
fi

REPO="$(cd "$REPO" && pwd)"
ARTIFACT_DIR="${ARTIFACT_DIR:-$REPO/.oms/artifacts/$KIND}"
agent_memory_ensure_oms_ignore_for_path "$ARTIFACT_DIR"
mkdir -p "$ARTIFACT_DIR"

slug="imported-result"
if [ -n "$PROMPT_FILE" ]; then
  slug="$(slugify "$(basename "$PROMPT_FILE")")"
fi
[ -n "$slug" ] || slug="imported-result"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
export OMS_OPERATION_ID="${OMS_OPERATION_ID:-import-$timestamp}"
artifact="$ARTIFACT_DIR/$PROVIDER-$slug-$timestamp.import.md"

{
  printf '# %s %s import\n\n' "$PROVIDER" "$KIND"
  printf -- '- imported: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf -- '- repo: %s\n' "$(ma_repo_label "$REPO")"
  if [ -n "$PROMPT_FILE" ]; then
    printf -- '- source-prompt: %s\n' "$(ma_artifact_relpath "$REPO" "$PROMPT_FILE" 2>/dev/null || basename "$PROMPT_FILE")"
  fi
  printf '\n## Prompt\n\n'
  if [ -n "$PROMPT_FILE" ]; then
    cat "$PROMPT_FILE"
  else
    printf 'Prompt not supplied at import time.\n'
  fi
  printf '\n\n## Output\n\n'
  cat "$RESULT_FILE"
  printf '\n\n## Exit\n\n%s\n' "$EXIT_CODE"
} > "$artifact"

ma_append_artifact_index "$REPO" "$KIND-import" "$PROVIDER" "$EXIT_CODE" "$artifact" "" "$PROMPT_FILE" "" "$PROMPT_FILE" || true
# Imports bypass every in-band completeness check, so judge the pasted answer
# once at the door. A warning, not a refusal: the operator may knowingly
# import a refusal or fragment, but it must not pass silently as an answer.
quality="$(ma_answer_quality "$artifact" 2>/dev/null || printf 'blocked')"
[ "$quality" = "ok" ] ||
  echo "warning: imported result reads as a non-answer ($quality); downstream consumers see only its recorded exit ($EXIT_CODE)" >&2
printf 'imported: %s -> %s\n' "$PROVIDER" "$artifact"
