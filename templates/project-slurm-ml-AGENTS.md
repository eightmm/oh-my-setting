# Slurm ML Project Guidelines

- Follow `templates/project-ml-AGENTS.md`.
- No long/GPU/high-CPU jobs on login nodes.
- Use `sbatch` for batch jobs or allocated `srun` for interactive work.
- Confirm partition/account/time/GPU/CPU/mem/output path before heavy jobs.
- Job scripts use `set -euo pipefail`, `cd "$SLURM_SUBMIT_DIR"`, `%x-%j` logs.
- Python commands run through `uv run`.
- Track job id, command, config, seed, commit, checkpoint, log path.

## Slurm Defaults

- Partition:
- Account:
- GPU type/count:
- CPUs:
- Memory:
- Time:
- Log path:
- Checkpoint path:

## Commands

- Queue: `squeue -u "$USER"`
- Cluster: `sinfo`
- Submit:
- Inspect:
- Cancel:
