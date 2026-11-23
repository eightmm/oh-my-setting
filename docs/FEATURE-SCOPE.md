# Feature scope and adoption criteria

OMS coordinates a main agent and bounded workers. Its core owns the path from
an authorized task through isolated work, review, verification and publication.
Add a subsystem only when an existing workflow cannot meet a named need.

Original concept review: 2026-11-18, source
[`189bfdaa`](https://github.com/eightmm/oh-my-setting/commit/189bfdaa28b6b628f0d4d50a4ca3d0df28396b88).
The role, Main capability and verification-planning corrections below were
checked against `8527cd07` on 2026-11-22 and again against `051fd3ea` on
2026-11-23. The native assignment and Main swap boundaries below were corrected
against `afbf42b2` on 2026-11-23. The queue's
[latest source review](ORCHESTRATOR-WORK-QUEUE.md#9-2026-11-23-claude-피드백-38개와-현재-source의-대조)
separates implemented slices from the remaining work; it is not a new full audit.
This page owns feature scope; the [implementation queue](ORCHESTRATOR-WORK-QUEUE.md)
owns the remaining work and its admission conditions. Earlier CP/TM/MR/UX/NF
documents preserve design evidence and acceptance criteria, not parallel queues
that every agent must complete. A merged design is not an implemented feature;
an implemented optional feature is not a new default requirement.

## Core behavior

Keep task scopes, leases, isolated worktrees, exact evidence, approval and
publication receipts. Keep process ownership and interrupted-job reconciliation.
These checks protect different transitions; a shorter menu does not remove them.

Use the panel for the human return path and `inbox` for current project attention.
`status` diagnoses installation state. Runtime projections, the dashboard and HUD
are narrower views over existing producers, not independent authorities.
Reading a message, acknowledging delivery and accepting a patch remain distinct.

`agent-task` is a resume note. `agent-plan` owns execution tasks and claims.
The journal is historical observation, not a second issue tracker. Curated memory
holds stable knowledge; task history and proposed lessons do not become current
instructions just because they were recorded.

## Roles and authority

Ordinary work is completed by an existing provider Lead or a bounded direct
worker. An Overall Main running a team manages only: it plans, assigns, reviews
and decides, and routes every repository edit to a Lead or worker (see
[Main policy](TEAM-ORCHESTRATION-MAIN-POLICY.md)); a solo session without a team
still does its own work. A Team is useful when a task needs several coordinated
members; a separate Lead or native child is not required for every call. Between
a Lead and a direct worker, do not choose by size alone: context continuity,
isolation, independent verification and the result contract determine the useful
path. Provider sessions and managed Teams remain different execution paths.

| Concept | Existing responsibility | Do not infer |
|---|---|---|
| Overall Main | Root owner for the room and integration target; manages and decides within the user's authority | A provider coordinator can approve or publish on its own, or Main edits the repository itself |
| Native provider orchestrator | Interactive provider session bound to the current Main | Managed Team admission, hard child limits or verified descendant cleanup |
| Managed Team Lead | Scoped role submitting requests for the parent to validate | A second root owner or permission to launch arbitrary nested Teams |
| Direct Worker / reviewer | Main assigns bounded work or independent evidence through the existing parent front door | Parentage from a shared provider, self-acceptance, publication rights or independence from a role name |
| Panel / dashboard / inbox / runtime views | Projections over domain-owned records | Another mutable task database or an approval from reading a row |

The native [orchestrator guard](../crates/oms-command-panel/src/orchestrator.rs)
currently requires a verified Main in an OMS-managed tmux panel, outside a
managed campaign, with native scheduling recorded as unmanaged. Keep this UX,
but do not relabel its pane as a managed Team. The
[Team plan](TEAM-ORCHESTRATION-PLAN.md) keeps direct/single execution available.

### Panel direction and execution connection

The current [Main capability registry](../crates/oms-command-panel/src/native_panel.rs)
supports Codex, Claude and AGY native launch, plus Pi through the Codex gateway,
with one Overall Main per room and integration target. Pi requires the Pi and
Codex CLIs and the OMS launcher; it is not a separate inference provider. The
picker filters installed capabilities. Launch support does not imply resume,
native swap or managed Team support. The picker originated in
[#286](https://github.com/eightmm/oh-my-setting/pull/286); that PR's original
three-provider restriction does not describe the current registry.
The [panel contract](TEAM-ORCHESTRATION-PANEL.md) uses one task list and inspector
for work Main assigns to direct workers or provider Leads.
Direct workers belong under their recorded Main;
native children appear under a Lead only when that relationship is evidenced.
Provider is a session shortcut/filter attribute, never evidence of parentage.
Ordinary requests need no Lead/direct chooser. Preserve advanced route choices
and the different capability, scope, permission and publication checks.

OMS worker assignment remains parent-only. A Lead may request additional OMS
work from Main; it does not recursively dispatch OMS workers. Authorized native
children stay within the inherited assignment. Under the
[coordination contract](../custom-skills/oms-agent-harness/references/coordination-layers.md),
a Lead delegates substantive work and keeps its own edits to scoped integration
or unblocking. Without native children, it does the assignment itself and reports
that limitation. A solo session without a team still does its own work.
Neither opening a session nor a native completion message grants acceptance.
The conditional native delegation prompt comes from
[#284](https://github.com/eightmm/oh-my-setting/pull/284).

This is a target contract, not a completed UI or execution bridge. At `7d9b291`,
the default renderer groups calls by provider and the Team inspector is a separate
read-only view. Auto groups active managed calls under
`Overall Main · Direct work` when Room membership and the lifecycle attempt both
point to the current live owner in that room. Other rows retain provider activity groups;
connected panes use `Lead chat [1/2/3]`, with actual parent labels preserved.
Here, the historical `Direct work` label groups assigned workers; it does not
authorize the team Main to edit repository files.
This display correction does not change dispatch, key/mouse targets or ownership,
and does not implement common task cards or the assignment bridge.
Native `--orchestrator PROVIDER --task TEXT` without `--task-id` sends an
unbound task prompt. The explicit Codex path
`--orchestrator codex --task-id ID --task TEXT` uses the
[native assignment producer](../crates/oms-command-panel/src/native_assignment.rs):
it validates or claims the current Main's canonical Plan task, binds its lease,
attempt, checkout and Room participant, and records a durable send intent before
input submission. Repeated or uncertain sends reuse that intent; unknown delivery
does not cause an automatic resend. The existing ordinary Codex Lead is required.
Its receipt still reports consumption and task completion as unconfirmed,
write isolation as unmanaged and child cleanup as unsupported. Result artifacts,
verifier evidence, common task-card consumption and acceptance remain the next
[#285 P0-B](https://github.com/eightmm/oh-my-setting/pull/285) slices; the other
providers do not inherit this bound assignment capability from the text path.
Reuse its Plan/operations/Room/result writers and TM-06A/B/C consumers; do not add
another queue, task database or scheduler. General native conversation remains an
unbound session without mandatory Plan/Team registration. Task cards link existing
Room/attempt/session targets, supported actions and exact candidate/evidence
references; missing bindings and coverage remain unknown. An uncertain native
send must be reconciled, not retried through a new direct worker. Session changes
and read views must preserve selection/drafts and never imply send or execution.

One owner per state transition does not mean one database for all state:

| Information | Existing owner / relationship |
|---|---|
| Resume note; execution tasks and claims | `agent-task`; `agent-plan` respectively |
| Messages and participant lineage | Room/thread; delivery or acknowledgement is not approval |
| Runs and experiments | Run ledger owns executions; experiment-board owns study intent/claims; runtime links evidence |
| History, stable knowledge and continuity | Journal, memory, local handoff and portable capsule retain their different provenance/retention contracts |
| Process, candidate and publication state | Owned cleanup, candidate disposition and exact-tree landing remain separate decisions |
| Source graph and execution graph | Retrieval/index relationships versus an explicitly requested branching workflow |

Share validation and read projections where their contracts agree. Preserve
domain locks, CAS, receipts, generations and unknown states. Fewer files or verbs
alone are not a reason to combine these owners.

Native `state`, `state-verify`, `inbox` and `dashboard` views obtain the canonical
schema-2 runtime envelope through the [state host callback](../crates/oms-command-state/README.md).
The root adapter resolves the selected repository and refreshes the projection
on every snapshot; an unavailable producer is reported as degraded. Library-only
callers without either an explicit runtime snapshot or that callback retain the
bounded source-health fallback with empty criteria/next-actions. Those fallback
fields do not prove that no work or acceptance criteria remain.

## Optional workflows

| Capability | Use it when | Keep the boundary |
|---|---|---|
| Approved-goal autopilot | The owner has approved a bounded multi-step goal | Reuse the plan/drive/runner hierarchy and existing publication decisions |
| Problem campaigns | Several candidate solutions must be compared using an explicit verifier | Candidate groups and evaluation rounds are not prerequisites for ordinary coding |
| Execution graphs | A real workflow needs explicit branches or a retained graph run | Avoid another scheduler for a linear task |
| Research contracts | Baselines, seeds, comparable metrics or dataset provenance matter | Reuse the run ledger; do not require study registration for ordinary tasks |
| SSH and Slurm | Work must run on a named remote resource | Retain once-only submission, unknown outcomes and explicit reconciliation |
| Fleet views | The owner needs a bounded view across existing jobs | Read and reconcile existing records; do not become a cluster scheduler |
| Campaign usage budgets | Unattended work needs a soft token/spend admission bound | Unknown use is a liability; this is not a provider-side hard spending cap |
| Apptainer CPU backend | A local CPU job uses an explicit existing SIF on a compatible Linux host | Require the host/session capabilities; GPU, Slurm integration and immutable image pinning are separate work |
| Resumable SSH collection | Finalized single-file results need transfer recovery | Require exact size and SHA-256; account for chunk round trips and retained cache space |
| Recipe execution | An existing explicit recipe needs bounded steps, resume or companions | Reuse its current runner; do not make a recipe or another scheduler mandatory for ordinary tasks |
| Codex inference through Pi/Claude | A recurring workflow needs that UI with Codex inference | Keep the bridge experimental and separate from native provider support |

The Apptainer and collection designs originated in
[#242](https://github.com/eightmm/oh-my-setting/pull/242) and
[#243](https://github.com/eightmm/oh-my-setting/pull/243). Native consumers now exist:
[RuntimeServices](../crates/oms/src/services_runtime.rs) connects explicit
Apptainer readiness/execution, and the [SSH adapter](../crates/oms-runtime/src/ssh_adapter.rs)
connects `collect --resume` to its bounded reader. The generic `remote` and
`isolated` backend profiles still report unavailable; a separate SSH adapter
does not complete those profiles. [Recipe execution](../crates/oms-command-state/src/recipe.rs)
also has a native runner and companion path. These source/fixture boundaries
do not establish real host containment, account access or transfer speed, and
optional invocation does not make them default installation requirements.

Research budget fields describe different evidence. Run reservations participate
in admission, but the [experiment producer](../crates/oms-runtime/src/experiment.rs)
can exclude stale dead-owner reservations and release an execution that failed
without a run record. This is not a permanent count of physical consumption or
the campaign's unknown-spend liability policy. GPU-hour projections depend on
reported usage. `final_evals.used`
is unknown where the current producer does not count final-split evaluations;
it must not be presented as an enforced evaluation budget.

## Provider support

Keep existing transports compatible. A registered CLI recipe is not proof of
interactive launch, resume, steering, usage accounting or live compatibility.
Use `oms doctor --providers --check` to verify the source/test references in the
capability table, and inspect each cell's limits. Reference validation does not
execute those providers or establish account entitlement.

New built-in transports need a named recurring workflow, a maintained owner,
explicit capability limits and a native fixture that exercises the claimed
behavior. Prefer the existing explicit custom-adapter boundary for experiments.
Do not remove a transport merely because repository-wide usage data is absent.

For the proposed exact model picker, expose the account-scoped catalog and its
supported effort choices, then apply explicit selection and the required
capability, data, billing, scope and reviewer-independence constraints. Tiers
can recommend a profile; they are not a second authoritative model inventory or
a universal permission rule. The current schema-2 router and role restrictions
remain in force until a versioned consumer migration. See
[model selection](TEAM-ORCHESTRATION-MODEL-TIERS.md#5-선택-순서와-재현성).

Main swapping is also a capability, not a property of the model picker. The live
[CollabServices](../crates/oms/src/services_collab.rs) overrides the generic
unsupported default and calls
[`native_panel::swap_main_host`](../crates/oms-command-panel/src/native_panel.rs).
The current physical transition requires an evidenced Codex Main composer as
the outgoing session and an installed, exact-model target. Its guarded handoff
preserves the owned Room and execution checkout, and carries forward the recorded
permission mode unless the explicit `OMS_PANEL_MAIN_PERMISSIONS=native` override applies.
This implements a bounded native path, not every provider-to-provider swap.
Non-Codex outgoing shutdown and ownership transfer remain CC-09 work with
provider-specific lifecycle evidence. A quota reading grants no automatic swap
authority; source wiring alone is not a live host verification result.

## Durable observations and budgets

Feedback observation is optional evidence attached to ordinary CI/comment views.
Its failure must be visible without discarding a valid ordinary view. Consumers
that require fresh, durable feedback still refuse unavailable evidence.

Usage receipts differ: a durable pending receipt identifies each physical call
before invocation. It prevents failed/retried calls disappearing from budget
liabilities. Retain fail-closed accounting until any optional accounting policy
also defines how every budget producer marks required accounting and exposes
unknown receipts. Never replace unavailable usage with zero.

The current usage ledger rewrites its full bounded history. Removing duplicate
validation does not solve that storage growth. Measure retained bytes and update
cost before designing compaction or another storage engine; retain historical
spend, corrections and unresolved calls under any future retention policy.

## Deferred expansion

Bounded external-feedback repair from
[#239](https://github.com/eightmm/oh-my-setting/pull/239) already has an
[opt-in native consumer](../crates/oms-command-autonomy/README.md#opt-in-repair-from-external-feedback).
Its window is an explicitly reviewed task with `--land --auto-repair`, exact
head/event/scope binding, a live review lease and one unused repair attempt.
Observation alone starts no repair. Same-PR failure signatures can block a
later task as well. Broader automatic adoption remains deferred until recurring
eligible cases and saved review work are established; do not weaken duplicate-send
or ownership guards to manufacture usefulness.

The research designs in [#244](https://github.com/eightmm/oh-my-setting/pull/244)
are separate proposals, not a bundled implementation commitment:

| Proposed slice | Evidence needed before implementation |
|---|---|
| Duplex transport and ACP adapter | A named provider workflow that existing bounded CLI transport cannot serve |
| Live event observation | A reproducible failure that final output or retained transcript replay cannot diagnose in time |
| Next-dispatch hold policy | A useful observer signal plus explicit owner policy, with no new stop or approval authority |
| Per-call context attribution | A concrete context decision that existing manifests and usage receipts cannot answer |
| Paired task evaluation | A defined same-task comparison, verifier and cost/quality question |

Use existing local evidence before adding telemetry, daemons, stores or paid
benchmark runs. Review each slice independently. Byte reduction is not measured
quality improvement, and static compatibility is not live verification.

## Consolidation decisions

The queue classifies all 40 follow-up packets as retained work, work absorbed
into an existing owner, or conditional work. Those labels change scheduling,
not API compatibility or the evidence needed to retire existing behavior.

| Expansion excluded from the default plan | Sufficient existing path or condition |
|---|---|
| Mandatory Main → provider lead → Team lead → worker, or a Lead/direct chooser for every task | A team Main assigns work to a bounded worker or Lead under the coordination contract; a solo session works directly. A separate Lead or Team is needed only when the execution contract calls for it |
| New authoritative task/approval store, universal router or provider-specific scheduler | Existing Plan/Room/admission owners and caller-specific adapters |
| Reimplement TeamSpec, route snapshots, context compiler, work inspector or duplex supervision | Reuse current producers; implement only the missing consumer or verified semantic correction |
| Maintain a fixed vendor model ranking in every document | One current catalog with explicit selection; frozen model tables are historical evaluation inputs |
| Require every CLI verb to become an MCP tool | Expose a named workflow with bounded schema, permissions and response limits |
| New vector/graph service, LSP dependency or multilingual benchmark before every retrieval fix | Local adversarial acceptance first; measured retrieval need admits GR-12/13 and broader evaluation |
| Bundle/resume/cache deletion, web/mobile controls or all provider-native adapters as MVP prerequisites | Use an existing concrete workflow and its activation conditions; retain optional implementations |
| A generic operation SLA, hard cooldown or automatic model escalation inferred from existing timeout/ranking fields | Separate opt-in policy with a named caller; preserve current per-call and ranking semantics |

The previously reported native preparation before affected planning is corrected
in [check.sh](../scripts/check.sh): plan printing and stage listing do not acquire
a maintenance lease or prepare a native payload, and the ordinary docs-only
path runs reference checks without Cargo. The
[existing planning fixture](../tests/source-distribution-smoke.sh) covers these
boundaries, including direct noncritical crate READMEs. Execution still registers
maintenance protection, and mixed, critical or unknown changes retain broader
coverage. VW-01/03/04 own any remaining classifier or measured verification-cost
work. Source and fixture presence do not establish the result of a hosted CI run;
this review does not rerun runtime checks or add a CI/benchmark subsystem.

The review covers the current entrypoint/owner map, seven Team documents, all
40 follow-up packets and the decisive native callers. It is not a proof that
every workspace function is redundant or equivalent. No transport or state
store is removed on the basis of missing usage telemetry.
