#!/usr/bin/env bash
set -euo pipefail

# Write the private local context agents read before touching this machine.
# One front door for the two generators: the default captures a workstation
# hardware/tooling snapshot (local/machine.md), and --cluster captures a
# Slurm cluster reference (local/slurm.md) instead. Both outputs are private
# local state and never belong in git.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Native-first entry (oms_native_route, scripts/lib/platform.sh).
if [ "${OMS_NATIVE_DISPATCHED:-}" != 1 ] && { [ -e "$ROOT/rust/target/oms-native" ] || [ -L "$ROOT/rust/target/oms-native" ]; } &&
   [ -f "$ROOT/scripts/lib/platform.sh" ]; then
  # shellcheck source=scripts/lib/platform.sh
  . "$ROOT/scripts/lib/platform.sh"
  oms_native_route strict "$ROOT" snapshot "$@" || exit
fi

usage() {
  cat <<'EOF'
Usage: snapshot.sh [--cluster] [generator args...]

Write the private local context agents read for machine awareness.

Options:
  --cluster    Capture a Slurm cluster reference (local/slurm.md) instead of
               the default hardware/tooling snapshot (local/machine.md).
  -h, --help   Show this help.

All other arguments pass through to the underlying generator; see
write-machine-snapshot.sh and generate-slurm-reference.sh for theirs.
EOF
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
  --cluster)
    shift
    exec "$ROOT/scripts/generate-slurm-reference.sh" "$@"
    ;;
esac
exec "$ROOT/scripts/write-machine-snapshot.sh" "$@"
