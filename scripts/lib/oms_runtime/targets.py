"""Same-path Git checkout management; registration grants no execution authority."""

from __future__ import annotations

import os
import re
import shlex
import signal
import subprocess
import unicodedata
from pathlib import Path
from typing import Any, Dict, Optional
from urllib.parse import urlsplit

from .common import (CoreError, MAX_STATE_BYTES, SECRET_VALUE_RE,
                     atomic_write_bytes, file_lock, pretty_json, read_json)


_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}")


def _name(value: Any, label: str) -> str:
    if not isinstance(value, str) or not _NAME.fullmatch(value):
        raise CoreError("%s must be 1..64 letters, digits, dots, underscores or hyphens, starting with a letter or digit" % label)
    return value


def _record(value: Any) -> Dict[str, str]:
    if not isinstance(value, dict) or set(value) != {"ssh_alias", "workdir"}:
        raise CoreError("remote target must contain only ssh_alias and workdir")
    _name(value["ssh_alias"], "SSH alias")
    workdir = value["workdir"]
    if (not isinstance(workdir, str) or
            any(unicodedata.category(char) == "Cc" for char in workdir)):
        raise CoreError("remote workdir must be a path without control characters")
    if not workdir.startswith("/") and (not workdir.startswith("~/") or not workdir[2:] or
            any(part in ("", ".", "..") for part in workdir[2:].split("/"))):
        raise CoreError("remote workdir must be ~/REL without dot components")
    if SECRET_VALUE_RE.search(workdir):
        raise CoreError("remote workdir contains sensitive-looking material")
    try:
        workdir.encode("utf-8")
    except UnicodeEncodeError:
        raise CoreError("remote workdir must be UTF-8")
    return value


def _path(repo: Path) -> Path:
    parent = repo
    for part in (".oms", "runtime"):
        parent = parent / part
        if parent.is_symlink() or (parent.exists() and not parent.is_dir()):
            raise CoreError("remote target state requires real directories")
    return parent / "targets.json"


def listing(repo: Path) -> Dict[str, Any]:
    """Read without creating state, including for harness children."""
    value = read_json(_path(repo), default={"schema": 1, "targets": {}}, limit=MAX_STATE_BYTES)
    if (not isinstance(value, dict) or set(value) != {"schema", "targets"} or
            type(value["schema"]) is not int or value["schema"] != 1 or
            not isinstance(value["targets"], dict)):
        raise CoreError("invalid remote target registry")
    for name, record in value["targets"].items():
        _name(name, "target name")
        _record(record)
    return value


def _update(repo: Path, name: str, record: Any) -> Dict[str, Any]:
    path = _path(repo)
    try:
        # Serialize the entire read/modify/replace; readers see the old or new
        # registry, and a failed write never truncates the existing one.
        with file_lock(path):
            value = listing(repo)
            entries = value["targets"]
            if record is None:
                if name not in entries:
                    raise CoreError("unknown remote target: %s" % name)
                del entries[name]
            else:
                if name in entries:
                    raise CoreError("remote target already exists: %s" % name)
                entries[name] = record
            encoded = pretty_json(value)
            if len(encoded) > MAX_STATE_BYTES:
                raise CoreError("remote target registry exceeds the state size limit")
            atomic_write_bytes(path, encoded)
    except OSError as exc:
        raise CoreError("cannot update remote target registry: %s" % exc)
    return value


def add(repo: Path, name: str, ssh_alias: str, workdir: Optional[str] = None) -> Dict[str, Any]:
    try:
        relative = repo.resolve().relative_to(Path.home().resolve()).as_posix()
    except ValueError:
        raise CoreError("repository is outside HOME; same-path remote targets require a repository under HOME")
    expected = "~/" + relative
    record = _record({"ssh_alias": ssh_alias, "workdir": expected})
    if relative == "." or (workdir is not None and workdir != expected):
        raise CoreError("remote workdir must equal the repository's resolved home-relative path: %s" % expected)
    return _update(repo, _name(name, "target name"), record)


