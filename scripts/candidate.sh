#!/usr/bin/env bash
set -euo pipefail

# Keep results from different tools and runs attached to one candidate id in
# .oms/candidates.jsonl. Bookkeeping only: it validates no structure or score.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

usage() {
  cat <<'EOF'
Usage: candidate.sh add --id CID [--kind ligand|protein|complex|pose|other]
                        [--ref LABEL=VALUE]... [--smiles S|-] [--store-structure] [--note TEXT]
       candidate.sh result --id CID --tool NAME --metric KEY --value NUMBER [--unit U]
                        (--run RUN_OR_CAPSULE_ID | --external RECEIPT_ID | --unlinked)
                        [--conditions TEXT] [--higher-is-better|--lower-is-better]
       candidate.sh show CID [--json]
       candidate.sh table [--metric KEY]... [--json|--format md|tsv]

CID is your own stable identifier (InChIKey, registry or library id): letters,
digits and ._:- up to 64 characters, never parsed as chemistry. --ref keeps a
labelled reference such as inchikey=... A --smiles string is stored as its
sha256 and length only; --store-structure keeps the string (unpublished
structures should not sit in a shared log). `-` reads the SMILES from stdin.

result links each number to a run capsule (.oms/runs/<id>), a run-ledger row id
or an `oms external` receipt; unknown ids are refused. --unlinked records a
number with no link and shows it as unlinked.
show groups results by tool and metric and flags a conflict when the same
tool, metric and conditions have different values from different links (both
shown, none picked). table is candidates x tool:metric with - for missing; it
orders rows only for exactly one --metric with a stated direction, and never
averages across tools.

Append-only, schema 1. This is bookkeeping: it does not validate structures or
scores, and it contacts no network.
EOF
}

case "${1:-}" in
  ""|-h|--help) usage; [ -n "${1:-}" ] && exit 0 || exit 2 ;;
esac
command -v python3 >/dev/null 2>&1 || { echo "error: python3 is required" >&2; exit 2; }

REPO="$(oms_repo_root "$PWD")"
LIB="$ROOT/scripts/lib/candidate_ledger.py"
STORE="${OMS_CANDIDATES_FILE:-$REPO/.oms/candidates.jsonl}"

case "$1" in
  add|result) ;;
  *) exec python3 -I "$LIB" --root "$REPO" "$@" ;;
esac

ROW_TMP="$(agent_memory_mktemp)" || { echo "error: mktemp failed" >&2; exit 2; }
trap 'rm -f "$ROW_TMP"' EXIT
summary="$(python3 -I "$LIB" --root "$REPO" --out "$ROW_TMP" "$@")" || exit $?

# Free text lands in a log that may be shared; refuse sensitive-looking rows.
if agent_memory_file_has_sensitive_content "$ROW_TMP"; then
  echo "error: candidate row looks sensitive (note, conditions or reference); nothing was recorded" >&2
  exit 2
fi

append_row() {
  python3 "$ROOT/scripts/lib/durable-jsonl.py" --label candidates.jsonl append "$STORE" < "$ROW_TMP"
}
mkdir -p "$(dirname "$STORE")"
agent_memory_ensure_oms_ignore_for_path "$STORE" 2>/dev/null || true
oms_with_file_lock "$STORE" append_row || { echo "error: could not append to $STORE" >&2; exit 2; }
printf '%s\n' "$summary"
