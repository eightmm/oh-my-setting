"""Offline, source-bound comparison views; browser actions are explicit imports."""

from __future__ import annotations

import argparse
import base64
import html
import json
import math
import os
from pathlib import Path
import re
import selectors
import shlex
import signal
import subprocess
import sys
import time
from typing import Any

from oms_runtime.common import (
    CoreError, SECRET_VALUE_RE, atomic_write_bytes, atomic_write_json,
    canonical_json, file_lock, read_bytes, read_json, read_jsonl,
    repo_root, safe_id, sha256_bytes, utc_now,
)

ASSETS = Path(__file__).with_name("artifact_view_assets")
MAX_ROWS = 10000
MAX_ASSET = 2 * 1024 * 1024
MAX_BUNDLE = 24 * 1024 * 1024
MAX_EVENTS = 1000
MOLECULE_FORMATS = {"pdb", "sdf", "mol2", "xyz"}


def clean_text(value: Any, label: str, limit: int = 4000) -> str:
    if not isinstance(value, str) or len(value) > limit or "\x00" in value:
        raise CoreError("invalid or oversized %s" % label)
    if SECRET_VALUE_RE.search(value):
        raise CoreError("%s appears to contain credentials; no view was written" % label)
    return value


def relative_file(root: Path, name: str) -> Path:
    clean_text(name, "artifact path", 1000)
    path = Path(name)
    if path.is_absolute() or any(part in {"..", ".git"} for part in path.parts) or not path.parts:
        raise CoreError("artifact paths must remain inside the selected repository")
    current = root
    for part in path.parts:
        current /= part
        if current.is_symlink():
            raise CoreError("artifact path traverses a symlink")
    if not current.resolve().is_relative_to(root.resolve()):
        raise CoreError("artifact path escaped the selected repository")
    return current


def row_key(row: dict[str, Any]) -> str:
    identity = [row.get(key) for key in ("run_id", "case_id", "variant", "repeat")]
    return "row-" + sha256_bytes(canonical_json(identity))[:24]


def finite_number(value: Any) -> bool:
    try:
        return not isinstance(value, bool) and isinstance(value, (int, float)) and math.isfinite(value)
    except OverflowError:
        return False


