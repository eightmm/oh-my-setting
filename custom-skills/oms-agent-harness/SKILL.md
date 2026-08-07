---
name: oms-agent-harness
description: >
  Shared state and multi-agent coordination for resume, memory, plans, recovery,
  roles/executors, provider/model routing, peer consultation, independent diff
  review, isolated write delegation, patch landing, artifacts, and session
  handoff across Codex, Claude Code, and Antigravity.
---

# Agent Harness

You own scope, admission, verification, commit, push, release, and synthesis.

## Find it by what you are about to do

Use the table by intent. Authority distinguishes reads, appends, isolated
worktree writes, and repository writes.

| About to… | Do this | Authority |
|---|---|---|
| start or resume and the state is unclear | `oms inbox --repo .`; use `oms state --repo .` for full detail (`oms init` only for a fresh repo) | read |
| check what is already known, or record what you learned | `oms agent-memory recall "…"`; `append`/`pin` to add | read, append |
| record a fact derived from checked-in code/docs | `oms agent-memory append --source-file PATH --source-line N --text "…"` | append |
| catch yourself re-deriving the same fix or procedure | `oms skill-forge add --name NAME` — it becomes a project skill every CLI loads | append |
| decide, and an outside view would change the next step | `oms consult "question"`; use `oms peer-ask --prompt "…"` for the same question to several peers | read |
| act irreversibly, or fail the same way twice | `oms fail-ledger check --cmd "…"`, then `oms advise` | read |
| hand work to another model | `oms peer-delegate --to NAME` (returns a patch) | worktree write |
| split work so several agents can proceed | `oms agent-plan add`, then `oms plan-run` | worktree write |
| judge a diff before it goes anywhere | `oms peer-review --gate` | read |
| ask what agents cost or how often they verified/delegated | `oms artifact-index telemetry` (routes, outcomes, native activity, tokens, wall time) | read |
| preserve local staged/unstaged state before a risky edit | `oms checkpoint create --label "…"`; restore is dry-run unless `--apply` | repo write |
| put a reviewed change into the tree | `oms patch-admit`, then `oms patch-land` | repo write |
| run one provider pass at a known boundary | `oms agent-run --mode read\|write` | read or worktree write |
| keep a worker in one persona across calls | `oms agent-role`, `oms agent-executor` | — |
| choose a provider model or effort | inspect `oms models`, then pass `--model`/`--effort`; see the routing reference | — |
| decide whether you are done or should continue | see the autonomy reference | — |

`oms list` catalogs every tool; each answers `oms <tool> --help`.

## Lines that do not move

- Keep secrets, private paths, machine details, raw logs/data, and checkpoints
  out of prompts and shared state.
- Do not hand-edit `.oms/`; prefer tool JSON. `oms gc` clears stale state.
- Provider workers are harness children: they cannot delegate again and never
  gain commit or push authority.
- Advisors are for irreversible or high-risk decisions, repeated failures, and
  release go/no-go — not routine completion.
- `plan-run` executes one task and stops in review unless landing was
  explicitly authorized.

## When a row is not enough

- Prompt hooks, memory, active task, project skills, change guard:
  [state-memory.md](references/state-memory.md)
- Plans, fail ledger, reclaim, GC: [plans-recovery.md](references/plans-recovery.md)
- Autonomous progress and stopping: [autonomy-loop.md](references/autonomy-loop.md)
- Roles and executor souls: [roles-executors.md](references/roles-executors.md)
- Provider calls, artifacts, export/import, landing:
  [delegation-artifacts.md](references/delegation-artifacts.md)
- Consulting peers and shared conversation threads:
  [cross-agent-consultation.md](references/cross-agent-consultation.md)
- Diff review, release gates, and export/import:
  [review-gates.md](references/review-gates.md)
- Model selection, capability checks, quorum diversity:
  [model-routing.md](references/model-routing.md)
- Prior provider session: [session-handoff.md](references/session-handoff.md)

Report the provider, the useful conclusion, the artifact or patch, the landing
decision, and any failed or skipped verification.
