#!/usr/bin/env bash
set -euo pipefail

# Publish one verified clean HEAD as a create-only GitHub branch and Draft PR.
# The durable local intent is the recovery boundary: replay observes the exact
# remote branch and PR, but this tool can never update a branch, merge, mark a
# PR ready, create a tag, or release anything.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"

GIT="${OMS_GIT_BIN:-git}"
GH="${OMS_GH_BIN:-gh}"
# Replacement refs can make inspection read one commit while push transfers
# the original object named by the frozen OID. All publisher Git calls must
# therefore resolve the native object graph that the remote will receive.
export GIT_NO_REPLACE_OBJECTS=1
REMOTE_PARSER="$ROOT/scripts/lib/github-remote.py"
INTENT_HELPER="$ROOT/scripts/lib/draft-pr-intent.py"
REPO="$PWD"
ACTION=""
REMOTE="origin"
BASE=""
VERIFY=""
INTENT=""
EXPECTED_HEAD=""
EXPECTED_TREE=""
EXPECTED_BASE_SHA=""
EXPECTED_SPEC_SHA256=""
REVIEW_EVIDENCE=""
REMOTE_SLUG=""
REMOTE_FETCH_URL=""
REMOTE_PUSH_URL=""
GH_VIEWER_LOGIN=""
SCAN_TIMEOUT_S="${OMS_DRAFT_SCAN_TIMEOUT_S:-180}"
SCAN_MAX_OBJECTS="${OMS_DRAFT_SCAN_MAX_OBJECTS:-20000}"
SCAN_MAX_OBJECT_BYTES="${OMS_DRAFT_SCAN_MAX_OBJECT_BYTES:-33554432}"
SCAN_MAX_TOTAL_BYTES="${OMS_DRAFT_SCAN_MAX_TOTAL_BYTES:-268435456}"
SCAN_MAX_MANIFEST_BYTES="${OMS_DRAFT_SCAN_MAX_MANIFEST_BYTES:-16777216}"
SCAN_MAX_MEMORY_BYTES="${OMS_DRAFT_SCAN_MAX_MEMORY_BYTES:-536870912}"
SCAN_RUNNER_MODE=""
SCAN_RUNNER_BIN=""
SCAN_PYTHON_BIN=""
PUBLISH_DIR_GUARD_ACTIVE=0
PUBLISH_DIR_GUARD_ID=""

usage() {
  cat <<'EOF'
Usage: draft-pr.sh [options] prepare
       draft-pr.sh --repo PATH --intent FILE publish

Prepare and publish exactly one clean commit as a new GitHub branch and Draft
PR. `prepare` runs the verifier and writes an immutable local intent;
`publish` rechecks it, creates the branch only if absent, and creates or
recovers the exact Draft PR.

  --repo PATH       Git repository (default: current directory).
  --remote NAME     GitHub remote (default: origin).
  --base BRANCH     Existing remote base branch (required by prepare).
  --verify COMMAND  Bounded acceptance command (required by prepare).
  --intent FILE     Prepared file under REPO/.oms/publish (publish only).
  --expected-head SHA      Optional caller-frozen HEAD for prepare.
  --expected-tree SHA      Optional caller-frozen tree for prepare.
  --expected-base-sha SHA  Optional caller-frozen remote base for prepare.
  --expected-spec-sha256 H Optional PROJECT.md guard for prepare/publish.
  --review-evidence TEXT   Optional semantic-review disclosure for prepare,
                           rendered in the PR body (which the publisher
                           verifies remotely by digest). Absence renders as
                           "not requested".

Exit 2 means the request violates the contract. Exit 3 means local checks,
authentication, permissions, verification, or remote state prevented a safe
publication. No command in this tool merges, releases, tags, marks ready, or
updates an existing remote branch.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

park() {
  echo "draft-pr: parked reason=$1" >&2
  [ -z "${2:-}" ] || echo "draft-pr: next: $2" >&2
  exit 3
}

draft_shell_join() {  # ARGV...
  python3 - "$@" <<'PY' | tr -d '\r'
import shlex, sys
print(" ".join(shlex.quote(value) for value in sys.argv[1:]))
PY
}

validate_scan_limit() {  # VALUE NAME MAX
  case "$1" in *[!0-9]*|"") fail "$2 must be a positive integer" ;; esac
  [ "$1" -gt 0 ] && [ "$1" -le "$3" ] ||
    fail "$2 must be between 1 and $3"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --remote) [ "$#" -ge 2 ] || fail "--remote requires a name"; REMOTE="$2"; shift 2 ;;
    --base) [ "$#" -ge 2 ] || fail "--base requires a branch"; BASE="$2"; shift 2 ;;
    --verify) [ "$#" -ge 2 ] || fail "--verify requires a command"; VERIFY="$2"; shift 2 ;;
    --intent) [ "$#" -ge 2 ] || fail "--intent requires a file"; INTENT="$2"; shift 2 ;;
    --expected-head) [ "$#" -ge 2 ] || fail "--expected-head requires a SHA"; EXPECTED_HEAD="$2"; shift 2 ;;
    --expected-tree) [ "$#" -ge 2 ] || fail "--expected-tree requires a SHA"; EXPECTED_TREE="$2"; shift 2 ;;
    --expected-base-sha)
      [ "$#" -ge 2 ] || fail "--expected-base-sha requires a SHA"
      EXPECTED_BASE_SHA="$2"; shift 2 ;;
    --expected-spec-sha256)
      [ "$#" -ge 2 ] || fail "--expected-spec-sha256 requires a digest"
      EXPECTED_SPEC_SHA256="$2"; shift 2 ;;
    --review-evidence)
      [ "$#" -ge 2 ] || fail "--review-evidence requires a summary"
      REVIEW_EVIDENCE="$2"; shift 2 ;;
    prepare|publish)
      [ -z "$ACTION" ] || fail "multiple actions: $ACTION, $1"
      ACTION="$1"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$ACTION" ] || { usage >&2; exit 2; }
[ "${OMS_HARNESS_CHILD:-0}" != 1 ] ||
  fail "a harness child cannot acquire remote publication authority"
REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
case "$REMOTE" in *[!A-Za-z0-9._-]*|"") fail "--remote has unsafe characters" ;; esac
command -v "$GIT" >/dev/null 2>&1 || park "git-unavailable" "install Git"
[ -f "$REMOTE_PARSER" ] && [ -f "$INTENT_HELPER" ] || fail "publisher helpers are missing"

for frozen_sha in "$EXPECTED_HEAD" "$EXPECTED_TREE" "$EXPECTED_BASE_SHA"; do
  [ -z "$frozen_sha" ] && continue
  case "$frozen_sha" in *[!0-9a-f]*) fail "expected Git SHAs must be lowercase hexadecimal" ;; esac
  case "${#frozen_sha}" in 40|64) ;; *) fail "expected Git SHAs must be 40 or 64 characters" ;; esac
done
if [ -n "$EXPECTED_SPEC_SHA256" ]; then
  case "$EXPECTED_SPEC_SHA256" in *[!0-9a-f]*) fail "expected spec digest must be lowercase SHA-256" ;; esac
  [ "${#EXPECTED_SPEC_SHA256}" -eq 64 ] || fail "expected spec digest must be 64 characters"
fi
if [ -n "$REVIEW_EVIDENCE" ]; then
  [ "$ACTION" = prepare ] || fail "--review-evidence applies to prepare only"
  [ "${#REVIEW_EVIDENCE}" -le 200 ] ||
    fail "--review-evidence must be at most 200 characters"
  [ -z "${REVIEW_EVIDENCE//[A-Za-z0-9._= -]/}" ] ||
    fail "--review-evidence allows only letters, digits, and [._= -]"
fi
validate_scan_limit "$SCAN_TIMEOUT_S" OMS_DRAFT_SCAN_TIMEOUT_S 3600
validate_scan_limit "$SCAN_MAX_OBJECTS" OMS_DRAFT_SCAN_MAX_OBJECTS 200000
validate_scan_limit "$SCAN_MAX_OBJECT_BYTES" OMS_DRAFT_SCAN_MAX_OBJECT_BYTES 536870912
validate_scan_limit "$SCAN_MAX_TOTAL_BYTES" OMS_DRAFT_SCAN_MAX_TOTAL_BYTES 2147483647
validate_scan_limit "$SCAN_MAX_MANIFEST_BYTES" OMS_DRAFT_SCAN_MAX_MANIFEST_BYTES 134217728
validate_scan_limit "$SCAN_MAX_MEMORY_BYTES" OMS_DRAFT_SCAN_MAX_MEMORY_BYTES 2147483647

