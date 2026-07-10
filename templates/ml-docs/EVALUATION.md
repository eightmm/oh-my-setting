# EVALUATION

Evaluation protocol. Locked — changes invalidate prior numbers.

## Metrics

| Metric | Definition | Direction | Primary? |
|--------|------------|-----------|----------|
|        |            | up/down   | yes/no   |

- Implementation: `src/<pkg>/evaluation/`
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

```bash
uv run python scripts/eval.py --config configs/eval.yaml --ckpt <path>
```

## Reporting

- Mean ± std over N seeds:
- Uncertainty unit (seed/fold/campaign/donor/etc.):
- CI/bootstrap unit:
- Per-class / per-subgroup breakdown:
- In-domain vs out-of-domain slices:
- Calibration / interval coverage:
- Applicability-domain coverage and rejection policy:

## Regression Policy

- Drop > X% on primary metric vs baseline -> block merge.
- New baseline requires PR review + `EXPERIMENTS.md` entry.

## Update Triggers

Metric definition, test split, or eval code change -> bump eval version + re-baseline.
