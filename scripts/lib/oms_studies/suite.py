"""Controlled cases × variants × repeats and independent evaluation passes."""
from __future__ import annotations

from pathlib import Path
from typing import Any

from oms_runtime.common import CoreError, atomic_write_bytes, canonical_json, read_bytes, read_json, safe_id, sha256_bytes
from . import execution
from .common import (
    MAX_FILE_BYTES, MAX_ROWS, artifact, blob, directions, format_name, identifier, load, metrics,
    obj, put_blob, read_results, records, results, snapshot, store, workdir, writable,
)


def create(repo: Path, spec: Any, name: str | None = None) -> dict:
    writable()
    raw = obj(spec, "suite")
    unknown = set(raw) - {"schema", "kind", "base_id", "cases", "variants", "repeats", "evaluator", "metric_directions"}
    if unknown:
        raise CoreError("unknown suite field: %s" % sorted(unknown)[0])
    if type(raw.get("schema", 1)) is not int or raw.get("schema", 1) != 1 or raw.get("kind", "oms-study-suite") != "oms-study-suite":
        raise CoreError("expected schema 1 oms-study-suite")
    base_id = safe_id(raw.get("base_id"), "base_id")
    metric_directions = directions(raw.get("metric_directions"))
    cases = raw.get("cases")
    variants = raw.get("variants")
    repeats = raw.get("repeats", [0])
    if not isinstance(cases, list) or not 1 <= len(cases) <= 1000:
        raise CoreError("suite cases must contain 1-1000 cases")
    if not isinstance(variants, list) or not 1 <= len(variants) <= 100:
        raise CoreError("suite variants must contain 1-100 variants")
    if (not isinstance(repeats, list) or not repeats or
            any(type(r) is not int or not 0 <= r <= 2**31 - 1 for r in repeats) or
            len(set(repeats)) != len(repeats)):
        raise CoreError("repeats must be unique nonnegative integers")
    if len(cases) * len(variants) * len(repeats) > MAX_ROWS:
        raise CoreError("suite exceeds %d planned cells" % MAX_ROWS)
    frozen_cases, seen = [], set()
    for raw_case in cases:
        case = obj(raw_case, "case")
        if set(case) - {"case_id", "input", "expected", "fixtures"}:
            raise CoreError("case supports case_id, input, expected, and fixtures")
        key = safe_id(case.get("case_id"), "case_id")
        if key in seen:
            raise CoreError("duplicate case_id: %s" % key)
        seen.add(key)
        if "input" not in case or "expected" not in case:
            raise CoreError("each case requires input and expected")
        fixtures = obj(case.get("fixtures", {}), "fixtures")
        if len(fixtures) > 100:
            raise CoreError("case has too many fixtures")
        captured = {safe_id(label, "fixture label"): snapshot(repo, path) for label, path in fixtures.items()}
        frozen_cases.append({"case_id": key, "input": case["input"], "expected": case["expected"], "fixtures": captured})
    frozen_variants, seen = [], set()
    for raw_variant in variants:
        variant = obj(raw_variant, "variant")
        if set(variant) - {"name", "command", "params", "output_format"}:
            raise CoreError("variant supports name, command, params, and optional output_format")
        key = safe_id(variant.get("name"), "variant name")
        if key in seen:
            raise CoreError("duplicate variant: %s" % key)
        seen.add(key)
        params = obj(variant.get("params", {}), "variant params")
        if any(isinstance(v, (dict, list)) for v in params.values()):
            raise CoreError("variant params must contain scalar JSON values")
        captured_variant = {"name": key, "command": execution.freeze(repo, variant.get("command")), "params": params}
        if "output_format" in variant:
            captured_variant["output_format"] = format_name(variant["output_format"])
        frozen_variants.append(captured_variant)
    value = {"schema": 1, "kind": "oms-study-suite", "base_id": base_id,
             "base_sha256": sha256_bytes(canonical_json(frozen_cases)), "cases": frozen_cases,
             "variants": frozen_variants, "repeats": repeats,
             "evaluator": execution.freeze(repo, raw.get("evaluator")), "metric_directions": metric_directions}
    name = name or "suite-" + sha256_bytes(canonical_json(value))[:16]
    value["suite_id"] = safe_id(name, "suite id")
    return store(repo, "suites", name, value)


