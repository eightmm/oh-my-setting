# Slurm Project Overlay

- Slurm rules are an overlay on top of `general` or `ml`; do not replace base project rules.
- No long/GPU/high-CPU jobs on login nodes.
- Use `sbatch` for batch jobs or allocated `srun` for interactive work.
- Confirm partition/account/time/GPU/CPU/memory/output path before heavy jobs.
- Job scripts use `set -euo pipefail`, `cd "$SLURM_SUBMIT_DIR"`, and `%x-%j` logs.
- Python commands run through `uv run` when the project uses `uv`.
- Track job id, command, config, seed, commit, checkpoint, and log path.

## Slurm Defaults

- Partition name:
- Account:
- GPU type/count:
- CPUs:
- Memory:
- Time:
- Log path:
- Checkpoint path:

## Commands

- Queue: `squeue -u "$USER"`
- Cluster overview: `sinfo`
- Associations/accounts: `sacctmgr show assoc user="$USER" -p`
- QOS and limits: `sacctmgr show qos -p`
- Partition configuration: `scontrol show partition`
- Node configuration: `scontrol show node`
- Submit:
- Inspect:
- Cancel:
