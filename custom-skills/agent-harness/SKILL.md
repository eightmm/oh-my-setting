---
name: agent-harness
description: >
  Shared state and multi-agent coordination for resume, memory, plans, recovery,
  roles/executors, provider delegation, patch landing, artifacts, and session
  handoff across Codex, Claude Code, and Antigravity.
---

# Agent Harness

The current agent owns scope, admission, final verification, commit, push,
release, and synthesis.

- On resume, run `oms state --repo .`; use `oms init` only for a fresh repo.
- Keep secrets, private paths, machine/cluster details, raw logs, datasets, and
  checkpoints out of prompts and shared state.
- Do not edit `.oms/` manually. Use `oms gc` for stale state and
  `oms patch-land` for delegated changes.
- Use advisors only for irreversible/high-risk decisions, repeated failures, or
  release go/no-go—not routine completion.
- Provider workers remain harness children, cannot recursively delegate, and do
  not gain commit/push authority.

Read only the reference needed:

- Prompt hooks, memory, active task, change guard:
  [state-memory.md](references/state-memory.md)
- Plans, fail ledger, reclaim, GC: [plans-recovery.md](references/plans-recovery.md)
- Autonomous progress and stopping: [autonomy-loop.md](references/autonomy-loop.md)
- Roles and executor souls: [roles-executors.md](references/roles-executors.md)
- Provider calls, artifacts, export/import, landing:
  [delegation-artifacts.md](references/delegation-artifacts.md)
- Prior provider session: [session-handoff.md](references/session-handoff.md)

For one provider pass, use `oms agent-run --mode read|write`; set the mode when
the authority boundary is known. For a pre-authorized plan task, use
`oms plan-run` for one task and stop in review unless landing was explicitly
authorized.

Report the provider, useful conclusion, artifact/patch, landing decision, and
failed or skipped verification.
