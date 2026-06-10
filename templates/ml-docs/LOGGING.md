# LOGGING

Run tracking and observability.

## Tracker: Weights & Biases

- Project name:
- Entity:
- Mode: `online` (default) / `offline` (HPC w/o net) / `disabled` (debug)

```python
import wandb
wandb.init(
    project=PROJECT, entity=ENTITY,
    name=run_name, group=group, tags=tags,
    config=config, resume="allow", id=run_id,
)
```

- DDP: init on rank 0 only.

## What to Log

Step level:
- `train/loss`, `train/lr`, `train/grad_norm`
- `train/throughput_samples_per_sec`
- `train/gpu_mem_alloc`, `train/gpu_mem_reserved`

Epoch / eval:
- `val/<metric>`, `val/loss`
- `test/<metric>` (only at final eval)

Artifacts:
- Config snapshot (yaml)
- Best checkpoint
- Eval plots

## Local Logs

- stdout: structured (JSON line or `[step N] key=val`)
- file: `outputs/logs/<run_id>.log`
- tqdm: rank 0 only

## Console Discipline

- No `print` for metrics — use logger.
- No emoji / decoration in log lines.
- One metric per key; do not concat.

## Resume

- `wandb.init(id=run_id, resume="allow")`
- Match seed, data version, model version from checkpoint.

## Update Triggers

New metric or removed metric -> update this file + `EVALUATION.md` if it affects reporting.
