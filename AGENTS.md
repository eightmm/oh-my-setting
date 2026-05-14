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

## Agentic Coding

- Read evidence before edit: files, call sites, tests, logs.
- Control blast radius. High-risk: API, DB, auth, config, deps, HPC jobs.
- No silent dependency/toolchain changes.
- Define interface contract before changing CLI/API/config/file formats.
- Verify by ladder: syntax -> focused interface test -> broader test if needed.
- Report failed checks with command, reason, next step.
- Destructive/irreversible work needs backup or explicit confirmation.
- Long jobs need Slurm script, logs, resources, checkpoint/resume plan.
- Leave handoff: changed, verified, not verified, next command.

## Test Strategy

- Prefer behavior/interface tests over tiny per-function tests.
- AI/ML priority: data validation -> dataloader batch -> model architecture -> output contract.
- Test shapes, dtypes, masks, devices, seeds, NaN/Inf, empty/small batches.
- For models, verify forward pass, loss contract, checkpoint load, inference output.
- Add narrow unit tests only for fragile pure logic or past bugs.

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
