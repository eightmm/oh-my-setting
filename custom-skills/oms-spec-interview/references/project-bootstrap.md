# Project Bootstrap

Run only for a new project after `PROJECT.md` is confirmed.

1. Select the template from the confirmed project type: `ml` for training or
   research pipelines, `general` otherwise, and a Slurm overlay only when the
   project or runtime requires it. Use `auto` only for existing-repo onboarding.
2. Initialize git first if absent (`git init`), so the template step can keep
   the agent files out of the history from the first commit.
3. Run `oms apply-project-template <type> .`, then `oms init`.
4. Create structure only; no feature logic. For confirmed Python projects use
   `uv`, a local `.venv`, and a src layout.
5. Create only confirmed dependencies and paths. Never overwrite existing
   files.
6. Run `oms project-doctor .`, repair template/sync issues, and rerun.

The agent-facing files (`AGENTS.md`, `CLAUDE.md`, `PROJECT.md`) are hidden from
git locally, so a public repo carries no harness residue. `oms init` applies the
exclusion for a repo that got git after its template; `oms project-private
status` confirms it, and `--no-private` keeps the files committable. They are
local-only: a fresh clone or `git clean -x` drops them, so re-run
`oms apply-project-template` in that checkout.

Feature logic, API/data/schema changes, dependency additions, and compute
allocations remain separate implementation decisions even after bootstrap.

Report template type, created paths, skipped existing files, doctor result, and
the first concrete implementation step.
