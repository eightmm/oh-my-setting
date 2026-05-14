# Slurm HPC Workflow

Use this workflow when Slurm commands are available on the machine.

1. Inspect cluster state with `sinfo` and current jobs with `squeue -u "$USER"`.
2. Avoid heavy work on login nodes. Use `srun` for interactive allocation or `sbatch` for batch jobs.
3. Put reproducible commands in a job script instead of relying on an interactive shell.
4. Use `uv run` inside job scripts for Python commands.
5. Set explicit job name, partition/account when required, wall time, CPU, memory, GPU, working directory, and output paths.
6. Use `sacct -j <job_id>` or `squeue -j <job_id>` to inspect jobs, and `scancel <job_id>` to stop them.

Minimal Python job:

```bash
#!/usr/bin/env bash
#SBATCH --job-name=python-job
#SBATCH --output=logs/%x-%j.out
#SBATCH --error=logs/%x-%j.err
#SBATCH --time=01:00:00
#SBATCH --cpus-per-task=4
#SBATCH --mem=16G

set -euo pipefail

cd "$SLURM_SUBMIT_DIR"
uv run python scripts/train.py
```

GPU job template:

```bash
#!/usr/bin/env bash
#SBATCH --job-name=gpu-job
#SBATCH --output=logs/%x-%j.out
#SBATCH --error=logs/%x-%j.err
#SBATCH --time=04:00:00
#SBATCH --cpus-per-task=8
#SBATCH --mem=64G
#SBATCH --gres=gpu:1

set -euo pipefail

cd "$SLURM_SUBMIT_DIR"
uv run python scripts/train.py --config configs/train.yaml
```
