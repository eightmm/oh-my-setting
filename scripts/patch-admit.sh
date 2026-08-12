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
# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT_LIB/work-journal.sh"

REPO="$PWD"
PATCH=""
VERIFY=""
ML=0
ALLOW_VERIFIER_CHANGE=0
ALLOW_TEST_REDUCTION=0
ALLOW_RESTRUCTURE=0
KEEP_WORKTREE=0
REPORT=""
PLAN_TASK=""
EXECUTOR_ID=""
SCOPE_ALLOWED=""
SCOPE_FORBIDDEN=""
ACCEPTANCE_FILES=""
worktree_parent=""
worktree=""
worktree_created=0
floor_worktree_parent=""
floor_worktree=""
floor_worktree_created=0
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
  --allow-verifier-change  Permit a patch that modifies the verify command's
                 own files. The base-owned verification floor still runs.
  --allow-test-reduction  Admit a patch that net-removes test assertions or
                 deletes a test file (normally rejected: passing by deleting
                 the check is the other way to self-certify). The base-owned
                 verification floor still runs.
  --allow-restructure  Admit a patch that adds a new top-level file or moves a
                 file across directories (normally rejected when no scope is
                 supplied: nothing else says where the patch may write).
  -h, --help     Show this help.

Ladder: patch applies cleanly (not stale) -> changed files stay in scope ->
changed shell files parse (bash -n) -> patch does not weaken the tests ->
candidate verification passes -> changed verification surfaces also pass with
those changes restored from HEAD. Exit 0 only if every gate passes. Without
--plan-task/--executor there is no declared scope, so the structural floor
stands in for it: keep the existing layout.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  if [ -n "$floor_worktree" ] && [ "$floor_worktree_created" = 1 ] && [ "$KEEP_WORKTREE" = 0 ]; then
    git -C "$REPO" worktree remove --force "$floor_worktree" >/dev/null 2>&1 || true
  fi
  if [ -n "$floor_worktree_parent" ] && [ "$KEEP_WORKTREE" = 0 ]; then
    rm -rf "$floor_worktree_parent"
  fi
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
    --allow-test-reduction) ALLOW_TEST_REDUCTION=1; shift ;;
    --allow-restructure) ALLOW_RESTRUCTURE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$PATCH" ] || fail "--patch is required"
[ -f "$PATCH" ] || fail "patch not found: $PATCH"
REPO="$(cd "$REPO" && pwd)" || fail "bad --repo"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"
PATCH="$(cd "$(dirname "$PATCH")" && pwd)/$(basename "$PATCH")"
oms_git_assert_safe_execution_config "$REPO" ||
  fail "unsafe executable Git config; remove it before patch admission"
oms_git_assert_plain_index "$REPO" ||
  fail "Git index has hidden or unreadable entries; clear them before patch admission"

if [ -n "$PLAN_TASK" ]; then
  case "$PLAN_TASK" in *[!A-Za-z0-9._-]*|"") fail "--plan-task must match [A-Za-z0-9._-]+" ;; esac
  plan_json="$($ROOT/scripts/agent-plan.sh --repo "$REPO" show --id "$PLAN_TASK")" || fail "cannot read plan task $PLAN_TASK"
  SCOPE_ALLOWED="$(printf '%s' "$plan_json" | python3 -c 'import json,sys;print(",".join(json.load(sys.stdin).get("allowed_paths",[])))')"
  SCOPE_FORBIDDEN="$(printf '%s' "$plan_json" | python3 -c 'import json,sys;print(",".join(json.load(sys.stdin).get("forbidden_paths",[])))')"
  ACCEPTANCE_FILES="$(printf '%s' "$plan_json" | python3 -c 'import json,sys; d=json.load(sys.stdin); c=d.get("project_contract") or {}; print("\n".join(c.get("acceptance_files") or d.get("acceptance_files") or []))')"
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

