# Slurm Project Overlay

- Never run long, GPU, or high-CPU work on login nodes.
- Before heavy work, confirm partition/account/resources/time, outputs, and
  checkpoints from `PROJECT.md` or the private cluster snapshot.
- Use `sbatch` for batch jobs and allocated `srun` interactively.
- Job scripts enter `$SLURM_SUBMIT_DIR`, fail fast, and write job-specific logs.
- Record job ID, command, config, seed, commit, checkpoints, and logs.
- Refresh private cluster data with `oms generate-slurm-reference`; never commit or
  share it.
- Use `oms job-digest` for long logs and `oms run-reconcile` for finished jobs.
