---
name: oms-spec-interview
description: >
  Resolve material specification gaps during project onboarding or feature
  design; clear bounded changes need no interview.
---

# Specification Interview

Clear bounded changes need neither an interview nor a separate spec. Inspect
local evidence and continue already-authorized work. Exploration and prototypes
must stay within current authority and not depend on unresolved material choices.

| State | Route |
|---|---|
| New project | Resolve material choices -> confirm `PROJECT.md` -> bootstrap; parent continues requested implementation |
| Existing, not onboarded | Inspect -> use current contract/evidence; resolve material gaps -> template + doctor when needed |
| Ongoing draft | Ask only about open decisions affecting this request |
| Ongoing confirmed | Proceed unless contract and implementation drifted |

Do not ask which state applies. Determine it from managed blocks,
`PROJECT.md`, source, config, and git.

Reuse decisions and approval already supplied for this request; record their
evidence without asking for redundant document approval. Use existing
confirmation/adoption procedures. A State label or model assumption grants no
new authority. Clarify public interfaces, persistence/schema,
auth/privacy, destructive or expensive work, Slurm resources, dependencies,
and acceptance criteria only when relevant.

Broad work and managed bootstrap still require a confirmed contract.
After bootstrap, continue implementation already authorized by the request.

Keep build/test commands and project facts in the existing contract. Add a
project skill only for a verified recurring procedure that existing guidance
cannot cover; onboarding alone does not justify creating skills.

Read one reference as needed:

- Questions: [question-ui.md](references/question-ui.md)
- New-project bootstrap: [project-bootstrap.md](references/project-bootstrap.md)
- Contract shapes: [spec-templates.md](references/spec-templates.md)

Report detected state, captured decisions, remaining blockers, changed files,
and verification. Run bootstrap commands directly when authorized.