base_full_sha="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || true)"
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
floor_verify_out=""
verify_mode=""
if [ "$apply_ok" = 1 ]; then
  oms_harness_prune_stale_worktrees "$REPO" 0 >/dev/null 2>&1 || true
  worktree_parent="$(mktemp -d "${TMPDIR:-/tmp}/oh-my-setting-admit.XXXXXX")" || fail "mktemp failed"
  worktree="$worktree_parent/wt"
  oms_harness_mark_tmpdir "$worktree_parent" "$REPO" "$worktree"
  if GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
    GIT_CONFIG_NOSYSTEM=1 git -c core.hooksPath=/dev/null \
    -c core.fsmonitor=false -C "$REPO" worktree add --quiet --detach \
    "$worktree" "${base_full_sha:-HEAD}" >/dev/null 2>&1; then
    worktree_created=1
    # Local-only agent files (project-private.sh) are not in HEAD; the verify
    # command may read PROJECT.md, so seed them the same way delegate does.
    oms_seed_local_agent_files "$REPO" "$worktree"
    # Auto-detection belongs to the trusted base, not to the patch being
    # admitted. Otherwise deleting or rewriting check.sh can also rewrite the
    # command that is supposed to judge that patch.
    if [ -z "$VERIFY" ] && [ -x "$worktree/scripts/check.sh" ]; then
      verify_mode=""
      if oms_check_sh_has_fast_mode "$worktree/scripts/check.sh"; then
        verify_mode="fast"
      fi
      if [ "$ML" = 1 ] && oms_check_sh_has_ml_smoke "$worktree/scripts/check.sh"; then
        verify_mode="ml-smoke"
      fi
      VERIFY="bash scripts/check.sh${verify_mode:+ $verify_mode}"
    fi
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
      GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
        GIT_CONFIG_NOSYSTEM=1 git -c core.fsmonitor=false -c diff.external= \
        -C "$worktree" diff --no-ext-diff --no-textconv --name-only \
        --no-renames HEAD -- 2>/dev/null
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

    # --- Gate 2b: structural floor when no scope constrains the paths -------
    # The scope gate is the only one that says WHERE a patch may write, and
    # with no --plan-task/--executor it has nothing to enforce, so any layout
    # change was admitted: a worker patch with malformed headers applied as a
    # rename and moved two test files to the repo root. When nothing declares
    # a scope, the changed set must still read as editing this tree rather
    # than rearranging it — no brand-new top-level file, no move across
    # directories. Judged from the applied worktree, not the patch headers,
    # because the headers are what lied.
    if [ -z "$SCOPE_ALLOWED$SCOPE_FORBIDDEN" ]; then
      # The base listing is repo-sized rather than patch-sized, so it travels
      # by file: --repo is arbitrary, and a few thousand paths would exceed the
      # per-string environment limit the other gates never approach.
      base_files="$worktree_parent/base-files.txt"
      git -C "$worktree" ls-tree -r --name-only HEAD > "$base_files" 2>/dev/null || true
      structure_detail="$(OMS_CHANGED="$changed_files" OMS_BASE_FILE="$base_files" OMS_WORKTREE="$worktree" python3 - <<'PY'
import os
changed=[x for x in os.environ.get("OMS_CHANGED","").splitlines() if x]
with open(os.environ["OMS_BASE_FILE"], errors="replace") as handle:
    base=set(x for x in handle.read().splitlines() if x)
wt=os.environ["OMS_WORKTREE"]
added, removed = [], []
for path in changed:
    here=os.path.lexists(os.path.join(wt, path))
    if here and path not in base: added.append(path)
    elif not here and path in base: removed.append(path)
bad=["new top-level file: " + p for p in added if "/" not in p]
for src in removed:
    for dst in added:
        if os.path.basename(src) == os.path.basename(dst) and os.path.dirname(src) != os.path.dirname(dst):
            bad.append("moved across directories: %s -> %s" % (src, dst))
