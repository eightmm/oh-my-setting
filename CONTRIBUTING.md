# Contributing

## Before you push

Run the core gate shared with CI (CI also runs install E2E and macOS fixtures):

```bash
bash scripts/check.sh
```

This runs `shellcheck -x -S warning` over `install.sh`, `scripts/*.sh`, and
`tests/*.sh`, then the smoke suite (`tests/scripts-smoke.sh`). It fails hard if
shellcheck is missing — never a silent skip.

## Style

- Bash, `set -euo pipefail`, POSIX-portable where practical (CI checks macOS).
- Keep changes task-scoped; match the surrounding script's conventions.
- New behavior needs a test in `tests/scripts-smoke.sh` (register the function
  in the runner list at the bottom). Prefer behavior/interface tests.
- No secrets in code, tests, commits, or fixtures — the suite self-scans its own
  sources; split secret-shaped literals (e.g. `'aws_secret_access_''key=...'`).
- Fail closed: a check that cannot run is an error, not a pass.

## Commits

Conventional Commits, subject <= 50 chars, body only for non-obvious why/risk:

```
feat(data-manifest): fingerprint id->key mapping
fix(run-ledger): block sensitive gate skip reasons
docs: ...
```

## Scope of changes to flag

Ask before changing: install/auto-update defaults, the symlink/backup behavior,
external toolchain installation, public CLI/flag contracts, or manifest/ledger
schema. These have a large blast radius (they touch user home dirs and agent
configs).
