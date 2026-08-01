# ML Project Guidelines

- Prefer repository conventions over global defaults. Read `PROJECT.md`; pause
  only when unresolved choices affect the requested change.
- Inspect the data path, split, loader, model, loss, metric, and tests relevant
  to the change. Do not hide shape/data errors with padding, truncation, broad
  exceptions, silent fallbacks, or skipped tests.
- Record the prediction unit, Inference-time information boundary, immutable
  entity IDs, label meaning/units, split keys, leakage risks, train-only fitted
  transforms, baseline, primary metric direction, and applicability domain in
  `PROJECT.md` before training or evaluation work depends on them.
- Fit preprocessing, feature selection, vocabularies, thresholds, retrieval
  indexes, and hyperparameters on training data only. Validation/test data may
  select checkpoints only when that policy is explicit.
- Verify shapes, dtypes, masks, devices, NaN/Inf handling, empty/small batches,
  loss reduction, evaluation mode, metric aggregation, and checkpoint symmetry
  at the affected interfaces.
- Run focused data, one-batch model, loss, and resume/inference smoke checks
  before long training or expensive compute. Never launch heavy work from an
  unverified data/model contract.
- For claim-bearing experiments, pre-register the hypothesis, predicted
  outcome, one changed variable, baseline, metric/split, and stop threshold;
  record negative and null results too.
- Keep commands, paths, data versions, seeds, configs, metrics, and required
  checks in `PROJECT.md`; keep data, outputs, checkpoints, and secrets out of git.

Use `oms data-manifest` for registered split/leakage checks and `oms
research-runner` for pre-registered claims; use the raw run ledger for
mechanical runs. Queue workstation jobs with `oms tsp-queue`. For Slurm work,
refresh the private cluster snapshot with `oms snapshot --cluster` and use
`oms job-digest` or `oms run-reconcile` for long or finished jobs.

Use `scripts/check.sh fast` as the default project verification contract when
the project provides it; use its GPU mode only when the change requires GPU
behavior.