print("; ".join(bad))
PY
)"
      if [ -z "$structure_detail" ]; then
        record "structure" "PASS" "changed files keep the existing layout"
      elif [ "$ALLOW_RESTRUCTURE" = 1 ]; then
        record "structure" "PASS" "restructure permitted by --allow-restructure: $structure_detail"
      else
        record "structure" "FAIL" "patch restructures the tree: $structure_detail (override: --allow-restructure)"
      fi
    else
      record "structure" "SKIP" "declared scope enforced instead"
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

    # --- Gate 3a: identify the automatic verification surface ---------------
    # A candidate verifier is allowed to exercise its own proposed tests, but
    # it cannot be the only authority over them. The floor below treats paths
    # directly named by VERIFY, common verifier configs, and test/spec paths as
    # one surface. This scan is independent of both --allow-* flags: overrides
    # may permit the edit, but they never suppress the base-owned second run.
    verifier_hit=""
    verification_surface=""
    surface_scan=""
    if [ -n "$VERIFY" ] && command -v python3 >/dev/null 2>&1; then
      surface_scan="$(OMS_VERIFY="$VERIFY" OMS_CHANGED="$changed_files" \
        OMS_ACCEPTANCE_FILES="$ACCEPTANCE_FILES" python3 - <<'PY' | tr -d '\r'
import os
import re
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
reviewed = set(x for x in os.environ.get("OMS_ACCEPTANCE_FILES", "").splitlines() if x)
ENTRY = {"check.sh", "Makefile", "makefile", "GNUmakefile", "package.json",
         "pyproject.toml", "tox.ini", "noxfile.py", "conftest.py",
         "setup.py", "setup.cfg", "justfile", "Justfile"}
# Top-level verifiers commonly fan out to repo-owned helpers without naming
# them in the command (for example check.sh -> check-python.sh). Treat the
# conventional helper family as verifier authority wherever it lives. This is
# intentionally narrower than a generic "contains check" rule so ordinary
# product modules such as checksum.py do not become false verification gates.
HELPER_PATH = re.compile(
    r"(^|/)(?:run[-_.])?(?:checks?|tests?|verify|lint)"
    r"(?:[-_.][^/]*)?\.(?:sh|bash|py|js|cjs|mjs|ts|tsx|rb|pl|ps1)$",
    re.IGNORECASE,
)
TEST_PATH = re.compile(
    r"(^|/)(tests?|spec)(/|$)|(^|/)test_[^/]+$|"
    r"[^/]*(_test|_spec|\.test|\.spec)\.[^/.]+$"
)
for f in os.environ["OMS_CHANGED"].splitlines():
    if not f:
        continue
    direct = f in named or os.path.basename(f) in named
    common = os.path.basename(f) in ENTRY or bool(HELPER_PATH.search(f))
    explicit = f in reviewed
    if direct or common or explicit:
        print("verifier\t" + f)
    if direct or common or explicit or TEST_PATH.search(f):
        print("surface\t" + f)
PY
)"
      surface_tab="$(printf '\t')"
      verifier_hit="$(printf '%s\n' "$surface_scan" | awk -F "$surface_tab" '$1 == "verifier" {print substr($0, length($1) + 2); exit}')"
      verification_surface="$(printf '%s\n' "$surface_scan" | awk -F "$surface_tab" '$1 == "surface" {print substr($0, length($1) + 2)}')"
    fi
    # --- Gate 3b: the patch must not quietly weaken the tests ---------------
    # Gate 3a protects the verify *entrypoint*, which leaves the other way to
    # certify yourself wide open: delete the assertions and the suite passes
    # honestly. That is the failure this ladder exists to catch, so it is a
    # gate rather than a note — net-removed assertions or a deleted test file
    # reject, and --allow-test-reduction is the explicit promotion for the
    # legitimate case (consolidating or replacing coverage).
    test_reduction=""
    if [ "$ALLOW_TEST_REDUCTION" = 0 ] && command -v python3 >/dev/null 2>&1; then
      test_reduction="$(OMS_PATCH="$PATCH" python3 - <<'PY'
import os
import re

