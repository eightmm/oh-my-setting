# Command Routing

When OMS commands overlap, choose the narrowest front door;
keep compatibility primitives when callers need their lower-level contract.

`oms list --frontdoor` exposes compact subsystem entrypoints, not every verb
below. `consult` is peer judgment; `peer-ask` and `advise` are council and
escalation variants. `autopilot` is bounded autonomy; `plan-run` and
`goal-drive` select narrower approved scopes. These stay discoverable here and
in `oms list --all` without expanding the compact catalog.

## Orient and resume

| Need | Use | Boundary |
|---|---|---|
| Ranked attention and one next command | `oms inbox --repo .` | First read on resume; `--fix-safe` is the only repair mode. |
| Decisions and blockers from prior sessions | `oms journal show --blockers`, then `--today` | Derived history, not current authority; read across sessions or days. |
| Effective goal, next action, or completion evidence | `oms runtime envelope show`, `next`, or `evidence show` | Typed projection; never mutation authority. |
| Full underlying dashboard | `oms state --repo .` | Drill down for source detail. |
| State consistency verdict | `oms state-verify --repo .` | Diagnosis only; never repairs state. |

## Diagnose and preserve

| Need | Use | Boundary |
|---|---|---|
| Installation summary | `oms status` | Link identity, tools, update state, and active tasks. |
| One repository's full OMS plane | `oms state` | Repo-local tasks, plans, runs, artifacts, and guards. |
| Composed operational view | `oms ops-cockpit` | Read-only, non-atomic lifecycle, review, approval, and telemetry projection. |
| Aggregate installation health | `oms doctor` | Managed files, tools, skills, manifests, and provider surfaces. |
| Narrow domain health | `oms project-doctor`, `oms model-doctor`, or `oms skill-doctor` | Diagnose only that project, model-routing, or skill surface. |
| Restorable tracked Git edits | `oms checkpoint` | Same-HEAD tracked snapshot; restore is dry-run unless applied. |
| Private machine or cluster context | `oms snapshot` | Local agent facts, not a Git restore point. |

## Ask, review, and delegate

| Need | Use | Do not substitute |
|---|---|---|
| One independent peer in a durable conversation | `oms consult`; add `--to PROVIDER` only when the seat matters | `agent-call` is the lower-level call primitive. |
| Several independent positions or a bounded debate | `oms peer-ask` | A repeated consult is not a symmetric council. |
| Findings on an existing diff | `oms peer-review --gate` | `peer-ask --diff` does not provide the same typed review and mechanical gate. |
| High-risk decision, repeated failure, or release go/no-go | `oms advise` | Do not spend an advisor on routine completion. |
| One bounded implementation | `oms peer-delegate --to PROVIDER` | Write workers return a patch, with no commit/push authority. |
| Isolated clean-HEAD audit with a worker role | `oms peer-delegate --read-only` | Not an executor: report only, never a patch. |

`agent-call` and `agent-run` remain public compatibility primitives. `agent-run`
dispatches to `agent-call` or `peer-delegate`; use `--mode` only when a caller
needs that wrapper, never wording classification when authority is known.

## Plan and autonomy depth

| Scope already authorized | Use |
|---|---|
| One independent bounded task | `oms peer-delegate` |
| Exactly one ready task in an existing plan | `oms plan-run --to PROVIDER --next` |
| Reviewed task whose stored patch should continue to landing | `oms plan-run --to PROVIDER --id TASK --land` |
| Repeated tasks in an already reviewed plan | `oms goal-drive` |
| Confirmed `PROJECT.md` through reviewed proposal and bounded drive | `oms autopilot` |

`agent-plan` owns the task graph and leases; it is not another execution loop.
Each autonomy layer retains its own stopping and publication contract, so do not
alias or collapse these commands.

## Land and maintain

| Need | Use | Boundary |
|---|---|---|
| Gate/push/update/CI a committed worktree | `oms land`; read `oms land status` | Detached job; receipt and log live under `$XDG_STATE_HOME/oh-my-setting/land/<repo-slug>/`; not `patch-land`. |
| Schedule adopted-repo maintenance | `oms tick register`, then `oms tick install` | Hourly sweep; GC requires `OMS_TICK_GC=1`. |

## Admit or mutate

- Use `oms patch-admit` for a read-only admission verdict.
- Use `oms patch-land` to mutate the main tree. It runs `patch-admit` itself,
  applies only after admission, and records landing lineage.
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

Prefer `oms runtime profile`, `backend`, and `experiment` for new typed flows.
Keep `install-profile`, `execution-profile`, and `research-runner` available for
their direct compatibility contracts. `execution-profile` retains its public
report but reuses the runtime backend resolver; do not add a second engine or
adapter probe there. In particular, `research-runner` is a
compact single-run/run-ledger wrapper; `runtime experiment` is the comparable
multi-arm, seed- and invariant-bound study path. Route discovery to the typed
path without deleting or silently migrating existing ledgers.
