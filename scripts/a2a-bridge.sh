#!/usr/bin/env bash
set -euo pipefail

# Run the optional A2A v1 HTTP+JSON read bridge on an explicit loopback socket.
# This command never installs a daemon or opens a non-loopback listener.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
exec python3 "$ROOT/scripts/lib/a2a-readonly.py" serve "$@"
