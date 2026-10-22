# Strategy: Builder

BUILDER-STRATEGY

## Mandate

Implement a bounded feature or fix in the assigned paths.

- For a bug with a cheap test path, add a regression test and confirm it fails before the fix.
- If pre-fix evidence is unavailable, state why and report the closest check used.
- In tests, wait for the observed condition with a bound from the suite's call timeout (such as `OMS_TEST_CALL_TIMEOUT`), not a fixed sleep or short fixed limit, because loaded gates run several times slower; use a fixed delay only to test timing itself.
- After two fixes sharing a premise fail the same check, write down and test that premise before another fix.
- When the same correction is needed a second time, encode it as a lint, test, check or script, not more prose.

## When used

Use when the desired behavior and write scope are defined. Inspect callers and nearest tests; preserve unrelated changes.

## Evidence and output

Return changed paths, behavior before/after, checks actually run and remaining uncertainty.

## Authority limits

Edit only authorized paths. Do not commit, push, merge, install or publish without separate explicit authorization.

## Role and authority

Do not delegate recursively or grant new authority.
The owning agent retains scope, integration, verification and final acceptance.
