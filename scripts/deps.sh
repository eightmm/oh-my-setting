#!/usr/bin/env bash
set -euo pipefail

# Show which version of a third-party package this project pins and has
# installed, with the documentation URLs the package itself declares. Reads
# root lockfiles and installed metadata only: nothing is fetched, imported
# or run, and nothing is written.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "${1:-}" in
  ""|-h|--help)
    cat <<'EOF'
Usage: oms deps show NAME [--repo PATH] [--json]

Print a card for one third-party package: the version each root lockfile
pins (requirements*.txt, constraints*.txt, uv.lock, poetry.lock, pdm.lock,
Pipfile.lock, environment*.yml, package-lock.json, Cargo.lock), the range
pyproject.toml declares, and for Python the installed version, location and
declared URLs from the package's own METADATA in ./.venv, ./venv,
VIRTUAL_ENV or CONDA_PREFIX.

Read-only. No interpreter of the project is started, no package is imported
and no URL is fetched; URLs are shown as the package declared them, without
credentials or query strings. Exit 0 when something was found, 1 when
nothing was, 2 on a bad request.
EOF
    [ -n "${1:-}" ] || exit 2
    exit 0
    ;;
esac

export PYTHONDONTWRITEBYTECODE=1
exec python3 -I "$ROOT/scripts/lib/deps_card.py" "$@"
