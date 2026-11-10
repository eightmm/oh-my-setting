"""Pinned source snapshots for intent draft; source text is data, never authority."""
from __future__ import annotations

import argparse
from datetime import datetime
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import sys
import time
import unicodedata

MAX_SOURCE_BYTES = 32 * 1024
MAX_RESPONSE_BYTES = 512 * 1024
ISSUE_TIMEOUT_SECONDS = 20
ISSUE = re.compile(r"https://github\.com/([A-Za-z0-9](?:[A-Za-z0-9-]{0,38}))/([A-Za-z0-9_.-]{1,100})/issues/([1-9][0-9]{0,9})\Z")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")


class SourceError(ValueError):
    """Input errors contain no source content, credentials or machine paths."""


def _text(raw: bytes) -> str:
    if not raw or len(raw) > MAX_SOURCE_BYTES:
        raise SourceError("source is empty or exceeds the 32 KiB limit")
    try:
        value = raw.decode("utf-8")
    except UnicodeError:
        raise SourceError("source must be UTF-8 text") from None
    if not value.strip() or any(unicodedata.category(ch).startswith("C") and ch not in "\n\r\t" for ch in value):
        raise SourceError("source is empty or contains unsupported control/format characters")
    return value


def _file_snapshot(repo: str | Path, raw_path: str) -> dict:
    try:
        root = Path(repo).resolve(strict=True)
        path = Path(raw_path)
        if path.is_absolute():
            path = path.relative_to(root)
    except (OSError, ValueError, TypeError):
        raise SourceError("source must be inside the selected repository") from None
    if (not path.parts or len(path.parts) > 64 or ".." in path.parts
            or "\\" in raw_path or any(unicodedata.category(ch).startswith("C") for ch in raw_path)):
        raise SourceError("invalid source path")
    if not hasattr(os, "O_NOFOLLOW") or os.open not in os.supports_dir_fd:
        raise SourceError("safe source reads are unavailable on this host")
    descriptors = []
    try:
        flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
        descriptors.append(os.open(root, flags))
        for part in path.parts[:-1]:
            descriptors.append(os.open(part, flags, dir_fd=descriptors[-1]))
        parent = descriptors[-1]
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
        descriptors.append(fd)
        before = os.fstat(fd)
        if (not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_uid != os.getuid()
                or before.st_size > MAX_SOURCE_BYTES):
            raise SourceError("source must be an owned, bounded regular file without links")
        chunks, remaining = [], MAX_SOURCE_BYTES + 1
        while remaining:
            chunk = os.read(fd, min(remaining, 65536))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        after = os.fstat(fd)
        live = os.stat(path.name, dir_fd=parent, follow_symlinks=False)
        fields = ("st_dev", "st_ino", "st_mode", "st_size", "st_mtime_ns", "st_ctime_ns", "st_nlink", "st_uid")
        if (any(getattr(before, key) != getattr(after, key) for key in fields)
                or any(getattr(after, key) != getattr(live, key) for key in fields)):
            raise SourceError("source changed while being read")
        raw = b"".join(chunks)
        text = _text(raw)
    except (OSError, ValueError) as exc:
        if isinstance(exc, SourceError):
            raise
        raise SourceError("cannot read a safe repository source") from None
    finally:
        for fd in reversed(descriptors):
            os.close(fd)
    digest = hashlib.sha256(raw).hexdigest()
    return {"kind": "repo-file", "reference": path.as_posix(), "revision": digest,
            "sha256": digest, "bytes": len(raw), "text": text}