def remove(repo: Path, name: str) -> Dict[str, Any]:
    return _update(repo, _name(name, "target name"), None)


def check(repo: Path, name: str) -> Dict[str, Any]:
    reason = "SSH target execution is not yet supported"
    valid = False
    try:
        _name(name, "target name")
        if name not in listing(repo)["targets"]:
            raise CoreError("unknown remote target: %s" % name)
        valid = True
    except CoreError as exc:
        reason = "%s; %s" % (exc, reason)
    return {"profile": "remote", "ready": False, "target_valid": valid,
            "reason": reason, "missing": ["ssh-execution-not-supported"],
            "details": {"target": name},
            "declared": {"transport": "ssh-alias"},
            "enforced": {"filesystem": False, "network": False},
            "unknown": ["SSH alias resolution", "remote filesystem", "remote network", "remote cleanup"],
            "warning": "Target metadata is local only; no SSH connection or isolation is established."}


_TIMEOUT = 120
_SYNC_TIMEOUT = 1800
_SHA = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})", re.I)
_REMOTE_SENTINEL = "\nOMS_TARGET_V1\n"
_REMOTE_REASONS = {"missing", "not-git", "dirty", "diverged", "behind", "foreign-origin",
                   "wrong-branch", "unpushed", "auth", "no-network", "would-overwrite-ignored", "would-overwrite-untracked",
                   "legacy-absolute", "timeout"}


def _identity(origin: str) -> str:
    """Compare repository identities without retaining transport credentials."""
    if any(ord(char) < 33 or ord(char) == 127 for char in origin):
        raise CoreError("origin must be a network SSH or HTTPS remote without credentials")
    scp = re.fullmatch(r"git@([^/:]+):([^/]+)/([^/]+)", origin)
    if scp:
        host, owner, project = scp.groups()
    else:
        try:
            parsed = urlsplit(origin)
            host = parsed.hostname or ""
            path = re.fullmatch(r"/([^/]+)/([^/]+)/?", parsed.path)
            if (parsed.scheme not in ("ssh", "https") or not host or path is None or
                    parsed.query or parsed.fragment or parsed.port is not None or
                    parsed.username not in ((None, "git") if parsed.scheme == "ssh" else (None,)) or
                    parsed.password):
                raise ValueError()
            owner, project = path.groups()
        except ValueError:
            raise CoreError("origin must be a network SSH or HTTPS remote without credentials")
    if host.lower() == "github.com":
        project = project.lower()
    project = project[:-4] if project.endswith(".git") else project
    if (not re.fullmatch(r"[A-Za-z0-9._-]+", host) or
            any(part in ("", ".", "..") or not re.fullmatch(r"[A-Za-z0-9._-]+", part)
                for part in (owner, project))):
        raise CoreError("origin must name a host and owner/repository")
    identity = "%s/%s/%s" % (host.lower(), owner, project)
    return identity.lower() if host.lower() == "github.com" else identity


def _network_reason(error: str) -> str:
    text = error.lower()
    if any(word in text for word in ("permission denied", "authentication", "could not read username",
                                     "host key", "remote host identification", "repository not found", "access denied")):
        return "auth"
    return "no-network"


def _run(command: Any, *, timeout: int, input: Optional[str] = None,
         **kwargs: Any) -> subprocess.CompletedProcess:
    if input is not None:
        kwargs["stdin"] = subprocess.PIPE
    with subprocess.Popen(command, **kwargs) as process:
        try:
            stdout, stderr = process.communicate(input=input, timeout=timeout)
        except subprocess.TimeoutExpired:
            try:
                if os.name == "nt":
                    process.kill()
                else:
                    os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.communicate()
            raise
        return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def _git(repo: Path, *args: str, timeout_seconds: int = _TIMEOUT) -> subprocess.CompletedProcess:
    environment = dict(os.environ, GIT_OPTIONAL_LOCKS="0", GIT_TERMINAL_PROMPT="0")
    environment.update({"SSH_ASKPASS_REQUIRE": "never", "GIT_ASKPASS": "false"})
    return _run(["git", "-C", str(repo), *args], stdout=subprocess.PIPE,
                stdin=subprocess.DEVNULL, start_new_session=True,
                stderr=subprocess.PIPE, text=True, timeout=timeout_seconds,
                env=environment)