TEST_PATH = re.compile(
    r"(^|/)(tests?|spec)/|(^|/)test_[^/]+$|[^/]*(_test|_spec|\.test|\.spec)\.[^/.]+$"
)
# Language-agnostic on purpose: the assertion vocabularies that matter here are
# python/bash/js/go, and a line that stops asserting stops protecting.
ASSERT = re.compile(
    r"\bassert|\bexpect\s*\(|\bshould\b|\bfail\b|t\.(Error|Fatal)|"
    r"\bok\s*\(|\brequire\s*\.|\bXCTAssert"
)

path = None
counted = False
added = removed = 0
deleted_files = []
try:
    with open(os.environ["OMS_PATCH"], errors="replace") as handle:
        lines = handle.read().splitlines()
except OSError:
    raise SystemExit(0)

for i, line in enumerate(lines):
    if line.startswith("diff --git "):
        parts = line.split(" b/", 1)
        path = parts[1].strip() if len(parts) == 2 else None
        counted = bool(path and TEST_PATH.search(path))
        continue
    if line.startswith("deleted file mode") and counted and path:
        deleted_files.append(path)
        continue
    if not counted or line.startswith(("+++", "---", "@@")):
        continue
    if line.startswith("+") and ASSERT.search(line):
        added += 1
    elif line.startswith("-") and ASSERT.search(line):
        removed += 1

if deleted_files:
    print("deleted test file %s" % deleted_files[0])
elif removed > added:
    print("%d assertion line(s) removed, %d added" % (removed, added))
PY
)"
    fi
    if [ -n "$test_reduction" ]; then
      record "tests" "FAIL" "patch weakens the tests: $test_reduction (override: --allow-test-reduction)"
    else
      record "tests" "PASS" "patch does not net-remove test assertions"
    fi

    surface_detail="$(printf '%s\n' "$verification_surface" | awk '
      NF { printf "%s%s", separator, $0; separator=", " }
      END { if (separator != "") print "" }
    ')"
    if [ -n "$verifier_hit" ] && [ "$ALLOW_VERIFIER_CHANGE" = 0 ]; then
      record "verifier" "FAIL" "patch modifies its own verifier: $verifier_hit (override: --allow-verifier-change; base floor still required)"
    elif [ -n "$verifier_hit" ]; then
      record "verifier" "PASS" "verifier change permitted; base floor still required: $verifier_hit"
    elif [ -n "$verification_surface" ]; then
      record "verifier" "PASS" "verification surface change requires base floor: $surface_detail"
    elif [ -n "$VERIFY" ]; then
      record "verifier" "PASS" "patch leaves the verification surface untouched"
    fi

    # Do not execute candidate-controlled project code after a policy or
    # integrity gate has already rejected the patch. A modified verifier can
    # have host-side effects even when the final verdict says REJECT. Explicit
    # overrides turn their respective gates into PASS and intentionally allow
    # the candidate run; otherwise both verification projections stay inert.
    preverify_failed=0
    case "$ladder" in *"	FAIL	"*) preverify_failed=1 ;; esac
    if [ -n "$VERIFY" ]; then
      if [ "$preverify_failed" = 1 ]; then
        record "verify" "SKIP" "pre-verification policy or integrity gate failed"
      elif verify_out="$(cd "$worktree" && run_verify_with_timeout bash -c "$VERIFY" 2>&1)"; then
        record "verify" "PASS" "$VERIFY"
      else
        record "verify" "FAIL" "$VERIFY"
      fi
    else
      record "verify" "SKIP" "no --verify and no base scripts/check.sh"
    fi

    # If any automatic verification surface changed, apply the complete patch
    # to a second worktree at the same HEAD, restore only that surface from the
    # base, and run the identical command. Product changes remain in place, so
    # a helper that made a broken candidate pass fails under base-owned checks.
    if [ "$preverify_failed" = 1 ]; then
      record "verify-floor" "SKIP" "pre-verification policy or integrity gate failed"
    elif [ -n "$VERIFY" ] && [ -n "$verification_surface" ]; then
      if floor_worktree_parent="$(mktemp -d "${TMPDIR:-/tmp}/oh-my-setting-admit.XXXXXX")"; then
        floor_worktree="$floor_worktree_parent/wt"
        oms_harness_mark_tmpdir "$floor_worktree_parent" "$REPO" "$floor_worktree"
        if GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
          GIT_CONFIG_NOSYSTEM=1 git -c core.hooksPath=/dev/null \
          -c core.fsmonitor=false -C "$REPO" worktree add --quiet --detach \
          "$floor_worktree" "${base_full_sha:-HEAD}" >/dev/null 2>&1; then
          floor_worktree_created=1
          oms_seed_local_agent_files "$REPO" "$floor_worktree"
          if git -C "$floor_worktree" apply --binary "$PATCH" >/dev/null 2>&1; then
            floor_restore_ok=1
            floor_restore_detail=""
            while IFS= read -r f; do
              [ -n "$f" ] || continue
              if git -C "$floor_worktree" cat-file -e "HEAD:$f" >/dev/null 2>&1; then
                if ! GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
                  GIT_CONFIG_NOSYSTEM=1 git -c core.hooksPath=/dev/null \
                  -c core.fsmonitor=false -C "$floor_worktree" checkout \
                  --quiet --force HEAD -- "$f" >/dev/null 2>&1 ||
                  ! GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
                  GIT_CONFIG_NOSYSTEM=1 git -c core.fsmonitor=false \
                  -c diff.external= -C "$floor_worktree" diff --no-ext-diff \
                  --no-textconv --quiet HEAD -- "$f"; then
                  floor_restore_ok=0
                  floor_restore_detail="could not restore tracked surface path: $f"
                  break
                fi
              else
                if ! git -C "$floor_worktree" clean -fdx -- "$f" >/dev/null 2>&1 ||
                  [ -e "$floor_worktree/$f" ] || [ -L "$floor_worktree/$f" ]; then
                  floor_restore_ok=0
                  floor_restore_detail="could not remove added surface path: $f"
                  break
                fi
              fi
            done <<EOF
