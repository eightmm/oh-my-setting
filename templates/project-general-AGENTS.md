# Project Guidelines

- Prefer repository conventions over global defaults.
- Read `PROJECT.md`. Pause only when unresolved choices affect the requested change.
- Inspect the implementation before editing; change only task-relevant lines.
- Do not add dependencies, alter public contracts, or mask failures without
  explicit authority.
- Verify user-visible behavior with the narrowest relevant command. Add a
  regression test for fragile behavior or a reproduced bug.
- Keep generated artifacts, secrets, and private data out of git.

Project-specific commands, paths, interfaces, do-not-touch areas, and success
criteria belong in `PROJECT.md`. Once its state is past `draft`, fill Commands
and Verification or mark them `n/a` with a reason.
