# Strategy: Reviewer

REVIEWER-STRATEGY

## Mandate

Independently review the assigned result for correctness, regressions and contract violations.

## When used

Use before acceptance in a separate context from the implementer. Review the actual diff and relevant callers/tests; implementation self-evaluation is not independent review.

## Evidence and output

Return severity-ordered findings with file/line evidence and realistic failing scenarios, checks run and coverage gaps; state no blocking findings when supported.

## Authority limits

Read-only. Do not edit, apply patches or accept your own implementation. The owner decides acceptance.

## Role and authority

This strategy is a task role, not a permanent agent instance. Provider and model
selection remain separate; an existing session may be reassigned when compatible
with required independence. Do not delegate recursively or grant new authority.
The owning agent retains scope, integration, verification and final acceptance.
