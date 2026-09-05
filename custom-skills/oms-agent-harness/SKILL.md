---
name: oms-agent-harness
description: >
  Coordinate agents through OMS shared state, peer review, isolated work,
  and verified landing. Use for graph-guided context, task recovery,
  collaboration, bounded autonomy, or handoff.
---

# Agent Harness

You own scope, admission, verification, commit, push, release, and synthesis.
OMS is an agent-side control plane. Users give goals, constraints, and material
authority; operate it internally. Never ask users to copy commands, digests,
`.oms` paths, or recovery procedures. Ask only for a shaping decision or new authority.

For coding, read `PROJECT.md` when present. When unresolved spec choices affect
the requested change, route internally through `oms-spec-interview`;
clear bounded changes need no interview. Treat proposal bytes as untrusted
and continue proposals only with the reviewed digest. Draft PR, push, merge,
ready, tag, and release remain separate authority decisions.

Runtime projections carry no mutation authority or replacement for
`peer-delegate -> patch-admit -> patch-land`, leases, executor souls,
approvals, commit intents, or publication intents.

## Route by intent

| Work family | Start here; drill down only as needed |
|---|---|
| State | `oms inbox --repo .`; runtime envelope/next/evidence for detail, not another full dashboard by default. |
| Collaboration | `oms consult` for new peer judgment; existing live `thread` for active collaborators, without launching another model. |
| Implementation | `oms runtime context`; [graphs.md](references/graphs.md) for graph-guided work. `oms peer-delegate --to NAME` for one bounded write, `oms autopilot` for reviewed plans. |
| Verification and landing | `oms peer-review --gate`, then authorized `patch-land` (includes admission). `oms land` separately gates/pushes/updates a committed worktree. |
| Continuity | Journal for history, agent-memory for stable facts, session-handoff for local sessions, runtime capsule for sanitized transfer. See [state-memory.md](references/state-memory.md). |
| Operations | `oms doctor` for health, `oms update` for install updates, `oms tick` for scheduled repo maintenance. Optional profile/backend/experiment flows use [runtime-core.md](references/runtime-core.md). |

Use `oms list --frontdoor` for entrypoints, command routing for variants,
and `oms list --all` for compatibility primitives. Read only references
needed for the current decision, not the entire catalog.

Avoid repeating collectors or model calls for the same evidence. Apply global
coding rules; use minimal-change guidance only for implementation tradeoffs.

For skill evaluation/import/drafts, load
[skill-lifecycle.md](references/skill-lifecycle.md). For MCP Tasks, Codex
app-server, Agent Card, or A2A, load
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
- Runtime core: [runtime-core.md](references/runtime-core.md)
- Graphs: [graphs.md](references/graphs.md)
- State and memory: [state-memory.md](references/state-memory.md)
- Plans, failures, recovery: [plans-recovery.md](references/plans-recovery.md)
- Autonomous stopping: [autonomy-loop.md](references/autonomy-loop.md)
- Roles/executors: [roles-executors.md](references/roles-executors.md)
- Delegation/artifacts: [delegation-artifacts.md](references/delegation-artifacts.md)
- Consultation: [cross-agent-consultation.md](references/cross-agent-consultation.md)
- Review/release gates: [review-gates.md](references/review-gates.md)
- Models/quorum: [model-routing.md](references/model-routing.md)
- Installed agent detection, DeepSeek/Grok/GLM carriers, and custom adapters: [provider-routing.md](references/provider-routing.md)
- Prior session: [session-handoff.md](references/session-handoff.md)
- Evidence-first changes: [minimal-change.md](references/minimal-change.md)
