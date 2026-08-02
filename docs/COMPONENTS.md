# Components

The full catalog of scripts and subsystems behind the capabilities in the
[README](../README.md). Everything here is invoked by your coding agent on its
own when the task calls for it — you describe intent in chat, the agent picks
the right script or skill. Nothing here is meant to be run by hand; it is
documented for transparency and recovery.

Grouped by area. Each entry names the script that implements it; the
narrower behaviour notes are the record of why it works that way.

## Project

**Start router + spec interview** — Detects empty/existing/ongoing state; gates
new, broad, ambiguous, or architecture-shaping work while clear bounded changes
proceed from local evidence

**Templates (`apply-project-template.sh`)** — Managed rule blocks for
general/ml/slurm projects; ML adds five core docs, a `check.sh` verification
contract, and `ml_smoke.py`; `--full-docs` adds every optional doc template;
the agent-facing files it writes are hidden from git by default (`--no-private`
keeps them visible)

**Local-only agent files (`project-private.sh`)** — Keeps the agent-facing
harness files (`AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `PROJECT.md`, plus
`--path` extras) out of a project's git history by listing them in a managed
block in `.git/info/exclude` — per-clone, never committed, and no entry added
to the project's own `.gitignore`. Applied by default at template time
(`--no-private` or `OH_MY_SETTING_PRIVATE_AGENT_FILES=0` opts out); `status`
reports hidden/tracked/exposed (`--check` fails on exposure, `--json` for
machines), `remove` drops the block without touching files. A file git already
tracks is reported, never silently untracked (`--untrack` stages that
explicitly). Delegate and admission worktrees seed the local-only copies so a
worker still reads the project rules, and the shared exclude keeps them out of
the patch

**Project doctor (`project-doctor.sh`)** — Verifies every agent sees the same
rules, spec state, and scaffold; warns on empty ML scientific-contract fields,
agent files exposed to git, and structure drift (stray root files, tracked
data, missing `src/` layout)

**State verify (`state-verify.sh`)** — Read-only consistency verdict over a
repository's `.oms` tree: composes `oms-run validate`, `artifact-index
validate`, task and journal status, then cross-checks what no single family
owns — dangling `CURRENT` pointers, active packets carrying `closed_at`,
unparseable packet timestamps, orphaned delegation markers, locks inside
`.oms`, derived journal views behind their events. Never repairs; every
finding names its remedy command

**Support bundle (`support-bundle.sh`)** — Redaction-first diagnostic bundle
for peer consultation and bug reports: bounded derived summaries only
(repo-state, state-verify, task/journal status, fail-ledger rows, artifact
index counts — never artifact contents, logs, diffs, or datasets), the repo
path, home directory, and hostname stripped from every file, then the shared
sensitive-content scrubber; a file that still trips it is replaced by an
omission notice, and `MANIFEST.md` records inclusions, omissions, and the
never-collected list

**Provider contract (`provider-contract.sh`)** — Cross-CLI conformance gate:
loader parity on a throwaway fixture (identical managed block sets across
`AGENTS.md`/`CLAUDE.md`/an adopted `GEMINI.md`, stale base styles retired),
harness-state MCP registration parity (every registered provider reads one
`oms-mcp-server.py`), and fail-open hook no-ops in unadopted directories; a
missing provider CLI is a note, never a failure

## Peer agents

**Review (`peer-review.sh`)** — Three distinct local models review the diff in
parallel; duplicate provider identities and the `agy`/`antigravity` alias pair
are rejected, while `model-doctor` separately verifies underlying configured
model-family diversity; ML pre-training gate (`--ml`), debate rounds,
per-finding verdicts, and `--gate` one-command pass/fail verdict backstopped by
a mechanical `--verify` run of the project's own `check.sh` — a self-reported
pass cannot pass a failing diff; untracked-file content absent from the diff is
flagged to reviewers

**Ask (`peer-ask.sh`)** — Same question to distinct local models for
independent opinions; `--tiers` (or `codex:deep` inside `--providers`) turns
the council into a panel across model tiers, each target writing its own
artifact, with the expansion refused above `OMS_COUNCIL_MAX_CALLS` (default 12)
so a Cartesian panel plus debate rounds cannot silently become dozens of calls,
and the run reports how many independent model families answered — one family
means replication, not corroboration; duplicate provider identities are
rejected and `model-doctor` can detect a shared underlying family; optional
debate rounds and hypothesis design-attack preset

**Delegate (`peer-delegate.sh`)** — Runs a write task in an isolated git
worktree and verifies it there, then returns a reviewable patch (verify bounded
by `OMS_PEER_VERIFY_TIMEOUT`, default 10m; provider calls by
`OMS_PEER_TIMEOUT`, default 5m, escalating SIGTERM→SIGKILL after
`OMS_PEER_KILL_AFTER`, default 15s; `OMS_REQUIRE_TIMEOUT=1` refuses to run
unbounded when no timeout binary exists). `--apply` only on a clean tree; a
dirty caller tree is reported and recorded in the artifact, since the worktree
is built from HEAD and the worker cannot see uncommitted work. `--task-id`
stamps lineage; `--plan-task` captures a fencing lease and hydrates
brief/verify/role; `--role NAME` uses a reusable profile; `--executor ID` uses
a hash-frozen task soul. Automatic `fast`/`balanced`/`deep` routing permits one
capacity-only fallback and never retries a dirty worktree; workers get child
provenance plus `OMS_STATE_REPO` and cannot create harness state in the
throwaway worktree.

**Repair rounds (`--repair N`)** — Preserves and re-injects the original
role/frozen executor soul, and once a repair round has *also* failed it
consults an advisor before trying again — the rules already said to do this
after repeated failures and nothing did, so the loop just re-sent the same
brief to the same worker. Fires once per delegation and only from the second
round, so an ordinary run never pays for it; the verdict is injected into the
next attempt as evidence and recorded in the artifact, and an unreachable
advisor leaves the repair exactly as it was (`OMS_ADVISE_ON_REPEAT=0` opts
out).

**Worker-authority guard (in `peer-delegate.sh`)** — Fingerprints the surfaces
a worker must not touch around every worker round: primary tracked state,
untracked and ignored files by stat (so replacing a file inside an ignored tree
is caught, which `git status --ignored` collapses away), local git config,
remotes, refs, object-store/worktree/submodule metadata, and hooks, plus shared
`.oms` state by its append-only contract — the JSONL families and the memory
log/pins, where growth is allowed but rewritten rows, truncation, or a deleted
state file are violations named with the file (`summary.md` is derived, so it
is tracked by presence only). For surfaces that can be attributed to the worker
(config, remotes, refs, hooks, metadata, shared state) it fails the run naming
the surface, keeping the worktree as evidence and recording the violation;
untracked/ignored churn and tracked content are reported but do not fail an
ordinary run, since the user editing their own repo is indistinguishable
(`OMS_WORKER_GUARD_STRICT=1` fails on everything). Detection, not a sandbox: a
reverted write, anything the worker reads, and anything outside the repo stay
invisible. The stat scan is bounded by `OMS_WORKER_GUARD_MAX_FILES` and spends
that budget on untracked-but-not-ignored paths before ignored ones — so a
repository with a huge ignored `data/` tree cannot push its new source files
out of the scan — and reports truncation, distinguishing "stopped inside the
ignored churn" (a note) from "could not even cover the untracked files"
(partial coverage). `OMS_WORKER_GUARD_OFF=1` opts out.

**Delegation depth cap (`OMS_DELEGATE_MAX_DEPTH`, default 1)** — A delegated
worker does not spawn its own worker, because nesting multiplies: cost,
worktrees, and writers against one repository all grow with the power rather
than the sum. The refusal says what to do instead (do the work, or report what
needs splitting so the caller fans out) and points at `oms consult`, since
read-only help never widens authority and stays available at any depth. A
genuine two-stage job raises the cap deliberately, in one place, where the cost
is visible.

**Role profiles (`agent-role.sh`)** — Named, reusable worker personas as
markdown in `.oms/roles/<name>.md` (global fallback
`~/.oh-my-setting/local/roles`); `list`/`show`/`resolve`/`init`. The owning
agent picks a role and injects it into a delegated worker (or an `agent-plan`
task's `role` field) so the same reviewer/refactorer/test-writer persona drives
any provider

**Executor souls (`agent-executor.sh`)** — Two-stage bounded-write contract: a
model proposes behavior, the owner validates and freezes it, and `meta.json`
retains provider/model route/task lease/base/scope/verify authority. Executors
always use isolated worktree write mode; read-only passes use
`agent-run --mode read`. The former `--mode worktree-write` remains a hidden
compatibility no-op, while legacy read metadata can only be inspected or
retired. `brief` rechecks the frozen hash; lifecycle is draft -> frozen ->
running -> done/failed; GC removes only aged non-active states

**Single-agent router (`agent-run.sh`)** — Routes one prompt to one provider:
read-only questions to a call, write tasks to a delegate worktree; automatic
routing follows the work phase first (routine execution/checks `fast`,
implementation/review `balanced`, decisions/gates `deep`) and uses the role
only as a fallback, with `--model-class`/`--model`/`--reasoning-effort`
overrides; `--executor ID` forwards a frozen executor only to write mode

**Task-fitted effort** — Automatic routing picks the tier from the work phase,
then the thinking level from the task and the model: an operation that gates
something irreversible (`advise`, `decision`, `release`, `review-gate`) takes
one step above its tier's effort, and the result is clamped to the scale the
selected model actually publishes. The same release gate therefore runs at
`xhigh` on codex and claude and at `high` on antigravity, without either being
spelled out anywhere. Routine work is untouched; `OMS_REASONING_NO_HEADROOM=1`
disables the escalation

**Capability snapshot (`lib/model-capability.sh`)** — What each provider CLI
can actually do, cached under `~/.cache/oh-my-setting/capabilities` and keyed
to the binary that produced it, so a new install or an nvm switch is a miss
rather than a wrong answer. Holds the thinking control's mechanism (`flag`,
`config`, or `none`) and the scale it accepts, read out of `--help`, plus the
live model catalog where a local source exists: agy prints one model per line,
and codex answers `model/list` on its app-server over JSON-RPC — which also
reports each model's *own* reasoning-effort scale, so `gpt-5.6-sol` reaching
`ultra` while `gpt-5.6-luna` stops at `max` is a fact the router has rather
than a guess. Claude Code has no catalog command at all and is reported as
unverified rather than assumed good — but it rejects an unknown model locally
in about two seconds, which is what makes the call-time fallback below cheap.
`model-doctor` is the writer — it already asks every CLI what it supports — and
routing is a pure reader: deciding a route must not spawn a provider process,
which would put a call in front of the one the caller asked for. With no
snapshot the registry declaration stands, so a provider is never reported as
incapable merely because nobody has looked. Detection widens the declaration
and never silently removes it: codex takes effort through
`-c model_reasoning_effort=`, which no help text advertises, so a probe that
cannot see it is not evidence of absence

**Model fallback at call time** — Claude's tiers carry the pinned name first
(an artifact should say which model actually ran) and the CLI's own public
alias (`fable`, `sonnet`, `haiku`) as the same-tier alternative: a pinned name
that rotates out is recovered from on the next call rather than turning that
whole tier into a hard failure, and the artifact records
`model-fallback: reason=model-unavailable`. A model whose own safeguard fires
on a message — Fable 5 does this to legitimate biology and coding work, says so
in the error, and names "change your model" as the remedy — falls through the
tier's remaining *distinct* models (`opus`, then `sonnet`, behind `fable`; an
alias resolves to the same model and is no answer to its filter), bounded by
`OMS_MODEL_SAFEGUARD_RETRIES` (default 2) and recorded step by step as
`model-fallback: reason=model-safeguard`. If every model in the tier flags the
message, the run fails saying exactly that rather than cycling. A considered
refusal reads differently and is reported with no retry at all.

**Model/CLI doctor (`model-doctor.sh`, `oms model-doctor`)** — Local-only by
default: checks provider binary/version and the exact CLI flags the harness
requires, resolves every `fast`/`balanced`/`deep` model and effort route, and
verifies model-family diversity even when Antigravity surfaces Claude or
GPT-family models. `--live-models` runs bounded catalog probes where a stable
list command is registered (currently `agy models`), `--json` emits schema-1
output, `--require-all` makes partial installs fail, and `--strict-diversity`
fails when fewer than two usable independent families remain

**Bounded plan driver (`plan-run.sh`, `oms plan-run`)** — Atomically claims and
executes exactly one pre-authorized plan task with its provider-neutral role,
scope, verification, lease, and bounded repair. A plan-bound executor resumes
only its already-claimed task with the exact provider and lease. Success stops
in `review`; explicit `--land` goes through the single `patch-land` mutation
boundary. Empty scope/verification and unchanged resolved route/task/executor
failures fail closed; it never loops, commits, pushes, publishes releases, or
recursively delegates

**Advise (`advise.sh`, `oms advise`)** — Cross-CLI advisor pass before
irreversible/high-risk decisions, after repeated failures, or at a
release go/no-go; routine completion does not require it. Composes an
adversarial VERDICT/RISKS/MISSING/NEXT prompt, attaches unresolved fail-ledger
rows (`--no-failures` to skip), picks the first available provider that is not
the caller (`OMS_ADVISOR_PROVIDER` or `--to` to pin), and sends it through
`agent-call.sh` (read-only, scrubbed, artifacts under `.oms/artifacts/advise`)

**Cross-agent threads (`agent-thread.sh`, `oms thread`)** — Durable multi-turn
conversations in `.oms/threads/<id>.jsonl`: any provider can read and extend
one transcript, so codex, claude, and antigravity build on each other's answers
across calls and sessions instead of firing one-shot questions. `context`
renders the recent turns within a byte/turn budget for prompt injection; turns
keep a bounded scrubbed excerpt plus the artifact path for the full text, and
the same sensitive-content gate as shared memory refuses secrets, private
paths, and cluster details. `--thread ID` on `agent-call`, `agent-run`,
`peer-ask`, `peer-delegate`, and `advise` records into the same conversation
(naming a thread creates it); `oms state` shows open threads and `oms gc`
sweeps only closed ones

**Consult (`agent-consult.sh`, `oms consult`)** — One verb for asking peers
mid-task: picks a provider that is not the caller, attaches the active task,
shared memory, and the running thread, records question and answer, and prints
the answer. `--all` asks every installed peer in parallel and records one
question with N answers in the same thread; `--to PROVIDER[:CLASS|:model=NAME]`
is repeatable, so one CLI can answer at several tiers or exact models and
`--all --tiers deep,balanced` spans both axes. Results report "N targets
answered, K independent model families" and flag a panel whose answers all
share a family, because repetition inside one family is not corroboration. A
CLI that exits 0 having printed only its own reason for doing nothing — a
denied tool, no login — is classified `blocked`, kept out of both counts, and
reported with that reason quoted, since the fix is the operator's and not
another retry; antigravity is the standing case, because its headless mode
cannot prompt and auto-denies every command outside `permissions.allow`, so
`doctor` warns when that allow-list is too narrow to read a repository. The
standing rules it needs are granted by `oms provider-permissions`.
`--thread`/`--new-thread` select the conversation. Read-only by design — a
write task goes to `peer-delegate`

**Export/import handoff (`--export-only`, `oms artifact-index import`)** —
Validates the route and writes the selected model with provider prompts when
the session may not call other agent CLIs directly; answers are imported back
into the same artifact index, passing the same outbound sensitive-content gate

**Change guard (`change-guard.sh`)** — Snapshots the live dirty tree and warns
when edits touch pre-existing dirty files, escape the declared `allowed_paths`,
or hit a `forbidden_paths` entry (deny beats allow); reads both from the active
task. Commits made after begin are included (diffs begin-HEAD against HEAD), so
committing does not bypass the scope check. Snapshot is written atomically and
stamped with a start time (optional `OMS_GUARD_PID` owner pid);
`status`/`oms state` flag an abandoned guard as STALE (`OMS_GUARD_TTL`, default
86400s) and `gc` sweeps it

**Patch admission (`patch-admit.sh`)** — Applies a delegated patch in a
throwaway worktree and runs a checks ladder (applies cleanly → task/executor
scope, deny first → shell/python/json syntax → verifier integrity →
verification contract) before it lands; ADMIT/REJECT verdict, recorded in the
artifact index; `--allow-verifier-change` overrides only the self-certification
guard

**Patch landing (`patch-land.sh`)** — The one mutating step that composes
admission: clean-tree check → `patch-admit` ADMIT gate → lease-fenced `landing`
state → landing intent recorded → `git apply` → records a land row in the
artifact index → optional `--plan-task` finish → completion recorded. Apply,
lineage, and plan finish are three writes, so each land brackets them with
intent/completion rows in `.oms/landings.jsonl`; `oms state` lists an
interrupted one and `patch-land --recover` finishes or abandons it idempotently
from what the tree actually shows. Nothing lands unless admission passes, the
tree is clean, and the captured lease is current. One landing at a time per
repository: the whole clean-tree-through-apply sequence is a check-then-act on
the shared tree, so a second lander is refused outright — queueing is pointless
when the tree it was admitted against is the one the first lander is changing.
A rejection is recorded in the fail-ledger; `--plan-task` alone reads the patch
path the task already stores

**Artifact index (`artifact-index.sh`)** — Schema-1 JSONL event envelope for
every cross-agent operation: event/operation/artifact IDs plus optional
run/task/delegation/parent lineage. `resolve --event-id` appends an idempotent
resolution event; `unresolved` and `oms state` replay
success/resolved/unresolved outcome status without conflating sibling
providers. Retention keeps or evicts target-resolution pairs atomically instead
of leaving dangling lineage, and state/doctor surface corrupt rows or invalid
contracts. External files are represented by name/hash descriptors, not host
paths. `validate`, atomic idempotent `migrate`, operation-aware `latest-run`,
high-water retention, and grace-protected orphan pruning keep the index bounded
and repairable. `telemetry [N]` (`--json`) groups the retained window by
provider/model route and reports recorded exits, verifier exits, fallbacks,
resolutions, and only the token/duration values recoverable from existing
artifacts. Coverage is explicit: an exit zero is not called semantic task
success, and rows outside the retained window cannot be inferred

**Safety rails (built-in)** — Outbound prompts are scrubbed before any external
CLI call (credentials, keys, machine/cluster details block the call); injected
context is fenced; diffs and debate quotes are sanitized; antigravity read
passes run in a detached-HEAD worktree (agy has no write-blocking flag) so
stray writes never reach the tree

## Agent state

**Work Journal (`scripts/lib/work-journal.sh`)** — Default-on, internal,
fail-open observer that projects completed run/capsule, validation, patch
review/admission, Git commit, CI/PR, experiment/job reconcile, explicit Agent
State, and handoff receipts into a bounded schema-versioned append-only event
log under `.oms/work-journal/`. A recoverable SQLite index makes duplicate
checks, changed-period rendering, and summary export incremental; the JSON
projection keeps only 256 recent descriptors while preserving all counts and
periods. Daily and ISO-week Markdown are deterministic atomic derived views;
late events rebuild their original periods. The prompt hook performs local
rollover catch-up, retries at most one pending Notion item within two seconds,
and injects a bounded once-per-local-day digest into agent context
(`OMS_WORK_JOURNAL_DIGEST=0` opts out). Work-time observers never call Notion;
an allowed top-level Stop captures final `HEAD` and force-syncs only today's
daily page.
`oms journal show --today|--week|--blockers|--recent N [--json]` is the agent
read path over the derived summaries and event index;
`oms journal status|rebuild|sync|configure`
provides the small operational surface and shares the observer's local/remote
locks. Recursive
sanitization excludes transcripts, raw logs, environments, diffs, credentials,
and unobserved facts. Notion export stores only nonsecret connection metadata
at install, delegates credentials to the official `ntn` CLI and OS keychain,
mirrors summaries as native blocks, upserts by stable key and content hash,
and leaves failed sync pending without changing local lifecycle results. Remote
work has its own non-blocking lock and bounded tick budget, so it never owns the
canonical append/materialization lock. Design, setup, responsibility
boundaries, examples, fallback behavior, and the cross-machine duplicate limit
are in
[WORK-JOURNAL.md](WORK-JOURNAL.md)

**Repo state (`repo-state.sh`)** — One read-only dashboard over the project
harness and all shared `.oms` state — applied rule styles, `PROJECT.md` state,
agent files exposed to git, active task/plan, experiment board, executor
lifecycle and soul hash, in-flight delegations, runs, CI, failures, artifacts,
and change-guard status; `--json` for machines, `oms state` alias. Hand-written
agent rules are reported as `unmanaged` instead of prompting for a template.
Pure query except opt-in `--refresh-ci`

**Failure ledger (`fail-ledger.sh`)** — Durable cross-session failure memory in
`.oms/failures.jsonl`: `record` a failed command by normalized fingerprint plus
a content-free git-state hash; `check` blocks the unchanged failure but permits
a retry after tracked state changes, `resolve` (by `--fingerprint` or by
`--cmd`, since a caller that just watched a command pass holds the
command)/`list` (`list --json` for machines); sensitive commands refused.
Surfaced in `oms state`. `agent-task verify` files a row when the stored gate
fails — with the failing line, written by the shell, no model involvement — and
clears it when the same gate later passes, which is what puts the primary
agent's own verification on the ledger instead of only harness-mediated paths.
A Claude Code `PostToolUseFailure` hook (`fail-ledger-hook.sh`, registered by
`install-claude-hooks.sh`, `OMS_FAIL_LEDGER_HOOK=0` opts out) extends that to
every failed Bash tool command in a harness-adopted repo: it surfaces what the
ledger already knows about the command as agent context, then records the
failure so the repeat crosses the advise threshold without anyone remembering
to call the ledger; interrupts/SIGPIPE are skipped, unadopted repos are never
seeded, and the hook is fail-open with a 5s ceiling

**Delegation liveness** — `peer-delegate` writes `.oms/delegations/<id>.json`
(pid, provider, model route, role/executor, soul hash, worktree, task lease)
while a worker is in flight and removes it on exit; GC fails a dead worker's
running executor and uses the task lease as a compare-and-swap fence, so an old
marker cannot release a newer claim

**Onboarding (`oms init`)** — Seeds the `.oms/` skeleton + `.gitignore`
(idempotent, non-destructive), hides the agent files from git (the recovery
point when a project got `git init` after its template, where the template step
could not), and prints a next-actions checklist tailored to the detected
project type and to what the repo already has — missing or half-applied project
rules outrank task/plan advice. Reports missing rules, never writes them: the
template needs a confirmed project type (`--no-private` skips the hiding)

**Skill router + turn guard + Claude HUD (`skill-router.sh`, `turn-guard.sh`,
`claude-statusline.py`)** —
UserPromptSubmit matches prompts against skill triggers and records route state
only for guarded work. Active-task recording is opt-in with `OMS_AUTO_TASK=1`.
Provider subprocesses stay silent, do not route/guard/write tasks, and emit
only a hash-only `ignored_child` event in the primary repo. Stop blocks at most
once when guarded work omits verification. Disable with
`OMS_SKILL_ROUTER_OFF=1` or `OMS_TURN_GUARD_OFF=1`. Claude Code's official
`statusLine` input feeds a local, dependency-free HUD with model, context,
subscriber rate-limit windows when present, estimated cost, and effort. The
renderer ignores transcript/path fields, bounds terminal output, strips control
characters, and makes no API call. The additive settings merge preserves a
user-owned status line; update and uninstall recognize only the managed
command. `install-claude-hooks.sh` also registers the `PreCompact`
handoff-snapshot hook, and `doctor.sh` verifies all four hook registrations
plus the HUD actually landed in `settings.json` whenever the install receipt
is valid — the installer treats registration failure as a warning, so the
doctor is what catches a silently hook-less install


**Shared memory (`agent-memory.sh`)** — Compact cross-agent facts in
`.oms/memory/`; append-only `shared.md`/`pins.md` remain the reversible source
of truth while `memory.sqlite3` is an automatically synchronized derived index.
New project notes retain a stable event ID, kind, task/session hashes, Git
HEAD, dirty bit, and bounded state fingerprint without storing a branch, path,
command, or raw diff; compact prompts omit that provenance and
`search`/`recall --json` exposes it on demand. `search` preserves exact
case-insensitive substring recall, `recall` ranks local FTS results without a
model call, and `rebuild` recovers the index from Markdown. The index takes
`.oms/failures.jsonl` as a third source, so one recall covers notes, pins, and
what already went wrong — `fail-ledger check --cmd` only answers for a
byte-identical command — and indexing it costs nothing, since a ledger row
already carries the command, exit code, and failing line. Closing a task also
promotes the latest `## Decisions` and `## Last Failure` lines the packet
already holds, which is the only recorded content that carries a reason rather
than a symptom; sensitive content is rejected at write time and notes are
attributed to the calling agent (`OMS_AGENT`, else CLI env markers). `health`
(`--json`) reads all three sources and opens SQLite in read-only mode to report
source counts, ledger state, schema/integrity/FTS status, currentness, and
provenance coverage. It exits nonzero for missing, stale, or degraded derived
state without creating or repairing anything; this is index health, not an
evaluation of whether the remembered content is useful

**Task handoff (`agent-task.sh`)** — Active packet with task ID,
active/verified/closed lifecycle, source-session hash, activity TTL, bounded
Current State, mechanical `verify` execution/evidence, explicit
skipped-verification reason that remains non-green, `rotate`, legacy migration,
and collision-safe archives; closing promotes the outcome into memory

**Task plan (`agent-plan.sh`)** — Shared schema-2 DAG with dependencies, scope,
verify command, and per-claim lease epoch/token. Harness workers carry the
captured token through start/review/release; only reviewed work with
artifact/patch evidence may enter `landing`, and only landing may finish.
`next --json` provides an atomic machine-readable claim for `plan-run`; reclaim
invalidates stale leases

**Strategy profiles (`agent-role.sh`, `roles/`)** — Provider-neutral
advisor/auditor/implementation/test/review profiles resolve project -> global
-> bundled. Cross-CLI workers receive them through `--role`/`--strategy`;
native Codex subagents receive the same full profile in their spawn message,
keeping execution contracts consistent across surfaces

**Harness-state MCP server (`oms-mcp-server.py`, `install-mcp.sh`,
`install-agy-plugin.sh`)** — Read-only MCP tools over one repository's shared
state: `oms_task_state`, `oms_fail_ledger`, `oms_handoffs`/`oms_handoff_show`,
and `oms_journal`. One stdlib stdio server serves every MCP client the same
state with no per-CLI hook code — which is also how Antigravity, whose CLI
fires no hook events headlessly, reads journal/handoff/fail-ledger context.
Claude Code and Codex register it directly (`install-mcp.sh`, idempotent,
user scope); Antigravity imports it as the `oh-my-setting` plugin, generated
with absolute paths at install time because `agy plugin install` copies the
plugin directory verbatim. Headless agy needs the consult permission profile,
which now includes `mcp(*)` (scoped mcp targets do not match on 1.1.9).
Strictly read-only: each tool maps to a fixed read-only subcommand, digest
reads take bare file names only, output is bounded

**Session handoff (`session-handoff.sh`)** — Distills a prior agent session
transcript (Claude/Codex/Antigravity) into a compact digest another agent can
pick up; mechanical, no model call. Digests land in the project's
`.oms/handoffs/` (the repo containing `--cwd`), which is where the Work
Journal's newest-handoff pointer looks. A `PreCompact` hook
(`precompact-handoff.sh`, registered for Claude Code by
`install-claude-hooks.sh` and for Codex by the plugin) captures a digest
automatically just before compaction discards the transcript detail:
best-effort by contract, harness-adopted repos only, never guesses when the
named session cannot be resolved, `OMS_PRECOMPACT_HANDOFF=0` opts out.
Antigravity exposes no compaction or prompt-submit hook surface, so it gets
skills and global rules but no automatic capture — run
`oms session-handoff capture --agent antigravity` by hand

## Experiments

**Run ledger (`run-ledger.sh`)** — Wraps training runs: pre-flight `check.sh`
gate, duplicate-run warning, one JSONL row per run in `docs/EXPERIMENTS.jsonl`,
`--metrics` records eval scalars; each row records the gate decision
(passed/skipped/recorded/none) and skipping an applicable gate needs `--reason`
(recorded, scanned for secrets); `top --metric KEY` ranks runs by a recorded
metric ("best run for val_auc")

**Research runner (`research-runner.sh`)** — Registered research runs:
hypothesis, pre-registered metric, and baseline recorded before launch, verdict
after

**Run capsule (`oms run capsule`)** — Reproducibility bundle per run: exact
commit + uncommitted diff + config/env/seed/output fingerprints + result;
`reproduce`/`verify`/`whence` (trace a checkpoint back to its run)

**Data manifest (`data-manifest.sh`)** — Fingerprints dataset splits;
`leakage --name <manifest>` flags train/eval overlap on the ID and any declared
`--key-column` (entity/pair/scaffold/family/assay/donor/batch/time), while
`check --name <manifest>` flags ID/key-set drift (stores only hashes, never
rows); fails closed on a missing split/column

**Job reconcile (`run-reconcile.sh`)** — Reconciles launched Slurm jobs against
`sacct`/`squeue` and writes the terminal state back to shared state so async
runs are not lost between sessions

**Study board (`experiment-board.sh`)** — Shared claim/start/finish lifecycle
above the ledger so agents do not duplicate runs; duplicate-claim guard with
stale-claim recovery; `list --stale` flags TTL-expired claims,
`list --owner NAME` filters by claimer, and `list --json` emits the derived
view for machines; `touch` heartbeats a live claim so it is not treated as
stale mid-run

**Run spine (`oms-run.sh`)** — Canonical `run_id` join index over the run tools
— `show`/`ls`, `diff` two runs (config/env/metric deltas), `validate` walks
every `.oms/**/*.jsonl` family, checks each line parses, and flags schema drift
(rows below the family's expected schema — the single place a migration is
signalled); `timeline` merges every `.oms` stream plus the ledger into one
time-ordered "what did the agents do here" view (`--json` for machines); `new`
writes a repo-scoped `.oms/runs/CURRENT` pointer and `current` resolves it, so
a second agent process joins the active run without env plumbing (stale
pointers expire after `OMS_RUN_CURRENT_TTL`); link rows record which agent
wrote them

**Job digest (`job-digest.sh`)** — Compresses long logs or Slurm jobs into a
compact digest; `--wait` blocks until the job finishes

**Single-machine queue (`tsp-queue.sh`)** — Sequential GPU job queue via
`tsp`/task-spooler for non-Slurm workstations; records completions to the
ledger, degrades to a nohup fallback

**ML context (`agent-ml-context.sh`)** — Compact ML digest (spec, ledger tail,
configs) attached to cross-agent calls

**Machine and cluster snapshots (`oms snapshot [--cluster]`)** — Private atomic machine and Slurm references with schema
validation and dry-run/check modes. Receipt modes preserve `0`/`1`/`auto`,
updates refresh enabled snapshots transactionally, and the Slurm reference
contains partitions, nodes, current-user associations/accounts, QOS/limits, a
captured queue view, and exact configured CPU/memory/time defaults

**Skill catalog** — Every install exposes the same compact, general-purpose
set: `agent-harness`, `oh-my-setting-ops`, `spec-interview`, `trace`, and
`trust-boundary`. Consultation, independent diff review, and isolated write
delegation are internal `agent-harness` routes; their `oms` commands remain
separate because their authority differs. `trust-boundary` is a
language-neutral security method for material trust boundaries, not a broad
framework checklist.

**Machine-conditional skills** — A manifest entry may declare
`"requires": ["cmd", ...]`; `link.sh` installs the skill only where every
listed command resolves on PATH and withdraws it when a machine loses one,
the router skips its triggers there (never naming a skill the session cannot
load), and the doctor reports the skip as a note. `slurm` (requires `sinfo`)
answers cluster questions from the private reference — partitions, node/GPU
inventory, and the `sacctmgr` account/QOS limits — instead of re-probing
nodes each session; `gpu-workstation` (requires `nvidia-smi`) checks VRAM
first, serializes runs through the tsp queue, and triages CUDA OOM in a
fixed order. Machines without those commands see exactly the five general
skills.

**Project skills (`skill-forge.sh`)** — Repository-scoped skills under
`.oms/skills/<name>/SKILL.md`, linked into `.agents/skills/` (Codex,
Antigravity) and `.claude/skills/` (Claude Code) so every CLI loads them
through native project discovery — no router entry, no manifest. Rails
rather than generation: `add` stores a skill only if it passes the Agent
Skills checks (name matches directory, description substantial enough to
route on, 500-line body budget) and the outbound scrubber, since a project
skill is standing context for every later session in that repo; links are
hidden from git through `project-private` and withdrawn when a skill goes
invalid. `validate`/`list`/`show`/`remove`/`status` make the set reviewable,
and the doctor reports per-repo health. A skill may declare a `verify:`
frontmatter command — the evidence it expects; validation checks the syntax
only, `status` counts declared contracts, `contracts` lists them, and
`agent-task close` reminds about each one rather than executing it. The ML template installs
`ml-experiment` (experiment-board duplicate check, pre-registered hypothesis
runs, run-ledger gate, reproducibility capsule) and `dataset-safety`
(manifest registration by declared group key, leakage and drift checks
before training) this way, so ML discipline exists only in ML repos.
`oms init` and `spec-interview` route the flow — evidence only, a few at
most — and `agent-task close` hints at promoting a lesson once a repeated
failure has been resolved in the repo. Until a repo holds its first project
skill, the skill router's daily state hint offers the forge on the same
signal, so the habit does not depend on using agent tasks.

## Code sources

**Registry (`code-source.sh`)** — Local registry of trusted reusable files
(e.g. personal model blocks); fetch by name into the current project

**GitHub fetch (`oms code-source github`)** — Profile/discover/fetch via `gh`; no
overwrite by default, provenance appended to `.oms/code-sources.jsonl`

## Maintenance

**Retention GC (`gc.sh`, `oms gc`)** — `--dry-run` by default; reclaims aged
transient state (orphaned delegation markers, archived task packets, stale open
runs, closed capsules, abandoned guards, draft/done/failed executors, resolved
failures) and delegates artifacts to `artifact-index prune`; never touches
frozen/running executors, live runs, the active task, unresolved failures,
review tasks, or the append-only board

**Verification gate (`check.sh`, `check-bash32.sh`, `check-python.sh`,
`pre-push-check.sh`, `install-hooks.sh`)** — One command runs the core checks
shared with CI (shellcheck, Bash 3.2 static scan, Python syntax, focused suites,
deterministic four-way smoke shards) and fails hard if a required tool is
missing. A passing run prints one timed `ok:` line per stage and one timed line
per shard; a failing stage prints its full output and `OMS_VERBOSE=1` restores
everything, so the gate stays readable between edits. CI enables bounded
per-test smoke timings (`OMS_SMOKE_TIMINGS=1`, default slowest 10) to make shard
rebalancing evidence-driven without filling every green log.
The Bash 3.2 scan is static plus one rule a linter cannot express: a
here-document inside `$( )` whose body holds an odd number of apostrophes makes
the whole file unparseable under 3.2 even behind a quoted delimiter, and that
has already shipped. `--lint-only`, `--focused-only`, and
`--scripts-smoke-only --shard I/N` give CI independent jobs, so a lint failure
cannot hide test results and each large shard gets its own runner. The fixed-name
`gate` job fails closed over lint, focused suites, every smoke matrix child, the
cross-platform install matrix, and macOS portability; branch protection should
require only `gate`, never a matrix child name. CI additionally runs the real
install lifecycle on Linux (both ownership modes), macOS, and Windows Git Bash;
the macOS job parses every script with its stock Bash 3.2 and runs the BSD
userland fixtures, the one place `sed`/`awk`/`date` differences surface.
`OMS_SMOKE_JOBS=1` enables serial local debugging. `install-hooks.sh` keeps the
complete gate as its safe default. After branch protection requires `gate`,
`install-hooks.sh --quick` installs exact-push-range changed-file feedback and
states explicitly that GitHub Actions remains authoritative; ambiguous pushes
fall back to the full local gate.

**CI status (`ci-status.sh`)** — Prints the latest CI conclusion for the
current branch and exits nonzero on a failed run, so a red push can't go
unnoticed; `record` appends the conclusion to `.oms/ci.jsonl` (deduped by sha)
so a later session / `oms state` sees "CI failed on <sha>" instead of the
result vanishing

**Provider permissions (`provider-permissions.sh`,
`oms provider-permissions`)** — The one setup step that used to live in a
person's head. Codex and Claude Code take authority per invocation and are
reported as needing nothing; antigravity needs standing rules, because headless
mode cannot prompt and auto-denies anything outside `permissions.allow`.
`--check` (what `doctor` runs) names the missing rules and the command that
grants them, `--print` emits them, `--apply` merges them after copying the
previous settings to `.bak` and never narrows an existing rule. Two profiles,
`consult` and `delegate`, named for what needs them (see the namespaces below).
A broader existing rule satisfies a narrower requirement, so re-running is
idempotent. `--allow-command` grants `unsandboxed(CMD)` one command at a time
and refuses a wildcard: that namespace is the sandbox boundary itself, and a
cold `uv run` needs it only because uv's cache lives outside the worktree.
`--allow-toolchains` grants the same for the package managers in a short
built-in list (uv, npm, npx, cargo, pnpm, yarn, go, poetry) *that are installed
here*, so a new machine reaches the same state without rediscovering it;
`pip`/`conda` (this setup uses uv), `docker` (socket is root-equivalent), and
`make`/`node`/`git` (they write in-tree, and a worker writing to the main
`.git` is what the worktree prevents) are excluded by name even when present

**Antigravity's permission namespaces** — Its four namespaces are `command`
(matched against the whole command line, so a curated list dies on the first
`cd x && rg y` — grant `command(*)`), `read_file`, `write_file` (a directory
target grants it recursively), and `unsandboxed`, which is what a shell
redirection actually needs: under the `--sandbox` this harness always passes, a
granted command still cannot write outside the sandbox, so `command(*)` is
shell access without write authority. A read-only peer therefore needs
`command(*)` and `read_file(*)`; a `peer-delegate` worker additionally needs
`write_file` covering the worktree parent (`$TMPDIR`, default `/tmp`).

**Install / update / doctor** — Stable/edge/pinned refs and minimal/full/custom
profiles. A schema-2 receipt records canonical ownership, boolean component
compatibility plus tri-state snapshot modes, managed targets, and the previous
successful commit while older receipts remain readable. Receipt-owned branch
and detached updates share one clean-tree transaction; link/snapshot/doctor
failure restores HEAD, links, snapshots, and receipt, and `update --rollback`
returns to the prior success. Foreign checkouts cannot mutate the canonical
install. Linux/macOS/WSL use symlinks; Windows Git Bash defaults to verified
copies with sidecar ownership hashes, so an untouched stale copy can be updated
without hiding the original user backup while a user-edited copy is preserved.
Native PowerShell is outside the Bash harness boundary

**`oms` dispatcher (`scripts/oms`)** — One stable entrypoint on PATH: the
public allowlist controls both `oms list` and dispatch (`run` aliases
`oms-run`); hidden install and hook scripts cannot be reached by guessing
filenames. `oms list` shows each tool's full first sentence, and any tool
exiting with the misuse convention (exit 2) gets a `--help` recovery hint

**Skill hygiene (`skill-doctor.sh`, `cleanup.sh`)** — Diagnoses
duplicate/missing skill-picker entries across all three agents, including
duplicate names across Codex's `.codex/skills` + shared `.agents/skills`
overlay, and rightsizes the skills themselves — a `SKILL.md` over the load
budget (`OMS_SKILL_WORDS`, default 900) or a `references/` directory no
`SKILL.md` links is flagged, keeping skills short routers into detail that is
read only when relevant. System and plugin skills remain upstream-owned rather
than being copied into OMS; cleanup removes only known legacy oms/backup
symlinks (dry-run by default, never regular files or plugins)

**Auto-update (`auto-update.sh`)** — Systemd timer or cron, installed by
default in apply mode: fast-forward only, dirty or diverged checkouts are
skipped (`OH_MY_SETTING_AUTO_UPDATE_MODE=check` records without applying;
`--no-auto-update` opts out entirely); `install`/`remove` own the user-level
trigger lifecycle, and the receipt records both the component and its mode
so updates preserve the choice — legacy receipts with no recorded mode stay
check

**Backup / unlink / uninstall** — Snapshot agent configs before changes; clean
removal that restores what it replaced
