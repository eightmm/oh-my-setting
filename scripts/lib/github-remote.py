#!/usr/bin/env python3
"""Normalize the small GitHub remote surface accepted by draft-pr."""

from __future__ import annotations

import re
import sys
from typing import NoReturn
from urllib.parse import urlsplit


PART = re.compile(r"^[A-Za-z0-9_.-]+$")


def fail(message: str) -> NoReturn:
    print("error: %s" % message, file=sys.stderr)
    raise SystemExit(2)


def validate_slug(owner: str, repo: str) -> str:
    if repo.endswith(".git"):
        repo = repo[:-4]
    if (
        not owner
        or not repo
        or owner in (".", "..")
        or repo in (".", "..")
        or not PART.fullmatch(owner)
        or not PART.fullmatch(repo)
    ):
        fail("GitHub remote must name one owner/repository")
    return "%s/%s" % (owner, repo)


def normalize(value: str) -> str:
    if not value or any(ch in value for ch in "\r\n\0"):
        fail("remote URL is empty or malformed")

    scp = re.fullmatch(r"git@github\.com:([^/]+)/([^/]+)", value)
    if scp:
        return validate_slug(scp.group(1), scp.group(2))

    try:
        parsed = urlsplit(value)
    except ValueError:
        fail("remote URL is malformed")
    if parsed.scheme not in ("https", "ssh") or (parsed.hostname or "").lower() != "github.com":
        fail("draft PR v1 accepts only github.com HTTPS or SSH remotes")
    if parsed.query or parsed.fragment:
        fail("GitHub remote must not carry query or fragment data")
    try:
        port = parsed.port
    except ValueError:
        fail("GitHub remote has a malformed port")
    if port not in (None, 22 if parsed.scheme == "ssh" else 443):
        fail("GitHub remote uses a non-standard port")
    if parsed.scheme == "https" and (parsed.username or parsed.password):
        fail("credential-bearing GitHub URLs are not accepted")
    if parsed.scheme == "ssh" and (parsed.username not in (None, "git") or parsed.password):
        fail("SSH GitHub remote has an unexpected account or credential")
    parts = [part for part in parsed.path.split("/") if part]
    if len(parts) != 2:
        fail("GitHub remote must have exactly owner/repository")
    return validate_slug(parts[0], parts[1])


def main() -> int:
    if len(sys.argv) != 2:
        fail("usage: github-remote.py URL")
    print(normalize(sys.argv[1]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
