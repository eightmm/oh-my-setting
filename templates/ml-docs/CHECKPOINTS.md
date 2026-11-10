# CHECKPOINTS

Record the existing checkpoint format and loading contract, not a new schema.

- Save/load implementation and format revision:
- Model, optimizer, scheduler and random state actually stored:
- Code/config/data provenance needed to interpret or resume a run:
- Verified round-trip, resume and conversion checks:
- Inference compatibility versus exact training-resume compatibility:
- Retention/storage policy and approval needed for deletion:

Treat untrusted checkpoints as data, not executable input; preserve the
framework's safe loading path. Do not reset optimizer state silently or mark
all old checkpoints incompatible solely because a version label changed.

## Default checkpoint policy

Applies unless this project's docs or config say otherwise. It is guidance for
agents; OMS does not save checkpoints or enforce it.

- File names: `checkpoints/<run-id>/latest.pt` and
  `checkpoints/<run-id>/best.pt`, with the framework's own extension on the
  same stem (`.ckpt`, `.safetensors`). Never put epoch, step or metric in a
  file name by default; that is what creates a pile. Step and metric live
  inside the checkpoint and in the run record.
- Keep exactly these two files per run. Top-k or periodic snapshots are
  opt-in: the project states k or the period and a disk estimate.
- `latest` is overwritten on an interval sized to the cost of losing work:
  by default at each validation, at least every 30 minutes on
  preemptible/Slurm jobs, and on SIGTERM/requeue; not every epoch by reflex.
  A resumed or requeued job loads `latest`.
- `best` is updated only at a validation, on the validation/selection split
  (never the test split), when the declared metric improves in the declared
  direction. A tie keeps the earlier `best`.
- Validation interval: choose it so total validation time stays a small
  fraction of training time (default target: at most about 10%). Short
  epochs: validate every N epochs or steps, not every epoch. Long epochs:
  validate on a step interval. A full validation pass per epoch is right only
  when it is cheap. State the interval in the config and `PROJECT.md`. Early
  stopping patience counts validations, so state it in validations.
- Write atomically: write a temporary file in the same directory, then rename,
  so a preempted job never leaves a truncated `latest`.
- A checkpoint holds what resuming needs: model, optimizer, scheduler, AMP
  scaler if used, step/epoch, RNG states, the best metric value so far with
  its step, and the config (or its digest) and git commit.
- Record the `best` checkpoint's path, metric value and step with the run (run
  ledger or capsule notes) so a reported number traces to a file. Final test
  evaluation loads `best`, not `latest`.
