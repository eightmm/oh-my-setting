# Project Guidelines

- Prefer local conventions over global defaults.
- Read `PROJECT.md` first. If missing/draft/incomplete, interview and update it before coding.
- Change only task-relevant lines.
- New/vague work: interview -> spec -> confirm -> code.
- Evidence before edit; no silent deps/toolchain changes.
- No broad `try/except`, fallback `if`, silent `return None`, or masking failures.
- Tests: behavior/interface first; narrow unit tests only for fragile pure logic.
- Python, if used: prefer `uv sync/add/run`; local `.venv`.

## Output And Artifacts

- Compact, direct, low-token. Fragments and arrows OK when clear.
- Commit messages: Conventional Commits; subject <= 50 chars; body only for non-obvious why/risk.
- Markdown/docs: short sections, bullets, direct commands; keep setup, safety, and specs explicit.

## Project Commands

- Setup:
- Test:
- Lint/typecheck:
- Run:
- Build:

## Project Contracts

- Public CLI/API/config:
- Data/files:
- Outputs/logs:
- Success criteria:
- Do not touch:
