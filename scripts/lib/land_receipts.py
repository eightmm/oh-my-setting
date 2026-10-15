"""Clone-local receipt range selection and producer-owned receipt handoff."""

import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile


def pushed_receipts(directory, sha, read):
    try:
        names = sorted(os.listdir(directory))
    except FileNotFoundError:
        names = []
    for name in names:
        prior = name.split("-", 1)[0]
        if (prior == sha or not name.endswith(".json") or
                not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", prior)):
            continue
        raw = read(os.path.join(directory, name))
        try:
            receipt = json.loads(raw)
        except (ValueError, UnicodeError):
            continue
        push = receipt.get("push") if isinstance(receipt, dict) else None
        if (isinstance(push, dict) and receipt.get("sha") == prior and
                type(push.get("rc")) is int and push["rc"] == 0 and push.get("moved") != 0):
            yield prior, name, raw


def range_base(chain, pushed):
    return next((c for c in chain[1:] if c in pushed), chain[1] if len(chain) > 1 else "")


def recorded_base(receipt):
    push = receipt.get("push", {})
    if "base" not in push:
        return None
    base = push["base"]
    if not isinstance(base, str) or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", base):
        raise ValueError("invalid receipt push.base")
    return base


def read_receipt(path):
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    flags |= getattr(os, "O_NONBLOCK", 0)
    if os.path.islink(path):
        raise ValueError("linked handoff receipt refused")
    fd = os.open(path, flags)
    with os.fdopen(fd, "rb") as handle:
        info = os.fstat(handle.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > 1024 * 1024:
            raise ValueError("handoff receipt must be a bounded regular file")
        raw = handle.read(1024 * 1024 + 1)
        if len(raw) > 1024 * 1024:
            raise ValueError("handoff receipt exceeds size limit")
        return raw


def git(repo, *args, local=False):
    environment = dict(os.environ, GIT_NO_LAZY_FETCH="1")
    if local:
        environment["GIT_ALLOW_PROTOCOL"] = ""
    result = subprocess.run(["git", "-C", repo, *args], env=environment,
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                            timeout=120, check=False)
    if result.returncode:
        raise ValueError("cannot verify or fetch receipt handoff destination")
    return result.stdout.decode("utf-8").strip().replace("\r", "")


def destination(repo, remote, push=False):
    args = ["remote", "get-url"] + (["--push"] if push else []) + ["--all", remote]
    urls = git(repo, *args, local=True).splitlines()
    if len(urls) != 1 or not urls[0]:
        raise ValueError("handoff requires one unambiguous destination URL")
    url = urls[0]
    # Resolve relative local remotes in their own checkout, never relative to
    # the handoff process. Network spellings must match exactly.
    if ":" not in url:
        return os.path.realpath(os.path.join(repo, url))
    return url


def safe_directory(directory):
    # XDG roots may use OS aliases such as macOS /var -> /private/var.
    # Resolve the parent, but refuse a linked receipt namespace itself.
    requested = Path(directory).absolute()
    path = requested.parent.resolve() / requested.name
    for part in [*reversed(path.parents), path]:
        try:
            part.mkdir(mode=0o700)
        except FileExistsError:
            pass
        if not stat.S_ISDIR(part.lstat().st_mode):
            raise ValueError("receipt namespace contains a link or non-directory")


def handoff(source, target, receipt_path, directory):
    raw = read_receipt(receipt_path)
    receipt = json.loads(raw)
    if (not isinstance(receipt, dict) or type(receipt.get("schema")) is not int or
            receipt["schema"] != 1 or receipt.get("state") != "passed"):
        raise ValueError("handoff requires a passed producer receipt")
    for stage in ("gate", "push"):
        row = receipt.get(stage)
        if not isinstance(row, dict) or type(row.get("rc")) is not int or row["rc"] != 0:
            raise ValueError("handoff requires successful gate and push")
    if not isinstance(receipt.get("ci"), dict) or receipt["ci"].get("conclusion") != "success":
        raise ValueError("receipt handoff requires CI success; skipped CI transfers nothing")
    sha, remote, branch = (receipt.get(key) for key in ("sha", "remote", "target"))
    if (not isinstance(sha, str) or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", sha) or
            not isinstance(remote, str) or not remote or remote.startswith("-") or
            not isinstance(branch, str) or not branch):
        raise ValueError("invalid producer receipt destination")
    name = os.path.basename(receipt_path)
    if receipt.get("request") != name[:-5] or not name.startswith(sha + "-") or not name.endswith(".json"):
        raise ValueError("producer receipt identity mismatch")
    git(target, "rev-parse", "--show-toplevel", local=True)
    if git(target, "rev-parse", "--is-bare-repository", local=True) != "false":
        raise ValueError("handoff target must be a git checkout")
    # request_stamp binds the push URL without recording credentials. Refuse
    # a remote configuration changed after push while CI was being checked.
    push_url = git(source, "remote", "get-url", "--push", remote, local=True)
    command = receipt["gate"].get("command")
    if not isinstance(command, str):
        raise ValueError("producer receipt lacks its gate command")
    request_digest = hashlib.sha256(json.dumps(
        [push_url, remote, branch, command], ensure_ascii=True).encode()).hexdigest()[:24]
    if receipt["request"] != sha + "-" + request_digest:
        raise ValueError("producer receipt push destination changed")
    if destination(source, remote, push=True) != destination(target, remote):
        raise ValueError("receipt handoff destination remote differs")
    ref = "refs/remotes/%s/%s" % (remote, branch)
    git(target, "check-ref-format", ref, local=True)
    git(target, "check-ref-format", "refs/heads/" + branch, local=True)
    # Fetch only the destination ref and objects; leave HEAD, index, worktree,
    # FETCH_HEAD and the linked-worktree registry untouched.
    git(target, "fetch", "--quiet", "--no-tags", "--no-write-fetch-head",
        "--no-recurse-submodules", remote, "+refs/heads/%s:%s" % (branch, ref))
    git(target, "merge-base", "--is-ancestor", sha, ref, local=True)
    predecessors = list(pushed_receipts(os.path.dirname(receipt_path), sha, read_receipt))
    if receipt["push"].get("moved") == 0:
        raise ValueError("this receipt reverified an already-landed SHA; hand off its landing receipt instead")
    base = recorded_base(receipt)
    if base is not None:
        git(target, "merge-base", "--is-ancestor", base, sha, local=True)
    elif any(recorded_base(json.loads(read_receipt(os.path.join(os.path.dirname(receipt_path), other)))) not in (None, sha)
             for other in sorted(os.listdir(os.path.dirname(receipt_path)))
             if other != name and other.startswith(sha + "-") and other.endswith(".json")):
        # A reverify carries no range; only the landing receipt that recorded push.base defines it.
        raise ValueError("this receipt reverified an already-landed SHA; hand off its landing receipt instead")
    else:
        chain = git(source, "rev-list", "--first-parent", "--max-count=1000", sha, local=True).split()
        if git(target, "rev-list", "--first-parent", "--max-count=1000", sha, local=True).split() != chain:
            raise ValueError("handoff requires the same local first-parent history")
        base = range_base(chain, {row[0] for row in predecessors})
        existing = list(pushed_receipts(directory, sha, read_receipt))
        if range_base(chain, {row[0] for row in existing} | {prior for prior, _, _ in predecessors if prior == base}) != base:
            raise ValueError("target receipts would change the producer's landed range")
    transfers = {n: data for prior, n, data in predecessors if prior == base}
    # Publish the primary receipt last, after its range boundary exists.
    transfers[name] = raw
    safe_directory(directory)
    for filename, data in transfers.items():
        path = os.path.join(directory, filename)
        if os.path.lexists(path) and read_receipt(path) != data:
            raise ValueError("conflicting existing handoff receipt: " + filename)
    for filename, data in transfers.items():
        path = os.path.join(directory, filename)
        fd, temporary = tempfile.mkstemp(prefix=".handoff-", dir=directory)
        try:
            with os.fdopen(fd, "wb") as handle:
                handle.write(data)
                handle.flush()
                os.fsync(handle.fileno())
            try:
                # A same-filesystem hard link publishes complete bytes without
                # the replace/overwrite race of rename or exclusive writes.
                os.link(temporary, path)
            except FileExistsError:
                if read_receipt(path) != data:
                    raise ValueError("conflicting existing handoff receipt: " + filename)
            if hashlib.sha256(read_receipt(path)).digest() != hashlib.sha256(data).digest():
                raise ValueError("receipt handoff digest mismatch")
        finally:
            os.unlink(temporary)
    print("receipt handoff: %d receipt(s); sha256 %s" %
          (len(transfers), hashlib.sha256(raw).hexdigest()))


if __name__ == "__main__":
    try:
        handoff(*sys.argv[1:])
    except (OSError, ValueError, subprocess.TimeoutExpired) as exc:
        print("error: receipt handoff: %s" % exc, file=sys.stderr)
        sys.exit(1)