def normalize(data: Any) -> dict[str, Any]:
    if not isinstance(data, dict) or type(data.get("schema")) is not int or data.get("schema") != 1 or data.get("kind") != "oms-study-results":
        raise CoreError("expected schema 1, kind oms-study-results")
    raw_rows = data.get("rows")
    if not isinstance(raw_rows, list) or len(raw_rows) > MAX_ROWS:
        raise CoreError("rows must be an array of at most %d records" % MAX_ROWS)
    directions = data.get("metric_directions", {})
    if not isinstance(directions, dict) or any(not isinstance(v, str) or v not in {"min", "max"} for v in directions.values()):
        raise CoreError("metric_directions values must be min or max")
    rows, keys = [], set()
    for raw in raw_rows:
        if not isinstance(raw, dict):
            raise CoreError("every row must be an object")
        row = {key: clean_text(raw.get(key, ""), key, 300)
               for key in ("run_id", "case_id", "variant")}
        if not all(row.values()):
            raise CoreError("run_id, case_id and variant are required")
        repeat = raw.get("repeat", 0)
        if not isinstance(repeat, int) or isinstance(repeat, bool) or not 0 <= repeat <= 9007199254740991:
            raise CoreError("repeat must be a nonnegative browser-safe integer")
        row["repeat"] = repeat
        row["status"] = clean_text(raw.get("status", "complete"), "status", 60)
        for field in ("metrics", "params"):
            values = raw.get(field, {})
            if not isinstance(values, dict) or len(values) > 128:
                raise CoreError("%s must be an object of at most 128 entries" % field)
            result = {}
            for name, value in values.items():
                clean_text(name, field + " name", 160)
                if field == "metrics":
                    if value is not None and not finite_number(value):
                        raise CoreError("metrics must be finite numbers or null")
                elif value is not None and not isinstance(value, (str, int, float, bool)):
                    raise CoreError("params values must be scalar")
                elif isinstance(value, (int, float)) and not isinstance(value, bool) and not finite_number(value):
                    raise CoreError("params must be finite")
                if isinstance(value, str):
                    clean_text(value, field + " value", 1000)
                result[name] = value
            row[field] = result
        artifacts = raw.get("artifacts", [])
        if not isinstance(artifacts, list) or len(artifacts) > 200:
            raise CoreError("artifacts must be an array of at most 200 records")
        row["artifacts"] = []
        for item in artifacts:
            if not isinstance(item, dict):
                raise CoreError("artifact must be an object")
            artifact = {key: clean_text(item.get(key, ""), "artifact " + key, 1000)
                        for key in ("path", "type", "label")}
            path = Path(artifact["path"])
            if path.is_absolute() or not path.parts or any(x in {"..", ".git"} for x in path.parts):
                raise CoreError("artifact paths must be repository-relative")
            for key in ("sha256", "format", "frame", "alignment"):
                if key in item:
                    artifact[key] = clean_text(item[key], key, 1000)
            if "sha256" in artifact and not re.fullmatch(r"[0-9a-f]{64}", artifact["sha256"]):
                raise CoreError("invalid artifact sha256")
            row["artifacts"].append(artifact)
        row["key"] = row_key(row)
        if row["key"] in keys:
            raise CoreError("duplicate run/case/variant/repeat identity")
        keys.add(row["key"])
        rows.append(row)
    result = {"schema": 1, "kind": "oms-study-results", "rows": rows,
              "metric_directions": {clean_text(k, "metric", 160): v for k, v in directions.items()}}
    for key in ("suite_id", "suite_sha256", "base_id", "base_sha256", "evaluator_sha256", "evaluation_pass_id", "source_sha256"):
        if data.get(key) is not None:
            result[key] = clean_text(data[key], key, 300)
    return result


def embed_artifacts(data: dict[str, Any], reader: Any) -> None:
    total = 0
    for row in data["rows"]:
        for item in row["artifacts"]:
            suffix = item.get("format", Path(item["path"]).suffix.lstrip(".")).lower()
            supported = suffix in MOLECULE_FORMATS | {"png", "jpg", "jpeg", "gif", "webp", "txt", "json", "csv", "tsv", "md"}
            if not supported:
                item["availability"] = "reference only: unsupported embedding format"
                continue
            raw = reader(item["path"])
            total += len(raw)
            if len(raw) > MAX_ASSET or total > MAX_BUNDLE // 2:
                raise CoreError("embedded artifacts exceed the per-file or total size limit")
            digest = sha256_bytes(raw)
            if item.get("sha256") and item["sha256"] != digest:
                raise CoreError("artifact bytes changed: %s" % item["path"])
            item["sha256"] = digest
            item["format"] = suffix
            if suffix in {"png", "jpg", "jpeg", "gif", "webp"}:
                valid = ((suffix == "png" and raw.startswith(b"\x89PNG\r\n\x1a\n")) or
                         (suffix in {"jpg", "jpeg"} and raw.startswith(b"\xff\xd8\xff")) or
                         (suffix == "gif" and raw.startswith((b"GIF87a", b"GIF89a"))) or
                         (suffix == "webp" and raw.startswith(b"RIFF") and raw[8:12] == b"WEBP"))
                if not valid:
                    raise CoreError("image signature does not match its extension")
                mime = "image/" + ("jpeg" if suffix == "jpg" else suffix)
                item["image"] = "data:%s;base64,%s" % (mime, base64.b64encode(raw).decode("ascii"))
            else:
                try:
                    item["text"] = clean_text(raw.decode("utf-8"), "artifact content", MAX_ASSET)
                except UnicodeDecodeError as exc:
                    raise CoreError("artifact must be UTF-8") from exc


