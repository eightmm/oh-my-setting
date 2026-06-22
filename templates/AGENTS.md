# Project Guidelines

- Terse, explicit, low-token. Preserve meaning; remove fluff.
- Change only task-relevant lines. Prefer local conventions.
- Smallest correct solution. No speculative features or abstractions.
- New/vague work: interview -> spec -> confirm -> code.
- Evidence before edit; no silent deps/toolchain changes.
- Implementation checks require reading the relevant source files/scripts, not
  only docs, status output, memory, or prior summaries.
- No masking bugs with broad `try/except`, fallback `if`, or zero padding.
- Verify by interface/behavior first, not tiny per-function tests.
- Python, if used: prefer `uv sync/add/run`; local `.venv`.

## Output And Artifacts

- Compact, direct, low-token. Fragments and arrows OK when clear.
- Commit messages: Conventional Commits; subject <= 50 chars; body only for non-obvious why/risk.
- Markdown/docs: short sections, bullets, direct commands; keep setup, safety, and specs explicit.

## Fill Per Project

- Setup:
- Test:
- Lint/typecheck:
- Run:
- Data/config paths:
- Output/log paths:
- Do not touch:
