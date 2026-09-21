#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PYTHONDONTWRITEBYTECODE=1 python3 "$ROOT/tests/codex_hud_config_test.py"
# Same config.toml writer and the same gate: one stage owns both helpers.
PYTHONDONTWRITEBYTECODE=1 python3 "$ROOT/tests/codex_usage_config_test.py"
echo "codex-hud-config-smoke: ok"