def read_capsules(repo: Path, ids: list[str]) -> dict[str, Any]:
    rows = []
    for ident in ids:
        safe_id(ident)
        if ident in {".", ".."} or "/" in ident:
            raise CoreError("invalid capsule id")
        path = relative_file(repo, ".oms/runs/%s/capsule.json" % ident)
        capsule = read_json(path)
        if not isinstance(capsule, dict) or capsule.get("id") != ident:
            raise CoreError("capsule missing or identity differs")
        metrics = capsule.get("result", {}).get("metrics") or {}
        if not isinstance(metrics, dict):
            raise CoreError("capsule metrics must be an object")
        params = {}
        for config in capsule.get("configs", []):
            if isinstance(config, dict) and isinstance(config.get("params"), dict):
                for key, value in config["params"].items():
                    params["%s:%s" % (config.get("path", "config"), key)] = value
        rows.append({"run_id": ident, "case_id": "aggregate", "variant": ident, "repeat": 0,
                     "status": "complete" if capsule.get("result", {}).get("exit") == 0 else "failed",
                     "metrics": metrics, "params": params, "artifacts": []})
    return normalize({"schema": 1, "kind": "oms-study-results", "rows": rows})


def render(data: dict[str, Any], title: str, molecule_js: bytes = b"") -> tuple[str, dict[str, Any]]:
    body = canonical_json(data)
    digest = sha256_bytes(body)
    ident = "view-" + digest[:24]
    payload = {"view_id": ident, "data_sha256": digest, "title": title, "data": data}
    # Base64 avoids script terminators and HTML parser interpretations of source data.
    encoded = base64.b64encode(canonical_json(payload)).decode("ascii")
    js = read_bytes(ASSETS / "analysis.js").decode("utf-8") + "\n" + read_bytes(ASSETS / "view.js").decode("utf-8")
    css = read_bytes(ASSETS / "view.css").decode("utf-8")
    page = read_bytes(ASSETS / "view.html").decode("utf-8")
    vendor = base64.b64encode(molecule_js).decode("ascii")
    page = page.replace("__TITLE__", html.escape(title)).replace("__CSS__", css)
    page = page.replace("__PAYLOAD__", encoded).replace("__MOLECULE_JS__", vendor).replace("__JS__", js)
    if len(page.encode("utf-8")) > MAX_BUNDLE:
        raise CoreError("rendered view exceeds 24 MiB")
    meta = {"schema": 1, "id": ident, "data_sha256": digest, "title": title,
            "row_keys": [r["key"] for r in data["rows"]], "created_at": utc_now(),
            "molecule_js_sha256": sha256_bytes(molecule_js) if molecule_js else None}
    return page, meta


def save_view(repo: Path, data: dict[str, Any], args: argparse.Namespace, reader: Any) -> dict[str, Any]:
    embed_artifacts(data, reader)
    vendor = read_bytes(Path(args.molecule_js).expanduser(), 4 * 1024 * 1024) if args.molecule_js else b""
    title = clean_text(args.title, "title", 200)
    page, meta = render(data, title, vendor)
    root = repo / ".oms" / "artifact-views"
    output = relative_file(repo, args.output) if args.output else root / (meta["id"] + ".html")
    if output.suffix != ".html":
        raise CoreError("view output must end in .html")
    if output.exists() or output.is_symlink():
        if not args.replace:
            raise CoreError("output exists; use --replace to replace this view")
        read_bytes(output, MAX_BUNDLE)
    meta["html_sha256"] = sha256_bytes(page.encode("utf-8"))
    meta["path"] = output.relative_to(repo).as_posix()
    # The source-bound object remains stable when another title/location is rendered.
    with file_lock(root / "registry"):
        saved = read_json(root / (meta["id"] + ".json"))
        if saved and saved.get("data_sha256") != meta["data_sha256"]:
            raise CoreError("view identity collision")
        if output.exists() and not args.replace:
            raise CoreError("output exists; use --replace to replace this view")
        atomic_write_bytes(output, page.encode("utf-8"))
        atomic_write_json(root / (meta["id"] + ".json"), meta)
    return meta


