# shellcheck shell=bash
# Cross-cutting primitives shared by the run and worktree tools. Sourced, not
# executed.

# A fresh worktree checks out committed content only, so agent-facing files kept
# local-only (project-private.sh) would be absent there and a worker would lose
# the project rules. Copy them in — but only while the shared .git/info/exclude
# still ignores them, since an unignored copy would be swept up by `git add -A`
# and pollute the patch.
# Locals are named wt_*/src_* on purpose: with `shellcheck -x`, a local named
# `worktree` here makes every sourcing script's `MODE=worktree-write` look like
# arithmetic (SC2100).
oms_seed_local_agent_files() {
  local src_repo="$1"
  local wt_dir="$2"
  local name

  [ -d "$src_repo" ] && [ -d "$wt_dir" ] || return 0
  for name in AGENTS.md CLAUDE.md GEMINI.md PROJECT.md; do
    [ -f "$src_repo/$name" ] || continue
    [ -e "$wt_dir/$name" ] && continue
    git -C "$wt_dir" check-ignore -q "$name" 2>/dev/null || continue
    cp "$src_repo/$name" "$wt_dir/$name" 2>/dev/null || true
  done
}

# Effective run id for auto-linking: explicit OMS_RUN_ID wins; otherwise the
# repo's .oms/runs/CURRENT pointer (written by oms-run.sh new) when it is
# fresh. A stale pointer must not misjoin unrelated later work, so it expires
# after OMS_RUN_CURRENT_TTL seconds (default 86400, same as board claims).
# Prints nothing and returns nonzero when neither applies.
oms_effective_run_id() {
  local state_root="$1"
  local current id minted now ttl

  if [ -n "${OMS_RUN_ID:-}" ]; then
    printf '%s\n' "$OMS_RUN_ID"
    return 0
  fi
  current="$state_root/.oms/runs/CURRENT"
  [ -f "$current" ] || return 1
  id="$(awk 'NR==1{print $1}' "$current")"
  minted="$(awk 'NR==1{print $2}' "$current")"
  [ -n "$id" ] || return 1
  case "$minted" in *[!0-9]*|"") return 1 ;; esac
  ttl="${OMS_RUN_CURRENT_TTL:-86400}"
  case "$ttl" in *[!0-9]*|"") ttl=86400 ;; esac
  now="$(date +%s)"
  [ $((now - minted)) -le "$ttl" ] || return 1
  printf '%s\n' "$id"
}

# Hash stdin / a file with whatever sha256 tool exists. Returns nonzero (no
# output) when none is available, so callers can compose without aborting.
oms_sha256_stream() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 | awk '{print $NF}'
  else
    return 1
  fi
}

oms_sha256_file() {
  [ -f "$1" ] || return 1
  oms_sha256_stream < "$1"
}

# Hash only git metadata and tracked diff bytes. The ledger never stores diff
# content, file paths, host paths, or secrets.
oms_git_state_fingerprint() {
  local root="$1"
  local head diff_hash untracked_hash

  if ! git -C "$root" rev-parse --git-dir >/dev/null 2>&1; then
    printf 'non-git\n'
    return 0
  fi
  head="$(git -C "$root" rev-parse HEAD 2>/dev/null || printf 'unborn')"
  diff_hash="$({
    git -C "$root" diff --binary HEAD -- 2>/dev/null || true
    git -C "$root" diff --cached --binary -- 2>/dev/null || true
  } | oms_sha256_stream)"
  untracked_hash="$(python3 - "$root" <<'PY'
import hashlib, os, subprocess, sys
root = sys.argv[1]
paths = subprocess.check_output(
    ["git", "-C", root, "ls-files", "-z", "--others", "--exclude-standard"])
h = hashlib.sha256()
for raw in sorted(x for x in paths.split(b"\0") if x):
    path = os.path.join(root, os.fsdecode(raw))
    h.update(hashlib.sha256(raw).digest())
    try:
        info = os.lstat(path)
        # Metadata makes creation/replacement/content writes visible without
        # reading an unbounded dataset/checkpoint. Paths are hashed above and
        # only this final aggregate digest reaches the ledger.
        h.update(("M:%o:%d:%d" % (
            info.st_mode, info.st_size,
            getattr(info, "st_mtime_ns", int(info.st_mtime * 1_000_000_000)),
        )).encode())
    except OSError:
        h.update(b"MISSING")
