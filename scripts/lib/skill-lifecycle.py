#!/usr/bin/env python3
"""Project skill evaluation, quarantine, provenance, rollback, and drafts.

This helper is intentionally behind ``oms skill-forge``.  It does not create a
second skill authority: native project discovery still consumes links managed
by skill-forge, while immutable imported revisions live outside ``.oms/skills``
until an explicit apply updates the provenance pointer.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Dict, Iterable, List, Mapping, Optional, Sequence, Tuple


MAX_JSON_BYTES = 1024 * 1024
MAX_BUNDLE_BYTES = 2 * 1024 * 1024
MAX_FILE_BYTES = 256 * 1024
MAX_FILES = 256
MAX_OUTPUT_BYTES = 64 * 1024
MAX_CASES = 100
NAME_RE = re.compile(r"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$")
SHA_RE = re.compile(r"^[0-9a-f]{64}$")
SAFE_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,159}$")
SECRET_RE = re.compile(
    r"(?i)(?:"
    r"bearer\s+[A-Za-z0-9._~+/=-]{12,}|"
    r"(?:api[_-]?key|access[_-]?token|auth[_-]?token|password|secret)\s*[:=]\s*\S+|"
    r"(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|"
    r"glpat-[A-Za-z0-9_-]{20,}|sk-[A-Za-z0-9_-]{20,}|"
    r"xox[baprs]-[A-Za-z0-9-]{20,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{30,})|"
    r"-----BEGIN (?:RSA |OPENSSH |EC |DSA )?PRIVATE KEY-----|"
    r"(?:https?|ssh)://[^/@\s:]+:[^/@\s]+@"
    r")"
)
PRIVATE_PATH_RE = re.compile(r"(?:/home/[^/\s]+|/Users/[^/\s]+|[A-Za-z]:[\\/]Users[\\/][^\\/\s]+)")


class LifecycleError(Exception):
    pass


def fail(message: str) -> "NoReturn":  # type: ignore[name-defined]
    raise LifecycleError(message)


def reject_constant(value: str) -> None:
    raise ValueError("non-finite number: %s" % value)


def reject_pairs(pairs: Sequence[Tuple[str, Any]]) -> Dict[str, Any]:
    row: Dict[str, Any] = {}
    for key, value in pairs:
        if key in row:
            raise ValueError("duplicate JSON key: %s" % key)
        row[key] = value
    return row


def finite(value: Any) -> bool:
    if isinstance(value, float):
        return math.isfinite(value)
    if isinstance(value, dict):
        return all(finite(item) for item in value.values())
    if isinstance(value, list):
        return all(finite(item) for item in value)
    return True


def strict_json_bytes(raw: bytes, where: str, *, require_object: bool = True) -> Any:
    if len(raw) > MAX_JSON_BYTES:
        fail("%s exceeds the 1 MiB input budget" % where)
    if b"\x00" in raw:
        fail("%s contains NUL" % where)
    try:
        text = raw.decode("utf-8")
        value = json.loads(
            text,
            object_pairs_hook=reject_pairs,
            parse_constant=reject_constant,
        )
    except (UnicodeDecodeError, ValueError, json.JSONDecodeError) as exc:
        fail("%s is not strict JSON: %s" % (where, exc))
    if require_object and not isinstance(value, dict):
        fail("%s must contain a JSON object" % where)
    if not finite(value):
        fail("%s contains a non-finite number" % where)
    return value


def read_regular(path: Path, where: str, maximum: int = MAX_JSON_BYTES) -> bytes:
    try:
        before = path.lstat()
    except OSError as exc:
        fail("cannot read %s: %s" % (where, exc))
    if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
        fail("%s must be a single-link regular file" % where)
    if before.st_size > maximum:
        fail("%s exceeds its input budget" % where)
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        fd = os.open(str(path), flags)
        try:
            opened = os.fstat(fd)
            if not stat.S_ISREG(opened.st_mode) or opened.st_nlink != 1:
                fail("%s changed before read" % where)
            if (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino):
                fail("%s changed before read" % where)
            raw = b""
            while len(raw) <= maximum:
                block = os.read(fd, min(65536, maximum + 1 - len(raw)))
                if not block:
                    break
                raw += block
        finally:
            os.close(fd)
    except OSError as exc:
        fail("cannot safely read %s: %s" % (where, exc))
    if len(raw) > maximum:
        fail("%s exceeds its input budget" % where)
    return raw


def canonical_json(value: Any) -> bytes:
    return (
        json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False)
        + "\n"
    ).encode("utf-8")


def atomic_write(path: Path, raw: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".%s." % path.name, dir=str(path.parent))
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def append_jsonl(path: Path, row: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("ab") as handle:
        handle.write(canonical_json(dict(row)))
        handle.flush()
        os.fsync(handle.fileno())


def validate_name(name: str, *, imported: bool = False) -> str:
    if not NAME_RE.fullmatch(name) or len(name) > 64 or "--" in name:
        fail("skill name must be lowercase kebab-case, at most 64 chars, with no consecutive hyphens")
    if imported and not name.startswith("oms-"):
        fail("imported project skills carry the oms- namespace marker")
    return name


def validate_sha(value: str, label: str) -> str:
    if not SHA_RE.fullmatch(value):
        fail("%s must be a lowercase SHA-256 digest" % label)
    return value


def parse_skill(path: Path) -> Tuple[str, str]:
    raw = read_regular(path, "SKILL.md", MAX_FILE_BYTES)
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        fail("SKILL.md is not UTF-8")
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        fail("SKILL.md is missing frontmatter")
    try:
        end = lines.index("---", 1)
    except ValueError:
        fail("SKILL.md has unterminated frontmatter")
    values: Dict[str, str] = {}
    for line in lines[1:end]:
        match = re.match(r"^([A-Za-z0-9_-]+):\s*(.*)$", line)
        if match and match.group(1) not in values:
            values[match.group(1)] = match.group(2).strip()
    name = validate_name(values.get("name", ""), imported=True)
    description = values.get("description", "")
    if len(description) < 40 or len(description) > 1024:
        fail("SKILL.md description must be 40-1024 characters")
    if len(lines) > 500:
        fail("SKILL.md exceeds the 500-line body budget")
    return name, description


def sensitive_bundle_path(relative: str) -> bool:
    parts = relative.replace("\\", "/").split("/")
    lowered = [part.lower() for part in parts]
    leaf = lowered[-1]
    if any(part in (".aws", ".ssh") for part in lowered):
        return True
    if leaf == ".env" or leaf.startswith(".env."):
        return True
    if leaf in (".netrc", ".npmrc", ".pypirc", ".pgpass", "credentials"):
        return True
    if leaf.startswith(("id_rsa", "id_ed25519", "id_ecdsa", "id_dsa")):
        return True
    if leaf.endswith((".key", ".pem", ".p8", ".p12", ".pfx")):
        return True
    if "credential" in leaf or "secret" in leaf:
        return True
    return len(lowered) >= 3 and lowered[-3:] == [".config", "gh", "hosts.yml"]


def bundle_entries(root: Path) -> List[Tuple[str, Path, int, int]]:
    try:
        root_state = root.lstat()
    except OSError as exc:
        fail("bundle source is unavailable: %s" % exc)
    if not stat.S_ISDIR(root_state.st_mode) or root.is_symlink():
        fail("bundle source must be a real directory")
    entries: List[Tuple[str, Path, int, int]] = []
    total = 0
    stack: List[Tuple[Path, str]] = [(root, "")]
    seen_dirs = 0
    while stack:
        directory, prefix = stack.pop()
        seen_dirs += 1
        if seen_dirs > MAX_FILES + 32:
            fail("bundle has too many directories")
        children = []
        try:
            with os.scandir(str(directory)) as iterator:
                for child in iterator:
                    children.append(child)
                    if len(children) > MAX_FILES + 32:
                        fail("bundle directory has too many entries")
        except OSError as exc:
            fail("cannot scan bundle: %s" % exc)
        for child in sorted(children, key=lambda item: item.name, reverse=True):
            name = child.name
            if not name or name in (".", "..") or "/" in name or "\\" in name:
                fail("bundle contains an unsafe path component")
            if name == ".git" or name.startswith(".oms-"):
                continue
            rel = (prefix + "/" + name).lstrip("/")
            if len(rel.encode("utf-8")) > 512:
                fail("bundle path exceeds 512 bytes")
            if sensitive_bundle_path(rel):
                fail("bundle contains a sensitive path: %s" % rel)
            try:
                state = child.stat(follow_symlinks=False)
            except OSError as exc:
                fail("cannot stat bundle entry %s: %s" % (rel, exc))
            mode = state.st_mode
            if stat.S_ISLNK(mode):
                fail("bundle symlinks are not allowed: %s" % rel)
            if stat.S_ISDIR(mode):
                stack.append((Path(child.path), rel))
                continue
            if not stat.S_ISREG(mode) or state.st_nlink != 1:
                fail("bundle entries must be single-link regular files: %s" % rel)
            if state.st_size > MAX_FILE_BYTES:
                fail("bundle file exceeds 256 KiB: %s" % rel)
            total += state.st_size
            if total > MAX_BUNDLE_BYTES:
                fail("bundle exceeds the 2 MiB total budget")
            entries.append((rel, Path(child.path), state.st_size, 1 if mode & stat.S_IXUSR else 0))
            if len(entries) > MAX_FILES:
                fail("bundle exceeds the 256-file budget")
    entries.sort(key=lambda item: item[0])
    if not any(rel == "SKILL.md" for rel, _, _, _ in entries):
        fail("bundle is missing SKILL.md")
    return entries


def digest_bundle(entries: Sequence[Tuple[str, Path, int, int]]) -> str:
    digest = hashlib.sha256()
    for rel, path, size, executable in entries:
        encoded = rel.encode("utf-8")
        digest.update(len(encoded).to_bytes(4, "big"))
        digest.update(encoded)
        digest.update(bytes((executable,)))
        digest.update(size.to_bytes(8, "big"))
        raw = read_regular(path, "bundle file %s" % rel, MAX_FILE_BYTES)
        if SECRET_RE.search(raw.decode("utf-8", errors="ignore")):
            fail("bundle contains secret-shaped content: %s" % rel)
        digest.update(raw)
    return digest.hexdigest()


def copy_bundle(entries: Sequence[Tuple[str, Path, int, int]], destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=False)
    for rel, source, _, executable in entries:
        target = destination / Path(rel)
        target.parent.mkdir(parents=True, exist_ok=True)
        raw = read_regular(source, "bundle file %s" % rel, MAX_FILE_BYTES)
        with target.open("xb") as handle:
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
        target.chmod(0o755 if executable else 0o644)


def git_output(arguments: Sequence[str], cwd: Path) -> str:
    env = dict(os.environ)
    env.update({"GIT_TERMINAL_PROMPT": "0", "GIT_CONFIG_NOSYSTEM": "1"})
    try:
        completed = subprocess.run(
            ["git", "-c", "core.hooksPath=/dev/null", *arguments],
            cwd=str(cwd), env=env, stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60, check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        fail("git source probe failed: %s" % exc)
    if completed.returncode != 0:
        fail("git source probe failed")
    if len(completed.stdout) > MAX_OUTPUT_BYTES:
        fail("git source probe output exceeds 64 KiB")
    return completed.stdout.decode("utf-8", errors="strict").strip()


class PreparedSource:
    def __init__(self, source: str, reference: str, subdir: str):
        self.source = source
        self.reference = reference
        self.subdir = subdir
        self.temporary: Optional[tempfile.TemporaryDirectory[str]] = None
        self.root: Optional[Path] = None
        self.provenance: Dict[str, str] = {}

    def __enter__(self) -> "PreparedSource":
        source_path = Path(self.source)
        if source_path.is_dir() and not self.reference:
            if source_path.is_symlink():
                fail("bundle source symlinks are refused")
            real = source_path.resolve()
            self.root = safe_bundle_subdir(real, self.subdir)
            locator = "local-sha256:" + hashlib.sha256(str(real).encode("utf-8")).hexdigest()
            revision = ""
            try:
                revision = git_output(["rev-parse", "HEAD"], real)
            except LifecycleError:
                revision = ""
            self.provenance = {"kind": "local", "locator": locator, "resolved_revision": revision}
            return self
        if "://" in self.source and re.search(r"://[^/@]+@", self.source):
            fail("git source URLs with embedded credentials are refused")
        if len(self.source.encode("utf-8")) > 2048:
            fail("git source locator exceeds 2048 bytes")
        self.temporary = tempfile.TemporaryDirectory(prefix="oms-skill-quarantine.")
        clone = Path(self.temporary.name) / "source"
        env = dict(os.environ)
        env.update({"GIT_TERMINAL_PROMPT": "0", "GIT_CONFIG_NOSYSTEM": "1"})
        command = ["git", "-c", "core.hooksPath=/dev/null", "clone", "--no-checkout", "--", self.source, str(clone)]
        try:
            completed = subprocess.run(command, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=120, check=False)
        except (OSError, subprocess.TimeoutExpired) as exc:
            fail("git quarantine clone failed: %s" % exc)
        if completed.returncode != 0:
            fail("git quarantine clone failed")
        wanted = self.reference or "HEAD"
        git_output(["checkout", "--detach", wanted], clone)
        revision = git_output(["rev-parse", "HEAD"], clone)
        self.root = safe_bundle_subdir(clone, self.subdir)
        self.provenance = {"kind": "git", "locator": self.source, "requested_ref": self.reference, "resolved_revision": revision}
        return self

    def __exit__(self, *_: Any) -> None:
        if self.temporary is not None:
            self.temporary.cleanup()


def safe_bundle_subdir(root: Path, subdir: str) -> Path:
    if not subdir:
        return root
    relative = Path(subdir)
    if relative.is_absolute() or any(part in ("", ".", "..") for part in relative.parts):
        fail("bundle subdirectory must be a safe relative path")
    current = root
    for part in relative.parts:
        current = current / part
        try:
            state = current.lstat()
        except OSError:
            fail("bundle subdirectory does not exist")
        if not stat.S_ISDIR(state.st_mode) or current.is_symlink():
            fail("bundle subdirectory must contain only real directories")
    return current


def prepare_bundle(source: str, reference: str, subdir: str) -> Tuple[PreparedSource, Dict[str, Any]]:
    prepared = PreparedSource(source, reference, subdir)
    prepared.__enter__()
    try:
        assert prepared.root is not None
        if not prepared.root.is_dir():
            fail("bundle subdirectory does not exist")
        entries = bundle_entries(prepared.root)
        name, description = parse_skill(prepared.root / "SKILL.md")
        digest = digest_bundle(entries)
    except Exception:
        prepared.__exit__()
        raise
    report = {
        "schema": 1,
        "status": "preview",
        "name": name,
        "description": description,
        "bundle_sha256": digest,
        "files": len(entries),
        "bytes": sum(item[2] for item in entries),
        "provenance": dict(prepared.provenance),
        "entries": entries,
    }
    return prepared, report


def repo_root(raw: str) -> Path:
    path = Path(raw).resolve()
    if not path.is_dir():
        fail("repo is not an accessible directory")
    try:
        value = git_output(["rev-parse", "--show-toplevel"], path)
        return Path(value).resolve()
    except LifecycleError:
        return path


def lock_path(repo: Path, name: str) -> Path:
    return repo / ".oms" / "skill-store" / name / "lock.json"


def preflight_project_links(repo: Path, name: str) -> None:
    allowed = (
        (repo / ".oms" / "skills").resolve(),
        (repo / ".oms" / "skill-store").resolve(),
    )
    for root_name in (".agents/skills", ".claude/skills"):
        candidate = repo / root_name / name
        if not os.path.lexists(str(candidate)):
            continue
        if not candidate.is_symlink():
            fail("foreign project skill entry blocks import: %s" % candidate.relative_to(repo))
        try:
            target = candidate.resolve(strict=False)
        except OSError:
            fail("foreign project skill link blocks import: %s" % candidate.relative_to(repo))
        if not any(os.path.commonpath((str(prefix), str(target))) == str(prefix) for prefix in allowed):
            fail("foreign project skill link blocks import: %s" % candidate.relative_to(repo))


def load_lock(repo: Path, name: str, *, missing_ok: bool = False) -> Optional[Dict[str, Any]]:
    path = lock_path(repo, name)
    if not path.exists() and not path.is_symlink():
        if missing_ok:
            return None
        fail("no imported skill provenance for %s" % name)
    value = strict_json_bytes(read_regular(path, "skill lock"), "skill lock")
    required = ("schema", "name", "bundle_sha256", "revision_path", "provenance")
    if any(key not in value for key in required) or value.get("schema") != 1 or value.get("name") != name:
        fail("skill lock has an invalid schema")
    validate_sha(str(value.get("bundle_sha256", "")), "stored bundle digest")
    if not isinstance(value.get("provenance"), dict):
        fail("skill lock provenance is invalid")
    revision = repo / str(value.get("revision_path", ""))
    store = (repo / ".oms" / "skill-store" / name / "revisions").resolve()
    try:
        resolved = revision.resolve(strict=True)
    except OSError:
        fail("skill lock revision is missing")
    if os.path.commonpath((str(store), str(resolved))) != str(store):
        fail("skill lock revision escapes the immutable store")
    entries = bundle_entries(resolved)
    if digest_bundle(entries) != value["bundle_sha256"]:
        fail("skill lock revision digest does not match")
    return value


def publish_bundle(repo: Path, report: Dict[str, Any], prepared: PreparedSource, *, action: str, expected_current: str) -> Dict[str, Any]:
    name = report["name"]
    digest = report["bundle_sha256"]
    preflight_project_links(repo, name)
    existing = load_lock(repo, name, missing_ok=True)
    if (repo / ".oms" / "skills" / name).exists() or (repo / ".oms" / "skills" / name).is_symlink():
        fail("an active local project skill already uses this name: %s" % name)
    if action == "import" and existing is not None:
        fail("skill is already imported; use update")
    if action == "update" and existing is None:
        fail("skill is not imported; use import")
    current = str(existing.get("bundle_sha256")) if existing else ""
    if action == "update":
        validate_sha(expected_current, "--expected-current-sha256")
        if current != expected_current:
            fail("current bundle digest changed before update")
    assert prepared.root is not None
    store = repo / ".oms" / "skill-store" / name
    revisions = store / "revisions"
    revisions.mkdir(parents=True, exist_ok=True)
    revision = revisions / digest / name
    if revision.exists() or revision.is_symlink():
        if revision.is_symlink() or not revision.is_dir() or digest_bundle(bundle_entries(revision)) != digest:
            fail("immutable revision occupant does not match its digest")
    else:
        temporary = Path(tempfile.mkdtemp(prefix=".%s." % digest, dir=str(revisions)))
        try:
            copy_bundle(report["entries"], temporary / name)
            if digest_bundle(bundle_entries(temporary / name)) != digest:
                fail("copied bundle digest changed in quarantine")
            destination = revisions / digest
            try:
                os.replace(str(temporary), str(destination))
            except FileExistsError:
                if not destination.is_dir() or digest_bundle(bundle_entries(destination / name)) != digest:
                    fail("concurrent immutable revision differs")
        finally:
            shutil.rmtree(temporary, ignore_errors=True)
    row: Dict[str, Any] = {
        "schema": 1,
        "name": name,
        "bundle_sha256": digest,
        "revision_path": str(revision.relative_to(repo)).replace(os.sep, "/"),
        "provenance": report["provenance"],
        "updated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    if current and current != digest:
        row["previous_sha256"] = current
    atomic_write(lock_path(repo, name), canonical_json(row))
    return row


def rollback_bundle(repo: Path, name: str, target: str, expected_current: str) -> Dict[str, Any]:
    name = validate_name(name, imported=True)
    target = validate_sha(target, "--to")
    expected_current = validate_sha(expected_current, "--expected-current-sha256")
    preflight_project_links(repo, name)
    current = load_lock(repo, name)
    assert current is not None
    if current["bundle_sha256"] != expected_current:
        fail("current bundle digest changed before rollback")
    revision = repo / ".oms" / "skill-store" / name / "revisions" / target / name
    if not revision.is_dir() or revision.is_symlink():
        fail("requested rollback revision is unavailable")
    if digest_bundle(bundle_entries(revision)) != target:
        fail("requested rollback revision failed digest verification")
    row = dict(current)
    row["bundle_sha256"] = target
    row["revision_path"] = str(revision.relative_to(repo)).replace(os.sep, "/")
    row["previous_sha256"] = expected_current
    row["updated_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    atomic_write(lock_path(repo, name), canonical_json(row))
    return row


def active_targets(repo: Path) -> None:
    root = repo / ".oms" / "skill-store"
    if not root.is_dir() or root.is_symlink():
        return
    for child in sorted(root.iterdir(), key=lambda item: item.name):
        if not child.is_dir() or child.is_symlink() or not NAME_RE.fullmatch(child.name):
            continue
        if not (child / "lock.json").exists() and not (child / "lock.json").is_symlink():
            continue
        row = load_lock(repo, child.name)
        assert row is not None
        target = (repo / row["revision_path"]).resolve()
        print("%s\t%s" % (child.name, target))


def command_array(value: Any, where: str) -> List[str]:
    if not isinstance(value, list) or not value or len(value) > 64:
        fail("%s must be a nonempty command array" % where)
    result = []
    for item in value:
        if not isinstance(item, str) or not item or "\x00" in item or len(item.encode("utf-8")) > 4096:
            fail("%s contains an invalid argument" % where)
        result.append(item)
    return result


def run_bounded(command: Sequence[str], *, cwd: Path, env: Mapping[str, str], stdin: bytes = b"", timeout: int = 30, capture: bool = False) -> Tuple[int, bytes]:
    output = tempfile.TemporaryFile()
    error = tempfile.TemporaryFile()
    try:
        process = subprocess.Popen(
            list(command), cwd=str(cwd), env=dict(env), stdin=subprocess.PIPE,
            stdout=output if capture else subprocess.DEVNULL,
            stderr=error if capture else subprocess.DEVNULL,
        )
        try:
            process.communicate(stdin, timeout=timeout)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
            return 124, b""
        if not capture:
            return process.returncode, b""
        output.seek(0)
        raw = output.read(MAX_OUTPUT_BYTES + 1)
        if len(raw) > MAX_OUTPUT_BYTES:
            return 125, b""
        return process.returncode, raw
    except OSError:
        return 127, b""
    finally:
        output.close()
        error.close()


def load_suite(path: Path, skill: str) -> Dict[str, Any]:
    value = strict_json_bytes(read_regular(path, "skill eval suite"), "skill eval suite")
    if value.get("schema") != 1 or value.get("skill") != skill:
        fail("skill eval suite schema/name does not match")
    router = command_array(value.get("router"), "router")
    triggers = value.get("trigger_cases", [])
    tasks = value.get("task_cases", [])
    if not isinstance(triggers, list) or not isinstance(tasks, list) or len(triggers) + len(tasks) > MAX_CASES:
        fail("skill eval suite has an invalid case list")
    ids = set()
    for index, case in enumerate(triggers):
        if not isinstance(case, dict) or not SAFE_ID_RE.fullmatch(str(case.get("id", ""))):
            fail("trigger case %d has an invalid id" % (index + 1))
        if case["id"] in ids:
            fail("duplicate skill eval case id: %s" % case["id"])
        ids.add(case["id"])
        prompt = case.get("prompt")
        if not isinstance(prompt, str) or len(prompt.encode("utf-8")) > 65536:
            fail("trigger case prompt is invalid")
        if not isinstance(case.get("should_trigger"), bool):
            fail("trigger case should_trigger must be boolean")
    for index, case in enumerate(tasks):
        if not isinstance(case, dict) or not SAFE_ID_RE.fullmatch(str(case.get("id", ""))):
            fail("task case %d has an invalid id" % (index + 1))
        if case["id"] in ids:
            fail("duplicate skill eval case id: %s" % case["id"])
        ids.add(case["id"])
        command_array(case.get("command"), "task command")
        command_array(case.get("verify"), "task verify")
    return {"router": router, "trigger_cases": triggers, "task_cases": tasks}


def evaluate(repo: Path, name: str, suite_path: Path, allow_host: bool, record: bool) -> Dict[str, Any]:
    name = validate_name(name)
    skill = repo / ".oms" / "skills" / name
    if not skill.is_dir() or skill.is_symlink():
        fail("eval requires a local project skill: %s" % name)
    parse_skill(skill / "SKILL.md")
    suite_raw = read_regular(suite_path, "skill eval suite")
    suite = load_suite(suite_path, name)
    if (suite["trigger_cases"] or suite["task_cases"]) and not allow_host:
        fail("black-box eval commands require --allow-host-commands")
    matrix: Dict[str, Dict[str, int]] = {
        "baseline": {"true_positive": 0, "true_negative": 0, "false_positive": 0, "false_negative": 0},
        "treatment": {"true_positive": 0, "true_negative": 0, "false_positive": 0, "false_negative": 0},
    }
    task_pass = {"baseline": 0, "treatment": 0}
    base_env = dict(os.environ)
    base_env.update({"OMS_SKILL_EVAL_SKILL": name, "OMS_SKILL_EVAL_PATH": str(skill.resolve()), "PYTHONHASHSEED": "0"})
    for arm, enabled in (("baseline", "0"), ("treatment", "1")):
        env = dict(base_env)
        env["OMS_SKILL_EVAL_TREATMENT"] = enabled
        for case in suite["trigger_cases"]:
            rc, raw = run_bounded(suite["router"], cwd=repo, env=env, stdin=case["prompt"].encode("utf-8"), capture=True)
            if rc != 0:
                fail("router failed for case %s with exit %d" % (case["id"], rc))
            value = strict_json_bytes(raw, "router output")
            selected = value.get("selected")
            if not isinstance(selected, list) or any(not isinstance(item, str) for item in selected):
                fail("router output selected must be a string list")
            actual = name in selected
            expected = bool(case["should_trigger"])
            key = "true_positive" if actual and expected else "true_negative" if not actual and not expected else "false_positive" if actual else "false_negative"
            matrix[arm][key] += 1
        for case in suite["task_cases"]:
            with tempfile.TemporaryDirectory(prefix="oms-skill-eval-case.") as workspace:
                cwd = Path(workspace)
                command = command_array(case["command"], "task command")
                verify = command_array(case["verify"], "task verify")
                rc, _ = run_bounded(command, cwd=cwd, env=env)
                verify_rc = 1
                if rc == 0:
                    verify_rc, _ = run_bounded(verify, cwd=cwd, env=env)
                if rc == 0 and verify_rc == 0:
                    task_pass[arm] += 1
    treated = matrix["treatment"]
    precision_denominator = treated["true_positive"] + treated["false_positive"]
    recall_denominator = treated["true_positive"] + treated["false_negative"]
    result: Dict[str, Any] = {
        "schema": 1,
        "skill": name,
        "skill_sha256": digest_bundle(bundle_entries(skill)),
        "suite_sha256": hashlib.sha256(suite_raw).hexdigest(),
        "trigger": {
            "baseline": matrix["baseline"],
            "treatment": treated,
            "precision": treated["true_positive"] / precision_denominator if precision_denominator else None,
            "recall": treated["true_positive"] / recall_denominator if recall_denominator else None,
        },
        "task": {
            "cases": len(suite["task_cases"]),
            "baseline_passed": task_pass["baseline"],
            "treatment_passed": task_pass["treatment"],
            "pass_delta": task_pass["treatment"] - task_pass["baseline"],
        },
    }
    if record:
        stored = dict(result)
        stored["created_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        stored["evaluation_id"] = "seval_" + hashlib.sha256(canonical_json(result)).hexdigest()[:24]
        append_jsonl(repo / ".oms" / "runtime" / "skill-evals.jsonl", stored)
    return result


def strict_jsonl(path: Path, where: str, maximum: int = MAX_JSON_BYTES) -> List[Dict[str, Any]]:
    raw = read_regular(path, where, maximum)
    if raw and not raw.endswith(b"\n"):
        fail("%s must end with LF" % where)
    rows = []
    for number, line in enumerate(raw.split(b"\n")[:-1], 1):
        if not line or b"\r" in line:
            fail("%s line %d is malformed" % (where, number))
        value = strict_json_bytes(line, "%s line %d" % (where, number))
        rows.append(value)
    return rows


def scrub_text(value: str) -> str:
    clean = SECRET_RE.sub("[REDACTED]", value)
    clean = PRIVATE_PATH_RE.sub("<private-path>", clean)
    clean = "".join(char if char >= " " or char in "\n\t" else " " for char in clean)
    return clean.strip()[:12000]


def source_evidence(repo: Path, kind: str, source_id: str) -> Tuple[bytes, List[str]]:
    if not SAFE_ID_RE.fullmatch(source_id):
        fail("source id is invalid")
    if kind == "thread":
        path = repo / ".oms" / "threads" / (source_id + ".jsonl")
        if not path.exists() and not path.is_symlink():
            fail("insufficient-source: thread does not exist")
        rows = strict_jsonl(path, "thread evidence")
        texts = []
        expected = 1
        for row in rows:
            if row.get("schema") != 1 or row.get("thread") != source_id or row.get("seq") != expected:
                fail("thread evidence has an invalid sequence/schema")
            expected += 1
            text = row.get("text")
            if isinstance(text, str) and text.strip():
                texts.append(scrub_text(text))
        raw = read_regular(path, "thread evidence")
    elif kind == "journal":
        path = repo / ".oms" / "work-journal" / "events.jsonl"
        if not path.exists() and not path.is_symlink():
            fail("insufficient-source: journal does not exist")
        rows = strict_jsonl(path, "journal evidence")
        match = [row for row in rows if row.get("event_id") == source_id]
        if len(match) != 1:
            fail("insufficient-source: journal event is missing or ambiguous")
        row = match[0]
        outcome = row.get("outcome") if isinstance(row.get("outcome"), dict) else {}
        texts = [scrub_text(str(value)) for value in (outcome.get("summary"), row.get("event_type"), row.get("verification_status")) if isinstance(value, str) and value]
        raw = canonical_json(row)
    elif kind == "attempt":
        path = repo / ".oms" / "lifecycle" / "events.jsonl"
        if not path.exists() and not path.is_symlink():
            fail("insufficient-source: attempt ledger does not exist")
        rows = strict_jsonl(path, "attempt evidence")
        selected = [row for row in rows if row.get("attempt_id") == source_id]
        if not selected:
            fail("insufficient-source: attempt does not exist")
        refs: List[str] = []
        for row in selected:
            value = row.get("refs")
            if isinstance(value, dict):
                refs.extend(item for item in value.values() if isinstance(item, str))
        texts = []
        for ref in refs:
            candidate = (repo / ref).resolve()
            if os.path.commonpath((str(repo), str(candidate))) != str(repo) or not candidate.is_file() or candidate.is_symlink():
                continue
            content = read_regular(candidate, "attempt referenced artifact", MAX_FILE_BYTES).decode("utf-8", errors="replace")
            texts.append(scrub_text(content))
        raw = canonical_json(selected)
    else:
        fail("unsupported derive source: %s" % kind)
    texts = [item for item in texts if len(item.encode("utf-8")) >= 20]
    if not texts or sum(len(item.encode("utf-8")) for item in texts) < 80:
        fail("insufficient-source: source has no reviewable procedural evidence")
    return raw, texts[:20]


def derive(repo: Path, kind: str, source_id: str, name: str, apply: bool) -> Dict[str, Any]:
    name = validate_name(name, imported=True)
    raw, texts = source_evidence(repo, kind, source_id)
    source_sha = hashlib.sha256(raw).hexdigest()
    bullets = []
    for text in texts:
        compact = re.sub(r"\s+", " ", text).strip()
        if compact:
            bullets.append("- " + compact[:600])
    description = "Reviewed operating guidance derived from bounded %s evidence; inspect provenance and verification before adoption." % kind
    skill_text = "\n".join((
        "---",
        "name: %s" % name,
        "description: %s" % description,
        "---",
        "",
        "# Candidate runbook",
        "",
        "> Unreviewed draft. Do not adopt until every instruction is verified against the current repository.",
        "",
        "## Evidence-derived observations",
        "",
        *bullets,
        "",
        "## Review checklist",
        "",
        "- Confirm each command and authority boundary against current code.",
        "- Add a focused regression that fails before the proposed guidance.",
        "- Remove incidental or one-off details before importing this draft.",
        "",
    ))
    review_text = "\n".join((
        "# Draft review receipt",
        "",
        "- status: unreviewed",
        "- source_kind: %s" % kind,
        "- source_id: %s" % source_id,
        "- source_sha256: %s" % source_sha,
        "- activation: none",
        "",
        "Adopt only through `oms skill-forge import ... --apply` after review and evaluation.",
        "",
    ))
    draft_sha = hashlib.sha256((skill_text + "\x00" + review_text).encode("utf-8")).hexdigest()
    relative = Path(".oms") / "drafts" / "skills" / name / draft_sha
    result: Dict[str, Any] = {"schema": 1, "status": "preview", "name": name, "draft_sha256": draft_sha, "source": {"kind": kind, "id": source_id, "sha256": source_sha}}
    if not apply:
        return result
    destination = repo / relative
    if destination.exists() or destination.is_symlink():
        if destination.is_symlink() or not destination.is_dir():
            fail("draft destination is unsafe")
        current_skill = read_regular(destination / "SKILL.md", "existing draft SKILL.md").decode("utf-8")
        current_review = read_regular(destination / "REVIEW.md", "existing draft REVIEW.md").decode("utf-8")
        if current_skill != skill_text or current_review != review_text:
            fail("existing draft digest occupant differs")
    else:
        destination.mkdir(parents=True, exist_ok=False)
        atomic_write(destination / "SKILL.md", skill_text.encode("utf-8"))
        atomic_write(destination / "REVIEW.md", review_text.encode("utf-8"))
    result["status"] = "written"
    result["draft_path"] = str(relative).replace(os.sep, "/")
    return result


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser()
    value.add_argument("--repo", default=".")
    sub = value.add_subparsers(dest="command", required=True)
    evaluate_parser = sub.add_parser("eval")
    evaluate_parser.add_argument("name")
    evaluate_parser.add_argument("--suite", required=True)
    evaluate_parser.add_argument("--allow-host-commands", action="store_true")
    evaluate_parser.add_argument("--record", action="store_true")
    evaluate_parser.add_argument("--json", action="store_true")
    for command in ("preview", "import", "update"):
        item = sub.add_parser(command)
        item.add_argument("--source", required=True)
        item.add_argument("--ref", default="")
        item.add_argument("--subdir", default="")
        item.add_argument("--expected-bundle-sha256", default="")
        item.add_argument("--expected-current-sha256", default="")
        item.add_argument("--apply", action="store_true")
        item.add_argument("--json", action="store_true")
    rollback = sub.add_parser("rollback")
    rollback.add_argument("name")
    rollback.add_argument("--to", required=True)
    rollback.add_argument("--expected-current-sha256", required=True)
    rollback.add_argument("--apply", action="store_true")
    rollback.add_argument("--json", action="store_true")
    active = sub.add_parser("active-targets")
    derive_parser = sub.add_parser("derive")
    derive_parser.add_argument("--from", dest="source_kind", choices=("attempt", "thread", "journal"), required=True)
    derive_parser.add_argument("--id", required=True)
    derive_parser.add_argument("--name", required=True)
    derive_parser.add_argument("--apply", action="store_true")
    derive_parser.add_argument("--json", action="store_true")
    return value


def emit(value: Mapping[str, Any], as_json: bool) -> None:
    if as_json:
        print(json.dumps(dict(value), ensure_ascii=False, sort_keys=True, allow_nan=False))
    else:
        for key in sorted(value):
            if key == "entries":
                continue
            print("%s: %s" % (key, value[key]))


def main() -> int:
    args = parser().parse_args()
    repo = repo_root(args.repo)
    if args.command == "active-targets":
        active_targets(repo)
        return 0
    if args.command == "eval":
        result = evaluate(repo, args.name, Path(args.suite), args.allow_host_commands, args.record)
        emit(result, args.json)
        return 0
    if args.command == "derive":
        result = derive(repo, args.source_kind, args.id, args.name, args.apply)
        emit(result, args.json)
        return 0
    if args.command == "rollback":
        if not args.apply:
            current = load_lock(repo, validate_name(args.name, imported=True))
            result = {"schema": 1, "status": "preview", "name": args.name, "from": current["bundle_sha256"], "to": validate_sha(args.to, "--to")}
        else:
            result = rollback_bundle(repo, args.name, args.to, args.expected_current_sha256)
            result = {"schema": 1, "status": "applied", **result}
        emit(result, args.json)
        return 0
    prepared, report = prepare_bundle(args.source, args.ref, args.subdir)
    try:
        public_report = {key: value for key, value in report.items() if key != "entries"}
        if args.command == "preview" or not args.apply:
            emit(public_report, args.json)
            return 0
        validate_sha(args.expected_bundle_sha256, "--expected-bundle-sha256")
        if report["bundle_sha256"] != args.expected_bundle_sha256:
            fail("bundle bytes changed after preview")
        result = publish_bundle(repo, report, prepared, action=args.command, expected_current=args.expected_current_sha256)
        emit({"schema": 1, "status": "applied", **result}, args.json)
        return 0
    finally:
        prepared.__exit__()


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except LifecycleError as exc:
        print("error: %s" % exc, file=sys.stderr)
        raise SystemExit(2)
