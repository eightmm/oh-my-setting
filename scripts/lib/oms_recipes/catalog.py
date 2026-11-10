"""Local immutable extension resources and inspection-only cross-repo worksets."""

from __future__ import annotations

import os
from pathlib import Path
import re

from oms_runtime.common import (
    CoreError, SECRET_VALUE_RE, atomic_write_bytes, atomic_write_json, file_lock,
    git_head, read_bytes, read_json, sha256_bytes, utc_now,
)
from .common import (
    HEX, MAX_PACK_BYTES, MAX_RECORDS, MAX_RESOURCES, MAX_RESOURCE_BYTES,
    digest, history, identifier, load_definition, object_fields, records,
    relative, state_path, state_root, text, under,
)


def _manifest(value: dict) -> dict:
    object_fields(value, {"schema", "kind", "id", "version", "description", "resources"},
                  {"schema", "kind", "id", "version", "resources"}, "pack manifest")
    if value["schema"] != 1 or type(value["schema"]) is not int or value["kind"] != "local-pack":
        raise CoreError("expected local-pack schema 1")
    identifier(value["id"], "pack id")
    if not value["id"].startswith("oms-"):
        raise CoreError("local extension pack ids must start with oms-")
    identifier(value["version"], "pack version")
    if "description" in value:
        text(value["description"], "pack description", 2000)
    resources = value["resources"]
    if not isinstance(resources, dict) or not 1 <= len(resources) <= MAX_RESOURCES:
        raise CoreError("pack resources must contain 1..64 entries")
    paths = set()
    for name, resource in resources.items():
        identifier(name, "resource name", name=True)
        object_fields(resource, {"kind", "path", "sha256", "executable"}, {"kind", "path", "sha256"}, "pack resource")
        if resource["kind"] not in {"recipe", "script", "renderer"}:
            raise CoreError("unsupported pack resource kind")
        relative(resource["path"], "resource path")
        if resource["path"] in paths or resource["path"] == "manifest.json":
            raise CoreError("pack resource paths must be distinct and cannot replace the manifest")
        paths.add(resource["path"])
        if not isinstance(resource["sha256"], str) or not HEX.fullmatch(resource["sha256"]):
            raise CoreError("pack resource needs a lowercase SHA-256")
        if type(resource.get("executable", False)) is not bool or (resource["kind"] == "recipe" and resource.get("executable")):
            raise CoreError("invalid resource executable flag")
    return value


def _pack_bytes(root: Path, manifest: dict, selected: list[str]) -> dict[str, bytes]:
    if len(set(selected)) != len(selected) or not selected or set(selected) - manifest["resources"].keys():
        raise CoreError("unknown, duplicate or empty resource selection")
    result = {}
    size = 0
    for name in selected:
        resource = manifest["resources"][name]
        data = read_bytes(under(root, resource["path"]), MAX_RESOURCE_BYTES)
        size += len(data)
        if size > MAX_PACK_BYTES:
            raise CoreError("selected pack resources exceed 16 MiB")
        if sha256_bytes(data) != resource["sha256"]:
            raise CoreError("pack resource digest mismatch: " + name)
        if SECRET_VALUE_RE.search(data.decode("utf-8", "replace")):
            raise CoreError("pack resource contains sensitive-looking material: " + name)
        if resource["kind"] == "recipe":
            from .definitions import validate
            validate(load_definition(under(root, resource["path"])))
        result[name] = data
    return result


def inspect_pack(directory: str) -> dict:
    root = Path(directory).expanduser()
    if root.is_symlink() or not root.is_dir():
        raise CoreError("pack source must be a local non-symlink directory")
    manifest = _manifest(load_definition(under(root, "manifest.json")))
    resources = _pack_bytes(root, manifest, sorted(manifest["resources"]))
    return {"schema": 1, "id": manifest["id"], "version": manifest["version"], "digest": digest(manifest),
            "resources": manifest["resources"], "bytes": sum(map(len, resources.values())), "verified": "all-resources",
            "execution": "import never executes scripts or installs dependencies"}


