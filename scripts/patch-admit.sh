#!/usr/bin/env bash
set -euo pipefail

# Admission gate for delegated patches. A worker's patch can be stale (built on
# a since-moved base), partial, or pass only under the worker's own assumptions.
# Before it touches the main tree, apply it in a throwaway worktree off the
# current HEAD and run a checks ladder: it must still APPLY, parse, and pass the
# project verification contract. Emit a compact admission report and a verdict;
# exit nonzero unless every gate passes. This is the trust boundary between
# multi-agent-delegate (which produces patches) and landing them.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd "$ROOT" && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"
# shellcheck source=scripts/lib/harness-residue.sh
. "$ROOT_LIB/harness-residue.sh"
# shellcheck source=scripts/lib/oms-common.sh
. "$ROOT_LIB/oms-common.sh"

REPO="$PWD"
PATCH=""
VERIFY=""
ML=0
KEEP_WORKTREE=0
REPORT=""
worktree_parent=""
worktree=""
worktree_created=0
cleanup_done=0

usage() {
  cat <<'EOF'
Usage: patch-admit.sh --patch FILE [options]

Apply a delegated patch in an isolated worktree off the current HEAD and run a
checks ladder before it is allowed onto the main tree.

Options:
  --patch FILE   Patch file to admit (required).
  --repo PATH    Target git repo (default: current directory).
  --verify CMD   Verification command run in the worktree after applying.
                 Default: scripts/check.sh <ml-smoke|fast> when present.
  --ml           Prefer the ml-smoke verification mode when auto-detecting.
  --report FILE  Write the admission report here (default: .oms/artifacts/admit/).
  --keep-worktree  Keep the worktree for inspection.
  -h, --help     Show this help.

Ladder: patch applies cleanly (not stale) -> changed shell files parse
(bash -n) -> verification command passes. Exit 0 only if every gate passes.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  if [ -n "$worktree" ] && [ "$worktree_created" = 1 ] && [ "$KEEP_WORKTREE" = 0 ]; then
    git -C "$REPO" worktree remove --force "$worktree" >/dev/null 2>&1 || true
  fi
  if [ -n "$worktree_parent" ] && [ "$KEEP_WORKTREE" = 0 ]; then
    rm -rf "$worktree_parent"
  fi
}
cleanup_signal() {
  local code="$1"
  trap - EXIT HUP INT TERM
  cleanup
  exit "$code"
}
trap cleanup EXIT
trap 'cleanup_signal 129' HUP
trap 'cleanup_signal 130' INT
trap 'cleanup_signal 143' TERM

while [ "$#" -gt 0 ]; do
  case "$1" in
    --patch) [ "$#" -ge 2 ] || fail "--patch requires a file"; PATCH="$2"; shift 2 ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --verify) [ "$#" -ge 2 ] || fail "--verify requires a command"; VERIFY="$2"; shift 2 ;;
    --ml) ML=1; shift ;;
    --report) [ "$#" -ge 2 ] || fail "--report requires a path"; REPORT="$2"; shift 2 ;;
    --keep-worktree) KEEP_WORKTREE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$PATCH" ] || fail "--patch is required"
[ -f "$PATCH" ] || fail "patch not found: $PATCH"
REPO="$(cd "$REPO" && pwd)" || fail "bad --repo"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"
PATCH="$(cd "$(dirname "$PATCH")" && pwd)/$(basename "$PATCH")"

base_sha="$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo 'no-commit')"
patch_sha="$(oms_sha256_stream < "$PATCH" 2>/dev/null | cut -c1-16)"
[ -n "$patch_sha" ] || patch_sha="nohash"

# Verdict accumulator.
ladder=""
verdict="ADMIT"
record() {
  # record GATE STATUS DETAIL. Verdict is computed from the ladder at the end;
  # only FAIL rejects (SKIP is neutral).
  ladder="$ladder$(printf '%s\t%s\t%s\n' "$1" "$2" "$3")
"
}

# --- Gate 1: applies cleanly to current HEAD (staleness check) --------------
if git -C "$REPO" apply --check --binary "$PATCH" >/dev/null 2>&1; then apply_ok=1; else apply_ok=0; fi
if [ "$apply_ok" = 1 ]; then
  record "apply" "PASS" "patch applies cleanly to $base_sha"
