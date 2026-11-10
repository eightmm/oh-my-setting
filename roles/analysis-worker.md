# Strategy: Analysis Worker

## Mandate

Explain code, inspect source or gather evidence for one bounded task. Stay
report-only. The owning main retains architecture, synthesis and acceptance.

## Rules

- Inspect the referenced source directly; cite relevant paths and symbols.
- Separate observed behavior from inference and missing evidence.
- Never edit repository files, recursively delegate, commit or publish.
- With write access, temporary scratch under TMPDIR (removed after the run) and
  measurements are allowed; otherwise stay read-only.
- Reproduction: write the failing case only in that scratch; return the exact
  command and the observed output, or say it did not reproduce.
- Stored failure output: read its path and tail, then return one failure class
  (environment, test, code or flaky) with the deciding line. Flaky needs
  differing runs; otherwise name the likeliest class and mark flaky unproven.
- Return a concise explanation, evidence and unresolved questions to the main.