def cells(suite: dict) -> list[dict]:
    planned = []
    for case in suite["cases"]:
        for variant in suite["variants"]:
            for repeat in suite["repeats"]:
                key = [case["case_id"], variant["name"], repeat]
                planned.append({"cell_id": "cell-" + sha256_bytes(canonical_json(key))[:16],
                                "case_id": key[0], "variant": key[1], "repeat": repeat})
    return planned


def coverage(repo: Path, suite_id: str) -> dict:
    suite = load(repo, "suites", suite_id)
    planned = cells(suite)
    indexed = {(c["case_id"], c["variant"], c["repeat"]): c for c in planned}
    completed, failures, unplanned = {}, {}, []
    for batch in records(repo, "results"):
        if batch.get("suite_sha256") != suite["content_sha256"] or batch.get("evaluation_pass_id"):
            continue
        for row in results(batch)["rows"]:
            key = (row["case_id"], row["variant"], row["repeat"])
            if key not in indexed:
                unplanned.append(row["run_id"])
            elif row["status"] == "complete":
                completed.setdefault(key, []).append(row["run_id"])
            else:
                failures.setdefault(key, []).append({"run_id": row["run_id"], "status": row["status"]})
    missing = [c for key, c in indexed.items() if key not in completed]
    duplicate = [dict(indexed[key], run_ids=run_ids) for key, run_ids in completed.items() if len(run_ids) > 1]
    terminal = {r.get("batch_id") for r in records(repo, "results")}
    pending = [r["batch_id"] for r in records(repo, "attempts")
               if r.get("suite_sha256") == suite["content_sha256"] and r.get("batch_id") not in terminal]
    return {"schema": 1, "kind": "oms-study-coverage", "suite_id": suite_id,
            "suite_sha256": suite["content_sha256"], "planned_count": len(planned),
            "completed_count": len(completed), "missing_count": len(missing), "missing": missing,
            "duplicates": duplicate, "failed": [dict(indexed[k], attempts=v) for k, v in failures.items()],
            "unplanned_run_ids": unplanned, "unfinished_attempt_ids": pending}


def _mapping(cwd: Path, row: dict) -> dict:
    return {"{input}": str(cwd / "input.json"), "{output}": str(cwd / "prediction.json"),
            "{prediction}": str(cwd / "prediction.json"), "{params}": str(cwd / "params.json"),
            "{case_id}": row["case_id"], "{variant}": row["variant"], "{repeat}": str(row["repeat"])}


def _evaluate(repo: Path, evaluator: dict, cwd: Path, row: dict, metric_directions: dict) -> None:
    mapping = _mapping(cwd, row)
    mapping["{output}"] = str(cwd / "metrics.json")
    before = {name: sha256_bytes(read_bytes(cwd / name, MAX_FILE_BYTES)) for name in ("input.json", "prediction.json", "params.json")}
    receipt = execution.execute(repo, evaluator, cwd, mapping)
    row["evaluation"] = receipt
    row["status"] = receipt["status"]
    if row["status"] == "complete":
        try:
            if any(sha256_bytes(read_bytes(cwd / name, MAX_FILE_BYTES)) != sha for name, sha in before.items()):
                raise CoreError("evaluator modified frozen input or prediction bytes")
            for item in row["artifacts"]:
                if item["type"] in ("input", "prediction", "fixture"):
                    blob(repo, item)
                if item["type"] == "fixture" and sha256_bytes(read_bytes(cwd / "fixtures" / item["label"], MAX_FILE_BYTES)) != item["sha256"]:
                    raise CoreError("evaluator modified a fixture")
            output = read_json(cwd / "metrics.json")
            row["metrics"] = metrics(obj(output, "evaluator output").get("metrics"), metric_directions)
            row["artifacts"].append(artifact(repo, read_bytes(cwd / "metrics.json"), "metrics", "Evaluation metrics"))
        except (CoreError, OSError, ValueError) as exc:
            row["status"], row["error"] = "failed", str(exc)


