# shellcheck shell=bash
# Cross-cutting primitives shared by the run and worktree tools. Sourced, not
# executed.

oms_require_current_task_contract() {
  printf '%s' "$1" | python3 -c '
import json,sys
row=json.load(sys.stdin)
if row.get("executor_id") or row.get("executor_soul_sha256"):
    sys.stderr.write("error: Soul executor receipt is retired; preserve the old evidence and create a fresh plan task\n")
    sys.exit(2)
'
}

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
# repo's .oms/runs/CURRENT pointer (written by run.sh new) when it is
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

# Git is an execution boundary, not only a content reader. Repository and
# worktree config can make seemingly read-only status/diff commands execute a
# worker-planted program (fsmonitor, external diff/textconv, filters, includes).
# Inspect those files directly: using `git config` to decide whether config is
# safe would already have followed include/includeIf directives.
#
# Command-scope config is parent-owned. The harness appends only an exact
# hooksPath=/dev/null guard; duplicates are harmless and everything else is
# rejected so an inherited GIT_CONFIG_* value cannot widen execution.
oms_git_assert_safe_execution_config() {  # REPO [diff-read]
  local root="$1"
  local mode="${2:-strict}"
  local command_problem=""

  # Pure shell on purpose: this guard fronts every git call the harness
  # makes, and a python3 start per call was the single largest process cost
  # in the control plane (two of every three interpreter starts in a
  # goal-drive cycle). The rules are unchanged: the count must be a canonical
  # non-negative decimal of at most 64, every pair must be present, only an
  # exact hooksPath=/dev/null pair is admitted, and with no pair rejected an
  # inherited GIT_CONFIG_PARAMETERS is itself the problem.
  local raw_count="${GIT_CONFIG_COUNT:-0}"
  local count=0 index=0 key_name="" value_name="" key="" value="" rejected=0
  case "$raw_count" in
    ''|*[!0-9]*) command_problem="invalid GIT_CONFIG_COUNT" ;;
    0) count=0 ;;
    0*) command_problem="invalid GIT_CONFIG_COUNT" ;;
    *) count="$raw_count" ;;
  esac
  if [ -z "$command_problem" ]; then
    if [ "${#count}" -gt 3 ] || [ "$count" -gt 64 ]; then
      command_problem="oversized GIT_CONFIG_COUNT"
    fi
  fi
  if [ -z "$command_problem" ]; then
    while [ "$index" -lt "$count" ]; do
      key_name="GIT_CONFIG_KEY_$index"
      value_name="GIT_CONFIG_VALUE_$index"
      if [ -z "${!key_name+set}" ] || [ -z "${!value_name+set}" ]; then
        command_problem="incomplete command-scope config"
        rejected=1
        break
      fi
      key="${!key_name}"
      value="${!value_name}"
      case "$key" in
        [Cc][Oo][Rr][Ee].[Hh][Oo][Oo][Kk][Ss][Pp][Aa][Tt][Hh])
          if [ "$value" = /dev/null ]; then
            index=$((index + 1))
            continue
          fi
          ;;
      esac
      command_problem="${key:-empty command-scope key}"
      rejected=1
      break
    done
    if [ "$rejected" -eq 0 ] && [ -n "${GIT_CONFIG_PARAMETERS:-}" ]; then
      command_problem="GIT_CONFIG_PARAMETERS"
    fi
  fi
  if [ -n "$command_problem" ]; then
    echo "unsafe command-scope Git config: $command_problem" >&2
    return 2
  fi

  root="$(cd "$root" 2>/dev/null && pwd -P)" || return 2
  python3 - "$root" "$mode" <<'PY'
import os
import shutil
import stat
import subprocess
import sys

root, mode = sys.argv[1:]

def refuse(reason):
    print("cannot inspect repository Git config: " + reason, file=sys.stderr)
    raise SystemExit(2)

if mode not in ("strict", "diff-read"):
    refuse("unknown inspection mode")

# Keep the trusted runtime environment (including Windows DLL lookup), but no
# inherited Git configuration, tracing, discovery redirects or command hooks.
git = shutil.which("git")
if not git:
    refuse("Git unavailable")
git = os.path.abspath(git)
env = {key: value for key, value in os.environ.items()
       if not key.upper().startswith("GIT_")}
env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_SYSTEM=os.devnull,
           GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_COUNT="0",
           GIT_TERMINAL_PROMPT="0", GIT_PAGER="cat", LC_ALL="C")
command = [git, "--no-pager", "-c", "core.hooksPath=/dev/null",
           "-c", "core.fsmonitor=false"]

# rev-parse does not run drivers, but discovery can read includes before we
# know the metadata paths. Bound that bootstrap too: a FIFO must not hang the
# guard. Only the stdin parser below promises not to read include targets.
def git_output(args, child_env, raw=None):
    try:
        result = subprocess.run(command + args, input=raw, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL, env=child_env,
                                timeout=5, check=False, shell=False)
    except (OSError, subprocess.TimeoutExpired):
        refuse("Git inspection failed or timed out")
    if result.returncode:
        refuse("Git inspection failed")
    return result.stdout

paths = []
for flag, name in (("--git-common-dir", "config"),
                   ("--git-dir", "config.worktree")):
    output = git_output(["-C", root, "rev-parse", flag], env)
    directory = os.fsdecode(output).replace("\r", "").rstrip("\n")
    if not directory:
        refuse("missing metadata path")
    directory = os.path.realpath(os.path.join(root, directory))
    paths.append(os.path.join(directory, name))

def read_config(path, optional):
    try:
        before = os.lstat(path)
    except FileNotFoundError:
        if optional:
            return None
        refuse("missing config")
    except OSError:
        refuse("unreadable config")
    if not stat.S_ISREG(before.st_mode):
        refuse("non-regular config")
    limit = 1024 * 1024
    if before.st_size > limit:
        refuse("config exceeds 1 MiB")
    # NOFOLLOW closes the lstat/open symlink race where available. NONBLOCK
    # avoids hanging on a FIFO swap; fstat and identity checks also cover
    # platforms without NOFOLLOW. Parse only these bytes, never reopen a path.
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    flags |= getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_BINARY", 0)
    try:
        with os.fdopen(os.open(path, flags), "rb") as handle:
            opened = os.fstat(handle.fileno())
            current = os.lstat(path)
            if (not stat.S_ISREG(opened.st_mode) or
                    not stat.S_ISREG(current.st_mode) or
                    (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino) or
                    (current.st_dev, current.st_ino) != (opened.st_dev, opened.st_ino)):
                refuse("config changed during inspection")
            raw = handle.read(limit + 1)
            after = os.fstat(handle.fileno())
            if (opened.st_size, opened.st_mtime_ns, opened.st_ctime_ns) != (
                    after.st_size, after.st_mtime_ns, after.st_ctime_ns):
                refuse("config changed during inspection")
    except OSError:
        refuse("unreadable config")
    if len(raw) > limit:
        refuse("config exceeds 1 MiB")
    if b"\0" in raw:
        refuse("NUL in config")
    try:
        raw.decode("utf-8-sig", "strict")
    except UnicodeError:
        refuse("invalid config encoding")
    return raw

def unsafe(key):
    section, _, tail = key.partition(b".")
    option = tail.rsplit(b".", 1)[-1]
    if section in (b"include", b"includeif"):
        return True
    if key == b"core.fsmonitor":
        return True
    if section == b"filter" and option in (b"clean", b"smudge", b"process"):
        return True
    if mode != "diff-read" and section == b"diff" and option in (
            b"command", b"external", b"textconv"):
        return True
    return False

# No shell cache: every pass validates the descriptors, including optional
# config.worktree when common-dir equals git-dir and after a previous pass.
configs = [read_config(path, optional=index == 1)
           for index, path in enumerate(paths)]
env["GIT_DIR"] = os.devnull
for raw in configs:
    if raw is None:
        continue
    keys = git_output(["config", "--no-includes", "--file", "-", "--null",
                       "--name-only", "--list"], env, raw)
    if keys and not keys.endswith(b"\0"):
        refuse("invalid Git key framing")
    for key in keys.split(b"\0")[:-1]:
        if unsafe(key):
            # Subsection names and parser stderr can contain private input.
            print("unsafe Git execution config", file=sys.stderr)
            raise SystemExit(2)
PY
}

# Refuse index hints that deliberately make tracked work invisible to ordinary
# status/diff. The caller checks execution config first; fsmonitor is disabled
# here as defense in depth before reading the index.
oms_git_assert_plain_index() {  # REPO
  local root="$1"
  local listing=""

  listing="$(agent_memory_mktemp 2>/dev/null || mktemp "${TMPDIR:-/tmp}/oms-index-flags.XXXXXX")" || {
    echo "cannot inspect Git index flags" >&2
    return 2
  }
  if ! git -c core.fsmonitor=false -C "$root" ls-files -v -z > "$listing"; then
    rm -f "$listing"
    echo "cannot inspect Git index flags" >&2
    return 2
  fi
  # Records are "<tag> <name>" NUL-delimited; skip-worktree is S and
  # assume-unchanged lowercases the tag. Splitting on NUL keeps every tag at a
  # line start, so a newline inside a name can only add a spurious match,
  # never hide one. Shell rather than python3: this runs before most git reads.
  local hidden=""
  hidden="$(tr '\0' '\n' < "$listing" | LC_ALL=C grep -a -m 1 -E '^(S|[a-z])( |$)' || true)"
  rm -f "$listing"
  if [ -n "$hidden" ]; then
    hidden="${hidden#? }"
    [ -n "$hidden" ] || hidden="<unknown>"
    echo "hidden Git index flag: $hidden" >&2
    return 2
  fi
  return 0
}

# Staged-content signature of a Git index, read without running Git. Another
# session's `git status` rewrites the index with refreshed stat data and caches;
# the guard keeps path, mode, object, stage, assume-unchanged, skip-worktree,
# intent-to-add and other extensions including the cache-tree, and drops stat
# fields, the untracked/fsmonitor caches and offset tables. The text is captured when this file is sourced, before any provider
# runs, so a worker editing OMS sources cannot change what the guard executes.
# Any index this reader cannot prove (bad checksum, bounds, version) returns
# None and the caller keeps the raw byte digest.
OMS_GIT_INDEX_SIGNATURE_PY="$(cat <<'PY'
import hashlib
import struct


def index_signature(data, oid_size):
    if len(data) < 12 + oid_size or data[:4] != b"DIRC":
        return None
    end = len(data) - oid_size
    if hashlib.new("sha1" if oid_size == 20 else "sha256", data[:end]).digest() != data[end:]:
        return None
    version, count = struct.unpack(">II", data[4:12])
    if version not in (2, 3, 4):
        return None
    digest = hashlib.sha256(b"oms-index-v1\0%d\0" % version)
    at, previous = 12, b""
    for _ in range(count):
        start = at
        if at + 42 + oid_size > end:
            return None
        mode = data[at + 24:at + 28]
        oid = data[at + 40:at + 40 + oid_size]
        flags, = struct.unpack(">H", data[at + 40 + oid_size:at + 42 + oid_size])
        at += 42 + oid_size
        extended = 0
        if flags & 0x4000:
            if version < 3 or at + 2 > end:
                return None
            extended, = struct.unpack(">H", data[at:at + 2])
            at += 2
        stop = -1
        if version == 4:
            if at >= end:
                return None
            byte = data[at]
            strip = byte & 0x7F
            at += 1
            while byte & 0x80:
                # The strip count only grows: stop before a long run builds an oversized integer.
                if at >= end or strip > len(previous):
                    return None
                byte = data[at]
                strip = ((strip + 1) << 7) | (byte & 0x7F)
                at += 1
            stop = data.find(b"\0", at, end)
            if stop < 0 or strip > len(previous):
                return None
            path = previous[:len(previous) - strip] + data[at:stop]
            at = stop + 1
        else:
            stop = data.find(b"\0", at, end)
            if stop < 0:
                return None
            path = data[at:stop]
            at = start + ((stop - start + 8) // 8) * 8
            if at > end or data[stop:at].strip(b"\0"):
                return None
        previous = path
        digest.update(struct.pack(">HH", flags & 0xB000, extended) + mode + oid + path + b"\0")
    while at < end:
        if at + 8 > end:
            return None
        name = data[at:at + 4]
        size, = struct.unpack(">I", data[at + 4:at + 8])
        if at + 8 + size > end:
            return None
        # TREE stays in: a forged cache-tree would become the next commit tree of the owner.
        if name not in (b"UNTR", b"FSMN", b"EOIE", b"IEOT"):
            digest.update(name + struct.pack(">I", size) + data[at + 8:at + 8 + size])
        at += 8 + size
    return digest.hexdigest()
PY
)"

