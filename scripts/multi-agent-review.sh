#!/usr/bin/env bash
set -euo pipefail

# Deprecated name; renamed to peer-review.sh (2026-07). Shim kept so installed
# rules and old habits keep working; it will be removed in a future release.

echo "oh-my-setting: multi-agent-review.sh is deprecated; use peer-review.sh (oms peer-review)" >&2
exec bash "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/peer-review.sh" "$@"
