# Project Coding Guidelines

- Terse, explicit, low-token. Preserve meaning; remove fluff.
- Change only task-relevant lines. Prefer local conventions.
- Smallest correct solution. No speculative features or abstractions.
- New/vague work: interview -> spec -> confirm -> code.
- Evidence before edit; no silent deps/toolchain changes.
- No masking bugs with broad `try/except`, fallback `if`, or zero padding.
- Verify by interface/behavior first, not tiny per-function tests.
- AI/ML tests: data validation -> dataloader -> model architecture -> output contract.
- Python: use `uv sync/add/run`; local `.venv`.
- Slurm: long CPU/GPU jobs use `sbatch` or allocated `srun`; no heavy login-node work.