# Freeze the raw config files and index visibility bits that can change what a
# later Git capture sees or executes. This intentionally does not invoke Git:
# it is the first post-provider check, before status/diff/ls-files can consume
# worker-controlled config. OUT lives in the private delegated-worktree parent.
oms_git_execution_state_snapshot() {  # REPO OUT [config-only]
  local root="$1"
  local out="$2"
  local mode="${3:-full}"
  local common_dir=""
  local worktree_dir=""
  local index_path=""
  local paths=""
  local tmp="$out.tmp.$$"

  common_dir="$(git -C "$root" rev-parse --git-common-dir 2>/dev/null || true)"
  worktree_dir="$(git -C "$root" rev-parse --git-dir 2>/dev/null || true)"
  index_path="$(git -C "$root" rev-parse --git-path index 2>/dev/null || true)"
  common_dir="${common_dir//$'\r'/}"
  worktree_dir="${worktree_dir//$'\r'/}"
  index_path="${index_path//$'\r'/}"
  [ -n "$common_dir" ] && [ -n "$worktree_dir" ] && [ -n "$index_path" ] || return 2
  case "$common_dir" in /*|[A-Za-z]:/*) ;; *) common_dir="$root/$common_dir" ;; esac
  case "$worktree_dir" in /*|[A-Za-z]:/*) ;; *) worktree_dir="$root/$worktree_dir" ;; esac
  case "$index_path" in /*|[A-Za-z]:/*) ;; *) index_path="$root/$index_path" ;; esac
  common_dir="$(cd "$common_dir" 2>/dev/null && pwd -P)" || return 2
  worktree_dir="$(cd "$worktree_dir" 2>/dev/null && pwd -P)" || return 2
  paths="$common_dir/config
$worktree_dir/config.worktree"
  [ "$mode" = config-only ] || paths="$paths
$index_path"

  # The common config's bytes are kept beside OUT (OUT.config) so a later
  # branch-tracking delta can be read against them; they are the bytes row 0
  # hashes, and the assert re-binds them to that digest.
  rm -f "$out.config"
  # The index row records its staged content, not stat bytes `git status` refreshes.
  local oid_size=20
  [ "$(git -C "$root" rev-parse --show-object-format 2>/dev/null | tr -d '\r')" != sha256 ] || oid_size=32
  OMS_GIT_EXEC_PATHS="$paths" OMS_GIT_EXEC_COPY="$tmp.config" OMS_GIT_EXEC_INDEX="$index_path" \
    OMS_GIT_EXEC_OID="$oid_size" python3 -I -B - "$OMS_GIT_INDEX_SIGNATURE_PY" <<'PY' > "$tmp" || {
import base64
import hashlib
import os
import stat
import sys
exec(sys.argv[1])

for number, path in enumerate(os.environ["OMS_GIT_EXEC_PATHS"].splitlines()):
    if not path:
        continue
    key = base64.b64encode(os.fsencode(path)).decode("ascii")
    try:
        info = os.lstat(path)
    except FileNotFoundError:
        print("%s\tabsent" % key)
        continue
    except OSError:
        print("%s\tunreadable" % key)
        raise SystemExit(2)
    if not stat.S_ISREG(info.st_mode):
        print("%s\tnon-regular:%o" % (key, info.st_mode))
        continue
    if info.st_size > 64 * 1024 * 1024:
        print("%s\toversized:%d" % (key, info.st_size))
        raise SystemExit(2)
    if path == os.environ["OMS_GIT_EXEC_INDEX"]:
        oid = int(os.environ["OMS_GIT_EXEC_OID"])
        with open(path, "rb") as handle:
            content = index_signature(handle.read(), oid)
        if content:
            print("%s\tindex:%d:%s" % (key, oid, content))
            continue
    digest = hashlib.sha256()
    copy = None
    if number == 0:
        copy = open(os.environ["OMS_GIT_EXEC_COPY"], "wb")
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
            if copy:
                copy.write(chunk)
    if copy:
        copy.close()
    print("%s\tregular:%o:%d:%s" % (
        key, stat.S_IMODE(info.st_mode), info.st_size, digest.hexdigest()))
PY
    rm -f "$tmp" "$tmp.config"
    return 2
  }
  if [ -f "$tmp.config" ]; then
    mv "$tmp.config" "$out.config" || { rm -f "$tmp" "$tmp.config"; return 2; }
  fi
  mv "$tmp" "$out"
}

# A sibling main that creates its task branch from a remote-tracking ref writes
# branch.<name>.remote/merge into the shared config (branch.autoSetupMerge, or
# push -u). That is the only tolerated delta. Git's own parser reads it:
# configparser folds an indented "[core]" line into the previous value, Git
# does not. `git config --file --list` reads no index and runs no configured
# program, so it is safe on worker-written bytes before the execution check.
oms_git_config_delta_is_branch_tracking() {  # BEFORE_FILE AFTER_FILE
  python3 - "$1" "$2" <<'PY'
import collections, os, re, subprocess, sys
env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null")
here = os.path.dirname(os.path.abspath(sys.argv[1]))
env["GIT_CEILING_DIRECTORIES"] = os.path.dirname(here)
def rows(path):
    out = subprocess.run(["git", "config", "--file", path, "--no-includes", "--list", "-z"],
                         env=env, cwd=here, stdin=subprocess.DEVNULL,
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                         check=True).stdout
    # "key\nvalue" or a bare "key" (true): keep the separator so the two differ.
    return [tuple(r.decode("utf-8", "surrogateescape").partition("\n"))
            for r in out.split(b"\0") if r]
try:
    before, after = rows(sys.argv[1]), rows(sys.argv[2])
except (OSError, subprocess.CalledProcessError):
    raise SystemExit(1)
b, a = collections.Counter(before), collections.Counter(after)
delta = list((a - b).elements()) + list((b - a).elements())
remotes = {k[7:-4] for k, _, _ in before if k.startswith("remote.") and k.endswith(".url")}
if not delta:
    raise SystemExit(1)
for key, sep, value in delta:
    m = re.fullmatch(r"branch\.(.+)\.(remote|merge)", key)
    if not m or not sep or "\n" in value:
        raise SystemExit(1)
    if m.group(2) == "remote" and value != "." and value not in remotes:
        raise SystemExit(1)
    if m.group(2) == "merge" and not value.startswith("refs/heads/"):
        raise SystemExit(1)
PY
}

# Emit bounded, value-free execution-state diagnostics from a frozen snapshot.
oms_git_execution_state_diagnostics() {  # SNAPSHOT ROLE
  python3 -I -B - "$1" "$1.config" "$2" "$OMS_GIT_INDEX_SIGNATURE_PY" <<'PY'
import base64
import collections
import hashlib
import os
import re
import stat
import subprocess
import sys
snapshot, saved_config, role, signature_source = sys.argv[1:]
exec(signature_source)
try:
    rows = open(snapshot, "rb").read().splitlines()
except OSError:
    raise SystemExit(0)

def raw_signature(path):
    try:
        info = os.lstat(path)
    except FileNotFoundError:
        return "absent"
    except OSError:
        return "unreadable"
    if not stat.S_ISREG(info.st_mode):
        return "non-regular:%o" % info.st_mode
    if info.st_size > 64 * 1024 * 1024:
        return "oversized:%d" % info.st_size
    digest = hashlib.sha256()
    try:
        with open(path, "rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError:
        return "unreadable"
    return "regular:%o:%d:%s" % (stat.S_IMODE(info.st_mode), info.st_size, digest.hexdigest())

def config_keys(path):
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    try:
        out = subprocess.check_output(
            ["git", "config", "--file", path, "--no-includes", "--list", "-z"],
            env=env, stdin=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except (OSError, subprocess.CalledProcessError):
        return None
    return [(key, value) for item in out.split(b"\0") if item
            for key, _, value in [item.partition(b"\n")]]

for row in rows:
    try:
        encoded, expected = row.split(b"\t", 1)
        path = os.fsdecode(base64.b64decode(encoded, validate=True))
        expected = expected.decode("ascii", "strict")
    except (ValueError, UnicodeError):
        continue
    basename = os.path.basename(path)
    if expected.startswith("index:"):
        try:
            with open(path, "rb") as handle:
                current_index = "index:%d:%s" % (
                    int(expected.split(":", 2)[1]),
                    index_signature(handle.read(), int(expected.split(":", 2)[1])))
        except (OSError, ValueError):
            current_index = "unreadable"
        if current_index == expected:
            continue
        kind = "hidden index flag/index signature"
    else:
        current = raw_signature(path)
        if current == expected:
            continue
        old, new = expected.split(":"), current.split(":")
        kind = "content"
        if (len(old) == 4 and len(new) == 4 and old[0] == new[0] == "regular"
                and old[1] != new[1] and old[2:] == new[2:]):
            kind = "mode"
        if basename == "config":
            before, after = config_keys(saved_config), config_keys(path)
            if before is not None and after is not None:
                added = collections.Counter(after) - collections.Counter(before)
                removed = collections.Counter(before) - collections.Counter(after)
                delta = sorted({key.decode("ascii", "replace")
                                for key, _ in list(added.elements()) + list(removed.elements())})
                if delta:
                    safe = [key if re.fullmatch(r"[A-Za-z0-9.-]+", key) else "[unusual key]"
                            for key in delta[:8]]
                    kind = "config key delta=" + ",".join(safe)
                    if len(delta) > 8:
                        kind += ",additional keys omitted"
    name = basename if re.fullmatch(r"[A-Za-z0-9._-]+", basename) else "[unusual path]"
    print("snapshot=%s path=%s kind=%s" % (role, name, kind))
PY
}

oms_git_execution_state_report_failure() {  # SNAPSHOT ROLE [DETAIL_FILE]
  local details=""
  details="$(oms_git_execution_state_diagnostics "$1" "$2" 2>/dev/null || true)"
  echo "Git execution config or hidden index state changed" >&2
  if [ -n "$details" ]; then
    printf '%s\n' "$details" >&2
    [ -z "${3:-}" ] || printf '%s\n' "$details" > "$3"
  fi
}

# Compare with the same no-Git raw walk and bind the private snapshot bytes to
# the hash held by the parent shell. A worker that finds and edits SNAPSHOT
# cannot manufacture a new trusted baseline.
oms_git_execution_state_assert_unchanged() {  # REPO SNAPSHOT EXPECTED_SHA [ROLE [DETAIL_FILE]]
  local root="$1"
  local snapshot="$2"
  local expected_sha="$3"
  local actual_sha=""
  local changed_config=""
  local role="${4:-repository}"
  local detail_file="${5:-}"

  # Root is part of the public signature for callers and documents which
  # repository the receipt belongs to. The trusted snapshot contains the
  # already-resolved physical paths, so the post-worker path never asks Git to
  # resolve them again.
  [ -n "$root" ] || return 2

  actual_sha="$(oms_sha256_file "$snapshot" 2>/dev/null || true)"
  if [ -z "$actual_sha" ] || [ "$actual_sha" != "$expected_sha" ]; then
    echo "Git execution-state snapshot changed or became unreadable" >&2
    return 2
  fi
  if ! changed_config="$(python3 -I -B - "$snapshot" "$snapshot.config" "$OMS_GIT_INDEX_SIGNATURE_PY" <<'PY'
import base64
import hashlib
import os
import stat
import sys
exec(sys.argv[3])

def staged(path, oid):
    try:
        info = os.lstat(path)
        if not stat.S_ISREG(info.st_mode) or info.st_size > 64 * 1024 * 1024:
            return None
        with open(path, "rb") as handle:
            content = index_signature(handle.read(), oid)
    except OSError:
        return None
    return content and "index:%d:%s" % (oid, content)

def signature(path):
    try:
        info = os.lstat(path)
    except FileNotFoundError:
        return "absent"
    except OSError:
        return "unreadable"
    if not stat.S_ISREG(info.st_mode):
        return "non-regular:%o" % info.st_mode
    if info.st_size > 64 * 1024 * 1024:
        return "oversized:%d" % info.st_size
    digest = hashlib.sha256()
    try:
        with open(path, "rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError:
        return "unreadable"
    return "regular:%o:%d:%s" % (
        stat.S_IMODE(info.st_mode), info.st_size, digest.hexdigest())

try:
    rows = open(sys.argv[1], "rb").read().splitlines()
except OSError:
    raise SystemExit(2)
# Row 0 is the common config in both snapshot modes. It alone may change in
# content, and only against the saved copy that its recorded digest binds.
for number, row in enumerate(rows):
    try:
        encoded, expected = row.split(b"\t", 1)
        path = os.fsdecode(base64.b64decode(encoded, validate=True))
        expected = expected.decode("ascii", "strict")
    except (ValueError, UnicodeError):
        raise SystemExit(2)
    if not os.path.isabs(path):
        raise SystemExit(2)
    if expected.startswith("index:"):
        if staged(path, int(expected.split(":")[1])) == expected:
            continue
        raise SystemExit(2)
    current = signature(path)
    if current == expected:
        continue
    old, new = expected.split(":"), current.split(":")
    if number != 0 or len(old) != 4 or len(new) != 4 or old[0] != "regular" or old[:2] != new[:2]:
        raise SystemExit(2)
    if signature(sys.argv[2]).split(":")[2:] != old[2:]:
        raise SystemExit(2)
    sys.stdout.write(path)
PY
)"
  then
    oms_git_execution_state_report_failure "$snapshot" "$role" "$detail_file"
    return 2
  fi
  if [ -n "$changed_config" ] &&
    ! oms_git_config_delta_is_branch_tracking "$snapshot.config" "$changed_config"; then
    oms_git_execution_state_report_failure "$snapshot" "$role" "$detail_file"
    return 2
  fi
  return 0
}

# Content fingerprint of the index and tracked worktree bytes without invoking
# diff machinery. Git diff may run clean filters even with --no-ext-diff and
# --no-textconv; ls-files plus direct byte reads has no filter/attribute command
# path and is safe for refusal/recovery bookkeeping.
oms_git_tracked_state_fingerprint() {  # REPO
  local root="$1"

  python3 - "$root" <<'PY'
import hashlib
import os
import stat
import subprocess
import sys

root = sys.argv[1]
root_b = os.fsencode(root)
base = ["git", "-c", "core.fsmonitor=false", "-C", root]
try:
    index = subprocess.check_output(base + ["ls-files", "--stage", "-z"],
                                    stderr=subprocess.DEVNULL)
    tracked = subprocess.check_output(base + ["ls-files", "-z"],
                                      stderr=subprocess.DEVNULL)
except (OSError, subprocess.CalledProcessError):
    raise SystemExit(2)

h = hashlib.sha256()
h.update(b"index\0" + index)
for rel in sorted(value for value in tracked.split(b"\0") if value):
    h.update(b"\0path\0" + hashlib.sha256(rel).digest())
    path = os.path.join(root_b, rel)
    try:
        info = os.lstat(path)
    except OSError:
        h.update(b"missing\0")
        continue
    h.update(("mode:%o\0" % info.st_mode).encode())
    if stat.S_ISLNK(info.st_mode):
        h.update(b"link\0" + os.fsencode(os.readlink(path)))
    elif stat.S_ISREG(info.st_mode):
        h.update(b"file\0")
        try:
            with open(path, "rb") as handle:
                for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                    h.update(chunk)
        except OSError:
            h.update(b"unreadable\0")
    else:
        h.update(("other:%d:%d\0" % (info.st_size,
                  getattr(info, "st_mtime_ns",
                          int(info.st_mtime * 1_000_000_000)))).encode())
print(h.hexdigest())
PY
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
  head="$(git -c core.fsmonitor=false -C "$root" rev-parse HEAD 2>/dev/null || printf 'unborn')"
  diff_hash="$(oms_git_tracked_state_fingerprint "$root" 2>/dev/null ||
    printf 'unreadable')"
  untracked_hash="$(python3 - "$root" <<'PY'
import hashlib, os, subprocess, sys
root = sys.argv[1]
paths = subprocess.check_output(
    ["git", "-c", "core.fsmonitor=false", "-C", root,
     "ls-files", "-z", "--others", "--exclude-standard"])
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
#
# Each surface is captured on its own (`oms_worker_surface_snapshot`) rather
# than folded into one aggregate digest, so a violation can name what moved
# instead of reporting that some hash differs.

# Compare recorded shared state against the current tree. Unlike the other
# surfaces this is not an equality check: appending is allowed, so only a
# changed prefix, a shrunk file, or a disappearance counts.
oms_worker_state_violations() {
  local repo="$1"
  local before="$2"

  [ -f "$before" ] || return 0
  OMS_WG_REPO="$repo" OMS_WG_BEFORE="$before" python3 - <<'PY'
import hashlib, json, os, re, stat

root = os.path.join(os.environ["OMS_WG_REPO"], ".oms")
problems = []

# The artifact index store compacts by design (artifact-index-store.py keeps the
# newest lineage groups once the row count passes its high-water mark), so a
# shrink or changed prefix there is accepted only when it has that shape: every
# surviving row is byte-identical and in order, new rows follow them, all
# dropped rows are older than the oldest survivor that is not a retained
# resolution target, and the row count had passed the keep floor.
def index_compacted(path, size, expected):
    try:
        with open(os.environ["OMS_WG_BEFORE"] + ".artifact-index", "rb") as fh:
            before = fh.read(size)
        with open(path, "rb") as fh:
            now = fh.read()
    except OSError:
        return False
    if len(before) != size or hashlib.sha256(before).hexdigest() != expected:
        return False

    def rows(data):
        out = []
        for line in data.splitlines(True):
            try:
                row = json.loads(line)
            except ValueError:
                return None
            event_id = row.get("event_id") if isinstance(row, dict) else None
            if not isinstance(event_id, str) or not event_id:
                return None
            out.append((event_id, line, row))
        return out

    old, new = rows(before), rows(now)
    if old is None or new is None:
        return False
    position = {event_id: i for i, (event_id, _line, _row) in enumerate(old)}
    if len(position) != len(old):
        return False
    retained, added = [], 0
    for event_id, line, _row in new:
        i = position.get(event_id)
        if i is None:
            added += 1
            continue
        if added or old[i][1] != line or (retained and i <= retained[-1]):
            return False
        retained.append(i)
    dropped = set(range(len(old))) - set(retained)
    if not dropped:
        return False
    try:
        keep = int(os.environ.get("OMS_ARTIFACT_INDEX_KEEP", "1000"))
    except ValueError:
        keep = 1000
    if len(old) + added <= keep:
        return False
    targets = {
        position.get(row.get("resolves_event_id"))
        for _event_id, _line, row in new
        if row.get("kind") == "artifact-resolution"
    }
    # The store also keeps rows pinned by plan completion receipts. Only pins recorded with the
    # baseline count, so a receipt written during the run cannot excuse a deletion.
    try:
        with open(os.environ["OMS_WG_BEFORE"] + ".completion-pins", encoding="utf-8") as handle:
            pins = {pin for pin in json.load(handle) if isinstance(pin, str)}
    except (OSError, ValueError, TypeError):
        pins = set()
    anchors = [i for i in retained if i not in targets and old[i][0] not in pins]
    return max(dropped) < min(anchors, default=len(old))

with open(os.environ["OMS_WG_BEFORE"], encoding="utf-8", errors="replace") as f:
    for line in f:
        parts = line.rstrip("\n").split(" ")
        if len(parts) < 2:
            continue
        rel, kind = parts[0], parts[1]
        path = os.path.join(root, rel)
        if not os.path.exists(path):
            if (kind == "CACHE" and os.environ.get("OMS_WORKER_GUARD_STRICT", "0") != "1"
                    and re.fullmatch(r"project-graph/cache/[0-9a-f]{64}\.json", rel)):
                if not os.path.lexists(path):
                    continue
            problems.append("%s was deleted" % rel)
            continue
        if kind == "CACHE":
            try:
                flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
                with os.fdopen(os.open(path, flags), "rb") as handle:
                    info = os.fstat(handle.fileno())
                    if not stat.S_ISREG(info.st_mode):
                        problems.append("%s derived cache changed" % rel)
                        continue
                    digest = hashlib.sha256()
                    for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                        digest.update(chunk)
                if digest.hexdigest() != parts[3] or info.st_size != int(parts[2]):
                    problems.append("%s derived cache changed" % rel)
            except (OSError, ValueError, IndexError):
                problems.append("%s derived cache became unreadable" % rel)
            continue
        if kind == "IMMUTABLE" and len(parts) == 4:
            try:
                size = int(parts[2])
                flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
                with os.fdopen(os.open(path, flags), "rb") as handle:
                    info = os.fstat(handle.fileno())
                    if os.path.islink(path) or not stat.S_ISREG(info.st_mode) or info.st_size != size:
                        problems.append("%s controller record changed" % rel)
                        continue
                    digest, remaining = hashlib.sha256(), size
                    while remaining > 0:
                        chunk = handle.read(min(1024 * 1024, remaining))
                        if not chunk:
                            break
                        remaining -= len(chunk)
                        digest.update(chunk)
                    changed = remaining != 0 or bool(handle.read(1)) or digest.hexdigest() != parts[3]
                if changed:
                    problems.append("%s controller record changed" % rel)
            except (OSError, ValueError):
                problems.append("%s controller record became unreadable" % rel)
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
        compactable = rel == "artifacts/index.jsonl"
        if now < size:
            if not (compactable and index_compacted(path, size, expected)):
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
        if digest.hexdigest() != expected and not (
                compactable and index_compacted(path, size, expected)):
            problems.append("%s had existing rows rewritten" % rel)
for problem in problems[:5]:
    print(problem)
PY
}

# Freeze the selected plan task, not the shared plan file: sibling tasks may
# legitimately change while the provider runs. Compare this lease-bound authority
# before publishing a review receipt or landing.
oms_worker_operation_snapshot() {  # REPO OUT PLAN_TASK LEASE
  local repo="$1"
  local out="$2"
  local plan_task="${3:-}"
  local lease="${4:-}"
  if [ -n "${5:-}${6:-}" ]; then
    echo "error: Soul executor snapshots are retired" >&2
    return 1
  fi
  local tmp="$out.tmp.$$"

  OMS_WG_REPO="$repo" OMS_WG_PLAN_TASK="$plan_task" OMS_WG_LEASE="$lease" \
    python3 - <<'PY' > "$tmp" || { rm -f "$tmp"; return 1; }
import json, os, sys

repo = os.environ["OMS_WG_REPO"]
task_id = os.environ["OMS_WG_PLAN_TASK"]
lease = os.environ["OMS_WG_LEASE"]

def load_regular(path, label):
    if os.path.islink(path) or not os.path.isfile(path):
        raise SystemExit("error: %s authority is not a regular file" % label)
    try:
        with open(path, encoding="utf-8") as handle:
            value = json.load(handle)
    except (OSError, TypeError, ValueError) as exc:
        raise SystemExit("error: cannot read %s authority: %s" % (label, exc))
    if not isinstance(value, dict):
        raise SystemExit("error: %s authority is not an object" % label)
    return value

# Retain the empty legacy field so historical bindings are never reinterpreted.
snapshot = {"schema": 2, "plan": None, "executor": None}
if task_id:
    plan = load_regular(os.path.join(repo, ".oms", "plan", "tasks.json"), "plan")
    tasks = plan.get("tasks")
    task = tasks.get(task_id) if isinstance(tasks, dict) else None
    if not isinstance(task, dict):
        raise SystemExit("error: current plan task %s is missing" % task_id)
    if not lease or task.get("lease_id") != lease:
        raise SystemExit("error: current plan task %s lease changed before provider start" % task_id)
    if task.get("executor_id") or task.get("executor_soul_sha256"):
        raise SystemExit("error: Soul executor receipt is retired")
    snapshot["plan"] = {"task_id": task_id, "lease_id": lease, "task": task}
elif lease:
    raise SystemExit("error: current operation has a lease without a plan task")

json.dump(snapshot, sys.stdout, ensure_ascii=False, sort_keys=True,
          separators=(",", ":"))
sys.stdout.write("\n")
PY
  mv "$tmp" "$out"
}

oms_worker_operation_violations() {  # REPO SNAPSHOT
  local repo="$1"
  local snapshot="$2"

  [ -f "$snapshot" ] || return 0
  OMS_WG_REPO="$repo" OMS_WG_OPERATION_SNAPSHOT="$snapshot" python3 - <<'PY'
import datetime, json, os

repo = os.environ["OMS_WG_REPO"]
snapshot_path = os.environ["OMS_WG_OPERATION_SNAPSHOT"]

def load_regular(path):
    if os.path.islink(path) or not os.path.isfile(path):
        raise OSError("not a regular file")
    with open(path, encoding="utf-8") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ValueError("not an object")
    return value

def changed_fields(before, after):
    if not isinstance(before, dict) or not isinstance(after, dict):
        return ["object-shape"]
    return sorted(key for key in set(before) | set(after)
                  if key not in before or key not in after or before[key] != after[key])

def parse_timestamp(value):
    if not isinstance(value, str):
        return None
    try:
        parsed = datetime.datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        return None
    if parsed.strftime("%Y-%m-%dT%H:%M:%SZ") != value:
        return None
    return parsed.replace(tzinfo=datetime.timezone.utc)

def is_touch_heartbeat(before, after, task_id, lease, fields):
    if not fields or not set(fields).issubset({"claimed_at", "updated"}):
        return False
    if before.get("id") != task_id or after.get("id") != task_id:
        return False
    provider = before.get("provider")
    if not isinstance(provider, str) or not provider or after.get("provider") != provider:
        return False
    if not isinstance(lease, str) or not lease:
        return False
    if before.get("lease_id") != lease or after.get("lease_id") != lease:
        return False
    state = before.get("state")
    if state not in ("claimed", "running") or after.get("state") != state:
        return False

    heartbeat = parse_timestamp(after.get("claimed_at"))
    updated = parse_timestamp(after.get("updated"))
    now = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0)
    if heartbeat is None or updated is None or heartbeat != updated or heartbeat > now:
        return False
    for key in ("claimed_at", "updated"):
        prior = before.get(key)
        if prior in (None, ""):
            continue
        prior_timestamp = parse_timestamp(prior)
        if prior_timestamp is None or heartbeat < prior_timestamp:
            return False
    return True

try:
    expected = load_regular(snapshot_path)
except (OSError, TypeError, ValueError):
    print("current operation snapshot became unreadable")
    raise SystemExit(0)

plan_expected = expected.get("plan")
if isinstance(plan_expected, dict):
    task_id = plan_expected.get("task_id", "")
    plan_path = os.path.join(repo, ".oms", "plan", "tasks.json")
    try:
        plan = load_regular(plan_path)
        tasks = plan.get("tasks")
        current = tasks.get(task_id) if isinstance(tasks, dict) else None
        if not isinstance(current, dict):
            raise KeyError(task_id)
    except (OSError, TypeError, ValueError, KeyError):
        print("plan task %s was deleted or became unreadable" % task_id)
    else:
        expected_task = plan_expected.get("task")
        fields = changed_fields(expected_task, current)
        lease = plan_expected.get("lease_id")
        if fields and is_touch_heartbeat(expected_task, current, task_id, lease, fields):
            fields = []
        if fields:
            detail = ", ".join(fields)
            if set(fields).issubset({"claimed_at", "updated"}):
                detail += " (not a valid same-lease touch heartbeat)"
            print("plan task %s changed: %s" % (task_id, detail))
        elif current.get("lease_id") != lease:
            print("plan task %s lease no longer matches its operation fence" % task_id)

if expected.get("executor"):
    print("Soul executor snapshot is retired; preserve it for inspection")
PY
}

# Repair the operation's own frozen authority after a violation, using only
# the snapshot this process hashed before the provider started. The
# discrimination is field-scoped, not clock-scoped: the claim-cycle fields
# (state/provider/ttl/claimed_at/reason/lease_id/lease_epoch) are the only
# surface a legitimate writer can move mid-run — an operator block, a
# heartbeat, a release/reclaim handoff — so under a changed lease they are
# kept and named rather than guessed. Authority and evidence fields have no
# legitimate mid-run writer under any lease, and reclaim preserves them, so a
# forged claim row would otherwise carry a weakened verifier or scope into
# the next legitimate claim. Those always restore. The run still fails either
# way: this repairs owner state, it never admits the worker's output.
OMS_COMMON_LIB_DIR="$(cd "${BASH_SOURCE[0]%/*}" && pwd)"
oms_worker_operation_restore() {  # REPO SNAPSHOT EXPECTED_SHA
  local repo="$1"
  local snapshot="$2"
  local expected_sha="${3:-}"
  local actual_sha=""
  local plan_file="$repo/.oms/plan/tasks.json"

  # The snapshot sits in the worker-writable worktree parent. Bytes that no
  # longer match the pre-launch hash are the worker's; restoring them would
  # install attacker authority under the owner's own lock.
  actual_sha="$(oms_sha256_file "$snapshot" 2>/dev/null || true)"
  if [ -z "$expected_sha" ] || [ -z "$actual_sha" ] ||
    [ "$actual_sha" != "$expected_sha" ]; then
    echo "restore refused: current-operation snapshot is untrusted (changed or deleted); plan state left for inspection"
    return 1
  fi
  if ! command -v oms_with_file_lock >/dev/null 2>&1; then
    # shellcheck source=scripts/lib/file-lock.sh
    . "$OMS_COMMON_LIB_DIR/file-lock.sh"
  fi
  oms_with_file_lock "$plan_file" oms_worker_operation_restore_plan \
    "$repo" "$snapshot"
}

oms_worker_operation_restore_plan() {  # REPO SNAPSHOT (under the plan file lock)
  local repo="$1"
  local snapshot="$2"

  OMS_WG_REPO="$repo" OMS_WG_OPERATION_SNAPSHOT="$snapshot" python3 - <<'PY'
import datetime, json, os, tempfile

repo = os.environ["OMS_WG_REPO"]
snapshot_path = os.environ["OMS_WG_OPERATION_SNAPSHOT"]
ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

# The fields a legitimate writer can move while this operation's worker runs.
# Everything outside this set is authority or evidence.
CLAIM_FIELDS = {"state", "provider", "ttl", "claimed_at", "reason",
                "lease_id", "lease_epoch", "updated"}

def refuse(msg):
    print(msg)
    raise SystemExit(1)

with open(snapshot_path, encoding="utf-8") as fh:
    expected = json.load(fh)
if expected.get("executor"):
    refuse("restore refused: Soul executor snapshot is retired; state left for inspection")
plan_expected = expected.get("plan")
if not isinstance(plan_expected, dict):
    raise SystemExit(0)
task_id = plan_expected.get("task_id", "")
expected_task = plan_expected.get("task")
if not task_id or not isinstance(expected_task, dict):
    refuse("restore refused: snapshot plan entry is malformed; plan state left for inspection")

plan_file = os.path.join(repo, ".oms", "plan", "tasks.json")
try:
    if os.path.islink(plan_file) or not os.path.isfile(plan_file):
        raise OSError("not a regular file")
    with open(plan_file, encoding="utf-8") as fh:
        plan = json.load(fh)
    tasks = plan.get("tasks") if isinstance(plan, dict) else None
    if not isinstance(tasks, dict):
        raise ValueError("not an object")
except (OSError, TypeError, ValueError):
    # Only this task's object is snapshotted; a destroyed plan file cannot be
    # rebuilt from it without fabricating the rest of the plan.
    refuse("restore refused: plan file unreadable; plan task %s left for inspection" % task_id)

def parses_ts(value):
    try:
        datetime.datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ")
        return True
    except Exception:
        return False

MISSING = object()
current = tasks.get(task_id)
restored = []
if not isinstance(current, dict):
    # No verb deletes a task; absence is always the worker.
    tasks[task_id] = dict(expected_task)
    restored = ["all fields (task was deleted)"]
else:
    same_lease = current.get("lease_id", "") == expected_task.get("lease_id", "")
    for key in sorted(set(expected_task) | set(current)):
        if key in CLAIM_FIELDS:
            continue
        if current.get(key, MISSING) == expected_task.get(key, MISSING):
            continue
        if key in expected_task:
            current[key] = expected_task[key]
        else:
            current.pop(key, None)
        restored.append(key)
    if same_lease:
        blocked = current.get("state") == "blocked"
        kept_heartbeat = False
        for key in sorted(CLAIM_FIELDS - {"updated"}):
            if current.get(key, MISSING) == expected_task.get(key, MISSING):
                continue
            if blocked and key in ("state", "reason"):
                continue
            if key == "claimed_at" and isinstance(current.get(key), str) \
                    and parses_ts(current[key]):
                kept_heartbeat = True
                continue
            if key in expected_task:
                current[key] = expected_task[key]
            else:
                current.pop(key, None)
            restored.append(key)
        if blocked and (current.get("state", MISSING) != expected_task.get("state", MISSING)
                        or current.get("reason", MISSING) != expected_task.get("reason", MISSING)):
            print("kept: plan task %s operator block (state, reason)" % task_id)
        if kept_heartbeat:
            print("kept: plan task %s claimed_at heartbeat" % task_id)
    else:
        moved = sorted(key for key in CLAIM_FIELDS - {"updated"}
                       if current.get(key, MISSING) != expected_task.get(key, MISSING))
        if moved:
            print("kept: plan task %s claim fields under a changed lease: %s "
                  "(a legitimate new owner may exist; inspect plan state)"
                  % (task_id, ", ".join(moved)))

if restored:
    row = tasks[task_id]
    row["updated"] = ts
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(plan_file))
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(plan, fh, ensure_ascii=False, indent=2)
        os.replace(tmp, plan_file)
    except Exception:
        os.unlink(tmp)
        raise
    print("restored: plan task %s fields: %s" % (task_id, ", ".join(restored)))
PY
}

oms_worker_surface_diff() {
  local repo="$1"
  local before_dir="$2"
  local operation_snapshot_sha="${3:-}"
  local current_worktree_physical="${4:-}"
  local changed=""
  local name
  local common

  : > "$before_dir/gitmeta-managed-added"
  for name in config remotes refs remote-refs work-branches tracked files gitmeta hooks; do
    [ -f "$before_dir/$name" ] || continue
    if ! OMS_WG_GITMETA_MANAGED_ADDED_OUT="$before_dir/gitmeta-managed-added" \
      oms_worker_surface_capture_one "$repo" "$name" \
      "$current_worktree_physical" | cmp -s - "$before_dir/$name"; then
      if [ "$name" = gitmeta ] && oms_worker_gitmeta_settles "$repo" \
        "$before_dir/gitmeta" "$current_worktree_physical"; then
        continue
      fi
      if [ "$name" = gitmeta ] && ! oms_worker_gitmeta_admin_changes_are_safe "$repo" \
        "$before_dir" "$current_worktree_physical"; then
        changed="${changed:+$changed, }gitmeta"
        continue
      fi
      if [ "$name" = gitmeta ] && [ "${OMS_WORKER_GUARD_STRICT:-0}" != 1 ] &&
        oms_worker_gitmeta_only_added_removed_siblings "$repo" \
        "$before_dir" "$current_worktree_physical"; then
        changed="${changed:+$changed, }$OMS_WG_GITMETA_SOFT"
        continue
      fi
      # Strict mode rejects classified soft changes, but preserves this existing
      # quiet exemption for validated live managed-worktree lifecycle activity.
      if [ "$name" = gitmeta ] && [ "$OMS_WG_GITMETA_ADMIN_QUIET" = 1 ] &&
        [ "${OMS_WG_GITMETA_PARTIAL_REMOVE_STUCK:-0}" != 1 ]; then
        continue
      fi
      if [ "$name" = gitmeta ] && oms_worker_gitmeta_only_kept_siblings "$repo" \
        "$before_dir" "$current_worktree_physical"; then
        changed="${changed:+$changed, }$OMS_WG_GITMETA_SOFT"
        continue
      fi
      if [ "${OMS_WORKER_GUARD_STRICT:-0}" != 1 ] &&
        [ "$name" = gitmeta ] && oms_worker_gitmeta_only_foreign_checkout_heads "$repo" \
        "$before_dir" "$current_worktree_physical"; then
        changed="${changed:+$changed, }foreign-ref"
        continue
      fi
      if [ "${OMS_WORKER_GUARD_STRICT:-0}" != 1 ] &&
        [ "$name" = gitmeta ] && oms_worker_gitmeta_only_foreign_worktrees "$repo" \
        "$before_dir" "$current_worktree_physical"; then
        changed="${changed:+$changed, }foreign-worktree"
        continue
      fi
      if [ "${OMS_WORKER_GUARD_STRICT:-0}" != 1 ] &&
        [ "$name" = refs ] && oms_worker_refs_only_foreign "$repo" \
        "$before_dir" "$current_worktree_physical"; then
        changed="${changed:+$changed, }foreign-ref"
        continue
      fi
      # Callers pass the execution-state assert first, which binds this copy.
      if [ "$name" = config ] && [ -f "$before_dir/git-execution-state.config" ] &&
        common="$(oms_harness_git_path_physical "$repo" --git-common-dir 2>/dev/null)" &&
        [ -n "$common" ] && oms_git_config_delta_is_branch_tracking \
          "$before_dir/git-execution-state.config" "$common/config"; then
        changed="${changed:+$changed, }branch-tracking"
        continue
      fi
      changed="${changed:+$changed, }$name"
    fi
  done
  # Shared state is compared by contract, not by equality: see above. The detail
  # goes to a file because this function runs in a command substitution, where
  # an exported variable would die with the subshell.
  oms_worker_state_violations "$repo" "$before_dir/omsstate" > "$before_dir/state-detail"
  [ ! -s "$before_dir/state-detail" ] || changed="${changed:+$changed, }shared-state"
  if [ -n "$operation_snapshot_sha" ]; then
    if [ ! -f "$before_dir/current-operation.json" ] ||
      [ "$(oms_sha256_file "$before_dir/current-operation.json" 2>/dev/null || true)" != "$operation_snapshot_sha" ]; then
      printf 'current operation snapshot was changed or deleted\n' \
        > "$before_dir/current-operation-detail"
    else
      oms_worker_operation_violations "$repo" "$before_dir/current-operation.json" \
        > "$before_dir/current-operation-detail"
    fi
    [ ! -s "$before_dir/current-operation-detail" ] ||
      changed="${changed:+$changed, }current-operation"
  fi
  printf '%s\n' "$changed"
}

# A marker records the checkout that made it; land or scratch may run from a
# linked worktree of the same repository. Same repository means the same Git
# common directory, not the same checkout path.
oms_worker_same_repository() {  # REPO_PHYSICAL OTHER
  local other_physical=""
  local repo_common=""
  local other_common=""

  other_physical="$(oms_harness_physical_dir "$2" 2>/dev/null || true)"
  [ -n "$other_physical" ] || return 1
  [ "$other_physical" != "$1" ] || return 0
  repo_common="$(oms_harness_git_path_physical "$1" --git-common-dir 2>/dev/null || true)"
  other_common="$(oms_harness_git_path_physical "$other_physical" --git-common-dir 2>/dev/null || true)"
  [ -n "$repo_common" ] && [ "$repo_common" = "$other_common" ]
}

# PID 0 probes the caller's process group. Lexical validation preserves decimal
# padding without integer overflow, including before the kept-marker bypass.
oms_worker_marker_pid_is_positive_decimal() {  # PID
  case "$1" in ""|*[!0-9]*) return 1 ;; esac
  case "$1" in *[1-9]*) return 0 ;; esac
  return 1
}

# A parallel delegate owns a temporary linked worktree in the same repository.
# Its add/remove is harness lifecycle, not a provider reaching into Git
# metadata. Ignore only registrations backed by a live private marker whose
# physical repo/worktree pair passes the residue validator and whose metadata
# directory is that worktree's real Git backpointer. The current worker's own
# registration is never ignored: a commit there moves its detached HEAD and
# otherwise collapses into an empty successful patch.
oms_worker_gitmeta_is_live_managed_worktree() {  # REPO ENTRY CURRENT_WORKTREE [kept]
  local repo="$1"
  local entry="$2"
  local current_worktree_physical="${3:-}"
  local allow_kept="${4:-}"
  local metadata_dir
  local metadata_physical=""
  local gitdir_file
  local gitfile=""
  local wt_path=""
  local wt_physical=""
  local worktree_git_dir=""
  local residue_dir=""
  local marker=""
  local marker_kind=""
  local marker_pid=""
  local marker_repo=""
  local marker_worktree=""
  local marker_temporary=""
  local repo_physical=""
  local base=""

  command -v oms_harness_safe_residue_worktree >/dev/null 2>&1 || return 1
  command -v oms_harness_temp_bases >/dev/null 2>&1 || return 1
  metadata_dir="$(dirname "$entry")"
  gitdir_file="$metadata_dir/gitdir"
  [ -f "$gitdir_file" ] && [ ! -L "$gitdir_file" ] || return 1
  gitfile="$(sed -n '1p' "$gitdir_file" 2>/dev/null || true)"
  gitfile="${gitfile//$'\r'/}"
  case "$gitfile" in
    */.git) wt_path="${gitfile%/.git}" ;;
    *) return 1 ;;
  esac
  wt_physical="$(oms_harness_physical_dir "$wt_path" 2>/dev/null || true)"
  if [ -n "$wt_physical" ] && [ -n "$current_worktree_physical" ] &&
    [ "$wt_physical" = "$current_worktree_physical" ]; then
    return 1
  fi
  if [ -z "$wt_physical" ] || [ ! -f "$wt_physical/.git" ] || [ -L "$wt_physical/.git" ]; then
    oms_worker_gitmeta_is_pending_managed_worktree "$repo" "$wt_path" \
      "$current_worktree_physical"
    return $?
  fi
  metadata_physical="$(oms_harness_physical_dir "$metadata_dir" 2>/dev/null || true)"
  [ -n "$metadata_physical" ] || return 1
  worktree_git_dir="$(
    oms_harness_git_path_physical "$wt_physical" --git-dir 2>/dev/null || true
  )"
  [ -n "$worktree_git_dir" ] &&
    [ "$metadata_physical" = "$worktree_git_dir" ] || return 1
  residue_dir="$(dirname "$wt_physical")"
  marker="$residue_dir/.oh-my-setting-tmp"
  [ -f "$marker" ] && [ ! -L "$marker" ] || return 1
  marker_kind="$(oms_harness_read_marker_value "$marker" kind)"
  marker_pid="$(oms_harness_read_marker_value "$marker" pid)"
  marker_repo="$(oms_harness_read_marker_value "$marker" repo)"
  marker_worktree="$(oms_harness_read_marker_value "$marker" worktree)"
  marker_temporary="$(oms_harness_read_marker_value "$marker" temporary)"
  [ "$marker_kind" = oh-my-setting-temp ] && [ "$marker_temporary" = 1 ] || return 1
  oms_worker_marker_pid_is_positive_decimal "$marker_pid" || return 1
  [ "$allow_kept" = kept ] || kill -0 "$marker_pid" 2>/dev/null || return 1
  repo_physical="$(oms_harness_physical_dir "$repo" 2>/dev/null || true)"
  [ -n "$repo_physical" ] || return 1

  while IFS= read -r base; do
    [ -n "$base" ] || continue
    if oms_harness_safe_residue_worktree "$base" "$residue_dir" \
        "$marker_repo" "$marker_worktree" &&
      oms_worker_same_repository "$repo_physical" "$OMS_HARNESS_SAFE_RESIDUE_REPO" &&
      [ "$OMS_HARNESS_SAFE_RESIDUE_WORKTREE" = "$wt_physical" ]; then
      return 0
    fi
  done <<EOF
