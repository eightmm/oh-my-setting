#!/usr/bin/env bash
set -euo pipefail

# Project graph and execution graph over the existing OMS control plane:
# deterministic code structure queries, context packs, GraphSpec validation,
# pure route evaluation, and event-backed runs that call plan-run and
# patch-land instead of re-implementing them (docs/GRAPH-ENGINEERING.md).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/pycache-env.sh
. "$ROOT/scripts/lib/pycache-env.sh"
oms_pycache_env "$ROOT"
exec python3 "$ROOT/scripts/lib/oms_graph_core.py" "$@"
