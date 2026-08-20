#!/usr/bin/env bash
set -euo pipefail

# Publishing is the only new remote-write boundary. It binds one clean HEAD to
# a create-only branch push and a draft PR, and it has no merge/release/update
# surface. A prepared intent can be replayed after either remote effect.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-draft-pr.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

sha256_file() {
  python3 - "$1" <<'PY'
import hashlib, sys
with open(sys.argv[1], "rb") as handle:
    print(hashlib.sha256(handle.read()).hexdigest())
PY
}

REAL_GIT="$(command -v git)"

make_repo() {
  local repo="$1"
  local bare="$2"

  mkdir -p "$repo"
  "$REAL_GIT" -C "$repo" init -q
  "$REAL_GIT" -C "$repo" config user.email test@example.com
  "$REAL_GIT" -C "$repo" config user.name Test
  # Command output and stub ledgers are deliberately created by the shell
  # before draft-pr starts. Keep them outside the clean-tree contract being
  # exercised by this fixture.
  printf '/.oms/\n/PROJECT.md\n/git.log\n/gh.log\n/pr-created*\n/*.out\n' > "$repo/.gitignore"
  printf 'confirmed local contract\n' > "$repo/PROJECT.md"
  printf 'base\n' > "$repo/file.txt"
  "$REAL_GIT" -C "$repo" add .gitignore file.txt
  "$REAL_GIT" -C "$repo" commit -qm base
  "$REAL_GIT" -C "$repo" branch -M main
  "$REAL_GIT" init -q --bare "$bare"
  "$REAL_GIT" -C "$repo" remote add origin "$bare"
  "$REAL_GIT" -C "$repo" push -q -u origin main
  "$REAL_GIT" -C "$repo" switch -qc codex/draft-fixture
  printf 'feature\n' >> "$repo/file.txt"
  "$REAL_GIT" -C "$repo" commit -qam 'feat: draft fixture'
}

write_git_wrapper() {
  local path="$1"
  cat > "$path" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "\$OMS_T_GIT_LOG"
repo_path=""
[ "\${1:-}" != -C ] || repo_path="\${2:-}"
if [ -n "\$repo_path" ] && "$REAL_GIT" -C "\$repo_path" config --bool oms.test.statusFail 2>/dev/null | grep -qx true; then
  case "\$*" in
    *' status --porcelain --untracked-files=all') exit 9 ;;
  esac
fi
if [ -n "\$repo_path" ] && "$REAL_GIT" -C "\$repo_path" config --bool oms.test.mergeBaseDelay 2>/dev/null | grep -qx true; then
  case "\$*" in
    *' merge-base --is-ancestor '*)
      : > "\$repo_path/.git/merge-base-delay-started"
      trap '' TERM
      sleep "\${OMS_T_SCAN_DELAY:-2}"
      trap - TERM
      : > "\$repo_path/.git/merge-base-delay-completed"
      ;;
  esac
fi
if [ -n "\$repo_path" ] && "$REAL_GIT" -C "\$repo_path" config --bool oms.test.countDelay 2>/dev/null | grep -qx true; then
  case "\$*" in
    *' rev-list --count '*)
      : > "\$repo_path/.git/rev-count-delay-started"
      trap '' TERM
      sleep "\${OMS_T_SCAN_DELAY:-2}"
      trap - TERM
      : > "\$repo_path/.git/rev-count-delay-completed"
      ;;
  esac
fi
if [ -n "\$repo_path" ] && "$REAL_GIT" -C "\$repo_path" config --bool oms.test.scanDelay 2>/dev/null | grep -qx true; then
  case "\$*" in
    *' cat-file -t '*)
      : > "\$repo_path/.git/scan-delay-started"
      trap '' TERM
      sleep "\${OMS_T_SCAN_DELAY:-2}"
      trap - TERM
      : > "\$repo_path/.git/scan-delay-completed"
      ;;
  esac
fi
case "\$*" in
  *' push '*)
    if [ -n "\$repo_path" ] && "$REAL_GIT" -C "\$repo_path" config --bool oms.test.pushDelay 2>/dev/null | grep -qx true; then
      : > "\$repo_path/.git/push-delay-started"
      trap '' TERM HUP INT
      sleep "\${OMS_T_PUSH_DELAY:-3}"
      trap - TERM HUP INT
      : > "\$repo_path/.git/push-delay-completed"
    fi
    if [ -n "\$repo_path" ] && "$REAL_GIT" -C "\$repo_path" config --bool oms.test.commitBeforePush 2>/dev/null | grep -qx true; then
      "$REAL_GIT" -C "\$repo_path" config --unset oms.test.commitBeforePush
      "$REAL_GIT" -C "\$repo_path" commit -q --allow-empty -m 'chore: concurrent local commit'
    fi
    ;;
esac
case "\$*" in
  *' remote get-url --push --all origin')
    echo 'git@github.com:eightmm/oh-my-setting.git'
    if [ -n "\$repo_path" ] && "$REAL_GIT" -C "\$repo_path" config --bool oms.test.extraPush 2>/dev/null | grep -qx true; then
      echo 'git@github.com:unreviewed/other.git'
    fi
    exit 0
    ;;
  *' remote get-url --all origin') echo 'git@github.com:eightmm/oh-my-setting.git'; exit 0 ;;
esac
translated=()
for arg in "\$@"; do
  [ "\$arg" != 'git@github.com:eightmm/oh-my-setting.git' ] || arg="\$OMS_T_BARE"
  translated+=("\$arg")
done
exec "$REAL_GIT" "\${translated[@]}"
EOF
  chmod +x "$path"
}

write_gh_stub() {
  local path="$1"
  cat > "$path" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$OMS_T_GH_LOG"
# gh does not support "<owner>:<branch>" for pr list --head, and the
# owner-qualified create form breaks on organization owners. The stub is
# strict so the unsupported forms cannot silently return to the publisher.
case "${1:-} ${2:-}" in
  'pr list'|'pr create')
    prev=''
    for arg in "$@"; do
      if [ "$prev" = --head ]; then
        case "$arg" in *:*) echo 'unsupported owner-qualified --head' >&2; exit 64 ;; esac
      fi
      prev="$arg"
    done
    ;;
esac
case "${1:-} ${2:-}" in
  'auth status')
    [ "${OMS_T_GH_AUTH:-ok}" = ok ] || { echo 'not logged in' >&2; exit 4; }
    ;;
  'repo view')
    printf '{"nameWithOwner":"eightmm/oh-my-setting","viewerPermission":"WRITE"}\n'
    ;;
  'api --hostname')
    printf '%s\n' "${OMS_T_GH_VIEWER:-fixture-user}"
    ;;
  'pr list')
    if [ -f "$OMS_T_PR_MARKER" ]; then
      python3 - "$OMS_T_PR_MARKER.title" "$OMS_T_PR_MARKER.body" <<'PY'
import json, os, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    title = handle.read()
with open(sys.argv[2], encoding="utf-8") as handle:
    body = handle.read()
print(json.dumps([{
    "number": 1, "url": "https://example.invalid/pr/1",
    "state": "OPEN", "isDraft": True, "title": title, "body": body,
    "headRefName": os.environ["OMS_T_BRANCH"],
    "baseRefName": "main", "baseRefOid": os.environ["OMS_T_BASE"],
    "headRefOid": os.environ["OMS_T_HEAD"], "isCrossRepository": False,
}]))
PY
    else
      printf '[]\n'
    fi
    ;;
  'pr create')
    case " $* " in *' --draft '*) ;; *) echo 'missing --draft' >&2; exit 9 ;; esac
    title=''
    body_file=''
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --title) title="$2"; shift 2 ;;
        --body-file) body_file="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    printf '%s' "$title" > "$OMS_T_PR_MARKER.title"
    cp "$body_file" "$OMS_T_PR_MARKER.body"
    : > "$OMS_T_PR_MARKER"
    echo 'https://example.invalid/pr/1'
    ;;
  *)
    echo "unexpected gh call: $*" >&2
    exit 8
    ;;
esac
EOF
  chmod +x "$path"
}

run_draft_pr() {
  local repo="$1"
  shift
  OMS_GIT_BIN="$TMP/bin/git" OMS_GH_BIN="$TMP/bin/gh" \
    OMS_T_GIT_LOG="$repo/git.log" OMS_T_GH_LOG="$repo/gh.log" \
    OMS_T_BARE="$(git -C "$repo" remote get-url origin)" \
    OMS_T_PR_MARKER="$repo/pr-created" \
    OMS_T_BRANCH="codex/draft-fixture" \
    OMS_T_BASE="$($REAL_GIT -C "$repo" rev-parse main)" \
    OMS_T_HEAD="$($REAL_GIT -C "$repo" rev-parse HEAD)" \
    "$ROOT/scripts/draft-pr.sh" --repo "$repo" "$@"
}

