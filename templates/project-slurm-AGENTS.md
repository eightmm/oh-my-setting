# Slurm Project Overlay

- Apply with general or ML rules.
- Never run long, GPU, or high-CPU work on login nodes.
- Before heavy work, confirm partition, account, time, resources, logs, and
  checkpoints from `PROJECT.md` or the local cluster reference.
- Use `sbatch` for batch work and allocated `srun` for interactive work.
- Job scripts fail fast, enter `$SLURM_SUBMIT_DIR`, and use job-specific logs.
- Record job id, command, config, seed, commit, checkpoints, and logs.
- Use `slurm-hpc` for cluster discovery, submission, monitoring, and reconcile.
