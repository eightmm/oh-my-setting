# Project Guidelines

- Prefer repository conventions over global defaults.
- Read `PROJECT.md`.
- Keep `PROJECT.md` as the root contract and reference index; put detailed
  specifications flat, one level deep, in `docs/*.md`.
- For `uv` packages, keep importable source in `src/<package>/`, tests in
  `tests/`, and locked dependencies in `uv.lock`; avoid stray root modules or
  Markdown files beyond `README*`, `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, and
  `PROJECT.md`.
- Inspect the implementation before editing; change only task-relevant lines.
- Ask only about out-of-scope dependency/contract changes or unresolved
  compatibility/cost risks. Never mask failures or re-ask unchanged authorization.
- Run affected checks; reuse tests and fixtures for uncovered behavior, bugs
  or safety boundaries. Broaden only for uncertainty or required gates.
- Keep routine CI small; no speculative jobs/matrices or duplicate suites.
  Separate expensive checks by affected risk, release or scheduled triggers.
- Keep generated artifacts, secrets, and private data out of git.

Keep commands, paths, interfaces, protected areas and success criteria in
`PROJECT.md`, including CI scope/runtime. Past `draft`, fill Commands and
Verification or explain `n/a`.
