#!/usr/bin/env python3
"""Resolve provider releases into an exact, validated installation snapshot."""

from __future__ import annotations

import argparse
import copy
import importlib.util
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from oms_runtime.common import CoreError, atomic_write_json, ensure_private_dir


_spec = importlib.util.spec_from_file_location("tool_lock", Path(__file__).with_name("tool-lock.py"))
lock_tools = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(lock_tools)
LockError = lock_tools.LockError
MANIFEST_HOST = "antigravity-cli-auto-updater-974169037036.us-central1.run.app"
MAX_RESPONSE = 2 * 1024 * 1024


def stable_version(value, previous):
    if not isinstance(value, str) or not re.fullmatch(r"\d+\.\d+\.\d+", value):
        raise LockError("latest provider release must be a stable numeric version")
    baseline = re.match(r"v?(\d+)\.(\d+)\.(\d+)", previous)
    if tuple(map(int, value.split("."))) < tuple(map(int, baseline.groups())):
        raise LockError("latest provider release is older than the installed snapshot")
    return value


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise LockError("provider metadata redirects are not allowed")


def fetch_json(url):
    parsed = urllib.parse.urlsplit(url)
    if (parsed.scheme != "https" or parsed.netloc not in {"registry.npmjs.org", MANIFEST_HOST}
            or parsed.query or parsed.fragment):
        raise LockError("provider metadata URL is not an approved HTTPS endpoint")
    request = urllib.request.Request(url, headers={"Accept": "application/json", "User-Agent": "oms-provider-latest"})
    try:
        with urllib.request.build_opener(NoRedirect()).open(request, timeout=20) as response:
            payload = response.read(MAX_RESPONSE + 1)
        if len(payload) > MAX_RESPONSE:
            raise LockError("provider metadata exceeds size limit")
        return lock_tools.mapping(json.loads(payload), "provider metadata")
    except (OSError, ValueError, urllib.error.URLError) as exc:
        raise LockError("cannot read provider release metadata") from exc


def npm_release(package, release):
    url = "https://registry.npmjs.org/%s/%s" % (
        urllib.parse.quote(package, safe="@"), urllib.parse.quote(release, safe=""))
    data = fetch_json(url)
    actual = lock_tools.version(data.get("version"), "npm release version")
    if data.get("name") != package or (release != "latest" and actual != release):
        raise LockError("npm metadata does not match the requested package and version")
    dist = lock_tools.mapping(data.get("dist"), "npm release dist")
    tarball = lock_tools.https(dist.get("tarball"), "npm tarball", "registry.npmjs.org")
    expected = "/%s/-/%s-%s.tgz" % (package, package.rsplit("/", 1)[-1], actual)
    if urllib.parse.unquote(urllib.parse.urlsplit(tarball).path) != expected:
        raise LockError("npm tarball is not bound to the requested package and version")
    return actual, lock_tools.integrity(dist.get("integrity"), "npm release integrity")


def resolve(lock, providers):
    lock_tools.validate(lock)
    snapshot = copy.deepcopy(lock)
    for provider in dict.fromkeys(providers):
        if provider in {"claude", "codex"}:
            row = snapshot["npm"][provider]
            version, digest = npm_release(row["package"], "latest")
            row["version"] = stable_version(version, row["version"])
            row["integrity"] = digest
            for platform, native in row["native"].items():
                release = row["version"]
                if provider == "codex":
                    release += "-" + lock_tools.npm_platform_suffix(platform)
                native["version"], native["integrity"] = npm_release(native["package"], release)
        elif provider == "agy":
            section = snapshot["antigravity"]
            versions = set()
            for platform, row in section["platforms"].items():
                data = fetch_json("https://%s/manifests/%s.json" % (MANIFEST_HOST, platform.replace("-", "_")))
                version = stable_version(data.get("version"), section["version"])
                versions.add(version)
                url = lock_tools.https(data.get("url"), "agy release URL", "storage.googleapis.com")
                path = urllib.parse.urlsplit(url).path
                prefix = "/antigravity-public/antigravity-cli/%s-" % version
                if not path.startswith(prefix) or any(part in {".", ".."} for part in path.split("/")):
                    raise LockError("agy release URL is not bound to its official bucket and version")
                row.update(url=url, sha512=data.get("sha512"))
            if len(versions) != 1:
                raise LockError("agy release is not consistent across supported platforms; retry later")
            section["version"] = versions.pop()
        else:
            raise LockError("unknown provider: %s" % provider)
    lock_tools.validate(snapshot)
    return snapshot


def checked_path(path):
    path = Path(os.path.abspath(path))
    if any(candidate.is_symlink() for candidate in (path, *path.parents)):
        raise LockError("provider snapshot path must not contain symlinks")
    return path


def save_snapshot(path, snapshot):
    path = checked_path(path)
    ensure_private_dir(path.parent)
    atomic_write_json(path, snapshot)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lock", required=True, type=Path)
    parser.add_argument("--provider", action="append", choices=("codex", "claude", "agy"))
    parser.add_argument("--previous", type=Path, help="retain previously resolved providers, if this file exists")
    parser.add_argument("--output", type=Path, help="atomically save private snapshot instead of stdout")
    args = parser.parse_args()
    try:
        base = lock_tools.load(args.lock)
        if args.previous:
            previous_path = checked_path(args.previous)
            if previous_path.exists():
                previous = lock_tools.load(previous_path)
                for provider in ("claude", "codex"):
                    base["npm"][provider] = previous["npm"][provider]
                base["antigravity"] = previous["antigravity"]
        snapshot = resolve(base, args.provider or ["codex", "claude", "agy"])
        if args.output:
            save_snapshot(args.output, snapshot)
        else:
            print(json.dumps(snapshot, indent=2))
    except (LockError, CoreError, OSError) as exc:
        print("error: %s" % exc, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
