# Question UI

Ask only questions whose answers change implementation, authority, risk, or
verification. Stop when remaining unknowns are local and reversible.

Use a question tool only when it is available and permitted in the current
session, and follow that tool's constraints. Otherwise ask concisely in plain
text.

- Each question must concern one decision. Batch independent material
  questions when the tool permits and within its limits.
- Offer two to four mutually exclusive choices when known.
- Mark a clearly best default as recommended and state its tradeoff briefly.
- Keep a free-form escape hatch.
- Do not ask for information already present in source, config, git, or
  `PROJECT.md`.
- Do not turn implementation details the agent can safely decide into user
  questions.
- Continue authorized work that does not depend on the answer while waiting.
  Silence is never approval.

Fallback shape:

```md
1. Which compatibility boundary applies?
   A. Preserve the current CLI exactly (recommended) — lowest migration risk.
   B. Allow a versioned breaking change — smaller implementation.
   C. Other: describe the boundary.
```