else
  record "apply" "FAIL" "patch does not apply to $base_sha (stale or conflicting)"
fi

changed_files=""
verify_out=""
verify_mode=""
if [ "$apply_ok" = 1 ]; then
  oms_harness_prune_stale_worktrees "$REPO" 0 >/dev/null 2>&1 || true
  worktree_parent="$(mktemp -d "${TMPDIR:-/tmp}/oh-my-setting-admit.XXXXXX")" || fail "mktemp failed"
  worktree="$worktree_parent/wt"
  oms_harness_mark_tmpdir "$worktree_parent" "$REPO" "$worktree"
  if git -C "$REPO" worktree add --quiet --detach "$worktree" HEAD >/dev/null 2>&1; then
    worktree_created=1
    git -C "$worktree" apply --binary "$PATCH" >/dev/null 2>&1 || true
    changed_files="$(git -C "$REPO" apply --numstat "$PATCH" 2>/dev/null | awk '{print $3}')"

    # --- Gate 2: changed shell files parse (bash -n) ------------------------
    syntax_ok=1
    syntax_detail="no shell files changed"
    while IFS= read -r f; do
      [ -n "$f" ] || continue
      case "$f" in
        *.sh)
          if [ -f "$worktree/$f" ]; then
            if ! bash -n "$worktree/$f" 2>/dev/null; then
              syntax_ok=0
              syntax_detail="bash -n failed: $f"
              break
            fi
            syntax_detail="changed shell files parse"
          fi
          ;;
      esac
    done <<EOF
$changed_files
EOF
    [ "$syntax_ok" = 1 ] && record "syntax" "PASS" "$syntax_detail" \
      || record "syntax" "FAIL" "$syntax_detail"

    # --- Gate 3: verification contract --------------------------------------
    if [ -z "$VERIFY" ] && [ -x "$worktree/scripts/check.sh" ]; then
      verify_mode="fast"
      if [ "$ML" = 1 ] && oms_check_sh_has_ml_smoke "$worktree/scripts/check.sh"; then
        verify_mode="ml-smoke"
      fi
      VERIFY="bash scripts/check.sh $verify_mode"
    fi
    if [ -n "$VERIFY" ]; then
      if verify_out="$(cd "$worktree" && bash -c "$VERIFY" 2>&1)"; then
        record "verify" "PASS" "$VERIFY"
      else
        record "verify" "FAIL" "$VERIFY"
      fi
    else
      record "verify" "SKIP" "no --verify and no scripts/check.sh"
    fi
  else
    record "worktree" "FAIL" "could not create admission worktree"
  fi
fi

# REJECT if any gate failed (SKIP does not reject).
case "$ladder" in
  *"	FAIL	"*) verdict="REJECT" ;;
esac

# --- Report -----------------------------------------------------------------
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
if [ -z "$REPORT" ]; then
  REPORT="$REPO/.oms/artifacts/admit/admit-$patch_sha-$(printf '%s' "$ts" | tr -c 'A-Za-z0-9' '-').md"
  agent_memory_ensure_oms_ignore_for_path "$REPO/.oms/artifacts/admit" 2>/dev/null || true
fi
mkdir -p "$(dirname "$REPORT")"
{
  printf '# Patch admission: %s\n\n' "$verdict"
  printf -- '- patch: %s\n' "$PATCH"
  printf -- '- patch_sha: %s\n' "$patch_sha"
  printf -- '- base: %s\n' "$base_sha"
  printf -- '- checked: %s\n\n' "$ts"
  printf '## Ladder\n\n'
  printf '%s' "$ladder" | while IFS=$'\t' read -r gate status detail; do
    [ -n "$gate" ] || continue
    printf -- '- %s: %s — %s\n' "$gate" "$status" "$detail"
  done
  if [ -n "$changed_files" ]; then
    printf '\n## Changed files\n\n'
    printf '%s\n' "$changed_files" | while IFS= read -r f; do
      [ -n "$f" ] && printf -- '- %s\n' "$f"
    done
  fi
  if [ -n "$verify_out" ]; then
    printf '\n## Verify output (tail)\n\n```\n%s\n```\n' "$(printf '%s' "$verify_out" | tail -n 40)"
  fi
} > "$REPORT"

echo "patch-admit: $verdict ($REPORT)" >&2
printf '%s\n' "$verdict"
[ "$verdict" = "ADMIT" ]