# The wire response is NUL-delimited, so paths and branch names cannot inject
# fields. All user values arrive as positional arguments, never script text.
_REMOTE_SCRIPT = r'''
export GIT_OPTIONAL_LOCKS=0 GIT_TERMINAL_PROMPT=0 GIT_LITERAL_PATHSPECS=1 LC_ALL=C
# Git and its hooks must not consume subsequent commands from sh -s input.
git() { command git "$@" </dev/null; }
limited_git() {
    if command -v timeout >/dev/null 2>&1; then
        command timeout "$seconds" git "$@" </dev/null
    else
        git "$@"
    fi
}
mode=$1
workdir=$HOME/${2#\~/}
expected=$3
origin=$4
sha=$5
wanted=$6
seconds=$7
reason=
exists=false
is_git=false
matches=false
head=
branch=
clean=false
untracked=false
before=
after=
finish() {
    printf '\nOMS_TARGET_V1\n'
    printf '%s\000' "$reason" "$workdir" "$exists" "$is_git" "$matches" "$head" "$branch" "$clean" "$untracked" "$before" "$after"
    if [ -n "$reason" ]; then exit 3; fi
    exit 0
}
fail() { reason=$1; finish; }
network() {
    message=$("$@" 2>&1) && return 0
    case "$?" in 124|137) fail timeout ;; esac
    case "$message" in
        *'Permission denied'*|*'Authentication'*|*'authentication'*|*'could not read Username'*|*'Host key verification'*|*'Repository not found'*|*'Access denied'*) fail auth ;;
        *'find remote ref'*|*'Remote branch'*'not found'*) fail unpushed ;;
        *) fail no-network ;;
    esac
}
identity() {
    case "$1" in
        git@*:*) host=${1#git@}; path=${host#*:}; host=${host%%:*} ;;
        ssh://git@*/*) host=${1#ssh://git@}; path=${host#*/}; host=${host%%/*} ;;
        ssh://*/*) host=${1#ssh://}; path=${host#*/}; host=${host%%/*} ;;
        https://*/*) host=${1#https://}; path=${host#*/}; host=${host%%/*} ;;
        *) return 1 ;;
    esac
    host=$(printf '%s' "$host" | tr '[:upper:]' '[:lower:]')
    path=${path%/}
    case "$path" in */*) ;; *) return 1 ;; esac
    owner=${path%%/*}
    project=${path#*/}
    if [ "$host" = github.com ]; then project=$(printf '%s' "$project" | tr '[:upper:]' '[:lower:]'); fi
    project=${project%.git}
    case "$host" in ''|*[!a-z0-9._-]*) return 1 ;; esac
    case "$owner" in ''|.|..|*[!a-zA-Z0-9._-]*) return 1 ;; esac
    case "$project" in ''|.|..|*[!a-zA-Z0-9._-]*) return 1 ;; esac
    if [ "$host" = github.com ]; then
        printf '%s/%s/%s' "$host" "$owner" "$project" | tr '[:upper:]' '[:lower:]'
    else
        printf '%s/%s/%s' "$host" "$owner" "$project"
    fi
}
fresh=false
case "$HOME" in /*) ;; *) fail not-git ;; esac
if [ ! -e "$workdir" ] && [ ! -L "$workdir" ]; then
    [ "$mode" = sync ] || fail missing
    mkdir -p -- "${workdir%/*}" || fail not-git
    network limited_git clone --branch "$wanted" -- "$origin" "$workdir"
    fresh=true
fi
exists=true
cd -- "$workdir" 2>/dev/null || fail not-git
workdir=$(pwd -P)
[ "$(git rev-parse --is-inside-work-tree 2>/dev/null)" = true ] || fail not-git
# A directory nested in another checkout is not the registered checkout.
[ "$(git rev-parse --show-prefix 2>/dev/null)" = '' ] || fail not-git
is_git=true
head=$(git rev-parse --verify HEAD 2>/dev/null) || fail not-git
branch=$(git symbolic-ref --quiet --short HEAD 2>/dev/null) || branch=
if git diff --quiet --ignore-submodules=none -- && git diff --cached --quiet --ignore-submodules=none --; then clean=true; fi
[ -z "$(git ls-files --others --exclude-standard)" ] || untracked=true
before=$head
[ "$fresh" = false ] || before=
actual=$(git config --get remote.origin.url 2>/dev/null)
actual=$(identity "$actual") || fail foreign-origin
[ "$actual" = "$expected" ] || fail foreign-origin
matches=true
if [ "$mode" = status ]; then finish; fi
[ "$clean" = true ] || fail dirty
[ "$branch" = "$wanted" ] || fail wrong-branch
network limited_git fetch origin "+refs/heads/$wanted:refs/remotes/origin/$wanted"
git merge-base --is-ancestor "$sha" "refs/remotes/origin/$wanted" 2>/dev/null || fail unpushed
if ! git merge-base --is-ancestor "$head" "$sha"; then
    if git merge-base --is-ancestor "$sha" "$head"; then fail behind; fi
    fail diverged
fi
# The local target-tree check excludes newline filenames before SSH. Preserve
# all other filename bytes while converting NUL separators for POSIX read.
collision() {
    if [ -n "$(git ls-files --others --exclude-standard -- "$1")" ]; then exit 11; fi
    exit 10
}
(
git diff --no-renames --name-only --diff-filter=A -z "$head" "$sha" -- |
    tr '\000' '\n' |
    while IFS= read -r path; do
        if [ -e "$path" ] || [ -L "$path" ]; then
            if git cat-file -e "$head:$path" >/dev/null 2>&1; then
                [ -z "$(git ls-files --others -- "$path")" ] || collision "$path"
            else
                collision "$path"
            fi
        fi
        while :; do
            case "$path" in */*) path=${path%/*} ;; *) break ;; esac
            if [ -L "$path" ] || { [ -e "$path" ] && [ ! -d "$path" ]; }; then
                git cat-file -e "$head:$path" >/dev/null 2>&1 || collision "$path"
            fi
        done
    done
)
collision_status=$?
case "$collision_status" in 10) fail would-overwrite-ignored ;; 11) fail would-overwrite-untracked ;; esac
limited_git merge --ff-only "$sha" >/dev/null 2>&1 || {
    case "$?" in 124|137) fail timeout ;; *) fail diverged ;; esac
}
head=$(git rev-parse --verify HEAD) || fail not-git
[ "$head" = "$sha" ] || fail diverged
after=$head
finish
'''


