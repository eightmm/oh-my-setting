---
name: slurm
description: Work on a Slurm cluster through the private cluster reference and the harness job tools instead of re-probing the cluster every time. Use for sbatch/srun submissions, queue or partition questions, node and GPU availability, job monitoring, and post-job log review. Skip on machines without Slurm.
---

# Slurm

Static cluster facts live in the private reference, not in repeated probes.

## Before heavy work

1. Read the cluster reference: `local/slurm.md` under the oh-my-setting
   install root (`oms status` reports the root). It records partitions,
   node/GPU inventory, and — from `sacctmgr show assoc`/`show qos` — your
   account associations and submission limits (GrpTRES, MaxTRES per job,
   MaxJobs, MaxSubmit, MaxWall). Check those limits there before sizing a
   job, instead of discovering them from a rejected submission. Validate or
   refresh with `oms snapshot --cluster --check` (regenerate without
   `--check` when the cluster changed). Never commit it.
2. Probe live state only for what the reference cannot know: `squeue` for the
   current queue, `sacct` for finished jobs. Do not re-run `sinfo` topology
   scans when the reference already answers the question.
3. Confirm partition, account, resources, time limit, output paths, and
   checkpoint plan against `PROJECT.md` before submitting.

## Submission discipline

- Never run long, GPU, or high-CPU work on login nodes — submit it.
- Job scripts start in `$SLURM_SUBMIT_DIR`, fail fast (`set -euo pipefail`),
  and write job-specific logs.
- Long or expensive runs go through `oms run-ledger` (pre-flight gate, one
  JSONL row per run) so parallel sessions see them.

## After a job

- `oms job-digest <log>` distills a job log to agent-sized context instead of
  pasting raw logs.
- `oms run-reconcile` reconciles finished Slurm jobs against the run ledger.
- Waiting on a job? Poll `squeue -j <id>` on a sensible interval, then digest
  the log and report; do not tail interactively.