$(oms_harness_temp_bases)
EOF
  return 1
}

# A sibling's registration passes through two states where its checkout cannot
# vouch for it: `git worktree add` registers the entry before the checkout and
# its `.git` backpointer exist (the residue marker is written before the add),
# and `git worktree remove` deletes the checkout before the entry. A capture
# inside either window must not turn harness lifecycle into a metadata
# violation, so an entry without a usable checkout is exempt only while the
# residue marker beside it is live: valid kind, a running pid, this repository,
# the pathname the entry points at, and a residue directory that is a direct
# child of a harness temp base. Every capture re-evaluates this, so a marker
# that dies -- or an entry whose checkout exists but fails the strict validator
# above -- stays on the hard surface. The current worker's own residue directory
# is never exempt.
oms_worker_gitmeta_is_pending_managed_worktree() {  # REPO WT_PATH CURRENT_WORKTREE
  local repo="$1"
  local wt_path="$2"
  local current_worktree_physical="${3:-}"
  local residue_dir=""
  local residue_physical=""
  local residue_parent=""
  local marker=""
  local marker_kind=""
  local marker_pid=""
  local marker_repo=""
  local marker_worktree=""
  local marker_temporary=""
  local marker_parent=""
  local repo_physical=""
  local base=""
  local base_physical=""

  command -v oms_harness_temp_bases >/dev/null 2>&1 || return 1
  command -v oms_harness_read_marker_value >/dev/null 2>&1 || return 1
  # Antigravity read passes register <base>/tree; every other harness tree is <base>/wt.
  case "$wt_path" in /*/wt|/*/tree) ;; *) return 1 ;; esac
  residue_dir="${wt_path%/*}"
  case "$(basename "$residue_dir")/${wt_path##*/}" in
    oh-my-setting-delegate.*/wt|oh-my-setting-admit.*/wt|oh-my-setting-scratch.*/wt|oh-my-setting-land.*/wt) ;;
    oh-my-setting-agy-read.*/tree) ;;
    *) return 1 ;;
  esac
  residue_physical="$(oms_harness_physical_dir "$residue_dir" 2>/dev/null || true)"
  [ -n "$residue_physical" ] || return 1
  [ -z "$current_worktree_physical" ] ||
    [ "$residue_physical" != "$(dirname "$current_worktree_physical")" ] || return 1
  marker="$residue_dir/.oh-my-setting-tmp"
  [ -f "$marker" ] && [ ! -L "$marker" ] || return 1
  marker_kind="$(oms_harness_read_marker_value "$marker" kind)"
  marker_pid="$(oms_harness_read_marker_value "$marker" pid)"
  marker_repo="$(oms_harness_read_marker_value "$marker" repo)"
  marker_worktree="$(oms_harness_read_marker_value "$marker" worktree)"
  marker_temporary="$(oms_harness_read_marker_value "$marker" temporary)"
  [ "$marker_kind" = oh-my-setting-temp ] && [ "$marker_temporary" = 1 ] || return 1
  oms_worker_marker_pid_is_positive_decimal "$marker_pid" || return 1
  kill -0 "$marker_pid" 2>/dev/null || return 1
  repo_physical="$(oms_harness_physical_dir "$repo" 2>/dev/null || true)"
  [ -n "$repo_physical" ] || return 1
  oms_worker_same_repository "$repo_physical" "$marker_repo" || return 1
  [ "${marker_worktree##*/}" = "${wt_path##*/}" ] || return 1
  marker_parent="$(oms_harness_physical_dir "${marker_worktree%/*}" 2>/dev/null || true)"
  [ -n "$marker_parent" ] && [ "$marker_parent" = "$residue_physical" ] || return 1
  residue_parent="$(dirname "$residue_physical")"
  while IFS= read -r base; do
    [ -n "$base" ] || continue
    base_physical="$(oms_harness_physical_dir "$base" 2>/dev/null || true)"
    [ -n "$base_physical" ] || continue
    [ "$base_physical" != "$residue_parent" ] || return 0
  done <<EOF
$(oms_harness_temp_bases)
EOF
  return 1
}

# A sibling delegate that finishes with its worktree kept (failure or rejection)
# loses its liveness and appears on the hard surface although nothing about it
# is the worker's. That holds both for a sibling live-exempt at the
# pre-provider snapshot and for one started after it. Name it softly
# (`kept-sibling`) only when every added line belongs to such an entry and its
# checkout, backpointer and marker still pass the validator with only the pid
# dead; an entry started after the snapshot must also be absent from the
# baseline and sit on a detached HEAD. Conversely, residue cleanup run by
# another main may unregister a sibling that was already dead and kept at the
# snapshot (`gitmeta-residue`); name that softly (`sibling-cleanup`) only when
# every removed line belongs to such an entry and none of its lines remain.
# If a managed sibling is added and removed between comparison captures, name
# it softly only when an intermediate capture validated its marker/backpointer,
# its entry was absent at baseline, and the complete registry entry is now gone.
# A complete checkout must have a detached HEAD; a pending add with no checkout
# is eligible only through the pending marker/path validator.
# A main may keep using its scratch after its owner pid died; a changed HEAD
# line alone on such a residue entry that still exists is `kept-sibling` too.
# This is false-positive relief for cooperative parallel runs, not
# attribution: a same-UID worker can forge the same shape (SECURITY.md). A
# removed marker, a partial removal, any other changed line, or
# OMS_WORKER_GUARD_STRICT=1 keeps gitmeta hard. The soft names are returned in
# OMS_WG_GITMETA_SOFT.
oms_worker_gitmeta_only_kept_siblings() {  # REPO BEFORE_DIR CURRENT_WORKTREE
  local repo="$1"
  local before_dir="$2"
  local current_worktree_physical="${3:-}"
  local git_dir
  local line
  local entry
  local removed
  local added
  local head
  local cleaned=""

  OMS_WG_GITMETA_SOFT=""
  git_dir="$(git -C "$repo" rev-parse --git-common-dir 2>/dev/null || printf '')"
  git_dir="${git_dir//$'\r'/}"
  case "$git_dir" in
    "") return 1 ;;
    /*) ;;
    *) git_dir="$repo/$git_dir" ;;
  esac
  oms_worker_surface_capture_one "$repo" gitmeta "$current_worktree_physical" |
    LC_ALL=C sort > "$before_dir/gitmeta-now" || return 1
  LC_ALL=C sort "$before_dir/gitmeta" > "$before_dir/gitmeta-before" || return 1
  removed="$(LC_ALL=C comm -23 "$before_dir/gitmeta-before" "$before_dir/gitmeta-now")"
  added="$(LC_ALL=C comm -13 "$before_dir/gitmeta-before" "$before_dir/gitmeta-now")"
  [ -n "$removed" ] || [ -n "$added" ] || return 1
  if [ -n "$removed" ]; then
    while IFS= read -r line; do
      case "$line" in
        "worktree-dir worktrees/"*) entry="${line#worktree-dir worktrees/}" ;;
        "worktree worktrees/"*/*)
          entry="${line#worktree worktrees/}"
          entry="${entry%%/*}"
          ;;
        *) return 1 ;;
      esac
      if grep -Fxq "worktrees/$entry" "$before_dir/gitmeta-managed-replaced" 2>/dev/null; then
        cleaned=1
        continue
      fi
      # A mixed wave can remove a baseline-live sibling while another becomes
      # kept. Its admin leaves remain in the snapshot even though HEAD was exempt.
      if ! grep -Fxq "worktrees/$entry" "$before_dir/gitmeta-residue" 2>/dev/null; then
        grep -Fxq "worktrees/$entry" "$before_dir/gitmeta-exempt" 2>/dev/null || return 1
        [ ! -e "$git_dir/worktrees/$entry" ] && [ ! -L "$git_dir/worktrees/$entry" ] || return 1
      fi
      # A dead-owner scratch its main still uses moves only its HEAD.
      if [ "$line" != "${line#"worktree worktrees/$entry/HEAD "}" ] &&
        awk -v k="worktrees/$entry/HEAD" '$1 == "worktree" && $2 == k { f = 1 } END { exit !f }' \
          "$before_dir/gitmeta-now"; then
        continue
      fi
      ! awk -v e="worktrees/$entry" '$2 == e || index($2, e "/") == 1 { found = 1 } END { exit !found }' \
        "$before_dir/gitmeta-now" || return 1
      cleaned=1
    done <<EOF
$removed
EOF
    [ -z "$cleaned" ] || OMS_WG_GITMETA_SOFT=sibling-cleanup
  fi
  [ -n "$added" ] || return 0
  while IFS= read -r line; do
    case "$line" in
      "worktree-dir worktrees/"*) entry="${line#worktree-dir worktrees/}" ;;
      "worktree worktrees/"*/*)
        entry="${line#worktree worktrees/}"
        entry="${entry%%/*}"
        ;;
      *) return 1 ;;
    esac
    if ! grep -Fxq "worktrees/$entry" "$before_dir/gitmeta-exempt" 2>/dev/null &&
      ! grep -Fxq "worktrees/$entry" "$before_dir/gitmeta-managed-replaced" 2>/dev/null &&
      ! { [ "$line" != "${line#"worktree worktrees/$entry/HEAD "}" ] &&
        grep -Fxq "worktrees/$entry" "$before_dir/gitmeta-residue" 2>/dev/null; }; then
      ! awk -v e="worktrees/$entry" '$2 == e || index($2, e "/") == 1 { found = 1 } END { exit !found }' \
        "$before_dir/gitmeta-before" || return 1
      [ -f "$git_dir/worktrees/$entry/HEAD" ] && [ ! -L "$git_dir/worktrees/$entry/HEAD" ] || return 1
      head="$(sed -n '1p' "$git_dir/worktrees/$entry/HEAD" 2>/dev/null || true)"
      head="${head//$'\r'/}"
      case "$head" in
        *[!0-9a-f]*|"") return 1 ;;
      esac
      [ "${#head}" -ge 40 ] || return 1
    fi
    oms_worker_gitmeta_is_live_managed_worktree "$repo" \
      "$git_dir/worktrees/$entry/gitdir" "$current_worktree_physical" kept || return 1
  done <<EOF
$added
EOF
  OMS_WG_GITMETA_SOFT="${OMS_WG_GITMETA_SOFT:+$OMS_WG_GITMETA_SOFT, }kept-sibling"
  return 0
}

# A sibling can finish between two non-atomic comparison captures. Preserve
# only the positive proof from the intermediate capture; at comparison time
# require that the entry was absent at baseline and is now wholly gone.
oms_worker_gitmeta_delta_is_partial_managed_removal() {  # BEFORE_DIR COMMON_DIR
  local before="$1" common="$2"
  local side line entry partial=0

  [ -s "$before/gitmeta-managed-added" ] || return 1
  for side in gitmeta-removed gitmeta-added; do
    while IFS= read -r line; do
      case "$line" in
        "worktree-dir worktrees/"*) entry="${line#worktree-dir worktrees/}" ;;
        "worktree worktrees/"*/*)
          entry="${line#worktree worktrees/}"
          entry="${entry%%/*}"
          ;;
        *) return 1 ;;
      esac
      entry="${entry//$'\r'/}"
      grep -Fxq "$entry" "$before/gitmeta-managed-added" || return 1
      ! awk -v e="worktrees/$entry" \
        '$2 == e || index($2, e "/") == 1 { found = 1 } END { exit !found }' \
        "$before/gitmeta-before" || return 1
      [ -d "$common/worktrees/$entry" ] || return 1
      # An add registers HEAD before materializing the checkout, so a missing
      # checkout with HEAD still present is an in-progress creation. Removal
      # deletes HEAD; only that evidence starts the bounded partial-remove retry.
      if [ ! -e "$common/worktrees/$entry/HEAD" ]; then
        partial=1
      else
        return 1
      fi
    done < "$before/$side"
  done
  [ "$partial" = 1 ]
}

oms_worker_gitmeta_only_added_removed_siblings() {  # REPO BEFORE_DIR CURRENT_WORKTREE
  local repo="$1" before="$2" worker="${3:-}"
  local common entry line side
  local found=0
  local attempt=0

  OMS_WG_GITMETA_SOFT=""
  OMS_WG_GITMETA_PARTIAL_REMOVE_STUCK=0
  [ "${OMS_WORKER_GUARD_STRICT:-0}" != 1 ] || return 1
  common="$(oms_harness_git_path_physical "$repo" --git-common-dir 2>/dev/null)" || return 1
  [ -s "$before/gitmeta-managed-added" ] || return 1
  oms_worker_surface_capture_one "$repo" gitmeta "$worker" |
    LC_ALL=C sort > "$before/gitmeta-now" || return 1
  LC_ALL=C sort "$before/gitmeta" > "$before/gitmeta-before" || return 1
  LC_ALL=C comm -23 "$before/gitmeta-before" "$before/gitmeta-now" > "$before/gitmeta-removed" || return 1
  LC_ALL=C comm -13 "$before/gitmeta-before" "$before/gitmeta-now" > "$before/gitmeta-added" || return 1
  if oms_worker_gitmeta_delta_is_partial_managed_removal "$before" "$common"; then
    while [ "$attempt" -lt 50 ]; do
      attempt=$((attempt + 1))
      sleep 0.1
      oms_worker_surface_capture_one "$repo" gitmeta "$worker" |
        LC_ALL=C sort > "$before/gitmeta-now" || return 1
      LC_ALL=C comm -23 "$before/gitmeta-before" "$before/gitmeta-now" > "$before/gitmeta-removed" || return 1
      LC_ALL=C comm -13 "$before/gitmeta-before" "$before/gitmeta-now" > "$before/gitmeta-added" || return 1
      if ! oms_worker_gitmeta_delta_is_partial_managed_removal "$before" "$common"; then
        break
      fi
    done
    if oms_worker_gitmeta_delta_is_partial_managed_removal "$before" "$common"; then
      OMS_WG_GITMETA_PARTIAL_REMOVE_STUCK=1
      return 1
    fi
  fi
  while IFS= read -r entry; do
    entry="${entry//$'\r'/}"
    [ -n "$entry" ] || continue
    if awk -v e="worktrees/$entry" \
      '$2 == e || index($2, e "/") == 1 { found = 1 } END { exit !found }' \
      "$before/gitmeta-before"; then
      continue
    fi
    if [ ! -e "$common/worktrees/$entry" ] && [ ! -L "$common/worktrees/$entry" ] &&
      ! awk -v e="worktrees/$entry" \
        '$2 == e || index($2, e "/") == 1 { found = 1 } END { exit !found }' \
        "$before/gitmeta-now"; then
      found=1
    fi
  done < "$before/gitmeta-managed-added"
  [ "$found" = 1 ] || return 1
  for side in gitmeta-removed gitmeta-added; do
    while IFS= read -r line; do
      case "$line" in
        "worktree-dir worktrees/"*) entry="${line#worktree-dir worktrees/}" ;;
        "worktree worktrees/"*/*)
          entry="${line#worktree worktrees/}"
          entry="${entry%%/*}"
          ;;
        *) return 1 ;;
      esac
      entry="${entry//$'\r'/}"
      grep -Fxq "$entry" "$before/gitmeta-managed-added" || return 1
      ! awk -v e="worktrees/$entry" \
        '$2 == e || index($2, e "/") == 1 { found = 1 } END { exit !found }' \
        "$before/gitmeta-before" || return 1
      [ ! -e "$common/worktrees/$entry" ] && [ ! -L "$common/worktrees/$entry" ] || return 1
      ! awk -v e="worktrees/$entry" \
        '$2 == e || index($2, e "/") == 1 { found = 1 } END { exit !found }' \
        "$before/gitmeta-now" || return 1
    done < "$before/$side"
  done
  OMS_WG_GITMETA_SOFT=kept-sibling
}

# Neither `git worktree add` nor `remove` is atomic: for a moment the registry
# entry exists without its `gitdir` file, or the file is already gone while the
# entry directory remains, and no marker can classify what cannot be read.
# Those states resolve on their own; a worker's write to the metadata surface
# does not. Re-capture briefly before naming gitmeta as changed.
oms_worker_gitmeta_settles() {  # REPO BEFORE_FILE CURRENT_WORKTREE
  local repo="$1"
  local before="$2"
  local current_worktree_physical="${3:-}"
  local attempt=0

  while [ "$attempt" -lt 3 ]; do
    attempt=$((attempt + 1))
    sleep 0.3
    if oms_worker_surface_capture_one "$repo" gitmeta \
      "$current_worktree_physical" | cmp -s - "$before"; then
      return 0
    fi
  done
  return 1
}

# A marker/name cannot establish ownership. Bind a foreign entry to a real
# checkout's reciprocal Git backpointer and common-dir, excluding both guarded
# and delegated checkouts. Capture this identity before allowing its removal.
oms_worker_gitmeta_is_foreign_worktree() {  # REPO METADATA CURRENT_WORKTREE
  local repo="$1" metadata="$2" worker="${3:-}"
  local gitfile physical actual common guarded
  OMS_WG_FOREIGN_PHYSICAL=""
  OMS_WG_FOREIGN_PATH=""
  [ -n "$worker" ] && [ -d "$metadata" ] && [ ! -L "$metadata" ] || return 1
  [ -f "$metadata/gitdir" ] && [ ! -L "$metadata/gitdir" ] &&
    [ -f "$metadata/HEAD" ] && [ ! -L "$metadata/HEAD" ] || return 1
  gitfile="$(sed -n '1p' "$metadata/gitdir")"
  gitfile="${gitfile//$'\r'/}"
  case "$gitfile" in */.git) ;; *) return 1 ;; esac
  physical="$(oms_harness_physical_dir "${gitfile%/.git}" 2>/dev/null)" || return 1
  worker="$(oms_harness_physical_dir "$worker" 2>/dev/null)" || return 1
  guarded="$(oms_harness_physical_dir "$repo" 2>/dev/null)" || return 1
  [ "$physical" != "$worker" ] && [ "$physical" != "$guarded" ] &&
    [ -f "$physical/.git" ] && [ ! -L "$physical/.git" ] || return 1
  metadata="$(oms_harness_physical_dir "$metadata" 2>/dev/null)" || return 1
  actual="$(oms_harness_git_path_physical "$physical" --git-dir 2>/dev/null)" || return 1
  [ "$actual" = "$metadata" ] || return 1
  actual="$(oms_harness_git_path_physical "$physical" --git-common-dir 2>/dev/null)" || return 1
  common="$(oms_harness_git_path_physical "$repo" --git-common-dir 2>/dev/null)" || return 1
  [ "$actual" = "$common" ] || return 1
  OMS_WG_FOREIGN_PHYSICAL="$physical"
  OMS_WG_FOREIGN_PATH="${gitfile%/.git}"
  return 0
}