DRAFT_TMPDIR=""
draft_cleanup() {
  [ -z "${DRAFT_TMPDIR:-}" ] || rm -rf -- "$DRAFT_TMPDIR"
}
trap draft_cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
DRAFT_TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/oms-draft-pr-run.XXXXXX")" ||
  fail "cannot allocate private publisher scratch"
case "$DRAFT_TMPDIR" in ""|/|"${HOME:-/nonexistent}") fail "unsafe publisher scratch path" ;; esac
chmod 700 "$DRAFT_TMPDIR" 2>/dev/null || fail "cannot protect publisher scratch"
OMS_LIB_TMPDIR="$DRAFT_TMPDIR"
export -n OMS_LIB_TMPDIR 2>/dev/null || true

draft_run_remote() {
  draft_run_scan "${OMS_DRAFT_REMOTE_TIMEOUT:-30s}" "$@"
}

draft_run_github() {
  draft_run_scan "${OMS_DRAFT_GH_TIMEOUT:-30s}" "$@"
}

draft_run_push() {
  draft_run_scan "${OMS_DRAFT_PUSH_TIMEOUT:-15m}" "$@"
}

draft_init_scan_runner() {
  local candidate python_bin
  [ -z "$SCAN_RUNNER_MODE" ] || return 0
  python_bin="$(command -v python3 2>/dev/null || true)"
  if [ -z "$python_bin" ] || [ ! -f "$ROOT/scripts/lib/run-bounded.py" ] ||
     ! "$python_bin" - <<'PY'
import os, resource
raise SystemExit(
    0 if os.name == "posix" and hasattr(resource, "RLIMIT_AS") else 1
)
PY
  then
    park "strong-scan-limits-unavailable" \
      "install POSIX Python 3 with address-space limit support"
  fi
  SCAN_PYTHON_BIN="$python_bin"
  for candidate in timeout gtimeout; do
    command -v "$candidate" >/dev/null 2>&1 || continue
    if "$candidate" --kill-after=1s 5s true >/dev/null 2>&1; then
      SCAN_RUNNER_MODE=timeout
      SCAN_RUNNER_BIN="$(command -v "$candidate")"
      return 0
    fi
  done
  SCAN_RUNNER_MODE=python
  SCAN_RUNNER_BIN="$python_bin"
}

draft_run_scan() {  # WALL COMMAND...
  local wall="$1"
  shift
  [ -n "$SCAN_RUNNER_MODE" ] || draft_init_scan_runner
  case "$SCAN_RUNNER_MODE" in
    timeout)
      "$SCAN_RUNNER_BIN" --kill-after=1s "$wall" "$@"
      ;;
    python)
      "$SCAN_RUNNER_BIN" "$ROOT/scripts/lib/run-bounded.py" \
        "$wall" 1s draft-payload-scan "$@"
      ;;
    *) return 127 ;;
  esac
}

draft_run_traversal() {  # WALL COMMAND...
  local wall="$1"
  shift
  OMS_RUN_BOUNDED_MAX_MEMORY_BYTES="$SCAN_MAX_MEMORY_BYTES" \
    "$SCAN_PYTHON_BIN" "$ROOT/scripts/lib/run-bounded.py" \
    "$wall" 1s draft-git-traversal "$@"
}

assert_plain_index() {
  local flags_file flag_state
  flags_file="$(agent_memory_mktemp)" || fail "cannot allocate index flag scan"
  if ! "$GIT" -C "$REPO" ls-files -v -z > "$flags_file"; then
    rm -f "$flags_file"
    park "git-index-unreadable" "repair the repository index"
  fi
  flag_state="$(python3 - "$flags_file" <<'PY'
import sys

with open(sys.argv[1], "rb") as handle:
    records = handle.read().split(b"\0")
for record in records:
    tag = record[:1]
    if tag == b"S" or (b"a" <= tag <= b"z"):
        print("hidden")
        break
else:
    print("plain")
PY
)" || {
    rm -f "$flags_file"
    park "git-index-unreadable" "repair the repository index"
  }
  rm -f "$flags_file"
  flag_state="${flag_state//$'\r'/}"
  [ "$flag_state" = plain ] ||
    park "git-index-hidden-paths" "clear assume-unchanged, skip-worktree, and sparse index flags"
}

assert_clean() {
  local status_out status_rc=0
  assert_plain_index
  status_out="$("$GIT" -C "$REPO" -c core.fsmonitor=false \
    status --porcelain --untracked-files=all)" || status_rc=$?
  [ "$status_rc" -eq 0 ] || park "git-status-failed" "repair the repository before publication"
  [ -z "$status_out" ] ||
    fail "tree is dirty; publication requires one committed snapshot"
}

current_branch() {
  local branch
  branch="$("$GIT" -C "$REPO" symbolic-ref --quiet --short HEAD 2>/dev/null)" ||
    fail "detached HEAD cannot be published"
  branch="${branch//$'\r'/}"
  "$GIT" check-ref-format --branch "$branch" >/dev/null 2>&1 ||
    fail "current branch name is invalid"
  printf '%s\n' "$branch"
}

remote_ref_sha() {  # URL REF -> absent | SHA
  local remote_url="$1"
  local ref="$2"
  local raw count sha found_ref
  raw="$(draft_run_remote "$GIT" -C "$REPO" ls-remote --heads "$remote_url" \
    "refs/heads/$ref" 2>/dev/null)" ||
    park "remote-unreachable" "check the remote and network"
  raw="${raw//$'\r'/}"
  count="$(printf '%s\n' "$raw" | awk 'NF {n++} END {print n+0}')"
  [ "$count" -le 1 ] || park "ambiguous-remote-ref" "inspect refs/heads/$ref"
  if [ "$count" -eq 0 ]; then
    printf 'absent\n'
    return 0
  fi
  sha="$(printf '%s\n' "$raw" | awk 'NF {print $1; exit}')"
  found_ref="$(printf '%s\n' "$raw" | awk 'NF {print $2; exit}')"
  case "$sha" in *[!0-9a-f]*|"") park "malformed-remote-ref" "inspect refs/heads/$ref" ;; esac
  [ "${#sha}" -eq 40 ] && [ "$found_ref" = "refs/heads/$ref" ] ||
    park "malformed-remote-ref" "inspect refs/heads/$ref"
  printf '%s\n' "$sha"
}

strict_remote() {
  local fetch_urls push_urls fetch_count push_count fetch_slug push_slug fetch_fold push_fold
  fetch_urls="$("$GIT" -C "$REPO" remote get-url --all "$REMOTE" 2>/dev/null)" ||
    fail "remote $REMOTE does not exist"
  push_urls="$("$GIT" -C "$REPO" remote get-url --push --all "$REMOTE" 2>/dev/null)" ||
    fail "remote $REMOTE has no push URL"
  fetch_urls="${fetch_urls//$'\r'/}"
  push_urls="${push_urls//$'\r'/}"
  fetch_count="$(printf '%s\n' "$fetch_urls" | awk 'NF {n++} END {print n+0}')"
  push_count="$(printf '%s\n' "$push_urls" | awk 'NF {n++} END {print n+0}')"
  [ "$fetch_count" -eq 1 ] || fail "remote $REMOTE must have exactly one fetch URL"
  [ "$push_count" -eq 1 ] || fail "remote $REMOTE must have exactly one push URL"
  REMOTE_FETCH_URL="$(printf '%s\n' "$fetch_urls" | awk 'NF {print; exit}')"
  REMOTE_PUSH_URL="$(printf '%s\n' "$push_urls" | awk 'NF {print; exit}')"
  fetch_slug="$(python3 "$REMOTE_PARSER" "$REMOTE_FETCH_URL")" ||
    fail "remote $REMOTE is not an accepted GitHub remote"
  push_slug="$(python3 "$REMOTE_PARSER" "$REMOTE_PUSH_URL")" ||
    fail "push URL for $REMOTE is not an accepted GitHub remote"
  fetch_slug="${fetch_slug//$'\r'/}"
  push_slug="${push_slug//$'\r'/}"
  fetch_fold="$(printf '%s' "$fetch_slug" | tr '[:upper:]' '[:lower:]')"
  push_fold="$(printf '%s' "$push_slug" | tr '[:upper:]' '[:lower:]')"
  [ "$fetch_fold" = "$push_fold" ] ||
    fail "fetch and push URLs target different GitHub repositories"
  REMOTE_SLUG="$fetch_slug"
}

require_github_write() {  # SLUG
  local slug="$1"
  local raw parsed owner permission expected actual viewer
  command -v "$GH" >/dev/null 2>&1 ||
    park "gh-unavailable" "install GitHub CLI and authenticate it"
  draft_run_github "$GH" auth status --hostname github.com >/dev/null 2>&1 ||
    park "gh-auth-failed" "run gh auth login"
  raw="$(GH_HOST=github.com draft_run_github "$GH" repo view "github.com/$slug" \
    --json nameWithOwner,viewerPermission 2>/dev/null)" ||
    park "github-permission-unreadable" "check repository access"
  parsed="$(OMS_DRAFT_REPO_VIEW="$raw" python3 - <<'PY'
import json, os, sys
try:
    row = json.loads(os.environ.get("OMS_DRAFT_REPO_VIEW", ""))
except ValueError:
    raise SystemExit(1)
owner = row.get("nameWithOwner")
permission = row.get("viewerPermission")
if not isinstance(owner, str) or not isinstance(permission, str):
    raise SystemExit(1)
print(owner)
print(permission)
PY
)" || park "github-permission-unreadable" "check repository access"
  parsed="${parsed//$'\r'/}"
  owner="$(printf '%s\n' "$parsed" | sed -n 1p)"
  permission="$(printf '%s\n' "$parsed" | sed -n 2p)"
  expected="$(printf '%s' "$slug" | tr '[:upper:]' '[:lower:]')"
  actual="$(printf '%s' "$owner" | tr '[:upper:]' '[:lower:]')"
  [ "$actual" = "$expected" ] ||
    park "github-repository-mismatch" "check gh authentication and the remote"
  case "$permission" in
    WRITE|MAINTAIN|ADMIN) ;;
    *) park "github-write-denied" "request write permission" ;;
  esac
  viewer="$(GH_HOST=github.com draft_run_github "$GH" api --hostname github.com user \
    --jq .login 2>/dev/null)" || park "github-viewer-unreadable" "check gh authentication"
  viewer="${viewer//$'\r'/}"
  case "$viewer" in *[!A-Za-z0-9-]*|"") park "github-viewer-invalid" "check gh authentication" ;; esac
  GH_VIEWER_LOGIN="$viewer"
}

run_verifier() {  # COMMAND -> verifier exit status
  local command_text="$1"
  local rc=0
  (
    cd "$REPO"
    [ "$PUBLISH_DIR_GUARD_ACTIVE" -eq 0 ] || exec 9<&-
    draft_run_scan "${OMS_DRAFT_VERIFY_TIMEOUT:-${OMS_PEER_VERIFY_TIMEOUT:-10m}}" \
      bash -c "$command_text"
  ) || rc=$?
  return "$rc"
}

draft_file_has_sensitive_content() {  # FILE -> matcher status
  agent_memory_file_has_sensitive_content "$1"
}

assert_native_git_graph() {
  local graft_path graft_state
  graft_path="$("$GIT" -C "$REPO" rev-parse --git-path info/grafts 2>/dev/null)" ||
    park "git-graph-unreadable" "repair the repository metadata"
  graft_path="${graft_path//$'\r'/}"
  [ -n "$graft_path" ] || park "git-graph-unreadable" "repair the repository metadata"
  graft_state="$(cd "$REPO" && python3 - "$graft_path" <<'PY'
import os, stat, sys

try:
    info = os.lstat(sys.argv[1])
except FileNotFoundError:
    print("absent")
except OSError:
    raise SystemExit(2)
else:
    if stat.S_ISREG(info.st_mode):
        print("present" if info.st_size else "empty")
    else:
        print("unsafe")
PY
)" || park "git-graph-unreadable" "inspect .git/info/grafts"
  graft_state="${graft_state//$'\r'/}"
  case "$graft_state" in
    absent|empty) ;;
    present) park "git-grafts-present" "remove deprecated grafts before publication" ;;
    unsafe) park "git-grafts-unsafe" "replace .git/info/grafts with ordinary repository metadata" ;;
    *) park "git-graph-unreadable" "inspect .git/info/grafts" ;;
  esac
}

scan_remaining() {  # DEADLINE_EPOCH -> remaining positive seconds
  local deadline="$1"
  local current
  current="$(date +%s)" || return 1
  current="${current//$'\r'/}"
  case "$current" in *[!0-9]*|"") return 1 ;; esac
  [ "$current" -lt "$deadline" ] || return 1
  printf '%s\n' "$((deadline - current))"
}

draft_bounded_file_has_sensitive_content() {  # FILE DEADLINE
  local file="$1"
  local deadline="$2"
  local remaining scan_rc=0 sensitive_re
  [ -s "$file" ] || return 1
  remaining="$(scan_remaining "$deadline")" ||
    return 124
  sensitive_re="$(agent_memory_sensitive_re)" ||
    return 125
  draft_run_scan "${remaining}s" grep -Eiq "$sensitive_re" "$file" || scan_rc=$?
  return "$scan_rc"
}

require_publish_file_clean() {  # FILE DEADLINE TEMP...
  local file="$1"
  local deadline="$2"
  local scan_rc=0
  shift 2
  draft_bounded_file_has_sensitive_content "$file" "$deadline" || scan_rc=$?
  case "$scan_rc" in
    0)
      rm -f "$@"
      park "sensitive-publish-payload" \
        "remove credentials and private machine details from the entire branch history"
      ;;
    1) return 0 ;;
    124|137|143)
      rm -f "$@"
      park "publish-payload-timeout" \
        "reduce the branch history or raise the bounded scan timeout"
      ;;
    *)
      rm -f "$@"
      park "publish-payload-scan-failed" \
        "restore the local sensitive-content scanner"
      ;;
  esac
}

