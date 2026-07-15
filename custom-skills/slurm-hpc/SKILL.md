---
name: slurm-hpc
description: >
  Slurm/HPC helper for cluster discovery, resource allocation, job submission,
  queue/status checks, logs, checkpoints, and finished-job reconciliation.
---

# Slurm/HPC

Protect login nodes. Ask before expensive or unclear allocations.

- Read `references/cluster.generated.md` when present before choosing partition,
  account, QOS, GPU, CPU, memory, or time. Never guess missing cluster values.
- Use `sbatch` for long jobs and allocated `srun` for interactive work. Never run
  long, GPU, or high-CPU work directly on a login node.
- Confirm resources, output path, and checkpoint behavior before submission.
- Use project-local commands; record job id, command, config, seed, commit,
  checkpoint, and log path.

Route by intent:

- Discover or submit: inspect the generated reference and native Slurm help as
  needed.
- Long log: `oms job-digest <log-or-job-id>`.
- Finished or stale job state: `oms run-reconcile scan`, then
  `oms run-reconcile apply --memory` for terminal jobs.

If no cluster reference exists and cluster-specific advice is needed, generate
it with `oms generate-slurm-skill`. Keep generated machine/cluster details out
of prompts, git, and shared state.
