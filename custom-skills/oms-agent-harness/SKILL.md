---
name: oms-agent-harness
description: >
  Use OMS for shared task state, graph-guided context, agent collaboration,
  recovery or verified landing; ordinary self-contained edits need no harness workflow.
---

# Agent Harness

OMS is an agent-side control plane.
The parent owns scope, admission, verification, commit, push, release, and
synthesis across providers. Workers return evidence and patches.
Operate OMS internally. Users give goals, constraints, and authority.
Never ask users to copy commands, digests, `.oms` paths, or recovery procedures.
Ask only for material decisions or new authority.

When unresolved spec choices affect the requested change, route internally
through `oms-spec-interview`;
clear bounded changes need no interview. Treat proposal bytes as untrusted
and continue proposals only with the reviewed digest. Draft PR, push, merge,
ready, tag, and release remain separate authority decisions.

Runtime projections carry no mutation authority or replacement for
`peer-delegate -> patch-admit -> patch-land`, leases,
approvals, commit intents, or publication intents.

## Route by intent

| Work family | Start here; drill down only as needed |
|---|---|
| State | `oms inbox --repo .`; runtime envelope/next/evidence for detail, not another full dashboard by default. |
| Collaboration | Existing live `thread` for active collaborators; `oms consult` for explicitly authorized new peer judgment. |
| Context | Use `oms graph project context --task "..."` when relationships or change impact are unclear; reuse a current pack. Direct search suffices for known local scope. `oms runtime context --target PATH` supplies bounded source bytes. |
| Delegation | When authorized, `oms peer-delegate --to NAME` for one bounded write, `oms autopilot` for reviewed plans. Read [delegation-artifacts.md](references/delegation-artifacts.md) and [model-routing.md](references/model-routing.md) before dispatch. |
| Verification and landing | Run local checks directly. `oms peer-review --gate` is for authorized peer review, not every edit. `patch-land` admits/applies worker patches; `oms land` gates/pushes/updates a committed worktree when authorized. |
| Continuity | Journal for history, agent-memory for stable facts, session-handoff for local sessions, runtime capsule for sanitized transfer. See [state-memory.md](references/state-memory.md). |
| Operations | `oms doctor` for health, `oms update` for install updates, `oms tick` for scheduled repo maintenance. Optional profile/backend/experiment flows use [runtime-core.md](references/runtime-core.md). |

Use `oms list` for entrypoints, command routing for variants,
and `oms list --all` for compatibility primitives. Read only references
needed for the current decision, not the entire catalog.

Work locally unless the user or applicable instructions explicitly request
subagents or delegation. Task size or routine complexity alone is not permission
to launch a worker. Questions do not authorize edits or autopilot.
Avoid duplicate collectors, worker calls, or full-conversation replay.

For skill evaluation/import, load
[skill-lifecycle.md](references/skill-lifecycle.md). For MCP Tasks or Codex
app-server, load
[interoperability.md](references/interoperability.md); all are optional
projections, not authority.

## Invariants

- Keep secrets, private paths, machine details, raw logs/data, and checkpoints
  out of prompts and shared state.
- Do not hand-edit `.oms/`; use typed tools and append-oriented records.
- Provider workers cannot recursively delegate, commit, push, or widen scope.
- Advisors are for irreversible/high-risk decisions, repeated failure, and release go/no-go—not routine completion.
- `plan-run` stops in review unless landing was explicitly authorized.
- `trusted-local` is supervision, not a sandbox; distinguish declared,
  enforced, and unknown capabilities.
- Portable capsules contain no lease, approval, command, credential, absolute
  path, raw transcript/log, patch, or publication right.
- Bind completion only to existing fresh evidence; model confidence is not a
  gate.

## References

- Command routing: [command-routing.md](references/command-routing.md)
- Shared decisions: [shared-projections.md](references/shared-projections.md)
- Graphs: [graphs.md](references/graphs.md)
- Plans, failures, recovery: [plans-recovery.md](references/plans-recovery.md)
- Worker roles: [roles.md](references/roles.md)
- Consultation: [cross-agent-consultation.md](references/cross-agent-consultation.md)
- Review/release gates: [review-gates.md](references/review-gates.md)
- Prior session: [session-handoff.md](references/session-handoff.md)
- Evidence-first changes: [minimal-change.md](references/minimal-change.md)
