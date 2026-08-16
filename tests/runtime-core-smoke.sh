#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH="$ROOT/scripts/lib" python3 -m unittest discover -v -s "$ROOT/tests" -p "test_oms_runtime_*.py"

# Exercise the real shell entrypoint and JSON surface, not only imports.
tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-runtime-cli.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM HUP
git -C "$tmp" init -q
git -C "$tmp" config user.email test@example.com
git -C "$tmp" config user.name test
cat > "$tmp/PROJECT.md" <<'EOF'
# Fixture

## Goal

Check the CLI.

## Acceptance Criteria

- [id:cli-json] Envelope is JSON.
EOF
git -C "$tmp" add PROJECT.md
git -C "$tmp" commit -qm fixture
"$ROOT/scripts/runtime-core.sh" --repo "$tmp" envelope show | python3 -c 'import json,sys; row=json.load(sys.stdin); assert row["schema"]==2'
"$ROOT/scripts/runtime-core.sh" --repo "$tmp" failure classify 'verification failed' | python3 -c 'import json,sys; assert json.load(sys.stdin)["code"]=="verifier_failed"'
"$ROOT/scripts/runtime-core.sh" --repo "$tmp" backend run trusted-local --timeout-seconds 10 -- python3 -c 'print("ok")' | python3 -c 'import json,sys; row=json.load(sys.stdin); assert row["exit"]==0'

echo "runtime-core-smoke: ok"
