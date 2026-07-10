#!/usr/bin/env bash
set -euo pipefail

# Deprecated name; renamed to peer-delegate.sh (2026-07). Shim kept so installed
# rules and old habits keep working; it will be removed in a future release.

echo "oh-my-setting: multi-agent-delegate.sh is deprecated; use peer-delegate.sh (oms peer-delegate)" >&2
exec bash "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/peer-delegate.sh" "$@"
