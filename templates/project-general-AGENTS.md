# Project Guidelines

- Prefer local conventions over global defaults.
- Read `PROJECT.md` first. If missing/draft/incomplete, interview and update it before coding.
- Change only task-relevant lines.
- New/vague work: interview -> spec -> confirm -> code.
- Evidence before edit; no silent deps/toolchain changes.
- No broad `try/except`, fallback `if`, silent `return None`, or masking failures.
- Tests: behavior/interface first; narrow unit tests only for fragile pure logic.
- Python, if used: prefer `uv sync/add/run`; local `.venv`.

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
