"""Append-only corrections and named, immutable baseline bindings."""
from __future__ import annotations

from pathlib import Path
from typing import Any

from oms_runtime.common import CoreError, canonical_json, file_lock, read_bytes, safe_id, sha256_bytes
from .common import (
    blob, finite_json, identifier, load, put_blob, read_results, records,
    state, store, writable,
)


def rationale(value: str) -> str:
    if not isinstance(value, str) or not value.strip() or len(value) > 4000:
        raise CoreError("rationale must contain 1-4000 characters")
    return value.strip()


def history(repo: Path, run_id: str) -> dict:
    safe_id(run_id, "run_id")
    chain = sorted((r for r in records(repo, "annotations") if r.get("run_id") == run_id), key=lambda r: r["sequence"])
    previous = None
    for sequence, item in enumerate(chain, 1):
        if item.get("sequence") != sequence or item.get("supersedes") != (previous["annotation_id"] if previous else None):
            raise CoreError("annotation history is not a contiguous immutable chain")
        if item.get("previous_sha256") != (previous["content_sha256"] if previous else None):
            raise CoreError("annotation predecessor hash mismatch")
        blob(repo, item["expected"])
        previous = item
    return {"schema": 1, "kind": "oms-study-annotation-history", "run_id": run_id, "annotations": chain}


def annotate(repo: Path, result_path: str, run_id: str, expected: Any, reason: str,
             *, supersedes: str | None = None, annotation_id: str | None = None) -> dict:
    writable()
    source, source_sha = read_results(repo, result_path)
    matched = [r for r in source["rows"] if r["run_id"] == run_id]
    if len(matched) != 1:
        raise CoreError("annotation run_id must select one result row")
    row = matched[0]
    finite_json(expected, "corrected expected output")
    reason = rationale(reason)
    inputs = [a for a in row["artifacts"] if a["type"] == "input"]
    predictions = [a for a in row["artifacts"] if a["type"] == "prediction"]
    if len(inputs) != 1 or len(predictions) != 1 or not source.get("suite_id"):
        raise CoreError("annotation requires a suite row with one hashed input and prediction")
    for item in inputs + predictions:
        blob(repo, item)
    name = safe_id(annotation_id or identifier("annotation"), "annotation id")
    with file_lock(state(repo) / "annotation-heads"):
        chain = history(repo, run_id)["annotations"]
        head = chain[-1] if chain else None
        if supersedes != (head["annotation_id"] if head else None):
            raise CoreError("correction must explicitly supersede the current annotation head")
        if head and (head["suite_sha256"] != source.get("suite_sha256") or
                     head["input_sha256"] != inputs[0]["sha256"] or head["prediction_sha256"] != predictions[0]["sha256"]):
            raise CoreError("annotation history cannot move to a different source")
        value = {"kind": "oms-study-annotation", "annotation_id": name, "sequence": len(chain) + 1,
                 "run_id": run_id, "case_id": row["case_id"], "suite_id": source["suite_id"],
                 "suite_sha256": source.get("suite_sha256"), "source_sha256": source_sha,
                 "input_sha256": inputs[0]["sha256"], "prediction_sha256": predictions[0]["sha256"],
                 "expected": put_blob(repo, canonical_json(expected)), "rationale": reason,
                 "supersedes": supersedes, "previous_sha256": head["content_sha256"] if head else None}
        return store(repo, "annotations", name, value)


def export_annotations(repo: Path, suite_id: str, annotation_ids: list[str], name: str) -> dict:
    import json
    writable()
    suite = load(repo, "suites", suite_id)
    if not annotation_ids or len(set(annotation_ids)) != len(annotation_ids):
        raise CoreError("export requires unique selected annotation IDs")
    selected, provenance = {}, []
    for annotation_id in annotation_ids:
        item = load(repo, "annotations", annotation_id)
        history(repo, item["run_id"])
        if item.get("suite_id") != suite_id or item.get("suite_sha256") != suite["content_sha256"]:
            raise CoreError("all selected annotations must bind the exact source suite")
        if item["case_id"] in selected:
            raise CoreError("select only one annotation per case")
        selected[item["case_id"]] = json.loads(blob(repo, item["expected"]).decode("utf-8"))
        provenance.append({"annotation_id": annotation_id, "sha256": item["content_sha256"]})
    cases = [dict(case, expected=selected[case["case_id"]]) for case in suite["cases"] if case["case_id"] in selected]
    if len(cases) != len(selected):
        raise CoreError("annotation refers to a case absent from the source suite")
    base_sha = sha256_bytes(canonical_json(cases))
    value = {k: v for k, v in suite.items() if k not in ("content_sha256", "created_at")}
    value.update(suite_id=safe_id(name, "suite id"), cases=cases, base_sha256=base_sha,
                 base_id="annotated-" + base_sha[:16], source_suite_id=suite_id,
                 source_suite_sha256=suite["content_sha256"], selected_annotations=provenance)
    return store(repo, "suites", name, value)


def bind_baseline(repo: Path, name: str, result_path: str, reason: str,
                  run_ids: list[str] | None = None) -> dict:
    writable()
    name = safe_id(name, "baseline name")
    reason = rationale(reason)
    source, source_sha = read_results(repo, result_path)
    wanted = set(run_ids or [r["run_id"] for r in source["rows"] if r["status"] == "complete"])
    if not wanted or (run_ids and len(wanted) != len(run_ids)):
        raise CoreError("baseline requires unique completed run IDs")
    selected = [r for r in source["rows"] if r["run_id"] in wanted]
    if len(selected) != len(wanted) or any(r["status"] != "complete" for r in selected):
        raise CoreError("baseline selection contains unknown or incomplete runs")
    frozen = []
    for row in selected:
        snapshots = []
        for item in row["artifacts"]:
            # Existing metadata (including structural frames/alignment) is
            # retained while the bytes move to a content-addressed object.
            captured = put_blob(repo, blob(repo, item))
            snapshots.append(dict(item, **captured))
        frozen.append(dict(row, artifacts=snapshots))
    source_bytes = read_bytes(repo / result_path)
    if sha256_bytes(source_bytes) != source_sha:
        raise CoreError("source results changed during baseline binding")
    value = {"kind": "oms-study-baseline", "name": name, "rationale": reason,
             "source_sha256": source_sha, "source": put_blob(repo, source_bytes),
             "suite_id": source.get("suite_id"), "suite_sha256": source.get("suite_sha256"),
             "base_id": source.get("base_id"), "base_sha256": source.get("base_sha256"),
             "evaluator_sha256": source.get("evaluator_sha256"),
             "metric_directions": source["metric_directions"], "rows": frozen}
    return store(repo, "baselines", name, value)


def baseline(repo: Path, name: str) -> dict:
    value = load(repo, "baselines", name)
    blob(repo, value["source"])
    for row in value["rows"]:
        for item in row["artifacts"]:
            blob(repo, item)
    return value
