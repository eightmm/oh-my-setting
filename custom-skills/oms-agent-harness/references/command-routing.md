# Command Routing

Use this reference when two OMS commands appear to overlap. Choose the narrowest
intent-specific front door; keep compatibility primitives for callers that need
their exact lower-level contract.

`oms list --frontdoor` exposes compact subsystem entrypoints, not every command
named below. `consult` represents peer judgment while `peer-ask` and `advise`
select council and escalation variants; `autopilot` represents bounded autonomy
while `plan-run` and `goal-drive` select narrower approved scopes. Those variants
remain discoverable here and in `oms list --all` without flattening the compact
catalog into every public verb.

## Orient and resume

| Need | Use | Boundary |
|---|---|---|
| Ranked attention and one next command | `oms inbox --repo .` | First read on resume; `--fix-safe` is the only narrow repair mode. |
| Decisions and blockers from prior sessions | `oms journal show --blockers`, then `--today` | Derived history, not current authority. Read it when work crosses a session or day. |
| Effective goal, next action, or completion evidence | `oms runtime envelope show`, `next`, or `evidence show` | Typed projection; never mutation authority. |
| Full underlying dashboard | `oms state --repo .` | Drill down when the inbox or runtime warning needs source detail. |
| State consistency verdict | `oms state-verify --repo .` | Diagnosis only; it does not repair state. |

## Diagnose and preserve

| Need | Use | Boundary |
|---|---|---|
| Installation summary | `oms status` | Link identity, installed tools, update state, and a small active-task summary. |
| One repository's full OMS plane | `oms state` | Repo-local tasks, plans, runs, artifacts, and guards. |
| Composed operational view | `oms ops-cockpit` | Read-only lifecycle, review, approval, and telemetry projection; non-atomic by design. |
| Aggregate installation health | `oms doctor` | Managed files, tools, skills, manifests, and provider surfaces. |
| Narrow domain health | `oms project-doctor`, `oms model-doctor`, or `oms skill-doctor` | Diagnose only that project, model-routing, or skill surface. |
| Restorable tracked Git edits | `oms checkpoint` | Same-HEAD tracked staged/unstaged snapshot; restore is dry-run unless applied. |
| Private machine or cluster context | `oms snapshot` | Local reference facts for agents, not a Git restore point. |

## Ask, review, and delegate

| Need | Use | Do not substitute |
|---|---|---|
| One independent peer in a durable conversation | `oms consult`; add `--to PROVIDER` only when the seat matters | `agent-call` is the lower-level call primitive. |
| Several independent positions or a bounded debate | `oms peer-ask` | A repeated consult is not a symmetric council. |
| Findings on an existing diff | `oms peer-review --gate` | `peer-ask --diff` does not provide the same typed review and mechanical gate. |
| High-risk decision, repeated failure, or release go/no-go | `oms advise` | Do not spend an advisor on routine completion. |
| One bounded implementation | `oms peer-delegate --to PROVIDER` | A write worker returns a patch and gains no commit or push authority. |
| Isolated clean-HEAD audit with a worker role | `oms peer-delegate --read-only` | This is not an executor; it produces a report, never a patch. |

`agent-call` and `agent-run` remain public compatibility primitives. `agent-run`
dispatches to `agent-call` or `peer-delegate`; use an explicit `--mode` when a
caller genuinely needs that wrapper, and never rely on wording classification
when read versus write authority is already known.

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
