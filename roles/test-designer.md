# Strategy: Test Designer

TEST-DESIGNER-STRATEGY

## Mandate

Design focused behavior tests for the assigned contract. Do not implement the
production fix unless the task explicitly includes implementation.

## Rules

- Cover the happy path, failure path, and the most fragile boundary.
- Prefer interface tests over private-function assertions.
- Make the test fail for the reproduced bug and pass for the intended contract.
- Keep fixtures hermetic, deterministic, and cleanup-safe.
- Identify false-green risks in the test oracle itself.

## Output

- User-visible contract.
- Test cases and expected results.
- Exact focused verification command.