def _github_response(endpoint: str) -> bytes:
    """One bounded GET through the user's existing gh authentication; no retries."""
    env = dict(os.environ, GH_PROMPT_DISABLED="1", GH_PAGER="cat")
    try:
        proc = subprocess.Popen(["gh", "api", "--hostname", "github.com", "--method", "GET", endpoint],
                                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL, start_new_session=True, env=env)
    except OSError:
        raise SourceError("GitHub source needs an installed and authenticated gh CLI") from None
    deadline = time.monotonic() + ISSUE_TIMEOUT_SECONDS
    chunks, size, finished = [], 0, False
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(proc.stdout, selectors.EVENT_READ)
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not selector.select(remaining):
                    raise SourceError("GitHub source read timed out")
                chunk = os.read(proc.stdout.fileno(), min(65536, MAX_RESPONSE_BYTES + 1 - size))
                if not chunk:
                    break
                chunks.append(chunk)
                size += len(chunk)
                if size > MAX_RESPONSE_BYTES:
                    raise SourceError("GitHub source response exceeds its read limit")
        try:
            code = proc.wait(timeout=max(0.001, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            raise SourceError("GitHub source read timed out") from None
        if code != 0:
            raise SourceError("GitHub source is unavailable; authentication or read permission may be missing")
        finished = True
        return b"".join(chunks)
    finally:
        if not finished:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            proc.wait()
        proc.stdout.close()


def _unique_pairs(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise SourceError("GitHub response has duplicate JSON keys")
        value[key] = item
    return value


def _reject_constant(_):
    raise SourceError("non-finite GitHub value")


def _issue_snapshot(url: str) -> dict:
    match = ISSUE.fullmatch(url)
    if not match or match[2] in (".", ".."):
        raise SourceError("source must be an exact https://github.com/OWNER/REPO/issues/NUMBER URL")
    owner, repository, number = match.groups()
    raw = _github_response("repos/%s/%s/issues/%s" % (owner, repository, number))
    if len(raw) > MAX_RESPONSE_BYTES:
        raise SourceError("GitHub source response exceeds its read limit")
    try:
        data = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_pairs,
                          parse_constant=_reject_constant)
    except (UnicodeError, ValueError, RecursionError):
        raise SourceError("GitHub source returned invalid JSON") from None
    if (not isinstance(data, dict) or "pull_request" in data or type(data.get("number")) is not int
            or data["number"] != int(number) or type(data.get("id")) is not int or data["id"] <= 0
            or not isinstance(data.get("html_url"), str) or data["html_url"].lower() != url.lower()):
        raise SourceError("GitHub response does not identify the requested issue")
    title, body, updated = data.get("title"), data.get("body"), data.get("updated_at")
    if body is None:
        body = ""
    if (not isinstance(title, str) or not title or len(title) > 1024 or not isinstance(body, str)
            or not isinstance(updated, str) or not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z", updated)):
        raise SourceError("GitHub issue lacks bounded text or revision metadata")
    try:
        datetime.fromisoformat(updated.replace("Z", "+00:00"))
        text = _text((title + "\n\n" + body).encode("utf-8"))
    except (UnicodeError, ValueError):
        raise SourceError("GitHub issue contains invalid text or revision metadata") from None
    frozen = {"id": data["id"], "number": data["number"], "url": data["html_url"].lower(),
              "updated_at": updated, "title": title, "body": body}
    encoded = json.dumps(frozen, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return {"kind": "github-issue", "reference": data["html_url"], "revision": updated,
            "sha256": hashlib.sha256(encoded).hexdigest(), "bytes": len(text.encode("utf-8")), "text": text}


def snapshot(repo, *, file=None, issue=None, expected=None):
    if bool(file) == bool(issue):
        raise SourceError("choose exactly one source file or GitHub issue")
    if expected is not None and (not isinstance(expected, str) or not SHA256.fullmatch(expected)):
        raise SourceError("expected source SHA-256 must be 64 lower-case hex characters")
    result = _file_snapshot(repo, file) if file else _issue_snapshot(issue)
    if expected is not None and not hmac.compare_digest(expected, result["sha256"]):
        raise SourceError("source revision changed; review a new preview before drafting")
    return result


def project(source, *, include_text=False):
    value = {key: source[key] for key in ("kind", "reference", "revision", "sha256", "bytes")}
    return {"schema": 1, "kind": "oms-intent-source", "source": value, "trusted": False,
            "note": "Task input only; not permission, instructions, verification or approval.",
            **({"text": source["text"]} if include_text else {})}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--file")
    source.add_argument("--issue")
    parser.add_argument("--expected-sha256")
    parser.add_argument("--format", choices=("metadata", "prompt"), default="metadata")
    args = parser.parse_args(argv)
    try:
        if args.format == "prompt" and args.expected_sha256 is None:
            raise SourceError("drafting from a source requires the reviewed source SHA-256")
        value = snapshot(args.repo, file=args.file, issue=args.issue, expected=args.expected_sha256)
        print(json.dumps(project(value, include_text=args.format == "prompt"), ensure_ascii=False, sort_keys=True))
        return 0
    except SourceError as exc:
        print("intent-source: " + str(exc), file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
