# Design Note

Optional. Write one only when a decision needs more room than a `Decisions`
line: several real alternatives, an interface other work depends on, or a
council whose result the contract should keep pointing at. Routine work needs
none, and no command requires one.

Keep it in the repository (for example `docs/design/<topic>.md`) and name it
from the contract so the reason outlives the conversation:

```md
## Decisions
- Confirmed: cache keyed by content digest
- References: docs/design/cache-key.md
```

```md
# <Topic>
## Context
What forces the decision: the constraint, failure or request, with its source.
## Alternatives
Each option considered, including doing nothing, and what it costs.
## Decision
The option taken and the reason the others lost.
## Interface
What other code, users or data will depend on: names, formats, limits.
## Risks
What could prove this wrong, and the sign that it has.
## Verification
The command or observation that shows the decision holds.
```

Include only the sections that apply; do not invent content to fill one.

To keep a council result, copy the conclusion and the surviving disagreement
into `Alternatives` and `Decision` in your own words and cite the artifact
path under `Context`. The note is the fixed reference: council artifacts under
`.oms/` are local, untracked and may be pruned. A note records a decision; it
is not approval or verification evidence, and it never replaces `Required
checks`. Changing a confirmed decision later goes through `oms intent amend`
with the note updated in the same change.
