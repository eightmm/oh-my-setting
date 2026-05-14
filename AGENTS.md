# Global Coding Rules

Default: terse, explicit, low-token. Preserve meaning; remove fluff.

## Execution

- Think first. State assumptions, ambiguity, and tradeoffs before risky work.
- Ask before guessing when the next step depends on unknown intent or context.
- Build the smallest correct solution. No speculative features, config, or abstractions.
- Change only task-relevant lines. Match local style. No unrelated cleanup.
- Preserve user/existing changes. Never revert unrelated work.
- Every changed line must trace to the request.
- Define success criteria for non-trivial work. Plan as step -> check.
- Verify with the narrowest useful command. Report checks not run.
- Explain directly: decision, risk, uncertainty, next step.

## Spec Gate

- For new projects/features or vague requests: interview first, spec second, code third.
- If ambiguity affects architecture/data model/API/UX/safety, stop and ask.
- Do not code until goal, constraints, success criteria, and verification are clear.
- Use `custom-skills/spec-interview` when asked to start/design/build from unclear intent.

## Python Development

- Use `uv` by default.
- Prefer `uv init/add/remove/sync/lock/run`; keep env in `.venv`.
- Run scripts/tools with `uv run`.
- Use `uv pip ...` only for legacy non-`pyproject.toml` repos.
- Do not introduce Conda, Poetry, or raw `requirements.txt` unless already used or requested.

## Slurm / HPC

- If `sbatch/srun/squeue/sinfo/scancel` exist, assume Slurm workflow.
- No long/GPU/high-CPU jobs on login nodes. Use `sbatch` or allocated `srun`.
- For heavy jobs, confirm partition/account/time/GPU/CPU/mem/output path.
- Prefer reproducible job scripts using `uv run`.
- Job script defaults: `set -euo pipefail`, `cd "$SLURM_SUBMIT_DIR"`, `%x-%j` logs.
- Inspect/control with `squeue -u "$USER"`, `sinfo`, `sacct`, `scancel`.
