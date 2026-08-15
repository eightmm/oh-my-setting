---
name: oms-agent-harness
description: >
  Shared state/plans/recovery, typed runtime projections, multi-agent
  coordination, peer consultation and review, model routing, isolated
  delegation, patch landing, bounded coding autopilot, Draft PRs, artifacts,
  experiments, and cross-provider handoff.
---

# Agent Harness

You own scope, admission, verification, commit, push, release, and synthesis.

## Interaction contract

Treat OMS as an agent-side control plane. Users provide goals, constraints, and
material authority; invoke, review, resume, and recover it internally.
Never ask users to copy commands, approve proposal digests, inspect `.oms`, or
restart a parked run. Ask only for a missing implementation-shaping decision or
new authority. Report outcomes, verification, and genuine blockers; omit
internal commands and state unless diagnosis was requested.

For authorized coding, the top-level parent confirms that `PROJECT.md` matches
the goal, reviews exact proposal bytes as untrusted data, returns their digest,
and handles exit 4 or `status` itself. Use `--draft-pr` only when the current
request or standing repo policy grants that remote effect. Push, merge, ready,
tag, and release need their own authority.
If `PROJECT.md` is missing, draft, or materially drifted, route internally
through `oms-spec-interview` and ask the user only for unresolved material gaps.

The typed runtime is a projection and execution adapter over the existing
hardened plane. It never replaces `peer-delegate -> patch-admit -> patch-land`,
plan leases, executor souls, approvals, commit intents, or publication intents.
Do not treat a runtime snapshot, capsule, backend receipt, context bundle, or
evidence view as mutation authority.

## Route by intent

| Need | Agent action | Authority |
|---|---|---|
| orient or resume unclear state | `oms inbox --repo .`; use `oms runtime envelope show` for the effective contract and evidence | read |
| see the next permitted action | `oms runtime next` | read |
| inspect criterion-level completion | `oms runtime evidence show`; bind only existing fresh receipts | read, append |
| compile bounded delegated context | `oms runtime context --phase implementation|review --target PATH` | read, local artifact |
| recall or record knowledge | `oms agent-memory recall`, `append`, or `pin` | read, append |
| preserve a repeating lesson | `oms skill-forge add --name NAME` | append |
| get an outside view | `oms consult`; use `oms peer-ask` for several peers | read |
| act irreversibly or handle a repeat failure | `oms fail-ledger check`, then `oms advise` | read |
| delegate a bounded write | `oms peer-delegate --to NAME` | worktree write |
| split dependent work | `oms agent-plan`, then `oms plan-run` | worktree write |
| implement an authorized goal | parent reviews `oms autopilot … propose`, then runs its digest-bound continuation | repo |
| publish its authorized Draft PR | add `--draft-pr` to that parent-owned flow | create-only remote |
| judge a diff | `oms peer-review --gate` | read |
| inspect route/outcome/cost telemetry | `oms artifact-index telemetry`; use `oms runtime benchmark` for content-free effectiveness views | read, append |
| preserve local state | `oms checkpoint create`; restore is dry-run unless `--apply` | repo write |
| move safe continuity across machines | `oms runtime capsule export|verify|import`; never copy raw `.oms` | local artifact, append |
| select the minimum installed capability | `oms runtime profile check|install-plan|install` | host tool install |
| run a bounded local/container/remote command | `oms runtime backend check|run` | execution only, never landing |
| run a comparable research study | `oms runtime experiment register|run|invariants|summarize` | execution, append |
| admit and land reviewed bytes | `oms patch-admit`, then `oms patch-land` | repo write |
| run one provider boundary | `oms agent-run --mode read\|write` | read or worktree write |
| pin persona, model, or effort | `oms agent-role`, `oms agent-executor`, `oms models` | — |

`oms list --frontdoor` shows the compact agent-facing catalog; `oms list --all`
keeps every compatibility primitive visible. Each answers `oms <tool> --help`.

## Lines that do not move

- Keep secrets, private paths, machine details, raw logs/data, and checkpoints
  out of prompts and shared state.
- Do not hand-edit `.oms/`; prefer tool JSON. `oms gc` clears stale state.
- Provider workers cannot delegate again and never gain commit or push authority.
- Advisors serve irreversible/high-risk decisions, repeats, release go/no-go —
  not routine completion.
- `plan-run` stops in review unless landing was explicitly authorized.
- `trusted-local` is process supervision, not a sandbox. Capability receipts
  must separate declared, enforced, and unknown properties.
- A portable capsule is advisory only and carries no lease, approval, command,
  credential, absolute path, raw transcript, or publication right.

## References

- Runtime projection, evidence, context, profiles, capsules, and backends: [runtime-core.md](references/runtime-core.md)
- Prompt hooks, memory, tasks, skills: [state-memory.md](references/state-memory.md)
- Plans, failures, recovery: [plans-recovery.md](references/plans-recovery.md)
- Autonomous stopping: [autonomy-loop.md](references/autonomy-loop.md)
- Roles/executors: [roles-executors.md](references/roles-executors.md)
- Delegation/artifacts: [delegation-artifacts.md](references/delegation-artifacts.md)
- Consultation: [cross-agent-consultation.md](references/cross-agent-consultation.md)
- Review/release gates: [review-gates.md](references/review-gates.md)
- Models/quorum: [model-routing.md](references/model-routing.md)
- Prior session: [session-handoff.md](references/session-handoff.md)

Internally retain provider, artifact/patch, landing, execution, criterion, and
context evidence. Report only useful conclusions, changed behavior,
verification, and skipped checks.
