# Contributing

## Verify and publish

During development, run the nearest affected tests and static checks. The
smoke runner accepts repeatable selectors:

```bash
bash tests/run-smoke-shard.sh --only TEST_NAME
```

For a committed range, use `bash scripts/check.sh --affected --changed-from BASE --changed-to HEAD`.
Ordinary docs use reference checks; critical changes or
uncertain impact select full coverage. Shell lint requires ShellCheck.

When publication is authorized, finish the scoped commits and run `oms land`.
It owns the gate, unchanged-tree push, CI receipt and eligible installation
update. Do not run a duplicate full gate first. See
[verification and release](docs/COMPONENTS.md#verification-and-release) for CI
scope, installation proof and direct-push requirements.

## Style

- Bash 3.2, `set -euo pipefail`, GNU/BSD-compatible on Linux, WSL2 and macOS.
- Keep changes task-scoped; match the surrounding script's conventions.
- Extend the canonical test or fixture; add a new test only for an uncovered
  path, and look for coverage it supersedes. The smoke runner discovers every
  `test_*` definition automatically. Prefer behavior/interface tests.
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
