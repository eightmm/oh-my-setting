#!/usr/bin/env bash
set -euo pipefail

# Install a pre-push git hook that runs the repo verification gate
# (scripts/check.sh) so red never reaches the remote. Re-runnable.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
hooks_dir="$ROOT/.git/hooks"

[ -d "$ROOT/.git" ] || { echo "error: not a git checkout: $ROOT" >&2; exit 2; }
mkdir -p "$hooks_dir"

cat > "$hooks_dir/pre-push" <<'EOF'
#!/usr/bin/env bash
# Installed by scripts/install-hooks.sh — runs the repo gate before every push.
# Bypass for an emergency with: git push --no-verify
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
exec "$root/scripts/check.sh"
EOF
chmod +x "$hooks_dir/pre-push"

echo "installed: $hooks_dir/pre-push -> scripts/check.sh"
echo "bypass once with: git push --no-verify"