# A valid Git backpointer proves registration, not that the worker could not
# have created it. New/deleted branch attribution therefore also requires the
# checkout to live below the parent-only worktree root.
oms_worker_path_is_managed_foreign_worktree() {  # PATH CURRENT_WORKTREE
  local path="$1" worker="${2:-}" allow_missing="${3:-0}"
  local root root_physical physical worker_physical
  root="$(oms_harness_delegate_worktree_root)" || return 1
  case "$root" in /*) ;; *) return 1 ;; esac
  [ -d "$root" ] && [ ! -L "$root" ] || return 1
  root_physical="$(oms_harness_physical_dir "$root" 2>/dev/null)" || return 1
  while [ "$root" != / ] && [ "${root%/}" != "$root" ]; do root="${root%/}"; done
  [ "$root" = "$root_physical" ] || return 1
  while [ "$path" != / ] && [ "${path%/}" != "$path" ]; do path="${path%/}"; done
  if [ -d "$path" ]; then
    physical="$(oms_harness_physical_dir "$path" 2>/dev/null)" || return 1
    [ "$path" = "$physical" ] || return 1
  else
    # A deleted checkout cannot be resolved again; its snapshot receipt holds
    # the canonical path captured while the registered directory still existed.
    [ "$allow_missing" = 1 ] || return 1
    case "$path" in *//*|*/./*|*/../*|*/.|*/..) return 1 ;; esac
    physical="$path"
  fi
  case "$physical" in "$root_physical"/*) ;; *) return 1 ;; esac
  worker_physical="$(oms_harness_physical_dir "$worker" 2>/dev/null)" || return 1
  case "$physical" in "$worker_physical"|"$worker_physical"/*) return 1 ;; esac
  # The worker's private fixture root is a writable sibling of its worktree.
  case "$physical" in "${worker_physical%/*}"/.oms-panel-fixtures.*) return 1 ;; esac
  return 0
}

# Lifecycle exemptions cannot hide executable per-worktree config or rewired
# registrations. Admin bytes stay fixed for surviving checkouts. Git may reuse
# a removed managed sibling slot only after both checkout identities are proven.
oms_worker_gitmeta_admin_changes_are_safe() {  # REPO BEFORE_DIR CURRENT_WORKTREE
  local repo="$1" before="$2" worker="${3:-}"
  local common entry receipt physical line side head
  OMS_WG_GITMETA_ADMIN_QUIET=0
  common="$(oms_harness_git_path_physical "$repo" --git-common-dir 2>/dev/null)" || return 1
  OMS_WG_GITMETA_MANAGED_ADDED_OUT="$before/gitmeta-managed-added" \
    oms_worker_surface_capture_one "$repo" gitmeta "$worker" \
    > "$before/gitmeta-admin-now" || return 1
  python3 - "$before/gitmeta" "$before/gitmeta-admin-now" "$before/gitmeta-admin-lifecycle" "$before/gitmeta-replacement-candidates" <<'PY' > "$before/gitmeta-new-entries" || return 1
import sys
def entries(path):
    result = {}
    for line in open(path):
        kind, _, value = line.rstrip('\r\n').partition(' ')
        if kind == 'worktree-dir':
            entry, leaf, digest = value, '', ''
        elif kind in ('worktree', 'worktree-link', 'worktree-entry'):
            name, digest = value.rsplit(' ', 1)
            entry, _, leaf = name.rpartition('/')
        else:
            continue
        if entry.startswith('worktrees/'):
            result.setdefault(entry, {})[leaf] = (kind, digest)
    return result
old, new = (entries(path) for path in sys.argv[1:3])
replacements = []
for entry in old.keys() & new.keys():
    if old[entry].get('config.worktree') != new[entry].get('config.worktree'):
        raise SystemExit(1)
    if any(old[entry].get(leaf) != new[entry].get(leaf) for leaf in ('gitdir', 'commondir')):
        # Shell validates the old checkout's disappearance and the new managed
        # checkout's reciprocal identity before this slot can be reused.
        replacements.append(entry[len('worktrees/'):])
with open(sys.argv[4], 'w') as handle:
    handle.write(''.join(entry + '\n' for entry in sorted(replacements)))
for entry in new.keys() - old.keys():
    if 'config.worktree' in new[entry]:
        raise SystemExit(1)
    print(entry[len('worktrees/'):])
# Preserve silent live OMS lifecycle exemptions: only newly visible admin
# leaves may differ, with no checkout, object-store or existing-entry edits.
delta = set(open(sys.argv[1]).read().splitlines()) ^ set(open(sys.argv[2]).read().splitlines())
quiet = bool(delta)
for line in delta:
    kind, _, value = line.partition(' ')
    name = value.rsplit(' ', 1)[0]
    entry, _, leaf = name.rpartition('/')
    if kind != 'worktree' or leaf not in ('gitdir', 'commondir', 'config.worktree') or entry in old.keys() & new.keys():
        quiet = False
with open(sys.argv[3], 'w') as handle:
    if quiet:
        for entry in sorted(old.keys() ^ new.keys()):
            handle.write(('added ' if entry in new else 'removed ') + entry[len('worktrees/'):] + '\n')
PY
  : > "$before/gitmeta-managed-replaced"
  while IFS= read -r entry; do
    entry="${entry//$'\r'/}"
    if ! grep -Fxq "worktrees/$entry" "$before/gitmeta-exempt"; then
      grep -Fxq "worktrees/$entry" "$before/gitmeta-residue" || return 1
    fi
    receipt="$before/foreign-worktrees/$entry"
    [ -f "$receipt/physical" ] && [ -f "$receipt/gitdir" ] || return 1
    physical="$(cat "$receipt/physical")"
    physical="${physical//$'\r'/}"
    [ ! -e "$physical" ] && [ ! -L "$physical" ] || return 1
    oms_worker_path_is_managed_foreign_worktree "$physical" "$worker" 1 || return 1
    # Bind the saved checkout identity to the protected baseline admin bytes.
    grep -Fxq "worktree worktrees/$entry/gitdir $(oms_sha256_file "$receipt/gitdir")" \
      "$before/gitmeta" || return 1
    oms_worker_gitmeta_is_live_managed_worktree "$repo" "$common/worktrees/$entry/gitdir" "$worker" kept || return 1
    head="$(sed -n '1p' "$common/worktrees/$entry/HEAD")"
    head="${head//$'\r'/}"
    case "$head" in ""|*[!0-9a-f]*) return 1 ;; esac
    [ "${#head}" = 40 ] || [ "${#head}" = 64 ] || return 1
    printf 'worktrees/%s\n' "$entry" >> "$before/gitmeta-managed-replaced"
  done < "$before/gitmeta-replacement-candidates"
  while IFS= read -r entry; do
    entry="${entry//$'\r'/}"
    if oms_worker_gitmeta_is_foreign_worktree "$repo" "$common/worktrees/$entry" "$worker"; then
      physical="$OMS_WG_FOREIGN_PHYSICAL"
      for receipt in "$before/foreign-worktrees/"*/physical; do
        [ -f "$receipt" ] || continue
        [ "$(cat "$receipt")" != "$physical" ] || return 1
      done
    fi
  done < "$before/gitmeta-new-entries"
  if [ -s "$before/gitmeta-admin-lifecycle" ]; then
    OMS_WG_GITMETA_ADMIN_QUIET=1
    while IFS= read -r line; do
      line="${line//$'\r'/}"
      side="${line%% *}" entry="${line#* }"
      if [ "$side" = added ]; then
        if oms_worker_gitmeta_is_live_managed_worktree "$repo" \
          "$common/worktrees/$entry/gitdir" "$worker"; then
          if [ ! -e "$common/worktrees/$entry" ] &&
            [ ! -L "$common/worktrees/$entry" ]; then
            OMS_WG_GITMETA_ADMIN_QUIET=0
          fi
        else
          OMS_WG_GITMETA_ADMIN_QUIET=0
        fi
      elif ! grep -Fxq "worktrees/$entry" "$before/gitmeta-exempt" ||
        [ -e "$common/worktrees/$entry" ] || [ -L "$common/worktrees/$entry" ]; then
        OMS_WG_GITMETA_ADMIN_QUIET=0
      fi
    done < "$before/gitmeta-admin-lifecycle"
  fi
  return 0
}

oms_worker_gitmeta_only_foreign_worktrees() {  # REPO BEFORE_DIR CURRENT_WORKTREE
  local repo="$1" before="$2" worker="${3:-}"
  local common line entry side other
  common="$(oms_harness_git_path_physical "$repo" --git-common-dir 2>/dev/null)" || return 1
  oms_worker_surface_capture_one "$repo" gitmeta "$worker" |
    LC_ALL=C sort > "$before/gitmeta-now" || return 1
  LC_ALL=C sort "$before/gitmeta" > "$before/gitmeta-before" || return 1
  LC_ALL=C comm -23 "$before/gitmeta-before" "$before/gitmeta-now" > "$before/gitmeta-removed"
  LC_ALL=C comm -13 "$before/gitmeta-before" "$before/gitmeta-now" > "$before/gitmeta-added"
  for side in removed added; do
    other="$before/gitmeta-now"
    [ "$side" != added ] || other="$before/gitmeta-before"
    while IFS= read -r line; do
      case "$line" in
        "worktree-dir worktrees/"*) entry="${line#worktree-dir worktrees/}" ;;
        "worktree worktrees/"*/*) entry="${line#worktree worktrees/}"; entry="${entry%%/*}" ;;
        *) return 1 ;;
      esac
      # Only whole-entry additions/removals qualify; edits never do.
      ! awk -v e="worktrees/$entry" '$2 == e || index($2, e "/") == 1 { f=1 } END { exit !f }' \
        "$other" || return 1
      if [ "$side" = removed ]; then
        [ -f "$before/foreign-worktrees/$entry/HEAD" ] &&
          [ ! -e "$common/worktrees/$entry" ] && [ ! -L "$common/worktrees/$entry" ] || return 1
      else
        ! grep -Fxq "worktrees/$entry" "$before/gitmeta-exempt" || return 1
        oms_worker_gitmeta_is_foreign_worktree "$repo" "$common/worktrees/$entry" "$worker" || return 1
      fi
    done < "$before/gitmeta-$side"
  done
  [ -s "$before/gitmeta-removed" ] || [ -s "$before/gitmeta-added" ]
}

# Ref age/committer identity alone is forgeable. Require an existing branch to
# stay checked out in the same foreign checkout, with an appended HEAD reflog
# recording its new tip and the ref log's committer/time. Plain update-ref in
# the worker/shared checkout leaves no such evidence. This is cooperative
# attribution, not isolation from a same-UID worker that writes foreign logs.
oms_worker_refs_only_foreign() {  # REPO BEFORE_DIR CURRENT_WORKTREE
  local repo="$1" before="$2" worker="${3:-}"
  local common receipt entry ref head logpath
  common="$(oms_harness_git_path_physical "$repo" --git-common-dir 2>/dev/null)" || return 1
  oms_worker_surface_capture_one "$repo" refs "$worker" > "$before/refs-now" || return 1
  python3 - "$before/refs" "$before/refs-now" "$before/refs-protected" <<'PY' > "$before/refs-changed" || return 1
import sys
def refs(path):
    return dict(line.rstrip('\r\n').split(' ', 1)[::-1] for line in open(path) if ' ' in line)
old, new = refs(sys.argv[1]), refs(sys.argv[2])
protected = set(open(sys.argv[3]).read().splitlines())
# Accepted pre-existing contract: evidence-backed moves of non-oms branches stay soft.
changed = [ref for ref in old.keys() | new.keys() if old.get(ref) != new.get(ref)]
if not changed or any(not ref.startswith('refs/heads/') or ref in protected or
                      (ref not in old and not ref.startswith('refs/heads/oms/')) or
                      (ref not in new and not ref.startswith('refs/heads/oms/'))
                      for ref in changed):
    raise SystemExit(1)
for ref in sorted(changed):
    print(ref)
# The bare resolved HEAD row is also protected, including detached HEAD.
def tip(path):
    return [line.strip() for line in open(path) if ' ' not in line.strip()]
if tip(sys.argv[1]) != tip(sys.argv[2]):
    raise SystemExit(1)
PY
  while IFS= read -r ref; do
    ref="${ref//$'\r'/}"
    receipt=""
    if ! awk -v r="$ref" '$2 == r { found=1 } END { exit !found }' "$before/refs"; then
      oms_worker_refs_foreign_branch_created "$repo" "$before" "$worker" "$ref" || return 1
      continue
    elif ! awk -v r="$ref" '$2 == r { found=1 } END { exit !found }' "$before/refs-now"; then
      oms_worker_refs_foreign_branch_deleted "$repo" "$before" "$worker" "$ref" || return 1
      continue
    fi
    for entry in "$before/foreign-worktrees/"*; do
      [ -f "$entry/HEAD" ] || continue
      head="$(sed -n '1p' "$entry/HEAD")"; head="${head//$'\r'/}"
      [ "$head" = "ref: $ref" ] || continue
      oms_worker_gitmeta_is_foreign_worktree "$repo" "$common/worktrees/${entry##*/}" "$worker" || continue
      cmp -s "$entry/gitdir" "$common/worktrees/${entry##*/}/gitdir" &&
        cmp -s "$entry/HEAD" "$common/worktrees/${entry##*/}/HEAD" || continue
      logpath="$common/worktrees/${entry##*/}/logs/HEAD"
      [ -f "$logpath" ] && [ ! -L "$logpath" ] &&
        [ -f "$common/logs/$ref" ] && [ ! -L "$common/logs/$ref" ] || continue
      [ -f "$entry/ref-log" ] || continue
      if python3 - "$entry/log" "$logpath" "$common/logs/$ref" "$before/refs" "$before/refs-now" "$ref" "$entry/ref-log" <<'PY'
import sys
before, now, branch = (open(path, 'rb').read() for path in sys.argv[1:4])
branch_before = open(sys.argv[7], 'rb').read()
ref = sys.argv[6]
def tip(path):
    return next(line.split()[0].encode() for line in open(path) if line.split()[-1] == ref)
old, new = tip(sys.argv[4]), tip(sys.argv[5])
if not now.startswith(before) or not branch.startswith(branch_before):
    raise SystemExit(1)
rows = [row.split(b'\t', 1)[0].split(b' ', 2) for row in branch[len(branch_before):].splitlines()]
head_rows = [row.split(b'\t', 1)[0].split(b' ', 2) for row in now[len(before):].splitlines()]
if not rows:
    raise SystemExit(1)
tip_oid = old
for fields in rows:
    if len(fields) != 3 or fields[0] != tip_oid or not any(
            len(head) == 3 and head[1:] == fields[1:] for head in head_rows):
        raise SystemExit(1)
    tip_oid = fields[1]
raise SystemExit(0 if tip_oid == new else 1)
PY
      then receipt=1; break; fi
    done
    [ -n "$receipt" ] || return 1
  done < "$before/refs-changed"
}

# A newly born oms/* ref has no before-snapshot ref-log receipt. Accept it only
# when a live, independently registered checkout now names it and Git recorded
# the same checkout event in both its branch and per-worktree HEAD reflogs.
oms_worker_refs_foreign_branch_created() {  # REPO BEFORE_DIR CURRENT_WORKTREE REF
  local repo="$1" before="$2" worker="${3:-}" ref="$4"
  local common entry head logpath branchlog receipt branch_before head_before
  case "$ref" in refs/heads/oms/*) ;; *) return 1 ;; esac
  common="$(oms_harness_git_path_physical "$repo" --git-common-dir 2>/dev/null)" || return 1
  for entry in "$common"/worktrees/*; do
    [ -f "$entry/HEAD" ] && [ ! -L "$entry/HEAD" ] || continue
    head="$(sed -n '1p' "$entry/HEAD")"; head="${head//$'\r'/}"
    [ "$head" = "ref: $ref" ] || continue
    oms_worker_gitmeta_is_foreign_worktree "$repo" "$entry" "$worker" || continue
    oms_worker_path_is_managed_foreign_worktree "$OMS_WG_FOREIGN_PATH" "$worker" || continue
    branchlog="$common/logs/$ref"; logpath="$entry/logs/HEAD"
    [ -f "$branchlog" ] && [ ! -L "$branchlog" ] &&
      [ -f "$logpath" ] && [ ! -L "$logpath" ] || continue
    receipt="$before/foreign-worktrees/${entry##*/}"
    branch_before="$before/branch-logs/${ref#refs/heads/}"
    head_before=""
    if [ -d "$receipt" ]; then
      cmp -s "$receipt/gitdir" "$entry/gitdir" || continue
      head_before="$receipt/log"
    fi
    if python3 - "$branchlog" "$logpath" "$before/refs-now" "$ref" \
      "$branch_before" "$head_before" <<'PY'
import os, sys
branch_log, head_log, refs_now, ref, branch_before, head_before = sys.argv[1:]
def read_rows(data):
    result = []
    for line in data.splitlines():
        fields = line.rstrip(b"\r\n").split(b"\t", 1)
        if len(fields) == 2:
            result.append((fields[0].split(b" ", 2), fields[1]))
    return result
branch_now = open(branch_log, "rb").read()
head_now = open(head_log, "rb").read()
branch_base = open(branch_before, "rb").read() if os.path.isfile(branch_before) else b""
head_base = open(head_before, "rb").read() if head_before else b""
if not branch_now.startswith(branch_base) or not head_now.startswith(head_base):
    raise SystemExit(1)
try:
    tip = next(line.split(" ", 1)[0].encode() for line in open(refs_now)
               if line.rstrip().endswith(" " + ref))
except StopIteration:
    raise SystemExit(1)
branch_rows = read_rows(branch_now[len(branch_base):])
head_rows = read_rows(head_now[len(head_base):])
if not branch_rows or not head_rows:
    raise SystemExit(1)
suffix = (" to " + ref[len("refs/heads/"):]).encode()
tip_oid = b"0" * len(tip)
checkout_seen = False
for fields, _ in branch_rows:
    if len(fields) != 3 or fields[0] != tip_oid:
        raise SystemExit(1)
    matching = [(head, message) for head, message in head_rows
                if len(head) == 3 and head[1:] == fields[1:]]
    if not matching:
        raise SystemExit(1)
    if any(message.startswith(b"checkout: moving from ") and message.endswith(suffix)
           for _, message in matching):
        checkout_seen = True
    tip_oid = fields[1]
raise SystemExit(0 if checkout_seen and tip_oid == tip else 1)
PY
    then return 0; fi
  done
  return 1
}

oms_worker_refs_foreign_branch_deleted() {  # REPO BEFORE_DIR CURRENT_WORKTREE REF
  local repo="$1" before="$2" worker="${3:-}" ref="$4"
  local common entry receipt head count=0 current physical path
  case "$ref" in refs/heads/oms/*) ;; *) return 1 ;; esac
  common="$(oms_harness_git_path_physical "$repo" --git-common-dir 2>/dev/null)" || return 1
  for receipt in "$before/foreign-worktrees/"*; do
    [ -f "$receipt/HEAD" ] || continue
    head="$(sed -n '1p' "$receipt/HEAD")"; head="${head//$'\r'/}"
    [ "$head" = "ref: $ref" ] || continue
    count=$((count + 1))
    [ -f "$receipt/physical" ] || return 1
    physical="$(sed -n '1p' "$receipt/physical")"; physical="${physical//$'\r'/}"
    [ -f "$receipt/path" ] || return 1
    path="$(sed -n '1p' "$receipt/path")"; path="${path//$'\r'/}"
    [ "$path" = "$physical" ] || return 1
    oms_worker_path_is_managed_foreign_worktree "$physical" "$worker" 1 || return 1
    entry="$common/worktrees/${receipt##*/}"
    if [ -e "$entry" ] || [ -L "$entry" ]; then
      oms_worker_gitmeta_is_foreign_worktree "$repo" "$entry" "$worker" || return 1
      [ "$OMS_WG_FOREIGN_PHYSICAL" = "$physical" ] || return 1
      [ "$OMS_WG_FOREIGN_PATH" = "$physical" ] || return 1
      oms_worker_path_is_managed_foreign_worktree "$OMS_WG_FOREIGN_PATH" "$worker" || return 1
      current="$(sed -n '1p' "$entry/HEAD")"; current="${current//$'\r'/}"
      [ "$current" != "ref: $ref" ] || return 1
    fi
    [ -f "$receipt/ref-log" ] || return 1
    if python3 - "$receipt/ref-log" "$before/refs" "$ref" <<'PY'
