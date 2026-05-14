---
name: spec-interview
description: >
  Specification-first interview workflow. Use when starting a new project,
  feature, refactor, agent workflow, CLI, app, research pipeline, or any vague
  build request. Ask staged questions first, write or update PROJECT.md for
  project work, then code only after the spec is confirmed.
---

Default: no coding before confirmed spec.

## Trigger

Use when request is vague, new, broad, architecture-shaping, or says:
`start`, `build`, `design`, `make`, `project`, `feature`, `spec`, `interview`.

## Flow

1. Stage 1 intent: ask goal, users/workflow, non-goals. No implementation.
2. Stage 2 scope: ask interface/API/CLI, data/files, paths, constraints.
3. Stage 3 execution: ask commands, verification, risks, resources.
4. Spec: write/update `PROJECT.md` for project work; otherwise write compact spec.
5. Gate: list assumptions and unresolved ambiguity.
6. Proceed only when user confirms. For project starts, `PROJECT.md` state must be `confirmed`.

## Question Rules

- Ask only questions that change implementation or verification.
- Prefer multiple-choice when options are known.
- If one default is clearly best, state it as recommended.
- Move stage by stage; do not ask every possible question at once.
- Stop asking when remaining unknowns are local and low risk.

## Blockers

Must ask before coding if unclear:
- core user/workflow
- project goal, scope, or non-goals
- data model or persistence
- public API/CLI contract
- auth/security/privacy
- destructive or expensive operations
- Slurm/HPC resources for heavy jobs
- acceptance criteria or verification
- missing or draft `PROJECT.md` for project start

## Output

For project start, create/update:

```md
# PROJECT.md

## Status
- State: draft | confirmed

## Interview
- Stage 1 intent:
- Stage 2 scope:
- Stage 3 execution:
- Open decisions:

## Project
- Name:
- Type:
- Goal:
- Users/workflow:
- Scope:
- Non-goals:

## Commands
- Setup:
- Test:
- Run:
- Lint/typecheck:

## Paths
- Data:
- Config:
- Outputs/logs:
- Checkpoints:

## Verification
- Success criteria:
- Required checks:
- Baseline/metric:
```

Keep `State: draft` while questions remain. Set `State: confirmed` only after user confirmation.

For non-project work, keep compact:

```md
Spec:
- Goal:
- Non-goals:
- Users:
- Scope:
- Constraints:
- Interface:
- Data:
- Success:
- Verification:
- Assumptions:
- Open questions:
```

Then ask: `Confirm PROJECT.md/spec, or answer open questions.`
