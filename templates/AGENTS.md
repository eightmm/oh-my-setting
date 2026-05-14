# Project Coding Guidelines

- Terse, explicit, low-token. Preserve meaning; remove fluff.
- Change only task-relevant lines. Prefer local conventions.
- Smallest correct solution. No speculative features or abstractions.
- New/vague work: interview -> spec -> confirm -> code.
- Verify with the narrowest useful command.
- Python: use `uv sync/add/run`; local `.venv`.
- Slurm: long CPU/GPU jobs use `sbatch` or allocated `srun`; no heavy login-node work.
