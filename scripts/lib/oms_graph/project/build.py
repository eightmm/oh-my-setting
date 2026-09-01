"""Incremental build, cross-file resolution, freshness check (W2)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from oms_graph import PARSER_VERSION, PROJECT_SCHEMA
from oms_runtime.common import atomic_write_bytes, atomic_write_json, canonical_json, ensure_private_dir, read_json, sha256_bytes, sha256_file, utc_now

from ..errors import GraphError

# A whole-repository graph outgrows the runtime's 8 MiB state-file default
# (this repository alone is ~4 MiB canonical); the graph is a regenerable
# cache, so it gets its own ceiling. Canonical one-line JSON keeps the bytes
# deterministic and about half the size of the pretty form.
GRAPH_BYTES_LIMIT = 256 * 1024 * 1024
# An auto-build is a side effect of somebody's read, so it carries a bound the
# explicit `build` does not: a first graph over an enormous tree has to be a
# decision, not a surprise. A refresh of an existing graph is never refused —
# whoever built it already made that decision.
AUTOBUILD_MAX_FILES = 20000

from .cache import cache_key, load_cached, prune, store
from .extract import discover_files, extract_file
from .parsers import parser_for
from .model import make_edge, node_id, sort_edges, sort_nodes


def state_dir(repo: Path, override: Optional[Path] = None) -> Path:
    return Path(override) if override else repo / ".oms" / "project-graph"


def _ensure_state_marker(repo: Path, directory: Path) -> None:
    """Drop the `.oms/.gitignore` ownership marker every harness tool leaves.

    The graph may be the first OMS state a repository ever gets (an auto-build
    behind a reader); without the marker `git status` would show `?? .oms/`
    and the cache could reach a commit. An explicit state directory outside
    `.oms` is the caller's to ignore.
    """
    oms = Path(repo) / ".oms"
    try:
        inside = Path(directory).resolve().is_relative_to(oms.resolve())
    except (OSError, ValueError, AttributeError):
        inside = str(Path(directory)).startswith(str(oms))
    if not inside:
        return
    marker = oms / ".gitignore"
    if not marker.exists() and not marker.is_symlink():
        atomic_write_bytes(marker, b"*\n", mode=0o644)


def build(repo: Path, *, state: Optional[Path] = None, include: Sequence[str] = (), exclude: Sequence[str] = (), max_bytes: int = 2 * 1024 * 1024, force: bool = False) -> Dict[str, Any]:
    """Write graph.json (no timestamps) and manifest.json; return the manifest summary."""
    repo = Path(repo).resolve()
    directory = ensure_private_dir(state_dir(repo, state))
    _ensure_state_marker(repo, directory)
    discovery = discover_files(repo, include=include, exclude=exclude, max_bytes=max_bytes)
    extractions: List[Dict[str, Any]] = []
    files: Dict[str, Dict[str, Any]] = {}
    keys: List[str] = []
    cached_count = 0
    for relpath in discovery["files"]:
        path = repo / relpath
        digest = sha256_file(path)
        # Parser versions are currently registry-wide, but keep the individual
        # version in the cache key so an added parser can invalidate safely.
        parser = parser_for(relpath)
        key = cache_key(relpath, digest, parser.version if parser else PARSER_VERSION, PROJECT_SCHEMA)
        keys.append(key)
        payload = None if force else load_cached(directory, key)
        was_cached = payload is not None
        if payload is None:
            payload = extract_file(repo, relpath)
            store(directory, key, payload)
        if was_cached:
            cached_count += 1
        extractions.append(payload)
        files[relpath] = {"sha256": digest, "bytes": path.stat().st_size, "parser": payload["parser"], "cached": was_cached}
    nodes, edges = resolve(extractions)
    revision_source = "\n".join("%s\t%s" % (path, files[path]["sha256"]) for path in sorted(files)) + "\n%d\n%d" % (PARSER_VERSION, PROJECT_SCHEMA)
    revision = sha256_bytes(revision_source.encode("utf-8"))
    graph = {"schema": PROJECT_SCHEMA, "revision": revision, "nodes": nodes, "edges": edges}
    atomic_write_bytes(directory / "graph.json", canonical_json(graph) + b"\n")
    manifest = {"schema": PROJECT_SCHEMA, "revision": revision, "generated_at": utc_now(), "parser_version": PARSER_VERSION,
                "discovery": {"include": sorted(include), "exclude": sorted(exclude), "max_bytes": int(max_bytes)},
                "files": {path: files[path] for path in sorted(files)}, "skipped": discovery["skipped"],
                "stats": {"files": len(files), "nodes": len(nodes), "edges": len(edges), "cached": cached_count, "parsed": len(files) - cached_count}}
    atomic_write_json(directory / "manifest.json", manifest)
    prune(directory, keys)
    return manifest


def ensure(repo: Path, *, state: Optional[Path] = None, include: Sequence[str] = (), exclude: Sequence[str] = (), max_bytes: int = 2 * 1024 * 1024, max_files: int = AUTOBUILD_MAX_FILES) -> Dict[str, Any]:
    """Make the graph current and say what that cost: {"action": fresh|built|refreshed, "revision", ["stats", "skipped"]}."""
    repo = Path(repo).resolve()
    status = check(repo, state=state)
    if status["present"] and status["fresh"]:
        return {"action": "fresh", "revision": status["revision"]}
    if status["present"]:
        # A refresh keeps the discovery options the graph was built with. A
        # reader must never widen (or narrow) an explicit --include/--exclude
        # by refreshing with this call's defaults; `check` compares against
        # those same stored options, so anything else would also never settle.
        manifest = read_json(state_dir(repo, state) / "manifest.json", {})
        options = manifest.get("discovery") if isinstance(manifest, dict) and isinstance(manifest.get("discovery"), dict) else {}
        include = tuple(options.get("include") or ())
        exclude = tuple(options.get("exclude") or ())
        max_bytes = int(options.get("max_bytes", max_bytes))
    else:
        found = len(discover_files(repo, include=include, exclude=exclude, max_bytes=max_bytes)["files"])
        if found > max_files:
            raise GraphError("project graph auto-build skipped: %d files exceed the %d-file bound; run: oms graph project build" % (found, max_files))
    manifest = build(repo, state=state, include=include, exclude=exclude, max_bytes=max_bytes)
    return {"action": "refreshed" if status["present"] else "built", "revision": manifest["revision"],
            "stats": manifest["stats"], "skipped": len(manifest["skipped"])}


def check(repo: Path, *, state: Optional[Path] = None) -> Dict[str, Any]:
    """{"present","fresh","revision","stale":[...],"missing":[...],"new":[...]} from working-tree bytes."""
    directory = state_dir(Path(repo).resolve(), state)
    manifest = read_json(directory / "manifest.json", None)
    if not isinstance(manifest, dict):
        return {"present": False, "fresh": False, "revision": "", "stale": [], "missing": [], "new": []}
    options = manifest.get("discovery") if isinstance(manifest.get("discovery"), dict) else {}
    listed = discover_files(Path(repo).resolve(), include=tuple(options.get("include", ())), exclude=tuple(options.get("exclude", ())), max_bytes=int(options.get("max_bytes", 2 * 1024 * 1024)))
    current = set(listed["files"])
    known = set(manifest.get("files", {}))
    stale = sorted(path for path in current & known if sha256_file(Path(repo) / path) != manifest["files"][path].get("sha256"))
    missing = sorted(known - current)
    new = sorted(current - known)
    # A parser or schema upgrade changes what the same bytes mean, so it makes
    # the whole graph stale even when no file moved; the per-file cache keys
    # carry the parser version, so the refresh re-parses everything.
    outdated = {}
    if manifest.get("parser_version") != PARSER_VERSION:
        outdated["parser_version"] = {"built": manifest.get("parser_version"), "current": PARSER_VERSION}
    if manifest.get("schema") != PROJECT_SCHEMA:
        outdated["schema"] = {"built": manifest.get("schema"), "current": PROJECT_SCHEMA}
    return {"present": True, "fresh": not (stale or missing or new or outdated), "revision": manifest.get("revision", ""),
            "stale": stale, "missing": missing, "new": new, "outdated": outdated}


def load_graph(repo: Path, *, state: Optional[Path] = None) -> Dict[str, Any]:
    graph = read_json(state_dir(Path(repo).resolve(), state) / "graph.json", None, limit=GRAPH_BYTES_LIMIT)
    if not isinstance(graph, dict):
        raise GraphError("project graph has not been built; run: oms graph project build")
    return graph


def resolve(extractions: Sequence[Mapping[str, Any]]) -> Tuple[List[Dict[str, Any]], List[Dict[str, Any]]]:
    """Turn per-file extractions (with unresolved refs) into sorted nodes and edges with confidence."""
    node_map: Dict[str, Dict[str, Any]] = {}
    for item in extractions:
        for node in item.get("nodes", []):
            node_map[node["id"]] = dict(node)
    nodes = sort_nodes(list(node_map.values()))
    raw_edges = [edge for item in extractions for edge in item.get("edges", [])]
    by_id = {node["id"]: node for node in nodes}
    files = {item["path"]: item for item in extractions}
    symbols_by_name: Dict[str, List[str]] = {}
    symbols_by_file: Dict[str, Dict[str, str]] = {}
    for node in nodes:
        if node["kind"] in ("class", "function", "method", "symbol"):
            symbols_by_name.setdefault(node["name"], []).append(node["id"])
            symbols_by_file.setdefault(node["path"], {})[node["name"]] = node["id"]
    for values in symbols_by_name.values():
        values.sort()
    module_to_path: Dict[str, str] = {}
    for path in files:
        if path.endswith(".py"):
            for prefix in ("", "src/", "scripts/", "scripts/lib/"):
                if path.startswith(prefix):
                    trimmed = path[len(prefix):]
                    module = trimmed[:-3].replace("/", ".")
                    if module.endswith(".__init__"):
                        module = module[:-9]
                    module_to_path.setdefault(module, path)

    def module_path(value: str, source_path: str) -> str:
        if value.startswith("."):
            level = len(value) - len(value.lstrip("."))
            rest = value[level:]
            base = source_path.rsplit("/", 1)[0] if "/" in source_path else ""
            parts = base.split("/") if base else []
            for _ in range(max(0, level - 1)):
                if parts: parts.pop()
            candidate = "/".join(parts + (rest.split(".") if rest else []))
            for suffix in (".py", "/__init__.py"):
                if candidate + suffix in files: return candidate + suffix
            return ""
        if value in module_to_path:
            return module_to_path[value]
        relative = value.replace(".", "/")
        choices = [relative + ".py", relative + "/__init__.py"]
        source_dir = source_path.rsplit("/", 1)[0] if "/" in source_path else ""
        choices += [source_dir + "/" + candidate for candidate in choices] if source_dir else []
        for candidate in choices:
            if candidate in files: return candidate
        return ""

    imported: Dict[str, Dict[str, Tuple[str, str]]] = {}
    include_paths: Dict[str, List[str]] = {}
    for item in extractions:
        source_path = item["path"]
        for ref in item.get("refs", []):
            if ref.get("relation") != "imports":
                continue
            target_path = module_path(ref["value"], source_path) if ref.get("kind") == "module" else ref["value"]
            if target_path not in files and ref.get("kind") == "path":
                for prefix in ("", "scripts/", "scripts/lib/"):
                    if prefix + target_path in files:
                        target_path = prefix + target_path; break
            if target_path in files:
                imported.setdefault(source_path, {})[ref.get("binding", "")] = (target_path, ref.get("imported", ""))
                include_paths.setdefault(source_path, []).append(target_path)
    for item in extractions:
        source_path, digest = item["path"], item["sha256"]
        source_file_id = node_id("test" if any(node.get("id") == node_id("test", source_path) for node in item["nodes"]) else "file", source_path)
        for ref in item.get("refs", []):
            target = ""; confidence = "EXTRACTED"; candidates: List[str] = []
            relation = ref.get("relation", "references")
            value = ref.get("value", "")
            if ref.get("kind") == "module":
                target_path = module_path(value, source_path)
                if not target_path and ref.get("imported"):
                    target_path = module_path(value + ref["imported"], source_path)
                if target_path:
                    target = node_id("module", target_path) if node_id("module", target_path) in by_id else node_id("file", target_path)
                    if source_file_id.startswith("test:"): relation = "tests"
            elif ref.get("kind") == "path":
                target_path = value
                if target_path not in files:
                    for prefix in ("", "scripts/", "scripts/lib/"):
                        if prefix + target_path in files:
                            target_path = prefix + target_path; break
                if target_path in files:
                    target = node_id("test" if node_id("test", target_path) in by_id else "file", target_path)
                    confidence = "INFERRED" if ref.get("variable") else "EXTRACTED"
            elif ref.get("kind") == "name":
                first, _, member = value.partition(".")
                target_path = ""
                if first in imported.get(source_path, {}):
                    target_path, imported_name = imported[source_path][first]
                    wanted = member or imported_name
                    target = symbols_by_file.get(target_path, {}).get(wanted, "")
                if not target:
                    target = symbols_by_file.get(source_path, {}).get(first, "")
                if not target and ref.get("shell_bare"):
                    for included in include_paths.get(source_path, []):
                        if first in symbols_by_file.get(included, {}):
                            target = symbols_by_file[included][first]; confidence = "INFERRED"; break
                if not target:
                    choices = symbols_by_name.get(first, [])
                    if len(choices) == 1:
                        target, confidence = choices[0], "INFERRED"
                    elif len(choices) > 1:
                        target, confidence, candidates = choices[0], "AMBIGUOUS", choices[1:]
            if target:
                raw_edges.append(make_edge(ref.get("from", source_file_id), target, relation, confidence, path=source_path, source_digest=digest, line=ref.get("line"), candidates=candidates))
    unique = {(edge["source"], edge["target"], edge["relation"], edge["confidence"], edge.get("evidence", {}).get("path"), edge.get("evidence", {}).get("line")): edge for edge in raw_edges}
    return nodes, sort_edges(list(unique.values()))
