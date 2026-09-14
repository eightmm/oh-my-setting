# EVALUATION

Evaluation protocol. Record changes that affect comparison with prior numbers.

## Metrics

| Metric | Definition | Direction | Primary? |
|--------|------------|-----------|----------|
|        |            | up/down   | yes/no   |

- Implementation:
- Locked commit SHA:

## Test Split

- Path:
- Size:
- Deployment population / generalization claim:
- Holdout axes (entity/family/assay/source/time/context):
- Frozen since (date / commit):
- DO NOT touch during model selection.

## Baseline

| Model | Metric | Value | Commit | Run ID |
|-------|--------|-------|--------|--------|
|       |        |       |        |        |

## Eval Command

- Existing evaluation command and observed result:

## Reporting

- Mean ± std over N seeds:
- Uncertainty unit (seed/fold/campaign/donor/etc.):
- CI/bootstrap unit:
- Per-class / per-subgroup breakdown:
- In-domain vs out-of-domain slices:
- Calibration / interval coverage:
- Applicability-domain coverage and rejection policy:

## Regression Policy

- Project-defined acceptance threshold and supporting evidence:
- Approval and recording policy for a new baseline:

## Update Triggers

Record metric, split or evaluation-code changes and assess comparability.
Retain the provenance of prior numbers; re-baseline when the evidence requires it.
