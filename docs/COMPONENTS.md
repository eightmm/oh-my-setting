# Components

This catalog is for people using or maintaining the oh-my-setting (OMS) harness.
It maps each command to its purpose and detailed guide; OMS shares rules, state,
peer workflows and bounded patch handling across installed coding-agent CLIs,
with one verification boundary for OMS-managed delegated patches. The
[Codex harness launcher](CODEX-HARNESSES.md) runs Pi and Claude Code through an
invocation-scoped local gateway; the selected harness keeps its tool permissions.
Work rooms can also deliver to an explicitly enrolled
[Claude Desktop Code or terminal inbox](TERMINAL-PANEL.md#claude-desktop-code-and-claude-code-terminal-delivery),
with separate delivery receipts and an explicit opt-in to possible model wakeup.

## In this guide

- [Panel and collaboration](#panel-and-collaboration)
- [Plans and landing](#plans-and-landing)
- [State and memory](#state-and-memory)
- [ML and HPC](#ml-and-hpc-components)
- [Install, update and doctor](#install-update-and-doctor)
- [Internals](#internals)
- [Architecture and contracts](#how-it-fits-together)

Use `oms list` for the front-door catalog, `oms list --all` for all commands,
and `oms <command> --help` for flags. The
[command-routing reference](../custom-skills/oms-agent-harness/references/command-routing.md)
explains overlapping entrypoints.

## Panel and collaboration

| Component | Purpose | Invoke | Details |
|---|---|---|---|
| `panel` | Open the terminal control panel for live collaboration. | `oms panel` | [Terminal panel](TERMINAL-PANEL.md) |
| `dashboard` | Read a terminal view of OMS collaboration state. | `oms dashboard` | [Panel and collaboration](TERMINAL-PANEL.md#shared-work-rooms-and-the-graph) |
| `inbox` | Prioritize attention from shared state. | `oms inbox` | [State, memory, and handoff](#state-memory-and-handoff) |
| `room` | Share task-scoped messages and room snapshots. | `oms room` | [Work rooms](TERMINAL-PANEL.md#shared-work-rooms-and-the-graph) |
| `thread` | Read and deliver shared conversation turns. | `oms thread` | [Asking other agents](#asking-other-agents) |
| `consult` | Ask one peer or continue a peer thread. | `oms consult` | [Asking other agents](#asking-other-agents) |
| `peer-ask` | Ask a provider council and save each answer. | `oms peer-ask` | [Asking other agents](#asking-other-agents) |
| `peer-review` | Review a diff with an independent provider council. | `oms peer-review` | [Asking other agents](#asking-other-agents) |
| `advise` | Request an adversarial pass at a decision point. | `oms advise` | [Asking other agents](#asking-other-agents) |
| `agent-call` | Run one read-only local peer call. | `oms agent-call` | [Asking other agents](#asking-other-agents) |
| `peer-delegate` | Send a bounded write task to an isolated worktree. | `oms peer-delegate` | [Delegating writes](#delegating-writes) |
| `agent-supervisor` | Queue and supervise attempts without landing them. | `oms agent-supervisor` | [Delegating writes](#delegating-writes) |
| `agent-events` | Record and project agent lifecycle events. | `oms agent-events` | [Delegating writes](#delegating-writes) |
| `agent-plan` | Maintain a shared task graph. | `oms agent-plan` | [Plans and bounded autonomy](#plans-and-bounded-autonomy) |
| `agent-role` | Manage reusable profiles for the three agent CLIs. | `oms agent-role` | [Models and reasoning effort](#models-and-reasoning-effort) |
| `agent-task` | Maintain the active handoff packet. | `oms agent-task` | [State, memory, and handoff](#state-memory-and-handoff) |
| `approval-inbox` | Request and consume fenced durable approvals. | `oms approval-inbox` | [Plans and bounded autonomy](#plans-and-bounded-autonomy) |

## Plans and landing

| Component | Purpose | Invoke | Details |
|---|---|---|---|
| `autopilot` | Drive bounded coding under plan and landing contracts. | `oms autopilot` | [Plans and bounded autonomy](#plans-and-bounded-autonomy) |
| `goal-drive` | Drive an existing approved plan to its acceptance command. | `oms goal-drive` | [Plans and bounded autonomy](#plans-and-bounded-autonomy) |
| `plan-from-spec` | Propose plan tasks from `PROJECT.md` or discovered work evidence. | `oms plan-from-spec` | [Plans and bounded autonomy](#plans-and-bounded-autonomy) |
| `plan-run` | Execute one pre-authorized plan task. | `oms plan-run` | [Plans and bounded autonomy](#plans-and-bounded-autonomy) |
| `patch-admit` | Check delegated patch scope and evidence. | `oms patch-admit` | [Delegating writes](#delegating-writes) |
| `patch-land` | Apply an admitted delegated patch. | `oms patch-land` | [Delegating writes](#delegating-writes) |
| `land` | Gate, push and update a committed worktree. | `oms land` | [Verification and release](#verification-and-release) |
| `sync-shared` | Fetch and fast-forward an idle shared checkout. | `oms sync-shared` | [Verification and release](#verification-and-release) |
| `draft-pr` | Publish a verified clean HEAD as a draft pull request. | `oms draft-pr` | [Verification and release](#verification-and-release) |
| `ci-status` | Read the CI conclusion for a branch HEAD. | `oms ci-status` | [Verification and release](#verification-and-release) |
| `change-guard` | Warn about concurrent agent edits. | `oms change-guard` | [Delegating writes](#delegating-writes) |
| `checkpoint` | Save a reversible snapshot of tracked Git state. | `oms checkpoint` | [Delegating writes](#delegating-writes) |
| `scratch-worktree` | Manage scratch worktrees around active workers. | `oms scratch-worktree` | [Delegating writes](#delegating-writes) |

## State and memory

| Component | Purpose | Invoke | Details |
|---|---|---|---|
| `state` | View active, stale, pending and changed harness state. | `oms state` | [State, memory, and handoff](#state-memory-and-handoff) |
| `state-verify` | Check consistency of repository harness state. | `oms state-verify` | [State, memory, and handoff](#state-memory-and-handoff) |
| `agent-memory` | Maintain shared harness memory. | `oms agent-memory` | [State, memory, and handoff](#state-memory-and-handoff) |
| `journal` | Read and manage the local-first Work Journal. | `oms journal` | [Work Journal](WORK-JOURNAL.md) |
| `fail-ledger` | Record and inspect cross-session failures. | `oms fail-ledger` | [State, memory, and handoff](#state-memory-and-handoff) |
| `artifact-index` | Inspect and maintain indexed harness artifacts. | `oms artifact-index` | [Asking other agents](#asking-other-agents) |
| `session-handoff` | Build a compact digest from an agent session. | `oms session-handoff` | [State, memory, and handoff](#state-memory-and-handoff) |
| `snapshot` | Record private local context for agents. | `oms snapshot` | [State, memory, and handoff](#state-memory-and-handoff) |
| `gc` | Apply retention to repository-local state. | `oms gc` | [State, memory, and handoff](#state-memory-and-handoff) |

## ML and HPC components

| Component | Purpose | Invoke | Details |
|---|---|---|---|
| `data-manifest` | Fingerprint dataset splits and detect drift or leakage. | `oms data-manifest` | [ML and HPC](#ml-and-hpc) |
| `experiment-board` | Coordinate shared research studies. | `oms experiment-board` | [ML and HPC](#ml-and-hpc) |
| `job-digest` | Summarize training or Slurm logs and jobs. | `oms job-digest` | [ML and HPC](#ml-and-hpc) |
| `run-ledger` | Record a command and its execution metadata. | `oms run-ledger` | [ML and HPC](#ml-and-hpc) |
| `run-reconcile` | Reconcile long-running Slurm jobs with saved run records. | `oms run-reconcile` | [ML and HPC](#ml-and-hpc) |
| `run` | Tie experiment and execution records together. | `oms run` | [ML and HPC](#ml-and-hpc) |
| `tsp-queue` | Queue local jobs through task-spooler. | `oms tsp-queue` | [ML and HPC](#ml-and-hpc) |
| `runtime` | Inspect typed projections and optional execution services. | `oms runtime` | [Runtime guide](OMS-RUNTIME.md) |
| `harness` | Run Codex, Pi or Claude Code harnesses with a Codex login. | `oms harness` | [Codex harnesses](CODEX-HARNESSES.md) |

## Install, update and doctor

| Component | Purpose | Invoke | Details |
|---|---|---|---|
| `install-profile` | Inspect or install tools for selected capabilities. | `oms install-profile` | [Install guide](INSTALL.md) |
| `status` | Show install, task and update status. | `oms status` | [Install and ownership](#install-and-ownership) |
| `doctor` | Check managed links, tools, skills and manifest sync. | `oms doctor` | [Install and ownership](#install-and-ownership) |
| `project-doctor` | Check project agent-facing files for agreement. | `oms project-doctor` | [Project onboarding](#project-onboarding) |
| `skill-doctor` | Diagnose and rightsize installed skills. | `oms skill-doctor` | [Local skills and optional bundle operations](#local-skills-and-optional-bundle-operations) |
| `skill-forge` | Create, validate and manage project skills. | `oms skill-forge` | [Local skills and optional bundle operations](#local-skills-and-optional-bundle-operations) |
| `model-doctor` | Probe provider binaries and cached capabilities. | `oms model-doctor` | [Models and reasoning effort](#models-and-reasoning-effort) |
| `provider-permissions` | Check or grant provider CLI permissions. | `oms provider-permissions` | [Provider integrations](#provider-integrations) |
| `init` | Initialize a repository harness. | `oms init` | [Project onboarding](#project-onboarding) |
| `apply-project-template` | Apply managed project policy and scaffold files. | `oms apply-project-template` | [Project onboarding](#project-onboarding) |
| `remove-project-template` | Remove managed project policy blocks. | `oms remove-project-template` | [Project onboarding](#project-onboarding) |
| `project-private` | Keep project harness files out of Git commits. | `oms project-private` | [Project onboarding](#project-onboarding) |
| `intent` | Turn a broad goal into a reviewed project specification. | `oms intent` | [Project onboarding](#project-onboarding) |
| `update` | Update an installation transactionally. | `oms update` | [Install and ownership](#install-and-ownership) |
| `auto-update` | Check or apply source updates. | `oms auto-update` | [Install and ownership](#install-and-ownership) |
| `tick` | Run unattended maintenance for registered repositories. | `oms tick` | [Durable operations and optional frontends](#durable-operations-and-optional-frontends) |
| `cleanup` | Remove safe install leftovers. | `oms cleanup` | [Install and ownership](#install-and-ownership) |
| `uninstall` | Remove managed links and restore backups. | `oms uninstall` | [Install and ownership](#install-and-ownership) |

## Internals

| Component | Purpose | Invoke | Details |
|---|---|---|---|
| `models` | List cached provider model capabilities. | `oms models` | [Models and reasoning effort](#models-and-reasoning-effort) |
| `graph` | Query project and execution graphs. | `oms graph` | [Graph engineering](GRAPH-ENGINEERING.md) |

The catalog names the command front doors. Their variants and authority remain
separate; a shared index does not combine permissions.

### Consolidated paths

Agent guidance groups discovery into state, collaboration, implementation,
verification/landing, continuity, and operations. Public commands remain
compatible; these groups do not grant combined authority.

- Inbox projects shared state; detail-only lifecycle/approval reads remain
  separate because summaries truncate those lists. Reports are non-atomic
  across underlying stores. State and runtime reuse validated task/plan results
  within one query, without a persistent status cache or another CLI input.
- Recent log windows read backwards; MCP display reads at most 64 KiB before
  its existing output cap. Full artifact/thread references stream once so an
  early result is not lost. Peer and MCP share final Output/Exit interpretation.
- Runtime context reuses fresh Project Graph evidence selection, preserving
  explicit inputs and budgets. Missing/stale/unsupported graphs retain local
  discovery; read-only context neither builds graphs nor grants authority.
  Private delegation packs use the same freshness/path checks after their
  bounded build; they never refresh the parent's graph from a worker tree.
- Consult and advice share exact call-receipt validation; absent or ambiguous
  receipts cannot borrow the newest artifact. Advice is a prompt variant using
  the shared model router, not a separate fixed-high-effort routing policy.
  Consult prints the current result without replaying recent thread history;
  the thread retains that history for follow-up calls. Advice injects its
  bundled decision contract once, retaining the contract for custom roles.
- Tick and install auto-update share scheduler detection/selection while
  preserving distinct schedules, ownership markers, and repair transactions.
- Diagnostic, council, autonomy, profile, experiment, and continuity variants
  remain behind the existing front doors. Already shared engines are reused,
  not replaced with another orchestration layer or a combined memory store.

### Surface boundaries

Use `oms list --all` for the current catalog. Keep distinct authority and
provenance boundaries; remove duplicated routing and generated boilerplate.

| Surface | Decision |
|---|---|
| State, threads, memory, journal, graph/context | Keep shared evidence and incremental collaboration; reuse injected resume/digest context before extra journal reads. |
| Plans, delegation, admission, recovery, release | Keep leases, reviewed bytes and explicit authority; model capability cannot replace concurrency or recovery guarantees. |
| Install, update, repair, scheduler | Keep reversible ownership. Tick's backlog and current-day journal sync have different scopes. |
| Councils, runtime experiments, HPC, external frontends | Keep explicit workflows; no new default calls. One peer precedes a council when sufficient. |
| Daily and task-close skill-creation recommendations | Removed: resolved failures and frequent tool use alone do not justify another skill. Explicit forge and existing skill verification reminders remain. |
| Tool-family telemetry | Opt-in with `OMS_USAGE_TRACK=1`; historical usage remains readable and GC-managed. No daily scan of usage/skill bodies to recommend new skills. |
| Skill selection and onboarding | ASCII trigger boundaries prevent `oom` matching `room`; generic continuation/model questions no longer force the harness. Keep specific collaboration/graph/autopilot triggers. Store ordinary build/test facts in the existing contract. |
| Generic keyword skill hints | Off by default; native skill discovery remains. `OMS_SKILL_HINTS=1` restores keyword hints without changing live collaboration or journal delivery. |
| Stop session budgets | Answer-format blocking is removed, including legacy opt-ins. Positive `OMS_SESSION_BUDGET_TURNS` or `OMS_SESSION_BUDGET_HOURS` enables a session cap. Journal finalization remains. |
| External adapters | Removed the old Herdr pane-control command and standalone A2A HTTP bridge/card. The panel has a separate optional local Herdr terminal host for wrapper launch, identity-checked navigation and reported footer values; it grants no landing authority. Native peer calls, live threads and CLI/MCP state reads remain. No extras dispatcher remains. |

The seven skills retain distinct installation, coordination, specification,
diagnosis, security, Slurm and local-GPU responsibilities. Combining them would
load unrelated procedures or blur safety boundaries. Trace requires competing
hypotheses only when real alternatives remain, not a fixed worksheet.
Retired daily hint knobs (`OMS_USAGE_HINT_MIN`, `OMS_USAGE_HINT_DAYS`,
`OMS_USAGE_SKILL_ROOTS`) no longer affect prompts; no stored skills or history
are deleted. `OMS_SKILL_FORGE_HINT` still controls existing skill-contract
verification reminders at task close.

## How it fits together

```text
user request
  -> provider skill/router
  -> oms <tool>
       -> read: consult / peer-ask / peer-review / advise
       -> write: peer-delegate -> isolated worktree -> patch
                                      -> patch-admit -> patch-land
       -> orchestrate: agent-supervisor -> agent-events -> approval-inbox
       -> operate: runtime backend
       -> observe: inbox / targeted state queries
  -> local state in .oms/
       -> inbox / state / handoff / MCP / Work Journal
```

The owning agent remains responsible for scope, admission, verification,
commit, push, and release decisions. Peer agreement is evidence, not approval.

## Install and ownership

Systemd auto-update setup stages unit files and restores prior files, timer
enablement/activity and any newly enabled linger if activation fails. Failed
restoration retains the backup and reports its location; this cannot undo an
update process that already started. CI prepares ShellCheck lazily, so a full
selection does not download it again in the planning job, and parallel narrow
checks share one verified download.
Selected smoke cases also use the existing parallel runner: selection is
validated before dispatch, partitioned without duplicates, and capped to the
selected case count. No test coverage is removed. CI cancellation groups
include the event as well as the ref, so scheduled/manual checks cannot cancel
a push check awaited by landing; newer pushes still replace obsolete pushes
([GitHub concurrency](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency)).
Native platform checks share the existing lifecycle matrix's macOS/Windows
runners instead of provisioning two extra jobs. The same checks remain gated;
`!cancelled()` lets later checks report failures after an earlier failure
without continuing a cancelled run
([GitHub status functions](https://docs.github.com/en/actions/reference/workflows-and-actions/expressions#status-check-functions)).

The default installer selects the `core` capability: Bash, Git, Python, the
harness, and one coding-agent provider. It installs or verifies only the locked
tools required by the selected profile; optional capabilities add the council,
GitHub, Notion, research, HPC, container, or remote surfaces. Setup fails rather
than silently recording a selected profile with missing required CLIs. Required
provider dependencies are part of that plan: Codex and Claude include Node,
while Antigravity does not. Bootstrap versions, URLs, and integrity values live in
`tools.lock.json`. Providers resolve official stable releases during installation
and updates; a private `provider-tools.lock.json` records the resolved checksum
snapshot used by the installer and doctor, not a permanent version pin.
Explicit `OH_MY_SETTING_TOOL_LOCK` keeps reproducible pinned operation.
Recognized standalone Codex uses its native updater and keeps its launcher;
OMS checks the resulting version, not a provider-owned payload digest.
Direct platform payloads and npm wrapper/native packages are
verified before use and npm installation runs offline with a fresh cache.
Install, update, repair, and uninstall share one user-wide lifecycle lock. Lock
ownership survives installer `exec` handoff. A checkout-selected foreground
child may borrow the live parent's critical section only after matching its
owner, PID, start token, and canonical path; ownership and release authority
remain parent-only, and the borrow cannot pass to another child.
An already-installed external CLI at the exact version is reused as
version-only evidence; doctor distinguishes it from a digest-owned install.
Managed direct binaries and npm package/shim swaps carry recovery state, and
doctor reports digest drift or an interrupted transaction separately from lock
schema validity. PATH persistence follows Bash's existing login-file priority
instead of creating a higher-priority profile.
Optional provider registrations report a warning and remain visible to `oms
doctor --contract`. Windows Git Bash requires the exact locked native Node
release before setup when the selected profile needs a Node-backed tool; Linux
and macOS can install it during setup.

Managed rules and skills use symlinks on Linux/macOS and verified copies on
Windows Git Bash. Existing user files move to timestamped backups and return
on `oms uninstall`. Uninstall stops before unlink or purge if any integration
cannot be removed. It intentionally leaves external CLIs and the user-local
PATH entry installed.

The source installer follows `edge` by default. The daily updater applies only
clean fast-forwards and skips dirty or diverged checkouts. Set
`OH_MY_SETTING_AUTO_UPDATE_MODE=check` for notification-only updates, or pin a
ref when reproducibility matters. A missing pinned ref fails closed unless the
operator explicitly chooses `--fallback-to-edge`.

Useful maintenance commands:

```bash
oms status
oms doctor
oms doctor --repo . --json
oms doctor --repo . --remediation-plan
oms update
oms uninstall
```

`oms cleanup` dry-runs unless `--apply` is given. It removes a dead delegate worktree with uncommitted work only when exactly one recorded run names it and that run finished (a failed or timed-out run only after an owner decision or `OMS_HARNESS_RESIDUE_AGED_DAYS` days, default 7), its changed files equal the run's preserved patch applied to HEAD byte for byte, its ignored files are Python bytecode only, its `.oms` holds only hook telemetry, caches and empty locks, and the owning repo survives; it repeats that proof just before removal. A scratch worktree stays kept while it has uncommitted or untracked files, its HEAD is on no branch, tag or remote ref, or it holds `.oms/plan/tasks.json`, `.oms/failures.jsonl`, `.oms/landings.jsonl` or `.oms/artifacts/index.jsonl`; a clean scratch worktree at a referenced HEAD is removable. Worktrees holding other `.oms` records stay kept, and every kept item prints its reason.

Structured doctor reports adapt the existing checks; they do not create a
second health engine. Findings carry stable IDs, severity, and content-free
remediation metadata. `--remediation-plan` prints commands and their authority
class but never executes them, and it cannot be combined with `--repair`.
Doctor checks installed Git HEAD/ref validity without fetching or rewriting
refs. Damaged references require targeted recovery with preserved originals;
link repair is not Git recovery.

`oms status` separates the active OMS workspace revision/changed-entry count
from the canonical installation's actual revision/changed-entry count. Alignment
is `in-sync`, `pending-source-changes`, `different-revision`,
`installed-uncommitted`, or `unknown`; receipt SHA alone does not prove live
checkout identity. This display neither updates an installation nor proves
plugin-cache freshness or verification.

## Project onboarding

Say “Start this project.” or “이 프로젝트 시작해줘.” The
`oms-spec-interview` skill routes by state:

- Empty directory: clarify the goal, write `PROJECT.md`, apply a template, run
  the project doctor.
- Existing repository: inspect first, then ask only about gaps that change the
  implementation.
- Ongoing harness project: report current state and the next actionable step.

`general`, `ml`, and `slurm` templates add a managed policy block and a small
verification contract without overwriting user content. `oms project-private`
keeps `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, and `PROJECT.md` out of commits via
`.git/info/exclude`; tracked files require an explicit untrack action.

```bash
oms init
oms apply-project-template general .
oms project-doctor .
oms project-private --check
```

## Local skills and optional bundle operations

Skills load in three layers: general-purpose skills everywhere,
machine-conditional skills only where their command exists (`oms-slurm`,
`oms-gpu-workstation`), and per-repo skills forged into `.oms/skills/`. Global
skills are named `oms-*` so they cannot collide with your own skills in the
shared skill roots. Newly forged project skills are also `oms-*`; stored legacy
names remain readable and are not renamed automatically.

Use `oms skill-forge add|validate|link` for ordinary local guidance. Neither
external bundle management nor evaluation is required for a local edit.
`oms skill-forge status` gives kind-specific recovery guidance for skills that
differ from same-name OMS templates; local replacement warns that local edits
will be lost, while imported updates remain explicit.
`oms skill-forge` remains the single project-skill authority. Local authored
skills live under `.oms/skills`; explicitly imported bundles first pass a bounded,
no-follow preview and then live as content-addressed immutable revisions under
`.oms/skill-store`. Import, update, and rollback require explicit apply plus
digest CAS. They reject links, hardlinks, nonregular entries, sensitive file
names/content, embedded URL credentials, oversized trees, and an unprefixed
skill name. Import never executes a bundle script.

```bash
oms skill-forge preview --source PATH_OR_GIT --json
oms skill-forge import --source PATH_OR_GIT --expected-bundle-sha256 SHA --apply
oms skill-forge update --source PATH_OR_GIT --expected-current-sha256 OLD --expected-bundle-sha256 NEW --apply
oms skill-forge rollback oms-NAME --to SHA --expected-current-sha256 CURRENT --apply
```

Skill evaluation is an explicit black-box experiment. A suite declares near-
miss trigger cases and baseline/treatment task commands; host commands run only
with `--allow-host-commands`. The result contains aggregate counts and command
digests, never prompts or outputs. `--record` appends content-free metrics that
`oms runtime benchmark` projects. Write useful guidance directly from reviewed
source evidence, then use `skill-forge add|link` for local skills or the
preview/import flow for bundles. Automatic draft generation is not required.

## Asking other agents

| Need | Front door | Result |
|---|---|---|
| One independent read or continued peer thread | `oms consult [--to PROVIDER]` | Threaded answer |
| Council or debate | `oms peer-ask` | Per-seat artifacts and family count |
| Diff review | `oms peer-review --gate` | Verdicts plus mechanical backstop |
| High-risk decision | `oms advise` | Adversarial recommendation |

Blocking review gates default to 192 KiB; set `OMS_PROMPT_DIFF_BYTES` to raise the
complete-diff budget, increasing prompt usage and provider cost.

Debate rounds after the initial independent answers share a 64 KiB prompt
budget across all seats (`OMS_DEBATE_ROUND_BYTES`). Each fresh call receives
section-balanced excerpts of current claims/findings, evidence, changes and
remaining disagreements, including its own position. Structured excerpts use
at most 4 KiB each; freeform and undersized section budgets use a bounded
head-and-tail excerpt, retaining the leading conclusion and final objections.
Original artifacts remain linked, and omitted content is marked rather than
presented as a complete summary. All prompts are prepared before any seat is
started; a frame too large to retain evidence fails without partial calls.
Within a round, identical artifact/quota pairs reuse the sanitized excerpt;
different quotas and later rounds are processed separately. This reduces local
processing, not the number of model calls or the prompt's token count.
Round-byte reporting excludes provider-native context, reads, retries and
synthesis, and must not be reported as measured token savings.
An agy print-timeout diagnostic is incomplete output even with exit status zero;
it cannot count as an answered seat, enter a later round, or pass a review gate.
Excerpts recognize plain and Markdown section headings without treating fenced
or indented code and `>`-quoted examples as new answers; short sections donate unused space
to longer ones. Early stopping requires every called seat to answer and its
entire change section to explicitly declare no change. A dropped seat's last
good answer remains available, labeled as stale in the synthesis.

Councils with `--thread ID` publish every completed turn without waiting for
slower seats. Opening answers and rebuttal rounds remain parallel, with one
prior-round evidence/notes snapshot and all-seat budget preflight per round.
Coordinator notes/decisions use `thread context --notes-only` (2 KiB, eight
turns); answers and provider failure notes are not duplicated there. Notes
arriving during a round apply only if another configured round runs.
MCP `kind=ask` returns a ready thread and accepts `debate_rounds=0..3` (default
zero); its suggested operation wait is zero while exchanging messages.
Threads stay open for authorized focused follow-up through `consult`; neither
notes nor reads start new calls. CLI export-only does not create/publish turns.
Closing a council rejects new CLI/MCP council starts and its next configured
round before provider dispatch. Ordinary history reads remain available;
already-running calls are not interrupted. `thread context --require-open`
is the shared preflight, not a lock across an entire provider call.
There is one parallel council, not a separate sequential scheduler, persistent
provider session, token stream, or mid-generation interruption.

Claude read calls retain public assistant text from `stream-json --verbose`
alongside the terminal result, so a closing remark cannot erase earlier
evidence. User/tool/thinking/subagent events are not answers; absent terminal
results are marked truncated. Existing JSON envelopes and write/interactive
transports remain supported, with unchanged permissions. The transport follows
the [official streaming contract](https://code.claude.com/docs/en/headless#stream-responses);
fixture validation is not a live-provider guarantee or measured cost saving.
Initial and subsequent debate prompts share evidence/verification framing,
while honoring explicitly requested answer formats. Claims use stable IDs and
provider-reported confirmed/refuted/unverified labels; agreement never promotes
them to owner-verified evidence. Retractions must accompany subsequent claims.
Each run keeps a bounded, sanitized initial request and answer copies, mirrored
into isolated read worktrees. Tracked base/state identity is shared and a tracked
source change stops later rounds. Base plus supplied diff is the shared view;
untracked or omitted bytes remain explicit uncertainty, not an identical full
checkout promise. The copies are removed after the run; original artifacts stay.
Usage details retain input/output and cache-read/write counts with unknowns as
null. Codex cache reads are included in its input count; Claude reports caches
separately. Historical `tokens used` semantics remain unchanged. Details are
cached in the existing artifact index, not a new store or a billable-cost claim.

Read calls scan outbound context for credentials and machine-sensitive data.
`--export-only` writes a prompt without invoking another CLI; import the answer
through `oms artifact-index import`.

The artifact index is a repo-bound, no-follow, copy-on-write JSONL store.
Normal views and mutations reject structural corruption. Agent recovery is
two-step: `oms artifact-index salvage` plans without writes, while
`salvage --apply` preserves the exact raw bytes in a content-addressed 0600
quarantine before one CAS repair and receipt. Legacy schema repair remains the
separate `migrate` operation. A row over 1 MiB or recovery snapshot over 256
MiB is refused without mutation. A write that would exceed the 16 MiB healthy
ceiling first applies lineage-aware retention and refuses only when its required
new rows still cannot fit; salvage uses the same output ceiling. Its receipt
references the raw quarantine, so ordinary artifact retention preserves it
until that receipt is pruned.

Provider count is not independence. Two routes backed by the same model family
remain one opinion for diversity reporting.

Provider discovery separates an executable transport from its model family.
`glm` is therefore never guessed as a provider: a GLM route names its carrier
and exact model, for example `opencode:model=zai/glm-4.7`. Likewise,
`deepseek` names the official DeepSeek Harness transport, while a DeepSeek model
behind Aider is expressed as `aider:model=deepseek/deepseek-chat` (or the exact
model name configured by that carrier).

`oms models --providers auto` is a no-exec physical inventory; its `usable`
field is unknown until `--refresh`. Refresh and `model-doctor --providers auto`
run bounded local version/help probes without inference, login, install, update,
or configuration changes. A visible but broken shim remains in diagnostics as
`broken`, while automatic routing admits only a probe-proven usable transport.
Optional agents do not become default council seats merely because they appear
on PATH: a custom adapter is excluded from every automatic pool (advisor
auto-pick, consult auto-pick and failover) and runs only when named with `--to`
or under an explicit `consult --all` fan-out.

Provider-native user, organization, and previously trusted project
configuration remains part of the trusted host boundary. OMS pins the
documented noninteractive mode, tool/permission surface, and outer worktree
fences, but those flags are not a substitute for reviewing native hooks,
plugins, MCP servers, credentials, or managed policy loaded by the provider.

Custom transports must be executable as `oms-agent-adapter-ID` on PATH or in
the configured adapter directory. Their protocol is a prompt-file based
`run --access read|write --workdir PATH` call. Read calls use a disposable
worktree. Write is refused unless `ID` appears in
`OMS_PROVIDER_WRITE_ADAPTERS`, then still returns through the normal isolated
delegation and patch gate. The adapter remains a trusted host executable, not
an OS sandbox, and carries no landing, commit, push, or publication authority.

The [terminal panel](TERMINAL-PANEL.md#additional-clis-and-adapters) reuses this
registry for its read-only CLI inventory and explicit `--dispatch --to` routes.
Installing an adapter does not require panel-specific provider code and does
not opt it into preset routing or native session messaging.

A provider subprocess is a harness child, not a new owner. Child-marked
processes may inspect saved review verdicts, but every peer-call and delegation
front door refuses a recursive start. The child reports that another opinion or
worker is needed and the parent decides whether to spend or fan out.
Global or explicit-file memory source/derived-state changes, global role
creation, journal configuration/sync, model-capability refresh, and host queue
enqueue/cancel are parent-owned as well. Pure inspection and project-local
counterparts remain available where applicable.

## Models and reasoning effort

Routing is deliberately small:

- No `--model`: judging calls use the provider default; write workers use the
  second seeded price rank among routable models. An explicitly declared
  `--workload routine` selects the lowest seeded routable rank. Unseeded
  generations use the provider default; `OMS_ROLE_ROUTING=0` disables presets.
- `--model NAME`: use that exact model with no implicit switch.
- DeepSeek Harness and Vibe currently expose no documented per-invocation model
  selector on their OMS headless surfaces. An explicit `--model` for either is
  refused instead of being silently ignored; configure the native profile or
  omit the flag.
- `--fallback-model NAME`: on a recognized capacity error, retry once with
  that model. A write attempt that changed its worktree is never retried.
- An unpinned provider-default route may use bounded catalog recovery for a
  model safeguard or unavailable-name error.
- Recognized policy declines are terminal. Authentication, permission, context,
  and verification failures are not model-capacity fallbacks.

`--reasoning-effort` accepts `auto`, `low`, `medium`, `high`, `xhigh`, `max`,
or `ultra`, then validates the value against the selected provider/model
capability snapshot. `oms models` reads the cache; `oms models --refresh` and
`oms model-doctor` perform explicit probes.
Cached catalog reads separate tab-delimited model IDs from display labels and
discard listing banners, so labels cannot leak into automatic recovery routes.

Artifacts record requested and selected models, reasoning effort, fallback use,
and reason.

The [model-routing contract](../custom-skills/oms-agent-harness/references/model-routing.md)
separates parent judgment from bounded execution and defines decision-point
reviews. Generic role prompts do not select a model or grant delegation authority. Native
subagents remain outside the shell router.

### Collaboration visibility

The [terminal control panel](TERMINAL-PANEL.md) is the human entrypoint: bare
`oms` on a TTY or `oms panel`. It opens Codex/Claude native sessions, resumes
explicit session IDs, and connects either owner to the same consult/delegate/
review paths. Optional installed tmux keeps native conversations beside shared
status; the portable inline menu works without it. Provider workers do not gain
owner or recursive delegation authority. The panel adds an explicit role policy:
Sol/Opus mains, Haiku/Luna light research, Sonnet/Terra routine implementation or
explanation, and Astra/Fable advisors and reviewers. The native main judges the workload and dispatches
through existing front doors. Separate advisor/worker sections show recorded
model, task, authority and execution location, bound to the native main's
lifecycle attempt. Generic provider defaults stay separate from this policy.

Use compact status during ordinary work and drill down when coordination or a
blocker needs explanation. `oms dashboard` composes existing state, lifecycle,
artifact and inbox projections into a read-only terminal view. `--watch`
refreshes the view and `--json` returns a bounded schema-1 projection. There is
no second state store or model call to render it. The retired `ops-cockpit`
remains retired. See [the dashboard guide](DASHBOARD.md) for coverage and controls.

An illustrative layout, not a live run:

```text
Goal / requested scope
Parent: planning | execution | integration | verification
Task       Owner / selected model   Route reason        Observed state
lookup     local tool               known path          evidence returned
implement  worker / recorded model  routine assignment  ready for review
Acceptance: required check / exact tree / pass, fail, missing or stale
Attention: blocker / review finding / next decision
```

Display an owner, model or route reason only when recorded; otherwise mark it
unknown. Identify snapshot time and evidence freshness. Distinguish agent
completion from integrated verification, and review not requested from a
passing review. Tokens, costs and durations need a measured source and scope;
absent values are unknown, not zero. Do not expose raw prompts or transcripts.
Session-native HUD data and OMS-managed attempt records have different
coverage; a view must say which it observes rather than implying that every
native agent is tracked. Viewing state grants no execution or publication
authority.

## Delegating writes

Choose `oms consult` (or the lower-level `agent-call`) for a read and
`oms peer-delegate` for a patch. The `agent-run` wrapper and keyword mode
classifier were removed. Task-aware calls/delegates record outcomes directly;
existing artifact and task history is preserved.

A write delegation:

1. Creates a detached worktree from the current `HEAD` under the private
   `${XDG_CACHE_HOME:-$HOME/.cache}/oh-my-setting/worktrees` root (override
   with `OMS_DELEGATE_WORKTREE_ROOT`).
2. Injects the brief, optional role, and bounded context.
3. Runs the provider and the declared verification command.
4. Returns an artifact and binary-safe patch; it does not commit or push.
5. Optionally uses bounded repair rounds with the prior failure attached.

Attached review/repair findings use bounded, sanitized reference quotes: local
paths are masked and sensitive lines redacted without rewriting the original
evidence. Long reviews keep their opening findings; unterminated final lines
remain readable. The operator brief still passes the strict outbound guard.

Explicit `--verify` and plan-task checks win. Otherwise writes default only to
an implemented `scripts/check.sh fast`; ML project detection does not select a
heavier suite. A check script without `fast` requires a chosen verification
command or an explicit `--no-verify`, before graph work or provider startup.
Review and patch admission use the same selector. Admission has no implicit
full-suite fallback or no-verify override; choose an explicit command when
`fast` is absent. An explicit ML preset still selects an available `ml-smoke`.
Read-only reports do not auto-run tests. Missing or blocked providers skip
verification with a non-success result, rather than test unchanged code.

With `--repair 2|3`, compact candidate/diagnostic hashes and exit codes go into
the existing artifact-index `repair_replay` field. Replay compares the requested
budget with stopping consecutive identical failures; single-repair runs skip
this overhead. Selection requires 12 distinct prompts with the same base SHA,
selected model/effort/provider, verifier command and cap. Latest complete baseline
runs form chronological two-thirds development and one-third held-out sets;
both must save calls. Any recorded success lost by early stopping, including
duplicate-prompt reruns, vetoes selection. After four challenger runs, the next
run retains the original cap to replenish uncensored evidence. These thresholds
are OMS heuristics, not paper results or statistical guarantees.

Reads are bounded to 1000 rows/2MiB. Censored failures, sparse/invalid history,
unknown models and fallback routes cannot justify early stopping. A stop remains
a failure. `OMS_REPAIR_REPLAY=0` retains the cap and records outcomes without
reading history; `oms artifact-index telemetry` reports recorded/delegation
counts, selections and stops, not a measured token or wall-clock saving.
No extra run, timer, model, test omission or automatic delegation is introduced.
Explicit repeated-failure advice bypasses replay: a different agent contributes
to that trajectory and its cost, so single-worker evidence is not comparable.

This is a narrow adaptation of [Dream-RSI §3](https://arxiv.org/html/2609.14858v1#S3),
not a reproduction of its full exploration system: two fixed stopping policies,
no generated policy code, no concurrency optimization. The [upstream repository](https://github.com/zhengkid/Dream-RSI)
listed its full implementation as pending on 2026-09-15. Historical non-regression
does not guarantee future task success or equivalent environments; a changed
base/verifier/model cohort starts without transferable evidence.

The worker does not see the caller's uncommitted changes. `patch-admit` checks
applicability, path scope, sensitive content, syntax, and the stored test
contract in another temporary worktree. If a patch changes its verifier,
tests, or common verification config, admission also runs the same command in
a second projection with that verification surface restored from HEAD.
Conventional repo-owned check/test/verify/lint helpers are included even when
the top-level command does not name them directly. A failed pre-verification
policy/integrity gate skips project-code execution; an explicit override opts
into the corresponding candidate run.
`patch-land` is the single mutation boundary and requires a clean main tree
(including untracked files) plus lease checks. A policy can require a one-use,
exact-action approval; `--request-approval` creates the request without applying
the patch.

Write providers receive task context but not the primary `.oms` pointer,
or attempt/lease capability. The default guard preserves
parallel agent state, rejects destructive rewrites, and binds the selected
task/lease objects exactly while other tasks may move. On a
violation the failing run also repairs the operation's own authority from its
hash-verified pre-launch snapshot: scope, verifier, and review evidence always restore, while claim-cycle fields restore
only under the operation's own lease and are otherwise kept and named. A
caller that guarantees no sibling writer may set
`OMS_WORKER_AUTHORITY_EXCLUSIVE=1` for complete authority-state comparison and
rollback. Plan landing and completion require the reviewed patch bytes,
verifier and lease to match exactly inside the
plan lock. The delegated checkout's physical identity and both Git
backpointers are rechecked
at every mutation boundary, including cleanup. Landing terminal rows close a
transaction only when their canonical receipt and durable plan, approval, and
lineage outcome also converge.


New native landing captures reserve a generation before writing payload bytes.
`OMS_LANDING_CAPTURE_COUNT` (default 256) and `OMS_LANDING_CAPTURE_BYTES`
(default 134217728, 128 MiB) cap newly managed generations and their reserved
peak payload bytes. Each input is limited to 16 MiB; the reservation charges both
the private admission input and its separate publication copy. Hardlinked
private anchors do not duplicate payload bytes. Admission failures, interrupted
publication and unknown reservations remain charged. A full budget refuses a
new capture before it writes bytes; it does not delete protected evidence to
make room. These limits cover new managed payloads, not legacy files, journal
metadata, externally modified files or total historical storage.

`oms gc` preserves legacy, unknown and referenced history. On supported POSIX
hosts it can reclaim an aged, unreferenced native generation only after checking
the private directory and retained inode anchors, the actual writer's terminal
seal, plan/completion/report/index pins, and quiescence. Plan `review`/`finish`
refuse a managed capture reference unless the plan file's replacement destination is under
a scanned root (`.oms/plan`, `.oms/artifacts`, `.oms/tasks`, `.oms/task`), and a plan file
other than the default `.oms/plan/tasks.json` also holds the default plan lock that
collection takes. Collection holds the
landing, marker, plan, lifecycle and artifact-index writer locks in that order.
A live panel main session does not itself prevent collection; marked reference
writers still prevent it until terminal, and other live attempts remain pins.
It first quarantines a shared pathname inside the private generation directory
and checks its inode again. A replacement is restored without overwriting a
new occupant, or preserved in quarantine; it is never deleted as native data.
Interrupted owned quarantines can resume. Unproven reservations stay charged.
Windows collection preserves captures until private ACL and quarantine semantics
can be proved; quota and backpressure still apply. Filesystems without native
hardlinks refuse new managed publication. Ordinary durable-file readers retain
their single-link rule; only designated landing readers validate native anchors.
The private generation directory is a cooperating-OMS ownership boundary, not
an OS isolation boundary against hostile code running as the same user.

```bash
oms peer-delegate --to codex --prompt "Implement the bounded change."
oms patch-admit --patch path/to/change.patch  # optional read-only preview
oms patch-land --patch path/to/change.patch   # runs admission, then mutates
```

## Security boundary

The harness provides isolation and post-run detection, not a complete process
sandbox. It detects repository, git metadata, ignored-file, hook, and shared
state changes around a worker. It cannot prove what a process read, see a write
that was undone before exit, or contain writes outside the repository. An
ordinary provider/verifier shell can leave a background process. On POSIX,
`agent-supervisor` owns and closes one process group; a process that deliberately
starts a new session can escape it. On Windows, a kill-on-close Job Object owns
the launched process tree. Neither boundary is an OS sandbox.

Treat a clean worker result as “no monitored repository surface moved,” not
“the process was harmless.” Stronger guarantees require OS/container isolation.
The default provider permission profile remains practical rather than strict;
outbound scrubbing, isolated worktrees, admission, and verification are the
main controls. Provider and verifier wall clocks use `timeout`/`gtimeout` or a
POSIX Python fallback; without either mechanism the call is refused.

## State, memory, and handoff

Project state lives under ignored `.oms/` paths:

| State | Purpose |
|---|---|
| task and plan | Current goal, acceptance, DAG state, leases |
| memory | Scrubbed shared notes with provenance |
| artifacts | Calls, reviews, patches, verification, lineage |
| failures | Open/resolved failure fingerprints |
| threads | Cross-provider conversation history |
| handoffs | Bounded pre-compaction and session summaries |
| work-journal | Daily local activity and optional Notion mirror |

`oms state` summarizes the repository; `oms inbox` ranks items needing
attention. `oms fail-ledger list`, `oms artifact-index unresolved`, and
`oms thread list` expose the underlying records. Writers are append-oriented;
use migration and GC commands instead of editing JSONL by hand.

Stop materializes the journal locally. Its detached publisher starts only if
a Notion target is configured or GitHub CI polling is available for this repo.
Existing content hashes, sync locks and CI polling intervals deduplicate work
against scheduled maintenance; hosts without a timer keep the Stop fallback.
Native telemetry registers only session boundaries, which live peer discovery
reads; per-tool and SubagentStop registrations were retired, and metricless
activity from an old registration is still dropped. Explicit failures remain; set
`OMS_TELEMETRY_DEBUG=1` only when empty activity is useful for diagnosis.

`.oms/` and project agent files are clone-local by design. A fresh clone needs
the template/private setup again; continuity is not silently copied between
machines.

Concurrent collaboration uses the same thread log: `thread new --live` opts a
scoped conversation into existing prompt/edit-hook delivery, `append` posts a
question or change, `updates --after CURSOR` reads complete new turns, and `ack`
records explicit consumption (not approval). Each hook session retains a small
delivery cursor, never a second transcript. Worktrees can address the canonical
log explicitly with `--repo`; no cross-machine sync or forced live-model
interruption is implied. MCP reuses `oms_peer_start` message/ack modes and
`oms_peer_result(thread, after)` without increasing its 12-tool core catalog.
Operation reads optionally wait up to 50 seconds (`wait_seconds`) for completion
inside the existing server; log activity does not wake that wait. Default reads
remain immediate and expiry never cancels or restarts the peer. The host timeout
must exceed the wait; this serial stdio server cannot handle another request
until it returns.
Start responses include `result_arguments` for that bounded read of the same
operation. Install/update distributes the shared waiting guidance through the
existing global-rule and skill links; it does not add polling hooks or overwrite
user-owned Codex `developer_instructions`. Configured ceilings permit longer
waits but do not enforce agent behavior or prove usage savings.
Operation results return a content `cursor`; passing it back as `after` omits
unchanged answer/log bodies while retaining status, exit and artifact references.
Omit it to reread the full bounded result. Elapsed time alone does not invalidate
the cursor; changed visible evidence, completion and failure do. This is stateless
conditional delivery, not automatic completion notification or acknowledgement.
Worker prompts omit the model catalog: model choice stays with the authorized
parent through `oms models`, not with non-delegating workers.
Read-call thread views omit an exactly repeated final question already supplied
separately; stored turns and earlier decisions stay intact. Repair briefs retain
the task and required context, but patches over 4 KiB are read from the preserved
staged worktree instead of embedding another copy. Artifact telemetry records
primary `prompt_bytes` separately from reported tokens and wall time; provider
context and retry/repair inputs are not included in this byte metric.
The detailed operating contract is in the harness state-memory reference.

## Plans and bounded autonomy

`oms plan-from-spec` proposes tasks from an active `PROJECT.md`; it refuses a
draft spec and does not alter the plan until the proposal is explicitly
applied. `oms plan-run` claims and executes at most one ready task. `oms
goal-drive` repeats within a cycle cap and acceptance command; the cap counts
task attempts across resumes of one autopilot contract, bounds attempts rather
than provider cost, and an exhausted contract is retired with `oms autopilot
abandon`. It is not an unbounded autonomous loop.

`oms plan-from-spec --discover` collects a bounded packet of failures, panel
metrics, undecided review findings, CI history, open plan tasks, and rejected
hypotheses. It fails closed on corrupt or over-budget evidence. The planner
proposes evidence-cited candidates through imagination, gap-critique, and
pruning lenses; speculative candidates remain report-only. A run with no
eligible work creates no proposal and exits successfully. Otherwise it prints
the proposal, evidence packet, and candidate report paths with their SHA-256
digests. With `--collaboration auto`, the council reviews the contract without
the packet. Applying a proposal requires `--expected-proposal-sha256` for the
reviewed proposal bytes. `oms autopilot propose --discover` uses discovery for
the initial tranche only; when no eligible work is found, its receipt remains
at proposing so the attempt can be retried.

Before landing,
`goal-drive` freezes the reviewed
patch and appends a commit intent. A restart reconciles an unapplied, applied,
or already committed intent without calling the provider again. Publication
builds the frozen tree in a private index, creates the commit without repository
commit hooks, and compare-and-sets `HEAD` against the recorded parent while
holding Git's index lock. A staged index that differs from the frozen base is
preserved and parks the run instead of being overwritten. Repository hooks are
disabled for every child Git operation in the drive, not only the final ref
update. Frozen patch creation enters the validated physical directory, uses
relative writes, and atomically replaces the final leaf without following a
directory symlink; a changed absolute lookup parks before consumption.

`agent-plan status --json` is the canonical read-time plan decision. Its
additive `contract` object compares the reviewed `project_contract.spec_sha256`
with one bounded, no-follow snapshot of `PROJECT.md`, and its `actionable`
list already includes dependency, claim-expiry, and contract gates. A bound
plan whose project file is missing, draft, invalid, unreadable, or byte-drifted
cannot issue a new `ready`, `next`, or `claim` authority. Exact transitions for
an existing lease remain available so drift cannot strand running work or
prevent cleanup. Legacy plans without a reviewed contract keep their existing
claim behavior. Runtime, state, and inbox consume this decision rather than
reconstructing readiness from stored task states.

For reviewed tasks integrated together by commit, use `oms agent-plan finish
--id ID --landed-commit SHA [--repo PATH]`. This requires review
artifact evidence, retained from `review` or given here with `--artifact` (a
`--patch` is recorded when supplied, never required), an exact-SHA `oms land` receipt with state `passed`,
successful gate/push/CI, and SHA reachable from the receipt's recorded local
remote/target ref; it performs no fetch. A task's current lease must match
when one is given, and a task with allowed paths needs the landed range, from
the nearest earlier pushed receipt (else the first parent) to SHA, to change at
least one of them, so a receipt for unrelated work cannot finish it. The task retains the commit and receipt
file SHA-256 as landing proof and counts as done in plan status and the panel.
`patch-land` remains the default front door for single patches. It passes
`--expected-landing-receipt-sha256` to `oms agent-plan finish` internally; this
is a `finish` argument, not a `patch-land` option.

Receipt discovery stays clone-local (linked worktrees share their canonical
namespace). When an authorized main lands from a separate clone, it can use
`oms land --handoff-receipt-to /path/to/shared-checkout` to import the producer's
exact receipt bytes into that checkout's canonical namespace. The target must
have the receipt's remote name configured to the same URL as the producer's
push destination; equivalent network URL spellings are not inferred. Handoff
fetches only that remote's destination branch and verifies the landed SHA is
reachable; it leaves the target HEAD, index and worktree untouched. It requires
a passed land with successful gate, push and CI; skipped or failed CI transfers
nothing. Any required installation stage must also have finished successfully.

The nearest pushed first-parent predecessor receipt is transferred too, so the
landed range stays the same. Existing target receipts that would change that
boundary cause refusal. Imports publish complete files atomically, with the
primary receipt last, preserve SHA-256 digests and refuse different bytes at an
existing filename. Identical bytes are idempotent. A failed handoff exits nonzero
and leaves the producer receipt intact. To retry only the handoff, use the same
commit, destination and gate with `--no-update --handoff-receipt-to PATH`. This
reuses the completed receipt without changing its bytes or requesting an
installation. The landed SHA must remain reachable from the fetched destination
ref; another main may have pushed a descendant in the meantime. Invocations
without `--no-update` run the normal CI/install steps and retain the ordinary
exact-tip safety checks before handing off the resulting receipt. A changed
receipt still cannot overwrite different bytes already imported at the same
filename. After successful handoff,
`oms agent-plan finish --repo /path/to/shared-checkout --id ID --landed-commit SHA`
uses the existing completion checks without an additional import or fetch.

To refresh panel boards after landing from another clone, use `oms sync-shared
--repo /path/to/shared-checkout [--remote origin] [--target main]` instead of
`git merge`. Calls register a shared lock before canonical snapshots and retain
it through final guards and receipts; sync takes the same admission gate
exclusively through fetch and `merge --ff-only`. It returns exit 75 with
`deferred: N calls running (ids...)` while calls remain, and refuses dirty
(including untracked files), detached, ahead or diverged trees; any local branch may follow the target.
New calls wait at most five seconds for an active sync, then refuse safely;
retry them after sync finishes. Dead PID/start identities are reclaimed,
while incomplete or unprovable lock owners continue to block. This refresh
does not push, install or supply landing evidence.

To request this refresh as a separate step of landing, add
`--sync-shared /path/to/shared-checkout` to `oms land`. PATH is required and
independent of `--handoff-receipt-to PATH`; use both to transfer landing evidence
and then refresh that checkout. Sync runs only after land passes and any handoff
succeeds. Deferred (exit 75) or refused sync leaves land passed with exit zero;
the land log and text status show `sync deferred: reason` or
`sync refused: reason`. Retry the same command when the checkout is idle and
clean. Omitting the flag preserves the target HEAD, index and worktree during
handoff.

The refresh outcome is stored in `sync/<request>.json` beside the producer's
landing receipt, with `sync.state` (`running`, `passed`, `deferred` or `refused`),
path, exit code and reason. `oms land status` includes this companion's `sync`
fields in JSON and text output. Keeping this mutable operation separate preserves
the original landing receipt bytes and handed-off digests, including on retries;
the companion is not imported as landing evidence.

The change guard sends its sorted changed-path stream through the canonical
scope engine once. The engine loads and validates the stored allow/deny rules,
compiles each bounded Bash-compatible glob once, and emits one deny-first
verdict per path. The public single-path scope operations remain compatible;
the batch operation is an internal performance boundary for large diffs.

An active plan that became stale after equivalent work landed elsewhere is
removed through `oms agent-plan retire`, never by inventing task transitions.
The default command is a no-write check that returns the exact plan SHA-256.
Apply is parent-only and requires that SHA, a disposition, and a reason. For
`completed-external`, the command itself runs a fresh acceptance on one clean
committed HEAD and binds full plan/acceptance/output hashes, ref, file
generation, and a unique proof token. `superseded` is limited to unclaimed old
plans and explicitly claims no verification. Both preserve the exact bytes in
one disposition-independent content-addressed archive and append a typed
receipt before unlinking the exact live generation. Claimed, running, review,
landing, malformed/unproven markers, or a live task marker veto retirement.
Authority JSONL is strict LF/CRLF-delimited finite JSON with bounded rows, and
every receipt is cross-linked to its archive and, when claimed, its unique
acceptance row. Replaying the same operation completes an interrupted unlink;
a different nonempty plan lineage is untouched, while same/ambiguous lineage
fails closed. Canonical plan mutations also veto that same-lineage residual.
After unlink already completed, `init`/reviewed proposal apply may remove the
validated intent before creating a new plan. `state` and runtime expose the
latest receipt as display context; `state-verify` is the trust surface that
rejects malformed/duplicate receipts, proof or task-state contradictions,
residual intents, and active/retired lineage conflicts.

With `--collaboration auto`, initial planning and the bounded remainder proposal
trigger a GPT-6/Claude council with one rebuttal round (Codex defaults to Astra
or the explicit Codex planner model). Both seats must
finish and meet `--deliberation`'s per-seat response contract: alternatives,
evidence, counterargument, verification and explicit deltas/disagreements.
JSON-only task lists or empty sections fail closed. A seat whose answer is
otherwise complete but lacks required headings is asked once, in the same
round, to restate its own answer under every heading; the restatement is used
only if it passes, and both answers stay in the artifacts. The council receives only
contract data; JSON decomposition instructions stay in the planner call.
Structure is checked mechanically; reasoning still needs parent judgment.
Planner reasoning effort applies only to decomposition; the council keeps
provider defaults, disclosed before dispatch alongside its retained log path.
That path remains available for parent inspection after early child failure,
even when the child did not emit its own summary.
The bounded synthesis feeds the planner as untrusted evidence before
normal exact parent proposal review. The durable contract retains this mode
through resume and enforces gated opposite-provider review. Codex routes are
pinned to a GPT-6-or-newer version of Astra, Sol, Terra or Luna, including reviewed assignments and optional
capacity fallbacks. Claude Code can implement with `--worker claude`, leaving
Codex as the reviewer; each campaign keeps one implementation transport to
preserve independent review. The existing provider-neutral behavior is unchanged
when the option is off. No background daemon or publication permission is added.

A peer review emits a `review-scope` artifact binding its base, HEAD, exact diff
digest and paths; external parent recovery patches are explicitly not covered.
The parent must obtain a separate or combined review of those exact patches
before declaring the whole request complete. Campaign scope remains immutable.

Acceptance stays fresh by default. An explicit `- Acceptance reuse: same-run`
in PROJECT.md's Verification section asserts snapshot-pure checks and stable
external tools. Only then may the immediate post-drive check reuse the latest
supervised pass from that same run, for at most ten minutes from acceptance start, with matching
command, plan/spec, manifest, repository snapshot, runner and environment.
Missing or stale evidence runs fresh. The review verifier, post-review check
and publication gates remain fresh; reuse neither creates a new pass nor
refreshes the original receipt. Network, time, hardware, filesystem metadata other than modes, and mutable
ignored inputs are ineligible. This reduces four acceptance executions to three in an
unchanged local campaign; it does not claim a model-token or wall-time saving.

`oms autopilot` is an agent-side control plane: the end user states the goal and
authority while the top-level parent performs every transition below. It
proposes an initial plan for parent review, atomically applies only that exact
proposal, drives the approved tasks, and may propose one `r1-` remainder tranche
of at most two tasks. The
mechanical acceptance command remains the hard gate. A non-pass acceptance persists its bounded, normalized output body under `.oms/plan/acceptance/` keyed by the receipt row's output digest. Custom acceptance commands
bind their declared `Required check files` and reject verifier mutation; Draft
PR publication defaults cross-family semantic review to a blocking gate. Its
base is frozen to a commit before the drive, so a moved branch name cannot make
the whole-change review empty. Each drive ends with one unique canonical result
that binds its internal receipt, status, and reason; the durable terminal row
must match it before the remainder tranche is authorized. `propose` requires
the base; its printed continuation is shell-safe, retains every effective
option, and accepts only a regular non-symlink proposal snapshot of at most
1 MiB.
`oms intent adopt` likewise validates one frozen candidate, then publishes only
that snapshot after a locked SHA-256 and byte-for-byte comparison with the live
candidate; an editor save during acceptance leaves the draft intact and creates
no `PROJECT.md`.
With `--draft-pr`, `oms draft-pr` rechecks the clean HEAD, tree, remote base,
GitHub identity, write permission, and verifier before a create-only push and
Draft PR. Every introduced Git object, including trees, is scanned for
credential-shaped and machine-private content; defaults cap this at 20,000
objects, 32 MiB per object, 256 MiB total, and a 180-second scan budget with
fixed termination escalation; Git history traversal also runs under a 512 MiB
process-memory ceiling. Hosts without both controls park before traversal.
The bounded history scan shares admission's public-configuration filter, scans
raw bytes in the C locale, and distinguishes filter errors from clean input.
Temporary payloads are removed before parking.
Hidden/sparse index entries and Git grafts are refused. Its local intent records
a pre-push uncertainty phase, and verifier mutation terminally spends that
intent, so a deleted branch is not recreated
after an interrupted or tampered push; it cannot update an existing remote
branch, merge, mark ready, tag, or release. Starting on the base branch
creates a deterministic local `oms/autopilot-<spec-digest>` branch before the
first drive cycle, with or without Draft PR publication; any other checked-out
branch parks rather than being silently driven. Recovery branches use strict
`-rN` names, and the complete committed diff from the reviewed base must stay
inside the plan envelope. That selected branch is frozen through review, and a
base-branch restart parks when a matching recovery branch already exists
instead of creating a competing lineage. A failed or ambiguous
push spends that intent; retry from a new branch name rather than risk replaying
an effect whose absence cannot be proven permanently. The two publication
recovery cases: an interrupted but unspent intent replays with
`oms draft-pr --repo REPO --intent INTENT publish`; a spent or terminal intent
never replays. Repeated preparation prints that exact, shell-safe replay command
only for an unblocked regular intent; otherwise it parks without advertising
publication. For a spent intent, rename the work branch once
(`git branch -m oms/autopilot-<digest> oms/autopilot-<digest>-r2`, which the
checkout guard accepts); the parent then resumes review and preparation on the
new name. This recovery procedure is agent-facing, not an end-user handoff.

Task-specific behavior belongs in the brief, not a separately generated Soul.
Soul executors and their command are removed. Existing records and bound reviews
remain readable evidence but cannot authorize new execution or landing; create
a fresh reviewed task without editing away the old binding. GC preserves legacy
executor markers instead of attempting to resume or retire their contracts.
Default state/envelope queries no longer scan or advertise these retired files.
Normal task leases, scoped admission, one-use approvals and one-shot repair remain.

`oms autopilot --max-parallel-tasks N` binds an immutable width of 1..4
(default 1); standalone `oms goal-drive` accepts the same flag. Propose/run
continuations and reentry retain it, and older receipts mean width 1. Each
new task attempt reserves one lifetime cycle before dispatch; adopting a
completed review spends no additional cycle or provider call. Width never
exceeds the remaining attempt budget.

Parallel goal-drive keeps per-task worktrees and selects dependency-ready tasks
in plan order under the explicit `--max-parallel-tasks N` opt-in. Intersecting
allowed paths serialize using graph scope overlap checks; an overlapping
candidate is named and skipped while selection continues, so later eligible
tasks can join the batch. Verifier syntax does not decide eligibility: Python,
pytest, npm and make checks can run concurrently. A batch of two or more tasks
prints a start line with width, selected tasks and skips, then a done line with
each task's outcome and the batch's elapsed seconds. If a later claim or the
prepare step fails before launch, already claimed tasks are rolled back;
startup also reports how to release owner claims left without a worker.
With this option the project asserts its checks tolerate concurrent runs.
Worktrees do not isolate shared caches, ports, databases or network services,
or provide resource isolation.

A batch barrier precedes serial native admission and exact commits in plan
order. Each candidate replays the already committed tasks' stored verifiers
from the same validated batch, then its own verifier, on the candidate tree.
Admission reuse binds that derived prefix. A live batch validates its full row;
a retained review from a retired batch replays its original contiguous,
lineage-verified done prefix, ending at a released or re-leased predecessor.
Recovery keeps this replay obligation. Any failure refuses the candidate before apply and
preserves the committed prefix. Ignoring repairs, baseline floor checks and
admission reuse, a width-n batch adds n(n-1)/2 verifier runs (one at width 2,
six at width 4). Goal acceptance stays after the batch. Campaign review
semantics are unchanged. A failed sibling preserves all results, integrates the
successful ordered prefix and closes the batch with its integrated prefix,
released tasks and preserved reviews, then parks with the existing recoverable
prefix contract. A retry uses a fresh batch and the remaining attempt budget.
Without `--retry-known`, retained reviews are adopted before fresh attempts,
including when the attempt budget is exhausted; the retry gate for new attempts stays unchanged.
`autopilot abandon` also retires the old batch; completed reviews remain reusable
without provider calls or charges, including when the replacement contract
chooses width 1. Claimed tasks are released only through exact-lease recovery
with live-marker protection; running or uncertain work is preserved. Reentry
reconciles the existing commit intent first, then the batch's lease-bound reviews.
Every committed prefix tree must match its frozen patch digest. Changed
contracts, leases or commit lineage park with `invalid-batch` and preserve evidence for the owning agent to reconcile.

Each new autopilot receipt also binds an opaque run owner. Claims and worker
markers inherit it. Re-entry recovers only that owner's exact current
`claimed`/`running` leases under the plan lock: live markers, markerless
running work, another owner, and `review`/`landing` evidence are preserved.
Its owner and dead claimant come from one receipt-lock judgment token, never a
later ledger reread. Routine GC CASes the observed state and lease and vetoes
any exact live worker marker under the plan lock, so a retry or task that
advances during cleanup cannot be requeued by stale evidence. Windows liveness
binds the Git Bash PID to its native WINPID and uses a wait-only process handle;
it never probes by sending signal zero through Python. A missing legacy native
identity is preserved as unknown. GC treats markers as bounded, no-follow
evidence and deletes only an unchanged generation under its marker lock. More
than 4,096 marker entries makes the entire scan unproven and preserves every
entry; it never turns a partial enumeration into recovery authority.
Legacy receipts and tasks without an owner stay readable; owner-based re-entry
never guesses them, while exact state+lease GC remains backward compatible.

Recovery tools include:

```bash
oms checkpoint create --label "before risky edit"
oms fail-ledger list
oms artifact-index unresolved
oms artifact-index salvage
oms approval-inbox expire [--apply]
oms approval-inbox reconcile --older-than-seconds 300 [--apply]
oms patch-land --recover
oms gc
```

## Graphs

`oms graph` (`scripts/lib/oms_graph/`, standard library only) adds two
inspectable graphs above the plane described in the previous section; the
contract is `docs/GRAPH-ENGINEERING.md`.

The **Project Graph** (`oms graph project build|check|map|find|api|search|neighbors|trace|blast|affected|context`)
is a deterministic structural graph of the repository — file, module,
class/function/method, test, config, and document nodes with `contains`,
`imports`, `calls`, `references`, and `tests` edges — extracted by
stdlib parsers (Python `ast`, bounded shell regexes, Markdown path
references) without a model, key, or network. Every edge carries
`EXTRACTED`, `INFERRED`, or `AMBIGUOUS` confidence, so inference is never
presented as fact. Deterministic source summaries improve task ranking;
`api` projects signatures without bodies, `search` groups exhaustive literal
hits by enclosing symbol, and overviews report unsupported-extension coverage
instead of presenting an empty graph as complete. Extraction is content-addressed under
`.oms/project-graph/cache/`, `graph.json` carries no timestamps and is
byte-identical for the same working tree, and `check` judges freshness by
working-tree bytes, not by commit. `blast --base` uses the merge base and
handles an unborn `HEAD`; `blast` walks reverse dependencies from
the changed files; `context` produces a bounded task-specific pack (files,
tests, blast radius, hubs, a byte estimate that is never called tokens) and
can compile it through `oms runtime context`. Discovery honors `.gitignore`,
skips symlinks, binaries, oversized and secret-shaped files, and treats
every source byte as data.

The **Execution Graph** (`oms graph exec validate|render|route|run|resume|decide|status|events|test|commit`)
is an advanced option for explicit branching, concurrent graph tasks or resuming
an existing graph run. Ordinary execution stays with plans/goal-drive/autopilot;
the bundled goal-drive graph is an example, not another default driver.
A GraphSpec (`config/graphs/*.json`)
declares `agent`, `tool`, `gate`, `router`, `subgraph`, and `terminal` nodes,
typed semantic outcomes (`completed failed unverified partial blocked
changes_requested approved skipped`, deliberately separate from the plan
task lifecycle), edges keyed by outcome with `repeat` budgets, `join`
semantics, and proof predicates over facts. The validator rejects unknown
endpoints, unreachable nodes, missing terminals, cycles without a stop
policy, ambiguous routes, and recursive subgraphs. `route` is a pure
evaluation over facts (plan state, admission/landing/acceptance receipts,
git) and recorded outcomes: a claimed `completed` whose proof facts are
absent is `unverified`, and no model participates in routing. Runs live in
`.oms/graph/runs/<run-id>/` as a frozen spec, an append-only `events.jsonl`
with idempotency keys, and a derived projection; `resume` rebuilds state
from events and current facts, never from conversation memory. Task identity
is frozen before work starts: `plan_task: "next"` is a selector resolved by
one read-only peek, the concrete id is recorded on the `node_started` row
and, under `bind_task`, as a run-scoped binding that `plan_task_from` nodes
execute exactly (`implement` choosing `t1` means `land` lands `t1` after the
plan's `next` has moved on). Agent nodes execute through
`plan-run --id TASK [--land] [--context-pack FILE]` — the project-graph
context pack an agent node's `context` field builds is orientation for the
worker brief, never authority; the adapter has no path to `agent-plan
land/finish/claim`, landing stays serialized by `patch-land`, and `--jobs N`
runs a scheduler wave concurrently only for disjoint explicit tasks (same
task, unknown or overlapping scope, landing, write tools, and a second
selector serialize). The read-tool cache is keyed by a workspace fingerprint
as well as HEAD and fails closed. `resume` reconciles a crashed node against
the plan — a live lease waits, an expired one is `unverified`, a dead write
tool is `blocked`. `exec commit --binding NAME` commits exactly the bound
task's landed patch so the next landing finds a clean tree. The retired
`exec shadow` comparison is replaced by
`runtime next` for control-plane actions and `exec route/status` for actual
graph runs; existing comparison history is preserved.

## Durable operations and optional frontends

Lifecycle and approval JSONL access rejects linked leaves/direct parents and
checks named/opened file identity. Trace-tail reads, append and POSIX private
permissions use the same verified handle; torn-tail recovery remains intact.
Host ancestry aliases remain supported. This is not a same-user filesystem sandbox.

| Front door | Actual boundary |
|---|---|
| `agent-events`, `agent-supervisor` | Append-only attempt lifecycle and bounded `trusted-local` execution. Resume creates a child attempt; reconcile closes stale supervisor-owned queues that lost their runtime record. The supervisor never lands, commits, or pushes. |
| `approval-inbox` | Private, version-CAS approval outside `.oms`; a patch grant binds the exact base, bytes, attempt when present, lease, profile, verifier hash/mode, ML mode and admission exceptions, then is consumed once. `expire --apply` closes unused grants; stale reservations reconcile dry-run first to terminal `interrupted` with an unknown outcome. Patch landing defers to `patch-land --recover`. |
| `land` | One detached job per landing: gate (in a detached worktree of the verified SHA, so untracked files and the live session's `.oms` writes stay out; `check.sh --affected --ci-parity --changed-from REMOTE/TARGET --changed-to HEAD` in the harness checkout itself, where a full selection runs lint and the quick lane, plus operator-tools when tier-routing inputs change, with every registry provider CLI hidden and a Bash 3.2 parse of changed shell files, and an explicit `--gate` overrides it), `git push --no-verify` of the verified SHA, CI success, then `oms update` guarded to that SHA when the repo is the harness checkout and a full local gate or a CI run whose jobs executed the full selection proves the SHA (`update.authority` records which), receipt beside its gate log under `$XDG_STATE_HOME/oh-my-setting/land/<repo-slug>/` read by `oms land status`. Refuses dirty or diverged trees and never pushes a HEAD that moved during the gate. Sibling worktrees of the same repository whose autopilot receipt is live (`proposing`, `proposal-review`, `driving`) are reported at intake and waited for before the push, up to `--sibling-wait` seconds (default 1800); at the deadline the landing ends `blocked` without moving shared refs, and `--ignore-siblings` records the override. |
| `init` | Creates repo-local `.oms` state and its ignore guard, then registers the canonical repo root with `oms tick`. The non-fatal summary says `registered`, `already registered`, or `not registered`; an unavailable tick helper or unwritable registry never prevents local initialization. |
| `tick` | Hourly unattended sweep of registered repos: `oms init` registers a newly initialized repo automatically, while `oms tick register` remains available for an adopted repo. The sweep performs journal sync, attempt reconcile, threads idle over 7d closed, active goal-less task packets closed after 7d, idle all-done plans retired after 14d, mechanically recovered or exactly superseded artifact failures resolved, and single stale failure-ledger rows retired after `OMS_TICK_FAILURE_STALE_DAYS` (14d). `OMS_TICK_RETIRE=0` opts out of task/plan/failure retirement but not artifact resolution; gc remains opt-in with `OMS_TICK_GC=1`. Each receipt and `swept` line reports `tasks_closed`, `plans_retired`, `artifacts_resolved`, `artifacts_superseded`, and `failures_retired`; a stale Codex plugin cache is refreshed. `install` wires a systemd user timer or a cron line this checkout owns. |
| `panel` | Native Sol/Opus main plus OMS activity sidebar; scoped role dispatch and a difficulty-tiered council (routine, deep or frontier) reuse peer front doors. Owner summaries and app delivery receipts are immutable indexed artifacts. `--results` reads retained evidence; `--finalize` preserves owner acceptance authority; `--retry-delivery` never resends a persisted revision. See [terminal panel](TERMINAL-PANEL.md). |
| `room` | Task-scoped work rooms on the canonical thread log: `new`, `join`/`bind`/`leave` with a declared role and scope, addressed `send`, cursor-based `updates` and `ack`, and `publish` to an explicitly enrolled Codex app or Claude Code session. Declared scope is not a write grant; acknowledgment is not approval. See [shared work rooms](TERMINAL-PANEL.md#shared-work-rooms-and-the-graph). |
| `autopilot`, `draft-pr` | Confirmed spec to reviewed plan, bounded landing, acceptance and semantic review; optional exact create-only GitHub branch plus Draft PR. No merge, release, ready, tag, or branch-update authority. |

`check.sh` and `tick` coordinate through the existing per-file lock namespace.
Checks may run concurrently; a sweep defers while a check or another sweep is
active on that repo. Dead check markers are recoverable, and the timer stays
enabled. This does not exclude maintenance receipts or task state from the
gate's state-leak detection; both entrypoints must have the updated code.

Commands that retain different authority may still share one decision engine.
Scope consumers use one Bash-compatible literal/glob matcher with deny
precedence; different glob declarations are never assumed to be subset-related.
Consultation entrypoints use one provider registry for aliases, availability,
and explicit/configured/automatic preference. State, inbox, runtime, plan, and
resume surfaces consume the same failure-attention, approval effective-state,
task-verification, plan-actionability, and PROJECT-state projections. These
shared reads do not grant mutation authority: durable approval `state` remains
distinct from its read-time `effective_state`, and `goal-drive` remains the
bounded executor for an approved plan while `autopilot` also owns spec review,
plan proposal, and final semantic review.

`trusted-local` inherits host files, credentials, processes, and network.
`isolated` only checks an existing Docker- or Podman-compatible daemon and local
image; `remote` only checks an operator-owned executable adapter. The compatibility
preflight and `oms runtime backend` resolve the same executable, so readiness and
execution cannot silently select different engines. The current supervisor does
not execute those two backends. Authenticated provider-native token/cost limiting is
not available, so a trusted-local job requesting either hard budget is refused
before launch. The private approval store is outside worker-writable `.oms/`
and uses `0700`/`0600`, but another process running as the same OS user can read
it; this is a write-integrity boundary, not account isolation.

VS Code and Orca are optional interfaces. Their session states are
observations, not OMS verification results. Direct commit, merge,
push, or PR actions in an external frontend bypass OMS admission and landing.
OMS is the authority only for flows started and completed through its managed
commands; keep one flow under one authority.

Scheduled `auto-update` is the unattended policy and status wrapper, not a
second updater. Receipt-owned schema-1 and schema-2 installs both preflight and
apply through `update.sh`, which remains the canonical rollback-capable install
transaction. Only receipt-less legacy checkouts retain the configured-upstream
compatibility path until a successful update creates an install receipt.
Before applying a new commit, both paths require successful `test.yml` push CI
for that exact SHA. Missing, pending, failed, or unreadable CI defers to the next
scheduled run; `gh` must be available and authenticated. Legacy updates merge
the checked SHA, without a second pull that could fetch an unchecked tip.
New cron/systemd triggers set `OMS_AUTO_UPDATE_MANAGED=1`: the wrapper selects
the checkout's exact private uv Python before reading receipts or running the
transaction. Runtime setup failures write a failed updater state instead of
leaving an old success visible. New installs provision the runtime before
provider configuration. Internal `scripts/python-runtime.sh` shares
versioned environments and launchers under the user's private OMS data folder;
system Python is used only to bootstrap the pinned uv download/lock parser.
Provisioning is locked, existing broken/unowned environments are preserved,
and rollback selects the old checkout's retained Python version. These runtime
files are outside the checkout and retained on uninstall for safe recovery.
`attention` reports skipped updates as `blocked` and a logout-dependent user
timer as `session-only`; systemd's `Persistent=true` alone does not keep the
user manager alive after logout. Linger is opt-in because it affects every user
service. Custom systemd drop-ins and private user skills remain user-owned.
Cron ownership is one exact begin/end marker pair. Install, removal, status,
and legacy receipt inference share the same `absent | valid | malformed`
classification; a malformed block is reported but never rewritten, so an
orphan marker cannot consume unrelated user cron entries. That malformed
verdict takes precedence over another installed trigger and is identical in
dry-run and apply mode.

## ML and HPC

The ML template records the scientific contract in PROJECT.md; optional
reference docs require `--full-docs` and never overwrite existing files.
ML/Slurm workflows use local-first experiment controls:

- `oms run new/current/show/timeline` for the run spine.
- `oms run capsule` for reproducibility capture and replay metadata.
- `oms runtime experiment` for comparable seed-, metric-, and invariant-bound
  studies; `oms runtime experiment launch` pre-registers a single command
  through the same run ledger (replacing `research-runner`).
- `oms data-manifest` for fingerprints, split checks, and leakage evidence.
- `oms experiment-board` for claims and collision avoidance.
- `oms run-reconcile`, `oms job-digest`, and `oms tsp-queue` for Slurm or local
  GPU work. `oms job-digest <id> --wait --wait-timeout S --max-bytes N` keeps
  the scheduler polling inside one shell call instead of repeated model turns:
  a spent budget exits 124 with observation incomplete and the job untouched, the
  digest is capped to whole lines with the omission stated, and a repeating
  controller error is reported once plus a total. Leaving the queue or empty
  accounting is reported as unknown, never as success. Where GNU `timeout` is
  present, both `squeue` and `sacct` use the remaining observation budget, with
  a one-second kill grace. Expired budgets skip accounting. Without GNU `timeout`
  (BSD/macOS), scheduler calls cannot be preempted; only sleeps are bounded.

Machine, cluster, dataset, and run details stay local unless the user explicitly
chooses a connector or tracked summary.

## Provider integrations

- Claude Code: skill hints, turn guard, failure memory, edit-time syntax guard
  (a file that does not parse after Edit/Write is reported in the same turn as
  feedback, never a block), session capture, handoff, and compact main/subagent
  HUDs. When supported by the CLI, OMS sets the Opus 5.5 per-model
  `modelSettings.<model>.autoCompactWindow` to 300000 and removes equal values;
  as with effort, an identical user value is treated as OMS-owned on removal.
  Model routing stays explicit in
  delegate/run commands; direct edits do not pay for a separate policy hook.
  Reinstalling converges each OMS hook's
  command, timeout and matcher and drops duplicates. A hook is OMS-owned only
  when its exact `bash <root>/scripts/<script>` command points at this or a
  verified earlier OMS checkout, so wrappers and same-named user scripts stay
  untouched and uninstall keeps user siblings in place. `oms doctor` and
  `oms doctor --surfaces` audit the same Claude Code registration contract
  read-only. Its shared event evidence does not establish provider-specific
  execution or usage frequency. Prompt hints and session-budget checks emit
  events conditionally; silence alone is not evidence that their hooks are
  unused. Lifecycle telemetry retains the recent-evidence window.
  Keyword skill hints remain opt-in (`OMS_SKILL_HINTS=1`). The Stop hook
  completes journal/relay work; its budget check runs only with an explicit
  `OMS_SESSION_BUDGET_TURNS` or `OMS_SESSION_BUDGET_HOURS` limit.
  `OMS_SKILL_ROUTER_OFF=1` skips the routing helper, including context/peer/relay
  hints and turn-start tracking. The prompt hook still runs its journal/daily-state
  work and state hints first; `OMS_STATE_HINTS=0` disables the hints.
  Prompt and tool state uses an explicit `OMS_STATE_REPO`, otherwise the
  payload cwd, falling back to the process cwd only when the payload omits it.
  Nested paths resolve to the repository root; handoff transcript selection
  and relative edited paths retain the original session cwd.
- Codex: local plugin hooks, including the edit-time syntax guard, plus a
  managed native status line when the user has not set one. On Python 3.9/3.10,
  arbitrary existing TOML requires `tomli` so
  the helper never rewrites an unvalidated config.
  The same installer manages usage-efficiency keys it can evidence:
  `background_terminal_max_timeout = 900000` (a longer ceiling for an empty
  background-terminal poll, not a wait duration) and, only where the user
  already enabled `[features.multi_agent_v2]` as a table, `min_wait_timeout_ms`
  and `default_wait_timeout_ms`. A user value, a boolean-form feature flag, or
  a lower user `max_wait_timeout_ms` is preserved; OMS never enables a feature,
  and model, reasoning, service tier, compaction, sandbox and approval keys are
  untouched. Codex rejects a config whose wait keys break `min <= default <=
  max`, so only when it writes those floors the installer lets the real `codex
  features list` load the result and removes its keys if that fails; an
  ordinary install pays no extra provider probe, since Codex ignores a root
  key it does not know. `oms doctor` reports the state; a project
  config, profile or `-c` flag can still outrank the written value.
  Install/update retires exact legacy OMS user-hook bridge entries only after
  verifying an enabled native plugin, current cache and stable hook support.
  It backs up the original bytes and preserves custom hooks; inconclusive
  replacement evidence or malformed/linked config leaves the file untouched.
- Antigravity: shared rules, skills, MCP, and provider calls. Hooks are enabled
  only after the installed binary passes a live surface probe; otherwise it is
  intentionally MCP-only. `--peer-permissions` grants `read_file(*)`,
  and sandboxed `command(*)` globally; all-MCP access remains approval-gated.
  Delegate exceptions accept only an exact command token and normalized
  absolute worktree parent. A sidecar lets uninstall remove only rules this
  install added.
  HUD/session-capture parity is not claimed.
- Optional CLI transports: Cursor, Grok Build, Gemini CLI, Qwen Code, OpenCode,
  DeepSeek Harness (`dsh`), Mistral Vibe, Pi, GitHub Copilot CLI, Factory Droid,
  and Aider join the same artifact, model-route, and provider-child contracts
  when detected. OMS does not install, authenticate, or reconfigure them.
  Provider-native read/edit modes and tool allowlists are pinned per invocation;
  Vibe trust is invocation-only, Pi project resources are ignored, and DeepSeek
  invocation telemetry is disabled. DeepSeek's filesystem preset does
  not confine network reads or every host process; Aider, OpenCode, and custom
  adapters additionally rely on the OMS worktree and post-run authority guard
  rather than an OS sandbox.
- MCP: shared state reads plus background peer actions that can incur provider
  cost and write `.oms` artifacts. Stdio records and prompts are byte-bounded;
  oversized input is rejected before provider argv or prompt files are built.
  By default `tools/list` exposes 12 core tools: inbox, repo state, journal,
  handoff list/read, peer start/result/operations, and graph render/query/trace/
  affected. Set `OMS_MCP_TOOL_PROFILE=full` in the MCP server's environment
  and restart its connection to discover all 26 tools; `core` is the default.
  The profile is fixed at process startup, so cached discovery is stable.
  Unlisted registered tools remain callable for existing clients: this setting
  reduces discovery overhead, not permissions. Schemas, annotations, UI resources,
  and protocol/Tasks negotiation are identical in both profiles.
  Runs are addressable from disk rather than from the conversation that started
  them: `oms_peer_operations` lists them newest first, a run whose process died
  without an exit reads as `stalled` instead of polling as running forever, and
  a finished result names the thread to continue from.
- MCP Apps: `oms_project_graph_render` is the presentation-only graph tool. It
  links the versioned `ui://oms/project-graph/v1.html` resource, while map,
  query, trace, blast, API, search, and affected remain reusable data tools.
  Node selection creates an editable instruction draft and never sends it
  without an explicit user action.
- MCP protocol revisions: the server is dual-era. `initialize` still
  negotiates for legacy clients; a `2026-07-28` client needs no handshake —
  `server/discover` answers statelessly and the revision named in each
  request's `_meta` selects the result shape, with `-32022` for a revision the
  server does not implement. Requests naming no revision keep their bytes.
- MCP Tasks: disabled unless `OMS_MCP_TASKS_EXTENSION=1`. With protocol
  `2026-07-28` and per-request `io.modelcontextprotocol/tasks` capability, a
  same-repository `oms_peer_start` reuses its durable operation ID as a Task;
  `tasks/get|update|cancel` add no list or second store. Older clients keep the
  prior CallToolResult shape.
- Provider work uses the existing CLI transport. The optional OMS app-server
  read adapter is retired; stale `OMS_CODEX_TRANSPORT=app-server` configuration
  fails explicitly instead of silently changing execution policy. Codex model
  discovery may still use its native app-server; that is not a work transport.

Integration removal failures propagate to `oms uninstall`; successful-looking
messages are emitted only after the corresponding CLI confirms removal.

## Verification and release

Installed global guidance and general/ML project templates favor one primary
test layer, reused fixtures, affected tests and small routine CI. New project
contracts include `CI scope/runtime`; expensive GPU/data/platform checks need
relevant-change, release, scheduled or explicit triggers under that contract.
The ML check scaffold uses `uv run --no-sync` after project Setup, so routine
checks do not implicitly resolve/install dependencies. Existing project check
scripts and workflows are not overwritten. The explicit `--python-ci` onboarding
option requires `pyproject.toml`, `uv.lock`, and a project-owned CPU `tests/`
suite. It adds one PR/manual CPU job, one locked sync, and the same
`bash scripts/check.sh fast --jobs 2 tests` entrypoint used locally. Other CI
or a custom checker requires project-specific integration; reapplying the
scaffold preserves existing files. No CI is added by default.

The checker keeps plain `fast` serial and fail-fast. Explicit `--jobs N`
(1–4) bounds concurrent compile/lint/selected-pytest stages, collects every
stage failure, and terminates their process groups on cancellation. It does
not shard pytest, add nested workers, or collect tests without selectors.
Only independent CPU checks belong in this mode; record actual scope/runtime
in the project's `PROJECT.md`. These defaults guide agents, not an enforcement
service or proof of measured CI savings.

`bash scripts/check.sh` is the repository gate: shell lint, Bash 3.2 parsing,
Python 3.9 grammar, skill validation, focused suites, and sharded smoke tests.
Development uses affected tests and static checks, not this full gate per edit.
Before landing a bundle, run `bash scripts/check.sh invariants` for the fast
repository-wide allowlist/review-state, skill and provider registry,
documentation reference and changed-source syntax checks; this lane is
intentionally limited to these invariants and does not replace affected checks
or the full landing gate.
For committed ranges, `--affected --changed-from BASE --changed-to HEAD` checks
ordinary README/docs changes directly, otherwise selects existing tests from
positive graph evidence. Skill/template Markdown uses the direct skill validator (including
project skill templates and their local references); `roles/*.md` relies on
the repo invariants' role-injection check; neither runs runtime
prompt/GC/lifecycle suites;
install/permission/state,
executable templates, selector changes, dirty trees, or uncertain graphs retain
full coverage. `source-distribution-smoke.sh --docs-only` checks documentation
references without its CI orchestration, installation or CLI-help probes.
`--quick` is partial feedback, not a substitute for affected or release checks.

CI selects once and runs affected tests plus changed-file lint in that same
job for PRs and main pushes. Full selections fan out to the existing Linux
lint, four focused lanes, four smoke shards and copy lifecycle runner. The
selector also emits `native=true|false`: install/lifecycle, shell portability,
Python compatibility, shared runtime/configuration and workflow changes require
macOS/Windows lifecycle, macOS Bash 3.2/BSD fixtures and real Python 3.9.
Weekly/manual runs and unavailable-base fallbacks require all native jobs.
The install matrix omits native hosts only on an explicit `native=false` plan;
the stable `gate` rejects missing, failed or cancelled required results and
accepts only planned skips. Require it in branch protection before enabling a local quick push hook;
having a workflow alone does not protect the branch. Deployment via `oms land`
verifies the exact committed tree with CI-parity affected checks against
`REMOTE/TARGET` (a full selection runs lint and the quick lane locally and the
full lanes in CI; explicit `--gate` wins), pushes its SHA, waits for CI
success, then updates only to that SHA when a full local gate or the run's
jobs prove the full selection. Failed or skipped CI, and an affected-only run
or one whose scope cannot be read, cannot refresh the installation. A moving update target is rejected by the updater.
Retrying the same commit, destination and gate resumes CI/install from its
successful push receipt; successful stages are reused. An installation receipt
is reused only while the installed checkout is clean and still at the same SHA.
A repository-wide lock prevents concurrent landing gates; background launches
acknowledge lock acquisition before reporting success. CI query errors are recorded immediately,
and each query is bounded by the remaining polling deadline (at most 30 seconds).

The existing check output and GitHub job summary show selection reasons and the
five slowest measured stages/tests per lane. Test timings reuse the smoke
runner's opt-in measurements; there is no extra run, job or timing database.
Mode-only/list output remains machine-readable and nested fixtures cannot
append to the real job summary. Summaries do not alter required coverage.

The gate fingerprints `.oms` file contents, entry modes, symlinks, and
directories so a test cannot quietly mutate the live checkout. Live `hooks/` and
`work-journal/` activity is excluded because it belongs to the active session.

Use an outside advisor for irreversible architecture choices, repeated
failures, or a release go/no-go. Routine bounded completion relies on the
declared mechanical gate.

## Known limits

- Native PowerShell is not a supported shell; Windows support means Git Bash.
- Full non-Linux behavior is strongest on the install lifecycle; most public
  tools run their comprehensive suites on Linux plus portability fixtures.
- On the exercised Windows surface (install, update, doctor, journal status,
  uninstall), paths cross to python as argv — which Git Bash converts — or
  base64-encoded where a value must survive the environment
  (provider-permissions). A new script on that surface must pick one of
  those two crossings; plain env path values reach native python rewritten.
- Locked upstream artifacts and the `edge` source ref remain supply-chain
  choices; review the lock and pin the source ref for higher assurance.
- Policy handling classifies provider text. Unfamiliar or localized refusal
  wording can be reported as an ordinary provider failure.
- Version publication, merge, and release remain explicit owner actions.
  `goal-drive` only creates local commits. `draft-pr publish` is the narrow
  exception for a verified create-only branch and Draft PR; it never updates
  an existing branch or advances the PR.
- Recovery-branch admission is a continuity guard, not an ownership proof:
  autopilot accepts only the deterministic branch or a strict
  `oms/autopilot-<spec-digest>-rN` branch descended from the reviewed base,
  with a clean in-envelope diff and the configured final gates, but no durable
  receipt attests which process created that local branch. Inspect any
  unexpected matching branch before checkout or resume; use
  `--review-mode gate` when provenance is uncertain.
- Frozen-patch path validation rejects static symlinks and detects the
  exercised directory-swap windows, but it is not a portable `dirfd`/`openat`
  transaction. A hostile same-UID process may still win a check-to-write race
  around `.oms/plan/commit-patches`; do not run `goal-drive` with untrusted
  same-account writers, and use OS isolation when that threat is in scope.
- Draft publication pushes with `--no-verify --no-signed`, so repository-local
  pre-push hooks and worker-writable signing configuration never execute under
  the publisher's credentials. It refuses repository-local executable
  transport, filter/diff/fsmonitor, proxy, credential, include, and URL-rewrite
  keys plus unsafe command-scope Git configuration before any remote operation, scans all new
  history objects, disables implicit tag/submodule
  pushes, and binds the `gh` viewer. Residue: a trusted global Git credential
  helper can still identify a different account; GitHub identity checks cover
  `gh`, not every possible transport credential source.
- GitHub creates pull requests from branch names, not caller-supplied expected
  object IDs. The publisher checks both refs immediately before and after the
  request and parks on drift, but another authorized GitHub writer can still
  move a ref in that request window; restrict writers or protect those branches
  when that residual race is unacceptable.

Claude's `oms-korean` output style declares `keep-coding-instructions: true`:
its final-reply language guidance retains Claude's native engineering
instructions. The repository `CLAUDE.md` imports `AGENTS.md`, avoiding a
second copy of policy. Global rules remain in `rules/global-AGENTS.md`;
provider system instructions and user-owned settings are not replaced.
