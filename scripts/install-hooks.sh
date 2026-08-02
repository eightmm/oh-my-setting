#!/usr/bin/env bash
set -euo pipefail

# Install a pre-push git hook. The safe default runs the complete local gate;
# --quick is for a branch protected by the required GitHub Actions `gate` job.
# Re-runnable.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  cat <<'EOF'
usage: install-hooks.sh [--full | --quick]

Install this repository's pre-push hook.

  --full   Run scripts/check.sh before every push (default).
  --quick  Run changed-file checks locally; requires the full GitHub Actions
           gate to be a protected-branch requirement.
EOF
}

mode=full
while [ "$#" -gt 0 ]; do
  case "$1" in
    --full) mode=full ;;
    --quick) mode=quick ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

hooks_dir="$ROOT/.git/hooks"

[ -d "$ROOT/.git" ] || { echo "error: not a git checkout: $ROOT" >&2; exit 2; }
mkdir -p "$hooks_dir"

if [ "$mode" = quick ]; then
  cat > "$hooks_dir/pre-push" <<'EOF'
#!/usr/bin/env bash
# Installed by scripts/install-hooks.sh --quick.
# This is partial local feedback; the full GitHub Actions gate must protect the
# destination branch. Bypass once with: git push --no-verify
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
exec "$root/scripts/pre-push-check.sh" "$@"
EOF
else
  cat > "$hooks_dir/pre-push" <<'EOF'
#!/usr/bin/env bash
# Installed by scripts/install-hooks.sh — runs the repo gate before every push.
# Bypass for an emergency with: git push --no-verify
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
exec "$root/scripts/check.sh"
EOF
fi
chmod +x "$hooks_dir/pre-push"

if [ "$mode" = quick ]; then
  echo "installed: $hooks_dir/pre-push -> scripts/pre-push-check.sh (quick)"
  echo "required safety net: protect the destination branch with CI job 'gate'"
else
  echo "installed: $hooks_dir/pre-push -> scripts/check.sh (full)"
fi
echo "bypass once with: git push --no-verify"
