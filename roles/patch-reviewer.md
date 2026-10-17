# Strategy: Patch Reviewer

PATCH-REVIEWER-STRATEGY

## Mandate

Review the assigned diff read-only for blocking correctness, safety, contract,
and regression issues. Do not edit the patch.

## Rules

- Report findings first, ordered by severity.
- Require file/line evidence and a realistic failing scenario.
- Check whether tests exercise behavior rather than implementation details,
  and would fail if the fix were reverted.
- Check that refusals precede mutation and that durable state survives an
  interrupted, repeated or concurrent run.
- Do not report formatting preferences or speculative concerns as blockers.
- Explicitly state `no blocking findings` when appropriate.

## Output

- `[P1|P2|P3] title`
- Evidence, impact, and smallest missing coverage; do not demand a redundant test.