def run_suite(repo: Path, suite_id: str, *, missing: bool = False, selected: list[str] | None = None,
              limit: int | None = None, batch_id: str | None = None) -> dict:
    writable()
    suite = load(repo, "suites", suite_id)
    planned = coverage(repo, suite_id)["missing"] if missing else cells(suite)
    if selected:
        known = {c["cell_id"] for c in cells(suite)}
        if len(set(selected)) != len(selected) or set(selected) - known:
            raise CoreError("selected cells must be unique planned cell IDs")
        planned = [c for c in planned if c["cell_id"] in selected]
    if limit is not None:
        if type(limit) is not int or limit < 1:
            raise CoreError("limit must be a positive integer")
        planned = planned[:limit]
    if not planned:
        raise CoreError("no cells selected; suite coverage is already complete or selection is empty")
    for command in [v["command"] for v in suite["variants"]] + [suite["evaluator"]]:
        execution.verify(repo, command)
    for case in suite["cases"]:
        for fixture in case["fixtures"].values():
            blob(repo, fixture)
    batch_id = safe_id(batch_id or identifier("batch"), "batch id")
    store(repo, "attempts", batch_id, {"kind": "oms-study-attempt", "batch_id": batch_id,
          "suite_id": suite_id, "suite_sha256": suite["content_sha256"], "selected": planned})
    case_map = {c["case_id"]: c for c in suite["cases"]}
    variant_map = {v["name"]: v for v in suite["variants"]}
    rows = []
    for cell in planned:
        case, variant = case_map[cell["case_id"]], variant_map[cell["variant"]]
        row = dict(cell, run_id=identifier("run"), status="failed", metrics={}, params=variant["params"], artifacts=[])
        cwd = workdir(repo, "runs", row["run_id"])
        fixture_paths = {}
        for label, fixture in case["fixtures"].items():
            filename = "fixtures/" + label
            atomic_write_bytes(cwd / filename, blob(repo, fixture), mode=0o400)
            fixture_paths[label] = filename
            row["artifacts"].append(dict(fixture, type="fixture", label=label))
        input_value = {"case_id": case["case_id"], "input": case["input"], "expected": case["expected"],
                       "fixtures": fixture_paths, "repeat": cell["repeat"], "params": variant["params"],
                       "base_id": suite["base_id"]}
        input_bytes = canonical_json(input_value)
        atomic_write_bytes(cwd / "input.json", input_bytes, mode=0o400)
        atomic_write_bytes(cwd / "params.json", canonical_json(variant["params"]), mode=0o400)
        row["artifacts"].append(artifact(repo, input_bytes, "input", "Frozen case input"))
        row["input_sha256"] = sha256_bytes(input_bytes)
        row["generator_sha256"] = variant["command"]["sha256"]
        row["evaluator_sha256"] = suite["evaluator"]["sha256"]
        try:
            row["generation"] = execution.execute(repo, variant["command"], cwd, _mapping(cwd, row))
            row["status"] = row["generation"]["status"]
            if row["status"] == "complete":
                if read_bytes(cwd / "input.json") != input_bytes or read_bytes(cwd / "params.json") != canonical_json(variant["params"]):
                    raise CoreError("generator modified frozen case inputs")
                for label, fixture in case["fixtures"].items():
                    if sha256_bytes(read_bytes(cwd / "fixtures" / label, MAX_FILE_BYTES)) != fixture["sha256"]:
                        raise CoreError("generator modified a frozen fixture")
                prediction = read_bytes(cwd / "prediction.json", MAX_FILE_BYTES)
                metadata = {"format": variant["output_format"]} if variant.get("output_format") else {}
                row["artifacts"].append(artifact(repo, prediction, "prediction", "Generated output", **metadata))
                _evaluate(repo, suite["evaluator"], cwd, row, suite["metric_directions"])
        except (CoreError, OSError, ValueError) as exc:
            row["status"], row["error"] = "failed", str(exc)
        except KeyboardInterrupt:
            row["status"], row["error"] = "interrupted", "study interrupted"
        rows.append(row)
        if row["status"] == "interrupted":
            break
    value = {"schema": 1, "kind": "oms-study-results", "batch_id": batch_id,
             "suite_id": suite_id, "suite_sha256": suite["content_sha256"],
             "base_id": suite["base_id"], "base_sha256": suite["base_sha256"],
             "evaluator_sha256": suite["evaluator"]["sha256"],
             "metric_directions": suite["metric_directions"], "rows": rows,
             "selected_count": len(planned), "unexecuted_cells": planned[len(rows):],
             "execution_scope": "trusted-local-explicit-argv"}
    return store(repo, "results", batch_id, results(value))


