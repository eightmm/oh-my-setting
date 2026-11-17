# Feature scope and adoption criteria

OMS coordinates a main agent and bounded workers. Its core owns the path from
an authorized task through isolated work, review, verification and publication.
Add a subsystem only when an existing workflow cannot meet a named need.

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
| Apptainer proposal | A recurring local CPU job uses an existing SIF on a compatible Linux host | Require the host/session capabilities; GPU, Slurm integration and immutable image pinning are separate work |
| Resumable SSH collection proposal | Finalized single-file results repeatedly fail during transfer | Require exact size and SHA-256; account for chunk round trips and retained cache space |
| Codex inference through Pi/Claude | A recurring workflow needs that UI with Codex inference | Keep the bridge experimental and separate from native provider support |

The Apptainer and collection proposals are [#242](https://github.com/eightmm/oh-my-setting/pull/242)
and [#243](https://github.com/eightmm/oh-my-setting/pull/243). Passing fixtures does
not establish real host resource enforcement, account access or transfer speed.
Their optional invocation does not make them default installation requirements.

Research budget fields describe different evidence. Run reservations enforce
the run count. GPU-hour projections depend on reported usage. `final_evals.used`
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

The bounded feedback repair proposal [#239](https://github.com/eightmm/oh-my-setting/pull/239)
remains deferred. Its useful window is an explicitly reviewed task with a live
review lease and one unused repair attempt. Same-PR failure signatures can block
a later task as well. First establish recurring eligible cases and saved review
work; do not weaken duplicate-send or ownership guards to manufacture usefulness.

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
