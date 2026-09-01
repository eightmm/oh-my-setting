"""Bounded context manifests for delegated implementation and review calls."""

from __future__ import annotations

import ast
import functools
import os
import re
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Set, Tuple

from . import RUNTIME_SCHEMA
from .common import CoreError, atomic_write_bytes, atomic_write_json, canonical_json, read_bytes, relative_path, sensitive_text, sha256_bytes, sha256_file, utc_now
from .evidence import build_envelope

DEFAULT_CONTEXT_BYTES = 64 * 1024
MIN_CONTEXT_BYTES = 4 * 1024
MAX_CONTEXT_BYTES = 512 * 1024
MAX_DISCOVERY_FILE_BYTES = 2 * 1024 * 1024


def _safe_repo_file(repo: Path, raw: str, *, allow_external: bool = False) -> Path:
    path = Path(raw).expanduser()
    if not path.is_absolute():
        path = repo / path
    lexical = Path(os.path.abspath(str(path)))
    cursor = lexical
    while True:
        if cursor.is_symlink():
            raise CoreError("context source path must not traverse a symbolic link: %s" % raw)
        if cursor == cursor.parent:
            break
        cursor = cursor.parent
    try:
        resolved = lexical.resolve(strict=True)
    except OSError:
        raise CoreError("context source does not exist: %s" % raw)
    if not resolved.is_file():
        raise CoreError("context source must be a regular file: %s" % raw)
    if not allow_external and not relative_path(resolved, repo):
        raise CoreError("external context source requires --allow-external: %s" % raw)
    return resolved


@functools.lru_cache(maxsize=4096)
def _discovery_text_cached(resolved: str, mtime_ns: int, size: int) -> str:
    try:
        return Path(resolved).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return ""


def _discovery_text(path: Path) -> str:
    # Discovery walks overlapping roots (the repo root contains src/ and
    # scripts/) and the test pass revisits files the symbol pass will read
    # again, so the same bytes were read up to twice per plan. The memo key
    # carries the stat signature: an edited file re-reads, an unchanged one
    # never does.
    try:
        stat = path.stat()
    except OSError:
        return ""
    if stat.st_size > MAX_DISCOVERY_FILE_BYTES:
        return ""
    return _discovery_text_cached(str(path), stat.st_mtime_ns, stat.st_size)


def _python_import_candidates(repo: Path, target: Path) -> List[Tuple[Path, str, int]]:
    result: List[Tuple[Path, str, int]] = []
    if target.suffix != ".py":
        return result
    try:
        source = _discovery_text(target)
        if not source:
            return result
        tree = ast.parse(source, filename=str(target))
    except (OSError, SyntaxError, UnicodeDecodeError):
        return result
    modules: Set[str] = set()
    relative_modules: Set[Tuple[int, str]] = set()
    symbols: Set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            modules.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            if node.level:
                relative_modules.add((int(node.level), node.module or ""))
            elif node.module:
                modules.add(node.module)
            symbols.update(alias.name for alias in node.names)
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            symbols.add(node.name)
    search_roots = [repo, repo / "src", repo / "scripts"]
    for level, module in sorted(relative_modules):
        base = target.parent
        for _ in range(max(0, level - 1)):
            base = base.parent
        relative = Path(*module.split(".")) if module else Path()
        candidates = (base / (str(relative) + ".py"), base / relative / "__init__.py") if module else (base / "__init__.py",)
        for candidate in candidates:
            if candidate.is_file() and not candidate.is_symlink() and candidate.resolve() != target.resolve():
                result.append((candidate.resolve(), "direct relative Python import", 88))
                break
    for module in sorted(modules):
        relative = Path(*module.split("."))
        for root in search_roots:
            for candidate in (root / (str(relative) + ".py"), root / relative / "__init__.py"):
                if candidate.is_file() and not candidate.is_symlink() and candidate.resolve() != target.resolve():
                    result.append((candidate.resolve(), "direct Python import", 85))
                    break
    target_rel = relative_path(target, repo)
    target_module = target_rel[:-3].replace("/", ".") if target_rel.endswith(".py") else ""
    target_stem = target.stem
    for test_root in (repo / "tests", repo / "test"):
        if not test_root.is_dir():
            continue
        for candidate in sorted(test_root.rglob("*.py"))[:5000]:
            if candidate.is_symlink() or candidate.resolve() == target.resolve():
                continue
            text = _discovery_text(candidate)
            if text and (target_stem in candidate.stem or (target_module and target_module in text) or re.search(r"\b%s\b" % re.escape(target_stem), text)):
                result.append((candidate.resolve(), "related test", 90))
    public_symbols = sorted(name for name in symbols if name and not name.startswith("_"))[:20]
    if public_symbols:
        for root in search_roots:
            if not root.is_dir():
                continue
            for candidate in sorted(root.rglob("*.py"))[:5000]:
                if candidate.is_symlink() or candidate.resolve() == target.resolve() or any(part in {".git", ".oms", "__pycache__"} for part in candidate.parts):
                    continue
                text = _discovery_text(candidate)
                if text and any(re.search(r"\b%s\b" % re.escape(symbol), text) for symbol in public_symbols):
                    result.append((candidate.resolve(), "symbol caller", 60))
    best: Dict[Path, Tuple[str, int]] = {}
    for path, reason, priority in result:
        current = best.get(path)
        if current is None or priority > current[1]:
            best[path] = (reason, priority)
    return [(path, reason, priority) for path, (reason, priority) in best.items()]