def import_pack(repo: Path, directory: str, expected_digest: str, selected: list[str]) -> dict:
    root = Path(directory).expanduser()
    if root.is_symlink() or not root.is_dir():
        raise CoreError("pack source must be a local non-symlink directory")
    manifest = _manifest(load_definition(under(root, "manifest.json")))
    fingerprint = digest(manifest)
    if fingerprint != expected_digest:
        raise CoreError("pack manifest digest changed or does not match --digest")
    selected = selected or sorted(manifest["resources"])
    contents = _pack_bytes(root, manifest, selected)
    destination = under(state_root(repo, create=True), "packs/" + manifest["id"] + "/" + manifest["version"])
    marker = under(destination, "pack.json")
    with file_lock(marker):
        previous = read_json(marker)
        if previous is not None:
            if not isinstance(previous, dict) or previous.get("digest") != fingerprint:
                raise CoreError("a different pack already owns this id/version; use a new version")
            old = previous.get("selected", [])
            if not isinstance(old, list) or set(old) - manifest["resources"].keys():
                raise CoreError("stored pack selection is invalid")
            _pack_bytes(under(destination, "resources"), manifest, old)
            selected = sorted(set(selected) | set(old))
        for name, data in contents.items():
            resource = manifest["resources"][name]
            path = under(destination, "resources/" + resource["path"])
            atomic_write_bytes(path, data, mode=0o700 if resource.get("executable") else 0o600)
        receipt = {"schema": 1, "id": manifest["id"], "version": manifest["version"], "digest": fingerprint,
                   "manifest": manifest, "selected": sorted(selected), "imported_at": utc_now(), "execution": "none"}
        atomic_write_json(marker, receipt)
        history(repo, "pack-imported", id=manifest["id"], version=manifest["version"], digest=fingerprint, selected=sorted(selected))
    return receipt


def list_packs(repo: Path) -> list[dict]:
    root = under(state_root(repo), "packs")
    if not root.exists():
        return []
    result = []
    scanned = 0
    with os.scandir(root) as packs:
        for pack in packs:
            if not pack.is_dir(follow_symlinks=False):
                raise CoreError("pack directories must not be symlinks")
            identifier(pack.name, "pack id")
            with os.scandir(pack.path) as versions:
                for version in versions:
                    scanned += 1
                    if scanned > MAX_RECORDS:
                        raise CoreError("pack scan exceeds supported capacity")
                    identifier(version.name, "pack version")
                    if not version.is_dir(follow_symlinks=False):
                        raise CoreError("pack version directories must not be symlinks")
                    receipt = read_json(under(Path(version.path), "pack.json"))
                    if not isinstance(receipt, dict):
                        raise CoreError("incomplete or invalid pack import")
                    result.append({key: receipt.get(key) for key in ("id", "version", "digest", "selected", "imported_at")})
    return sorted(result, key=lambda item: (item["id"], item["version"]))


def pack_source(repo: Path, locator: str) -> tuple[dict, dict[str, Path], dict]:
    match = re.fullmatch(r"pack:([A-Za-z0-9][A-Za-z0-9._-]{0,95})@([A-Za-z0-9][A-Za-z0-9._-]{0,95})/([A-Za-z][A-Za-z0-9_-]{0,63})", locator)
    if not match:
        raise CoreError("pack recipe locators use pack:ID@VERSION/RESOURCE")
    name, version, resource_name = match.groups()
    root = under(state_root(repo), "packs/" + name + "/" + version)
    receipt = read_json(under(root, "pack.json"))
    if not isinstance(receipt, dict):
        raise CoreError("selected pack is not imported")
    manifest = _manifest(receipt.get("manifest"))
    if manifest["id"] != name or manifest["version"] != version or digest(manifest) != receipt.get("digest"):
        raise CoreError("stored pack manifest is inconsistent")
    selected = receipt.get("selected")
    if not isinstance(selected, list) or resource_name not in selected or manifest["resources"].get(resource_name, {}).get("kind") != "recipe":
        raise CoreError("recipe resource was not selected during import")
    resource_root = under(root, "resources")
    _pack_bytes(resource_root, manifest, selected)
    resources = {key: under(resource_root, manifest["resources"][key]["path"]) for key in selected}
    value = load_definition(resources[resource_name])
    return value, resources, {"label": locator, "sha256": digest(value), "pack_digest": receipt["digest"]}


