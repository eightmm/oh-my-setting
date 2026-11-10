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

### Harness evaluation scenarios

`bash tests/harness-eval-smoke.sh [--json] [--only NAME]` asks whether the
harness as a whole keeps five promises across a realistic sequence: a resumed
drive repeats no work, verification or landing; a reworded criterion makes its
evidence stale; an unrelated verification receipt does not verify a review
finding; the panel shows what the lifecycle records say (and does not show an
unreadable source as live); and the note a task needs fits a bounded
session-start block with its source id. Each scenario runs real `oms` commands
in a temp repository with fake provider CLIs, no model call and no network, then
asserts on recorded state. It is a regression instrument for harness behavior,
not a benchmark of model quality. A scenario for a known defect is listed in
`xfail_branch` with the branch that fixes it and prints `xfail(BRANCH)`; once
that fix lands it reports `unexpected-pass` and the suite fails until the entry
is removed. A scenario the environment cannot run exits 77 and prints
`skip(REASON)`.

The `hook-*` scenarios check the registered Claude Code guards (the
`PreToolUse` rows of `scripts/install-claude-hooks.sh --print-expected`:
`command-guard-hook.sh` and `git-move-guard-hook.sh`) by driving each script
with the JSON payload the provider sends. A refused payload exits 2 with one
reason line on stderr; an allowed one exits 0 with nothing on stderr; the
registered matcher equals the tool the script's payload check expects (a grep
over the source); and removing or altering a guard row in a fixture
`settings.json` is reported by `oms doctor` in both modes. For a degraded
environment (no `python3` on PATH, Bash older than 5, a truncated payload, the
helper library missing) the property is that a guard never passes a payload it
would refuse without a diagnostic on stderr. Fail-open with a diagnostic holds;
silent fail-open does not, and is listed in `xfail_branch` as
`owner-decision:SLUG` until the hook's owner rules on it. The suite ends with a
hook-by-condition matrix (`blocks`, `passes+diagnostic`, `passes silently`; also
under `hook_matrix` with `--json`). A new `PreToolUse` row fails the matcher
scenario until it has conformance scenarios of its own.

## Style

- Bash 5.0+, `set -euo pipefail`, GNU/BSD-compatible on Linux, WSL2 and macOS.
  Only the prelude of `install.sh` above its Bash 5 check stays parseable by
  Bash 3.2, so macOS's system Bash can find a newer Bash or print the hint.
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
