#!/usr/bin/env python3
"""Validate and read the pinned bootstrap tool supply-chain contract."""

from __future__ import annotations

import argparse
import base64
import datetime
import hashlib
import hmac
import json
import os
import posixpath
import re
import shutil
import stat
import sys
import tarfile
import tempfile
import urllib.parse
import zipfile
from pathlib import Path
from pathlib import PurePosixPath
from typing import Any, Dict, Optional, Sequence


class LockError(Exception):
    pass


SEMVER = re.compile(r"^v?\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?$")
HEX64 = re.compile(r"^[0-9a-f]{64}$")
HEX128 = re.compile(r"^[0-9a-f]{128}$")
PLATFORMS = {
    "darwin-amd64",
    "darwin-arm64",
    "linux-amd64",
    "linux-arm64",
    "windows-amd64",
    "windows-arm64",
}
CLAUDE_PLATFORMS = PLATFORMS | {"linux-amd64-musl", "linux-arm64-musl"}
DRIVE_RELATIVE = re.compile(r"^[A-Za-z]:")

NODE_ARTIFACTS = {
    "darwin-amd64": "darwin-x64",
    "darwin-arm64": "darwin-arm64",
    "linux-amd64": "linux-x64",
    "linux-arm64": "linux-arm64",
}
UV_ARTIFACTS = {
    "darwin-amd64": "uv-x86_64-apple-darwin.tar.gz",
    "darwin-arm64": "uv-aarch64-apple-darwin.tar.gz",
    "linux-amd64": "uv-x86_64-unknown-linux-gnu.tar.gz",
    "linux-arm64": "uv-aarch64-unknown-linux-gnu.tar.gz",
    "windows-amd64": "uv-x86_64-pc-windows-msvc.zip",
    "windows-arm64": "uv-aarch64-pc-windows-msvc.zip",
}
ANTIGRAVITY_ARTIFACTS = {
    "darwin-amd64": "darwin-x64/cli_mac_x64.tar.gz",
    "darwin-arm64": "darwin-arm/cli_mac_arm64.tar.gz",
    "linux-amd64": "linux-x64/cli_linux_x64.tar.gz",
    "linux-arm64": "linux-arm/cli_linux_arm64.tar.gz",
    "windows-amd64": "windows-x64/cli_windows_x64.exe",
    "windows-arm64": "windows-arm/cli_windows_arm64.exe",
}
GH_ARTIFACTS = {
    "darwin-amd64": "macOS_amd64.zip",
    "darwin-arm64": "macOS_arm64.zip",
    "linux-amd64": "linux_amd64.tar.gz",
    "linux-arm64": "linux_arm64.tar.gz",
    "windows-amd64": "windows_amd64.zip",
    "windows-arm64": "windows_arm64.zip",
}


