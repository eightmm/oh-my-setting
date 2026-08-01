---
name: spec-interview
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

After the contract lands, distill what inspection actually verified — build
and test invocations, repo-specific pitfalls, domain data rules — into at
most a few project skills with `oms skill-forge add --name NAME`. Evidence
only: a skill you cannot back with an inspected fact is context tax, not
knowledge. All three CLIs load them natively from the project.

Read one reference as needed:

- Questions: [question-ui.md](references/question-ui.md)
- New-project bootstrap: [project-bootstrap.md](references/project-bootstrap.md)
- Contract shapes: [spec-templates.md](references/spec-templates.md)

Report detected state, captured decisions, remaining blockers, changed files,
and verification. Run bootstrap commands directly when authorized.