import sys
old_log, refs_before, ref = sys.argv[1:]
try:
    tip = next(line.split(" ", 1)[0].encode() for line in open(refs_before)
               if line.rstrip().endswith(" " + ref))
except StopIteration:
    raise SystemExit(1)
rows = [line.split(b"\t", 1)[0].split(b" ", 2)
        for line in open(old_log, "rb").read().splitlines()]
raise SystemExit(0 if rows and len(rows[-1]) == 3 and rows[-1][1] == tip else 1)
PY
    then :; else return 1; fi
  done
  [ "$count" -eq 1 ] || return 1
  for entry in "$common"/worktrees/*; do
    [ -f "$entry/HEAD" ] && [ ! -L "$entry/HEAD" ] || continue
    current="$(sed -n '1p' "$entry/HEAD")"; current="${current//$'\r'/}"
    [ "$current" != "ref: $ref" ] || return 1
  done
  return 0
}

oms_worker_gitmeta_only_foreign_checkout_heads() {  # REPO BEFORE_DIR CURRENT_WORKTREE
  local repo="$1" before="$2" worker="${3:-}"
  local common entry old_head new_head ref
  common="$(oms_harness_git_path_physical "$repo" --git-common-dir 2>/dev/null)" || return 1
  oms_worker_refs_only_foreign "$repo" "$before" "$worker" || return 1
  OMS_WG_GITMETA_EXEMPT_OUT=/dev/null OMS_WG_GITMETA_RESIDUE_OUT=/dev/null \
    oms_worker_surface_capture_one "$repo" gitmeta "$worker" > "$before/gitmeta-checkout-now" || return 1
  python3 - "$before/gitmeta" "$before/gitmeta-checkout-now" \
    "$before/gitmeta-checkout-entries" <<'PY' || return 1
import sys
def surfaces(path):
    heads, other = {}, []
    for line in open(path):
        row = line.rstrip("\r\n")
        kind, _, value = row.partition(" ")
        name, _, digest = value.rpartition(" ")
        if kind == "worktree" and name.endswith("/HEAD"):
            heads[name] = digest
        else:
            other.append(row)
    return heads, sorted(other)
old, old_other = surfaces(sys.argv[1])
new, new_other = surfaces(sys.argv[2])
if old_other != new_other or old.keys() != new.keys():
    raise SystemExit(1)
changed = [name for name in old if old[name] != new[name]]
if not changed:
    raise SystemExit(1)
with open(sys.argv[3], "w") as handle:
    for name in changed:
        handle.write(name + "\n")
PY
  while IFS= read -r entry; do
    entry="${entry#worktrees/}"
    entry="${entry%/HEAD}"
    [ -f "$before/foreign-worktrees/$entry/HEAD" ] || return 1
    oms_worker_gitmeta_is_foreign_worktree "$repo" "$common/worktrees/$entry" "$worker" || return 1
    cmp -s "$before/foreign-worktrees/$entry/gitdir" "$common/worktrees/$entry/gitdir" || return 1
    old_head="$(sed -n '1p' "$before/foreign-worktrees/$entry/HEAD")"; old_head="${old_head//$'\r'/}"
    new_head="$(sed -n '1p' "$common/worktrees/$entry/HEAD")"; new_head="${new_head//$'\r'/}"
    case "$new_head" in 'ref: refs/heads/'*) ;; *) return 1 ;; esac
    ref="${new_head#ref: }"
    [ "$old_head" != "$new_head" ] && grep -Fxq "$ref" "$before/refs-changed" || return 1
  done < "$before/gitmeta-checkout-entries"
  return 0
}

# Capture and later re-check the delegated checkout's identity. A provider owns
# the files inside this directory, but it must not be able to rename the
# directory and replace the old pathname with a symlink to the primary checkout:
# every later `git -C "$worktree"` would otherwise follow that new target. The
# real directory and gitdir device/inode identities, common-dir, the regular
# `.git` file, and Git's regular backpointer must all stay bound to their
# pre-provider receipt.
# Results are globals so callers do not need to serialize pathnames through a
# delimiter (a repository pathname may legally contain tabs or newlines).
OMS_WORKER_IDENTITY_PHYSICAL=""
OMS_WORKER_IDENTITY_GIT_DIR=""
OMS_WORKER_IDENTITY_COMMON_DIR=""
OMS_WORKER_IDENTITY_GITFILE_SHA=""
OMS_WORKER_IDENTITY_BACKPOINTER_SHA=""
OMS_WORKER_IDENTITY_WORKTREE_STAT=""
OMS_WORKER_IDENTITY_GITDIR_STAT=""
OMS_WORKER_IDENTITY_DETAIL=""

oms_worker_worktree_identity_capture() {  # REPO WORKTREE
  local repo="$1"
  local worker_tree="$2"
  local physical=""
  local git_dir=""
  local common_dir=""
  local repo_common=""
  local gitfile_line=""
  local gitfile_target=""
  local gitfile_target_physical=""
  local backpointer_file=""
  local backpointer=""
  local backpointer_parent=""
  local backpointer_physical=""
  local gitfile_sha=""
  local backpointer_sha=""
  local identity_stats=""
  local identity_tab=""
  local worktree_stat=""
  local gitdir_stat=""

  OMS_WORKER_IDENTITY_PHYSICAL=""
  OMS_WORKER_IDENTITY_GIT_DIR=""
  OMS_WORKER_IDENTITY_COMMON_DIR=""
  OMS_WORKER_IDENTITY_GITFILE_SHA=""
  OMS_WORKER_IDENTITY_BACKPOINTER_SHA=""
  OMS_WORKER_IDENTITY_WORKTREE_STAT=""
  OMS_WORKER_IDENTITY_GITDIR_STAT=""
  OMS_WORKER_IDENTITY_DETAIL=""

  physical="$(oms_harness_physical_dir "$worker_tree" 2>/dev/null || true)"
  if [ -z "$physical" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree path is missing, moved, or symbolic"
    return 1
  fi
  if [ ! -f "$worker_tree/.git" ] || [ -L "$worker_tree/.git" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree .git is not a regular backpointer file"
    return 1
  fi

  gitfile_line="$(sed -n '1p' "$worker_tree/.git" 2>/dev/null || true)"
  gitfile_line="${gitfile_line//$'\r'/}"
  case "$gitfile_line" in
    "gitdir: "*) gitfile_target="${gitfile_line#gitdir: }" ;;
    *)
      OMS_WORKER_IDENTITY_DETAIL="delegated worktree .git backpointer is malformed"
      return 1
      ;;
  esac
  case "$gitfile_target" in
    /*|[A-Za-z]:/*) ;;
    *) gitfile_target="$worker_tree/$gitfile_target" ;;
  esac
  gitfile_target_physical="$(
    oms_harness_physical_dir "$gitfile_target" 2>/dev/null || true
  )"
  git_dir="$(oms_harness_git_path_physical "$worker_tree" --git-dir 2>/dev/null || true)"
  common_dir="$(
    oms_harness_git_path_physical "$worker_tree" --git-common-dir 2>/dev/null || true
  )"
  repo_common="$(oms_harness_git_path_physical "$repo" --git-common-dir 2>/dev/null || true)"
  if [ -z "$git_dir" ] || [ "$gitfile_target_physical" != "$git_dir" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree .git no longer points to its actual gitdir"
    return 1
  fi
  if [ -z "$common_dir" ] || [ -z "$repo_common" ] ||
    [ "$common_dir" != "$repo_common" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree no longer belongs to the primary common gitdir"
    return 1
  fi
  case "$git_dir" in
    "$common_dir"/worktrees/*)
      case "${git_dir#"$common_dir"/worktrees/}" in
        ""|*/*)
          OMS_WORKER_IDENTITY_DETAIL="delegated worktree gitdir has an invalid registration path"
          return 1
          ;;
      esac
      ;;
    *)
      OMS_WORKER_IDENTITY_DETAIL="delegated worktree gitdir is outside the common worktree registry"
      return 1
      ;;
  esac

  backpointer_file="$git_dir/gitdir"
  if [ ! -f "$backpointer_file" ] || [ -L "$backpointer_file" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree registry gitdir is not a regular backpointer file"
    return 1
  fi
  backpointer="$(sed -n '1p' "$backpointer_file" 2>/dev/null || true)"
  backpointer="${backpointer//$'\r'/}"
  case "$backpointer" in
    */.git) backpointer_parent="${backpointer%/.git}" ;;
    *)
      OMS_WORKER_IDENTITY_DETAIL="delegated worktree registry backpointer is malformed"
      return 1
      ;;
  esac
  case "$backpointer_parent" in
    /*|[A-Za-z]:/*) ;;
    *) backpointer_parent="$git_dir/$backpointer_parent" ;;
  esac
  backpointer_physical="$(
    oms_harness_physical_dir "$backpointer_parent" 2>/dev/null || true
  )"
  if [ "$backpointer_physical" != "$physical" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree registry does not point back to the current checkout"
    return 1
  fi

  gitfile_sha="$(oms_sha256_file "$worker_tree/.git" 2>/dev/null || true)"
  backpointer_sha="$(oms_sha256_file "$backpointer_file" 2>/dev/null || true)"
  if [ -z "$gitfile_sha" ] || [ -z "$backpointer_sha" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree backpointer could not be hashed"
    return 1
  fi
  if ! identity_stats="$(python3 - "$physical" "$git_dir" <<'PY'
import os
import stat
import sys

values = []
for path in sys.argv[1:]:
    info = os.lstat(path)
    if not stat.S_ISDIR(info.st_mode):
        raise SystemExit(2)
    values.append("%s:%s" % (info.st_dev, info.st_ino))
print("\t".join(values))
PY
  )"; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree inode identity could not be read"
    return 1
  fi
  # Native Windows Python writes CRLF, and the same worktree can have more than
  # one path spelling. Paths were canonicalized above; strip CR only from this
  # compact device/inode receipt before comparing it across provider phases.
  identity_stats="${identity_stats//$'\r'/}"
  identity_tab="$(printf '\t')"
  case "$identity_stats" in
    *"$identity_tab"*) ;;
    *)
      OMS_WORKER_IDENTITY_DETAIL="delegated worktree inode identity is malformed"
      return 1
      ;;
  esac
  worktree_stat="${identity_stats%%"$identity_tab"*}"
  gitdir_stat="${identity_stats#*"$identity_tab"}"
  if [ -z "$worktree_stat" ] || [ -z "$gitdir_stat" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree inode identity is incomplete"
    return 1
  fi

  OMS_WORKER_IDENTITY_PHYSICAL="$physical"
  OMS_WORKER_IDENTITY_GIT_DIR="$git_dir"
  OMS_WORKER_IDENTITY_COMMON_DIR="$common_dir"
  OMS_WORKER_IDENTITY_GITFILE_SHA="$gitfile_sha"
  OMS_WORKER_IDENTITY_BACKPOINTER_SHA="$backpointer_sha"
  OMS_WORKER_IDENTITY_WORKTREE_STAT="$worktree_stat"
  OMS_WORKER_IDENTITY_GITDIR_STAT="$gitdir_stat"
  return 0
}

oms_worker_worktree_identity_check() {  # REPO WORKTREE PHYSICAL GITDIR COMMON GITFILE_SHA BACKPOINTER_SHA WORKTREE_STAT GITDIR_STAT
  local repo="$1"
  local worker_tree="$2"
  local expected_physical="$3"
  local expected_git_dir="$4"
  local expected_common_dir="$5"
  local expected_gitfile_sha="$6"
  local expected_backpointer_sha="$7"
  local expected_worktree_stat="$8"
  local expected_gitdir_stat="$9"

  if ! oms_worker_worktree_identity_capture "$repo" "$worker_tree"; then
    printf '%s\n' "$OMS_WORKER_IDENTITY_DETAIL"
    return 1
  fi
  if [ "$OMS_WORKER_IDENTITY_PHYSICAL" != "$expected_physical" ] ||
    [ "$OMS_WORKER_IDENTITY_GIT_DIR" != "$expected_git_dir" ] ||
    [ "$OMS_WORKER_IDENTITY_COMMON_DIR" != "$expected_common_dir" ] ||
    [ "$OMS_WORKER_IDENTITY_GITFILE_SHA" != "$expected_gitfile_sha" ] ||
    [ "$OMS_WORKER_IDENTITY_BACKPOINTER_SHA" != "$expected_backpointer_sha" ] ||
    [ "$OMS_WORKER_IDENTITY_WORKTREE_STAT" != "$expected_worktree_stat" ] ||
    [ "$OMS_WORKER_IDENTITY_GITDIR_STAT" != "$expected_gitdir_stat" ]; then
    printf 'delegated worktree physical path, inode, or Git backpointer changed\n'
    return 1
  fi
  return 0
}

oms_worker_surface_capture_one() {
  local repo="$1"
  local name="$2"
  local current_worktree_physical="${3:-}"
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
      # Local branch names grant no exemption. Foreign moves are classified
      # against the snapshot's checkout/log evidence by surface_diff.
      (git -C "$repo" show-ref 2>/dev/null || true) |
        LC_ALL=C awk '$2 != "refs/stash" && index($2, "refs/remotes/") != 1 &&
          index($2, "refs/codex/") != 1' |
        LC_ALL=C sort
      git -C "$repo" rev-parse HEAD 2>/dev/null || printf 'unborn\n'
      printf '%s HEAD-identity\n' "$(git -C "$repo" symbolic-ref -q HEAD 2>/dev/null || printf detached)"
      ;;
    work-branches)
      # Retain the snapshot slot; all local branches now belong to refs.
      :
      ;;
    remote-refs)
      # Keep the allow-list anchored: refs merely containing "refs/remotes"
      # still belong to the hard `refs` surface above.
      (git -C "$repo" show-ref 2>/dev/null || true) |
        LC_ALL=C awk '$2 == "refs/stash" || index($2, "refs/remotes/") == 1 ||
          index($2, "refs/codex/") == 1' |
        LC_ALL=C sort
      ;;
    tracked)
      # Status categories are not content: a file already marked ` M` stays
      # ` M` however much a worker rewrites it. More importantly, status can
      # consult attributes and an inherited global clean filter. Hash raw
      # index/tracked bytes only; this also avoids the filter execution path.
      printf 'tracked-state %s\n' \
        "$(oms_git_tracked_state_fingerprint "$repo" 2>/dev/null || printf unreadable)"
      ;;
    files)
      # Untracked AND ignored content, by stat rather than by reading bytes.
      # `git status --ignored` collapses a fully ignored directory to one entry,
      # so replacing .venv/bin/python or planting sitecustomize.py inside an
      # ignored tree is invisible to it. Enumerating with stat catches the
      # replacement without reading a single dataset byte. The walk is bounded:
      # a truncated scan says so instead of quietly covering less.
      # Untracked-but-not-ignored paths are scanned first and ignored ones
      # after, because the budget is spent in path order: a real ML repo whose
      # ignored data/ and runs/ trees hold tens of thousands of files would
      # otherwise eat the whole budget alphabetically and leave the handful of
      # genuinely new source files — the ones a stray worker write would land
      # in — unscanned. Truncation now only ever drops ignored churn.
      OMS_WG_REPO="$repo" OMS_WG_MAX="${OMS_WORKER_GUARD_MAX_FILES:-20000}" \
      OMS_WG_EXCLUDE="${OMS_WORKER_GUARD_EXCLUDE:-}" python3 - <<'PY'
import hashlib, os, subprocess, sys

root = os.environ["OMS_WG_REPO"]
try:
    budget = int(os.environ.get("OMS_WG_MAX", "20000"))
except ValueError:
    budget = 20000
def listing(args):
    try:
        raw = subprocess.check_output(
            ["git", "-c", "core.fsmonitor=false", "-C", root,
             "ls-files", "-z"] + args,
            stderr=subprocess.DEVNULL)
    except Exception:
        return []
    return sorted(p for p in raw.split(b"\0") if p)


untracked = listing(["--others", "--exclude-standard"])
seen_untracked = set(untracked)
ignored = [p for p in listing(["--others", "--ignored", "--exclude-standard"])
           if p not in seen_untracked]
paths = untracked + ignored
first_ignored = len(untracked)
count = 0
# The harness writes this run's own artifacts and patch somewhere; when that is
# inside the repo it is the harness moving, not the worker.
excluded = [".oms"]
for extra in (os.environ.get("OMS_WG_EXCLUDE") or "").splitlines():
    extra = extra.strip().strip("/")
    if extra:
        excluded.append(extra)
for index, rel in enumerate(paths):
    name = os.fsdecode(rel)
    if any(name == prefix or name.startswith(prefix + "/") for prefix in excluded):
        continue
    count += 1
    if count > budget:
        # Say which class was cut. "ignored" means every untracked source file
        # was still covered; "untracked" means the budget is too small for this
        # repository and the guard is genuinely partial.
        klass = "ignored" if index >= first_ignored else "untracked"
        print("TRUNCATED after %d entries (%s)" % (budget, klass))
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
      local transient_helper
      transient_helper="$(cd "${BASH_SOURCE[0]%/*}" && pwd -P)/worker-state-transients.py"
      OMS_WG_REPO="$repo" python3 - "$transient_helper" <<'PY'
import hashlib, importlib.util, os, re, stat, sys

root = os.path.join(os.environ["OMS_WG_REPO"], ".oms")
if not os.path.isdir(root):
    raise SystemExit(0)
rows = []
spec = importlib.util.spec_from_file_location("_oms_worker_state_transients", sys.argv[1])
transients = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transients)
for base, dirs, files in os.walk(root):
    # Delegation markers are ephemeral owner liveness. A sibling legitimately
    # creates/removes them while this worker runs; the exclusive provider-window
    # guard covers their boundary when a caller explicitly guarantees quiescence.
    dirs[:] = sorted(d for d in dirs
                     if not os.path.islink(os.path.join(base, d))
                     and not (base == root and d == "delegations")
                     # Canonical sync admission and call holders have the same
                     # ephemeral lifecycle as delegation liveness markers.
                     and not (base == os.path.join(root, "locks") and (d in (
                         "canonical-sync.d", "canonical-sync.gate",
                         "canonical-sync.gate.reclaim-queue")
                         # A call entry is staged here, then renamed into canonical-sync.d.
                         or d.startswith(".canonical-call."))))
    for name in sorted(files):
        path = os.path.join(base, name)
        rel = os.path.relpath(path, root).replace(os.sep, "/")
        if os.path.islink(path):
            rows.append("%s LINK" % rel)
            continue
        if transients.is_internal_atomic_temp(root, rel):
            continue
        if re.fullmatch(r"project-graph/cache/[0-9a-f]{64}\.json", rel):
            try:
                info = os.lstat(path)
                if stat.S_ISREG(info.st_mode):
                    digest = hashlib.sha256()
                    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
                    with os.fdopen(os.open(path, flags), "rb") as f:
                        opened = os.fstat(f.fileno())
                        if not stat.S_ISREG(opened.st_mode):
                            rows.append("%s EXISTS" % rel)
                            continue
                        for chunk in iter(lambda: f.read(1024 * 1024), b""):
                            digest.update(chunk)
                    rows.append("%s CACHE %d %s" % (rel, opened.st_size, digest.hexdigest()))
                else:
                    rows.append("%s EXISTS" % rel)
            except OSError:
                rows.append("%s UNREADABLE" % rel)
            continue
        # Append-only by contract: the JSONL families, plus the memory log and
        # its pins. summary.md is derived from shared.md and regenerated, and
        # plan/executor/run state is rewritten by the harness by design, so
        # those are tracked by presence alone.
        append_only = rel.endswith(".jsonl") or rel in (
            "memory/shared.md", "memory/pins.md")
        # The resume hint is a cache the parent's own prompt hook consumes
        # (hook_state.py panel_main_hint unlinks it) while a worker runs.
        if rel.startswith("hooks/sessions/") and rel.endswith(".resume.json"):
            continue
        # Existing launch/exit/pending records are immutable controller state.
        # New sibling records remain possible while another controller runs;
        # scope release independently probes their recorded process identities.
        immutable = transients.is_panel_call_record(rel)
        if not append_only and not immutable:
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
            rows.append("%s %s %d %s" % (rel, "IMMUTABLE" if immutable else "APPEND", size, digest.hexdigest()))
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
        local meta head worktree_gitfile worktree_path
        for meta in objects/info/alternates objects/info/http-alternates \
          info/exclude info/grafts info/attributes shallow; do
          if [ -f "$git_dir/$meta" ]; then
            printf '%s %s\n' "$meta" \
              "$(oms_sha256_file "$git_dir/$meta" 2>/dev/null || printf 'unreadable')"
          fi
        done
        if [ -d "$git_dir/worktrees" ]; then
          # Walk registry entries themselves rather than only regular HEAD and
          # gitdir leaves. `find -type f` silently skipped a symlink directory,
          # even though Git follows it as a duplicate worktree registration.
          # A live sibling is exempt only after its real gitdir/backpointer and
          # private liveness marker pass the validator above; every symlink and
          # every other registry entry remains part of the hard gitmeta surface.
          find "$git_dir/worktrees" -mindepth 1 -maxdepth 1 -print 2>/dev/null |
            LC_ALL=C sort |
            while IFS= read -r wt; do
              if [ -L "$wt" ]; then
                printf 'worktree-link %s %s\n' "${wt#"$git_dir"/}" \
                  "$(readlink "$wt" 2>/dev/null | oms_sha256_stream || printf 'unreadable')"
                continue
              fi
              # A sibling worktree removed between find and this test is gone,
              # not a planted file; recording it would fail the run on cleanup.
              [ -e "$wt" ] || continue
              if [ ! -d "$wt" ]; then
                printf 'worktree-entry %s non-directory\n' "${wt#"$git_dir"/}"
                continue
              fi
              # Liveness exempts checkout churn, never per-worktree execution
              # configuration or the wiring that makes Git find it.
              for meta in "$wt/gitdir" "$wt/commondir" "$wt/config.worktree"; do
                if [ -L "$meta" ]; then
                  printf 'worktree-link %s %s\n' "${meta#"$git_dir"/}" \
                    "$(readlink "$meta" 2>/dev/null | oms_sha256_stream || printf 'unreadable')"
                elif [ -f "$meta" ]; then
                  printf 'worktree %s %s\n' "${meta#"$git_dir"/}" \
                    "$(oms_sha256_file "$meta" 2>/dev/null || printf 'unreadable')"
                elif [ -e "$meta" ]; then
                  printf 'worktree-entry %s non-regular\n' "${meta#"$git_dir"/}"
                fi
              done
              if oms_worker_gitmeta_is_live_managed_worktree "$repo" "$wt/gitdir" \
                "$current_worktree_physical"; then
                [ -z "${OMS_WG_GITMETA_EXEMPT_OUT:-}" ] ||
                  printf '%s\n' "${wt#"$git_dir"/}" >> "$OMS_WG_GITMETA_EXEMPT_OUT"
                if [ -n "${OMS_WG_GITMETA_MANAGED_ADDED_OUT:-}" ]; then
                  if [ -f "$wt/HEAD" ] && [ ! -L "$wt/HEAD" ]; then
                    head="$(sed -n '1p' "$wt/HEAD" 2>/dev/null || true)"
                    head="${head//$'\r'/}"
                    case "$head" in
                      ""|*[!0-9a-f]*) ;;
                      *)
                        if [ "${#head}" = 40 ] || [ "${#head}" = 64 ]; then
                          printf '%s\n' "${wt##*/}" >> "$OMS_WG_GITMETA_MANAGED_ADDED_OUT"
                        fi
                        ;;
                    esac
                  else
                    worktree_gitfile="$(sed -n '1p' "$wt/gitdir" 2>/dev/null || true)"
                    worktree_gitfile="${worktree_gitfile//$'\r'/}"
                    case "$worktree_gitfile" in
                      */.git)
                        worktree_path="${worktree_gitfile%/.git}"
                        if oms_worker_gitmeta_is_pending_managed_worktree "$repo" \
                          "$worktree_path" "$current_worktree_physical"; then
                          printf '%s\n' "${wt##*/}" >> "$OMS_WG_GITMETA_MANAGED_ADDED_OUT"
                        fi
                        ;;
                    esac
                  fi
                fi
                continue
              fi
              # Dead harness residue stays on the surface; the snapshot only
              # notes it so a later cleanup of the whole entry can read softly.
              if [ -n "${OMS_WG_GITMETA_RESIDUE_OUT:-}" ] &&
                oms_worker_gitmeta_is_live_managed_worktree "$repo" "$wt/gitdir" \
                  "$current_worktree_physical" kept; then
                printf '%s\n' "${wt#"$git_dir"/}" >> "$OMS_WG_GITMETA_RESIDUE_OUT"
              fi
              printf 'worktree-dir %s\n' "${wt#"$git_dir"/}"
              meta="$wt/HEAD"
              if [ -L "$meta" ]; then
                printf 'worktree-link %s %s\n' "${meta#"$git_dir"/}" \
                  "$(readlink "$meta" 2>/dev/null | oms_sha256_stream || printf 'unreadable')"
              elif [ -f "$meta" ]; then
                printf 'worktree %s %s\n' "${meta#"$git_dir"/}" \
                  "$(oms_sha256_file "$meta" 2>/dev/null || printf 'unreadable')"
              elif [ -e "$meta" ]; then
                printf 'worktree-entry %s non-regular\n' "${meta#"$git_dir"/}"
              fi
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

