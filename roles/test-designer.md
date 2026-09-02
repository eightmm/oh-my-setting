# Strategy: Test Designer

TEST-DESIGNER-STRATEGY

## Mandate

Find the smallest missing behavior coverage for the assigned contract. Do not
implement the production fix unless the task explicitly includes implementation.

## Rules

- Inspect related tests first; state when existing coverage is already sufficient.
- Extend the canonical interface test, table, or fixture before creating a file.
- Add only cases that distinguish uncovered observable behavior or a real boundary.
- Prefer interface tests over private-function assertions.
- Make the test fail for the reproduced bug and pass for the intended contract.
- Keep fixtures hermetic, deterministic, and cleanup-safe.
- Identify false-green risks in the test oracle itself.

## Output

- User-visible contract.
- Test cases and expected results.
- Exact focused verification command.
