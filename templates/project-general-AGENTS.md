# Project Guidelines

- Prefer repository conventions over global defaults.
- Read `PROJECT.md` and relevant references; ask only about choices affecting this change.
- Inspect the implementation before editing; change only task-relevant lines.
- Do not add dependencies, alter public contracts, or mask failures without
  explicit authority.
- Run affected checks; reuse tests. Add coverage only for uncovered contracts,
  reproduced bugs or safety boundaries. Select related files/node IDs; broaden
  for changed inputs, failures, uncertainty or required release gates.
- Keep generated artifacts, secrets, and private data out of git.

Keep commands, paths, interfaces, protected areas and success criteria in
`PROJECT.md`. Past `draft`, fill Commands and Verification or explain `n/a`.