def rescore(repo: Path, result_path: str, evaluator_spec: Any, *, pass_id: str | None = None,
            metric_directions: dict | None = None) -> dict:
    writable()
    source, source_sha = read_results(repo, result_path)
    evaluator = execution.freeze(repo, evaluator_spec)
    metric_directions = directions(metric_directions or source["metric_directions"])
    eligible = []
    for source_row in source["rows"]:
        predictions = [a for a in source_row["artifacts"] if a["type"] == "prediction"]
        inputs = [a for a in source_row["artifacts"] if a["type"] == "input"]
        if not predictions:
            continue
        if len(predictions) != 1 or len(inputs) != 1:
            raise CoreError("rescore requires exactly one hashed prediction and input per eligible row")
        for item in predictions + inputs + [a for a in source_row["artifacts"] if a["type"] == "fixture"]:
            blob(repo, item)
        eligible.append((source_row, predictions[0], inputs[0]))
    if not eligible:
        raise CoreError("no existing generated outputs are available to rescore")
    pass_id = safe_id(pass_id or identifier("eval"), "evaluation pass id")
    # All source artifacts are checked before a command starts. The original
    # result bytes are also retained, so later edits cannot move provenance.
    source_bytes = read_bytes(repo / result_path)
    if sha256_bytes(source_bytes) != source_sha:
        raise CoreError("source results changed during evaluation planning")
    source_object = put_blob(repo, source_bytes)
    store(repo, "attempts", pass_id, {"kind": "oms-study-evaluation-attempt", "batch_id": pass_id,
          "evaluation_pass_id": pass_id, "source": source_object, "source_sha256": source_sha,
          "evaluator": evaluator, "run_ids": [row[0]["run_id"] for row in eligible]})
    rows = []
    for old, prediction, input_artifact in eligible:
        row = {k: old[k] for k in ("case_id", "variant", "repeat", "params")}
        row.update(run_id=identifier("evalrun"), source_run_id=old["run_id"], evaluation_pass_id=pass_id,
                   metrics={}, status="failed", artifacts=list(old["artifacts"]),
                   input_sha256=input_artifact["sha256"], prediction_sha256=prediction["sha256"],
                   evaluator_sha256=evaluator["sha256"])
        row["artifacts"] = [a for a in row["artifacts"] if a["type"] != "metrics"]
        cwd = workdir(repo, "evaluations", row["run_id"])
        atomic_write_bytes(cwd / "prediction.json", blob(repo, prediction), mode=0o400)
        atomic_write_bytes(cwd / "input.json", blob(repo, input_artifact), mode=0o400)
        atomic_write_bytes(cwd / "params.json", canonical_json(row["params"]), mode=0o400)
        for item in row["artifacts"]:
            if item["type"] == "fixture":
                atomic_write_bytes(cwd / "fixtures" / safe_id(item["label"], "fixture label"), blob(repo, item), mode=0o400)
        try:
            _evaluate(repo, evaluator, cwd, row, metric_directions)
        except (CoreError, OSError, ValueError) as exc:
            row["status"], row["error"] = "failed", str(exc)
        except KeyboardInterrupt:
            row["status"], row["error"] = "interrupted", "evaluation interrupted"
        rows.append(row)
        if row["status"] == "interrupted":
            break
    value = {"schema": 1, "kind": "oms-study-results", "batch_id": pass_id,
             "evaluation_pass_id": pass_id, "source_sha256": source_sha, "source": source_object,
             "suite_id": source.get("suite_id"), "suite_sha256": source.get("suite_sha256"),
             "base_id": source.get("base_id"), "base_sha256": source.get("base_sha256"),
             "evaluator_sha256": evaluator["sha256"], "metric_directions": metric_directions,
             "generation_rerun": False, "rows": rows,
             "unexecuted_source_run_ids": [r[0]["run_id"] for r in eligible[len(rows):]]}
    return store(repo, "results", pass_id, results(value))
