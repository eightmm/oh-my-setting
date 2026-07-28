# Changelog

All notable changes to this project are documented here. The format loosely
follows [Keep a Changelog](https://keepachangelog.com/); versions track the
`VERSION` file.

## [Unreleased]

### Added
- The failure ledger now sees the gate the primary agent actually runs.
  `agent-task verify` files a row when the stored verification command fails —
  carrying the failing line, written by the shell with no model involvement —
  and clears that row when the same command later passes, so the ledger answers
  "is this still broken" rather than "did this ever break". Recording only ever
  covered harness-mediated paths (`plan-run`, `patch-land`, `peer-delegate`),
  which is why one repo held two rows across 136 commits while the same gate
  failed the same way more than once in a single session. The readers were
  already wired: `patch-land.sh:305` and `plan-run.sh:220` ask `check --cmd`
  before acting, so a row filed here is consumed by machinery already running.
  `resolve` accepts `--cmd` as well as `--fingerprint`, because a caller that
  just watched a command pass holds the command, not the hash it was filed
  under.
- Settled by a three-model council, which disagreed usefully: hooking the
  project's `check.sh` was rejected because the project this serves has none
  (its gate is `uv run pytest`), and a `resolve --summary` field was dropped
  because `record --summary` already carries the diagnosis. Session-start
  memory injection is deferred until the write side is non-empty.
- Project memory now has a local SQLite query index at
  `.oms/memory/memory.sqlite3`. The append-only `shared.md` and `pins.md` logs
  remain the human-readable source of truth, so the change is reversible and
  existing projects migrate automatically on `oms init`, append, pin, search,
  or recall. Exact `search` keeps its case-insensitive substring contract;
  `recall` adds ranked local FTS without a model call or embedding download.
  Schema 2 links each new project note to a stable event ID, kind, active
  task/session hashes, current Git HEAD, dirty bit, and bounded state
  fingerprint. The append-only Markdown source carries that provenance through
  rebuilds, compact provider context omits it, and `search`/`recall --json`
  exposes it only on demand. Existing schema-1 databases and metadata-free
  notes migrate automatically with stable legacy IDs.
  Concurrent indexers use a bounded SQLite write transaction, `rebuild`
  recreates a damaged derived database from Markdown, and `doctor` checks its
  schema and integrity.
- Routing follows what each provider CLI can actually do, instead of asserting
  it. Two assertions in the table had gone stale while the CLIs moved, and a
  stale capability is not a harmless comment: antigravity was refused a
  `--effort` control it has had for a while ("antigravity has no effort flag"),
  and claude's `xhigh` and `max` were unreachable because the shared validator
  stopped at `high`. A capability snapshot now records each CLI's thinking
  mechanism, the scale it accepts, and its live model catalog; `model-doctor`
  writes it as a by-product of the probes it already runs, and routing only
  reads it — a route decision must not spawn a provider call. Where a catalog
  exists, a tier picks the first of its candidates that the catalog actually
  has, so a retired model slides to the next one instead of failing the call; a
  model named outright by an operator is still used as given.
- Codex has a local catalog after all: its app-server answers `model/list` over
  JSON-RPC without starting a conversation, so enumerating models costs a local
  process and no model call. It also reports each model's own reasoning-effort
  scale, which turns out to differ per model — `gpt-5.6-sol` accepts `ultra`,
  `gpt-5.6-luna` stops at `max`, `gpt-5.5` at `xhigh` — so effort is now
  validated against the model that was actually chosen, and the provider-level
  check uses the union of its models' scales. Both `model-doctor` and routing
  read catalogs through one function; `codex models` (which is not a
  subcommand) is gone. Claude Code has no catalog source and is reported as
  unverified rather than assumed good. Catalogs are probed once per doctor run
  and read back from the snapshot, rather than asked for twice: `agy models`
  started while a previous agy invocation is still exiting hangs until its
  timeout, so the second ask was intermittently no answer at all.
- Delegation depth is enforced instead of merely written down. The project
  rules said a worker does not recursively delegate; nothing checked it, and a
  worker that tried simply succeeded. Depth is now counted and capped
  (`OMS_DELEGATE_MAX_DEPTH`, default 1), because nesting multiplies: a worker
  that delegates can spawn workers that delegate, and cost, worktrees, and the
  number of things writing to one repository grow with the power, not the sum.
  The refusal names the alternative — do the work, or report what needs
  splitting so the caller fans out — and points at `oms consult`, which is
  read-only, widens nothing, and stays available at any depth.
- A model the account is not entitled to now falls back within its tier, the
  same way an unrecognised name does — both are availability problems, and
  dropping a tier is the answer to neither.
- A model whose own safeguard fires on a message is retried once on a different
  model. Verified live: Fable 5 refused a protein-ligand binding-affinity
  question about a real repository with *"our intentionally broad safeguards
  ... can sometimes flag legitimate coding, cybersecurity, and biology tasks"*
  and *"change your model"*. That is a property of the model, not a judgement
  on the request, and the remedy is the one the error names — so the deep tier
  now carries `opus` and then `sonnet` behind `fable`, and each retry uses a
  genuinely different model rather than the same one under an alias — the same
  broad filter can flag the second model as readily as the first, so the tier
  is walked rather than tried once. Bounded by `OMS_MODEL_SAFEGUARD_RETRIES`
  (default 2), and when every model in the tier flags the message the run says
  so instead of cycling. The artifact records each step as
  `model-fallback: reason=model-safeguard selected=<model>`. A considered
  refusal is a different message and is still never retried.
- A request the provider declines is reported as a decline rather than a
  generic failure: the artifact and the caller both say which provider and
  model declined it. It is deliberately not re-sent to another model, and a
  delegate spends no repair round on it. A decline is the provider's decision
  about the request; machinery that keeps asking a different model until one
  answers would be a way around that decision no matter what any individual
  request happens to be.
- Effort is chosen from the task and then fitted to the model, instead of a
  fixed low/medium/high map. Work that gates something irreversible — an
  advisor pass, a release, a review gate — takes one step more thinking,
  because that answer is acted on rather than retried; everything else keeps
  its tier's ordinary effort, since spending the top of a scale on routine work
  is how a budget disappears without anything improving. The result is then
  clamped to the scale the chosen model actually publishes, so the same release
  gate runs at `xhigh` on codex and claude and comes back to `high` on
  antigravity, whose scale stops there. `OMS_REASONING_NO_HEADROOM=1` turns the
  escalation off.
- A claude model name that stops resolving is recovered from instead of failing
  the tier. Claude Code publishes no catalog, so a pinned name cannot be checked
  before use — but it rejects an unknown one locally in about two seconds. Each
  claude tier now carries the pinned name first, because that is what makes an
  artifact say which model actually ran, and the CLI's own public alias
  (`fable`, `sonnet`, `haiku`) as the same-tier alternative. Dropping a tier is
  the answer to a busy model, not to a name the provider does not have.
- `provider-permissions` turns the last human-only setup step into a harness
  tool. Codex and Claude Code take authority per invocation and need nothing;
  antigravity needs standing rules, and finding out which ones took a session
  of probing. It now reports them (`--check`, which is what `doctor` runs),
  emits them (`--print`), or grants them (`--apply`, keeping the previous
  settings as `.bak`), in a `consult` or `delegate` profile. `unsandboxed` is
  granted one named command at a time and a wildcard is refused: that namespace
  *is* the sandbox boundary. A cold `uv run` needs `unsandboxed(uv)` only
  because uv's cache lives outside the worktree — a warm one, and every test
  the worker runs inside its worktree, needs nothing.
  `--allow-toolchains` does the same for the package managers in a short
  built-in list that are actually installed on the machine, so a second machine
  reaches the same state without rediscovering it one denial at a time. `pip`
  and `conda` (this setup uses uv), `docker` (its socket is root-equivalent),
  and `make`, `node`, `git` (they write in-tree) stay out of that list even when
  present.
- The README and `oms help` now say plainly what was only implied: install is
  the one command a person types, and everything after it is the agent's.

### Added
- The advisor now fires on its own after repeated failure. The rules told every
  agent to consult one "after repeated failures" and nothing ever did — this
  session is the evidence: an agent with those rules loaded pushed a dozen
  gates, hit the same class of mistake twice, and never once reached for
  `oms advise` unprompted. A line in a rules file competes with everything else
  in the context; a code path does not. `peer-delegate` now consults an advisor
  when a repair round has itself failed, injects the verdict into the next
  attempt as evidence rather than instruction, and records it in the artifact.
  Once per delegation, never on a first failure, and an unreachable advisor
  leaves the repair exactly as it was. `OMS_ADVISE_ON_REPEAT=0` opts out.

### Fixed
- The capability refresh stopped asking each CLI for `--help` a second time.
  `model-doctor` already had that output, so the refresh added a duplicate
  invocation of every installed provider — slower everywhere, and in the test
  suite it turned fixture runs into real CLI calls, which made unrelated
  timing-sensitive tests fail at random. Caught by the suite failing twice on
  two different tests while three runs of the pre-capability commit passed. The
  doctor now hands its help output over, and three consecutive runs are clean.
  Routing's own binary-identity check is memoised per process and uses `stat`
  rather than starting a python interpreter.
- The worker guard was blind to new source files in any repository with a large
  ignored tree. It spent its entry budget in path order, so a real project whose
  `data/` and `runs/` hold tens of thousands of ignored files exhausted the cap
  alphabetically and never reached the handful of untracked source files — the
  exact places a stray worker write lands. Untracked-but-not-ignored paths are
  now scanned first, so truncation can only ever drop ignored churn, and the
  message says which class was cut: reaching the cap inside the ignored tree is
  a note, failing to cover the untracked files is a warning that coverage is
  partial.
- `peer-delegate` says when the caller's tree is dirty. The worktree is built
  from HEAD, which is the right isolation and was completely silent: a brief
  written about code the caller has on screen can describe something the worker
  will never find, and the whole round is spent against a tree that does not
  contain the problem. The count is printed once and recorded in the artifact,
  so whoever reviews the patch later knows which base it was written against.
- `peer-delegate` no longer reports a worker that could not act as a clean run.
  A CLI denied a tool it cannot prompt for exits 0 having printed only its
  refusal, and the empty patch that follows looks exactly like honest work on an
  already-correct tree. The worker status is now 126 ("found but could not
  execute"), the reason is quoted, and — like a missing CLI — no repair round is
  spent, because no rewording of a brief grants a permission.
- Landings are now serialized per repository. Everything from the clean-tree
  check through `git apply` was a check-then-act on the shared working tree with
  no lock, so two agents could both be admitted against the same base and both
  apply — each reviewed without the other's changes. A second lander is refused
  outright rather than queued, because the tree it was admitted against is the
  one the first lander is changing. `oms_hold_file_lock` holds a lock for the
  rest of a process, which `oms_with_file_lock`'s subshell cannot do when later
  steps need variables the earlier steps set.
- A council counted a provider refusal as an answer. Antigravity's headless mode
  cannot prompt for a tool permission, so it auto-denies anything outside
  `permissions.allow`, exits 0, and prints only its own explanation — text
  declarative and long enough to pass every answer-quality rule. The last live
  audit therefore reported "2 answered, 2 independent model families" when one
  provider had spoken. Answer quality now classifies a body that is *entirely*
  CLI diagnostics as `blocked` (an answer that merely discusses permissions is
  untouched), keeps it out of the answered and family counts, and quotes the
  provider's own reason so the operator knows what to fix. `doctor` reports the
  same thing before a call is spent, by checking whether antigravity's
  allow-list covers the commands a repository read needs. Probing the CLI
  showed a curated command list cannot work — a `command` rule is matched
  against the whole command line, so the first `cd x && rg y` a peer runs is
  denied — while `command(*)` is not the write authority it appears to be:
  under `--sandbox`, which is how this harness invokes agy, escaping to write
  needs a separate `unsandboxed` rule. The check now asks for `command(*)` plus
  `read_file(*)` instead of an unwinnable list of commands.

### Added
- A second audit council was asked where the harness is incomplete *and* where
  it is over-built. Three integrity holes it found are fixed: the worker guard
  compared `git status` categories, so a worker rewriting a file the user had
  already modified stayed invisible (it now hashes the diff bytes); thread
  appends read the max sequence and wrote it back without a lock, so parallel
  panel answers could claim the same number (now serialized, and the current
  pointer is bound to the task that opened it, so a new task cannot inherit an
  unrelated conversation); and landing now refuses to apply when the intent
  cannot be recorded, while `--recover` requires the patch to still hash to what
  the intent recorded and hands a changed one to a human instead of guessing.
- Acting on the same council's over-building findings, three things were cut
  back rather than grown: guard surfaces that cannot be attributed (untracked
  and ignored files, tracked content) now report instead of failing the run,
  because a user editing their own repo during a delegation looks identical to
  a worker doing it (`OMS_WORKER_GUARD_STRICT=1` restores hard failure);
  councils refuse an expansion above `OMS_COUNCIL_MAX_CALLS` (default 12) rather
  than quietly making 27 provider calls; and answer classification no longer
  treats brevity as a defect — only an empty body or a reply that is nothing but
  questions counts as a non-answer, since rejecting a concise correct answer
  spent another provider call for nothing.
- Shared memory has a declared write contract instead of an accidental one:
  `shared.md` and `pins.md` are append-only (a correction is another appended
  note), `summary.md` is derived and regenerated by `compact`, and the
  worker-authority guard now holds the two logs to that contract. Editing a note
  another agent wrote — the way a worker would rewrite what the next session
  reads as fact — is reported as `memory/shared.md had existing rows rewritten`.
  A regression test pins the writers to append-only so a later refactor cannot
  silently break what the guard depends on.
- Shared `.oms` state is checked by its contract instead of being trusted
  wholesale. Workers are handed `OMS_STATE_REPO` so they can append, so the
  guard allows growth but records each JSONL family's length and the hash of
  exactly those bytes: rewriting rows already there, truncating a ledger, or
  deleting a state file fails the run and names the file and what happened
  ("failures.jsonl was truncated (42 -> 3 bytes)"). This closes the last gap the
  live council raised — erasing the failure that blocks a retry, or forging a
  thread turn another agent reads as evidence, was previously invisible because
  `.oms` was excluded entirely.
- Worker-authority detection covers what a live four-model council said it
  missed: untracked and ignored content is compared by stat, so planting
  `sitecustomize.py` or swapping a file inside an ignored `.venv/` is caught
  (`git status --ignored` collapses a fully ignored directory to one entry and
  sees neither), and object-store wiring is compared too — alternates,
  `info/exclude`, grafts, shallow, linked-worktree registrations, and submodule
  config/HEAD under `.git/modules`. The stat scan is bounded by
  `OMS_WORKER_GUARD_MAX_FILES` (default 20000) and says when it truncated. The
  limits are now documented rather than implied: a reverted write, anything the
  worker reads, and anything outside the repository need process isolation a
  bash harness does not have.
- The verification gate answers at feature level, not per line of chatter:
  `scripts/check.sh` prints one `ok: <stage>` per suite and the smoke runner one
  `scripts-smoke: ok (N tests)` per shard, so a green run is ~25 lines instead of
  a few thousand. A failing stage still prints its full output, and
  `OMS_VERBOSE=1` restores everything — the detail is evidence for a failure, not
  something an agent should read (and pay for) on every pass.
- Panels reach the canonical councils: `peer-ask` and `peer-review` take
  `--tiers fast,balanced,deep` and accept a tier inside `--providers`
  (`codex:deep`), so one council can span providers and model tiers. Each target
  writes its own tier-labelled artifact, the same target twice is still refused
  (the `agy` alias included), and both tools now report how many independent
  model families answered — with a warning when they all share one, because
  agreement inside a family is replication, not corroboration. Target parsing
  and family accounting are shared with `oms consult` so the notation cannot
  drift.
- Worker authority is checked mechanically, not only stated in the brief.
  `peer-delegate` fingerprints the surfaces a worker must not touch — the primary
  worktree's tracked state, local git config, remotes, refs, and hooks — before
  and after every worker round. A change fails the run, names the surface, keeps
  the worktree as evidence, records the violation in the fail-ledger, and fails
  the executor: patch scope only ever inspected the patch, so a worker writing
  around it was invisible. This is detection, not a sandbox
  (`OMS_WORKER_GUARD_OFF=1` opts out).
- `oms consult` targets can name a tier or an exact model and may repeat a
  provider, so a panel is several models rather than one per CLI:
  `--to codex:deep --to codex:balanced --to claude:model=claude-opus-5` records
  each answer separately, and `--all --tiers deep,balanced` spans providers and
  tiers. Results are reported as "N targets answered, K independent model
  families", and a panel whose answers all share a family says so — repetition
  within one family is not corroboration.
- Worker tiers follow the role before the phase: a `decision-advisor` invoked
  under a `verify` phase resolved to the cheapest model, which is the wrong
  model for the most consequential read. Precedence is now explicit class >
  role > phase > default, role files can declare their own tier
  (`oms-model-class: deep`), `agent-call`/`agent-run` take `--operation NAME` so
  a caller declares the work phase instead of inheriting the calling script's
  label, and the route line records which rule decided
  (`class=deep (role)`).
- Patch landing is now a recoverable transaction: `patch-land` writes an intent
  row to `.oms/landings.jsonl` (patch hash, base sha, task, lease) before the
  irreversible apply and a completion row after lineage and plan finish, so a
  crash in between is visible instead of looking like nothing happened. `oms
  state` lists interrupted landings, and `patch-land --recover` decides from the
  tree whether the patch went in, records the lineage and plan completion the
  crash skipped (or releases the task and marks it abandoned), and is idempotent
  — it never applies anything itself.
- `oms run validate` grew a family registry: beyond parsing and schema drift it
  now checks the fields each reader indexes on (`run_id`, `landing_id`,
  `fingerprint`, thread `seq`/`role`, …), closed lifecycle sets (landing events,
  board status, thread roles), plan task states, and plan dependencies pointing
  at tasks that do not exist — the failure mode where valid JSON silently
  vanishes from derived views.
- Antigravity routing moves to the current Flash generation (`Gemini 3.6 Flash
  (Low)`/`(Medium)` for fast/balanced; deep stays `Gemini 3.1 Pro (High)`, the
  strongest reasoning model the CLI exposes).
- Verification that cannot go stale unnoticed: `agent-task verify` now binds a
  pass to a content-free fingerprint of the tree it passed on and to the
  contract that ran, `status` reports `verification: fresh|stale|none` (also in
  `--json`), and `close` refuses a packet whose declared verification is missing
  or stale unless `--reason TEXT` records why — closing promotes "Closed task"
  into shared memory, so an unverified close hands the next session a false
  green.
- `oms state` marks CI recorded for a different commit as STALE with the
  current HEAD and the refresh command (`ci.fresh`/`ci.current_sha` in `--json`),
  instead of printing an old green conclusion with no indication it is old.
- Cross-agent answers are checked mechanically before they count: `ma_answer_quality`
  classifies a provider reply as ok, thin, or empty (a CLI can exit 0 and return
  a banner or a clarifying question), threads label non-ok turns, `oms consult`
  falls back to one other peer when an automatically picked one fails or does
  not answer (never for a pinned `--to`, never after an outbound-gate block),
  and `--all` reports how many peers actually answered.
- `skill-doctor` rightsizes instruction files: it flags a `SKILL.md` over the
  load budget (`OMS_SKILL_WORDS`, default 900) and references/ that no SKILL.md
  links, so skills stay short routers into detail read only when relevant.
  Generated `PROJECT.md` now leads its notes with gotchas — the non-obvious
  decisions an agent would otherwise get wrong — rather than restating what the
  repo already shows.
- Cross-agent conversations (`agent-thread.sh`, `oms thread`) and one verb to
  use them (`agent-consult.sh`, `oms consult`). Every cross-agent call used to
  be one-shot — the peer answered into an artifact and the next call started
  from nothing — so agents could not exchange context. A thread is an
  append-only transcript in `.oms/threads/<id>.jsonl` that any provider reads
  and extends: `oms consult "question"` picks a peer that is not the caller,
  attaches the active task, shared memory, and the conversation so far, records
  both turns, and prints the answer; `--all` asks every installed peer in
  parallel into one thread. `--thread ID` also works on `agent-call`,
  `agent-run`, `peer-ask`, `peer-delegate`, and `advise`, so council answers,
  delegated patch outcomes, and advisor verdicts land in the same conversation.
  Turns are budget-truncated and pass the shared sensitive-content gate before
  they can be replayed into another provider's prompt. `oms state` lists open
  threads and `oms gc` sweeps only closed ones.
- Machine-readable views for the remaining state tools: `agent-task status
  --json`, `artifact-index list|latest|failures|unresolved --json`,
  `run-ledger list --json`, `run-capsule list --json`, `data-manifest list
  --json`, and `session-handoff list --json`, all schema-1 and empty-state safe.
- `oms state` and `oms init` now cover the project harness itself, not only
  `.oms` state: applied rule styles, `PROJECT.md` state, and any agent file
  visible to git, with hand-written agent rules reported as `unmanaged` rather
  than prompting for a template. `oms init` applies the git exclusion when a
  repo got `git init` after its template (the order the bootstrap flow uses,
  where the template step cannot), reports missing or half-applied rules ahead
  of task/plan advice, and takes `--no-private`. `remove-project-template`
  points at the exclusion it leaves behind.
- Local-only project harness (`project-private.sh`, `oms project-private`): the
  agent-facing files (`AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `PROJECT.md`, plus
  `--path` extras) are listed in a managed block in `.git/info/exclude`, so a
  public repo carries no harness residue and the project's own `.gitignore`
  stays untouched. `apply-project-template` applies it by default
  (`--no-private` / `OH_MY_SETTING_PRIVATE_AGENT_FILES=0` opts out),
  `project-doctor` warns when an agent file is visible to git, `oms init`
  points at the fix, and delegate/admission worktrees seed the local-only
  copies so workers still read the project rules without those copies reaching
  a patch. An already-tracked agent file is reported, never silently untracked
  (`apply --untrack` stages that explicitly).
- Machine-readable views for shared state: `fail-ledger list --json`,
  `experiment-board list --json`, and `oms-run timeline --json` emit schema-1
  JSON objects, and `repo-state --json` now declares `"schema": 1`.
- The `oms` dispatcher prints a `--help` recovery hint whenever a tool exits
  with the misuse convention (exit 2), and `oms list` descriptions are joined
  to the full first sentence of each tool's header instead of a truncated
  first line.
- Tool discoverability for agents: `agent-harness` and the global rules point
  at `oms list`, `.oms` state files are documented as read-only-by-hand, and
  the skill router gains English resume triggers plus generic data-leakage
  triggers routing to `research-method` (with an `oms data-manifest` pointer
  in its skill).
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
- Antigravity never received its prompt. `agy --print` takes the prompt as its
  value (`--prompt` is an alias), so a prompt piped on stdin was ignored and the
  next flag became the prompt — every antigravity answer was a reply to the
  literal string `--sandbox`, in councils, reviews, and delegated writes alike.
  The prompt is now passed as the flag value.
- A landing no longer reports `complete` when its records did not land: a failed
  lineage row or plan finish leaves `applied-pending-receipt`, which `oms state`
  shows and `patch-land --recover` retries until both records exist.
- `patch-land` rechecks the base commit and clean tree immediately before the
  apply and refuses when either moved during admission, so what lands is the
  combination that was verified.
- Antigravity model routing follows the published benchmarks: 3.6 Flash wins
  every coding and agentic suite against 3.1 Pro (SWE-Bench Pro, DeepSWE,
  Terminal-Bench, MLE-Bench) at roughly twice the speed, and Pro keeps only a
  narrow pure-reasoning lead, so all three tiers now use the 3.6 Flash variants.
- `model-doctor --live-models` no longer reports every Antigravity route as
  missing: the CLI lists its catalog as slugs (`gemini-3.6-flash-low`) while
  naming the same model `Gemini 3.6 Flash (Low)` on `--model`, so the comparison
  now normalizes both sides. The smoke fixture printed display names and hid the
  mismatch; it now uses the notation the real CLI emits.
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

### Removed
- GitHub Release publication, release-only checksum tooling, documentation,
  and CI contracts. Installation now uses the repository source channel; exact
  tags, branches, or commits remain available through `--ref`.

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
- Source installation refs: `--ref` and `OH_MY_SETTING_REF` can select `edge`,
  a tag, branch, or commit. The source installer follows the origin default
  branch in edge mode and pins exact refs to detached commits.
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
  `install.sh.sha256`, and a `SHA256SUMS` manifest. This historical publication
  path was removed after the project switched to source-only distribution.

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
