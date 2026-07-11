# Project Guidelines

- Prefer local conventions over global defaults.
- Read `PROJECT.md` when it exists. Stop for spec work only when unresolved choices affect the requested change.
- Change only task-relevant lines.
- New projects and broad/vague work: interview -> spec -> confirm -> code. Clear bounded changes may proceed from local evidence.
- Evidence before edit; no silent deps/toolchain changes.
- Implementation checks require reading the relevant source files/scripts, not
  only docs, status output, memory, or prior summaries.
- No broad `try/except`, fallback `if`, silent `return None`, or masking failures.
- Tests: behavior/interface first; narrow unit tests only for fragile pure logic.
- Python, if used: prefer `uv sync/add/run`; local `.venv`.

## Output And Artifacts

- Compact, direct, low-token. Fragments and arrows OK when clear.
- Commit messages: Conventional Commits; subject <= 50 chars; body only for non-obvious why/risk.
- Markdown/docs: short sections, bullets, direct commands; keep setup, safety, and specs explicit.

## Project Commands And Contracts

The per-project contract lives in `PROJECT.md`, not here:

- Commands (Setup/Test/Run/Lint) -> `PROJECT.md` `## Commands`
- Success criteria + required checks -> `PROJECT.md` `## Verification`
- Public CLI/API/config, data/files, outputs/logs, do-not-touch -> `PROJECT.md`
  `## Paths` and `## Notes`

Once `PROJECT.md` is past `draft`, leaving Commands or Success criteria empty
is flagged by `project-doctor.sh`. Fill them or mark `n/a` with a reason.