print(h.hexdigest())
PY
)"
  printf '%s:%s:%s\n' "$head" "$diff_hash" "$untracked_hash"
}

# --- Worker authority detection ---------------------------------------------
# "A worker cannot widen its authority" was prose in the brief and a scope check
# on the returned patch. Neither notices a worker that reaches around the patch
# entirely: editing the primary worktree by absolute path, rewriting local git
# config, adding a remote, moving refs, or installing a hook. Those surfaces are
# not supposed to change while a worker runs, so they can simply be fingerprinted
# before and after. This is detection, not a sandbox: it says loudly that
# something moved and keeps the evidence, and it cannot prevent the write.
#
# .oms is not compared byte-for-byte: workers are given OMS_STATE_REPO so they
# can append shared harness state, and the harness itself writes there during a
# run. But "may append" is not "may rewrite" — the append-only JSONL families
# are checked for exactly that, and no state file may vanish.
#
# What this cannot see, by construction: a write that is undone before the
# worker exits, anything the worker reads (inherited tokens, ssh agents), and
# anything it does outside the repository (HOME, /tmp, a surviving background
# process). Those need process isolation, which a bash harness does not have.
oms_worker_surface_fingerprint() {
  local repo="$1"
  local git_dir

  git -C "$repo" rev-parse --git-dir >/dev/null 2>&1 || { printf 'non-git\n'; return 0; }
  git_dir="$(git -C "$repo" rev-parse --git-common-dir 2>/dev/null || printf '')"
  case "$git_dir" in
    "") git_dir="$(git -C "$repo" rev-parse --git-dir)" ;;
    /*) ;;
    *) git_dir="$repo/$git_dir" ;;
  esac
  {
    printf 'config\n'
    git -C "$repo" config --local --list 2>/dev/null | LC_ALL=C sort
    printf 'remotes\n'
    git -C "$repo" remote -v 2>/dev/null | LC_ALL=C sort
    printf 'refs\n'
    git -C "$repo" show-ref 2>/dev/null | LC_ALL=C sort
    git -C "$repo" rev-parse HEAD 2>/dev/null || printf 'unborn\n'
    printf 'tracked\n'
    git -C "$repo" status --porcelain --untracked-files=no 2>/dev/null | LC_ALL=C sort
    printf 'hooks\n'
    if [ -d "$git_dir/hooks" ]; then
      find "$git_dir/hooks" -maxdepth 1 -type f ! -name '*.sample' -print 2>/dev/null |
        LC_ALL=C sort |
        while IFS= read -r hook; do
          printf '%s %s\n' "$(basename "$hook")" "$(oms_sha256_file "$hook" 2>/dev/null || printf 'unreadable')"
        done
    fi
  } | oms_sha256_stream
}

# Which surface moved, for a message that names the problem instead of just
# reporting a hash mismatch.
# Compare recorded shared state against the current tree. Unlike the other
# surfaces this is not an equality check: appending is allowed, so only a
# changed prefix, a shrunk file, or a disappearance counts.
oms_worker_state_violations() {
  local repo="$1"
  local before="$2"

  [ -f "$before" ] || return 0
  OMS_WG_REPO="$repo" OMS_WG_BEFORE="$before" python3 - <<'PY'
import hashlib, os

root = os.path.join(os.environ["OMS_WG_REPO"], ".oms")
problems = []
with open(os.environ["OMS_WG_BEFORE"], encoding="utf-8", errors="replace") as f:
    for line in f:
        parts = line.rstrip("\n").split(" ")
        if len(parts) < 2:
            continue
        rel, kind = parts[0], parts[1]
        path = os.path.join(root, rel)
        if not os.path.exists(path):
            problems.append("%s was deleted" % rel)
            continue
        if kind != "APPEND" or len(parts) != 4:
            continue
        try:
            size = int(parts[2])
        except ValueError:
            continue
        expected = parts[3]
        try:
            now = os.path.getsize(path)
        except OSError:
            problems.append("%s became unreadable" % rel)
            continue
        if now < size:
            problems.append("%s was truncated (%d -> %d bytes)" % (rel, size, now))
            continue
        digest = hashlib.sha256()
        with open(path, "rb") as fh:
            remaining = size
            while remaining > 0:
                chunk = fh.read(min(1024 * 1024, remaining))
                if not chunk:
                    break
                remaining -= len(chunk)
                digest.update(chunk)
        if digest.hexdigest() != expected:
            problems.append("%s had existing rows rewritten" % rel)
for problem in problems[:5]:
    print(problem)
PY
}

oms_worker_surface_diff() {
  local repo="$1"
  local before_dir="$2"
  local changed=""
  local name

  for name in config remotes refs tracked files gitmeta hooks; do
    [ -f "$before_dir/$name" ] || continue
    if ! oms_worker_surface_capture_one "$repo" "$name" | cmp -s - "$before_dir/$name"; then
      changed="${changed:+$changed, }$name"
    fi
  done
  # Shared state is compared by contract, not by equality: see above. The detail
  # goes to a file because this function runs in a command substitution, where
  # an exported variable would die with the subshell.
  oms_worker_state_violations "$repo" "$before_dir/omsstate" > "$before_dir/state-detail"
  [ ! -s "$before_dir/state-detail" ] || changed="${changed:+$changed, }shared-state"
  printf '%s\n' "$changed"
}

oms_worker_surface_capture_one() {
  local repo="$1"
  local name="$2"
  local git_dir

  git_dir="$(git -C "$repo" rev-parse --git-common-dir 2>/dev/null || printf '')"
  case "$git_dir" in
    "") git_dir="$(git -C "$repo" rev-parse --git-dir 2>/dev/null || printf '')" ;;
    /*) ;;
    *) git_dir="$repo/$git_dir" ;;
  esac
  case "$name" in
    config) git -C "$repo" config --local --list 2>/dev/null | LC_ALL=C sort ;;
    remotes) git -C "$repo" remote -v 2>/dev/null | LC_ALL=C sort ;;
    refs)
      git -C "$repo" show-ref 2>/dev/null | LC_ALL=C sort
      git -C "$repo" rev-parse HEAD 2>/dev/null || printf 'unborn\n'
      ;;
    tracked) git -C "$repo" status --porcelain --untracked-files=no 2>/dev/null | LC_ALL=C sort ;;
    files)
      # Untracked AND ignored content, by stat rather than by reading bytes.
      # `git status --ignored` collapses a fully ignored directory to one entry,
      # so replacing .venv/bin/python or planting sitecustomize.py inside an
      # ignored tree is invisible to it. Enumerating with stat catches the
      # replacement without reading a single dataset byte. The walk is bounded:
      # a truncated scan says so instead of quietly covering less.
      OMS_WG_REPO="$repo" OMS_WG_MAX="${OMS_WORKER_GUARD_MAX_FILES:-20000}" \
      OMS_WG_EXCLUDE="${OMS_WORKER_GUARD_EXCLUDE:-}" python3 - <<'PY'
import hashlib, os, subprocess, sys

root = os.environ["OMS_WG_REPO"]
try:
    budget = int(os.environ.get("OMS_WG_MAX", "20000"))
except ValueError:
    budget = 20000
try:
    raw = subprocess.check_output(
        ["git", "-C", root, "ls-files", "-z", "--others"],
        stderr=subprocess.DEVNULL)
except Exception:
    raise SystemExit(0)
paths = sorted(p for p in raw.split(b"\0") if p)
count = 0
# The harness writes this run's own artifacts and patch somewhere; when that is
# inside the repo it is the harness moving, not the worker.
excluded = [".oms"]
for extra in (os.environ.get("OMS_WG_EXCLUDE") or "").splitlines():
    extra = extra.strip().strip("/")
    if extra:
        excluded.append(extra)
for rel in paths:
    name = os.fsdecode(rel)
    if any(name == prefix or name.startswith(prefix + "/") for prefix in excluded):
        continue
    count += 1
    if count > budget:
        print("TRUNCATED after %d entries" % budget)
        break
    full = os.path.join(root, name)
    try:
        info = os.lstat(full)
        print("%s %o %d %d" % (
            hashlib.sha256(rel).hexdigest(), info.st_mode, info.st_size,
            getattr(info, "st_mtime_ns", int(info.st_mtime * 1_000_000_000))))
    except OSError:
        print("%s MISSING" % hashlib.sha256(rel).hexdigest())
PY
      ;;
    omsstate)
      # Shared state is append-only by contract: every .oms JSONL family is
      # written with >>. A worker that rewrites history there — dropping the
      # failure that blocks its retry, forging a landing, editing a thread turn
      # another agent will read as evidence — leaves a file whose existing bytes
      # changed. Record each file's length and the hash of exactly those bytes;
      # growth is legitimate, mutation of the prefix is not. Deletion of any
      # state file is a violation regardless of family.
      OMS_WG_REPO="$repo" python3 - <<'PY'
import hashlib, os, sys

root = os.path.join(os.environ["OMS_WG_REPO"], ".oms")
if not os.path.isdir(root):
    raise SystemExit(0)
rows = []
for base, dirs, files in os.walk(root):
    dirs[:] = sorted(d for d in dirs if not os.path.islink(os.path.join(base, d)))
    for name in sorted(files):
        path = os.path.join(base, name)
        rel = os.path.relpath(path, root).replace(os.sep, "/")
        if os.path.islink(path):
            rows.append("%s LINK" % rel)
            continue
        if not name.endswith(".jsonl"):
            # Presence only: these are rewritten by the harness by design
            # (plan transitions, executor lifecycle, run pointers).
            rows.append("%s EXISTS" % rel)
            continue
        try:
            size = os.path.getsize(path)
            digest = hashlib.sha256()
            with open(path, "rb") as f:
                remaining = size
                while remaining > 0:
                    chunk = f.read(min(1024 * 1024, remaining))
                    if not chunk:
                        break
                    remaining -= len(chunk)
                    digest.update(chunk)
            rows.append("%s APPEND %d %s" % (rel, size, digest.hexdigest()))
        except OSError:
            rows.append("%s UNREADABLE" % rel)
for row in rows:
    print(row)
PY
      ;;
    gitmeta)
      # Object-store wiring and repository metadata outside config/refs/hooks:
      # an added alternate silently widens where objects may come from, a graft
      # or shallow edit rewrites history's shape, a new linked worktree is
      # another checkout of this repo, and a submodule keeps its own config,
      # refs, and hooks under .git/modules.
      if [ -n "$git_dir" ] && [ -d "$git_dir" ]; then
        local meta
        for meta in objects/info/alternates objects/info/http-alternates \
          info/exclude info/grafts info/attributes shallow; do
          if [ -f "$git_dir/$meta" ]; then
            printf '%s %s\n' "$meta" \
              "$(oms_sha256_file "$git_dir/$meta" 2>/dev/null || printf 'unreadable')"
          fi
        done
        if [ -d "$git_dir/worktrees" ]; then
          find "$git_dir/worktrees" -maxdepth 2 \( -name gitdir -o -name HEAD \) -type f \
            -print 2>/dev/null | LC_ALL=C sort |
            while IFS= read -r wt; do
              printf 'worktree %s %s\n' "${wt#"$git_dir"/}" \
                "$(oms_sha256_file "$wt" 2>/dev/null || printf 'unreadable')"
            done
        fi
        if [ -d "$git_dir/modules" ]; then
          find "$git_dir/modules" -maxdepth 3 \( -name config -o -name HEAD \) -type f \
            -print 2>/dev/null | LC_ALL=C sort |
            while IFS= read -r mod; do
              printf 'module %s %s\n' "${mod#"$git_dir"/}" \
                "$(oms_sha256_file "$mod" 2>/dev/null || printf 'unreadable')"
            done
        fi
      fi
      ;;
    hooks)
      if [ -n "$git_dir" ] && [ -d "$git_dir/hooks" ]; then
        find "$git_dir/hooks" -maxdepth 1 -type f ! -name '*.sample' -print 2>/dev/null |
          LC_ALL=C sort |
          while IFS= read -r hook; do
            printf '%s %s\n' "$(basename "$hook")" "$(oms_sha256_file "$hook" 2>/dev/null || printf 'unreadable')"
          done
      fi
      ;;
  esac
}

# Capture each surface separately so a later comparison can name what moved.
oms_worker_surface_snapshot() {
  local repo="$1"
  local dir="$2"
  local name

  mkdir -p "$dir" || return 1
  for name in config remotes refs tracked files gitmeta hooks omsstate; do
    oms_worker_surface_capture_one "$repo" "$name" > "$dir/$name" 2>/dev/null || true
  done
}
