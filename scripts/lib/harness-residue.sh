# shellcheck shell=bash
# Crash-residue helpers for oh-my-setting harness state. Sourced, not executed.

# shellcheck source=file-lock.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/file-lock.sh"

OMS_HARNESS_RESIDUE_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OMS_HARNESS_RESIDUE_REMOVED=0
OMS_HARNESS_RESIDUE_WOULD_REMOVE=0
OMS_HARNESS_RESIDUE_KEPT=0

oms_harness_residue_reset() {
  OMS_HARNESS_RESIDUE_REMOVED=0
  OMS_HARNESS_RESIDUE_WOULD_REMOVE=0
  OMS_HARNESS_RESIDUE_KEPT=0
}

oms_harness_tmp_base() {
  printf '%s\n' "${TMPDIR:-/tmp}"
}

oms_harness_delegate_worktree_root() {
  if [ -n "${OMS_DELEGATE_WORKTREE_ROOT:-}" ]; then
    printf '%s\n' "$OMS_DELEGATE_WORKTREE_ROOT"
  else
    printf '%s\n' "${XDG_CACHE_HOME:-$HOME/.cache}/oh-my-setting/worktrees"
  fi
}

# Most harness scratch state is under TMPDIR. Delegated worktrees use a private
# user-local root so provider write permission can be scoped to one stable path.
# Keep both recovery surfaces explicit and de-duplicate the unusual case where
# an operator points them at the same directory.
oms_harness_temp_bases() {
  local tmp_base
  local delegate_root

  tmp_base="$(oms_harness_tmp_base)"
  delegate_root="$(oms_harness_delegate_worktree_root)"
  printf '%s\n' "$tmp_base"
  case "$delegate_root" in
    /*) [ "$delegate_root" = "$tmp_base" ] || printf '%s\n' "$delegate_root" ;;
  esac
}

# The marker's pid is the process whose life bounds the directory: the creating
# shell by default, or an explicit OWNER_PID for a command that outlives it.
oms_harness_mark_tmpdir() {  # DIR REPO WORKTREE [OWNER_PID]
  local dir="$1"
  local repo="$2"
  local worktree="$3"
  local owner_pid="${4:-$$}"

  [ -d "$dir" ] || return 0
  {
    printf 'kind=oh-my-setting-temp\n'
    printf 'pid=%s\n' "$owner_pid"
    printf 'repo=%s\n' "$repo"
    printf 'worktree=%s\n' "$worktree"
    printf 'temporary=1\n'
  } > "$dir/.oh-my-setting-tmp"
}

oms_harness_read_marker_value() {
  local file="$1"
  local key="$2"

  sed -n "s/^$key=//p" "$file" 2>/dev/null | sed -n '1p'
}

oms_harness_physical_dir() {
  local dir="$1"
  local physical=""

  [ -d "$dir" ] && [ ! -L "$dir" ] || return 1
  physical="$(cd "$dir" && pwd -P)" || return 1
  printf '%s\n' "${physical//$'\r'/}"
}

oms_harness_git_path_physical() {
  local repo="$1"
  local query="$2"
  local raw=""
  local candidate=""
  local physical=""

  raw="$(git -C "$repo" rev-parse "$query" 2>/dev/null || true)"
  raw="${raw//$'\r'/}"
  [ -n "$raw" ] || return 1
  case "$raw" in
    /*|[A-Za-z]:/*) candidate="$raw" ;;
    *) candidate="$repo/$raw" ;;
  esac
  [ -d "$candidate" ] || return 1
  physical="$(cd "$candidate" && pwd -P)" || return 1
  printf '%s\n' "${physical//$'\r'/}"
}

OMS_HARNESS_SAFE_RESIDUE_REPO=""
OMS_HARNESS_SAFE_RESIDUE_WORKTREE=""

# Resolve a marker only as evidence. The removal target itself is derived from
# the managed residue shape and can therefore never be an arbitrary marker
# path. Production creates delegate, admission, scratch and land-gate worktrees as
# <residue>/wt and Antigravity read worktrees as <residue>/tree.
oms_harness_safe_residue_worktree() {
  local base="$1"
  local dir="$2"
  local marker_repo="$3"
  local marker_worktree="$4"
  local base_physical=""
  local dir_physical=""
  local relative=""
  local child=""
  local expected=""
  local expected_physical=""
  local marker_worktree_physical=""
  local repo_physical=""
  local repo_common=""
  local worktree_common=""
  local worktree_git_dir=""
  local registered=""
  local registered_physical=""
  local found=0

  OMS_HARNESS_SAFE_RESIDUE_REPO=""
  OMS_HARNESS_SAFE_RESIDUE_WORKTREE=""
  [ -n "$marker_repo" ] && [ -n "$marker_worktree" ] || return 1
  base_physical="$(oms_harness_physical_dir "$base")" || return 1
  dir_physical="$(oms_harness_physical_dir "$dir")" || return 1
  case "$base_physical" in
    /) relative="${dir_physical#/}" ;;
    *)
      case "$dir_physical" in
        "$base_physical"/*) relative="${dir_physical#"$base_physical"/}" ;;
        *) return 1 ;;
      esac
      ;;
  esac
  case "$relative" in
    */*) return 1 ;;
    oh-my-setting-delegate.*|oh-my-setting-admit.*|oh-my-setting-scratch.*|oh-my-setting-land.*) child=wt ;;
    oh-my-setting-agy-read.*) child=tree ;;
    *) return 1 ;;
  esac

  expected="$dir_physical/$child"
  [ -d "$expected" ] && [ ! -L "$expected" ] || return 1
  expected_physical="$(oms_harness_physical_dir "$expected")" || return 1
  [ "$expected_physical" = "$expected" ] || return 1
  marker_worktree_physical="$(oms_harness_physical_dir "$marker_worktree")" || return 1
  [ "$marker_worktree_physical" = "$expected_physical" ] || return 1

  repo_physical="$(oms_harness_physical_dir "$marker_repo")" || return 1
  git -C "$repo_physical" rev-parse --git-dir >/dev/null 2>&1 || return 1
  git -C "$expected_physical" rev-parse --git-dir >/dev/null 2>&1 || return 1
  repo_common="$(oms_harness_git_path_physical "$repo_physical" --git-common-dir)" || return 1
  worktree_common="$(oms_harness_git_path_physical "$expected_physical" --git-common-dir)" || return 1
  [ "$repo_common" = "$worktree_common" ] || return 1
  worktree_git_dir="$(oms_harness_git_path_physical "$expected_physical" --git-dir)" || return 1
  case "$worktree_git_dir" in
    "$repo_common"/worktrees/*) ;;
    *) return 1 ;;
  esac

  while IFS= read -r registered; do
    registered="${registered//$'\r'/}"
    [ -n "$registered" ] || continue
    registered_physical="$(oms_harness_physical_dir "$registered" 2>/dev/null || true)"
    [ "$registered_physical" = "$expected_physical" ] || continue
    found=1
    break
  done <<EOF
$(git -C "$repo_physical" worktree list --porcelain 2>/dev/null | sed -n 's/^worktree //p')
EOF
  [ "$found" = 1 ] || return 1

  OMS_HARNESS_SAFE_RESIDUE_REPO="$repo_physical"
  OMS_HARNESS_SAFE_RESIDUE_WORKTREE="$expected_physical"
  return 0
}

OMS_HARNESS_RESIDUE_KEEP_REASON=""
OMS_HARNESS_RESIDUE_MERGE_ROWS=0
OMS_HARNESS_RESIDUE_COPY_FILES=0

# Evidence that a dead worktree's work is already preserved in the owning repo.
# Prints one line: "ok [N]" or the reason it is not proven. Only `merge` writes
# (to the owning repo's canonical artifact index, under the caller's lock).
#   settled  delegate run: the one attempt recording this worktree finished
#            (failed/timed_out ones only once decided or aged past
#            OMS_HARNESS_RESIDUE_AGED_DAYS, default 7, like --settle-review),
#            and the patch its run preserved reproduces the worktree's tree.
#   index    worktree .oms/artifacts/index.jsonl rows are in the canonical
#            index or can be appended, and each evidence file they name is
#            byte-identical in the owning repo or absent there; prints
#            "ok ROWS FILES".
#   merge    copy those absent evidence files, then append those rows.
oms_harness_residue_evidence() {  # MODE REPO WORKTREE PID
  python3 -B - "$OMS_HARNESS_RESIDUE_LIB_DIR" "$@" <<'PY' 2>/dev/null | tr -d '\r'
import hashlib, json, os, re, runpy, shutil, subprocess, sys, tempfile
lib, mode, repo, wt, pid = sys.argv[1:6]
aged_days = os.environ.get("OMS_HARNESS_RESIDUE_AGED_DAYS") or "7"
INDEX = ".oms/artifacts/index.jsonl"
DECIDED = ("completed", "accepted", "rejected")
try:
    store = runpy.run_path(os.path.join(lib, "artifact-index-store.py"))
    durable = runpy.run_path(os.path.join(lib, "durable-jsonl.py"))

    def rows_of(root):
        body = store["read_index"](root, INDEX, missing_ok=True)
        return [(line, json.loads(line.decode("utf-8"))) for line in body.splitlines(True)]

    def read(root, rel, limit):
        return durable["read_no_follow"](root, rel, "residue evidence", max_bytes=limit)

    def evidence_gap(row, copies):
        # Every row, indexed already or not, must find its worktree-held evidence
        # byte-identical in the owning repo, or absent there so it can be copied;
        # reads never follow links.
        for key in ("artifact", "patch", "source"):
            rel = row.get(key)
            if not isinstance(rel, str) or not rel:
                continue
            if os.path.isabs(rel):
                # A merged absolute path would still name the removed worktree; no row needs one.
                return "row %s evidence %s is an absolute path" % (row["event_id"], rel)
            if not os.path.lexists(os.path.join(wt, rel)):
                continue
            if not rel.startswith(".oms/") or ".." in rel.split("/"):
                return "row %s evidence %s is outside .oms" % (row["event_id"], rel)
            try:
                mine = read(wt, rel, 64 * 1024 * 1024)
            except BaseException:
                return "row %s evidence %s is not a safe regular file in the worktree" % (row["event_id"], rel)
            if not os.path.lexists(os.path.join(repo, rel)):
                if copies.setdefault(rel, mine) != mine:
                    return "row %s evidence %s has two contents" % (row["event_id"], rel)
                continue
            try:
                theirs = read(repo, rel, 64 * 1024 * 1024)
            except BaseException:
                return "row %s evidence %s is not a safe regular file in the owning repo" % (row["event_id"], rel)
            if mine != theirs:
                return "row %s evidence %s differs from the owning repo's copy" % (row["event_id"], rel)
        return ""

    def missing_rows():
        have = {}
        for _, r in rows_of(repo):
            have.setdefault(r.get("event_id"), []).append(r)
        todo, copies = [], {}
        for line, row in rows_of(wt):
            if not isinstance(row.get("event_id"), str):
                return None, None, "a row has no event_id"
            gap = evidence_gap(row, copies)
            if gap:
                return None, None, gap
            if row["event_id"] not in have:
                todo.append(line)
            elif row not in have[row["event_id"]]:
                return None, None, "row %s conflicts with the owning repo's row of that id" % row["event_id"]
        return todo, copies, ""

    if mode == "index":
        todo, copies, why = missing_rows()
        print("ok %d %d" % (len(todo), len(copies)) if todo is not None else why)
    elif mode == "merge":
        todo, copies, why = missing_rows()
        if todo is None:
            print(why)
        else:
            # Evidence first, so no merged row ever names a file the owning repo lacks.
            for rel, data in sorted(copies.items()):
                durable["atomic_mutate"](os.path.join(repo, rel), lambda old, data=data: data, repo,
                                         missing_ok=True, create_parent=True, expected_exists=False,
                                         label="residue evidence")
            if todo:
                store["append_rows"](repo, os.path.join(repo, INDEX), todo)
            print("ok %d %d" % (len(todo), len(copies)))
    else:
        # The run: the one attempt whose recorded location is this worktree.
        name = "worktree:%s/wt" % os.path.basename(os.path.dirname(os.path.realpath(wt)))
        listed = subprocess.run(["bash", os.path.join(os.path.dirname(lib), "oms"), "agent-events", "--repo", repo,
                                 "list", "--json"], check=True, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL).stdout
        runs = [a for a in json.loads(listed.decode("utf-8")) if (a.get("refs") or {}).get("panel_location") == name]
        if len(runs) != 1:
            print("%d attempts record this worktree, not one" % len(runs))
            raise SystemExit(0)
        run = runs[0]
        state, task = run.get("state"), run.get("task_id")
        if state in ("failed", "timed_out"):
            # A stopped run is the salvage source until its owner decides or it ages out like --settle-review.
            sys.path.insert(0, lib)
            import datetime, pathlib
            import panel_results
            outcome = panel_results.outcomes(pathlib.Path(repo), [task]).get(task) if isinstance(task, str) else None
            changed = datetime.datetime.strptime(run["updated_at"], "%Y-%m-%dT%H:%M:%SZ")
            now = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None)
            aged = now - changed >= datetime.timedelta(days=float(aged_days))
            if outcome not in DECIDED and not aged:
                print("run %s is %s with no owner decision and younger than %s days" % (run["attempt_id"], state, aged_days))
                raise SystemExit(0)
        elif state not in ("done", "cancelled", "abandoned"):
            print("run %s is %s, not finished" % (run["attempt_id"], state))
            raise SystemExit(0)
        # The patch the run preserved: the one delegate artifact of this pid.
        stems = [f[:-3] for f in os.listdir(os.path.join(repo, ".oms/artifacts/delegate"))
                 if re.fullmatch(r".+-[0-9]{8}T[0-9]{6}Z-%s\.md" % re.escape(pid), f)]
        if len(stems) != 1:
            print("%d delegate artifacts carry pid %s, not one" % (len(stems), pid))
            raise SystemExit(0)
        rel = ".oms/artifacts/delegate/%s.patch" % stems[0]
        try:
            patch = read(repo, rel, 64 * 1024 * 1024)
        except BaseException:
            print("no preserved patch %s" % rel)
            raise SystemExit(0)
        if not patch.strip():
            print("preserved patch %s is empty while the worktree has changes" % rel)
            raise SystemExit(0)
        for _, row in rows_of(repo):
            if row.get("patch") == rel and row.get("patch_sha256") not in (None, hashlib.sha256(patch).hexdigest()):
                print("preserved patch %s no longer matches its indexed digest" % rel)
                raise SystemExit(0)
        # Compare trees, not diff text: abbreviated blob ids in a patch grow with the object count.
        scratch = tempfile.mkdtemp(prefix="oms-residue-index.")
        try:
            env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull, GIT_CONFIG_NOSYSTEM="1")
            def git(*args, index=None):
                return subprocess.run(["git", "-c", "core.hooksPath=" + os.devnull, "-c", "core.fsmonitor=false",
                                       "-C", wt] + list(args), check=True, stdin=subprocess.DEVNULL,
                                      stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                      env=dict(env, GIT_INDEX_FILE=index) if index else env).stdout.decode().strip()
            with open(os.path.join(scratch, "preserved.patch"), "wb") as handle:
                handle.write(patch)
            current, patched = os.path.join(scratch, "current"), os.path.join(scratch, "patched")
            git("read-tree", "HEAD", index=current)
            git("add", "-A", index=current)
            git("read-tree", "HEAD", index=patched)
            git("apply", "--cached", "--binary", os.path.join(scratch, "preserved.patch"), index=patched)
            trees = git("write-tree", index=current), git("write-tree", index=patched)
            staged, head = git("write-tree"), git("rev-parse", "HEAD^{tree}")
        finally:
            shutil.rmtree(scratch, ignore_errors=True)
        if trees[0] != trees[1]:
            print("preserved patch %s does not reproduce the worktree's files" % rel)
        elif staged not in (head, trees[1]):
            print("staged content in the worktree is not reproduced by preserved patch %s" % rel)
        else:
            print("ok")
except SystemExit:
    raise
except BaseException as error:
    print("evidence unreadable (%s)" % type(error).__name__)
PY
}

# A dead owner does not make its worktree disposable: a crashed run can leave
# the only copy of uncommitted edits, an unreferenced commit, or ignored .oms
# records there. Sets OMS_HARNESS_RESIDUE_KEEP_REASON when the worktree must be
# kept; anything git cannot answer is kept too. Never moves or rewrites it.
# With REPO, KIND and PID (the marker's), a delegate worktree whose finished
# run and preserved patch prove its work settled, and a worktree whose .oms
# index rows reach the owning repo's index, are removable; the latter sets
# OMS_HARNESS_RESIDUE_MERGE_ROWS and OMS_HARNESS_RESIDUE_COPY_FILES for the
# caller to merge before removal.
oms_harness_worktree_keep_reason() {  # WORKTREE [REPO KIND PID]
  local wt="$1"
  local repo="${2:-}"
  local kind="${3:-}"
  local pid="${4:-}"
  local status=""
  local head=""
  local record=""
  local proof=""

  OMS_HARNESS_RESIDUE_KEEP_REASON=""
  OMS_HARNESS_RESIDUE_MERGE_ROWS=0
  OMS_HARNESS_RESIDUE_COPY_FILES=0
  if ! status="$(git -c core.fsmonitor=false -C "$wt" --no-optional-locks \
      status --porcelain --untracked-files=normal 2>/dev/null)"; then
    OMS_HARNESS_RESIDUE_KEEP_REASON="status is unreadable"
    return 0
  fi
  if [ -n "$status" ]; then
    OMS_HARNESS_RESIDUE_KEEP_REASON="has uncommitted or untracked files"
    # Only a delegate run preserves a patch an owner can have decided on; a
    # scratch worktree is the main's own and nothing proves its edits.
    if [ "$kind" = delegate ] && [ -n "$repo" ] && [ -n "$pid" ]; then
      proof="$(oms_harness_residue_evidence settled "$repo" "$wt" "$pid")"
      if [ "$proof" = ok ]; then
        OMS_HARNESS_RESIDUE_KEEP_REASON=""
      else
        OMS_HARNESS_RESIDUE_KEEP_REASON="$OMS_HARNESS_RESIDUE_KEEP_REASON; not settled: $proof"
        return 0
      fi
    else
      return 0
    fi
  fi
  head="$(git -C "$wt" rev-parse -q --verify HEAD 2>/dev/null || true)"
  head="${head//$'\r'/}"
  if [ -n "$head" ] && [ -z "$(git -C "$wt" for-each-ref --count=1 \
      --contains "$head" --format=x refs/heads refs/tags refs/remotes \
      2>/dev/null)" ]; then
    OMS_HARNESS_RESIDUE_KEEP_REASON="HEAD ${head:0:12} is on no branch, tag or remote ref"
    return 0
  fi
  # .oms is ignored, so status cannot see it; a symlinked .oms belongs elsewhere
  # and worktree removal does not follow it.
  [ -d "$wt/.oms" ] && [ ! -L "$wt/.oms" ] || return 0
  for record in plan/tasks.json failures.jsonl landings.jsonl artifacts/index.jsonl; do
    [ -s "$wt/.oms/$record" ] || continue
    if [ "$record" = artifacts/index.jsonl ] && [ -n "$repo" ]; then
      proof="$(oms_harness_residue_evidence index "$repo" "$wt" "$pid")"
      case "$proof" in
        "ok "*)
          proof="${proof#ok }"
          OMS_HARNESS_RESIDUE_MERGE_ROWS="${proof%% *}"
          OMS_HARNESS_RESIDUE_COPY_FILES="${proof#* }"
          continue
          ;;
      esac
      OMS_HARNESS_RESIDUE_KEEP_REASON="holds .oms/$record; not mergeable: $proof"
      return 0
    fi
    OMS_HARNESS_RESIDUE_KEEP_REASON="holds .oms/$record"
    return 0
  done
}

# Returns 0 when the dead residue's worktree is verified for removal (see
# oms_harness_safe_residue_worktree) and sets the keep reason for it.
oms_harness_classify_dead_residue() {  # BASE DIR MARKER
  OMS_HARNESS_RESIDUE_KEEP_REASON=""
  oms_harness_safe_residue_worktree "$1" "$2" \
    "$(oms_harness_read_marker_value "$3" repo)" \
    "$(oms_harness_read_marker_value "$3" worktree)" || return 1
  oms_harness_worktree_keep_reason "$OMS_HARNESS_SAFE_RESIDUE_WORKTREE" \
    "$OMS_HARNESS_SAFE_RESIDUE_REPO" "$(basename "$2" | sed 's/^oh-my-setting-\([a-z-]*\)\..*/\1/')" \
    "$(oms_harness_read_marker_value "$3" pid)"
}