test_remote_parser() {
  local parser="$ROOT/scripts/lib/github-remote.py"
  [ "$(python3 "$parser" git@github.com:eightmm/oh-my-setting.git)" = \
    'eightmm/oh-my-setting' ] || fail "scp GitHub remote was not normalized"
  [ "$(python3 "$parser" https://github.com/EightMM/oh-my-setting.git)" = \
    'EightMM/oh-my-setting' ] || fail "HTTPS GitHub remote was not normalized"
  if python3 "$parser" 'https://token@github.com/eightmm/oh-my-setting.git' >/dev/null 2>&1; then
    fail "credential-bearing remote should be rejected"
  fi
  if python3 "$parser" 'https://gitlab.com/eightmm/oh-my-setting.git' >/dev/null 2>&1; then
    fail "non-GitHub remote should be rejected"
  fi
  if python3 "$parser" 'ssh://git@github.com:notaport/eightmm/oh-my-setting.git' >/dev/null 2>&1; then
    fail "malformed GitHub port should be rejected without a traceback"
  fi
  if python3 "$parser" 'ssh://git@[github.com/eightmm/oh-my-setting.git' >/dev/null 2>&1; then
    fail "malformed bracketed host should be rejected without a traceback"
  fi

  # A rejected internal helper path must not create its outside parent before
  # discovering that it escaped the private publication directory.
  local helper_repo="$TMP/helper-repo"
  local outside="$TMP/helper-escape"
  mkdir -p "$helper_repo/.oms/publish"
  printf 'body\n' > "$TMP/body.md"
  if python3 "$ROOT/scripts/lib/draft-pr-intent.py" create \
      --repo "$helper_repo" --path "$outside/intent.json" \
      --head 0000000000000000000000000000000000000000 \
      --tree 0000000000000000000000000000000000000000 \
      --branch codex/test --base main \
      --base-sha 0000000000000000000000000000000000000000 \
      --remote origin --repo-slug eightmm/oh-my-setting \
      --fetch-url git@github.com:eightmm/oh-my-setting.git \
      --push-url git@github.com:eightmm/oh-my-setting.git \
      --viewer-login fixture-user --verify true --title test \
      --body-file "$TMP/body.md" >/dev/null 2>&1; then
    fail "outside intent path should be rejected"
  fi
  [ ! -e "$outside" ] || fail "rejected intent path created an outside directory"

  local symlink_repo="$TMP/helper-symlink-repo"
  local symlink_out="$TMP/helper-symlink-out"
  mkdir -p "$symlink_repo" "$symlink_out"
  ln -s "$symlink_out" "$symlink_repo/.oms"
  if python3 "$ROOT/scripts/lib/draft-pr-intent.py" create \
      --repo "$symlink_repo" --path "$symlink_repo/.oms/publish/intent.json" \
      --head 0000000000000000000000000000000000000000 \
      --tree 0000000000000000000000000000000000000000 \
      --branch codex/test --base main \
      --base-sha 0000000000000000000000000000000000000000 \
      --remote origin --repo-slug eightmm/oh-my-setting \
      --fetch-url git@github.com:eightmm/oh-my-setting.git \
      --push-url git@github.com:eightmm/oh-my-setting.git \
      --viewer-login fixture-user --verify true --title test \
      --body-file "$TMP/body.md" >/dev/null 2>&1; then
    fail "symlinked .oms intent root should be rejected"
  fi
  [ ! -e "$symlink_out/publish" ] || fail "symlinked .oms escape was touched"

  rm -f "$symlink_repo/.oms"
  mkdir -p "$symlink_repo/.oms/publish" "$symlink_out/nested"
  ln -s "$symlink_out/nested" "$symlink_repo/.oms/publish/link"
  if python3 "$ROOT/scripts/lib/draft-pr-intent.py" create \
      --repo "$symlink_repo" --path "$symlink_repo/.oms/publish/link/intent.json" \
      --head 0000000000000000000000000000000000000000 \
      --tree 0000000000000000000000000000000000000000 \
      --branch codex/test --base main \
      --base-sha 0000000000000000000000000000000000000000 \
      --remote origin --repo-slug eightmm/oh-my-setting \
      --fetch-url git@github.com:eightmm/oh-my-setting.git \
      --push-url git@github.com:eightmm/oh-my-setting.git \
      --viewer-login fixture-user --verify true --title test \
      --body-file "$TMP/body.md" >/dev/null 2>&1; then
    fail "nested symlink intent path should be rejected"
  fi
  [ ! -e "$symlink_out/nested/intent.json" ] || fail "nested symlink escape was touched"

  # The helper's first writer is create-only. Two callers synchronized at the
  # link boundary cannot overwrite one another's fully written intent.
  local atomic_repo="$TMP/helper-atomic-repo"
  mkdir -p "$atomic_repo/.oms/publish"
  chmod 700 "$atomic_repo/.oms/publish"
  python3 - "$ROOT/scripts/lib/draft-pr-intent.py" \
      "$atomic_repo/.oms/publish/race.json" <<'PY' || fail "intent exclusive-create race failed"
import contextlib, errno, importlib.util, io, json, sys, threading

spec = importlib.util.spec_from_file_location("draft_pr_intent", sys.argv[1])
module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(module)
path = module.Path(sys.argv[2])
barrier = threading.Barrier(2)
real_link = module.os.link
results = []
errors = []

def synchronized_link(source, target):
    barrier.wait(timeout=5)
    return real_link(source, target)

def writer(value):
    try:
        results.append(module.atomic_create(path, {"writer": value}))
    except BaseException as exc:
        errors.append(exc)

module.os.link = synchronized_link
threads = [threading.Thread(target=writer, args=(value,)) for value in ("a", "b")]
for thread in threads:
    thread.start()
for thread in threads:
    thread.join(10)
assert not errors, errors
assert sorted(results) == [False, True], results
with path.open(encoding="utf-8") as handle:
    assert json.load(handle)["writer"] in ("a", "b")

# File fsync success followed by a real directory I/O failure must never be
# reported as a durable create or replace.
module.os.link = real_link
real_fsync = module.os.fsync

def fail_directory_fsync(fd):
    fail_directory_fsync.calls += 1
    if fail_directory_fsync.calls == 2:
        raise OSError(errno.EIO, "fixture directory fsync failure")
    return real_fsync(fd)

fail_directory_fsync.calls = 0
module.os.fsync = fail_directory_fsync
with contextlib.redirect_stderr(io.StringIO()):
    try:
        module.atomic_create(path.parent / "fsync-create.json", {"value": 1})
    except SystemExit:
        pass
    else:
        raise AssertionError("directory EIO was accepted during atomic create")

module.os.fsync = real_fsync
write_path = path.parent / "fsync-write.json"
assert module.atomic_create(write_path, {"value": 1})
fail_directory_fsync.calls = 0
module.os.fsync = fail_directory_fsync
with contextlib.redirect_stderr(io.StringIO()):
    try:
        module.atomic_write(write_path, {"value": 2})
    except SystemExit:
        pass
    else:
        raise AssertionError("directory EIO was accepted during atomic replace")
module.os.fsync = real_fsync
PY

  # A raced deterministic path with different immutable bytes is terminally
  # poisoned; a later invocation cannot adopt the collision as its baseline.
  local collision_repo="$TMP/helper-collision-repo"
  local collision_intent="$collision_repo/.oms/publish/collision.json"
  mkdir -p "$collision_repo/.oms/publish"
  chmod 700 "$collision_repo/.oms" "$collision_repo/.oms/publish"
  printf 'first body\n' > "$TMP/collision-first.md"
  printf 'second body\n' > "$TMP/collision-second.md"
  python3 "$ROOT/scripts/lib/draft-pr-intent.py" create \
    --repo "$collision_repo" --path "$collision_intent" \
    --head 0000000000000000000000000000000000000000 \
    --tree 0000000000000000000000000000000000000000 \
    --branch codex/collision --base main \
    --base-sha 0000000000000000000000000000000000000000 \
    --remote origin --repo-slug eightmm/oh-my-setting \
    --fetch-url git@github.com:eightmm/oh-my-setting.git \
    --push-url git@github.com:eightmm/oh-my-setting.git \
    --viewer-login fixture-user --verify true --title first \
    --body-file "$TMP/collision-first.md" >/dev/null ||
    fail "collision fixture initial intent failed"
  if python3 "$ROOT/scripts/lib/draft-pr-intent.py" create \
      --repo "$collision_repo" --path "$collision_intent" \
      --head 0000000000000000000000000000000000000000 \
      --tree 0000000000000000000000000000000000000000 \
      --branch codex/collision --base main \
      --base-sha 0000000000000000000000000000000000000000 \
      --remote origin --repo-slug eightmm/oh-my-setting \
      --fetch-url git@github.com:eightmm/oh-my-setting.git \
      --push-url git@github.com:eightmm/oh-my-setting.git \
      --viewer-login fixture-user --verify true --title second \
      --body-file "$TMP/collision-second.md" >/dev/null 2>&1; then
    fail "mismatched deterministic intent collision was accepted"
  fi
  [ "$(python3 "$ROOT/scripts/lib/draft-pr-intent.py" blocked \
      --repo "$collision_repo" --path "$collision_intent")" = 1 ] ||
    fail "mismatched deterministic intent collision was not terminally blocked"
}

