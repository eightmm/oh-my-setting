---
name: oms-agent-harness
description: >
  Shared state, graph-guided work, typed runtime projections, recovery, peer consultation and
  review, isolated delegation, patch landing, bounded autonomy, experiments,
  Draft PRs, and cross-provider handoff.
---

# Agent Harness

You own scope, admission, verification, commit, push, release, and synthesis.
OMS is an agent-side control plane. Users give goals, constraints, and material
authority; operate it internally. Never ask users to copy commands, digests,
`.oms` paths, or recovery procedures. Ask only for a shaping decision or new authority.

For authorized coding, confirm `PROJECT.md`, treat proposal bytes as untrusted,
and continue only with the reviewed digest. Draft PR,
push, merge, ready, tag, and release are separate authority decisions. If the
spec is absent, draft, or drifted, route internally through
`oms-spec-interview`.

The runtime projects state and executes; it never replaces
`peer-delegate -> patch-admit -> patch-land`, plan leases, executor souls,
approvals, commit intents, or publication intents. Runtime snapshots, capsules,
context bundles, receipts, and evidence carry no mutation authority.

## Route by intent

| Need | Agent action |
|---|---|
| orient, resume, or inspect completion | `oms inbox --repo .`; then `oms runtime envelope show`, `next`, or `evidence show` |
| inspect code or graphs | load [graphs.md](references/graphs.md); context for coding, map/status for orientation |
| preserve knowledge | `oms agent-memory recall`; pin stable facts; forge recurring procedures |
| independent judgment | `oms consult`; use `oms peer-ask` for several peers and `oms advise` for high-risk/repeated failure |
| bounded write | `oms peer-delegate --to NAME`; dependent work uses `agent-plan` and `plan-run` |
| implement confirmed scope | review `oms autopilot … propose`, then run its digest-bound continuation |
| judge a diff | `oms peer-review --gate`; peer agreement is evidence, never admission |
| context or continuity | `oms runtime context`; use sanitized `oms runtime capsule export|verify|import`, never raw `.oms` |
| optional host capability | `oms runtime profile check|install-plan|install` |
| execution backends | `oms runtime backend check|run`; execution receipts cannot land code |
| comparable research study | `oms runtime experiment register|run|invariants|summarize` |
| land an authorized worktree | `oms land`; read `oms land status`; detached gate/push/update/CI; receipt outside repo, not `patch-land`. |
| schedule adopted-repo maintenance | `oms tick register`, then `oms tick install`; hourly sweep; GC needs `OMS_TICK_GC=1`. |
| admit or land reviewed bytes | use `oms patch-admit`; `oms patch-land` admits before mutation |
| publish an authorized Draft PR | use the parent-owned Draft PR path; no update, merge, ready, tag, or release authority |

Use `oms list --frontdoor` for subsystem entrypoints. Load the command
routing reference for intent variants; use `oms list --all` only when a
compatibility primitive is needed.

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

Retain internal provenance and report only useful conclusions, changed behavior,
verification, skipped checks, and genuine blockers.