def text(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value or any(ch in value for ch in "\r\n\t"):
        raise LockError("%s must be a non-empty one-line string" % label)
    return value


def version(value: Any, label: str) -> str:
    value = text(value, label)
    if not SEMVER.fullmatch(value):
        raise LockError("%s must be an exact semantic version" % label)
    return value


def https(value: Any, label: str, host: Optional[str] = None) -> str:
    value = text(value, label)
    try:
        parsed = urllib.parse.urlsplit(value)
        port = parsed.port
    except ValueError as exc:
        raise LockError("%s must be a valid HTTPS URL" % label) from exc
    if (
        parsed.scheme != "https"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or port is not None
        or parsed.query
        or parsed.fragment
        or (host is not None and parsed.hostname.lower() != host)
        or any(marker in value for marker in ("latest", "${", "*"))
    ):
        raise LockError("%s must be an exact HTTPS URL" % label)
    return value


def url_path_endswith(url: str, suffix: str, label: str) -> None:
    parsed = urllib.parse.urlsplit(url)
    if parsed.query or parsed.fragment or not parsed.path.endswith("/" + suffix):
        raise LockError("%s is not bound to its platform artifact" % label)


def mapping(value: Any, label: str) -> Dict[str, Any]:
    if not isinstance(value, dict):
        raise LockError("%s must be an object" % label)
    return value


def exact_keys(value: Dict[str, Any], expected: Sequence[str], label: str) -> None:
    wanted = set(expected)
    if set(value) != wanted:
        missing = sorted(wanted - set(value))
        extra = sorted(set(value) - wanted)
        details = []
        if missing:
            details.append("missing %s" % ", ".join(missing))
        if extra:
            details.append("unexpected %s" % ", ".join(extra))
        raise LockError("%s fields are invalid (%s)" % (label, "; ".join(details)))


def integrity(value: Any, label: str) -> str:
    value = text(value, label)
    try:
        parse_sri(value)
    except LockError as exc:
        raise LockError("%s must be a sha512 SRI digest" % label) from exc
    return value


def npm_platform_suffix(platform: str) -> str:
    os_name, arch, *variant = platform.split("-")
    npm_arch = "x64" if arch == "amd64" else arch
    suffix = "%s-%s" % ("win32" if os_name == "windows" else os_name, npm_arch)
    if variant:
        suffix += "-" + "-".join(variant)
    return suffix


def validate(lock: Dict[str, Any]) -> None:
    if lock.get("schema") != 2:
        raise LockError("unsupported tool lock schema")
    exact_keys(
        lock,
        ("schema", "updated_at", "python", "node", "nvm", "uv", "npm", "antigravity", "gh"),
        "tool lock",
    )
    updated_at = text(lock.get("updated_at"), "updated_at")
    try:
        datetime.date.fromisoformat(updated_at)
    except ValueError as exc:
        raise LockError("updated_at must be a real YYYY-MM-DD date") from exc
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", updated_at):
        raise LockError("updated_at must be YYYY-MM-DD")
    python = mapping(lock.get("python"), "python")
    exact_keys(python, ("version",), "python")
    version(python.get("version"), "python.version")
    node = mapping(lock.get("node"), "node")
    exact_keys(node, ("version", "platforms"), "node")
    node_version = version(node.get("version"), "node.version")
    node_platforms = mapping(node.get("platforms"), "node.platforms")
    unix_platforms = {name for name in PLATFORMS if not name.startswith("windows-")}
    if set(node_platforms) != unix_platforms:
        raise LockError("node must lock all four supported Unix platforms")
    for platform, raw in node_platforms.items():
        row = mapping(raw, "node.platforms.%s" % platform)
        exact_keys(row, ("url", "sha256", "archive"), "node.platforms.%s" % platform)
        url = https(row.get("url"), "node %s url" % platform, "nodejs.org")
        if node_version.lstrip("v") not in url or "nodejs.org/dist/" not in url:
            raise LockError("node %s URL is not version-pinned" % platform)
        url_path_endswith(
            url,
            "node-v%s-%s.tar.gz" % (node_version.lstrip("v"), NODE_ARTIFACTS[platform]),
            "node %s URL" % platform,
        )
        if not HEX64.fullmatch(str(row.get("sha256", ""))):
            raise LockError("node %s has invalid sha256" % platform)
        if row.get("archive") != "tar.gz":
            raise LockError("node %s has invalid archive type" % platform)

    nvm = mapping(lock.get("nvm"), "nvm")
    exact_keys(nvm, ("version", "url", "sha256", "script_sha256"), "nvm")
    nvm_version = version(nvm.get("version"), "nvm.version")
    nvm_url = https(nvm.get("url"), "nvm.url", "github.com")
    if (
        nvm_version not in nvm_url
        or not HEX64.fullmatch(str(nvm.get("sha256", "")))
        or not HEX64.fullmatch(str(nvm.get("script_sha256", "")))
    ):
        raise LockError("nvm source and script are not pinned to their version and sha256")
    url_path_endswith(
        nvm_url,
        "nvm-sh/nvm/archive/refs/tags/%s.tar.gz" % nvm_version,
        "nvm URL",
    )

    uv = mapping(lock.get("uv"), "uv")
    exact_keys(uv, ("version", "platforms"), "uv")
    uv_version = version(uv.get("version"), "uv.version")
    uv_platforms = mapping(uv.get("platforms"), "uv.platforms")
    if set(uv_platforms) != PLATFORMS:
        raise LockError("uv must lock all six supported platforms")
    for platform, raw in uv_platforms.items():
        row = mapping(raw, "uv.platforms.%s" % platform)
        exact_keys(row, ("url", "sha256", "archive"), "uv.platforms.%s" % platform)
        uv_url = https(row.get("url"), "uv %s url" % platform, "github.com")
        if uv_version not in uv_url or not HEX64.fullmatch(str(row.get("sha256", ""))):
            raise LockError("uv %s is not version- and digest-pinned" % platform)
        url_path_endswith(uv_url, UV_ARTIFACTS[platform], "uv %s URL" % platform)
        expected_archive = "zip" if platform.startswith("windows-") else "tar.gz"
        if row.get("archive") != expected_archive:
            raise LockError("uv %s has invalid archive type" % platform)

    npm = mapping(lock.get("npm"), "npm")
    if set(npm) != {"claude", "codex", "ntn"}:
        raise LockError("npm lock must contain claude, codex, and ntn")
    expected = {
        "claude": ("@anthropic-ai/claude-code", "claude"),
        "codex": ("@openai/codex", "codex"),
        "ntn": ("ntn", "ntn"),
    }
    for name, (package, binary) in expected.items():
        row = mapping(npm.get(name), "npm.%s" % name)
        common_fields = {"package", "binary", "version", "integrity"}
        expected_fields = common_fields | ({"native"} if name in {"claude", "codex"} else set())
        exact_keys(row, tuple(expected_fields), "npm.%s" % name)
        if text(row.get("package"), "npm.%s.package" % name) != package:
            raise LockError("npm.%s package changed" % name)
        if text(row.get("binary"), "npm.%s.binary" % name) != binary:
            raise LockError("npm.%s binary changed" % name)
        version(row.get("version"), "npm.%s.version" % name)
        integrity(row.get("integrity"), "npm.%s.integrity" % name)

        if name not in {"claude", "codex"}:
            continue
        native = mapping(row.get("native"), "npm.%s.native" % name)
        expected_platforms = CLAUDE_PLATFORMS if name == "claude" else PLATFORMS
        if set(native) != expected_platforms:
            raise LockError(
                "npm.%s.native must lock every supported platform variant" % name
            )
        main_version = str(row.get("version"))
        for platform, raw_native in native.items():
            native_row = mapping(raw_native, "npm.%s.native.%s" % (name, platform))
            exact_keys(
                native_row,
                ("alias", "package", "version", "integrity"),
                "npm.%s.native.%s" % (name, platform),
            )
            suffix = npm_platform_suffix(platform)
            if name == "claude":
                expected_alias = "@anthropic-ai/claude-code-%s" % suffix
                expected_package = expected_alias
                expected_version = main_version
            else:
                expected_alias = "@openai/codex-%s" % suffix
                expected_package = "@openai/codex"
                expected_version = "%s-%s" % (main_version, suffix)
            if text(native_row.get("alias"), "native alias") != expected_alias:
                raise LockError("npm.%s.native.%s alias changed" % (name, platform))
            if text(native_row.get("package"), "native package") != expected_package:
                raise LockError("npm.%s.native.%s package changed" % (name, platform))
            if version(native_row.get("version"), "native version") != expected_version:
                raise LockError("npm.%s.native.%s version changed" % (name, platform))
            integrity(
                native_row.get("integrity"),
                "npm.%s.native.%s.integrity" % (name, platform),
            )

    for family, digest_name, digest_re in (
        ("antigravity", "sha512", HEX128),
        ("gh", "sha256", HEX64),
    ):
        section = mapping(lock.get(family), family)
        exact_keys(section, ("version", "platforms"), family)
        family_version = version(section.get("version"), "%s.version" % family)
        platforms = mapping(section.get("platforms"), "%s.platforms" % family)
        if set(platforms) != PLATFORMS:
            raise LockError("%s must lock all six supported platforms" % family)
        for platform, raw in platforms.items():
            row = mapping(raw, "%s.platforms.%s" % (family, platform))
            exact_keys(
                row,
                ("url", digest_name, "archive"),
                "%s.platforms.%s" % (family, platform),
            )
            url = https(
                row.get("url"),
                "%s %s url" % (family, platform),
                "storage.googleapis.com" if family == "antigravity" else "github.com",
            )
            if family_version.lstrip("v") not in url:
                raise LockError("%s %s URL is not version-pinned" % (family, platform))
            suffix = (
                ANTIGRAVITY_ARTIFACTS[platform]
                if family == "antigravity"
                else "gh_%s_%s" % (family_version.lstrip("v"), GH_ARTIFACTS[platform])
            )
            url_path_endswith(url, suffix, "%s %s URL" % (family, platform))
            if not digest_re.fullmatch(str(row.get(digest_name, ""))):
                raise LockError("%s %s has invalid %s" % (family, platform, digest_name))
            if family == "antigravity":
                expected_archive = "binary" if platform.startswith("windows-") else "tar.gz"
            else:
                expected_archive = "tar.gz" if platform.startswith("linux-") else "zip"
            if row.get("archive") != expected_archive:
                raise LockError("%s %s has invalid archive type" % (family, platform))


def load(path: Path) -> Dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise LockError("cannot read tool lock: %s" % path) from exc
    value = mapping(value, "tool lock")
    validate(value)
    return value


def lookup(value: Dict[str, Any], dotted: str) -> Any:
    current: Any = value
    for part in dotted.split("."):
        if not part or not isinstance(current, dict) or part not in current:
            raise LockError("unknown tool lock field: %s" % dotted)
        current = current[part]
    if isinstance(current, (dict, list)):
        raise LockError("tool lock field is not scalar: %s" % dotted)
    return current


def parse_sri(value: str) -> bytes:
    if not value.startswith("sha512-"):
        raise LockError("integrity must use sha512")
    try:
        digest = base64.b64decode(value[7:], validate=True)
    except (ValueError, TypeError) as exc:
        raise LockError("integrity is invalid base64") from exc
    if len(digest) != 64:
        raise LockError("integrity is not a sha512 digest")
    return digest


def verify_sri(path: Path, integrity: str) -> None:
    expected = parse_sri(integrity)
    actual = hashlib.sha512()
    try:
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                actual.update(chunk)
    except OSError as exc:
        raise LockError("cannot read package payload: %s" % path) from exc
    if not hmac.compare_digest(actual.digest(), expected):
        raise LockError("package integrity mismatch")


def safe_parts(name: str, label: str) -> Sequence[str]:
    if not name or "\x00" in name or "\\" in name or DRIVE_RELATIVE.match(name):
        raise LockError("%s has an unsafe path" % label)
    path = PurePosixPath(name)
    parts = path.parts
    if path.is_absolute() or not parts or any(
        part in ("", ".", "..") or ":" in part for part in parts
    ):
        raise LockError("%s has an unsafe path" % label)
    if parts[0].endswith(":") or DRIVE_RELATIVE.match(parts[0]):
        raise LockError("%s has an unsafe drive path" % label)
    return parts


def safe_link(member_name: str, link_name: str, *, root_relative: bool) -> None:
    if (
        not link_name
        or "\x00" in link_name
        or "\\" in link_name
        or posixpath.isabs(link_name)
        or DRIVE_RELATIVE.match(link_name)
    ):
        raise LockError("archive link has an unsafe path")
    if root_relative:
        combined = link_name
    else:
        combined = posixpath.join(posixpath.dirname(member_name), link_name)
    normalized = posixpath.normpath(combined)
    if normalized == ".." or normalized.startswith("../") or posixpath.isabs(normalized):
        raise LockError("archive link escapes the destination")
    safe_parts(normalized, "archive link")


def real_parent(root: Path, path: Path) -> Path:
    """Create path parents without ever traversing an archive-created link."""
    current = root
    for part in path.relative_to(root).parts[:-1]:
        current = current / part
        if current.is_symlink():
            raise LockError("archive member parent is a symbolic link")
        if current.exists():
            if not current.is_dir():
                raise LockError("archive member parent is not a directory")
        else:
            current.mkdir()
    return current


def extract_tar_members(archive: tarfile.TarFile, records: Sequence[Any], root: Path) -> None:
    """Extract regular entries first, then links, with no link-following writes."""
    directories = []
    regular = {}
    links = []

    for member, parts, key in records:
        target = root.joinpath(*parts)
        if member.isdir():
            real_parent(root, target)
            if target.is_symlink() or (target.exists() and not target.is_dir()):
                raise LockError("archive directory conflicts with another member")
            target.mkdir(exist_ok=True)
            directories.append((target, member.mode))
        elif member.isfile():
            real_parent(root, target)
            if target.exists() or target.is_symlink():
                raise LockError("archive file conflicts with another member")
            source_handle = archive.extractfile(member)
            if source_handle is None:
                raise LockError("cannot read archive member")
            with source_handle, target.open("xb") as output:
                shutil.copyfileobj(source_handle, output, length=1024 * 1024)
            if target.stat().st_size != member.size:
                raise LockError("archive member size changed during extraction")
            target.chmod(member.mode & 0o777)
            regular[key] = target
        else:
            links.append((member, parts, key, target))

    # A hard link may point only at a regular member from this same archive.
    # That excludes link chains and host files while preserving legitimate
    # release archives that deduplicate ordinary files.
    for member, _parts, _key, target in links:
        if not member.islnk():
            continue
        real_parent(root, target)
        if target.exists() or target.is_symlink():
            raise LockError("archive hard link conflicts with another member")
        link_key = "/".join(safe_parts(member.linkname, "archive hard link"))
        source_target = regular.get(link_key)
        if source_target is None:
            raise LockError("archive hard link does not target a regular member")
        os.link(str(source_target), str(target))

    symlinks = []
    for member, _parts, _key, target in links:
        if not member.issym():
            continue
        real_parent(root, target)
        if target.exists() or target.is_symlink():
            raise LockError("archive symbolic link conflicts with another member")
        os.symlink(member.linkname, str(target))
        symlinks.append(target)

    resolved_root = root.resolve()
    for target in symlinks:
        try:
            target.resolve(strict=False).relative_to(resolved_root)
        except (OSError, RuntimeError, ValueError) as exc:
            raise LockError("archive symbolic-link graph escapes the destination") from exc

    # Apply directory modes only after children and links are complete, so a
    # read-only directory in the archive cannot break safe extraction midway.
    for target, mode in reversed(directories):
        target.chmod(mode & 0o777)


def safe_extract(archive_type: str, source: Path, destination: Path) -> None:
    try:
        destination.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        raise LockError("cannot create extraction destination") from exc
    if destination.is_symlink() or not destination.is_dir() or any(destination.iterdir()):
        raise LockError("extraction destination must be an empty directory")
    max_members = 100000
    max_bytes = 2 * 1024 * 1024 * 1024
    try:
        if archive_type == "tar.gz":
            with tarfile.open(str(source), mode="r:gz") as archive:
                members = archive.getmembers()
                if len(members) > max_members:
                    raise LockError("archive has too many members")
                total = 0
                records = []
                seen = set()
                for member in members:
                    parts = tuple(safe_parts(member.name, "archive member"))
                    key = "/".join(parts)
                    if key in seen:
                        raise LockError("archive contains a duplicate member path")
                    seen.add(key)
                    if not (
                        member.isfile()
                        or member.isdir()
                        or member.issym()
                        or member.islnk()
                    ):
                        raise LockError("archive contains a special file")
                    if member.issym():
                        safe_link(member.name, member.linkname, root_relative=False)
                    elif member.islnk():
                        safe_link(member.name, member.linkname, root_relative=True)
                    total += max(0, int(member.size))
                    if total > max_bytes:
                        raise LockError("archive expands beyond the size limit")
                    records.append((member, parts, key))
                extract_tar_members(archive, records, destination)
        elif archive_type == "zip":
            with zipfile.ZipFile(str(source)) as archive:
                members = archive.infolist()
                if len(members) > max_members:
                    raise LockError("archive has too many members")
                total = 0
                seen = set()
                for member in members:
                    parts = safe_parts(member.filename.rstrip("/"), "archive member")
                    key = "/".join(parts)
                    if key in seen:
                        raise LockError("zip archive contains a duplicate member path")
                    seen.add(key)
                    mode = (member.external_attr >> 16) & 0xFFFF
                    if stat.S_ISLNK(mode):
                        raise LockError("zip archive contains a symbolic link")
                    total += max(0, int(member.file_size))
                    if total > max_bytes:
                        raise LockError("archive expands beyond the size limit")
                archive.extractall(str(destination))
        else:
            raise LockError("unsupported archive type: %s" % archive_type)
    except (OSError, tarfile.TarError, zipfile.BadZipFile) as exc:
        raise LockError("cannot extract verified archive") from exc

    root = destination.resolve()
    for path in destination.rglob("*"):
        try:
            path.resolve().relative_to(root)
        except (OSError, ValueError) as exc:
            raise LockError("extracted path escapes the destination") from exc


def npm_archive_manifest(source: Path) -> Dict[str, Any]:
    """Read a package manifest only after validating every tar member."""
    max_members = 100000
    max_bytes = 2 * 1024 * 1024 * 1024
    try:
        with tarfile.open(str(source), mode="r:gz") as archive:
            members = archive.getmembers()
            if len(members) > max_members:
                raise LockError("npm package has too many members")
            total = 0
            manifest_member = None
            for member in members:
                safe_parts(member.name, "npm package member")
                if not (member.isfile() or member.isdir()):
                    raise LockError("npm package contains an unsafe special file")
                total += max(0, int(member.size))
                if total > max_bytes:
                    raise LockError("npm package expands beyond the size limit")
                if member.name == "package/package.json":
                    manifest_member = member
            if manifest_member is None or not manifest_member.isfile() or manifest_member.size > 1024 * 1024:
                raise LockError("npm package has no bounded package.json")
            handle = archive.extractfile(manifest_member)
            if handle is None:
                raise LockError("cannot read npm package.json")
            raw = handle.read()
    except (OSError, tarfile.TarError) as exc:
        raise LockError("cannot read verified npm package") from exc
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise LockError("npm package.json is invalid") from exc
    return mapping(value, "npm package.json")


def verify_npm_package(source: Path, expected_name: str, expected_version: str) -> None:
    manifest = npm_archive_manifest(source)
    if manifest.get("name") != expected_name or manifest.get("version") != expected_version:
        raise LockError("npm package identity does not match the lock")


def verify_installed_npm(path: Path, expected_name: str, expected_version: str) -> None:
    try:
        manifest = json.loads((path / "package.json").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise LockError("cannot read installed npm package") from exc
    if not isinstance(manifest, dict):
        raise LockError("installed npm package.json must be an object")
    if manifest.get("name") != expected_name or manifest.get("version") != expected_version:
        raise LockError("installed npm package identity does not match the lock")


def install_npm_payload(
    source: Path,
    destination: Path,
    allowed_root: Path,
    expected_name: str,
    expected_version: str,
) -> None:
    """Install a verified npm native package with recoverable directory swaps."""
    verify_npm_package(source, expected_name, expected_version)
    try:
        root = allowed_root.resolve(strict=True)
        destination.parent.mkdir(parents=True, exist_ok=True)
        parent = destination.parent.resolve(strict=True)
    except OSError as exc:
        raise LockError("cannot create npm payload destination") from exc
    try:
        parent.relative_to(root)
    except ValueError as exc:
        raise LockError("npm payload destination escapes its package root") from exc
    if destination.name in ("", ".", ".."):
        raise LockError("invalid npm payload destination")
    destination = parent / destination.name
    backup = parent / (destination.name + ".oh-my-setting-backup")
    if backup.exists() or backup.is_symlink():
        if not destination.exists() and not destination.is_symlink():
            os.replace(str(backup), str(destination))
        else:
            raise LockError("stale npm payload backup requires manual recovery")

    stage_root = Path(tempfile.mkdtemp(prefix=".oms-npm-payload-", dir=str(parent)))
    extracted = stage_root / "extracted"
    try:
        safe_extract("tar.gz", source, extracted)
        payload = extracted / "package"
        verify_installed_npm(payload, expected_name, expected_version)
        had_destination = destination.exists() or destination.is_symlink()
        if had_destination:
            os.replace(str(destination), str(backup))
        try:
            os.replace(str(payload), str(destination))
        except OSError:
            if had_destination and backup.exists():
                os.replace(str(backup), str(destination))
            raise
        if backup.exists() or backup.is_symlink():
            if backup.is_dir() and not backup.is_symlink():
                shutil.rmtree(str(backup))
            else:
                backup.unlink()
    except OSError as exc:
        raise LockError("cannot install verified npm payload") from exc
    finally:
        shutil.rmtree(str(stage_root), ignore_errors=True)


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lock", required=True)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("validate")
    get = sub.add_parser("get")
    get.add_argument("field")
    sri = sub.add_parser("verify-sri")
    sri.add_argument("--integrity", required=True)
    sri.add_argument("--file", required=True)
    extract = sub.add_parser("extract")
    extract.add_argument("--archive", required=True, choices=("tar.gz", "zip"))
    extract.add_argument("--source", required=True)
    extract.add_argument("--dest", required=True)
    npm_verify = sub.add_parser("verify-npm-package")
    npm_verify.add_argument("--file", required=True)
    npm_verify.add_argument("--name", required=True)
    npm_verify.add_argument("--version", required=True)
    npm_installed = sub.add_parser("verify-installed-npm")
    npm_installed.add_argument("--path", required=True)
    npm_installed.add_argument("--name", required=True)
    npm_installed.add_argument("--version", required=True)
    npm_install = sub.add_parser("install-npm-payload")
    npm_install.add_argument("--file", required=True)
    npm_install.add_argument("--dest", required=True)
    npm_install.add_argument("--root", required=True)
    npm_install.add_argument("--name", required=True)
    npm_install.add_argument("--version", required=True)
    args = parser.parse_args(argv)
    value = load(Path(args.lock))
    if args.command == "validate":
        print("tool-lock: ok")
    elif args.command == "get":
        selected = lookup(value, args.field)
        if isinstance(selected, bool):
            print("true" if selected else "false")
        else:
            print(selected)
    elif args.command == "verify-sri":
        verify_sri(Path(args.file), args.integrity)
        print("integrity: ok")
    elif args.command == "extract":
        safe_extract(args.archive, Path(args.source), Path(args.dest))
        print("extract: ok")
    elif args.command == "verify-npm-package":
        verify_npm_package(Path(args.file), args.name, args.version)
        print("npm package: ok")
    elif args.command == "verify-installed-npm":
        verify_installed_npm(Path(args.path), args.name, args.version)
        print("installed npm package: ok")
    else:
        install_npm_payload(
            Path(args.file), Path(args.dest), Path(args.root), args.name, args.version
        )
        print("npm payload install: ok")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except LockError as exc:
        print("error: %s" % exc, file=sys.stderr)
        raise SystemExit(2)
