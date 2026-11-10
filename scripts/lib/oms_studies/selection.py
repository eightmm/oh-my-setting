"""Paired descriptive comparisons, Pareto fronts, and optional adaptive search."""
from __future__ import annotations

import importlib
import math
from pathlib import Path
from typing import Any

from oms_runtime.common import CoreError, canonical_json
from .common import directions, identifier, number, obj, read_results, results, store, writable


def compare(raw: dict, left: str, right: str) -> dict:
    value = results(raw)
    if left == right:
        raise CoreError("comparison variants must be different")
    names = {r["variant"] for r in value["rows"]}
    if left not in names or right not in names:
        raise CoreError("comparison variant is absent from results")
    paired = {}
    for row in value["rows"]:
        if row["variant"] not in (left, right):
            continue
        for field in ("evaluator_sha256", "suite_sha256", "base_sha256"):
            if row.get(field, value.get(field)) != value.get(field):
                raise CoreError("pairwise comparison has mismatched controls: " + field)
        key = (row["case_id"], row["repeat"])
        side = paired.setdefault(key, {})
        if row["variant"] in side:
            raise CoreError("duplicate completed case/variant/repeat cell; select one attempt before comparing")
        side[row["variant"]] = row
    pairs, unpaired = [], []
    for (case, repeat), sides in sorted(paired.items()):
        complete = {side for side, row in sides.items() if row["status"] == "complete"}
        if complete != {left, right}:
            unpaired.append({"case_id": case, "repeat": repeat, "missing_completed": sorted({left, right} - complete),
                             "statuses": {side: row["status"] for side, row in sides.items()}})
            continue
        first, second = sides[left], sides[right]
        delta = {metric: second["metrics"][metric] - first["metrics"][metric] for metric in value["metric_directions"]}
        for metric, change in delta.items():
            number(change, "paired delta for " + metric)
        pairs.append({"case_id": case, "repeat": repeat, "left_run_id": first["run_id"],
                      "right_run_id": second["run_id"], "delta_right_minus_left": delta})
    summary = {}
    for metric, direction in value["metric_directions"].items():
        deltas = [p["delta_right_minus_left"][metric] for p in pairs]
        favorable = [(d if direction == "max" else -d) for d in deltas]
        summary[metric] = {"direction": direction, "paired_count": len(deltas),
                           "mean_delta_right_minus_left": math.fsum(d / len(deltas) for d in deltas) if deltas else None,
                           "right_wins": sum(v > 0 for v in favorable), "left_wins": sum(v < 0 for v in favorable),
                           "ties": sum(v == 0 for v in favorable)}
    controlled = all(value.get(k) for k in ("suite_sha256", "base_sha256", "evaluator_sha256"))
    return {"schema": 1, "kind": "oms-study-pairwise", "left": left, "right": right,
            "controls": {"recorded": controlled, "suite_sha256": value.get("suite_sha256"),
                         "base_sha256": value.get("base_sha256"), "evaluator_sha256": value.get("evaluator_sha256")},
            "paired_count": len(pairs), "pairs": pairs, "unpaired": unpaired, "metrics": summary,
            "inference": "descriptive paired differences; no significance or causality claim"}


def pareto(raw: dict, objectives: dict | None = None, constraints: dict | None = None) -> dict:
    value = results(raw)
    objectives = directions(objectives or value["metric_directions"])
    constraints = obj(constraints or {}, "constraints")
    for metric, bounds in constraints.items():
        obj(bounds, "constraint bounds")
        if not bounds or set(bounds) - {"min", "max"}:
            raise CoreError("constraints support min and max bounds")
        for bound in bounds.values():
            number(bound, "constraint bound")
        if "min" in bounds and "max" in bounds and bounds["min"] > bounds["max"]:
            raise CoreError("constraint minimum exceeds maximum")
    accepted, excluded = [], []
    required = set(objectives) | set(constraints)
    for row in value["rows"]:
        reason = ""
        if row["status"] != "complete":
            reason = "incomplete"
        elif required - set(row["metrics"]):
            reason = "missing objective or constraint metric"
        else:
            for metric, bounds in constraints.items():
                v = row["metrics"][metric]
                if ("min" in bounds and v < bounds["min"]) or ("max" in bounds and v > bounds["max"]):
                    reason = "constraint violation: " + metric
                    break
        if reason:
            excluded.append({"run_id": row["run_id"], "reason": reason})
        else:
            accepted.append(row)

    def dominates(first, second):
        comparisons = [(first["metrics"][m] <= second["metrics"][m], first["metrics"][m] < second["metrics"][m])
                       if d == "min" else
                       (first["metrics"][m] >= second["metrics"][m], first["metrics"][m] > second["metrics"][m])
                       for m, d in objectives.items()]
        return all(weak for weak, _ in comparisons) and any(strict for _, strict in comparisons)

    front = [row for row in accepted if not any(dominates(other, row) for other in accepted if other is not row)]
    ids = {r["run_id"] for r in front}
    return {"schema": 1, "kind": "oms-study-pareto", "objectives": objectives, "constraints": constraints,
            "selection_unit": "individual completed run", "search_performed": False,
            "front": front, "dominated_run_ids": [r["run_id"] for r in accepted if r["run_id"] not in ids],
            "excluded": excluded, "tie_policy": "retain all equally scoring nondominated runs"}


