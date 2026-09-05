# Command Routing

Choose the narrowest front door; retain primitives for lower-level contracts.

`oms list --frontdoor` exposes compact subsystem entrypoints; `oms list --all`
retains every public primitive. Start with the six work families in SKILL.md;
this reference resolves intent variants, not additional default steps.

## Orient and resume

| Need | Use | Boundary |
|---|---|---|
| Ranked attention and one next command | `oms inbox --repo .` | When injected state is insufficient; only `--fix-safe` repairs. |
| Missing prior decisions/blockers | Relevant `oms journal show --blockers` or `--today` | Reuse injected history first; not authority. |
| Effective goal, next action, or completion evidence | `oms runtime envelope show`, `next`, or `evidence show` | Typed projection; never mutation authority. |
| Full underlying dashboard | `oms state --repo .` | Drill down for source detail. |
| State consistency verdict | `oms state-verify --repo .` | Diagnosis only; never repairs state. |

## Diagnose and preserve

| Need | Use | Boundary |
|---|---|---|
| Installation summary | `oms status` | Link identity, tools, update state, and active tasks. |
| Repository detail | `oms state` | Tasks, plans, runs, artifacts, guards. |
| Composed operational view | `oms ops-cockpit` | Read-only, non-atomic lifecycle, review, approval, and telemetry projection. |
| Aggregate installation health | `oms doctor` | Managed files, tools, skills, manifests, and provider surfaces. |
| Narrow domain health | `oms project-doctor`, `oms model-doctor`, or `oms skill-doctor` | Diagnose only that project, model-routing, or skill surface. |
| Restorable tracked Git edits | `oms checkpoint` | Same-HEAD tracked snapshot; restore is dry-run unless applied. |
| Private machine or cluster context | `oms snapshot` | Local agent facts, not a Git restore point. |

Choose one report first. Cockpit projects inbox from the same collected state.
`doctor` already includes model/skill checks; run narrow diagnostics only for
additional evidence. Installation health and project integrity remain distinct.

## Ask, review, and delegate

| Need | Use | Do not substitute |
|---|---|---|
| One independent peer | `oms consult`; optionally `--to PROVIDER` | `agent-call` is the lower-level primitive. |
| Several independent positions or a bounded debate | `oms peer-ask` | A repeated consult is not a symmetric council. |
| Findings on an existing diff | `oms peer-review --gate` | `peer-ask --diff` does not provide the same typed review and mechanical gate. |
| High-risk decision, repeated failure, or release go/no-go | `oms advise` | Do not spend an advisor on routine completion. |
| One bounded implementation | `oms peer-delegate --to PROVIDER` | Write workers return a patch, with no commit/push authority. |
| Questions/changes for agents already working together | `oms thread append`, `updates`, `ack` on a scoped live thread | Messages neither spawn providers nor grant authority; see state-memory.md. |
| Isolated clean-HEAD audit with a worker role | `oms peer-delegate --read-only` | Not an executor: report only, never a patch. |

`agent-call`/`agent-run` are compatibility primitives, not extra council seats.
`agent-run` dispatches to read or write; select known authority explicitly.
All peer outputs use the same artifact section reader; review remains typed.

## Plan and autonomy depth

| Scope already authorized | Use |
|---|---|
| One independent bounded task | `oms peer-delegate` |
| Exactly one ready task in an existing plan | `oms plan-run --to PROVIDER --next` |
| Reviewed task whose stored patch should continue to landing | `oms plan-run --to PROVIDER --id TASK --land` |
| Repeated tasks in an already reviewed plan | `oms goal-drive` |
| Confirmed `PROJECT.md` through reviewed proposal and bounded drive | `oms autopilot` |

`autopilot -> goal-drive -> plan-run` is an execution hierarchy, not three
parallel loops. `agent-plan` owns tasks/leases. Start only the required layer;
retain its stop/publication boundary and the frozen executor contract.

## Land and maintain

| Need | Use | Boundary |
|---|---|---|
| Gate/push/update/CI | `oms land`; read `oms land status` | Committed worktree; detached job, external receipt; not `patch-land`. |
| Schedule adopted-repo maintenance | `oms tick register`, then `oms tick install` | Hourly sweep; GC requires `OMS_TICK_GC=1`. |

Tick and auto-update share scheduler selection, not timers or ownership.
`cleanup` removes legacy links; `gc` retires repo state; `uninstall` restores
installation ownership. Never widen one deletion scope to another.

## Admit or mutate

- Use `oms patch-admit` for a read-only admission verdict.
- `oms patch-land` admits before mutation and records landing lineage.
- Run both only when a separate preview was actually needed; never replace
  `patch-land` with a manual `git apply`.

## Preserve the right kind of continuity

| Information | Use |
|---|---|
| Current short-lived task handoff | `oms agent-task` |
| Derived prior-session activity | `oms journal` |
| Curated stable facts and recurring pitfalls | `oms agent-memory` |
| Multi-provider question history | `oms thread` through `consult` or peer commands |
| Same-machine provider-session continuation | `oms session-handoff` |
| Sanitized cross-machine project continuity | `oms runtime capsule` |

These stores have different retention, provenance, and sensitive-content
contracts. Do not merge them or treat one as another's authority source.

## Typed runtime and compatibility surfaces

Prefer `oms runtime profile`, `backend`, and `experiment`. `install-profile`
and `execution-profile` already share their engines; do not repeat probes.
`research-runner` is a single-run wrapper, not a comparable multi-arm study.
Experiment-board owns intent/claims, run-ledger records execution, and runtime
experiment links study evidence. Keep their IDs linked, not their stores merged.
