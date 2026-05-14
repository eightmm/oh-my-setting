# Global Coding Guidelines

These rules apply by default when writing, reviewing, or refactoring code.

- Think before coding. State assumptions explicitly, surface ambiguity, and ask when the next step would otherwise depend on guessing.
- Prefer the simplest implementation that solves the requested problem. Do not add speculative features, configurability, or abstractions.
- Make surgical changes. Touch only files and lines that directly support the task, match the existing style, and avoid unrelated cleanup.
- Preserve user or existing changes. Do not revert or rewrite unrelated work; work with it when it affects the task.
- Define verifiable success criteria for non-trivial changes. Prefer a short plan that pairs each step with a check.
- Verify the work. Run the narrowest useful tests or commands, and report any checks that could not be run.
- Keep explanations direct. Mention tradeoffs, risks, and remaining uncertainty instead of hiding them behind confident language.

## Python Development

- Use `uv` by default for Python work.
- Prefer `uv init`, `uv add`, `uv remove`, `uv sync`, `uv lock`, and `uv run` over direct `pip`, `python -m venv`, Poetry, or Conda commands.
- Keep project environments local as `.venv` unless the project already has a different convention.
- Run Python scripts and tools through `uv run` so dependencies and Python versions stay reproducible.
- Use `uv pip ...` only when working inside a legacy project that does not yet use `pyproject.toml`.
- Do not introduce Conda, Poetry, or raw `requirements.txt` workflows unless the repository already depends on them or the user explicitly asks.

## Slurm / HPC

- If Slurm commands are available (`sbatch`, `srun`, `squeue`, `sinfo`, `scancel`), assume HPC-safe execution by default.
- Do not run long training, GPU, or high-CPU jobs directly on login nodes. Prepare an `sbatch` script or use `srun` for interactive allocations.
- Before proposing resource-heavy commands, check or ask for partition, account, wall time, GPU type/count, CPU count, memory, and output path requirements.
- Prefer reproducible job scripts that activate the project through `uv run` rather than relying on an already-activated shell.
- Include useful Slurm logging defaults: job name, `%x-%j` output paths, `set -euo pipefail`, and explicit working directory.
- Use `squeue -u "$USER"`, `sinfo`, `sacct`, and `scancel` for job inspection and control when relevant.
