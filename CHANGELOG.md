# Changelog

All notable changes to this project are documented here. The format loosely
follows [Keep a Changelog](https://keepachangelog.com/); versions track the
`VERSION` file.

## [Unreleased]

### Added
- Snapshot lifecycle contracts: machine and Slurm generators now support
  `--dry-run`/`--check`, write private atomic schema-marked files, and report
  broader local hardware context. Schema-2 receipts preserve `0`/`1`/`auto`
  snapshot policy, and transactional updates refresh applicable snapshots.
- `project-doctor --strict` for CI/release gates, including filled Slurm
  partition/account, resource, log, and checkpoint requirements past draft.
- Explicit `tsp-queue enqueue --allow-noqueue` opt-in for degraded background
  execution when task-spooler is unavailable.
- Slurm cluster references now capture all partition and node records, the
  current user's `sacctmgr` associations/accounts, QOS limits, and effective
  configured CPU, memory, and time defaults without inventing missing values.
- Automatic reasoning-effort routing alongside worker model classes:
  `fast` uses low, `balanced` medium, and `deep` high effort. Codex receives a
  `model_reasoning_effort` override, Claude receives `--effort`, and
  Antigravity uses Low/Medium/High model variants because its CLI has no
  independent effort flag. Capacity fallback lowers automatic effort with the
  model tier; executor and artifact metadata freeze and record the route.

### Fixed
- Receipt-owned branch and detached auto-updates now share `update.sh`'s
  rollback transaction instead of maintaining a second half-linked path.
- `update.sh --tools` now requests real provider CLI and uv upgrades instead of
  silently accepting already-installed binaries.
- `peer-ask --repo` now keeps its default artifacts under the selected state
  repository instead of leaking them into the caller's working directory.
- `fail-ledger` now accepts the documented `--repo` option and honors
  `OMS_STATE_REPO`, matching the other shared harness state commands.
- Removed the unused read-executor surface: executors are now write-worktree
  contracts only. `--mode worktree-write` remains an unadvertised compatibility
  no-op for existing callers; legacy `mode: read` metadata stays inspectable
  and retireable but cannot validate, start, or delegate.
- Mixed read/write requests such as `review and fix` now route to an isolated
  write worker instead of stopping at a read-only pass.
- Peer quorum lists reject duplicate providers after canonicalizing the `agy`
  alias, preventing one CLI from counting as multiple independent reviewers.
- Capacity fallback now treats ignored files as worktree mutations, recreates
  Antigravity read isolation before retrying, and removes ignored verification
  byproducts before repair.
- Dry-run and export-only passes validate their provider route and record the
  selected model; unknown Antigravity variants no longer claim an inferred
  reasoning effort that the CLI did not expose.
- Plan-bound executors can run through `plan-run` against their exact claimed
  provider and lease. Creation rejects invalid plan claims, signal cleanup
  preserves review evidence, and known failures key on resolved contracts.
- Legacy executor metadata without reasoning fields now honors an explicit
  caller effort instead of silently replacing it with automatic effort.

## [0.4.0] - Unreleased

### Added
- Provider-neutral worker model routing for `agent-call`, `agent-run`,
  `peer-ask`, `peer-review`, `peer-delegate`, `plan-run`, and `advise`.
  `fast`/`balanced`/`deep` classes map to each installed CLI, roles and
  operations select a class automatically, and capacity errors permit at most
  one lower-class retry. Exact models, mappings, and fallback policy remain
  overridable; write fallback is blocked after any worktree mutation. Artifact
  rows and delegation liveness retain the resolved route, while executor souls
  freeze their model contract with provider/scope/verification metadata.
- Bounded autonomous plan execution (`oms plan-run`): atomically claims one
  scoped task, delegates it in an isolated worktree with bounded repair, and
  stops in review unless `--land` explicitly sends it through `patch-land`.
  Machine-readable plan claims, known-failure gating, signal-safe lease release,
  and focused autonomy regressions keep the controller composable and fail-closed.
- Active task verification now executes and records the mechanical command;
  skipped or failed checks remain non-green. Plan tasks can reach `done` only
  through reviewed artifact/patch evidence and the fenced landing transition.
- Failure memory includes a content-free git-state fingerprint, allowing a
  justified retry after tracked changes while blocking an unchanged dead end.
- Prompt routing uses ASCII token boundaries for short English terms and rotates
  auto-recorded task packets when a genuinely different explicit goal arrives.
- Stable/edge installation channels: `--ref` and `OH_MY_SETTING_REF` can select
  `edge`, a tag, branch, or commit. Release assets embed their exact release tag
  and install to a detached commit, while the source installer explicitly
  tracks the origin default branch in edge mode.
- A strict release contract smoke test verifies version parity, the generated
  pinned installer, release workflow wiring, and positive/negative checksum
  validation. `gen-checksums.sh --verify` works with GNU `sha256sum` and BSD
  `shasum`.
- Transactional updates: schema-2 receipts persist the install ref, profile,
  concrete components, managed targets, and previous successful commit.
  `update --check` is read-only, link/doctor failures restore HEAD, links, and
  receipt, and `update --rollback` returns to the prior success.
- Task-scoped executor souls (`agent-executor.sh`): model-proposed behavior is
  validated and hash-frozen while machine-owned metadata retains provider,
  task lease, base commit, path scope, and verification authority. Native and
  cross-CLI executors share the same brief; repair preserves the frozen soul.
- Patch admission now enforces plan/executor allowed and forbidden paths before
  verification, with deny precedence; artifact and liveness rows carry
  executor ID and soul hash provenance.
- `advise.sh` (`oms advise`): agent-agnostic advisor pass at decision points
  (before irreversible/high-risk decisions, after repeated failures, or at a
  release go/no-go). Composes an adversarial VERDICT/RISKS/MISSING/NEXT prompt, attaches
  unresolved fail-ledger rows, defaults to the first available provider that
  is not the caller (`OMS_ADVISOR_PROVIDER`/`--to` to pin), and routes through
  `agent-call.sh` (read-only, scrubbed). Gives Codex and Antigravity the same
  decision-point advisor Claude Code has natively.

- Skill router (`skill-router.sh` + `install-claude-hooks.sh`): a Claude Code
  UserPromptSubmit hook that matches each prompt against new per-skill
  `triggers` phrases (en+ko) in `skills.manifest.json` and injects a one-line
  skill hint, so skills fire at task time instead of relying on recall.
  Precision-first: max 2 suggestions per prompt, each skill hinted once per
  session, silent on no match, system prompts (slash commands, notifications)
  skipped, fail-open, `OMS_SKILL_ROUTER_OFF=1` kill switch. install/update
  register it via an additive `~/.claude/settings.json` merge (backup +
  idempotent + refuses invalid JSON; `OH_MY_SETTING_CLAUDE_HOOKS=0` opts out)
  and uninstall removes only its own entry. Claude-only by nature; Codex and
  Antigravity keep the skill picker plus a new AGENTS.md skill-consult rule.
- Terminal-verb wiring — every create now has a crash-path close: `gc` appends
  a close event to stale open runs (no spine event in `--days`; open runs no
  longer protect their capsules from GC forever), releases the claimed/running
  plan task coupled to a dead delegation marker (the two records describing
  the same dead worker are finally joined), and sweeps abandoned change-guards.
- `patch-land.sh` feeds the shared failure memory: a rejection is recorded in
  the fail-ledger fingerprinted by patch content, a retry of a known-rejected
  patch warns first, and a later successful land resolves the entry. The land
  row append is no longer silently swallowed, and `--plan-task` alone reads
  the patch path the plan task already stores.
- `agent-plan reclaim --include-review`: opts an abandoned review back to
  ready on its own clock (`updated`, default TTL 86400s), keeping
  artifact/patch; `oms state` flags stale reviews (`OMS_PLAN_REVIEW_TTL`).
- `change-guard.sh` liveness: snapshots stamp a start time (optional
  `OMS_GUARD_PID`), `status` and `oms state` flag an abandoned guard STALE
  (`OMS_GUARD_TTL`), and the snapshot write is atomic.
- `repo-state.sh --refresh-ci`: opt-in `ci-status record` before reading, so
  the CI section reflects the latest run in one command.
- CI `install-e2e` job: the real `install.sh` → `update.sh` → `uninstall.sh`
  lifecycle against a throwaway HOME — the installer path was previously only
  linted, never executed.

### Changed
- Removed the redundant `git-cli-workflow` custom skill; global policy already
  owns its complete local git/gh safety contract, and relinking removes stale
  owned skill links from all three providers.
- Converted `agent-harness` and `ml-training` into compact task routers with
  one-level references. Corrected ML guidance for semantic optimizer grouping,
  short schedules, unequal-count DDP gradients, portable DDP checkpoints, and
  opt-in static graphs.
- Minimal install is now the default: provider tools, `.bashrc` mutation,
  machine snapshots, auto-update timers, and the star prompt require explicit
  flags or `--full`; Codex plugin setup auto-detects an existing CLI. Standalone
  doctor runs treat provider CLIs and an uninstalled Codex plugin as optional
  unless strict checks are explicitly requested.
- New ML scaffolds create five core docs by default; `--full-docs` retains the
  complete 13-document scaffold and existing project files are never removed.
- Routine `status.sh` reports local paths without launching provider CLIs;
  `--verbose` opts into version and Codex plugin probes.
- Expanded shell validation to shared libraries, plugin hooks, and generated
  project checks; doctor now reports supported Bash 3.2 as healthy.
- Reduced routine harness overhead: prompt hooks no longer create `.oms` or
  active task packets for read-only questions, automatic task recording now
  requires `OMS_AUTO_TASK=1`, `update.sh` refreshes provider tools only with
  `--tools`, and the `oms list` public allowlist now also enforces dispatch so
  hidden install and hook scripts cannot be invoked by guessed filenames.
- Narrowed multi-agent review activation to explicit cross-agent review,
  release gates, and requested ML pre-training gates. Generated project loaders
  now allow clear bounded changes without waiting on unrelated draft choices.
- Reduced global `rules/global-AGENTS.md` from a harness manual to a compact policy layer:
  prompt-level provider/model ladders and routine advisor calls are gone;
  bounded model selection now lives in executable harness policy, ambiguous
  work alone triggers the spec gate, and detailed
  coordination routes through the `agent-harness` skill while parent judgment
  and executor scope fences stay.
- Split install-wide rules from the repository `AGENTS.md` overlay so working on
  oh-my-setting no longer injects the same global policy twice.
- Removed unused legacy prompt/template placeholders, consolidated duplicate
  plugin hook wrappers, and removed standalone workflow files in favor of
  their maintained skills.
- Renamed the cross-CLI tool family `multi-agent-*` to `peer-*` to stop
  colliding with generic in-app multi-agent features: `peer-ask.sh`,
  `peer-review.sh`, `peer-delegate.sh`, `lib/peer-common.sh`, skills
  `peer-ask`/`peer-review`/`peer-delegate`, and env vars `OMS_PEER_*`.
  Version 0.4 removes the deprecated `multi-agent-*.sh` shims; legacy
  `OMS_MULTI_AGENT_*` variables now fail explicitly with their `OMS_PEER_*`
  replacements instead of silently changing timeout behavior.

- MD/trigger layer strengthened so skills actually fire at task time: skill
  frontmatter descriptions now carry concrete "use when" phrases and Korean
  user wordings (agent-harness enumerates its whole surface — state/resume,
  fail-ledger, gc, patch-land, plan DAG, session handoff; git-cli, slurm-hpc,
  ops, research-method, delegate follow). multi-agent-delegate's body
  documents `--plan-task`/`--role`/`--repair`/`--no-verify` and routes the
  post-worker path through patch-admit/patch-land. AGENTS.md reframes `oms
  gc` as the crash-path recovery step and adds the new lifecycle levers
  (`oms state --refresh-ci`, `reclaim --include-review`, patch-land ↔
  fail-ledger, change-guard). Templates call tools via the `oms` dispatcher;
  README.ko.md mirrors the EN condensed structure; timeout env knobs
  documented in COMPONENTS.

### Removed
- Deprecated `workflows/{spec-first,slurm-hpc,new-server}.md`, their global
  workflow link, and `multi-agent-{ask,review,delegate}.sh`. Upgrade cleanup
  restores the newest user workflow backup and preserves foreign targets.

### Fixed
- Aligned advisor and spec gates across global rules, skills, templates, docs,
  and prompts: routine completion and clear bounded changes no longer trigger
  mandatory advisor/interview workflows.
- Hardened install ownership and parity: foreign symlinks round-trip through
  backup/restore, foreign doctor calls delegate to the canonical implementation,
  plugin identity uses the full marketplace ID, stale/missing expected plugins
  fail health checks, and auto-update refreshes hooks/plugins before reporting
  success. Repeated relinks preserve restoration backups, scheduled updates keep
  hook/plugin opt-outs, and unlink accepts pre-split global-rule links.
- Signal cleanup now terminates the full provider subprocess tree, preventing a
  cancelled read-only call from retaining output pipes until its timeout.
- GC now counts an empty compacted failure ledger as zero instead of producing
  a duplicate `0` value and an integer-comparison warning.
- `link.sh` now removes dangling skill links owned by the checkout (left
  behind when a skill is renamed or removed), so renames like
  `multi-agent-*` -> `peer-*` do not strand old links in agent skill dirs.
- Crash-atomicity where the harness diverged from its own tmp+mv standard:
  the `.oms/runs/CURRENT` pointer (read locklessly by every auto-linking
  tool), the change-guard snapshot, and `artifact-index prune` (now an atomic
  replace at the symlink target instead of truncate-in-place).
- Provider/verify timeouts escalate to SIGKILL via `--kill-after` (a worker
  that traps SIGTERM no longer survives the bound; probed for busybox), and
  `OMS_REQUIRE_TIMEOUT=1` refuses to run unbounded when no timeout binary
  exists instead of only warning.
- Run-id entropy no longer degrades to a bare pid when `/dev/urandom` is
  unreadable (pid+time+`$RANDOM` mix), and a failed urandom read no longer
  yields an empty suffix.
- Lock fallback under real contention and delegate SIGKILL recovery are now
  covered by tests (`OMS_LOCK_FORCE_MKDIR` two-writer race; `kill -9` mid-
  delegate → `gc` sweeps the orphan marker and releases the plan task).

### Added (earlier)
- Failure memory (`fail-ledger.sh`): durable `.oms/failures.jsonl` fingerprint
  ledger so the three agents stop repeating the same failing command across
  sessions — `record`/`check` (exit 3 on a known-unresolved failure)/`resolve`/
  `list`; sensitive commands refused. Surfaced in `oms state`.
- Delegation liveness: `multi-agent-delegate` writes `.oms/delegations/<id>.json`
  while a worker is in flight and removes it on exit; `oms state` shows live
  workers and flags dead-pid orphans (no daemon — the launcher is the writer).
- `ci-status.sh record`: appends the latest CI conclusion to `.oms/ci.jsonl`
  (deduped by sha); `oms state` shows the latest conclusion for HEAD's branch.
- `oms init` (`oms-init.sh`): seeds the `.oms/` skeleton + `.gitignore`
  (idempotent) and prints a next-actions checklist tailored to the detected
  project type — a first move for an agent landing in a fresh repo.
- `oms gc` (`gc.sh`): `--dry-run` by default; reclaims aged transient state
  (orphaned delegation markers, archived task packets, capsules of non-open
  runs, resolved failure rows) and delegates artifacts to `artifact-index
  prune`; never touches open runs, the active task, unresolved failures, or the
  append-only board.
- `oms-run validate` now walks every `.oms/**/*.jsonl` family and flags schema
  drift (rows below a family's expected schema) — the one place a future schema
  bump is signalled — in addition to the parse check.

### Added (earlier since 0.3.0)
- Role profiles (`agent-role.sh`): named, reusable worker personas as markdown
  in `.oms/roles/<name>.md` (global fallback `~/.oh-my-setting/local/roles`);
  `list`/`show`/`resolve`/`init`. `multi-agent-delegate.sh --role NAME` prepends
  the profile to the worker brief, and an `agent-plan` task's new `role` field is
  auto-injected when delegated via `--plan-task` — so the same reviewer /
  refactorer / test-writer role can drive any of the three providers.

### Fixed (earlier)
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
