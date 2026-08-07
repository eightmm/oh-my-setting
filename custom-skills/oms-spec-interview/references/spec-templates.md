# Spec Shapes

Minimum `PROJECT.md` sections:

```md
# PROJECT.md
## Status
- State: draft | confirmed
## Project
- Goal:
- Users/workflow:
- Scope:
- Non-goals:
## Interface and Data
- Public API/CLI/config:
- Persistence/schema:
- Inputs/outputs:
## Commands
- Setup:
- Test:
- Run:
## Verification
- Success criteria:
- Required checks:
## Decisions
- Confirmed:
- Open:
```

Keep the state draft while task-relevant decisions remain. Record paths,
resources, security constraints, and do-not-touch boundaries only when they
apply; use `n/a` with a reason rather than invented detail.

For non-project work, use a compact spec: goal, non-goals, scope, constraints,
interface/data, success criteria, verification, assumptions, and open
questions. Ask for confirmation only when material choices remain.
