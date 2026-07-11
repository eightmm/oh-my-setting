#!/usr/bin/env bash
set -euo pipefail

# Admission gate for delegated patches. A worker's patch can be stale (built on
# a since-moved base), partial, or pass only under the worker's own assumptions.
# Before it touches the main tree, apply it in a throwaway worktree off the
# current HEAD and run a checks ladder: it must still APPLY, parse, and pass the
# project verification contract. Emit a compact admission report and a verdict;
# exit nonzero unless every gate passes. This is the trust boundary between
# peer-delegate (which produces patches) and landing them.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd "$ROOT" && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"
# shellcheck source=scripts/lib/harness-residue.sh
. "$ROOT_LIB/harness-residue.sh"
# shellcheck source=scripts/lib/oms-common.sh
. "$ROOT_LIB/oms-common.sh"
# shellcheck source=scripts/lib/peer-common.sh
. "$ROOT_LIB/peer-common.sh"

REPO="$PWD"
PATCH=""
VERIFY=""
ML=0
ALLOW_VERIFIER_CHANGE=0
KEEP_WORKTREE=0
REPORT=""
PLAN_TASK=""
EXECUTOR_ID=""
SCOPE_ALLOWED=""
SCOPE_FORBIDDEN=""
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
  --plan-task ID  Enforce this agent-plan task's allowed/forbidden paths.
  --executor ID   Enforce a frozen executor's scope and soul hash.
  --keep-worktree  Keep the worktree for inspection.
  --allow-verifier-change  Admit a patch that modifies the verify command's
                 own files (normally rejected: it could self-certify).
  -h, --help     Show this help.

Ladder: patch applies cleanly (not stale) -> changed shell files parse
(bash -n) -> patch does not modify its own verifier -> verification command
passes. Exit 0 only if every gate passes.
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
    --plan-task) [ "$#" -ge 2 ] || fail "--plan-task requires id"; PLAN_TASK="$2"; shift 2 ;;
    --executor) [ "$#" -ge 2 ] || fail "--executor requires id"; EXECUTOR_ID="$2"; shift 2 ;;
    --keep-worktree) KEEP_WORKTREE=1; shift ;;
    --allow-verifier-change) ALLOW_VERIFIER_CHANGE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$PATCH" ] || fail "--patch is required"
[ -f "$PATCH" ] || fail "patch not found: $PATCH"
REPO="$(cd "$REPO" && pwd)" || fail "bad --repo"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"
PATCH="$(cd "$(dirname "$PATCH")" && pwd)/$(basename "$PATCH")"

if [ -n "$PLAN_TASK" ]; then
  case "$PLAN_TASK" in *[!A-Za-z0-9._-]*|"") fail "--plan-task must match [A-Za-z0-9._-]+" ;; esac
  plan_json="$($ROOT/scripts/agent-plan.sh --repo "$REPO" show --id "$PLAN_TASK")" || fail "cannot read plan task $PLAN_TASK"
  SCOPE_ALLOWED="$(printf '%s' "$plan_json" | python3 -c 'import json,sys;print(",".join(json.load(sys.stdin).get("allowed_paths",[])))')"
  SCOPE_FORBIDDEN="$(printf '%s' "$plan_json" | python3 -c 'import json,sys;print(",".join(json.load(sys.stdin).get("forbidden_paths",[])))')"
  export OMS_TASK_ID="$PLAN_TASK"
