# Project Coding Guidelines

- Keep changes focused on the requested task.
- Prefer local project conventions over global preferences.
- Run the narrowest useful verification command before finishing.
- For Python, use `uv` by default: `uv sync`, `uv add`, `uv run`, and local `.venv`.
- If Slurm is available, submit long CPU/GPU jobs with `sbatch` or allocated `srun`; avoid heavy work on login nodes.