scan_publish_range() {  # BASE_SHA HEAD_SHA
  local base_sha="$1"
  local head_sha="$2"
  local object_names object_ids object_payload oid object_type object_size actual_size
  local started deadline remaining object_count=0 scanned_bytes=0
  draft_init_scan_runner
  started="$(date +%s)" || park "publish-payload-timeout" "restore the local clock"
  started="${started//$'\r'/}"
  case "$started" in *[!0-9]*|"") park "publish-payload-timeout" "restore the local clock" ;; esac
  deadline="$((started + SCAN_TIMEOUT_S))"
  object_names="$(agent_memory_mktemp)" || fail "cannot allocate publication payload scan"
  object_ids="$(agent_memory_mktemp)" || {
    rm -f "$object_names"
    fail "cannot allocate publication object scan"
  }
  object_payload="$(agent_memory_mktemp)" || {
    rm -f "$object_names" "$object_ids"
    fail "cannot allocate publication content scan"
  }
  remaining="$(scan_remaining "$deadline")" || {
    rm -f "$object_names" "$object_ids" "$object_payload"
    park "publish-payload-timeout" "reduce the branch history or raise the bounded scan timeout"
  }
  if ! draft_run_traversal "${remaining}s" "$GIT" -C "$REPO" rev-list \
       --objects --no-object-names "$base_sha..$head_sha" |
     LC_ALL=C awk -v max_lines="$SCAN_MAX_OBJECTS" \
       -v max_bytes="$SCAN_MAX_MANIFEST_BYTES" '
         { bytes += length($0) + 1 }
         NR > max_lines || bytes > max_bytes { exit 42 }
         { print }
       ' > "$object_ids"; then
    rm -f "$object_names" "$object_ids" "$object_payload"
    park "publish-payload-enumeration-failed" "the object count/manifest exceeded its cap or Git traversal failed"
  fi
  remaining="$(scan_remaining "$deadline")" || {
    rm -f "$object_names" "$object_ids" "$object_payload"
    park "publish-payload-timeout" "reduce the branch history or raise the bounded scan timeout"
  }
  if ! draft_run_traversal "${remaining}s" "$GIT" -C "$REPO" rev-list \
       --objects "$base_sha..$head_sha" |
     LC_ALL=C awk -v max_lines="$SCAN_MAX_OBJECTS" \
       -v max_bytes="$SCAN_MAX_MANIFEST_BYTES" '
         { bytes += length($0) + 1 }
         NR > max_lines || bytes > max_bytes { exit 42 }
         { print }
       ' > "$object_names"; then
    rm -f "$object_names" "$object_ids" "$object_payload"
    park "publish-payload-enumeration-failed" "the object count/manifest exceeded its cap or Git traversal failed"
  fi
  require_publish_file_clean "$object_names" "$deadline" \
    "$object_names" "$object_ids" "$object_payload"
  remaining="$(scan_remaining "$deadline")" || {
    rm -f "$object_names" "$object_ids" "$object_payload"
    park "publish-payload-timeout" "reduce the branch history or raise the bounded scan timeout"
  }
  if ! draft_run_traversal "${remaining}s" "$GIT" -C "$REPO" \
       -c core.quotePath=true log --format= --name-only --no-renames \
       --no-ext-diff -m "$base_sha..$head_sha" |
     LC_ALL=C awk -v max_bytes="$SCAN_MAX_MANIFEST_BYTES" '
         { bytes += length($0) + 1 }
         bytes > max_bytes { exit 42 }
         { print }
       ' > "$object_names"; then
    rm -f "$object_names" "$object_ids" "$object_payload"
    park "publish-payload-enumeration-failed" "the path manifest exceeded its cap or Git traversal failed"
  fi
  require_publish_file_clean "$object_names" "$deadline" \
    "$object_names" "$object_ids" "$object_payload"
  while IFS= read -r oid || [ -n "$oid" ]; do
    object_count="$((object_count + 1))"
    [ "$object_count" -le "$SCAN_MAX_OBJECTS" ] || {
      rm -f "$object_names" "$object_ids" "$object_payload"
      park "publish-payload-object-limit" "reduce the branch history or raise the bounded object cap"
    }
    oid="${oid//$'\r'/}"
    case "$oid" in *[!0-9a-f]*|"")
      rm -f "$object_names" "$object_ids" "$object_payload"
      park "publish-payload-unreadable" "repair the exact local commit range"
      ;;
    esac
    case "${#oid}" in 40|64) ;; *)
      rm -f "$object_names" "$object_ids" "$object_payload"
      park "publish-payload-unreadable" "repair the exact local commit range"
      ;;
    esac
    remaining="$(scan_remaining "$deadline")" || {
      rm -f "$object_names" "$object_ids" "$object_payload"
      park "publish-payload-timeout" "reduce the branch history or raise the bounded scan timeout"
    }
    object_type="$(draft_run_scan "${remaining}s" "$GIT" -C "$REPO" \
      cat-file -t "$oid" 2>/dev/null)" || {
      rm -f "$object_names" "$object_ids" "$object_payload"
      park "publish-payload-unreadable" "restore every object in the exact commit range"
    }
    object_type="${object_type//$'\r'/}"
    case "$object_type" in
      blob|commit|tag|tree)
        remaining="$(scan_remaining "$deadline")" || {
          rm -f "$object_names" "$object_ids" "$object_payload"
          park "publish-payload-timeout" "reduce the branch history or raise the bounded scan timeout"
        }
        object_size="$(draft_run_scan "${remaining}s" "$GIT" -C "$REPO" \
          cat-file -s "$oid" 2>/dev/null)" || {
          rm -f "$object_names" "$object_ids" "$object_payload"
          park "publish-payload-unreadable" "restore every object in the exact commit range"
        }
        object_size="${object_size//$'\r'/}"
        case "$object_size" in *[!0-9]*|"")
          rm -f "$object_names" "$object_ids" "$object_payload"
          park "publish-payload-unreadable" "inspect the malformed Git object size"
          ;;
        esac
        [ "$object_size" -le "$SCAN_MAX_OBJECT_BYTES" ] || {
          rm -f "$object_names" "$object_ids" "$object_payload"
          park "publish-payload-object-too-large" "split or remove the oversized object before publication"
        }
        [ "$object_size" -le "$((SCAN_MAX_TOTAL_BYTES - scanned_bytes))" ] || {
          rm -f "$object_names" "$object_ids" "$object_payload"
          park "publish-payload-total-too-large" "reduce the total new object bytes before publication"
        }
        scanned_bytes="$((scanned_bytes + object_size))"
        remaining="$(scan_remaining "$deadline")" || {
          rm -f "$object_names" "$object_ids" "$object_payload"
          park "publish-payload-timeout" "reduce the branch history or raise the bounded scan timeout"
        }
        if ! draft_run_scan "${remaining}s" "$GIT" -C "$REPO" \
          cat-file "$object_type" "$oid" > "$object_payload"; then
          rm -f "$object_names" "$object_ids" "$object_payload"
          park "publish-payload-unreadable" "restore every object in the exact commit range"
        fi
        actual_size="$(wc -c < "$object_payload" | tr -d ' \r')" || {
          rm -f "$object_names" "$object_ids" "$object_payload"
          park "publish-payload-unreadable" "inspect the materialized Git object"
        }
        [ "$actual_size" = "$object_size" ] || {
          rm -f "$object_names" "$object_ids" "$object_payload"
          park "publish-payload-unreadable" "Git object bytes changed during scanning"
        }
        require_publish_file_clean "$object_payload" "$deadline" \
          "$object_names" "$object_ids" "$object_payload"
        ;;
      *)
        rm -f "$object_names" "$object_ids" "$object_payload"
        park "publish-payload-unreadable" "inspect the unexpected Git object type"
        ;;
    esac
  done < "$object_ids"
  rm -f "$object_names" "$object_ids" "$object_payload"
}

poison_intent() {
  if [ "$PUBLISH_DIR_GUARD_ACTIVE" -eq 1 ]; then
    python3 "$INTENT_HELPER" block-at --repo "$REPO" \
      --intent-name "${INTENT##*/}" --dir-fd 9 \
      --expected-dir-id "$PUBLISH_DIR_GUARD_ID" >/dev/null ||
      fail "cannot durably block the guarded publication intent"
  else
    python3 "$INTENT_HELPER" block --path "$INTENT" --repo "$REPO" >/dev/null ||
      fail "cannot durably block a verifier-modified publication intent"
  fi
}

open_publish_dir_guard() {
  exec 9<"$REPO/.oms/publish" || fail "cannot hold the publication directory"
  PUBLISH_DIR_GUARD_ID="$(python3 "$INTENT_HELPER" dir-id \
    --path "$INTENT" --repo "$REPO" --dir-fd 9)" || {
      exec 9<&-
      fail "cannot bind the publication directory guard"
    }
  PUBLISH_DIR_GUARD_ID="${PUBLISH_DIR_GUARD_ID//$'\r'/}"
  case "$PUBLISH_DIR_GUARD_ID" in
    *[!0-9a-f:]*|""|:*|*:|*:*:*)
      exec 9<&-
      fail "publication directory guard returned an invalid identity"
      ;;
    *:*) ;;
    *)
      exec 9<&-
      fail "publication directory guard returned an invalid identity"
      ;;
  esac
  PUBLISH_DIR_GUARD_ACTIVE=1
}

close_publish_dir_guard() {
  [ "$PUBLISH_DIR_GUARD_ACTIVE" -eq 0 ] || exec 9<&-
  PUBLISH_DIR_GUARD_ACTIVE=0
  PUBLISH_DIR_GUARD_ID=""
}

