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

- Keep exactly two checkpoints per run: `latest` (overwritten at every
  checkpoint interval; what a resumed or requeued job loads) and `best`
  (overwritten only when the declared selection metric improves, in the
  declared direction, on the validation/selection split, never the test split).
- Keep no per-epoch or per-step checkpoints by default. Top-k or periodic
  snapshots are opt-in and must state a disk estimate.
- Write atomically: write a temporary file in the same directory, then rename,
  so a preempted job never leaves a truncated `latest`.
- A checkpoint holds what resuming needs: model, optimizer, scheduler, AMP
  scaler if used, step/epoch, RNG states, the best metric value so far with
  its step, and the config (or its digest) and git commit.
- Record the `best` checkpoint's path, metric value and step with the run (run
  ledger or capsule notes) so a reported number traces to a file. Final test
  evaluation loads `best`, not `latest`.
