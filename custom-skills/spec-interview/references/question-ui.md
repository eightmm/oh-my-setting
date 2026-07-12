# Question UI

Ask only questions whose answers change implementation, authority, risk, or
verification. Stop when remaining unknowns are local and reversible.

Use the native question UI when available (`AskUserQuestion` in Claude Code,
`request_user_input` in interactive Codex). Use plain Markdown only when no
question UI exists or the session is non-interactive.

- Ask one decision at a time.
- Offer two to four mutually exclusive choices when known.
- Mark a clearly best default as recommended and state its tradeoff briefly.
- Keep a free-form escape hatch.
- Do not ask for information already present in source, config, git, or
  `PROJECT.md`.
- Do not turn implementation details the agent can safely decide into user
  questions.

Fallback shape:

```md
1. Which compatibility boundary applies?
   A. Preserve the current CLI exactly (recommended) — lowest migration risk.
   B. Allow a versioned breaking change — smaller implementation.
   C. Other: describe the boundary.
```
