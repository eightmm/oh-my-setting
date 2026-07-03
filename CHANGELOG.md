# Changelog

All notable changes to this project are documented here. The format loosely
follows [Keep a Changelog](https://keepachangelog.com/); versions track the
`VERSION` file.

## [Unreleased]

### Added
- Role profiles (`agent-role.sh`): named, reusable worker personas as markdown
  in `.oms/roles/<name>.md` (global fallback `~/.oh-my-setting/local/roles`);
  `list`/`show`/`resolve`/`init`. `multi-agent-delegate.sh --role NAME` prepends
  the profile to the worker brief, and an `agent-plan` task's new `role` field is
  auto-injected when delegated via `--plan-task` — so the same reviewer /
  refactorer / test-writer role can drive any of the three providers.

### Fixed
- `patch-admit.sh`: a worktree apply failure was swallowed (`|| true`), so the
  syntax/verify gates could pass against the UNPATCHED tree — now recorded as an
  `apply-worktree` FAIL and the gates are skipped. numstat parsing split on
  whitespace (paths with spaces escaped the syntax gate) — now tab-delimited.
  The verifier-integrity gate was bypassable with `cd`/absolute `--verify`
  spellings — now matches by path and basename and also protects common build
  entrypoints (Makefile, package.json, pyproject.toml, …).
- bash 3.2: `multi-agent-review.sh` used `declare -A` (verdicts/`--gate` died on
  macOS) — replaced with newline-delimited records; `change-guard.sh begin`
  aborted under `set -u` with no `--allow` (unguarded array expansion) — fixed.
- `experiment-board.sh`: a stale-claim reclaim kept the dead original owner
  (broke `--owner` and attribution) — a (re)claim now reassigns the owner while
  touch/start/finish keep it.
- `multi-agent-common.sh`: an agy isolated read worktree leaked on
  INT/TERM/HUP — its temp dir is now residue-marked (prefix `oh-my-setting-*`)
  so `cleanup.sh`/doctor reclaim a signal-leaked worktree.
- `multi-agent-delegate.sh`: `REPO` is normalized to the git worktree root, so
  `--plan-task` verify hydration no longer silently drops when run from a
  subdirectory; the hydrated-brief temp file no longer leaks on a hydration
  failure.
- `oms-run.sh ls --open`: applied the open filter before taking the last N,
  hiding older still-open runs — now filters first, then slices.
- CI (`test.yml`): added a static bash-4ism gate (`declare -A`/`mapfile`/case-
  conversion) and put `scripts/oms` under shellcheck and macOS `bash -n`, since
  `bash -n` alone let runtime-only bash-4 constructs slip past.

### Added (earlier)
- `repo-state.sh` (`oms state`): one read-only dashboard over all shared `.oms`
  state — active task goal/next, plan tasks by state with stale claims flagged,
  experiment board active/stale, current + open runs, latest artifact rows, and
  change-guard status; `--json` for machines. Answers "what is active, stale,
  or open here?" in one command instead of cat-ing five files.
- `patch-land.sh` (`oms patch-land`): the one mutating step that composes the
  trust boundary — clean-tree check → `patch-admit` ADMIT gate → `git apply` →
  land row in the artifact index → optional `--plan-task` finish. Nothing lands
  unless admission passes and the tree is clean.
- Claim heartbeat: `agent-plan.sh touch --id` and `experiment-board.sh touch
  --id` refresh a live claim's timestamp so a still-running worker is not
  reclaimed / flagged stale mid-run (the reclaim/stale TTL clock restarts).
- `scripts/oms` dispatcher, symlinked to `~/.local/bin/oms`: `oms <tool>`
  invokes any harness script by name from any of the three agent CLIs
  (`run` aliases `oms-run`); `oms list` prints every tool with its one-line
  purpose. Linked/unlinked/doctored with the install.
- `oms-run.sh new` writes a repo-scoped `.oms/runs/CURRENT` pointer and
  `oms-run.sh current` resolves the effective run id; `link` and the
  run-ledger/run-capsule/experiment-board auto-links fall back to a fresh
  CURRENT when `OMS_RUN_ID` is unset, so a second agent process joins the
  active run without env plumbing. Stale pointers expire
  (`OMS_RUN_CURRENT_TTL`, default 86400 s) instead of misjoining.
- Agent identity: `oms_detect_agent` (explicit `OMS_AGENT` > CLI env markers >
  generic "agent") now attributes memory notes, task bullets, board claims,
  capsules, and reconcile rows; spine link rows carry a new `agent` field
  (`link --agent` overrides; `show`/`timeline` display it).
- Delegate workers receive `OMS_STATE_REPO` — agent-memory/task/plan resolve
  to the primary repo's shared `.oms` instead of the empty throwaway
  worktree — and `OMS_AGENT=<provider>` for attribution.
- `agent-run.sh --task-id`/`--plan-task`, forwarded to the delegate for plan
  lineage and lifecycle coupling.
- AGENTS.md "Run Provenance & Coordination" is now a capability catalog with
  the `oms` invocation path; the agent-harness skill documents the plan DAG.

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

- `oms-run.sh close [id]` + `ls --open`: mark a run terminal and list
  open-vs-closed runs; close also clears a `CURRENT` pointer naming the run so
  later tool events stop auto-joining a finished run.
- `oms-run.sh timeline --agent NAME` / `--tool NAME`: filter the merged
  cross-stream timeline by who or which tool (case-insensitive substring).
- `experiment-board.sh list --stale` / `--owner NAME`: surface TTL-expired
  (reclaimable) claims and filter by claimer, instead of staleness only
  showing up at the next claim collision.
- `agent-memory.sh search PATTERN` (`--agent` author filter): recall over
  shared memory and pins by entry, replacing `show` cat-ing the whole file.
- `multi-agent-delegate.sh --plan-task ID` without `--prompt`/`--brief-file`
  hydrates the worker brief from the task, and without `--verify` uses the
  task's stored verify command — `delegate --to codex --plan-task t3` is now
  a complete one-liner.

### Fixed
- `patch-admit.sh` records each admission in the artifact index, so the report
  survives `artifact-index.sh prune --files` (which deletes unreferenced files
  under `.oms/artifacts/`); and it now fails closed when a patch modifies its
  own verifier (e.g. rewriting `scripts/check.sh` to self-certify), overridable
  with `--allow-verifier-change`.
- `change-guard.sh check` includes changes committed after `begin` (diffs the
  stored begin-HEAD against HEAD), so an agent that commits no longer escapes
  the allow/deny path scope; the stored begin-head is finally read.
- Run-cluster state (spine, default ledger, capsules, board, manifests,
  reconcile) anchors to the git worktree root instead of `$PWD`, so a
  subdirectory invocation no longer forks a second `.oms`; every run tool now
  also drops the `.oms/.gitignore` guard on first write.
- File locks live in a fixed `~/.cache/oh-my-setting/locks`: an interactive
  and a cron/ssh agent no longer compute different lock dirs (via
  `XDG_RUNTIME_DIR`) for the same state file, which defeated mutual exclusion.
- `doctor.sh` certifies symlink identity, not existence: a config or skill
  link resolving to a foreign/stale target fails as "linked elsewhere"
  (regular files where a link is expected also fail).
- bash 3.2: `declare -A` in the skill-doctor duplicate check aborted the whole
  check on macOS; replaced with a portable dedup.
- Provider namespace is canonical: `agy` normalizes to `antigravity` in
  `agent-run.sh` and `agent-plan.sh` claims; unknown provider names are
  rejected instead of polluting the board.
- Verify commands in `multi-agent-delegate.sh` and the review `--gate`
  backstop are bounded by `OMS_MULTI_AGENT_VERIFY_TIMEOUT` (default 10m); a
  hung test suite fails the run instead of wedging it forever.
- Antigravity read passes run in an isolated detached-HEAD worktree (or a
  scratch dir outside git): agy has no file-write-blocking flag, so stray
  writes are discarded instead of reaching the caller's tree.

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