$verification_surface
EOF
            if [ "$floor_restore_ok" = 1 ]; then
              if floor_verify_out="$(cd "$floor_worktree" && run_verify_with_timeout bash -c "$VERIFY" 2>&1)"; then
                record "verify-floor" "PASS" "$VERIFY (restored from HEAD: $surface_detail)"
              else
                record "verify-floor" "FAIL" "$VERIFY (restored from HEAD: $surface_detail)"
              fi
            else
              record "verify-floor" "FAIL" "$floor_restore_detail"
            fi
          else
            record "verify-floor" "FAIL" "patch did not apply to the base-floor worktree"
          fi
        else
          record "verify-floor" "FAIL" "could not create the base-floor worktree"
        fi
      else
        record "verify-floor" "FAIL" "could not allocate the base-floor worktree"
      fi
    elif [ -n "$VERIFY" ]; then
      record "verify-floor" "SKIP" "verification surface unchanged"
    else
      record "verify-floor" "SKIP" "no verification command"
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
  if [ -n "$floor_verify_out" ]; then
    printf '\n## Base-floor verify output (tail)\n\n```\n%s\n```\n' "$(printf '%s' "$floor_verify_out" | tail -n 40)"
  fi
} > "$REPORT"

# Index the admission so the audit trail survives `artifact-index.sh prune
# --files` (which removes unreferenced files under .oms/artifacts/).
admit_exit=1
[ "$verdict" = "ADMIT" ] && admit_exit=0
ma_append_artifact_index "$REPO" patch-admit "" "$admit_exit" "$REPORT" "$PATCH" || true
if [ "$verdict" = "ADMIT" ]; then
  work_journal_observe "$REPO" patch-admit "$REPORT" \
    --source-id "admit:$patch_sha:$base_sha:$verdict" \
    --outcome "Patch admission passed" --outcome-status success \
    --verification-status passed
else
  work_journal_observe "$REPO" patch-admit "$REPORT" \
    --source-id "admit:$patch_sha:$base_sha:$verdict" \
    --outcome "Patch admission rejected" --outcome-status failure \
    --verification-status failed
fi

echo "patch-admit: $verdict ($REPORT)" >&2
printf '%s\n' "$verdict"
[ "$verdict" = "ADMIT" ]
