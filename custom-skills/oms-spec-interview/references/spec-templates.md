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
- Required check files:
## Decisions
- Confirmed:
- Open:
```

Keep the state draft while task-relevant decisions remain. Record paths,
resources, security constraints, and do-not-touch boundaries only when they
apply; use `n/a` with a reason rather than invented detail.

Keep `Required checks` to one executable Markdown line. One complete inline
code wrapper is allowed (for example, `` `bash scripts/check.sh` ``). For a
custom or composed command, list every repo-relative regular file that defines
the verifier under `Required check files` (comma-separated; at most 64 paths,
240 bytes each). Conventional project check entrances such as
`scripts/check.sh`, `tests/run.sh`, `make test`, or a package-manager test
command can be discovered automatically; their verifier floor is enforced by
the harness even when the explicit file list is empty.

For non-project work, use a compact spec: goal, non-goals, scope, constraints,
interface/data, success criteria, verification, assumptions, and open
questions. Ask for confirmation only when material choices remain.
