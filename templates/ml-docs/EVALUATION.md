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
- CI (bootstrap): yes/no
- Per-class / per-subgroup breakdown: yes/no

## Regression Policy

- Drop > X% on primary metric vs baseline -> block merge.
- New baseline requires PR review + `EXPERIMENTS.md` entry.

## Update Triggers

Metric definition, test split, or eval code change -> bump eval version + re-baseline.