assert_snapshot() {  # BRANCH HEAD TREE BASE_SHA
  local branch="$1"
  local head="$2"
  local head_tree="$3"
  local base_sha="$4"
  local current
  assert_clean
  [ "$(current_branch)" = "$branch" ] || park "branch-changed" "return to $branch"
  current="$("$GIT" -C "$REPO" rev-parse HEAD)"
  current="${current//$'\r'/}"
  [ "$current" = "$head" ] || park "head-changed" "prepare a new publication intent"
  current="$("$GIT" -C "$REPO" rev-parse 'HEAD^{tree}')"
  current="${current//$'\r'/}"
  [ "$current" = "$head_tree" ] || park "tree-changed" "prepare a new publication intent"
  current="$(remote_ref_sha "$REMOTE_FETCH_URL" "$BASE")"
  [ "$current" = "$base_sha" ] || park "base-moved" "review against the new remote base"
  if [ -n "$EXPECTED_SPEC_SHA256" ]; then
    [ -f "$REPO/PROJECT.md" ] && [ ! -L "$REPO/PROJECT.md" ] ||
      park "project-spec-missing" "restore the reviewed PROJECT.md"
    current="$(oms_sha256_file "$REPO/PROJECT.md")" ||
      park "project-spec-unreadable" "restore the reviewed PROJECT.md"
    [ "$current" = "$EXPECTED_SPEC_SHA256" ] ||
      park "project-spec-changed" "prepare from the reviewed project contract"
  fi
}

assert_private_publish_root() {
  [ ! -L "$REPO/.oms" ] || fail ".oms must not be a symlink"
  [ ! -e "$REPO/.oms" ] || [ -d "$REPO/.oms" ] || fail ".oms must be a directory"
  [ ! -L "$REPO/.oms/publish" ] || fail ".oms/publish must not be a symlink"
  [ ! -e "$REPO/.oms/publish" ] || [ -d "$REPO/.oms/publish" ] ||
    fail ".oms/publish must be a directory"
}

prepare() {
  local branch slug head tree base_sha ahead source_sha title publish_dir safe_branch branch_digest
  local body_file title_file intent_path fetch_url push_url viewer_login verifier_scan verifier_rc
  local verifier_scan_rc title_scan_rc body_scan_rc topology_rc ahead_rc old_umask
  local resume_safe resume_cmd

  [ -n "$BASE" ] || fail "prepare requires --base"
  [ -n "$VERIFY" ] || fail "prepare requires --verify"
  "$GIT" check-ref-format --branch "$BASE" >/dev/null 2>&1 || fail "--base is not a valid branch"
  assert_native_git_graph
  assert_clean
  branch="$(current_branch)"
  [ "$branch" != "$BASE" ] || fail "source branch must differ from the base branch"
  strict_remote
  slug="$REMOTE_SLUG"
  require_github_write "$slug"
  fetch_url="$REMOTE_FETCH_URL"
  push_url="$REMOTE_PUSH_URL"
  viewer_login="$GH_VIEWER_LOGIN"
  base_sha="$(remote_ref_sha "$REMOTE_FETCH_URL" "$BASE")"
  [ "$base_sha" != absent ] || park "base-missing" "push or select an existing remote base"
  [ -z "$EXPECTED_BASE_SHA" ] || [ "$base_sha" = "$EXPECTED_BASE_SHA" ] ||
    park "unexpected-base" "repeat whole-change review against the current remote base"
  head="$("$GIT" -C "$REPO" rev-parse HEAD)"
  tree="$("$GIT" -C "$REPO" rev-parse 'HEAD^{tree}')"
  head="${head//$'\r'/}"
  tree="${tree//$'\r'/}"
  [ -z "$EXPECTED_HEAD" ] || [ "$head" = "$EXPECTED_HEAD" ] ||
    park "unexpected-head" "repeat acceptance and whole-change review"
  [ -z "$EXPECTED_TREE" ] || [ "$tree" = "$EXPECTED_TREE" ] ||
    park "unexpected-tree" "repeat acceptance and whole-change review"
  publish_dir="$REPO/.oms/publish"
  safe_branch="$(printf '%s' "$branch" | tr -c 'A-Za-z0-9._-' '-' | cut -c1-80)"
  branch_digest="$(printf '%s' "$branch" | oms_sha256_stream)" ||
    fail "cannot hash the source branch"
  intent_path="$publish_dir/${safe_branch}-$(printf '%.12s' "$branch_digest")-$(printf '%.12s' "$head").json"
  if [ -e "$intent_path" ] || [ -L "$intent_path" ] ||
     [ -e "$intent_path.blocked" ] || [ -L "$intent_path.blocked" ] ||
     [ -e "$intent_path.push-attempted" ] || [ -L "$intent_path.push-attempted" ]; then
    resume_safe=0
    if [ -f "$intent_path" ] && [ ! -L "$intent_path" ] &&
       [ ! -e "$intent_path.blocked" ] && [ ! -L "$intent_path.blocked" ]; then
      if { [ ! -e "$intent_path.push-attempted" ] &&
           [ ! -L "$intent_path.push-attempted" ]; } ||
         { [ -f "$intent_path.push-attempted" ] &&
           [ ! -L "$intent_path.push-attempted" ]; }; then
        resume_safe=1
      fi
    fi
    if [ "$resume_safe" -eq 1 ]; then
      resume_cmd="$(draft_shell_join oms draft-pr --repo "$REPO" \
        --intent "$intent_path" publish)" ||
        fail "cannot render the exact publish continuation"
      resume_cmd="${resume_cmd//$'\r'/}"
      park "intent-already-prepared" "$resume_cmd"
    fi
    park "intent-already-prepared" "inspect the terminal intent slot or use a new branch name"
  fi
  if ! "$GIT" -C "$REPO" cat-file -e "$base_sha^{commit}" 2>/dev/null; then
    draft_run_remote "$GIT" -C "$REPO" fetch --no-tags "$REMOTE_FETCH_URL" \
      "$base_sha" >/dev/null 2>&1 ||
      park "base-object-missing" "fetch the remote base"
  fi
  draft_init_scan_runner
  topology_rc=0
  draft_run_traversal "${SCAN_TIMEOUT_S}s" "$GIT" -C "$REPO" \
    merge-base --is-ancestor "$base_sha" "$head" 2>/dev/null || topology_rc=$?
  case "$topology_rc" in
    0) ;;
    1) park "base-not-ancestor" "rebase or merge the current remote base" ;;
    124|137|143)
      park "publish-payload-timeout" "reduce the branch history or raise the bounded scan timeout"
      ;;
    *) park "commit-range-invalid" "inspect the bounded branch topology" ;;
  esac
  ahead_rc=0
  ahead="$(draft_run_traversal "${SCAN_TIMEOUT_S}s" "$GIT" -C "$REPO" \
    rev-list --count "$base_sha..$head")" || ahead_rc=$?
  case "$ahead_rc" in
    0) ;;
    124|137|143)
      park "publish-payload-timeout" "reduce the branch history or raise the bounded scan timeout"
      ;;
    *) park "commit-range-invalid" "inspect the bounded branch history" ;;
  esac
  ahead="${ahead//$'\r'/}"
  case "$ahead" in *[!0-9]*|"") park "commit-range-invalid" "inspect the branch history" ;; esac
  [ "$ahead" -gt 0 ] || fail "source branch has no commit beyond the remote base"
  source_sha="$(remote_ref_sha "$REMOTE_PUSH_URL" "$branch")"
  if [ "$source_sha" != absent ]; then
    [ "$source_sha" = "$head" ] ||
      park "source-branch-exists" "choose a new branch; existing branches are never updated"
    [ -f "$intent_path" ] && [ ! -L "$intent_path" ] ||
      park "unowned-source-branch" "choose a new branch; no prior publication intent owns this ref"
  fi

  assert_snapshot "$branch" "$head" "$tree" "$base_sha"
  verifier_scan="$(agent_memory_mktemp)" || fail "cannot allocate verifier scan"
  printf '%s\n' "$VERIFY" > "$verifier_scan"
  verifier_scan_rc=0
  draft_file_has_sensitive_content "$verifier_scan" || verifier_scan_rc=$?
  rm -f "$verifier_scan"
  case "$verifier_scan_rc" in
    0) fail "verifier contains credential-shaped text or a machine-specific path" ;;
    1) ;;
    *) park "sensitive-scan-failed" "restore the local sensitive-content scanner" ;;
  esac

  verifier_rc=0
  run_verifier "$VERIFY" || verifier_rc=$?
  if [ -e "$intent_path" ] || [ -L "$intent_path" ] ||
     [ -e "$intent_path.blocked" ] || [ -L "$intent_path.blocked" ] ||
     [ -e "$intent_path.push-attempted" ] || [ -L "$intent_path.push-attempted" ]; then
    python3 "$INTENT_HELPER" block --path "$intent_path" --repo "$REPO" >/dev/null ||
      fail "cannot durably block an intent path created during verification"
    park "intent-created-by-verifier" "this intent path is terminal; use a new branch name"
  fi
  [ "$verifier_rc" -eq 0 ] ||
    park "verification-failed" "repair the exact acceptance command (exit $verifier_rc)"
  assert_native_git_graph
  assert_snapshot "$branch" "$head" "$tree" "$base_sha"
  strict_remote
  [ "$REMOTE_SLUG" = "$slug" ] && [ "$REMOTE_FETCH_URL" = "$fetch_url" ] &&
    [ "$REMOTE_PUSH_URL" = "$push_url" ] ||
    park "remote-changed-during-verification" "restore the reviewed remote configuration"
  require_github_write "$slug"
  [ "$GH_VIEWER_LOGIN" = "$viewer_login" ] ||
    park "github-viewer-changed" "restore the GitHub identity used for preparation"
  scan_publish_range "$base_sha" "$head"
  source_sha="$(remote_ref_sha "$REMOTE_PUSH_URL" "$branch")"
  if [ "$source_sha" != absent ]; then
    [ "$source_sha" = "$head" ] ||
      park "source-branch-raced" "choose a new branch; existing branches are never updated"
    [ -f "$intent_path" ] && [ ! -L "$intent_path" ] ||
      park "source-branch-raced" "an unowned branch appeared during verification"
  fi

  title="$("$GIT" -C "$REPO" log -1 --format=%s "$head")"
  title="${title//$'\r'/}"
  [ -n "$title" ] || fail "HEAD has no commit subject for the PR title"
  body_file="$(agent_memory_mktemp)" || fail "cannot allocate PR body"
  title_file="$(agent_memory_mktemp)" || { rm -f "$body_file"; fail "cannot allocate title scan"; }
  printf '%s\n' "$title" > "$title_file"
  {
    printf '## Summary\n\n'
    "$GIT" -C "$REPO" log --reverse --format='- %s' "$base_sha..$head"
    printf '\n## Verification\n\n'
    printf -- '- Local acceptance passed for `%s`.\n' "$(printf '%.12s' "$head")"
    printf -- '- Semantic review: %s.\n' "${REVIEW_EVIDENCE:-not requested}"
    printf -- '- This remains a Draft PR; merge and release require separate authority.\n'
  } > "$body_file"
  title_scan_rc=0
  body_scan_rc=0
  draft_file_has_sensitive_content "$title_file" || title_scan_rc=$?
  draft_file_has_sensitive_content "$body_file" || body_scan_rc=$?
  rm -f "$title_file"
  case "$title_scan_rc:$body_scan_rc" in
    0:*|*:0)
      rm -f "$body_file"
      fail "PR title or body resembles sensitive content"
      ;;
    1:1) ;;
    *)
      rm -f "$body_file"
      park "sensitive-scan-failed" "restore the local sensitive-content scanner"
      ;;
  esac

  assert_private_publish_root
  old_umask="$(umask)"
  umask 077
  if ! mkdir -p "$publish_dir"; then
    umask "$old_umask"
    fail "cannot create the private publication directory"
  fi
  umask "$old_umask"
  chmod 700 "$publish_dir" 2>/dev/null ||
    fail "cannot protect the private publication directory"
  agent_memory_ensure_oms_ignore_for_path "$publish_dir" 2>/dev/null ||
    fail "cannot keep publication intents out of Git"
  "$GIT" -C "$REPO" check-ignore -q -- "$intent_path" 2>/dev/null ||
    fail "publication intent path is not Git-ignored"
  intent_path="$(python3 "$INTENT_HELPER" create --path "$intent_path" --repo "$REPO" \
    --head "$head" --tree "$tree" --branch "$branch" --base "$BASE" \
    --base-sha "$base_sha" --remote "$REMOTE" --repo-slug "$slug" \
    --fetch-url "$REMOTE_FETCH_URL" --push-url "$REMOTE_PUSH_URL" \
    --viewer-login "$GH_VIEWER_LOGIN" --spec-sha256 "$EXPECTED_SPEC_SHA256" \
    --verify "$VERIFY" --title "$title" --body-file "$body_file")" || {
      rm -f "$body_file"
      fail "could not write the exact publication intent"
    }
  rm -f "$body_file"
  intent_path="${intent_path//$'\r'/}"
  "$GIT" -C "$REPO" check-ignore -q -- "$intent_path" 2>/dev/null ||
    fail "prepared publication intent lost Git-ignore protection"
  echo "intent: $intent_path"
}