def _workset(value: dict) -> dict:
    object_fields(value, {"schema", "kind", "id", "title", "spec", "repos"}, {"schema", "kind", "id", "spec", "repos"}, "workset")
    if value["schema"] != 1 or type(value["schema"]) is not int or value["kind"] != "workset":
        raise CoreError("expected workset schema 1")
    identifier(value["id"])
    if "title" in value:
        text(value["title"], "workset title", 500)
    if not isinstance(value["repos"], list) or not 1 <= len(value["repos"]) <= 32:
        raise CoreError("workset requires 1..32 repo references")
    names = set()
    for repo in value["repos"]:
        object_fields(repo, {"id", "path", "tasks"}, {"id", "path", "tasks"}, "workset repo")
        identifier(repo["id"], "repo id")
        if repo["id"] in names:
            raise CoreError("workset repo ids must be unique")
        names.add(repo["id"])
        path = text(repo["path"], "repo reference", 1024)
        if path.startswith(("/", "~")) or "\\" in path or ":" in path or os.path.normpath(path) != path:
            raise CoreError("repo references must be normalized paths relative to the coordinator")
        if not isinstance(repo["tasks"], list) or len(repo["tasks"]) > 64 or len(set(repo["tasks"])) != len(repo["tasks"]):
            raise CoreError("task references must be a bounded unique array")
        for task in repo["tasks"]:
            identifier(task, "task reference")
    spec = object_fields(value["spec"], {"repo", "path", "sha256"}, {"repo", "path"}, "shared specification")
    if spec["repo"] not in names:
        raise CoreError("specification must reference a workset repo")
    relative(spec["path"], "specification path")
    if ".oms" in spec["path"].split("/"):
        raise CoreError("worksets link specifications, not private OMS state")
    if "sha256" in spec and (not isinstance(spec["sha256"], str) or not HEX.fullmatch(spec["sha256"])):
        raise CoreError("specification digest must be a lowercase SHA-256")
    return value


def _specification(repo: Path, workset: dict) -> tuple[dict, str | None]:
    spec = workset["spec"]
    selected = next(item for item in workset["repos"] if item["id"] == spec["repo"])
    target = (repo / selected["path"]).resolve()
    if not target.is_dir():
        return spec, None
    path = under(target, spec["path"])
    if not path.exists():
        return spec, None
    return spec, sha256_bytes(read_bytes(path, MAX_RESOURCE_BYTES))


def put_workset(repo: Path, value: dict, *, replace: bool = False) -> dict:
    value = _workset(value)
    spec, actual = _specification(repo, value)
    if actual is None or ("sha256" in spec and spec["sha256"] != actual):
        raise CoreError("shared specification is missing or does not match its digest")
    value = dict(value, spec=dict(spec, sha256=actual))
    state_root(repo, create=True)
    path = state_path(repo, "worksets", value["id"])
    with file_lock(path):
        previous = read_json(path)
        if previous is not None and previous.get("digest") != digest(value) and not replace:
            raise CoreError("workset exists; --replace is required to change its links")
        if previous is None and len(records(repo, "worksets")) >= MAX_RECORDS:
            raise CoreError("workset capacity reached")
        receipt = {"schema": 1, "id": value["id"], "definition": value, "digest": digest(value), "updated_at": utc_now()}
        atomic_write_json(path, receipt)
        history(repo, "workset-linked", id=value["id"], digest=receipt["digest"])
    return show_workset(repo, value["id"])


def show_workset(repo: Path, key: str) -> dict:
    stored = read_json(state_path(repo, "worksets", key))
    if not isinstance(stored, dict) or stored.get("id") != key:
        raise CoreError("workset is missing or invalid")
    definition = _workset(stored.get("definition"))
    if stored.get("digest") != digest(definition):
        raise CoreError("workset definition digest mismatch")
    spec, actual = _specification(repo, definition)
    references = []
    for item in definition["repos"]:
        target = (repo / item["path"]).resolve()
        references.append(dict(item, available=target.is_dir(), head=git_head(target) if target.is_dir() else ""))
    return {"schema": 1, "id": key, "title": definition.get("title", key), "digest": stored["digest"],
            "spec": dict(spec, current_sha256=actual, status="missing" if actual is None else "current" if actual == spec["sha256"] else "changed"),
            "repos": references, "authority": "inspection only; task references do not import state, leases or permissions"}
