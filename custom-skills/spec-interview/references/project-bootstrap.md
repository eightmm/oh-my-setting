# Project Bootstrap

Run only for a new project after `PROJECT.md` is confirmed.

1. Select the template from the confirmed project type: `ml` for training or
   research pipelines, `general` otherwise, and a Slurm overlay only when the
   project or runtime requires it. Use `auto` only for existing-repo onboarding.
2. Run `oms apply-project-template <type> .`.
3. Create structure only; no feature logic. Initialize git if absent. For
   confirmed Python projects use `uv`, a local `.venv`, and a src layout.
4. Create only confirmed dependencies and paths. Never overwrite existing
   files.
5. Run `oms project-doctor .`, repair template/sync issues, and rerun.

Feature logic, API/data/schema changes, dependency additions, and compute
allocations remain separate implementation decisions even after bootstrap.

Report template type, created paths, skipped existing files, doctor result, and
the first concrete implementation step.
