# CHECKPOINTS

Record the existing checkpoint format and loading contract, not a new schema.

- Save/load implementation and format revision:
- Model, optimizer, scheduler and random state actually stored:
- Code/config/data provenance needed to interpret or resume a run:
- Verified round-trip, resume and conversion checks:
- Inference compatibility versus exact training-resume compatibility:
- Retention/storage policy and approval needed for deletion: (`oms run checkpoints` reports directories that pile up files; it never deletes)

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

## Checked Slurm resume

The supplied `slurm-job.sh` assumes OMS is installed on the execution host and
that `SLURM_SUBMIT_DIR` is the canonical/shared project checkout containing the
original registered contract and experiment run ledger. An SSH Git bundle does
not transfer ignored `.oms` state or install OMS. For a separate Git-only
remote checkout, collect the checkpoint and its manifest through the existing
bounded `runtime ssh-adapter collect` path to the canonical project, validate
there, then stage the exact verified file through the project's reviewed resume
implementation. This template refuses missing canonical state; it does not
create a second remote ledger.

It resumes only when `OMS_CHECKPOINT_MANIFEST` passes
`oms runtime experiment checkpoint --id ID --run-id RUN --manifest FILE`.
A requeued job without that manifest stops with code 75. No newest-file lookup
or automatic fallback to another run is performed. A fresh run can start
without a manifest. The application writes the manifest atomically after the
checkpoint and rechecks the selected bytes immediately before loading.

The bounded JSON manifest has `schema: 1`, `experiment_id`, `run_id`,
`contract_digest`, `arm`, `seed`, `snapshot` (the run's value, or null for local
runs), `code_base`, `dataset_split`, `config_digest`, `evaluation_sha256`,
nonnegative `step`, `rng_state_sha256`, and
`checkpoint: {path, size, sha256}`. The checkpoint path is relative to the
repository; links and changing files are refused. `evaluation_sha256` is the
canonical JSON SHA-256 of the registered `metrics`, `success`, and
`invariant_pack` object. Code/data/config declarations must match the registered
contract; a remote snapshot must match the run, and a local code base must
match current HEAD. Empty provenance is refused. The default file bound is
512 MiB and can be raised explicitly with `--max-bytes` (maximum 64 GiB);
`--timeout-seconds` bounds the verification loop (default 30, maximum 300).

This checks declared provenance and checkpoint bytes. It does not inspect the
framework's serialization or prove optimizer/scheduler/RNG restoration. The
application's safe load and round-trip tests remain necessary, including the
meaning of `step` and the retained RNG-state digest. Keep those tests and the
format contract above with the training implementation.