def import_events(repo: Path, source: Path) -> dict[str, Any]:
    data = read_json(source, limit=1024 * 1024)
    if not isinstance(data, dict) or type(data.get("schema")) is not int or data.get("schema") != 1 or data.get("kind") != "oms-artifact-feedback":
        raise CoreError("expected oms-artifact-feedback schema 1")
    ident = safe_id(data.get("view_id"), "view_id")
    root = repo / ".oms" / "artifact-views"
    meta = read_json(relative_file(repo, ".oms/artifact-views/" + ident + ".json"))
    if not meta or data.get("data_sha256") != meta.get("data_sha256"):
        raise CoreError("feedback does not identify a registered source view")
    raw_events = data.get("events")
    if not isinstance(raw_events, list) or len(raw_events) > MAX_EVENTS:
        raise CoreError("at most %d feedback events per import" % MAX_EVENTS)
    events = []
    known = set(meta["row_keys"])
    for raw in raw_events:
        if not isinstance(raw, dict) or raw.get("row_key") not in known:
            raise CoreError("feedback names a row outside this view")
        if raw.get("action") not in {"select", "accept", "reject", "uncertain", "comment"}:
            raise CoreError("unsupported feedback action")
        event = {"row_key": raw["row_key"], "action": raw["action"],
                 "comment": clean_text(raw.get("comment", ""), "comment")}
        events.append(event)
    report = data.get("report", {})
    if not isinstance(report, dict) or set(report) - {"title", "question", "decision", "next"}:
        raise CoreError("invalid report fields")
    report = {k: clean_text(v, "report " + k, 12000) for k, v in report.items()}
    batch = {"view_id": ident, "data_sha256": data["data_sha256"], "events": events, "report": report}
    batch_id = sha256_bytes(canonical_json(batch))
    target = root / "feedback.jsonl"
    with file_lock(target):
        rows = read_jsonl(target)
        if any(row.get("batch_id") == batch_id for row in rows):
            return {"imported": False, "duplicate": True, "batch_id": batch_id}
        batch.update({"schema": 1, "batch_id": batch_id, "created_at": utc_now()})
        original = read_bytes(target, 32 * 1024 * 1024) if target.exists() else b""
        if original and not original.endswith(b"\n"):
            raise CoreError("feedback ledger has an incomplete final row")
        atomic_write_bytes(target, original + canonical_json(batch) + b"\n")
    return {"imported": True, "events": len(events), "batch_id": batch_id}


