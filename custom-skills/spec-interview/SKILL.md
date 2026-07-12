---
name: spec-interview
description: >
  Specification-first workflow for new projects, existing-repo onboarding,
  unresolved draft specs, and broad or architecture-shaping features. Detect
  project state before asking questions and resolve only choices that affect
  implementation. Clear bounded changes do not require an interview.
---

# Specification Interview

Do not code before the task-relevant contract is clear. Do not force an
interview for a bounded change with a clear local contract.
Bounded changes with a clear local contract do not require an interview.

## State Router

| State | Route |
|---|---|
| New project | Interview intent/scope/execution -> draft `PROJECT.md` -> confirm -> bootstrap |
| Existing repo, not onboarded | Inspect code/config/git -> draft from evidence -> resolve material gaps -> confirm -> apply template -> doctor |
| Ongoing draft | Read open decisions; ask only when they affect this request |
| Ongoing confirmed | Read current task/verification; proceed unless spec and reality drifted |

Use managed block markers, `PROJECT.md` state, and the source tree as evidence;
do not ask the user which state applies.

## Flow

1. Inspect before asking.
2. Clarify goal, users, and non-goals, then interface, data, paths, and constraints, then
   success criteria/verification/resources. Skip questions answered by evidence.
3. Write or update `PROJECT.md` for project work; use a compact task spec for
   non-project work.
4. List assumptions and unresolved material decisions.
5. Proceed after confirmation. A new-project bootstrap requires confirmed
   `PROJECT.md`; a bounded existing change may proceed from its local contract.

Must resolve uncertainty about public API/CLI, persistence/schema, auth/privacy,
destructive or expensive operations, Slurm resources, dependencies, and
acceptance criteria when it affects the requested work.

## Route to One Reference

- Native question UI, fallback format, and question discipline:
  [question-ui.md](references/question-ui.md)
- New-project template selection and safe bootstrap:
  [project-bootstrap.md](references/project-bootstrap.md)
- `PROJECT.md` and compact non-project spec shapes:
  [spec-templates.md](references/spec-templates.md)

Report detected state, evidence used, decisions captured, remaining blockers,
files created/changed, and verification. Never tell the user to run bootstrap
commands manually when the current agent can run them.
