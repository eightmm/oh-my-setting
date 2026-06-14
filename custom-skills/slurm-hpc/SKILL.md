---
name: slurm-hpc
description: >
  Slurm/HPC workflow helper. Use when working on clusters, partitions, nodes,
  sbatch/srun jobs, GPU/CPU allocations, queues, logs, checkpoints, or any
  resource-heavy command. Reads local cluster reference when generated.
---

Default: protect login nodes. Ask before expensive or unclear resource use.

## Rules

- If Slurm commands exist, prefer `sbatch` for long jobs and allocated `srun` for interactive work.
- Never run long/GPU/high-CPU jobs directly on login nodes.
- Confirm partition, account, time, GPU/CPU/mem, output path before heavy jobs.
- Job scripts should use `set -euo pipefail`, `cd "$SLURM_SUBMIT_DIR"`, and `%x-%j` logs.
- Python jobs should use project-local commands, usually `uv run`.
- Track job id, command, config, seed, commit, checkpoint, and log path.

## Local Cluster Reference

If present, read `references/cluster.generated.md` before suggesting partitions,
nodes, GPU types, limits, or default Slurm resources.

If missing, ask the user to run:

```bash
~/.oh-my-setting/scripts/generate-slurm-skill.sh
```

## Useful Commands

```bash
sinfo
squeue -u "$USER"
scontrol show partition
scontrol show node <node>
sacct -j <job_id>
scancel <job_id>
```

## Reconciling Async Jobs

A long Slurm job outlives the agent session that launched it. The run ledger
records the launch (with `slurm_job_id`), but not the terminal state, so
"is it done? did it OOM?" goes stale and agents relaunch duplicates. Reconcile
recorded jobs against `sacct`/`squeue` and write the outcome back so the next
agent — any of the three — sees ground truth.

```bash
~/.oh-my-setting/scripts/run-reconcile.sh scan            # current state per tracked job id
~/.oh-my-setting/scripts/run-reconcile.sh apply --memory  # record FINISHED jobs + note to shared memory
~/.oh-my-setting/scripts/run-reconcile.sh list
```

It only records terminal jobs (COMPLETED/FAILED/TIMEOUT/OOM/…), is idempotent
(skips already-reconciled ids), and saves a `job-digest` summary per job under
`.oms/runs/reconcile/`. Run it on session start when work may have finished
while you were away.