def inspect_target(repo: Path, name: str, *, sync: bool = False,
                   sha: Optional[str] = None, timeout_seconds: Optional[int] = None) -> Dict[str, Any]:
    """Observe or advance the registered checkout without a command runner."""
    if timeout_seconds is None:
        timeout_seconds = _SYNC_TIMEOUT if sync else _TIMEOUT
    if type(timeout_seconds) is not int or not 10 <= timeout_seconds <= 7200:
        raise CoreError("--timeout must be between 10 and 7200 seconds")
    _name(name, "target name")
    row = listing(repo)["targets"].get(name)
    result: Dict[str, Any] = {"schema": 1, "target": name, "ok": False,
                              "reason": "missing"}
    if sync:
        result.update(before_sha=None, after_sha=None)
    if row is None:
        result["reason"] = "unknown-target"
        return result
    if row["workdir"].startswith("/"):
        result.update(reason="legacy-absolute", fix="oms runtime backend target remove %s && oms runtime backend target add %s --ssh-alias %s" %
                      (name, name, row["ssh_alias"]))
        return result
    try:
        origin = _git(repo, "config", "--get", "remote.origin.url", timeout_seconds=timeout_seconds).stdout.strip().rstrip("\r")
        try:
            identity = _identity(origin)
        except CoreError:
            result["reason"] = "foreign-origin"
            return result
        requested, branch = "", ""
        if sync:
            revision = sha if sha is not None else "HEAD"
            if not revision or (sha is not None and not _SHA.fullmatch(sha)):
                raise CoreError("--sha must be a full 40- or 64-character commit SHA")
            fetched = _git(repo, "fetch", "origin", timeout_seconds=timeout_seconds)
            if fetched.returncode:
                result["reason"] = _network_reason(fetched.stderr)
                return result
            commit = _git(repo, "rev-parse", "--verify", revision + "^{commit}", timeout_seconds=timeout_seconds)
            if commit.returncode:
                result["reason"] = "unpushed"
                result["message"] = "push first: requested commit is unavailable"
                return result
            requested = commit.stdout.strip().rstrip("\r")
            branch = _git(repo, "symbolic-ref", "--quiet", "--short", "HEAD", timeout_seconds=timeout_seconds).stdout.strip().rstrip("\r")
            if not branch:
                result["reason"] = "wrong-branch"
                return result
            tree = _git(repo, "ls-tree", "-r", "-z", "--name-only", requested, timeout_seconds=timeout_seconds)
            if tree.returncode:
                raise CoreError("cannot inspect requested commit paths")
            if "\n" in tree.stdout:
                raise CoreError("target filenames containing newlines are unsafe for remote sync")
            reachable = _git(repo, "for-each-ref", "--format=%(refname)", "--contains=" + requested,
                             "refs/remotes/origin/", timeout_seconds=timeout_seconds).stdout.strip()
            if not reachable:
                result.update(reason="unpushed", message="push first: requested commit is not on origin")
                return result
        values = ["sync" if sync else "status", row["workdir"], identity, origin, requested, branch, str(timeout_seconds)]
        command = [os.environ.get("OMS_SSH_BIN", "ssh"), "-a", "-o", "ClearAllForwardings=yes", "-o", "BatchMode=yes", "--",
                   row["ssh_alias"], "sh", "-s", "--", *[shlex.quote(value) for value in values]]
        remote = _run(command, input=_REMOTE_SCRIPT, stdout=subprocess.PIPE,
                      stderr=subprocess.PIPE, text=True, timeout=timeout_seconds,
                      start_new_session=True)
        _, sentinel, payload = remote.stdout.rpartition(_REMOTE_SENTINEL)
        fields = payload.split("\0")
        if not sentinel or len(fields) != 12 or fields[-1] or remote.returncode not in (0, 3):
            result["reason"] = _network_reason(remote.stderr)
            return result
        reason, workdir, exists, is_git, matches, head, current, clean, untracked, before, after = fields[:-1]
        if (reason not in _REMOTE_REASONS | {""} or
                (remote.returncode == 0) != (reason == "") or
                any(field not in ("true", "false") for field in (exists, is_git, matches, clean, untracked)) or
                not workdir.startswith("/") or
                any(value and not _SHA.fullmatch(value) for value in (head, before, after))):
            result["reason"] = _network_reason(remote.stderr)
            return result
        result.update(ok=remote.returncode == 0, reason=reason or None, workdir=workdir,
                      exists=exists == "true", is_git=is_git == "true", origin_matches=matches == "true",
                      head=head or None, branch=current or None, clean=clean == "true",
                      untracked=untracked == "true")
        if sync:
            result.update(before_sha=before or None, after_sha=after or None, requested_branch=branch)
        if reason == "wrong-branch":
            result["message"] = "remote branch %s differs from local branch %s" % (current or "(detached)", branch)
        if reason == "unpushed":
            result["message"] = "push first: requested commit is not on the remote origin branch"
        if reason == "timeout":
            result["state_unknown"] = True
        return result
    except subprocess.TimeoutExpired:
        result.update(reason="timeout", state_unknown=True)
        return result
    except (OSError, UnicodeError):
        result["reason"] = "no-network"
        return result
