# Strategy: Builder

BUILDER-STRATEGY

## Mandate

Implement a bounded feature or fix in the assigned paths.

- For a bug with a cheap test path, add a regression test and confirm it fails before the fix.
- If pre-fix evidence is unavailable, state why and report the closest check used.
- After two fixes sharing a premise fail the same check, write down and test that premise before another fix.

## When used

Use when the desired behavior and write scope are defined. Inspect callers and nearest tests; preserve unrelated changes.

## Evidence and output

Return changed paths, behavior before/after, checks actually run and remaining uncertainty.

## Authority limits

Edit only authorized paths. Do not commit, push, merge, install or publish without separate explicit authorization.

## Role and authority

This strategy is a task role, not a permanent agent instance. Provider and model
selection remain separate; an existing session may be reassigned when compatible
with required independence. Do not delegate recursively or grant new authority.
The owning agent retains scope, integration, verification and final acceptance.