def ssh_read(host: str, root: str, name: str) -> bytes:
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.@-]{0,200}", host):
        raise CoreError("host must be a configured SSH alias or user@host")
    if not root.startswith("/") or "\x00" in root or "\n" in root:
        raise CoreError("remote root must be an absolute directory")
    p = Path(name)
    if p.is_absolute() or not p.parts or any(x in {"..", ".git"} for x in p.parts):
        raise CoreError("remote artifact must be relative to --remote-root")
    # A bounded stdlib reader rejects symlink descendants on the selected host.
    # ssh sends one shell command: quote code and all user-provided arguments.
    remote_code = "\n".join([
        "import pathlib,stat,sys",
        "base=pathlib.Path(sys.argv[1]).resolve(strict=True)",
        "if not base.is_dir(): raise ValueError('root is not a directory')",
        "path=base",
        "for part in pathlib.Path(sys.argv[2]).parts:",
        " path=path/part",
        " if path.is_symlink(): raise ValueError('symlink refused')",
        "if not path.resolve().is_relative_to(base): raise ValueError('path escaped root')",
        "info=path.stat()",
        "if not stat.S_ISREG(info.st_mode): raise ValueError('not a regular file')",
        "limit=int(sys.argv[3])",
        "if info.st_size>limit: raise ValueError('file too large')",
        "with path.open('rb') as handle: sys.stdout.buffer.write(handle.read(limit+1))",
    ])
    command = shlex.join(["python3", "-I", "-c", remote_code, root, name, str(MAX_ASSET)])
    process = None
    try:
        process = subprocess.Popen(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "--", host, command],
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                   start_new_session=True)
        output = bytearray()
        deadline = time.monotonic() + 20
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise CoreError("remote snapshot read timed out")
                for key, _ in selector.select(min(remaining, 1)):
                    chunk = os.read(key.fd, min(65536, MAX_ASSET + 1 - len(output)))
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    output.extend(chunk)
                    if len(output) > MAX_ASSET:
                        raise CoreError("remote snapshot file exceeds 2 MiB")
        code = process.wait(timeout=max(.01, deadline - time.monotonic()))
        if code:
            raise CoreError("remote snapshot read failed (ssh exit %d)" % code)
        return bytes(output)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise CoreError("remote snapshot read failed") from exc
    finally:
        if process is not None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            process.stdout.close()


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="oms artifact-view", description=__doc__)
    p.add_argument("--repo", default=".")
    sub = p.add_subparsers(dest="action", required=True)
    for name in ("build", "capsules", "remote"):
        cmd = sub.add_parser(name)
        cmd.add_argument("--output", help="repository-relative .html output (default private .oms view)")
        cmd.add_argument("--title", default="Experiment comparison")
        cmd.add_argument("--molecule-js", help="explicit local 3Dmol.js asset, embedded for offline use")
        cmd.add_argument("--replace", action="store_true")
        if name == "build":
            cmd.add_argument("--input", required=True)
        elif name == "capsules":
            cmd.add_argument("ids", nargs="+")
        else:
            cmd.add_argument("--host", required=True)
            cmd.add_argument("--remote-root", required=True)
            cmd.add_argument("--input", required=True, help="path relative to remote root")
    incoming = sub.add_parser("import-events")
    incoming.add_argument("file")
    events = sub.add_parser("events")
    events.add_argument("--view")
    sub.add_parser("list")
    return p


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        repo = repo_root(args.repo)
        is_child = os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0"
        if is_child and args.action not in {"list", "events"}:
            raise CoreError("a harness child may only list recorded views and feedback")
        if args.action in {"build", "capsules"}:
            data = (normalize(read_json(Path(args.input).expanduser())) if args.action == "build"
                    else read_capsules(repo, args.ids))
            result = save_view(repo, data, args, lambda name: read_bytes(relative_file(repo, name), MAX_ASSET))
        elif args.action == "remote":
            reader = lambda name: ssh_read(args.host, args.remote_root, name)
            data = normalize(json.loads(reader(args.input)))
            result = save_view(repo, data, args, reader)
            result["snapshot"] = "remote read-only; not a live view"
        elif args.action == "import-events":
            result = import_events(repo, Path(args.file).expanduser())
        elif args.action == "events":
            result = {"feedback": [r for r in read_jsonl(relative_file(repo, ".oms/artifact-views/feedback.jsonl"))
                                   if not args.view or r.get("view_id") == args.view]}
        else:
            root = relative_file(repo, ".oms/artifact-views")
            paths = sorted(root.glob("view-*.json"))
            if len(paths) > MAX_ROWS:
                raise CoreError("view catalog exceeds 10,000 records; archive old views")
            result = {"views": [read_json(p) for p in paths]}
        print(json.dumps(result, ensure_ascii=False, allow_nan=False, sort_keys=True))
        return 0
    except (CoreError, OSError, ValueError, TypeError) as exc:
        print("artifact-view: %s" % exc, file=sys.stderr)
        return getattr(exc, "exit_code", 2)


if __name__ == "__main__":
    raise SystemExit(main())