def _default_layers(repo: Path) -> List[Tuple[Path, str, int, str]]:
    candidates = [(repo / "PROJECT.md", "project contract", 100, "head"), (repo / ".oms" / "task" / "current.md", "active task packet", 100, "tail"), (repo / ".oms" / "memory" / "summary.md", "compacted project memory", 55, "tail"), (repo / ".oms" / "work-journal" / "today.md", "current work journal", 35, "tail")]
    return [row for row in candidates if row[0].is_file() and not row[0].is_symlink()]


def _slice(data: bytes, budget: int, policy: str) -> Tuple[bytes, int, str]:
    if len(data) <= budget:
        return data, 0, "full"
    if budget <= 0:
        return b"", len(data), "omitted"
    marker = ("\n[TRUNCATED: %d bytes omitted]\n" % max(0, len(data) - budget)).encode("utf-8")
    usable = max(0, budget - len(marker))
    if policy == "tail":
        selected = marker + data[-usable:]
    elif policy == "head":
        selected = data[:usable] + marker
    else:
        left = usable // 2
        selected = data[:left] + marker + data[-(usable - left):]
    return selected[:budget], len(data) - min(len(data), usable), policy


def plan_context(repo: Path, *, targets: Sequence[str] = (), explicit: Sequence[Tuple[str, str]] = (), required: Sequence[str] = (), max_bytes: int = DEFAULT_CONTEXT_BYTES, bundle_path: Optional[Path] = None, manifest_path: Optional[Path] = None, allow_external: bool = False, phase: str = "implementation") -> Dict[str, Any]:
    """Compile a bounded context bundle. Every `targets` entry is a direct
    target (required, with its Python imports discovered); a project-graph
    context pack hands its whole file list here, in pack order."""
    if phase not in ("orientation", "implementation", "verification", "review", "research"):
        raise CoreError("unsupported context phase: %s" % phase)
    if max_bytes < MIN_CONTEXT_BYTES or max_bytes > MAX_CONTEXT_BYTES:
        raise CoreError("context budget must be between %d and %d bytes" % (MIN_CONTEXT_BYTES, MAX_CONTEXT_BYTES))
    candidates: List[Tuple[Path, str, int, str]] = _default_layers(repo)
    target_labels: List[str] = []
    for target in targets:
        if not target:
            continue
        target_path = _safe_repo_file(repo, target, allow_external=allow_external)
        label = relative_path(target_path, repo) or (target_path.name if allow_external else "")
        if label and label not in target_labels:
            target_labels.append(label)
        candidates.append((target_path, "direct target", 120, "middle"))
        for path, reason, priority in _python_import_candidates(repo, target_path):
            candidates.append((path, reason, priority, "middle"))
    for raw, reason in explicit:
        candidates.append((_safe_repo_file(repo, raw, allow_external=allow_external), reason or "explicit context", 110, "middle"))
    unresolved_required: List[str] = []
    required_resolved: Dict[str, str] = {}
    for raw in required:
        try:
            required_path = _safe_repo_file(repo, raw, allow_external=allow_external)
        except CoreError:
            unresolved_required.append(raw)
            continue
        label = relative_path(required_path, repo) or str(required_path)
        required_resolved[raw] = label
        candidates.append((required_path, "required context", 130, "middle"))
    best: Dict[Path, Tuple[str, int, str]] = {}
    for path, reason, priority, policy in candidates:
        current = best.get(path.resolve())
        if current is None or priority > current[1]:
            best[path.resolve()] = (reason, priority, policy)
    ordered = sorted(best.items(), key=lambda item: (-item[1][1], relative_path(item[0], repo) or str(item[0])))
    selected: List[Dict[str, Any]] = []
    omitted: List[Dict[str, Any]] = []
    bundle_parts: List[bytes] = []
    remaining = max_bytes
    max_nonrequired_source = max(MIN_CONTEXT_BYTES, max_bytes // 3)
    included_paths: Set[str] = set()
    required_paths = set(required_resolved.values())
    required_paths.update(target_labels)
    for path, (reason, priority, policy) in ordered:
        label = relative_path(path, repo) or (path.name if allow_external else "")
        if not label:
            omitted.append({"path": "[external]", "reason": "external path omitted", "priority": priority})
            continue
        data = read_bytes(path)
        try:
            text = data.decode("utf-8")
        except UnicodeDecodeError:
            omitted.append({"path": label, "reason": "non-UTF-8 source", "priority": priority, "bytes": len(data)})
            continue
        if sensitive_text(text):
            omitted.append({"path": label, "reason": "sensitive-looking content", "priority": priority, "bytes": len(data)})
            continue
        header = ("\n--- begin context: %s (%s) ---\n" % (label, reason)).encode("utf-8")
        footer = ("\n--- end context: %s ---\n" % label).encode("utf-8")
        allocation = min(max(0, remaining - len(header) - len(footer)), max(MIN_CONTEXT_BYTES, max_bytes // 2))
        if label not in required_paths:
            allocation = min(allocation, max_nonrequired_source)
        if allocation <= 0:
            omitted.append({"path": label, "reason": "budget exhausted", "priority": priority, "bytes": len(data)})
            continue
        selected_data, omitted_bytes, selection = _slice(data, allocation, policy)
        part = header + selected_data + footer if selected_data else b""
        if not part or len(part) > remaining:
            omitted.append({"path": label, "reason": "budget exhausted", "priority": priority, "bytes": len(data)})
            continue
        bundle_parts.append(part)
        remaining -= len(part)
        included_paths.add(label)
        # Hash the bytes already in hand: re-opening the file let a write
        # between read and hash record a digest that disagrees with the
        # bytes actually bundled.
        selected.append({"path": label, "reason": reason, "priority": priority, "bytes_before": len(data), "bytes_selected": len(selected_data), "omitted_bytes": omitted_bytes, "selection": selection, "sha256": sha256_bytes(data)})
    truncated_required = sorted(item["path"] for item in selected if item["path"] in required_paths and int(item.get("omitted_bytes", 0)) > 0)
    missing_required = sorted(set(unresolved_required) | (required_paths - included_paths) | set(truncated_required))
    bundle = b"".join(bundle_parts)
    if sensitive_text(bundle.decode("utf-8", "replace")):
        raise CoreError("compiled context contains sensitive-looking content")
    manifest: Dict[str, Any] = {"schema": RUNTIME_SCHEMA, "generated_at": utc_now(), "phase": phase, "budget_bytes": max_bytes, "targets": list(target_labels), "selected_bytes": len(bundle), "remaining_bytes": remaining, "max_nonrequired_source_bytes": max_nonrequired_source, "selected": selected, "omitted": omitted, "context_debt": len(missing_required), "missing_required": missing_required, "truncated_required": truncated_required, "sufficient": not missing_required, "bundle_sha256": sha256_bytes(bundle), "contract_state_digest": build_envelope(repo).get("state_digest")}
    digest = sha256_bytes(canonical_json(manifest))
    if bundle_path is None:
        bundle_path = repo / ".oms" / "runtime" / "context" / (digest + ".txt")
    if manifest_path is None:
        manifest_path = repo / ".oms" / "runtime" / "context" / (digest + ".json")
    atomic_write_bytes(bundle_path, bundle)
    manifest["bundle_path"] = relative_path(bundle_path, repo) or bundle_path.name
    atomic_write_json(manifest_path, manifest)
    manifest["manifest_path"] = relative_path(manifest_path, repo) or manifest_path.name
    return manifest
