#!/usr/bin/env bash
set -euo pipefail

# Advisory file leases: parallel sessions in one repository declare, with a time limit, which paths they intend to edit, so others see it before they collide.
# Nothing in OMS refuses an edit because of a lease; call `oms lease check` yourself.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"

usage() {
  cat <<'EOF'
Usage: lease.sh add PATH_OR_GLOB... [--ttl 30m] [--reason TEXT] [--shared]
       lease.sh list [--json]
       lease.sh release ID | --mine
       lease.sh check PATH... | --staged

Leases are advisory rows in .oms/leases.jsonl (append-only; add and release
rows). Paths and globs are resolved from the current directory and must stay
inside the repository. --ttl is 1m to 8h (default 30m); expiry is computed on
read, an expired lease is listed as expired and never deleted. Shared leases do
not conflict with each other; an exclusive lease conflicts with any overlap, and
when two patterns cannot be compared exactly they count as overlapping.

The holder is OMS_ATTEMPT_ID, else OMS_ROOM_PARTICIPANT, else unknown-session
(sessions without either cannot be told apart and count as one holder).
add warns about overlaps but still records. check exits 0 when free, 1 when an
exclusive lease held by someone else covers a path, 2 on usage errors; --staged
checks `git diff --cached --name-only` and suits a personal pre-commit hook.
EOF
}

case "${1:-}" in
  "") usage >&2; exit 2 ;;
  -h|--help|help) usage; exit 0 ;;
  add|list|check|release) ;;
  *) usage >&2; exit 2 ;;
esac
command -v python3 >/dev/null 2>&1 || { echo "error: python3 is required" >&2; exit 2; }

verb="$1"
shift
repo="$(oms_repo_root "$PWD")"
repo="$(cd "$repo" && pwd -P)"
state="$(oms_state_root "$PWD")"
store="$state/.oms/leases.jsonl"
mkdir -p "$state/.oms"
run=(python3 -I "$ROOT/scripts/lib/file_lease.py" --root "$repo" --store "$store" "$verb" "$@")
case "$verb" in
  add|release) oms_with_file_lock "$store" "${run[@]}" ;;
  *) "${run[@]}" ;;
esac
