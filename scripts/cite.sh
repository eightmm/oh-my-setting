#!/usr/bin/env bash
set -euo pipefail

# Per-repo literature notes and identifier checks: add/list/verify/export over
# .oms/literature.jsonl. Offline unless `verify --online` is given; then only
# the DOI or arXiv id is sent to Crossref or the arXiv API. No full-text RAG.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

usage() {
  cat <<'EOF'
Usage: cite.sh add --id ID [--title T] [--year Y] [--claim C] [--locator L]
       cite.sh list [--json]
       cite.sh verify [--id ID] [--online]
       cite.sh export --bibtex

ID is a DOI (10.x/y, doi:..., https://doi.org/...) or an arXiv id (2101.00001,
arxiv:..., hep-th/9901001). Notes go to .oms/literature.jsonl (append-only; the
last row for an id wins). Statuses: unverified, verified, mismatch, not-found,
invalid-id.

verify      Offline it only checks identifier syntax. --online asks Crossref
            (DOI) or arXiv for the record and compares title and year; the
            identifier is the only thing sent. A failed lookup stays
            unverified. OMS_CITE_CROSSREF_URL / OMS_CITE_ARXIV_URL override
            the endpoints. Exit 1 unless every checked row is verified.
export      BibTeX @misc entries; the note field carries the OMS status.

A verified id proves the reference exists and matches title and year. It does
not prove the claim recorded next to it is supported by the paper.
EOF
}

case "${1:-}" in
  ""|-h|--help) usage; [ -n "${1:-}" ] && exit 0 || exit 2 ;;
esac
command -v python3 >/dev/null 2>&1 || { echo "error: python3 is required" >&2; exit 2; }
exec python3 -I "$ROOT/scripts/lib/literature.py" --root "$(oms_repo_root "$PWD")" "$@"
