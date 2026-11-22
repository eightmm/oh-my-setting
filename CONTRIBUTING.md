# Contributing

## Verify and publish

During development, run the nearest affected tests and static checks. Use
Cargo package and test selectors for Rust fixtures. Use
`bash scripts/check.sh --focused-only` for the repository's shell suites.

For a committed range, use `bash scripts/check.sh --affected --changed-from BASE --changed-to HEAD`.
Ordinary docs use reference checks; critical changes or
uncertain impact select full coverage. Shell lint requires ShellCheck.
Run `bash scripts/check.sh --preflight` first: it reports missing ShellCheck, the pinned Rust toolchain, or crate dependencies without installing anything.

Commit code and config changes on a work branch in an isolated worktree, not
on `main`. When
publication is authorized, finish the scoped commits and run `oms land`.
It owns the gate, unchanged-tree push, pull request and CI receipt; it merges
and updates an eligible installation only with `--merge`, which is passed when
the maintainer asked to merge. Do not run a duplicate full gate first. See
[verification and release](docs/COMPONENTS.md#verification-and-release) for CI
scope, installation proof and direct-push requirements.

### Harness regression tests

Choose the native fixture that covers the behavior under change. The fixtures
use temporary repositories and local services or stubbed providers.

| Behavior | Suite |
|---|---|
| Plan leases, evidence and completion | [Plan CLI tests](crates/oms-command-plan/tests/cli.rs) and [completion tests](crates/oms-command-plan/tests/completion_gc.rs) |
| Apply, verification and exact landing receipts | [Landing contract tests](crates/oms-command-landing/tests/landing_contract.rs) |
| Panel lifecycle and result projections | [Panel parity tests](crates/oms-command-panel/tests/panel_parity.rs) |
| Context manifest inspection and comparison | [Native context manifest tests](crates/oms/tests/context_manifest_contract.rs) |
| Managed hook callbacks and provider wiring | [Hook adapter tests](crates/oms-command-hooks/tests/host_adapter.rs) and [native lifecycle tests](crates/oms-command-lifecycle/tests/native_lifecycle.rs) |

For example, `cargo +1.99.1 test --locked -p oms-command-hooks --test host_adapter`
runs the callback suite. Use the same package and test-target selection for the
other native fixtures.

## Style

### Rust workspace

OMS runs as a pure-Rust CLI (`Cargo.toml` + `crates/*`) with thin Bash entry
and lifecycle wrappers on the host contract:

```bash
cargo +1.99.1 test --manifest-path Cargo.toml --workspace --all-targets --locked --offline
```

The `oms-migrate` inventory check rejects new or increased recorded Python
footprint; removals and reductions are allowed. An intentional baseline increase
needs a PR explanation, the affected milestone and a removal condition. Do not
regenerate the baseline as an automatic CI fix. The lexical inventory is a
hygiene check; passing it does not establish a Python-free runtime.

### Current source conventions

- Bash 5.0+, `set -euo pipefail`, GNU/BSD-compatible on Linux, WSL2 and macOS.
  Only the prelude of `install.sh` above its Bash 5 check stays parseable by
  Bash 3.2, so macOS's system Bash can find a newer Bash or print the hint.
- Keep changes task-scoped; match the surrounding script's conventions.
- Extend the canonical test or fixture; add a new test only for an uncovered
  path, and look for coverage it supersedes. Prefer tests of behavior and
  public interfaces.
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
