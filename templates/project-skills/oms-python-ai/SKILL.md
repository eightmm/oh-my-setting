---
name: oms-python-ai
description: Build uv src-layout Python AI and LLM packages with shallow docs/*.md, validated boundaries, and tensor safety.
---

# Python AI engineering

Keep `PROJECT.md` as the single concise root contract, command map, and
reference index. List detailed specifications under its `References` section;
place them flat, one level deep in `docs/*.md`, such as
`docs/ARCHITECTURE.md` and `docs/DECISIONS.md`. Do not create nested `docs/`
directories or root Markdown files beyond `README*`, `AGENTS.md`, `CLAUDE.md`,
`GEMINI.md`, and `PROJECT.md`.

Use a `uv` package with `pyproject.toml`, `uv.lock`,
`src/<package>/__init__.py`, `src/<package>/py.typed`, `tests/`, and
`scripts/check.sh`. Keep importable code under `src/<package>/` and helper
runners in `scripts/`; do not put stray `.py` modules at the root. Set up with
`uv sync` (`uv sync --locked` once `uv.lock` exists); run checks using the synced environment with
`uv run --no-sync`, including `compileall`, `ruff check`, a type checker when
configured, and `pytest`.

Validate structured schemas and untrusted tool and model inputs and outputs at
boundaries. Check tensor and batch shapes and dtypes early. Before paid API
calls or GPU jobs, run deterministic CPU, mock, or dry-run smoke checks through
`scripts/check.sh fast tests/test_smoke.py` (or explicit smoke test paths) in
under 60 seconds. Version prompt and evaluation
regressions with fixed fixtures, and bound timeouts, retries, and budgets. Keep
`.env`, credentials, checkpoints, and raw datasets out of git.