fi
if [ -n "$EXECUTOR_ID" ]; then
  case "$EXECUTOR_ID" in *[!A-Za-z0-9._-]*|"") fail "--executor must match [A-Za-z0-9._-]+" ;; esac
  "$ROOT/scripts/agent-executor.sh" validate --repo "$REPO" --id "$EXECUTOR_ID" >/dev/null ||
    fail "executor $EXECUTOR_ID failed frozen validation"
  executor_json="$($ROOT/scripts/agent-executor.sh show --repo "$REPO" --id "$EXECUTOR_ID")"
  executor_values="$(printf '%s' "$executor_json" | python3 -c 'import json,sys;d=json.load(sys.stdin);print("\t".join([",".join(d.get("allowed_paths",[])),",".join(d.get("forbidden_paths",[])),d.get("task_id",""),d.get("soul_sha256","")]))')"
  executor_allowed="$(printf '%s' "$executor_values" | cut -f1)"
  executor_forbidden="$(printf '%s' "$executor_values" | cut -f2)"
  executor_task="$(printf '%s' "$executor_values" | cut -f3)"
  executor_soul_sha="$(printf '%s' "$executor_values" | cut -f4)"
  [ -z "$PLAN_TASK" ] || [ -z "$executor_task" ] || [ "$PLAN_TASK" = "$executor_task" ] ||
    fail "executor task conflicts with --plan-task"
  [ -z "$SCOPE_ALLOWED" ] || [ "$SCOPE_ALLOWED" = "$executor_allowed" ] || fail "executor allowed scope conflicts with plan task"
  [ -z "$SCOPE_FORBIDDEN" ] || [ "$SCOPE_FORBIDDEN" = "$executor_forbidden" ] || fail "executor forbidden scope conflicts with plan task"
  SCOPE_ALLOWED="$executor_allowed"; SCOPE_FORBIDDEN="$executor_forbidden"
  export OMS_EXECUTOR_ID="$EXECUTOR_ID" OMS_SOUL_SHA256="$executor_soul_sha"
fi

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

# --- Gate 1b: patch carries no secrets (added lines only) -------------------
# A patch that applies and verifies can still smuggle a credential onto the
# main tree. Scan the added lines with the shared sensitive regex before any
# worktree work. This is a landing-side mirror of the outbound scrubber.
secret_scan_file="$(mktemp)" || fail "mktemp failed"
grep -E '^\+' "$PATCH" | grep -Ev '^\+\+\+ ' > "$secret_scan_file" || true
if agent_memory_file_has_sensitive_content "$secret_scan_file"; then
  record "secrets" "FAIL" "added lines contain sensitive-looking content (secret/key/token/private path)"
else
  record "secrets" "PASS" "no sensitive-looking content in added lines"
fi
rm -f "$secret_scan_file"

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
    # The apply into the worktree must succeed, otherwise the syntax/verify
    # gates below would run against the UNPATCHED tree and pass vacuously. Gate
    # 1 only checks apply against HEAD; the worktree tree state can still differ.
    if git -C "$worktree" apply --binary "$PATCH" >/dev/null 2>&1; then
    # git apply --numstat is TAB-delimited (add<TAB>del<TAB>path); split on the
    # tab so a path containing spaces is not truncated (which would silently
    # skip its syntax/verifier check).
    # numstat names additions/destinations, while the applied worktree diff
    # with rename detection disabled exposes deleted/renamed source paths.
    # Union both so moving a forbidden file into an allowed path cannot hide
    # the forbidden source side of the patch.
    changed_files="$({
      git -C "$REPO" apply --numstat "$PATCH" 2>/dev/null | awk -F '\t' '{print $3}'
      git -C "$worktree" diff --name-only --no-renames HEAD -- 2>/dev/null
    } | LC_ALL=C sort -u)"

    # --- Gate 2: task/executor path scope ----------------------------------
    scope_detail="$(OMS_CHANGED="$changed_files" OMS_ALLOWED="$SCOPE_ALLOWED" OMS_FORBIDDEN="$SCOPE_FORBIDDEN" python3 - <<'PY'
import fnmatch, os, re
changed=[x for x in os.environ.get("OMS_CHANGED","").splitlines() if x]
def split(v): return [x for x in re.split(r"[,\s]+",v) if x]
allowed, forbidden=split(os.environ.get("OMS_ALLOWED","")),split(os.environ.get("OMS_FORBIDDEN",""))
def match(path, pattern):
    if pattern in (".", "./"): return True
    p=pattern[2:] if pattern.startswith("./") else pattern
    if any(c in p for c in "*?["): return fnmatch.fnmatchcase(path,p)
    p=p.rstrip("/")
    return path == p or path.startswith(p + "/")
bad=[]
for path in changed:
    if any(match(path,p) for p in forbidden): bad.append("forbidden: " + path); continue
    if allowed and not any(match(path,p) for p in allowed): bad.append("outside allowed paths: " + path)
print("; ".join(bad))
PY
)"
    if [ -n "$scope_detail" ]; then
      record "scope" "FAIL" "$scope_detail"
    elif [ -n "$SCOPE_ALLOWED$SCOPE_FORBIDDEN" ]; then
      record "scope" "PASS" "changed files satisfy task/executor scope"
    else
      record "scope" "SKIP" "no task/executor scope supplied"
    fi

    # --- Gate 3: changed syntax-checked files parse -------------------------
    syntax_ok=1
    syntax_checked=0
    syntax_detail="no syntax-checked files changed"
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
            syntax_checked=1
          fi
          ;;
        *.py)
          if [ -f "$worktree/$f" ] && command -v python3 >/dev/null 2>&1; then
            if ! python3 -m py_compile "$worktree/$f" 2>/dev/null; then
              syntax_ok=0
              syntax_detail="python compile failed: $f"
              break
            fi
            syntax_checked=1
          fi
          ;;
        *.json)
          if [ -f "$worktree/$f" ] && command -v python3 >/dev/null 2>&1; then
            if ! python3 -m json.tool "$worktree/$f" >/dev/null 2>&1; then
              syntax_ok=0
              syntax_detail="json parse failed: $f"
              break
            fi
            syntax_checked=1
          fi
          ;;
      esac
    done <<EOF
$changed_files
EOF
    if [ "$syntax_ok" = 1 ] && [ "$syntax_checked" = 1 ]; then
      syntax_detail="changed shell/python/json files parse"
    fi
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

    # --- Gate 3a: the patch must not modify its own verifier ----------------
    # Gate 3 runs VERIFY inside the PATCHED worktree, so a patch that rewrites
    # the verify entrypoint (e.g. scripts/check.sh -> exit 0) would certify
    # itself. Flag a changed file if VERIFY names it (by path or basename, so
    # `cd scripts && bash check.sh` and absolute spellings do not bypass it) or
    # if it is a common build entrypoint whose edit silently changes what
    # "verify" does. --allow-verifier-change is the explicit override.
    verifier_hit=""
    if [ -n "$VERIFY" ] && [ "$ALLOW_VERIFIER_CHANGE" = 0 ] && command -v python3 >/dev/null 2>&1; then
      verifier_hit="$(OMS_VERIFY="$VERIFY" OMS_CHANGED="$changed_files" python3 - <<'PY'
import os
try:
    import shlex
    toks = shlex.split(os.environ["OMS_VERIFY"])
except Exception:
    toks = os.environ["OMS_VERIFY"].split()
named = set()
for t in toks:
    if "/" in t or "." in t:
        named.add(t)
        named.add(t.lstrip("./"))
        named.add(os.path.basename(t))
ENTRY = {"check.sh", "Makefile", "makefile", "GNUmakefile", "package.json",
         "pyproject.toml", "tox.ini", "noxfile.py", "conftest.py",
         "setup.py", "setup.cfg", "justfile", "Justfile"}
for f in os.environ["OMS_CHANGED"].splitlines():
    f = f.strip()
    if not f:
        continue
    if f in named or os.path.basename(f) in named or os.path.basename(f) in ENTRY:
        print(f)
        break
PY
)"
    fi
    if [ -n "$verifier_hit" ]; then
      record "verifier" "FAIL" "patch modifies its own verifier: $verifier_hit (override: --allow-verifier-change)"
      record "verify" "SKIP" "not run: verifier integrity gate failed"
    elif [ -n "$VERIFY" ]; then
      record "verifier" "PASS" "patch leaves the verify command's files untouched"
      if verify_out="$(cd "$worktree" && run_verify_with_timeout bash -c "$VERIFY" 2>&1)"; then
        record "verify" "PASS" "$VERIFY"
      else
        record "verify" "FAIL" "$VERIFY"
      fi
    else
      record "verify" "SKIP" "no --verify and no scripts/check.sh"
    fi
    else
      record "apply-worktree" "FAIL" "patch did not apply to the admission worktree (tree state differs from HEAD)"
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

# Index the admission so the audit trail survives `artifact-index.sh prune
# --files` (which removes unreferenced files under .oms/artifacts/).
admit_exit=1
[ "$verdict" = "ADMIT" ] && admit_exit=0
ma_append_artifact_index "$REPO" patch-admit "" "$admit_exit" "$REPORT" "$PATCH" || true

echo "patch-admit: $verdict ($REPORT)" >&2
printf '%s\n' "$verdict"
[ "$verdict" = "ADMIT" ]
