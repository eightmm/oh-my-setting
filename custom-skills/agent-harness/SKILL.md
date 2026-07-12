---
name: agent-harness
description: >
  Shared multi-agent harness state and coordination for Codex, Claude Code,
  and Antigravity. Use for repo resume/state, shared memory or active-task
  handoff, plan claims/recovery, fail-ledger checks, stale `.oms` cleanup,
  role/executor setup, local provider calls or write delegation, patch
  admission/landing, artifact provenance, and prior-session handoff.
---

# Agent Harness

Keep cross-agent work reproducible while the current agent remains the owner.

## Core Rules

- On resume, run `oms state --repo .`; use `oms init` only for a fresh repo.
- Keep secrets, machine/cluster details, raw logs, datasets, checkpoints, and
  private paths out of prompts and shared state.
- The parent owns scope, plan approval, verification, patch landing, commit,
  push, release, and final synthesis.
- Use an advisor only for irreversible/high-risk decisions, repeated failures,
  or a release go/no-go; routine completion does not require one.
- Use `oms gc` for stale claims, workers, guards, runs, and retained artifacts.
  Do not repair `.oms/` by hand.
- Use `oms agent-run --mode read` for one read-only provider pass and
  `--mode write` for isolated worktree delegation. Do not rely on `auto` when
  the authority boundary is already known.
- Land delegated changes through `oms patch-land`; a worker patch is evidence,
  not permission to mutate the main tree.
- Provider subprocesses must remain harness children and must not recursively
  delegate or create state in their temporary worktree.
- For a pre-authorized plan task, prefer `oms plan-run` over manually composing
  claim, delegation, review, and landing. It executes one task only, stops in
  review by default, and requires explicit `--land` for main-tree mutation.

## Route to One Reference

Read only the reference needed for the request:

- Memory, active task, prompt hooks, live edit guards:
  [state-memory.md](references/state-memory.md)
- Plan DAG, fail ledger, liveness, reclaim, GC:
  [plans-recovery.md](references/plans-recovery.md)
- Autonomous task intake, bounded progress, verification, and stopping:
  [autonomy-loop.md](references/autonomy-loop.md)
- Strategy roles and task-scoped executor souls:
  [roles-executors.md](references/roles-executors.md)
- Provider calls, delegation, artifacts, export/import, patch landing:
  [delegation-artifacts.md](references/delegation-artifacts.md)
- Capturing and loading a prior provider session:
  [session-handoff.md](references/session-handoff.md)

For experiment claims, run provenance, Slurm, or local GPU queues, use the
`research-method`, `slurm-hpc`, or `tsp-queue` skill instead of loading harness
details unrelated to the task.

## Fast Paths

```bash
oms state --repo . --refresh-ci
oms agent-memory --repo . context
oms agent-task --repo . status
oms agent-plan --repo . ready
oms plan-run --repo . --to codex --next       # one task -> review
oms gc --repo .                 # dry-run
oms artifact-index --repo . unresolved
```

Report the provider called, useful conclusion, artifact or patch path, landing
decision, and every failed or skipped verification.
