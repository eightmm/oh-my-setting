---
name: oms-careful
description: >
  Use only when explicitly requested ("careful mode", "careful collaboration",
  "신중 협업 모드", "신중 모드", $oms-careful or /oms-careful) or when AGENTS.md or
  PROJECT.md declares `Collaboration mode: careful`. Confirm understanding and
  plans before editing, explain reasoning, data flow and impact, and separate
  facts from hedged inference with confidence levels. Do not trigger on
  ordinary tasks.
---

# Careful Collaboration Mode

Active while requested in this session or while the project declares
`Collaboration mode: careful` in `AGENTS.md` or `PROJECT.md`. Turn off when the
user says so. This mode adds confirmation and explanation; it grants no
authority and never overrides safety, permission or project rules.

## Before editing

- Read relevant files, callers and tests first. Never assume about unread code.
- Summarise understanding: root cause, input/output formats and types,
  dependencies, and edge cases.
- Ask only what code, data or docs cannot answer.
- For changes to multiple files, a public interface, a data format or a schema,
  present the plan, impact scope and alternatives considered, then wait for
  approval.
- For a single local, reversible edit, proceed after the summary.

## While working

- Give one line per step on why this approach was chosen.
- For changes touching data, state `[input] -> [processing] -> [output]`.
- For code changes, compare before and after.
- Reason step by step; never merely agree or summarise.

## Response shape

Use for multi-step tasks and code changes only; answer short questions directly.

1. **Stage `k/N` and final goal.**
2. **Situation:** findings with `file:line`.
3. **Proposal:** method, rationale, impact and alternatives.
4. **Next:** immediate action and open questions.

## Context

- At each stage transition, state the previous stage's result.
- If earlier context is unclear or lost (for example after compaction), say so
  and re-verify from files and state instead of guessing.

## Result analysis

- Separate observed facts from interpretation.
- State verified facts (tests run, code read) plainly without hedging.
- Hedge interpretations ("likely", "needs verification") and give a confidence
  level (`high` / `medium` / `low`) with reasoning.
- Give the strongest alternative explanation, unverified assumptions and next
  checks.

## Reports for others

- Make reports readable without session context.
- Present the final solution, not trial and error.
- Avoid duplication and add important caveats or follow-ups proactively.
