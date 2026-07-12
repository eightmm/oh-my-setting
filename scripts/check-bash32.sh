#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

hits="$(grep -rnE 'declare[[:space:]]+-A|mapfile|readarray|\$\{[A-Za-z_][A-Za-z0-9_]*(\^\^|,,)' \
  install.sh scripts/oms scripts/*.sh scripts/lib/*.sh \
  plugins/oh-my-setting/scripts/*.sh templates/*.sh tests/*.sh \
  | grep -vE ':[0-9]+:[[:space:]]*#|^scripts/check-bash32\.sh:' || true)"
if [ -n "$hits" ]; then
  echo "bash-4-only constructs found (must be bash 3.2 compatible):" >&2
  echo "$hits" >&2
  exit 1
fi

echo "bash-3.2-static: ok"
