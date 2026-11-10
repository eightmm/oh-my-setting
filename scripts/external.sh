#!/usr/bin/env bash
set -euo pipefail

# Receipts for external science skills, MCP servers, models and datasets in
# .oms/external.jsonl: version, sha256, declared license, environment lock and
# whether use can send data out. Records only; it installs and judges nothing.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

usage() {
  cat <<'EOF'
Usage: external.sh add --kind skill|mcp|model|dataset|tool --name NAME [options]
       external.sh list [--json]
       external.sh show ID
       external.sh check [ID]

add options:
  --version V       Version or commit.
  --file PATH       File to hash (sha256 recorded). --sha256 HEX records a
                    known digest instead, or must equal the file's.
  --license TEXT    License as declared by the source; recorded, never judged.
  --env PATH        Environment lock file (conda-lock, requirements, uv.lock);
                    its sha256 is recorded.
  --egress KIND     none (default), public-query, uploads-input. Anything but
                    none needs --approver NAME: unpublished structures or
                    sequences may leave this machine. "none" is the
                    provider's description, not a network observation.
  --approver NAME   Who accepted the egress (recorded as stated).
  --note TEXT

check    Recompute recorded file and env-lock hashes; exit 1 on drift or a
         missing file. Egress rows print a reminder.
Receipts are append-only; the last row for an id wins.
EOF
}

case "${1:-}" in
  ""|-h|--help) usage; [ -n "${1:-}" ] && exit 0 || exit 2 ;;
esac
command -v python3 >/dev/null 2>&1 || { echo "error: python3 is required" >&2; exit 2; }
exec python3 -I "$ROOT/scripts/lib/external_receipts.py" --root "$(oms_repo_root "$PWD")" "$@"
