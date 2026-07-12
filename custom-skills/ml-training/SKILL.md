---
name: ml-training
description: >
  PyTorch training correctness for optimizers and schedules, distributed/DDP,
  variable-size masked losses, checkpoint save/load, CUDA runtime settings,
  and equivariant model checks. Use when writing or reviewing training loops,
  optimizer/scheduler setup, multi-GPU code, loss reduction, or checkpoints.
---

# ML Training

Treat optimizer, schedule, DDP, and checkpoint choices as project contracts,
not universal defaults.

## Universal Invariants

- Establish a simple AdamW or project baseline before adopting specialized
  optimizers or schedules.
- Declare the loss unit and denominator. Exclude padded and missing labels
  before reduction.
- In DDP, every rank must execute the same collectives in the same order.
- Save portable unwrapped model weights; rank 0 writes, every rank loads.
- Resume must restore every state that affects the next update: model,
  optimizer, scheduler, scaler, step/epoch, sampler position when applicable,
  and RNG state required by the reproducibility contract.
- Do not enable `static_graph` or `find_unused_parameters` without evidence
  that the model graph requires and supports it.
- Load untrusted checkpoints with `weights_only=True`; never deserialize
  arbitrary Python objects from an untrusted source.

## Route to the Relevant References

Start with the closest reference. Load additional references only when the
request crosses contracts such as distributed masking plus checkpointing.

- Parameter grouping, AdamW baseline, optional Muon, short-run-safe schedules:
  [optimizer-schedule.md](references/optimizer-schedule.md)
- DDP initialization, sampler, collectives, validation, cleanup:
  [distributed.md](references/distributed.md)
- Variable atoms/residues/pairs and sparse-label loss normalization:
  [loss-masking.md](references/loss-masking.md)
- Portable save/load and exact-resume state:
  [checkpoint.md](references/checkpoint.md)
- SE(3)/equivariance implementation and transformation tests:
  [equivariance.md](references/equivariance.md)

For experiment design use `research-method`; for molecule/protein data splits,
labels, and metrics use `chem-bio-ml`; for cluster allocation use `slurm-hpc`.

## Verification Ladder

1. One CPU batch: forward, loss, backward, optimizer step.
2. One device batch with the production dtype/masking path.
3. Checkpoint save/load round trip and next-step equivalence when resume matters.
4. Two-rank smoke test for DDP code, including unequal valid counts.
5. Only then run a long or multi-node job.