test_prepare_publish_recovery() {
  local repo="$TMP/repo"
  local bare="$TMP/remote.git"
  local intent remote_head rc=0

  mkdir -p "$TMP/bin"
  make_repo "$repo" "$bare"
  write_git_wrapper "$TMP/bin/git"
  write_gh_stub "$TMP/bin/gh"
  : > "$repo/git.log"
  : > "$repo/gh.log"
  "$REAL_GIT" -C "$repo" tag -a v-test -m 'must not follow this tag'
  "$REAL_GIT" -C "$repo" config push.followTags true

  (
    umask 0000
    GH_HOST=enterprise.invalid run_draft_pr "$repo" prepare --remote origin --base main --verify true \
      --expected-head "$($REAL_GIT -C "$repo" rev-parse HEAD)" \
      --expected-tree "$($REAL_GIT -C "$repo" rev-parse 'HEAD^{tree}')" \
      --expected-base-sha "$($REAL_GIT -C "$repo" rev-parse main)" \
      --expected-spec-sha256 "$(sha256_file "$repo/PROJECT.md")"
  ) > "$repo/prepare.out" || fail "draft intent prepare failed under permissive umask"
  intent="$(sed -n 's/^intent: //p' "$repo/prepare.out" | tail -n 1)"
  [ -n "$intent" ] && [ -f "$intent" ] || fail "prepared intent missing"
  python3 - "$intent" <<'PY' || fail "prepared intent contract is incomplete"
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    row = json.load(handle)
assert row["schema"] == 1 and row["kind"] == "draft-pr-intent"
assert row["phase"] == "prepared"
assert row["draft"] is True and row["create_only"] is True
assert row["merge"] is False and row["release"] is False
assert row["push_attempted"] is False
assert row["branch"] == "codex/draft-fixture" and row["base"] == "main"
assert row["spec_sha256"]
assert row["viewer_login"] == "fixture-user"
PY

  GH_HOST=enterprise.invalid run_draft_pr "$repo" publish --intent "$intent" > "$repo/publish.out" ||
    fail "exact draft PR publish failed: $(tail -8 "$repo/publish.out")"
  grep -Fq 'draft-pr: published https://example.invalid/pr/1' "$repo/publish.out" ||
    fail "published URL missing"
  remote_head="$($REAL_GIT --git-dir "$bare" rev-parse refs/heads/codex/draft-fixture)"
  [ "$remote_head" = "$($REAL_GIT -C "$repo" rev-parse HEAD)" ] ||
    fail "remote branch does not equal the approved HEAD"
  grep -Fq 'pr create' "$repo/gh.log" || fail "draft PR was not created"
  grep -Fq -- '--draft' "$repo/gh.log" || fail "PR was not forced to draft"
  grep -Fq -- '--no-follow-tags' "$repo/git.log" || fail "push did not disable implicit tags"
  grep -Fq -- '--recurse-submodules=no' "$repo/git.log" || fail "push did not disable submodule writes"
  grep -Fq -- '--no-verify' "$repo/git.log" || fail "push did not suppress local hooks"
  grep -Fq -- '--no-signed' "$repo/git.log" || fail "push did not suppress signing"
  grep -Fq 'Semantic review: not requested.' "$repo/pr-created.body" ||
    fail "the PR body does not disclose the absent semantic review"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/tags/v-test; then
    fail "ambient push.followTags created a forbidden remote tag"
  fi
  grep -Fq 'github.com/eightmm/oh-my-setting' "$repo/gh.log" ||
    fail "GitHub calls were not pinned to github.com"
  if grep -Eq 'pr (merge|ready)|release|refs/tags/' "$repo/gh.log" "$repo/git.log"; then
    fail "publisher exposed merge, ready, release, or tag effects"
  fi

  # A completed intent is idempotent. A crash after push/create is recovered
  # by remote observation instead of a force update or duplicate PR.
  before_create="$(grep -c 'pr create' "$repo/gh.log")"
  python3 - "$intent" <<'PY'
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["phase"] = "pushed"
row["pr_url"] = ""
row["pr_number"] = None
with open(path, "w", encoding="utf-8", newline="\n") as handle:
    json.dump(row, handle, ensure_ascii=False, indent=2)
    handle.write("\n")
PY
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/replay.out" ||
    fail "post-create crash should be recoverable"
  [ "$before_create" = "$(grep -c 'pr create' "$repo/gh.log")" ] ||
    fail "publish replay created a duplicate PR"

  # A completed publication is terminal. Deleting its remote branch revokes
  # it; replay observes the deletion and never recreates the branch.
  "$REAL_GIT" --git-dir "$bare" update-ref -d refs/heads/codex/draft-fixture
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/deleted.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "deleted completed branch should park, got $rc"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "completed intent recreated a deleted source branch"
  fi

  # Auth is checked before any remote branch is created.
  local denied="$TMP/denied"
  local denied_bare="$TMP/denied.git"
  make_repo "$denied" "$denied_bare"
  : > "$denied/git.log"
  : > "$denied/gh.log"
  rc=0
  OMS_T_GH_AUTH=fail run_draft_pr "$denied" prepare --remote origin --base main \
    --verify true > "$denied/auth.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "unauthenticated prepare should fail closed, got $rc"
  if $REAL_GIT --git-dir "$denied_bare" show-ref --verify --quiet \
    refs/heads/codex/draft-fixture; then
    fail "auth failure still created the remote source branch"
  fi

  printf 'dirty\n' >> "$denied/file.txt"
  rc=0
  run_draft_pr "$denied" prepare --remote origin --base main --verify true \
    >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "dirty tree should be a contract error, got $rc"

  # A caller-supplied semantic-review disclosure rides the hashed PR body
  # end to end, and unsafe evidence text is a contract error.
  local evidence_repo="$TMP/evidence"
  local evidence_bare="$TMP/evidence.git"
  make_repo "$evidence_repo" "$evidence_bare"
  : > "$evidence_repo/git.log"; : > "$evidence_repo/gh.log"
  rc=0
  run_draft_pr "$evidence_repo" prepare --remote origin --base main --verify true \
    --review-evidence 'mode=shadow outcome=$(pwned)' >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "unsafe review evidence should be a contract error, got $rc"
  run_draft_pr "$evidence_repo" prepare --remote origin --base main --verify true \
    --review-evidence 'mode=shadow outcome=advisory-fail reviewer=claude' \
    > "$evidence_repo/prepare.out" || fail "evidence prepare failed"
  intent="$(sed -n 's/^intent: //p' "$evidence_repo/prepare.out" | tail -n 1)"
  run_draft_pr "$evidence_repo" publish --intent "$intent" \
    > "$evidence_repo/publish.out" || fail "evidence publish failed"
  grep -Fq 'Semantic review: mode=shadow outcome=advisory-fail reviewer=claude.' \
    "$evidence_repo/pr-created.body" ||
    fail "the PR body does not carry the semantic review disclosure"
}

