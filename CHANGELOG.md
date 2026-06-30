# Changelog

All notable changes to this project are documented here. The format loosely
follows [Keep a Changelog](https://keepachangelog.com/); versions track the
`VERSION` file.

## [Unreleased]

### Added
- `agent-plan.sh`: shared subtask DAG (`.oms/plan/tasks.json`) with per-task
  dependencies, path scope, and verify command; `ready`/`status` compute what is
  actionable now so work can be split across agents without collisions.
  `next`/`brief` emit a paste-able work brief; `next --claim --provider` is a
  pull-work primitive (exit 3 when nothing is actionable). All mutations and
  `next --claim` run under a file lock so concurrent agents cannot both claim the
  same task; adds `review`/`release` commands, a stricter lifecycle (finish only
  from claimed/running/review; a blocked task must be reopened before claim), and
  a `claimed_at` timestamp.
- `multi-agent-delegate.sh --task-id` and artifact-index lineage: every index
  row now records `base_sha` and any `task_id`, surfaced in `artifact-index list`,
  so a run traces back to the plan subtask and commit it came from.
- `change-guard.sh`: `forbidden_paths` task constraint (deny beats allow),
  documented in `agent-task.sh` help.
- `data-manifest.sh`: `--key-column` entity-overlap leakage (inchikey/scaffold/
  cluster/assay) and per-key fingerprints with `(id -> key)` mapping drift and
  empty-key counts (manifest schema 3).
- `run-ledger.sh`: each row records its gate decision (passed/skipped/recorded/
  none); `list` surfaces it.
- `project-doctor.sh`: flags an empty `## Commands`/`## Verification` once
  `PROJECT.md` is past draft.
- `LICENSE` (MIT), `SECURITY.md`, `CONTRIBUTING.md`, this changelog.
- Tag-triggered `release` workflow: gates on `scripts/check.sh`, verifies the
  tag matches `VERSION`, and publishes a GitHub Release with `install.sh`,
  `install.sh.sha256`, and a `SHA256SUMS` manifest (`scripts/gen-checksums.sh`).
  See `docs/RELEASE.md`.

### Changed
- README "What's Inside" is now an eight-row capability table; the full
  per-script catalog moved to `docs/COMPONENTS.md` (no scripts removed). The
  task plan is grouped under "Agent state", not "Memory".
- Auto-update trigger defaults to **check-only** (records availability) instead
  of auto-applying; opt in with `OH_MY_SETTING_AUTO_UPDATE_MODE=apply`.

### Security
- `data-manifest.sh` leakage fails closed when a recorded split file, id column,
  or key column is missing; manifest names reject path traversal.
- `run-ledger.sh` blocks a sensitive-looking gate skip `--reason` and no longer
  echoes the raw reason to stderr.
- CI workflow runs with `contents: read` permissions.

## [0.3.0]

- Baseline: cross-agent harness (Codex/Claude Code/Antigravity) with project
  templates, multi-agent review/delegate, run ledger/capsule, data manifest,
  Slurm/HPC helpers, and shared memory.
