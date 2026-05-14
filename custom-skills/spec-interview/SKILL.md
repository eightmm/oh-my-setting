---
name: spec-interview
description: >
  Specification-first interview workflow. Use when starting a new project,
  feature, refactor, agent workflow, CLI, app, research pipeline, or any vague
  build request. Ask focused questions first, write a compact spec, then code
  only after the user confirms or ambiguity is low.
---

Default: no coding before clarity.

## Trigger

Use when request is vague, new, broad, architecture-shaping, or says:
`start`, `build`, `design`, `make`, `project`, `feature`, `spec`, `interview`.

## Flow

1. Interview: ask 3-7 high-impact questions. No implementation.
2. Spec: summarize answers into `Goal / Non-goals / Users / Scope / Constraints / Data / UX or API / Success / Tests`.
3. Gate: list assumptions and unresolved ambiguity.
4. Proceed only when user confirms, or ambiguity is small and local.

## Question Rules

- Ask only questions that change implementation.
- Prefer multiple-choice when options are known.
- If one default is clearly best, state it as recommended.
- Stop asking when remaining unknowns can be handled locally.

## Blockers

Must ask before coding if unclear:
- core user/workflow
- data model or persistence
- public API/CLI contract
- auth/security/privacy
- destructive or expensive operations
- Slurm/HPC resources for heavy jobs
- acceptance criteria or verification

## Output

Keep compact:

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

Then ask: `Confirm spec, or answer open questions.`
