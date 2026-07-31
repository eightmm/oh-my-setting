---
name: agent-harness
description: >
  Shared state and multi-agent coordination for resume, memory, plans, recovery,
  roles/executors, provider/model routing, patch landing, artifacts, and session
  handoff across Codex, Claude Code, and Antigravity.
---

# Agent Harness

You own scope, admission, final verification, commit, push, release, and
synthesis. Everything below is reached for from inside a task.

## Find it by what you are about to do

Each row names the authority it takes. A read costs a call; a worktree write is
isolated and comes back as a reviewable patch; a repo write changes the tree you
are working in. Do not cross those lines by accident — that is why they are on
every row rather than grouped by feature.

| About to… | Do this | Authority |
|---|---|---|
| start or resume and the state is unclear | `oms state --repo .` (`oms init` only for a fresh repo) | read |
| check what is already known, or record what you learned | `oms agent-memory recall "…"`; `append`/`pin` to add | read, append |
| decide, and an outside view would change the next step | `oms consult "question"` (`--all` for every peer) | read |
| act irreversibly, or fail the same way twice | `oms fail-ledger check --cmd "…"`, then `oms advise` | read |
| hand work to another model | `oms peer-delegate --to NAME` (returns a patch) | worktree write |
| split work so several agents can proceed | `oms agent-plan add`, then `oms plan-run` | worktree write |
| judge a diff before it goes anywhere | `oms peer-review --gate` | read |
| ask what the peer calls cost, or how often they verified | `oms artifact-index telemetry` (routes, exits, fallbacks, tokens, wall time) | read |
| put a reviewed change into the tree | `oms patch-admit`, then `oms patch-land` | repo write |
| run one provider pass at a known boundary | `oms agent-run --mode read\|write` | read or worktree write |
| keep a worker in one persona across calls | `oms agent-role`, `oms agent-executor` | — |
| pin a model tier or thinking level | routing follows the work phase on its own; see the routing reference | — |
| decide whether you are done or should continue | see the autonomy reference | — |

`oms list` catalogs every tool; each answers `oms <tool> --help`.

## Lines that do not move

- Keep secrets, private paths, machine/cluster details, raw logs, datasets, and
  checkpoints out of prompts and shared state.
- Never write `.oms/` by hand. Reading its files is fine; prefer `--json` where
  a tool offers it. `oms gc` clears stale state.
- Provider workers are harness children: they cannot delegate again and never
  gain commit or push authority.
- Advisors are for irreversible or high-risk decisions, repeated failures, and
  release go/no-go — not routine completion.
- `plan-run` executes one task and stops in review unless landing was
  explicitly authorized.

## When a row is not enough

- Prompt hooks, memory, active task, change guard:
  [state-memory.md](references/state-memory.md)
- Plans, fail ledger, reclaim, GC: [plans-recovery.md](references/plans-recovery.md)
- Autonomous progress and stopping: [autonomy-loop.md](references/autonomy-loop.md)
- Roles and executor souls: [roles-executors.md](references/roles-executors.md)
- Provider calls, artifacts, export/import, landing:
  [delegation-artifacts.md](references/delegation-artifacts.md)
- Consulting peers and shared conversation threads:
  [cross-agent-consultation.md](references/cross-agent-consultation.md)
- Model selection, capability checks, quorum diversity:
  [model-routing.md](references/model-routing.md)
- Prior provider session: [session-handoff.md](references/session-handoff.md)

Report the provider, the useful conclusion, the artifact or patch, the landing
decision, and any failed or skipped verification.