intent_field() {  # NAME
  python3 "$INTENT_HELPER" field --path "$INTENT" --repo "$REPO" --name "$1"
}

query_prs() {  # SLUG BRANCH OUTPUT
  # gh rejects "<owner>:<branch>" for pr list --head, and pr create's
  # owner-qualified form breaks on organization owners; publication is
  # same-repository only, so the unqualified branch name is the exact form.
  # A same-named fork PR widening this list is fail-closed downstream:
  # inspect-pr refuses multiple rows and non-exact/cross-repository rows.
  GH_HOST=github.com draft_run_github "$GH" pr list --repo "github.com/$1" \
    --head "$2" --state all --limit 10 \
    --json number,url,state,isDraft,title,body,headRefName,headRefOid,baseRefName,baseRefOid,isCrossRepository \
    > "$3" 2>/dev/null ||
    park "github-pr-query-failed" "check GitHub connectivity and permission"
}

publish_locked() {
  local phase head tree branch base base_sha remote slug verify title current_sha
  local fetch_url push_url viewer_login spec_sha title_sha body_sha saved_url saved_number
  local pr_json pr_state pr_url pr_number body_file verifier_scan push_out push_rc create_out create_rc
  local immutable_sha current_immutable blocked armed verifier_rc verifier_scan_rc
  local state_sha current_state

  assert_native_git_graph
  blocked="$(python3 "$INTENT_HELPER" blocked --path "$INTENT" --repo "$REPO")" ||
    fail "cannot validate the publication revocation marker"
  blocked="${blocked//$'\r'/}"
  case "$blocked" in 0) ;; 1)
    park "intent-terminally-blocked" "prepare a new intent with a new branch name"
    ;; *) fail "publication revocation marker returned an invalid state" ;; esac
  immutable_sha="$(python3 "$INTENT_HELPER" immutable --path "$INTENT" --repo "$REPO")" ||
    fail "cannot freeze publication intent"
  immutable_sha="${immutable_sha//$'\r'/}"
  state_sha="$(python3 "$INTENT_HELPER" state --path "$INTENT" --repo "$REPO")" ||
    fail "cannot freeze publication intent state"
  state_sha="${state_sha//$'\r'/}"
  phase="$(intent_field phase)"
  head="$(intent_field head_sha)"
  tree="$(intent_field tree_sha)"
  branch="$(intent_field branch)"
  base="$(intent_field base)"
  base_sha="$(intent_field base_sha)"
  remote="$(intent_field remote)"
  slug="$(intent_field repo_slug)"
  fetch_url="$(intent_field fetch_url)"
  push_url="$(intent_field push_url)"
  viewer_login="$(intent_field viewer_login)"
  spec_sha="$(intent_field spec_sha256)"
  verify="$(intent_field verify)"
  title="$(intent_field title)"
  title_sha="$(intent_field title_sha256)"
  body_sha="$(intent_field body_sha256)"
  saved_url="$(intent_field pr_url)"
  saved_number="$(intent_field pr_number)"
  phase="${phase//$'\r'/}"; head="${head//$'\r'/}"; tree="${tree//$'\r'/}"
  branch="${branch//$'\r'/}"; base="${base//$'\r'/}"; base_sha="${base_sha//$'\r'/}"
  remote="${remote//$'\r'/}"; slug="${slug//$'\r'/}"; verify="${verify//$'\r'/}"
  fetch_url="${fetch_url//$'\r'/}"; push_url="${push_url//$'\r'/}"
  viewer_login="${viewer_login//$'\r'/}"; spec_sha="${spec_sha//$'\r'/}"
  title="${title//$'\r'/}"; title_sha="${title_sha//$'\r'/}"; body_sha="${body_sha//$'\r'/}"
  saved_url="${saved_url//$'\r'/}"; saved_number="${saved_number//$'\r'/}"
  current_immutable="$(python3 "$INTENT_HELPER" immutable --path "$INTENT" --repo "$REPO")" ||
    fail "cannot revalidate publication intent"
  current_immutable="${current_immutable//$'\r'/}"
  [ "$current_immutable" = "$immutable_sha" ] ||
    fail "publication intent changed while its fields were loaded"
  current_state="$(python3 "$INTENT_HELPER" state --path "$INTENT" --repo "$REPO")" ||
    fail "cannot revalidate publication intent state"
  current_state="${current_state//$'\r'/}"
  [ "$current_state" = "$state_sha" ] ||
    fail "publication intent state changed while its fields were loaded"
  REMOTE="$remote"
  BASE="$base"
  EXPECTED_SPEC_SHA256="$spec_sha"
  case "$REMOTE" in *[!A-Za-z0-9._-]*|"") fail "intent remote is invalid" ;; esac
  "$GIT" check-ref-format --branch "$branch" >/dev/null 2>&1 || fail "intent branch is invalid"
  "$GIT" check-ref-format --branch "$BASE" >/dev/null 2>&1 || fail "intent base is invalid"
  strict_remote
  [ "$(printf '%s' "$REMOTE_SLUG" | tr '[:upper:]' '[:lower:]')" = \
    "$(printf '%s' "$slug" | tr '[:upper:]' '[:lower:]')" ] ||
    park "remote-changed" "restore the reviewed GitHub remote"
  [ "$REMOTE_FETCH_URL" = "$fetch_url" ] && [ "$REMOTE_PUSH_URL" = "$push_url" ] ||
    park "remote-url-changed" "restore the exact reviewed fetch/push URLs"
  require_github_write "$slug"
  [ "$GH_VIEWER_LOGIN" = "$viewer_login" ] ||
    park "github-viewer-changed" "publish with the GitHub identity that prepared the intent"
  assert_snapshot "$branch" "$head" "$tree" "$base_sha"
  verifier_scan="$(agent_memory_mktemp)" || fail "cannot allocate verifier scan"
  printf '%s\n' "$verify" > "$verifier_scan"
  verifier_scan_rc=0
  draft_file_has_sensitive_content "$verifier_scan" || verifier_scan_rc=$?
  rm -f "$verifier_scan"
  case "$verifier_scan_rc" in
    0) fail "intent verifier contains sensitive durable text" ;;
    1) ;;
    *) park "sensitive-scan-failed" "restore the local sensitive-content scanner" ;;
  esac
  open_publish_dir_guard
  verifier_rc=0
  run_verifier "$verify" || verifier_rc=$?
  current_immutable="$(python3 "$INTENT_HELPER" immutable --path "$INTENT" --repo "$REPO")" || {
    poison_intent
    park "intent-changed-by-verifier" "this intent is terminal; prepare with a new branch name"
  }
  current_immutable="${current_immutable//$'\r'/}"
  if [ "$current_immutable" != "$immutable_sha" ]; then
    poison_intent
    park "intent-changed-by-verifier" "this intent is terminal; prepare with a new branch name"
  fi
  current_state="$(python3 "$INTENT_HELPER" state --path "$INTENT" --repo "$REPO")" || {
    poison_intent
    park "intent-state-changed-by-verifier" "this intent is terminal; prepare with a new branch name"
  }
  current_state="${current_state//$'\r'/}"
  if [ "$current_state" != "$state_sha" ]; then
    poison_intent
    park "intent-state-changed-by-verifier" "this intent is terminal; prepare with a new branch name"
  fi
  blocked="$(python3 "$INTENT_HELPER" blocked --path "$INTENT" --repo "$REPO")" || {
    poison_intent
    park "intent-revocation-changed-by-verifier" \
      "this intent is terminal; prepare with a new branch name"
  }
  blocked="${blocked//$'\r'/}"
  case "$blocked" in
    0) close_publish_dir_guard ;;
    1)
      close_publish_dir_guard
      park "intent-terminally-blocked" "prepare a new intent with a new branch name"
      ;;
    *)
      poison_intent
      park "intent-revocation-changed-by-verifier" \
        "this intent is terminal; prepare with a new branch name"
      ;;
  esac
  [ "$verifier_rc" -eq 0 ] ||
    park "verification-failed" "repair the exact acceptance command (exit $verifier_rc)"
  assert_native_git_graph
  assert_snapshot "$branch" "$head" "$tree" "$base_sha"
  strict_remote
  [ "$REMOTE_FETCH_URL" = "$fetch_url" ] && [ "$REMOTE_PUSH_URL" = "$push_url" ] ||
    park "remote-changed-during-verification" "restore the reviewed remote configuration"
  require_github_write "$slug"
  [ "$GH_VIEWER_LOGIN" = "$viewer_login" ] ||
    park "github-viewer-changed" "restore the GitHub identity used for preparation"
  scan_publish_range "$base_sha" "$head"
  armed="$(python3 "$INTENT_HELPER" armed --path "$INTENT" --repo "$REPO")" ||
    park "push-attempt-state-invalid" "inspect the exact publication intent and sidecars"
  armed="${armed//$'\r'/}"
  case "$armed" in 0|1) ;; *) park "push-attempt-state-invalid" "inspect the publication sidecars" ;; esac
  case "$phase" in
    prepared) ;;
    pushing|pushed|complete)
      [ "$armed" = 1 ] ||
        park "push-attempt-state-missing" "later phases require the durable push-attempt marker"
      ;;
  esac

  current_sha="$(remote_ref_sha "$push_url" "$branch")"
  if [ "$current_sha" = absent ]; then
    [ "$armed" = 0 ] ||
      park "published-branch-missing" "an armed intent never recreates a deleted or uncertain branch"
    [ "$phase" = prepared ] ||
      park "published-branch-missing" "later intent phases never recreate a deleted branch"
    push_out="$(agent_memory_mktemp)" || fail "cannot allocate push diagnostics"
    # Persist uncertainty before the remote effect. If the process dies after
    # this point, an absent branch is never recreated: deletion remains a
    # reliable revocation signal even when push completion was not recorded.
    python3 "$INTENT_HELPER" arm --path "$INTENT" --repo "$REPO" \
      --expected-immutable-sha256 "$immutable_sha" \
      --expected-state-sha256 "$state_sha" >/dev/null ||
      park "intent-state-raced-before-push" "inspect the publication intent"
    armed=1
    state_sha="$(python3 "$INTENT_HELPER" mark --path "$INTENT" --repo "$REPO" \
      --phase pushing --expected-phase prepared \
      --expected-immutable-sha256 "$immutable_sha" --expected-state-sha256 "$state_sha")" ||
      park "intent-state-raced-before-push" "inspect the publication intent"
    state_sha="${state_sha//$'\r'/}"
    phase=pushing
    current_state="$(python3 "$INTENT_HELPER" state --path "$INTENT" --repo "$REPO")" ||
      park "intent-state-unreadable-before-push" "inspect the publication intent"
    current_state="${current_state//$'\r'/}"
    [ "$current_state" = "$state_sha" ] ||
      park "intent-state-raced-before-push" "inspect the publication intent"
    push_rc=0
    # The worker's legitimate write phase can plant .git/hooks/pre-push or set
    # push.gpgSign/gpg.program in worker-writable .git/config; both would run
    # arbitrary code under the publisher's credentials at push time. Suppress
    # hooks and signing; verification already happened against the frozen tree.
    draft_run_push "$GIT" -C "$REPO" push --no-verify --no-signed \
      --no-follow-tags --recurse-submodules=no \
      --force-with-lease="refs/heads/$branch:" "$push_url" \
      "$head:refs/heads/$branch" > "$push_out" 2>&1 || push_rc=$?
    current_sha="$(remote_ref_sha "$push_url" "$branch")"
    if [ "$push_rc" -ne 0 ] && [ "$current_sha" != "$head" ]; then
      echo "--- create-only push output (last 20 lines) ---" >&2
      tail -n 20 "$push_out" >&2
      rm -f "$push_out"
      if [ "$current_sha" = absent ]; then
        park "create-only-push-rejected" "this uncertain intent is spent; fix the failure, create a new branch name, and prepare a new intent"
      fi
      park "create-only-push-raced" "another writer created the source branch"
    fi
    rm -f "$push_out"
  fi
  [ "$current_sha" = "$head" ] ||
    park "source-branch-conflict" "existing remote branches are never updated"
  assert_snapshot "$branch" "$head" "$tree" "$base_sha"
  if [ "$phase" = prepared ]; then
    # Recovery of an older prepared intent whose exact remote ref is already
    # observable still enters the uncertainty phase before being confirmed.
    if [ "$armed" = 0 ]; then
      python3 "$INTENT_HELPER" arm --path "$INTENT" --repo "$REPO" \
        --expected-immutable-sha256 "$immutable_sha" \
        --expected-state-sha256 "$state_sha" >/dev/null ||
        park "intent-state-raced-after-push" "inspect the publication intent"
      armed=1
    fi
    state_sha="$(python3 "$INTENT_HELPER" mark --path "$INTENT" --repo "$REPO" \
      --phase pushing --expected-phase prepared \
      --expected-immutable-sha256 "$immutable_sha" --expected-state-sha256 "$state_sha")" ||
      park "intent-state-raced-after-push" "inspect the publication intent"
    state_sha="${state_sha//$'\r'/}"
    phase=pushing
  fi
  if [ "$phase" = pushing ]; then
    state_sha="$(python3 "$INTENT_HELPER" mark --path "$INTENT" --repo "$REPO" \
      --phase pushed --expected-phase pushing \
      --expected-immutable-sha256 "$immutable_sha" --expected-state-sha256 "$state_sha")" ||
      park "intent-state-raced-after-push" "inspect the publication intent"
    state_sha="${state_sha//$'\r'/}"
    phase=pushed
  fi

  pr_json="$(agent_memory_mktemp)" || fail "cannot allocate PR query state"
  query_prs "$slug" "$branch" "$pr_json"
  pr_state="$(python3 "$INTENT_HELPER" inspect-pr --json-file "$pr_json" \
    --branch "$branch" --base "$BASE" --base-sha "$base_sha" --head "$head" \
    --title-sha256 "$title_sha" --body-sha256 "$body_sha")" || {
      rm -f "$pr_json"
      park "existing-pr-conflict" "inspect the source branch's pull requests"
    }
  pr_state="${pr_state//$'\r'/}"
  if [ "$pr_state" = none ]; then
    # GitHub's create API accepts branch names rather than expected object IDs.
    # Narrow that unavoidable CAS window to the request itself, then validate
    # both OIDs again from GitHub immediately after creation.
    assert_snapshot "$branch" "$head" "$tree" "$base_sha"
    current_sha="$(remote_ref_sha "$push_url" "$branch")"
    [ "$current_sha" = "$head" ] ||
      park "source-branch-moved-before-pr" "restore the exact reviewed source ref"
    body_file="$(agent_memory_mktemp)" || { rm -f "$pr_json"; fail "cannot allocate PR body"; }
    python3 "$INTENT_HELPER" body --path "$INTENT" --repo "$REPO" --output "$body_file" \
      --expected-immutable-sha256 "$immutable_sha" --expected-state-sha256 "$state_sha"
    create_out="$(agent_memory_mktemp)" || { rm -f "$body_file" "$pr_json"; fail "cannot allocate PR diagnostics"; }
    create_rc=0
    GH_HOST=github.com draft_run_github "$GH" pr create --repo "github.com/$slug" \
      --draft --base "$BASE" --head "$branch" \
      --title "$title" --body-file "$body_file" > "$create_out" 2>&1 || create_rc=$?
    if [ "$create_rc" -ne 0 ]; then
      echo "--- Draft PR create output (last 20 lines) ---" >&2
      tail -n 20 "$create_out" >&2
      rm -f "$create_out"
      rm -f "$body_file" "$pr_json"
      park "draft-pr-create-failed" "retry publish; remote state will be observed"
    fi
    rm -f "$create_out"
    rm -f "$body_file"
    query_prs "$slug" "$branch" "$pr_json"
    pr_state="$(python3 "$INTENT_HELPER" inspect-pr --json-file "$pr_json" \
      --branch "$branch" --base "$BASE" --base-sha "$base_sha" --head "$head" \
      --title-sha256 "$title_sha" --body-sha256 "$body_sha")" || {
        rm -f "$pr_json"
        park "draft-pr-not-observable" "retry after GitHub records the Draft PR"
      }
    pr_state="${pr_state//$'\r'/}"
  fi
  rm -f "$pr_json"
  assert_snapshot "$branch" "$head" "$tree" "$base_sha"
  current_sha="$(remote_ref_sha "$push_url" "$branch")"
  [ "$current_sha" = "$head" ] ||
    park "source-branch-moved" "the exact Draft PR publication no longer matches"
  case "$pr_state" in
    exact$'\t'*)
      pr_url="$(printf '%s' "$pr_state" | awk -F '\t' '{print $2}')"
      pr_number="$(printf '%s' "$pr_state" | awk -F '\t' '{print $3}')"
      ;;
    *) park "draft-pr-not-exact" "inspect GitHub's PR state" ;;
  esac
  case "$pr_number" in *[!0-9]*|"") park "draft-pr-number-invalid" "inspect GitHub's PR state" ;; esac
  [ -n "$pr_url" ] || park "draft-pr-url-missing" "inspect GitHub's PR state"
  if [ "$phase" = complete ]; then
    [ "$saved_url" = "$pr_url" ] && [ "$saved_number" = "$pr_number" ] ||
      park "completed-pr-identity-changed" "inspect the completed publication intent"
  fi
  if [ "$phase" != complete ]; then
    state_sha="$(python3 "$INTENT_HELPER" mark --path "$INTENT" --repo "$REPO" \
      --phase complete --url "$pr_url" --number "$pr_number" \
      --expected-phase pushed --expected-immutable-sha256 "$immutable_sha" \
      --expected-state-sha256 "$state_sha")" ||
      park "intent-state-raced-after-pr" "inspect the exact Draft PR and local intent"
    state_sha="${state_sha//$'\r'/}"
  fi

  # CI is an observer after publication. Its outage is visible but cannot
  # mutate, repair, merge, or invalidate the already-created Draft PR.
  if ! (cd "$REPO" && GH_HOST=github.com GH_REPO="github.com/$slug" \
      OMS_GH_BIN="$GH" "$ROOT/scripts/ci-status.sh" record "$branch" \
      >/dev/null 2>&1); then
    echo "draft-pr: CI observation unavailable; retry: oms ci-status record $branch" >&2
  fi
  echo "draft-pr: published $pr_url"
}

case "$ACTION" in
  prepare)
    [ -z "$INTENT" ] || fail "prepare does not accept --intent"
    prepare
    ;;
  publish)
    [ -n "$INTENT" ] || fail "publish requires --intent"
    [ -z "$BASE" ] && [ -z "$VERIFY" ] &&
      [ -z "$EXPECTED_HEAD$EXPECTED_TREE$EXPECTED_BASE_SHA$EXPECTED_SPEC_SHA256" ] ||
      fail "publish reads every frozen value from the intent"
    # The helper resolves and rejects symlinks/out-of-repository paths before
    # the lock key is trusted; a field read is also a complete schema check.
    INTENT="$(python3 "$INTENT_HELPER" canonical --path "$INTENT" --repo "$REPO")" ||
      fail "intent is not a canonical private publication file"
    INTENT="${INTENT//$'\r'/}"
    "$GIT" -C "$REPO" check-ignore -q -- "$INTENT" 2>/dev/null ||
      fail "publication intents must remain Git-ignored"
    oms_with_file_lock "$INTENT" publish_locked
    ;;
esac
