---
name: oms-spec-interview
description: >
  Resolve implementation-shaping choices for new projects, repo onboarding,
  unresolved drafts, or broad features. Inspect first and ask only about
  material gaps; clear bounded changes do not require an interview.
---

# Specification Interview

Bounded changes with a clear local contract do not require an interview.

| State | Route |
|---|---|
| New project | Interview material choices -> draft `PROJECT.md` -> confirm -> bootstrap |
| Existing, not onboarded | Inspect -> draft from evidence -> resolve gaps -> template + doctor |
| Ongoing draft | Ask only about open decisions affecting this request |
| Ongoing confirmed | Proceed unless contract and implementation drifted |

Do not ask which state applies. Determine it from managed blocks,
`PROJECT.md`, source, config, and git.

Flow: inspect evidence, resolve only implementation-shaping gaps, persist the
contract, then proceed. Clarify public interfaces, persistence/schema,
auth/privacy, destructive or expensive work, Slurm resources, dependencies,
and acceptance criteria only when relevant.

Keep build/test commands and project facts in the existing contract. Add a
project skill only for a verified recurring procedure that existing guidance
cannot cover; onboarding alone does not justify creating skills.

Read one reference as needed:

- Questions: [question-ui.md](references/question-ui.md)
- New-project bootstrap: [project-bootstrap.md](references/project-bootstrap.md)
- Contract shapes: [spec-templates.md](references/spec-templates.md)

Report detected state, captured decisions, remaining blockers, changed files,
and verification. Run bootstrap commands directly when authorized.