test_remote_and_recovery_hardening() {
  local repo bare intent rc=0

  # Multiple push URLs are rejected before either remote target is touched.
  repo="$TMP/multiple-push"
  bare="$TMP/multiple-push.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  "$REAL_GIT" -C "$repo" config oms.test.extraPush true
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/multiple.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "multiple push URLs should be a contract error, got $rc"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "multiple-push refusal still changed the approved remote"
  fi

  # A failed status command is unknown repository state, never an empty/clean
  # answer that can proceed to verification or intent persistence.
  repo="$TMP/status-failure"
  bare="$TMP/status-failure.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  "$REAL_GIT" -C "$repo" config oms.test.statusFail true
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/status.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "failed git status should park, got $rc"
  [ ! -d "$repo/.oms/publish" ] || fail "failed git status produced a publication intent"

  # Index visibility flags can make status appear clean while the verifier
  # reads worktree bytes that differ from the frozen HEAD sent by push.
  repo="$TMP/hidden-index-path"
  bare="$TMP/hidden-index-path.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  "$REAL_GIT" -C "$repo" update-index --assume-unchanged file.txt
  printf 'working-only fix\n' > "$repo/file.txt"
  [ -z "$("$REAL_GIT" -C "$repo" status --porcelain --untracked-files=all)" ] ||
    fail "assume-unchanged fixture did not hide the worktree edit"
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main \
    --verify "grep -q 'working-only fix' file.txt" > "$repo/hidden-index.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "hidden index path should park, got $rc"
  grep -Fq 'reason=git-index-hidden-paths' "$repo/hidden-index.out" ||
    fail "hidden index path refusal was not identified"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "verifier-approved hidden worktree bytes reached publication"
  fi

  # The exact object history is outbound payload. Refuse a credential in the
  # final tree before persisting an intent or creating a remote branch.
  repo="$TMP/sensitive-final-tree"
  bare="$TMP/sensitive-final-tree.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  printf '%s%s\n' 'ghp_' '12345678901234567890' > "$repo/leak.txt"
  "$REAL_GIT" -C "$repo" add leak.txt
  "$REAL_GIT" -C "$repo" commit -qm 'test: add outbound fixture'
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/sensitive-final.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "sensitive final tree should park, got $rc"
  grep -Fq 'reason=sensitive-publish-payload' "$repo/sensitive-final.out" ||
    fail "sensitive final tree refusal was not identified"
  if find "$repo/.oms/publish" -type f -name '*.json' -print -quit 2>/dev/null | grep -q .; then
    fail "sensitive final tree produced a publication intent"
  fi
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "sensitive final tree reached the remote"
  fi

  # A new path can reuse a blob already reachable from the base, so object-name
  # output alone is incomplete. Scan changed path history independently.
  repo="$TMP/sensitive-reused-path"
  bare="$TMP/sensitive-reused-path.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  local reused_name
  reused_name="$(printf '%s%s' 'ghp_' '12345678901234567890')"
  "$REAL_GIT" -C "$repo" show main:file.txt > "$repo/$reused_name"
  "$REAL_GIT" -C "$repo" add "$reused_name"
  "$REAL_GIT" -C "$repo" commit -qm 'test: reuse a base blob at a new path'
  [ "$("$REAL_GIT" -C "$repo" rev-parse main:file.txt)" = \
    "$("$REAL_GIT" -C "$repo" rev-parse "HEAD:$reused_name")" ] ||
    fail "reused-path fixture did not reuse the base blob"
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/sensitive-path.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "sensitive reused path should park, got $rc"
  grep -Fq 'reason=sensitive-publish-payload' "$repo/sensitive-path.out" ||
    fail "sensitive reused path refusal was not identified"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "sensitive reused path reached the remote"
  fi

  # A secret removed from the final tree remains in the objects a push sends.
  # Scan every introduced object, not just the visible diff or final snapshot.
  repo="$TMP/sensitive-history"
  bare="$TMP/sensitive-history.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  printf '%s%s\n' '/hom' 'e/fixture/private-note' > "$repo/transient.txt"
  "$REAL_GIT" -C "$repo" add transient.txt
  "$REAL_GIT" -C "$repo" commit -qm 'test: add transient outbound fixture'
  "$REAL_GIT" -C "$repo" rm -q transient.txt
  "$REAL_GIT" -C "$repo" commit -qm 'test: remove transient outbound fixture'
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/sensitive-history.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "sensitive historical object should park, got $rc"
  grep -Fq 'reason=sensitive-publish-payload' "$repo/sensitive-history.out" ||
    fail "sensitive historical object refusal was not identified"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "sensitive historical object reached the remote"
  fi

  # Replacement refs must not make local inspection see a clean substitute
  # while the frozen original OID sends a sensitive commit object.
  repo="$TMP/sensitive-replace-ref"
  bare="$TMP/sensitive-replace-ref.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  local original_commit clean_commit commit_tree commit_parent sensitive_subject
  sensitive_subject="$(printf '%s%s' 'ghp_' '12345678901234567890')"
  "$REAL_GIT" -C "$repo" commit -q --allow-empty -m "$sensitive_subject"
  original_commit="$("$REAL_GIT" -C "$repo" rev-parse HEAD)"
  commit_tree="$("$REAL_GIT" -C "$repo" rev-parse 'HEAD^{tree}')"
  commit_parent="$("$REAL_GIT" -C "$repo" rev-parse 'HEAD^')"
  clean_commit="$(printf 'test: clean replacement\n' | \
    "$REAL_GIT" -C "$repo" commit-tree "$commit_tree" -p "$commit_parent")"
  "$REAL_GIT" -C "$repo" replace "$original_commit" "$clean_commit"
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/sensitive-replace.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "sensitive original behind replace ref should park, got $rc"
  grep -Fq 'reason=sensitive-publish-payload' "$repo/sensitive-replace.out" ||
    fail "native object graph was not scanned behind the replace ref"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "sensitive original behind replace ref reached the remote"
  fi

  # Scanner infrastructure is a tri-state gate. A matcher error is unknown,
  # never equivalent to the matcher's ordinary no-match status.
  repo="$TMP/sensitive-scan-error"
  bare="$TMP/sensitive-scan-error.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  printf '%s%s\n' 'ghp_' '12345678901234567890' > "$repo/scanner-error.txt"
  "$REAL_GIT" -C "$repo" add scanner-error.txt
  "$REAL_GIT" -C "$repo" commit -qm 'test: scanner error fixture'
  local grep_fail_bin="$TMP/grep-fail-bin"
  mkdir -p "$grep_fail_bin"
  cat > "$grep_fail_bin/grep" <<'EOF'
#!/usr/bin/env bash
exit 2
EOF
  chmod +x "$grep_fail_bin/grep"
  local scanner_tmp="$TMP/scanner-error-tmp"
  mkdir -p "$scanner_tmp"
  rc=0
  PATH="$grep_fail_bin:$PATH" TMPDIR="$scanner_tmp" \
    run_draft_pr "$repo" prepare \
    --remote origin --base main --verify true > "$repo/scanner-error.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "sensitive scanner error should park, got $rc"
  grep -Fq 'reason=sensitive-scan-failed' "$repo/scanner-error.out" ||
    fail "sensitive scanner error was not identified"
  [ -z "$(find "$scanner_tmp" -type f -print -quit)" ] ||
    fail "sensitive scanner error left temporary payload files"
  "$REAL_GIT" -C "$repo" show HEAD:scanner-error.txt >/dev/null ||
    fail "scanner-error fixture lost its sensitive commit"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "scanner error reached the remote"
  fi

  # Object enumeration, individual expansion, aggregate bytes, and elapsed
  # time are all explicit limits rather than attacker-controlled work.
  repo="$TMP/payload-resource-bounds"
  bare="$TMP/payload-resource-bounds.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  local topology_started topology_finished topology_elapsed
  "$REAL_GIT" -C "$repo" config oms.test.mergeBaseDelay true
  topology_started="$(date +%s)"
  rc=0
  OMS_T_SCAN_DELAY=10 OMS_MA_TIMEOUT_HAS_KILL_AFTER=0 OMS_PEER_KILL_AFTER=30 \
    OMS_DRAFT_SCAN_TIMEOUT_S=1 run_draft_pr "$repo" prepare \
    --remote origin --base main --verify true > "$repo/merge-base-timeout.out" 2>&1 || rc=$?
  topology_finished="$(date +%s)"
  topology_elapsed="$((topology_finished - topology_started))"
  [ "$rc" = 3 ] || fail "merge-base timeout should park, got $rc"
  grep -Fq 'reason=publish-payload-timeout' "$repo/merge-base-timeout.out" ||
    fail "merge-base timeout was not identified"
  [ -f "$repo/.git/merge-base-delay-started" ] &&
    [ ! -f "$repo/.git/merge-base-delay-completed" ] ||
    fail "merge-base timeout did not kill the TERM-ignoring traversal"
  [ "$topology_elapsed" -le 4 ] ||
    fail "merge-base traversal exceeded hard wall: ${topology_elapsed}s"
  "$REAL_GIT" -C "$repo" config --unset oms.test.mergeBaseDelay

  "$REAL_GIT" -C "$repo" config oms.test.countDelay true
  topology_started="$(date +%s)"
  rc=0
  OMS_T_SCAN_DELAY=10 OMS_MA_TIMEOUT_HAS_KILL_AFTER=0 OMS_PEER_KILL_AFTER=30 \
    OMS_DRAFT_SCAN_TIMEOUT_S=1 run_draft_pr "$repo" prepare \
    --remote origin --base main --verify true > "$repo/rev-count-timeout.out" 2>&1 || rc=$?
  topology_finished="$(date +%s)"
  topology_elapsed="$((topology_finished - topology_started))"
  [ "$rc" = 3 ] || fail "rev-list count timeout should park, got $rc"
  grep -Fq 'reason=publish-payload-timeout' "$repo/rev-count-timeout.out" ||
    fail "rev-list count timeout was not identified"
  [ -f "$repo/.git/rev-count-delay-started" ] &&
    [ ! -f "$repo/.git/rev-count-delay-completed" ] ||
    fail "rev-list count timeout did not kill the TERM-ignoring traversal"
  [ "$topology_elapsed" -le 4 ] ||
    fail "rev-list count traversal exceeded hard wall: ${topology_elapsed}s"
  "$REAL_GIT" -C "$repo" config --unset oms.test.countDelay

  rc=0
  OMS_DRAFT_SCAN_MAX_MEMORY_BYTES=1 run_draft_pr "$repo" prepare \
    --remote origin --base main --verify true > "$repo/traversal-memory.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "low traversal memory ceiling should park, got $rc"
  grep -Eq 'reason=(commit-range-invalid|publish-payload-enumeration-failed)' \
    "$repo/traversal-memory.out" || fail "traversal memory ceiling was not enforced"

  topology_started="$(date +%s)"
  rc=0
  OMS_MA_TIMEOUT_HAS_KILL_AFTER=0 OMS_PEER_KILL_AFTER=30 \
    OMS_DRAFT_VERIFY_TIMEOUT=1 run_draft_pr "$repo" prepare \
    --remote origin --base main \
    --verify ': > .git/verifier-delay-started; trap "" TERM; sleep 10; : > .git/verifier-delay-completed' \
    > "$repo/verifier-timeout.out" 2>&1 || rc=$?
  topology_finished="$(date +%s)"
  topology_elapsed="$((topology_finished - topology_started))"
  [ "$rc" = 3 ] || fail "TERM-ignoring verifier timeout should park, got $rc"
  grep -Fq 'reason=verification-failed' "$repo/verifier-timeout.out" ||
    fail "TERM-ignoring verifier timeout was not identified"
  [ -f "$repo/.git/verifier-delay-started" ] &&
    [ ! -f "$repo/.git/verifier-delay-completed" ] ||
    fail "TERM-ignoring verifier was allowed to finish"
  [ "$topology_elapsed" -le 4 ] ||
    fail "TERM-ignoring verifier exceeded hard wall: ${topology_elapsed}s"

  rc=0
  OMS_DRAFT_SCAN_MAX_OBJECTS=1 run_draft_pr "$repo" prepare \
    --remote origin --base main --verify true > "$repo/object-cap.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "object-count cap should park, got $rc"
  grep -Fq 'reason=publish-payload-enumeration-failed' "$repo/object-cap.out" ||
    fail "object-count cap refusal was not identified"
  rc=0
  OMS_DRAFT_SCAN_MAX_OBJECT_BYTES=1 run_draft_pr "$repo" prepare \
    --remote origin --base main --verify true > "$repo/object-bytes.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "individual object byte cap should park, got $rc"
  grep -Fq 'reason=publish-payload-object-too-large' "$repo/object-bytes.out" ||
    fail "individual object byte cap refusal was not identified"
  rc=0
  OMS_DRAFT_SCAN_MAX_TOTAL_BYTES=1 run_draft_pr "$repo" prepare \
    --remote origin --base main --verify true \
    > "$repo/total-bytes.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "total object byte cap should park, got $rc"
  grep -Fq 'reason=publish-payload-total-too-large' "$repo/total-bytes.out" ||
    fail "total byte cap refusal was not identified"

  # Tree objects carry outbound path bytes too. They count toward both the
  # individual and aggregate object limits rather than bypassing blob-only
  # accounting.
  repo="$TMP/tree-resource-bounds"
  bare="$TMP/tree-resource-bounds.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  mkdir -p "$repo/tree-heavy"
  local tree_i=0
  while [ "$tree_i" -lt 120 ]; do
    printf 'tree entry %s\n' "$tree_i" > "$repo/tree-heavy/entry-$tree_i.txt"
    tree_i="$((tree_i + 1))"
  done
  "$REAL_GIT" -C "$repo" add tree-heavy
  "$REAL_GIT" -C "$repo" commit -qm 'test: large tree fixture'
  local subtree_oid subtree_size
  subtree_oid="$($REAL_GIT -C "$repo" rev-parse HEAD:tree-heavy)"
  subtree_size="$($REAL_GIT -C "$repo" cat-file -s "$subtree_oid")"
  [ "$subtree_size" -gt 1000 ] || fail "large-tree fixture is unexpectedly small"
  [ "$($REAL_GIT -C "$repo" cat-file -s HEAD)" -lt 1000 ] ||
    fail "large-tree fixture commit masks the tree-specific cap"
  rc=0
  OMS_DRAFT_SCAN_MAX_OBJECT_BYTES=1000 run_draft_pr "$repo" prepare \
    --remote origin --base main --verify true > "$repo/tree-object-cap.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "tree object byte cap should park, got $rc"
  grep -Fq 'reason=publish-payload-object-too-large' "$repo/tree-object-cap.out" ||
    fail "tree object byte cap refusal was not identified"

  local tree_ids="$TMP/tree-object-ids"
  local tree_oid tree_type tree_size non_tree_total=0
  "$REAL_GIT" -C "$repo" rev-list --objects --no-object-names main..HEAD > "$tree_ids"
  while IFS= read -r tree_oid || [ -n "$tree_oid" ]; do
    tree_type="$($REAL_GIT -C "$repo" cat-file -t "$tree_oid")"
    tree_size="$($REAL_GIT -C "$repo" cat-file -s "$tree_oid")"
    [ "$tree_type" = tree ] || non_tree_total="$((non_tree_total + tree_size))"
  done < "$tree_ids"
  [ "$non_tree_total" -gt 0 ] || fail "tree aggregate fixture has no non-tree bytes"
  rc=0
  OMS_DRAFT_SCAN_MAX_TOTAL_BYTES="$non_tree_total" run_draft_pr "$repo" prepare \
    --remote origin --base main --verify true > "$repo/tree-total-cap.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "tree aggregate byte cap should park, got $rc"
  grep -Fq 'reason=publish-payload-total-too-large' "$repo/tree-total-cap.out" ||
    fail "tree aggregate byte cap refusal was not identified"

  repo="$TMP/payload-resource-bounds"
  bare="$TMP/payload-resource-bounds.git"
  local scan_tmp="$TMP/payload-scan-tmp"
  local scan_started scan_finished scan_elapsed
  mkdir -p "$scan_tmp"
  "$REAL_GIT" -C "$repo" config oms.test.scanDelay true
  scan_started="$(date +%s)"
  rc=0
  TMPDIR="$scan_tmp" OMS_T_SCAN_DELAY=10 \
    OMS_MA_TIMEOUT_HAS_KILL_AFTER=0 OMS_PEER_KILL_AFTER=30 \
    OMS_DRAFT_SCAN_TIMEOUT_S=3 run_draft_pr "$repo" prepare \
    --remote origin --base main --verify true > "$repo/scan-timeout.out" 2>&1 || rc=$?
  scan_finished="$(date +%s)"
  scan_elapsed="$((scan_finished - scan_started))"
  [ "$rc" = 3 ] || fail "payload scan timeout should park, got $rc"
  grep -Fq 'reason=publish-payload-' "$repo/scan-timeout.out" ||
    fail "payload scan timeout did not report its bounded stage"
  [ -f "$repo/.git/scan-delay-started" ] ||
    fail "payload timeout fixture never reached the TERM-ignoring scanner"
  [ ! -f "$repo/.git/scan-delay-completed" ] ||
    fail "TERM-ignoring payload scanner was allowed to finish"
  [ "$scan_elapsed" -le 6 ] ||
    fail "TERM-ignoring payload scan exceeded hard wall: ${scan_elapsed}s"
  [ -z "$(find "$scan_tmp" -type f -print -quit)" ] ||
    fail "payload scan timeout left temporary payload files"
  "$REAL_GIT" -C "$repo" config --unset oms.test.scanDelay
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "resource-limit failures reached the remote"
  fi

  # Grafts can hide a real parent from traversal even with replacement refs
  # disabled. Refuse deprecated graft metadata instead of inspecting one graph
  # and publishing another.
  repo="$TMP/sensitive-graft"
  bare="$TMP/sensitive-graft.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  local graft_safe_parent graft_secret_commit graft_head graft_file graft_subject
  graft_safe_parent="$("$REAL_GIT" -C "$repo" rev-parse HEAD)"
  graft_subject="$(printf '%s%s' 'ghp_' '12345678901234567890')"
  "$REAL_GIT" -C "$repo" commit -q --allow-empty -m "$graft_subject"
  graft_secret_commit="$("$REAL_GIT" -C "$repo" rev-parse HEAD)"
  "$REAL_GIT" -C "$repo" push -q origin \
    "$graft_secret_commit:refs/heads/transient-sensitive"
  "$REAL_GIT" --git-dir "$bare" update-ref -d refs/heads/transient-sensitive
  "$REAL_GIT" -C "$repo" commit -q --allow-empty -m 'test: safe graft head'
  graft_head="$("$REAL_GIT" -C "$repo" rev-parse HEAD)"
  graft_file="$("$REAL_GIT" -C "$repo" rev-parse --git-path info/grafts)"
  case "$graft_file" in
    /*|[A-Za-z]:/*) ;;
    *) graft_file="$repo/$graft_file" ;;
  esac
  mkdir -p "$(dirname "$graft_file")"
  printf '%s %s\n' "$graft_head" "$graft_safe_parent" > "$graft_file"
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/graft.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "repository graft should park, got $rc"
  grep -Fq 'reason=git-grafts-present' "$repo/graft.out" ||
    fail "repository graft refusal was not identified"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "grafted history reached the remote"
  fi

  # An existing selective .oms/.gitignore is not proof that executable intent
  # state is excluded. Refuse before creating the intent file.
  repo="$TMP/unignored-intent"
  bare="$TMP/unignored-intent.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  sed -i.bak '\|^/.oms/$|d' "$repo/.gitignore"
  rm -f "$repo/.gitignore.bak"
  mkdir -p "$repo/.oms"
  printf 'some-other-file\n' > "$repo/.oms/.gitignore"
  "$REAL_GIT" -C "$repo" add .gitignore
  "$REAL_GIT" -C "$repo" add -f .oms/.gitignore
  "$REAL_GIT" -C "$repo" commit -qm 'test: track selective oms ignore'
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/unignored.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "unignored publication intent should be refused, got $rc"
  if find "$repo/.oms/publish" -type f -name '*.json' -print -quit 2>/dev/null | grep -q .; then
    fail "unignored publication intent was persisted"
  fi

  # Re-running prepare for the same deterministic HEAD must not execute a
  # verifier against an existing intent. The existing bytes remain publishable
  # only through the dedicated replay path.
  repo="$TMP/repeated prepare"
  bare="$TMP/repeated prepare.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  cat > "$repo/.git/hooks/tamper-on-prepare-replay" <<'EOF'
#!/usr/bin/env bash
if [ -f .git/tamper-on-prepare-replay-now ]; then
  python3 - <<'PY'
import glob, hashlib, json
path = glob.glob(".oms/publish/*.json")[0]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["body"] = "prepare replay tamper"
row["body_sha256"] = hashlib.sha256(row["body"].encode("utf-8")).hexdigest()
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
fi
EOF
  chmod +x "$repo/.git/hooks/tamper-on-prepare-replay"
  run_draft_pr "$repo" prepare --remote origin --base main \
    --verify .git/hooks/tamper-on-prepare-replay > "$repo/repeat-first.out" ||
    fail "repeat-prepare fixture initial intent failed"
  intent="$(sed -n 's/^intent: //p' "$repo/repeat-first.out" | tail -n 1)"
  local repeat_before resume_cmd resume_bin resume_argv repeat_saved
  repeat_before="$(sha256_file "$intent")"
  : > "$repo/.git/tamper-on-prepare-replay-now"
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main \
    --verify .git/hooks/tamper-on-prepare-replay > "$repo/repeat-second.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "second prepare of an existing intent should park, got $rc"
  grep -Fq 'reason=intent-already-prepared' "$repo/repeat-second.out" ||
    fail "second prepare did not identify the existing exact intent"
  [ "$(grep -c '^draft-pr: parent-agent next:' "$repo/repeat-second.out")" = 1 ] ||
    fail "second prepare should print exactly one resume command"
  resume_cmd="$(sed -n 's/^draft-pr: parent-agent next: //p' "$repo/repeat-second.out")"
  [ -n "$resume_cmd" ] || fail "second prepare did not print its exact publish command"
  case "$resume_cmd" in
    'oms draft-pr '*) ;;
    *) fail "second prepare next line is prose, not an oms draft-pr command" ;;
  esac

  # The recovery line is a command for an operator to paste, not explanatory
  # prose. Execute it against a capture-only oms shim so shell quoting must
  # preserve the repository and intent as one argv each even though both paths
  # contain spaces.
  resume_bin="$repo/.git/resume-bin"
  resume_argv="$repo/.git/resume-argv"
  mkdir -p "$resume_bin"
  cat > "$resume_bin/oms" <<'EOF'
#!/usr/bin/env bash
printf '%s\0' "$@" > "$OMS_T_RESUME_ARGV"
EOF
  chmod +x "$resume_bin/oms"
  PATH="$resume_bin:$PATH" OMS_T_RESUME_ARGV="$resume_argv" \
    bash -c "$resume_cmd" || fail "printed publish command was not shell executable"
  python3 - "$resume_argv" "$repo" "$intent" <<'PY' || fail "printed publish command did not round-trip its exact argv"
import sys

with open(sys.argv[1], "rb") as handle:
    fields = handle.read().split(b"\0")
assert fields[-1] == b"", fields
argv = [field.decode("utf-8") for field in fields[:-1]]
assert argv == [
    "draft-pr", "--repo", sys.argv[2], "--intent", sys.argv[3], "publish"
], argv
PY
  [ "$repeat_before" = "$(sha256_file "$intent")" ] ||
    fail "second prepare allowed its verifier to mutate the existing intent"

  # A deterministic slot with terminal poison, or a symlink in place of the
  # reviewed regular file, must never advertise that slot as publishable.
  : > "$intent.blocked"
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main \
    --verify .git/hooks/tamper-on-prepare-replay > "$repo/repeat-blocked.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "blocked existing intent should park, got $rc"
  if grep '^draft-pr: parent-agent next:' "$repo/repeat-blocked.out" | grep -Fq 'oms draft-pr'; then
    fail "blocked intent exposed a publish resume command"
  fi
  [ "$repeat_before" = "$(sha256_file "$intent")" ] ||
    fail "blocked prepare ran its verifier against the existing intent"
  rm -f "$intent.blocked"

  repeat_saved="$repo/.git/repeat-intent.saved"
  mv "$intent" "$repeat_saved"
  ln -s "$repeat_saved" "$intent"
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main \
    --verify .git/hooks/tamper-on-prepare-replay > "$repo/repeat-symlink.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "symlink existing intent should park, got $rc"
  if grep '^draft-pr: parent-agent next:' "$repo/repeat-symlink.out" | grep -Fq 'oms draft-pr'; then
    fail "symlink intent exposed a publish resume command"
  fi
  [ "$repeat_before" = "$(sha256_file "$repeat_saved")" ] ||
    fail "symlink prepare ran its verifier against the intent target"
  rm -f "$intent"
  mv "$repeat_saved" "$intent"
  rm -f "$repo/.git/tamper-on-prepare-replay-now"
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/repeat-publish.out" ||
    fail "untouched existing intent did not remain publishable"

  # A verifier cannot add a second push destination in ignored Git config.
  repo="$TMP/verifier-remote"
  bare="$TMP/verifier-remote.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main \
    --verify "$REAL_GIT -C '$repo' config oms.test.extraPush true" \
    > "$repo/verifier-remote.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "verifier remote mutation should fail closed, got $rc"
  [ ! -d "$repo/.oms/publish" ] ||
    fail "remote mutation still produced a publication intent"

  # PROJECT.md is commonly clone-local/ignored. A verifier cannot replace the
  # caller-frozen contract while leaving Git HEAD and tree clean.
  repo="$TMP/spec-mutation"
  bare="$TMP/spec-mutation.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main \
    --expected-head "$($REAL_GIT -C "$repo" rev-parse HEAD)" \
    --expected-tree "$($REAL_GIT -C "$repo" rev-parse 'HEAD^{tree}')" \
    --expected-spec-sha256 "$(sha256_file "$repo/PROJECT.md")" \
    --verify "printf changed > '$repo/PROJECT.md'" \
    > "$repo/spec-mutation.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "ignored PROJECT.md mutation should park, got $rc"
  [ ! -d "$repo/.oms/publish" ] || fail "spec mutation still produced an intent"

  # gh identity is part of the durable intent, not merely write permission.
  repo="$TMP/viewer-change"
  bare="$TMP/viewer-change.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  OMS_T_GH_VIEWER=alice run_draft_pr "$repo" prepare --remote origin --base main \
    --verify true > "$repo/viewer-prepare.out" || fail "viewer fixture prepare failed"
  intent="$(sed -n 's/^intent: //p' "$repo/viewer-prepare.out" | tail -n 1)"
  rc=0
  OMS_T_GH_VIEWER=bob run_draft_pr "$repo" publish --intent "$intent" \
    > "$repo/viewer-publish.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "changed GitHub viewer should park, got $rc"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "viewer mismatch still created the source branch"
  fi

  # The repository is worker-writable, while publication uses the parent's
  # GitHub authority. Transport-affecting local config must be rejected before
  # any remote operation: core.sshCommand and credential helpers are executable
  # command surfaces even when hooks and signing are disabled at push time.
  repo="$TMP/hostile-transport-config"
  bare="$TMP/hostile-transport-config.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  cat > "$repo/.git/hostile-ssh" <<'EOF'
#!/usr/bin/env bash
: > .git/hostile-transport-fired
exit 91
EOF
  chmod +x "$repo/.git/hostile-ssh"
  "$REAL_GIT" -C "$repo" config --local core.sshCommand .git/hostile-ssh
  "$REAL_GIT" -C "$repo" config --local credential.helper '!printf fired > .git/hostile-credential-fired'
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/hostile-transport.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "hostile local transport config should be rejected, got $rc"
  grep -Fq 'unsafe local Git transport config' "$repo/hostile-transport.out" ||
    fail "hostile local transport refusal was not explained"
  [ ! -e "$repo/.git/hostile-transport-fired" ] ||
    fail "repository core.sshCommand executed with publisher authority"
  [ ! -e "$repo/.git/hostile-credential-fired" ] ||
    fail "repository credential helper executed with publisher authority"

  # With extensions.worktreeConfig enabled, transport commands can also live
  # in config.worktree. It is just as worker-writable and must not bypass the
  # repository-local transport scan.
  repo="$TMP/hostile-worktree-transport-config"
  bare="$TMP/hostile-worktree-transport-config.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  "$REAL_GIT" -C "$repo" config --local extensions.worktreeConfig true
  "$REAL_GIT" -C "$repo" config --worktree core.sshCommand .git/hostile-worktree-ssh
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/hostile-worktree-transport.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "hostile worktree transport config should be rejected, got $rc"
  grep -Fq 'unsafe local Git transport config' "$repo/hostile-worktree-transport.out" ||
    fail "hostile worktree transport refusal was not explained"

  repo="$TMP/hostile-fsmonitor-config"
  bare="$TMP/hostile-fsmonitor-config.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  cat > "$repo/.git/hostile-fsmonitor" <<EOF
#!/usr/bin/env bash
: > "$repo/.git/hostile-fsmonitor-fired"
exit 0
EOF
  chmod +x "$repo/.git/hostile-fsmonitor"
  "$REAL_GIT" -C "$repo" config --local core.fsmonitor "$repo/.git/hostile-fsmonitor"
  rc=0
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/hostile-fsmonitor.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "hostile fsmonitor config should fail before status, got $rc"
  [ ! -e "$repo/.git/hostile-fsmonitor-fired" ] ||
    fail "repository fsmonitor executed with publisher authority"

  repo="$TMP/hostile-command-config"
  bare="$TMP/hostile-command-config.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  rc=0
  GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.sshCommand \
    GIT_CONFIG_VALUE_0="$repo/.git/hostile-command" \
    run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/hostile-command.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "hostile command-scope Git config should be rejected, got $rc"
  grep -Fq 'unsafe command-scope Git config' "$repo/hostile-command.out" ||
    fail "command-scope Git config refusal was not explained"

  # UI/task cancellation targets the publisher PID. It must synchronously
  # terminate the whole push group, including a descendant that ignores TERM,
  # before returning 143; no delayed remote effect may survive cancellation.
  repo="$TMP/push-cancel"
  bare="$TMP/push-cancel.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/cancel-prepare.out" || fail "push-cancel fixture prepare failed"
  intent="$(sed -n 's/^intent: //p' "$repo/cancel-prepare.out" | tail -n 1)"
  "$REAL_GIT" -C "$repo" config --local oms.test.pushDelay true
  (
    export OMS_GIT_BIN="$TMP/bin/git" OMS_GH_BIN="$TMP/bin/gh"
    export OMS_T_GIT_LOG="$repo/git.log" OMS_T_GH_LOG="$repo/gh.log"
    export OMS_T_BARE="$bare" OMS_T_PR_MARKER="$repo/pr-created"
    export OMS_T_BRANCH="codex/draft-fixture"
    OMS_T_BASE="$($REAL_GIT -C "$repo" rev-parse main)"
    OMS_T_HEAD="$($REAL_GIT -C "$repo" rev-parse HEAD)"
    export OMS_T_BASE OMS_T_HEAD
    export OMS_DRAFT_CANCEL_GRACE_S=0.2 OMS_T_PUSH_DELAY=3
    exec "$ROOT/scripts/draft-pr.sh" --repo "$repo" publish --intent "$intent"
  ) > "$repo/cancel-publish.out" 2>&1 &
  local cancel_pid=$! cancel_seen=0 cancel_try=0
  while [ "$cancel_try" -lt 100 ]; do
    if [ -e "$repo/.git/push-delay-started" ]; then
      cancel_seen=1
      break
    fi
    sleep 0.05
    cancel_try=$((cancel_try + 1))
  done
  [ "$cancel_seen" = 1 ] || fail "push-cancel fixture never reached the push"
  kill -TERM "$cancel_pid"
  rc=0
  wait "$cancel_pid" || rc=$?
  [ "$rc" = 143 ] || fail "targeted publisher TERM should exit 143, got $rc"
  # The fixture's delayed push lands at OMS_T_PUSH_DELAY=3s; a 1s window
  # closed before the escaped write could ever land, making this assert
  # vacuous on every machine. Outwait the deadline with margin.
  sleep 4
  [ ! -e "$repo/.git/push-delay-completed" ] ||
    fail "cancelled push descendant survived and completed"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "cancelled publisher still created the source branch"
  fi

  # A worker-planted local pre-push hook must never execute with the
  # publisher's authority, so the create-only push suppresses hooks and signing.
  repo="$TMP/hook-suppress"
  bare="$TMP/hook-suppress.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/hook-prepare.out" || fail "hook-suppress fixture prepare failed"
  intent="$(sed -n 's/^intent: //p' "$repo/hook-prepare.out" | tail -n 1)"
  cat > "$repo/.git/hooks/pre-push" <<'EOF'
#!/usr/bin/env bash
: > .git/pre-push-fired
echo 'fixture pre-push gate failed' >&2
exit 7
EOF
  chmod +x "$repo/.git/hooks/pre-push"
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/hook-publish.out" 2>&1 || rc=$?
  [ "$rc" = 0 ] || fail "publish must suppress local pre-push hooks, got $rc"
  if [ -e "$repo/.git/pre-push-fired" ]; then
    fail "the planted pre-push hook executed during publish"
  fi
  "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture ||
    fail "hook-suppressed publication did not create the source branch"

  # A remote-side rejection is visible and is not mislabeled as a remote
  # race; no remote ref is left behind.
  repo="$TMP/hook-reject"
  bare="$TMP/hook-reject.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/hook-prepare.out" || fail "hook fixture prepare failed"
  intent="$(sed -n 's/^intent: //p' "$repo/hook-prepare.out" | tail -n 1)"
  cat > "$bare/hooks/pre-receive" <<'EOF'
#!/usr/bin/env bash
echo 'fixture pre-receive gate failed' >&2
exit 7
EOF
  chmod +x "$bare/hooks/pre-receive"
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/hook-publish.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "remote rejection should park, got $rc"
  grep -Fq 'fixture pre-receive gate failed' "$repo/hook-publish.out" ||
    fail "remote rejection diagnostics were hidden"
  grep -Fq 'reason=create-only-push-rejected' "$repo/hook-publish.out" ||
    fail "remote rejection was mislabeled"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "rejected push still created the source branch"
  fi
  local push_attempts
  push_attempts="$(grep -c ' push ' "$repo/git.log" || true)"
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/hook-replay.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "spent uncertain intent should park on replay, got $rc"
  [ "$push_attempts" = "$(grep -c ' push ' "$repo/git.log" || true)" ] ||
    fail "spent uncertain intent retried the remote push"
  grep -Fq 'reason=published-branch-missing' "$repo/hook-replay.out" ||
    fail "spent intent replay did not explain the terminal branch state"

  # A concurrently created PR with the right refs but different reviewed
  # content is not adopted as the exact publication.
  repo="$TMP/pr-content"
  bare="$TMP/pr-content.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/content-prepare.out" || fail "content fixture prepare failed"
  intent="$(sed -n 's/^intent: //p' "$repo/content-prepare.out" | tail -n 1)"
  "$REAL_GIT" -C "$repo" push -q origin HEAD:refs/heads/codex/draft-fixture
  printf 'different title' > "$repo/pr-created.title"
  printf 'different body' > "$repo/pr-created.body"
  : > "$repo/pr-created"
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/content-publish.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "mismatched existing PR content should park, got $rc"
  [ "$(python3 "$ROOT/scripts/lib/draft-pr-intent.py" field \
      --repo "$repo" --path "$intent" --name phase)" = pushed ] ||
    fail "content conflict did not preserve the recoverable pushed phase"

  # The push source is the frozen object id, not mutable HEAD. If a concurrent
  # clean commit advances the local branch immediately before push, only the
  # reviewed commit may reach the remote. The pre-effect `pushing` phase then
  # prevents replay from resurrecting that branch after explicit deletion.
  repo="$TMP/frozen-push"
  bare="$TMP/frozen-push.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/frozen-prepare.out" || fail "frozen-push prepare failed"
  intent="$(sed -n 's/^intent: //p' "$repo/frozen-prepare.out" | tail -n 1)"
  local frozen_head moved_head remote_head
  frozen_head="$(python3 "$ROOT/scripts/lib/draft-pr-intent.py" field \
    --repo "$repo" --path "$intent" --name head_sha)"
  "$REAL_GIT" -C "$repo" config oms.test.commitBeforePush true
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/frozen-publish.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "post-snapshot local commit should park, got $rc"
  remote_head="$("$REAL_GIT" --git-dir "$bare" rev-parse refs/heads/codex/draft-fixture)"
  [ "$remote_head" = "$frozen_head" ] || fail "mutable HEAD escaped the frozen push refspec"
  [ "$(python3 "$ROOT/scripts/lib/draft-pr-intent.py" field \
      --repo "$repo" --path "$intent" --name phase)" = pushing ] ||
    fail "uncertain push did not retain its pre-effect phase"
  moved_head="$("$REAL_GIT" -C "$repo" rev-parse HEAD)"
  "$REAL_GIT" -C "$repo" update-ref refs/heads/codex/draft-fixture "$frozen_head" "$moved_head"
  "$REAL_GIT" --git-dir "$bare" update-ref -d refs/heads/codex/draft-fixture
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/frozen-replay.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "deleted uncertain branch should park, got $rc"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "uncertain intent recreated a deliberately deleted branch"
  fi

  # The publish-time verifier is same-UID code. If it rewrites valid immutable
  # intent fields, their frozen digest must stop before the branch push; later
  # body extraction also carries the same digest CAS.
  repo="$TMP/verifier-intent-tamper"
  bare="$TMP/verifier-intent-tamper.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  cat > "$repo/.git/hooks/tamper-publish-intent" <<'EOF'
#!/usr/bin/env bash
python3 - <<'PY'
import glob, hashlib, json
paths = glob.glob(".oms/publish/*.json")
if paths:
    path = paths[0]
    with open(path, encoding="utf-8") as handle:
        row = json.load(handle)
    row["body"] = "unreviewed body"
    row["body_sha256"] = hashlib.sha256(row["body"].encode("utf-8")).hexdigest()
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(row, handle)
PY
EOF
  chmod +x "$repo/.git/hooks/tamper-publish-intent"
  run_draft_pr "$repo" prepare --remote origin --base main \
    --verify .git/hooks/tamper-publish-intent > "$repo/tamper-prepare.out" ||
    fail "intent-tamper fixture prepare failed"
  intent="$(sed -n 's/^intent: //p' "$repo/tamper-prepare.out" | tail -n 1)"
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/tamper.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "verifier intent tamper should park, got $rc"
  grep -Fq 'reason=intent-changed-by-verifier' "$repo/tamper.out" ||
    fail "verifier intent tamper was not identified"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "verifier-tampered intent reached the remote"
  fi
  local tamper_pushes tamper_gh_calls
  tamper_pushes="$(grep -c ' push ' "$repo/git.log" || true)"
  tamper_gh_calls="$(wc -l < "$repo/gh.log" | tr -d ' ')"
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/tamper-replay.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "terminally blocked tamper replay should park, got $rc"
  grep -Fq 'reason=intent-terminally-blocked' "$repo/tamper-replay.out" ||
    fail "tampered intent replay did not honor its terminal marker"
  [ "$tamper_pushes" = "$(grep -c ' push ' "$repo/git.log" || true)" ] ||
    fail "terminally blocked tamper replay attempted a push"
  [ "$tamper_gh_calls" = "$(wc -l < "$repo/gh.log" | tr -d ' ')" ] ||
    fail "terminally blocked tamper replay called GitHub"

  # Replacing the intent with an out-of-root symlink must still poison the
  # original lexical slot. Restoring the reviewed bytes cannot revive it.
  repo="$TMP/verifier-intent-symlink"
  bare="$TMP/verifier-intent-symlink.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  cat > "$repo/.git/hooks/symlink-publish-intent" <<'EOF'
#!/usr/bin/env bash
if [ -f .git/symlink-publish-now ]; then
  for path in .oms/publish/*.json; do
    [ -f "$path" ] || continue
    mv "$path" "$path.original"
    printf 'verifier replacement\n' > .git/verifier-intent-target
    ln -s "$PWD/.git/verifier-intent-target" "$path"
    break
  done
fi
EOF
  chmod +x "$repo/.git/hooks/symlink-publish-intent"
  run_draft_pr "$repo" prepare --remote origin --base main \
    --verify .git/hooks/symlink-publish-intent > "$repo/symlink-prepare.out" ||
    fail "intent-symlink fixture prepare failed"
  intent="$(sed -n 's/^intent: //p' "$repo/symlink-prepare.out" | tail -n 1)"
  : > "$repo/.git/symlink-publish-now"
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/symlink.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "verifier intent symlink should park, got $rc"
  grep -Fq 'reason=intent-changed-by-verifier' "$repo/symlink.out" ||
    fail "verifier intent symlink was not identified"
  [ -f "$intent.blocked" ] && [ ! -L "$intent.blocked" ] ||
    fail "verifier intent symlink did not create a lexical terminal marker"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "verifier-symlinked intent reached the remote"
  fi
  rm -f "$intent" "$repo/.git/symlink-publish-now"
  mv "$intent.original" "$intent"
  local symlink_pushes symlink_gh_calls
  symlink_pushes="$(grep -c ' push ' "$repo/git.log" || true)"
  symlink_gh_calls="$(wc -l < "$repo/gh.log" | tr -d ' ')"
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/symlink-replay.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "terminally blocked symlink replay should park, got $rc"
  grep -Fq 'reason=intent-terminally-blocked' "$repo/symlink-replay.out" ||
    fail "restored symlink intent replay did not honor terminal poison"
  [ "$symlink_pushes" = "$(grep -c ' push ' "$repo/git.log" || true)" ] ||
    fail "terminally blocked symlink replay attempted a push"
  [ "$symlink_gh_calls" = "$(wc -l < "$repo/gh.log" | tr -d ' ')" ] ||
    fail "terminally blocked symlink replay called GitHub"

  # The parent publication directory can be renamed while preserving a live
  # symlink view. A pre-verifier directory handle must carry terminal poison
  # into the original directory that a later operator could restore.
  repo="$TMP/verifier-publish-dir-symlink"
  bare="$TMP/verifier-publish-dir-symlink.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  cat > "$repo/.git/hooks/symlink-publish-directory" <<'EOF'
#!/usr/bin/env bash
if [ -f .git/symlink-publish-directory-now ]; then
  mv .oms/publish .git/original-publish-directory
  ln -s "$PWD/.git/original-publish-directory" .oms/publish
fi
EOF
  chmod +x "$repo/.git/hooks/symlink-publish-directory"
  run_draft_pr "$repo" prepare --remote origin --base main \
    --verify .git/hooks/symlink-publish-directory > "$repo/dir-symlink-prepare.out" ||
    fail "publish-directory-symlink fixture prepare failed"
  intent="$(sed -n 's/^intent: //p' "$repo/dir-symlink-prepare.out" | tail -n 1)"
  : > "$repo/.git/symlink-publish-directory-now"
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" \
    > "$repo/dir-symlink.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "verifier publication directory symlink should park, got $rc"
  grep -Fq 'reason=intent-changed-by-verifier' "$repo/dir-symlink.out" ||
    fail "verifier publication directory symlink was not identified"
  [ -f "$repo/.git/original-publish-directory/${intent##*/}.blocked" ] ||
    fail "renamed publication directory did not receive terminal poison"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "publication-directory-symlinked intent reached the remote"
  fi
  rm -f "$repo/.oms/publish" "$repo/.git/symlink-publish-directory-now"
  mv "$repo/.git/original-publish-directory" "$repo/.oms/publish"
  local dir_symlink_pushes dir_symlink_gh_calls
  dir_symlink_pushes="$(grep -c ' push ' "$repo/git.log" || true)"
  dir_symlink_gh_calls="$(wc -l < "$repo/gh.log" | tr -d ' ')"
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" \
    > "$repo/dir-symlink-replay.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "restored publication directory replay should park, got $rc"
  grep -Fq 'reason=intent-terminally-blocked' "$repo/dir-symlink-replay.out" ||
    fail "restored publication directory replay ignored terminal poison"
  [ "$dir_symlink_pushes" = "$(grep -c ' push ' "$repo/git.log" || true)" ] ||
    fail "restored publication directory replay attempted a push"
  [ "$dir_symlink_gh_calls" = "$(wc -l < "$repo/gh.log" | tr -d ' ')" ] ||
    fail "restored publication directory replay called GitHub"

  # A verifier that mutates durable bytes and then exits nonzero is still
  # checked and poisoned before its ordinary verification failure is handled.
  repo="$TMP/verifier-tamper-fail"
  bare="$TMP/verifier-tamper-fail.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  cat > "$repo/.git/hooks/tamper-intent-and-fail" <<'EOF'