def propose(repo: Path, result_path: str, space: Any, *, count: int = 1, seed: int = 0,
            proposal_id: str | None = None) -> dict:
    writable()
    if type(count) is not int or not 1 <= count <= 100 or type(seed) is not int or not 0 <= seed < 2**32:
        raise CoreError("proposal count must be 1-100 and seed an unsigned 32-bit integer")
    value, source_sha = read_results(repo, result_path)
    space = obj(space, "search space")
    if not space or len(space) > 30:
        raise CoreError("search space requires 1-30 parameters")
    try:
        optuna = importlib.import_module("optuna")
    except ImportError:
        raise CoreError("adaptive proposals require the optional Optuna dependency in this interpreter; Pareto selection does not")
    distributions = {}
    try:
        for name, spec in space.items():
            obj(spec, "parameter distribution")
            kind = spec.get("type")
            if kind in ("float", "int"):
                if set(spec) - {"type", "low", "high", "log", "step"}:
                    raise CoreError("unknown numeric distribution field")
                number(spec.get("low"), "distribution low")
                number(spec.get("high"), "distribution high")
                if type(spec.get("log", False)) is not bool:
                    raise CoreError("distribution log must be boolean")
                ctor = optuna.distributions.FloatDistribution if kind == "float" else optuna.distributions.IntDistribution
                if kind == "int" and any(type(spec[k]) is not int for k in ("low", "high")):
                    raise CoreError("integer distribution bounds must be integers")
                kwargs = {k: v for k, v in spec.items() if k != "type"}
                distributions[name] = ctor(**kwargs)
            elif kind == "categorical":
                choices = spec.get("choices")
                if set(spec) != {"type", "choices"} or not isinstance(choices, list) or not choices:
                    raise CoreError("categorical distribution needs a nonempty choices list")
                if any(isinstance(v, (dict, list)) for v in choices):
                    raise CoreError("categorical choices must be scalar JSON")
                if len({canonical_json(v) for v in choices}) != len(choices):
                    raise CoreError("categorical choices must be unique")
                distributions[name] = optuna.distributions.CategoricalDistribution(choices)
            else:
                raise CoreError("distribution type must be float, int, or categorical")
        objective_names = sorted(value["metric_directions"])
        sampler = optuna.samplers.TPESampler(seed=seed, n_startup_trials=0)
        study = optuna.create_study(directions=["minimize" if value["metric_directions"][m] == "min" else "maximize"
                                                for m in objective_names], sampler=sampler)
        observations, seen = [], set()
        for row in value["rows"]:
            if row["status"] != "complete":
                continue
            if set(row["params"]) != set(space):
                raise CoreError("each completed observation must have exactly the search-space parameters")
            params = row["params"]
            trial = optuna.trial.create_trial(params=params, distributions=distributions,
                                              values=[float(row["metrics"][m]) for m in objective_names])
            study.add_trial(trial)
            observations.append(row["run_id"])
            seen.add(canonical_json(params))
        if not observations:
            raise CoreError("adaptive proposals need at least one completed observed run")
        proposals = []
        for _ in range(count * 50):
            if len(proposals) == count:
                break
            trial = study.ask(fixed_distributions=distributions)
            params = dict(trial.params)
            key = canonical_json(params)
            if key in seen:
                study.tell(trial, state=optuna.trial.TrialState.FAIL)
                continue
            seen.add(key)
            proposals.append({"proposal_index": len(proposals), "params": params})
    except (TypeError, ValueError, AttributeError, RuntimeError) as exc:
        raise CoreError("Optuna proposal failed: %s" % exc)
    name = proposal_id or identifier("proposal")
    return store(repo, "proposals", name, {"kind": "oms-study-proposals", "proposal_id": name,
          "adapter": "optuna-tpe", "adapter_version": getattr(optuna, "__version__", "unknown"),
          "source_sha256": source_sha, "seed": seed, "space": space,
          "objective_names": objective_names, "observed_run_ids": observations, "proposals": proposals,
          "requested_count": count, "exhausted_or_duplicate_limit": len(proposals) < count,
          "execution_performed": False, "search_performed": True,
          "scope": "reconstruct an in-memory TPE study from supplied observations; no persistent optimizer or execution"})