oms_worker_surface_is_soft() {
  case "$1" in
    files|tracked|remote-refs|kept-sibling|sibling-cleanup|branch-tracking|foreign-worktree|foreign-ref) return 0 ;;
    *) return 1 ;;
  esac
}

# Capture each surface separately so a later comparison can name what moved.
oms_worker_surface_snapshot() {
  local repo="$1"
  local dir="$2"
  local current_worktree_physical="${3:-}"
  local name common entry target ref

  mkdir -p "$dir" || return 1
  : > "$dir/gitmeta-exempt"
  : > "$dir/gitmeta-residue"
  for name in config remotes refs remote-refs work-branches tracked files gitmeta hooks omsstate; do
    OMS_WG_GITMETA_EXEMPT_OUT="$dir/gitmeta-exempt" \
      OMS_WG_GITMETA_RESIDUE_OUT="$dir/gitmeta-residue" \
      oms_worker_surface_capture_one "$repo" "$name" \
      "$current_worktree_physical" > "$dir/$name" 2>/dev/null || true
  done
  {
    printf 'refs/heads/main\nrefs/heads/master\nHEAD-identity\n'
    git -C "$repo" symbolic-ref -q HEAD 2>/dev/null || true
    target="$(git -C "$repo" symbolic-ref -q --short HEAD 2>/dev/null || true)"
    target="${target//$'\r'/}"
    [ -z "$target" ] || git -C "$repo" config --get "branch.$target.merge" 2>/dev/null || true
    target="$(git -C "$repo" symbolic-ref -q refs/remotes/origin/HEAD 2>/dev/null || true)"
    target="${target//$'\r'/}"
    case "$target" in refs/remotes/origin/*) printf 'refs/heads/%s\n' "${target#refs/remotes/origin/}" ;; esac
    if [ -n "$current_worktree_physical" ]; then
      target="$(git -C "$current_worktree_physical" symbolic-ref -q HEAD 2>/dev/null || true)"
      target="${target//$'\r'/}"
      [ -z "$target" ] || printf '%s\n' "$target"
    fi
  } > "$dir/refs-protected"
  common="$(oms_harness_git_path_physical "$repo" --git-common-dir 2>/dev/null || true)"
  mkdir -p "$dir/foreign-worktrees"
  if [ -n "$common" ] && [ -n "$current_worktree_physical" ]; then
    for entry in "$common/worktrees/"*; do
      oms_worker_gitmeta_is_foreign_worktree "$repo" "$entry" "$current_worktree_physical" || continue
      mkdir -p "$dir/foreign-worktrees/${entry##*/}"
      printf '%s\n' "$OMS_WG_FOREIGN_PHYSICAL" > "$dir/foreign-worktrees/${entry##*/}/physical"
      printf '%s\n' "$OMS_WG_FOREIGN_PATH" > "$dir/foreign-worktrees/${entry##*/}/path"
      cp "$entry/HEAD" "$entry/gitdir" "$dir/foreign-worktrees/${entry##*/}/" || return 1
      if [ -f "$entry/logs/HEAD" ] && [ ! -L "$entry/logs/HEAD" ]; then
        cp "$entry/logs/HEAD" "$dir/foreign-worktrees/${entry##*/}/log" || return 1
      else
        : > "$dir/foreign-worktrees/${entry##*/}/log"
      fi
      ref="$(sed -n 's/^ref: //p' "$entry/HEAD")"; ref="${ref//$'\r'/}"
      case "$ref" in
        refs/heads/*)
          if git check-ref-format "$ref" && [ -f "$common/logs/$ref" ] && [ ! -L "$common/logs/$ref" ]; then
            cp "$common/logs/$ref" "$dir/foreign-worktrees/${entry##*/}/ref-log" || return 1
          fi ;;
      esac
    done
  fi
  mkdir -p "$dir/branch-logs"
  if [ -n "$common" ] && [ -d "$common/logs/refs/heads/oms" ] &&
    [ ! -L "$common/logs/refs/heads/oms" ]; then
    python3 - "$common/logs/refs/heads/oms" "$dir/branch-logs/oms" <<'PY' || return 1
import os, shutil, sys
root, out = sys.argv[1:]
for current, directories, files in os.walk(root, followlinks=False):
    directories[:] = [name for name in directories
                      if not os.path.islink(os.path.join(current, name))]
    for name in files:
        source = os.path.join(current, name)
        if os.path.islink(source) or not os.path.isfile(source):
            continue
        target = os.path.join(out, os.path.relpath(source, root))
        os.makedirs(os.path.dirname(target), exist_ok=True)
        shutil.copyfile(source, target)
PY
  fi
  # Rows the index store may legitimately compact away; verified against the
  # recorded prefix digest before use, so a later append here is harmless.
  if [ -f "$repo/.oms/artifacts/index.jsonl" ] && [ ! -L "$repo/.oms/artifacts/index.jsonl" ]; then
    cp "$repo/.oms/artifacts/index.jsonl" "$dir/omsstate.artifact-index" 2>/dev/null || true
    # Completion pins exist before the provider runs; a receipt created during the run pins nothing.
    python3 - "$OMS_COMMON_LIB_DIR" "$repo" "$dir/omsstate.completion-pins" <<'PY' 2>/dev/null || true
import json, os, runpy, sys
lib, repo, out = sys.argv[1:]
try:
    pins = runpy.run_path(os.path.join(lib, "artifact-index-store.py"))["completion_pins"](repo) or set()
except (Exception, SystemExit):
    pins = set()
with open(out, "w", encoding="utf-8") as handle:
    json.dump(sorted(pins), handle)
PY
  fi
}