#!/usr/bin/env bash
if [ -f .git/tamper-intent-and-fail-now ]; then
  python3 - <<'PY'
import glob, hashlib, json
path = glob.glob(".oms/publish/*.json")[0]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["body"] = "failed verifier tamper"
row["body_sha256"] = hashlib.sha256(row["body"].encode("utf-8")).hexdigest()
row["verify"] = "true"
row["verify_sha256"] = hashlib.sha256(row["verify"].encode("utf-8")).hexdigest()
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
  exit 1
fi
EOF
  chmod +x "$repo/.git/hooks/tamper-intent-and-fail"
  run_draft_pr "$repo" prepare --remote origin --base main \
    --verify .git/hooks/tamper-intent-and-fail > "$repo/tamper-fail-prepare.out" ||
    fail "failed-verifier tamper fixture prepare failed"
  intent="$(sed -n 's/^intent: //p' "$repo/tamper-fail-prepare.out" | tail -n 1)"
  : > "$repo/.git/tamper-intent-and-fail-now"
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/tamper-fail.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "mutating failed verifier should park, got $rc"
  grep -Fq 'reason=intent-changed-by-verifier' "$repo/tamper-fail.out" ||
    fail "mutating failed verifier bypassed integrity-first handling"
  local tamper_fail_pushes tamper_fail_gh_calls
  tamper_fail_pushes="$(grep -c ' push ' "$repo/git.log" || true)"
  tamper_fail_gh_calls="$(wc -l < "$repo/gh.log" | tr -d ' ')"
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/tamper-fail-replay.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "failed-verifier tamper replay should park, got $rc"
  grep -Fq 'reason=intent-terminally-blocked' "$repo/tamper-fail-replay.out" ||
    fail "failed-verifier tamper replay did not honor terminal poison"
  [ "$tamper_fail_pushes" = "$(grep -c ' push ' "$repo/git.log" || true)" ] ||
    fail "failed-verifier tamper replay attempted a push"
  [ "$tamper_fail_gh_calls" = "$(wc -l < "$repo/gh.log" | tr -d ' ')" ] ||
    fail "failed-verifier tamper replay called GitHub"

  # Mutable recovery state is also frozen across verifier execution. Rewinding
  # a complete intent to prepared cannot make a later branch deletion
  # recreatable, even though the publication payload itself is unchanged.
  repo="$TMP/verifier-phase-rewind"
  bare="$TMP/verifier-phase-rewind.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  cat > "$repo/.git/hooks/rewind-publish-phase" <<'EOF'