oms_harness_count_stale_worktrees() {
  local repo="$1"
  local count=0
  local path

  git -C "$repo" rev-parse --git-dir >/dev/null 2>&1 || {
    printf '0\n'
    return 0
  }
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    [ -e "$path" ] || count=$((count + 1))
  done <<EOF
$(git -C "$repo" worktree list --porcelain 2>/dev/null | sed -n 's/^worktree //p')
EOF
  printf '%s\n' "$count"
}

oms_harness_prune_stale_worktrees() {
  local repo="$1"
  local dry_run="${2:-0}"
  local before

  before="$(oms_harness_count_stale_worktrees "$repo")"
  [ "$before" -gt 0 ] || return 0
  if [ "$dry_run" = "1" ]; then
    printf 'would prune: %s stale git worktree registration(s)\n' "$before"
    OMS_HARNESS_RESIDUE_WOULD_REMOVE=$((OMS_HARNESS_RESIDUE_WOULD_REMOVE + before))
  else
    git -C "$repo" worktree prune >/dev/null 2>&1 || true
    printf 'pruned: %s stale git worktree registration(s)\n' "$before"
    OMS_HARNESS_RESIDUE_REMOVED=$((OMS_HARNESS_RESIDUE_REMOVED + before))
  fi
}

oms_harness_lock_residue_count() {
  local lock_root
  local lock_dir
  local pid
  local count=0

  lock_root="$(oms_file_lock_dir)"
  [ -d "$lock_root" ] || {
    printf '0\n'
    return 0
  }
  for lock_dir in "$lock_root"/*.lock; do
    [ -d "$lock_dir" ] || continue
    [ -f "$lock_dir/pid" ] || continue
    pid="$(sed -n '1p' "$lock_dir/pid" 2>/dev/null || true)"
    if ! oms_file_lock_pid_alive "$pid"; then
      count=$((count + 1))
    fi
  done
  printf '%s\n' "$count"
}

# flock-path locks are regular files, and the design never unlinks them: a
# released flock file cannot be removed safely, because a waiter blocked on the
# old inode and a new arrival creating a fresh one would both hold the lock.
# They are therefore permanent by construction. Counting them anyway, because
# the residue scan above only sees mkdir-path directories — on this machine that
# meant reporting 0 while 12k files sat there, which reads as "nothing to clean"
# rather than "this kind is not cleanable". Growth is bounded by the number of
# distinct state-file paths ever locked; a run that leaks its lock dir into a
# real HOME (a test without OMS_LOCK_DIR) is what makes that number large.
oms_harness_lock_file_count() {
  local lock_root
  local lock_file
  local count=0

  lock_root="$(oms_file_lock_dir)"
  [ -d "$lock_root" ] || {
    printf '0\n'
    return 0
  }
  for lock_file in "$lock_root"/*.lock; do
    [ -f "$lock_file" ] || continue
    count=$((count + 1))
  done
  printf '%s\n' "$count"
}

oms_harness_cleanup_dead_locks() {
  local dry_run="${1:-0}"
  local lock_root
  local lock_dir
  local pid

  lock_root="$(oms_file_lock_dir)"
  [ -d "$lock_root" ] || return 0
  for lock_dir in "$lock_root"/*.lock; do
    [ -d "$lock_dir" ] || continue
    [ -f "$lock_dir/pid" ] || continue
    pid="$(sed -n '1p' "$lock_dir/pid" 2>/dev/null || true)"
    oms_file_lock_pid_alive "$pid" && continue
    if [ "$dry_run" = "1" ]; then
      printf 'would remove: %s (dead harness lock)\n' "$lock_dir"
      OMS_HARNESS_RESIDUE_WOULD_REMOVE=$((OMS_HARNESS_RESIDUE_WOULD_REMOVE + 1))
    else
      rm -rf "$lock_dir"
      printf 'removed: %s (dead harness lock)\n' "$lock_dir"
      OMS_HARNESS_RESIDUE_REMOVED=$((OMS_HARNESS_RESIDUE_REMOVED + 1))
    fi
  done
}

oms_harness_tmp_residue_count() {
  local base
  local dir
  local marker
  local pid
  local temporary
  local count=0

  while IFS= read -r base; do
    [ -d "$base" ] || continue
    for dir in "$base"/oh-my-setting-*; do
      [ -d "$dir" ] || continue
      # Only trust directories this user owns. A planted marker must never
      # steer worktree removal at a real checkout, regardless of scratch root.
      [ -O "$dir" ] && [ ! -L "$dir" ] || continue
      marker="$dir/.oh-my-setting-tmp"
      [ -f "$marker" ] && [ ! -L "$marker" ] || continue
      [ "$(oms_harness_read_marker_value "$marker" kind)" = "oh-my-setting-temp" ] || continue
      temporary="$(oms_harness_read_marker_value "$marker" temporary)"
      [ "$temporary" = "1" ] || continue
      pid="$(oms_harness_read_marker_value "$marker" pid)"
      oms_file_lock_pid_alive "$pid" && continue
      # Kept work is not residue: counting it would warn with a remedy that
      # cannot act on it.
      oms_harness_classify_dead_residue "$base" "$dir" "$marker" || true
      [ -n "$OMS_HARNESS_RESIDUE_KEEP_REASON" ] || count=$((count + 1))
    done
  done <<EOF
$(oms_harness_temp_bases)
EOF
  printf '%s\n' "$count"
}

oms_harness_cleanup_temp_dirs() {
  local dry_run="${1:-0}"
  local base
  local dir
  local marker
  local pid
  local safe
  local temporary
  local proof

  while IFS= read -r base; do
    [ -d "$base" ] || continue
    for dir in "$base"/oh-my-setting-*; do
      [ -d "$dir" ] || continue
      # Only act on directories this user owns (see count function).
      [ -O "$dir" ] && [ ! -L "$dir" ] || continue
      marker="$dir/.oh-my-setting-tmp"
      [ -f "$marker" ] && [ ! -L "$marker" ] || continue
      [ "$(oms_harness_read_marker_value "$marker" kind)" = "oh-my-setting-temp" ] || continue
      temporary="$(oms_harness_read_marker_value "$marker" temporary)"
      [ "$temporary" = "1" ] || continue
      pid="$(oms_harness_read_marker_value "$marker" pid)"
      oms_file_lock_pid_alive "$pid" && continue

      safe=0
      oms_harness_classify_dead_residue "$base" "$dir" "$marker" && safe=1
      if [ -n "$OMS_HARNESS_RESIDUE_KEEP_REASON" ]; then
        printf 'kept: %s (dead harness temp dir; worktree %s)\n' \
          "$dir" "$OMS_HARNESS_RESIDUE_KEEP_REASON"
        OMS_HARNESS_RESIDUE_KEPT=$((OMS_HARNESS_RESIDUE_KEPT + 1))
        continue
      fi
      if [ "$dry_run" = "1" ]; then
        if [ "$OMS_HARNESS_RESIDUE_MERGE_ROWS" -gt 0 ] || [ "$OMS_HARNESS_RESIDUE_COPY_FILES" -gt 0 ]; then
          printf 'would remove: %s (dead harness temp dir; would merge %s artifact index row(s) and copy %s evidence file(s) first)\n' \
            "$dir" "$OMS_HARNESS_RESIDUE_MERGE_ROWS" "$OMS_HARNESS_RESIDUE_COPY_FILES"
        else
          printf 'would remove: %s (dead harness temp dir)\n' "$dir"
        fi
        OMS_HARNESS_RESIDUE_WOULD_REMOVE=$((OMS_HARNESS_RESIDUE_WOULD_REMOVE + 1))
        continue
      fi

      if [ "$safe" = 1 ] && [ -s "$OMS_HARNESS_SAFE_RESIDUE_WORKTREE/.oms/artifacts/index.jsonl" ]; then
        proof="$(oms_with_file_lock "$OMS_HARNESS_SAFE_RESIDUE_REPO/.oms/artifacts/index.jsonl" \
          oms_harness_residue_evidence merge "$OMS_HARNESS_SAFE_RESIDUE_REPO" \
          "$OMS_HARNESS_SAFE_RESIDUE_WORKTREE" "" 2>/dev/null || true)"
        case "$proof" in
          "ok "*) ;;
          *)
            printf 'kept: %s (dead harness temp dir; worktree holds .oms/artifacts/index.jsonl; not mergeable: %s)\n' \
              "$dir" "${proof:-index merge failed}"
            OMS_HARNESS_RESIDUE_KEPT=$((OMS_HARNESS_RESIDUE_KEPT + 1))
            continue
            ;;
        esac
      fi
      if [ "$safe" = 1 ]; then
        # Both arguments are physical paths established above. In particular,
        # the remove target is the expected residue child, never marker input.
        git -C "$OMS_HARNESS_SAFE_RESIDUE_REPO" worktree remove --force \
          "$OMS_HARNESS_SAFE_RESIDUE_WORKTREE" >/dev/null 2>&1 || true
        git -C "$OMS_HARNESS_SAFE_RESIDUE_REPO" worktree prune >/dev/null 2>&1 || true
      fi
      rm -rf "$dir"
      printf 'removed: %s (dead harness temp dir)\n' "$dir"
      OMS_HARNESS_RESIDUE_REMOVED=$((OMS_HARNESS_RESIDUE_REMOVED + 1))
    done
  done <<EOF
$(oms_harness_temp_bases)
EOF
}

oms_harness_cleanup_residue() {
  local repo="${1:-}"
  local dry_run="${2:-0}"

  oms_harness_residue_reset
  if [ -n "$repo" ] && git -C "$repo" rev-parse --git-dir >/dev/null 2>&1; then
    oms_harness_prune_stale_worktrees "$repo" "$dry_run"
  fi
  oms_harness_cleanup_dead_locks "$dry_run"
  oms_harness_cleanup_temp_dirs "$dry_run"
}

# A delegate killed by SIGKILL never runs its cleanup trap, so its liveness
# marker stays and reads as an ORPHAN forever; gc only runs on request. Each
# delegate start therefore reaps markers whose pid is proven dead and that no
# recovery path needs: task/lease/executor/autopilot-coupled markers stay for
# gc's plan recovery. Validation mirrors gc's delegation_marker_snapshot. The
# set lock is only tried, never waited for, and every entry that is not
# provably safe is kept, so a delegate run never stalls or fails here.
oms_harness_reap_dead_delegation_markers() {  # PHYSICAL_REPO
  local repo="$1"

  [ -d "$repo/.oms/delegations" ] && [ ! -L "$repo/.oms/delegations" ] || return 0
  command -v python3 >/dev/null 2>&1 || return 0
  oms_try_file_lock "$repo/.oms/delegations/.marker-set-lock-target" \
    oms_harness_reap_dead_delegation_markers_locked "$repo" \
    "$OMS_FILE_LOCK_LIB_DIR/process_liveness.py" >/dev/null 2>&1 || true
}

oms_harness_reap_dead_delegation_markers_locked() {  # REPO LIVENESS_PY
  python3 - "$1" "$2" <<'PY'
import json, os, random, re, runpy, stat, sys

repo = os.path.realpath(sys.argv[1])
liveness = runpy.run_path(sys.argv[2])
pid_alive = liveness["pid_alive"]
native_proven = liveness["persisted_native_pid_is_proven"]
marker_dir = os.path.join(repo, ".oms", "delegations")
safe_name = re.compile(r"^[A-Za-z0-9._-]+\.json$")
limit = 64 * 1024
flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
flags |= getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)

info = os.lstat(marker_dir)
if (not stat.S_ISDIR(info.st_mode) or
        os.path.normcase(os.path.realpath(marker_dir)) !=
        os.path.normcase(os.path.abspath(marker_dir))):
    raise SystemExit(0)
names = []
with os.scandir(marker_dir) as iterator:
    for entry in iterator:
        if len(names) >= 65536:
            break
        if safe_name.fullmatch(entry.name):
            names.append(entry.name)
# Bounded per run but still progressing: a shuffled sample lets successive
# starts drain a backlog that kept entries at the front would otherwise block.
random.shuffle(names)

def reapable(path):
    before = os.lstat(path)
    if not stat.S_ISREG(before.st_mode) or before.st_size > limit:
        return None
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        if (not stat.S_ISREG(opened.st_mode) or
                (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)):
            return None
        payload = os.read(descriptor, limit + 1)
        if len(payload) > limit or os.read(descriptor, 1):
            return None
    finally:
        os.close(descriptor)
    marker = json.loads(payload.decode("utf-8"))
    if not isinstance(marker, dict):
        return None
    schema = marker.get("schema")
    pid = marker.get("pid")
    native_pid = marker.get("native_pid")
    if (isinstance(schema, bool) or schema not in {1, 2, 3, 4} or
            isinstance(pid, bool) or not isinstance(pid, int) or
            not 0 < pid <= 0x7FFFFFFF):
        return None
    if "native_pid" in marker and (
            isinstance(native_pid, bool) or not isinstance(native_pid, int)
            or not 0 < native_pid <= 0xFFFFFFFF):
        return None
    if schema == 4 and "native_pid" not in marker:
        return None
    if not native_proven(native_pid, marker.get("native_pid_source")):
        return None
    for key in ("task_id", "lease_id", "executor_id", "autopilot_owner_id"):
        if marker.get(key, ""):
            return None
    if pid_alive(pid, native_pid=native_pid):
        return None
    return (opened.st_dev, opened.st_ino, opened.st_size)

removed = 0
for name in names[:4096]:
    path = os.path.join(marker_dir, name)
    try:
        identity = reapable(path)
        if identity is None:
            continue
        final = os.lstat(path)
        if (not stat.S_ISREG(final.st_mode) or
                (final.st_dev, final.st_ino, final.st_size) != identity):
            continue
        os.unlink(path)
        removed += 1
    except Exception:
        continue
if removed:
    try:
        directory = os.open(marker_dir, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except OSError:
        pass
PY
}

oms_harness_count_unindexed_artifacts() {
  local repo="$1"
  local artifacts_dir="$repo/.oms/artifacts"
  local index="$artifacts_dir/index.jsonl"

  [ -d "$artifacts_dir" ] || {
    printf '0\n'
    return 0
  }
  command -v python3 >/dev/null 2>&1 || {
    printf '0\n'
    return 0
  }
  # Count with the same grace window prune --files deletes with. A live
  # provider call writes its artifact before the index row lands, so counting
  # age-blind makes doctor warn about in-flight work and then prescribe a
  # command that correctly deletes nothing — a remedy that cannot act on the
  # complaint.
  python3 - "$repo" "$artifacts_dir" "$index" \
    "${OMS_ARTIFACT_ORPHAN_GRACE:-86400}" <<'PY'
import json
import os
import sys
import time

repo, artifacts_dir, index, grace_raw = sys.argv[1:]
try:
    grace = max(0, int(grace_raw))
except ValueError:
    grace = 86400
now = time.time()
tracked = set()
if os.path.exists(index):
    with open(index, "r", encoding="utf-8") as f:
        for line in f:
            try:
                row = json.loads(line)
            except Exception:
                continue
            if not isinstance(row, dict):
                continue
            for key in ("artifact", "patch", "source"):
                value = row.get(key)
                if not isinstance(value, str) or not value:
                    continue
                path = value if os.path.isabs(value) else os.path.join(repo, value)
                tracked.add(os.path.realpath(path))

count = 0
for root, dirs, files in os.walk(artifacts_dir):
    dirs[:] = [d for d in dirs if not d.startswith(".")]
    for name in files:
        if not (name.endswith(".md") or name.endswith(".patch")):
            continue
        path = os.path.join(root, name)
        if os.path.realpath(path) in tracked:
            continue
        try:
            if now - os.stat(path).st_mtime < grace:
                continue
        except OSError:
            continue
        count += 1
print(count)
PY
}
