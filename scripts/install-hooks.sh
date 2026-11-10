#!/usr/bin/env bash
set -euo pipefail

# Install a pre-push git hook. The safe default runs the complete local gate;
# --quick is for a branch protected by the required GitHub Actions `gate` job.
# Re-runnable.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  cat <<'EOF'
usage: install-hooks.sh [--quick [--check-protection]]

Install this repository's pre-push hook.

  (default) Run scripts/check.sh before pushes except published tags.
  --quick  Run partial changed-file checks locally; requires the risk-based
           GitHub Actions gate to be a protected-branch requirement. Prints
           the read-only command that checks it: oms ci-status protection
  --check-protection
           With --quick, run that probe once (it uses the network through gh)
           and install the quick hook only when it exits 0 (proven). Absent
           (1) and unknown (2) both refuse, showing the probe's own line.
           Without this flag nothing contacts the network.
EOF
}

mode=full
check_protection=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --quick) mode=quick ;;
    --check-protection) check_protection=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

if [ "$check_protection" -eq 1 ] && [ "$mode" != quick ]; then
  echo "error: --check-protection only applies together with --quick" >&2
  exit 2
fi

if [ "$check_protection" -eq 1 ]; then
  # The one network use of this script, and only on request. OMS_CI_STATUS_BIN
  # replaces the probe for tests.
  probe_rc=0
  probe_out="$(cd "$ROOT" && "${OMS_CI_STATUS_BIN:-$ROOT/scripts/ci-status.sh}" protection 2>&1)" || probe_rc=$?
  if [ "$probe_rc" -ne 0 ]; then
    printf '%s\n' "${probe_out%%$'\n'*}" >&2
    case "$probe_rc" in
      1) echo "refused: the destination branch is not proven to require CI job 'gate' (probe: absent); quick hook not installed" >&2 ;;
      *) echo "refused: could not prove that the destination branch requires CI job 'gate' (probe exit $probe_rc: unknown); quick hook not installed" >&2 ;;
    esac
    exit 1
  fi
  printf '%s\n' "${probe_out%%$'\n'*}"
fi

hooks_dir="$(git -C "$ROOT" rev-parse --git-path hooks 2>/dev/null)" ||
  { echo "error: not a git checkout: $ROOT" >&2; exit 2; }
hooks_dir="${hooks_dir//$'\r'/}"
case "$hooks_dir" in /*|[A-Za-z]:/*) ;; *) hooks_dir="$ROOT/$hooks_dir" ;; esac
mkdir -p "$hooks_dir"
hooks_dir="$(cd "$hooks_dir" && pwd -P)"

if [ "$mode" = quick ]; then
  cat > "$hooks_dir/pre-push" <<'EOF'
#!/usr/bin/env bash
# Installed by scripts/install-hooks.sh --quick.
# This is partial local feedback; the risk-based CI gate must protect the
# destination branch. Bypass once with: git push --no-verify
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
exec "$root/scripts/pre-push-check.sh" "$@"
EOF
else
  cat > "$hooks_dir/pre-push" <<'EOF'
#!/usr/bin/env bash
# Installed by scripts/install-hooks.sh — runs the full gate except for published tags.
# Bypass for an emergency with: git push --no-verify
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
exec "$root/scripts/pre-push-check.sh" --full "$@"
EOF
fi
chmod +x "$hooks_dir/pre-push"

if [ "$mode" = quick ]; then
  echo "installed: $hooks_dir/pre-push -> scripts/pre-push-check.sh (quick)"
  echo "required safety net: protect the destination branch with CI job 'gate'"
  [ "$check_protection" -eq 1 ] ||
    echo "check it first (read-only): oms ci-status protection   (or rerun with --quick --check-protection)"
else
  echo "installed: $hooks_dir/pre-push -> scripts/pre-push-check.sh (full)"
fi
echo "single-gate release: oms land --repo CHECKOUT (checks, pushes the same HEAD, then follows CI)"