#!/usr/bin/env bash
if [ -f .git/rewind-publish-now ]; then
  python3 - <<'PY'
import glob, json
path = glob.glob(".oms/publish/*.json")[0]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["phase"] = "prepared"
row["pr_url"] = ""
row["pr_number"] = None
row["push_attempted"] = False
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
fi
EOF
  chmod +x "$repo/.git/hooks/rewind-publish-phase"
  run_draft_pr "$repo" prepare --remote origin --base main \
    --verify .git/hooks/rewind-publish-phase > "$repo/rewind-prepare.out" ||
    fail "phase-rewind fixture prepare failed"
  intent="$(sed -n 's/^intent: //p' "$repo/rewind-prepare.out" | tail -n 1)"
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/rewind-first.out" ||
    fail "phase-rewind fixture initial publish failed"
  : > "$repo/.git/rewind-publish-now"
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/rewind.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "verifier phase rewind should park, got $rc"
  grep -Fq 'reason=intent-state-changed-by-verifier' "$repo/rewind.out" ||
    fail "valid phase rewind was not identified as a state change"
  local rewind_pushes rewind_gh_calls
  rewind_pushes="$(grep -c ' push ' "$repo/git.log" || true)"
  rewind_gh_calls="$(wc -l < "$repo/gh.log" | tr -d ' ')"
  "$REAL_GIT" --git-dir "$bare" update-ref -d refs/heads/codex/draft-fixture
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/rewind-deleted.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "terminally blocked phase rewind should park, got $rc"
  grep -Fq 'reason=intent-terminally-blocked' "$repo/rewind-deleted.out" ||
    fail "phase rewind replay did not honor its terminal marker"
  [ "$rewind_pushes" = "$(grep -c ' push ' "$repo/git.log" || true)" ] ||
    fail "terminally blocked phase rewind attempted a push"
  [ "$rewind_gh_calls" = "$(wc -l < "$repo/gh.log" | tr -d ' ')" ] ||
    fail "terminally blocked phase rewind called GitHub"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "rewound complete intent recreated a deleted branch"
  fi

  # Canonical intent validation is complete before any effect. A non-string
  # body cannot satisfy validation by hashing its Python string rendering.
  repo="$TMP/malformed-intent"
  bare="$TMP/malformed-intent.git"
  make_repo "$repo" "$bare"
  : > "$repo/git.log"; : > "$repo/gh.log"
  run_draft_pr "$repo" prepare --remote origin --base main --verify true \
    > "$repo/malformed-prepare.out" || fail "malformed fixture prepare failed"
  intent="$(sed -n 's/^intent: //p' "$repo/malformed-prepare.out" | tail -n 1)"
  python3 - "$intent" <<'PY'
import hashlib, json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["body"] = {"not": "text"}
row["body_sha256"] = hashlib.sha256(str(row["body"]).encode("utf-8")).hexdigest()
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
  rc=0
  run_draft_pr "$repo" publish --intent "$intent" > "$repo/malformed.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "malformed intent should fail before effects, got $rc"
  if "$REAL_GIT" --git-dir "$bare" show-ref --verify --quiet refs/heads/codex/draft-fixture; then
    fail "malformed intent reached the remote"
  fi
}

test_remote_parser
test_prepare_publish_recovery
test_remote_and_recovery_hardening

echo "draft-pr-smoke: ok"
