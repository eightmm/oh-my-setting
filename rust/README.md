# Rust migration workspace

This workspace prepares the [complete Rust migration](../docs/RUST-MIGRATION.md).
It contains the development inventory tool `oms-migrate`, a standalone native
`oms` binary, and seventeen native libraries: `oms-context` (manifest projections
and lossless session JSON input), `oms-safe-fs` (descriptor-relative reads),
`oms-json` (domain byte encodings), `oms-output` (streaming digest selection,
reversible reducers and scoped retrieval metadata), `oms-graph-shell` (source
graph extraction), `oms-context-report` (text reports), `oms-context-session`
(injection, session and timeline projections), `oms-contract-markdown`
(task/project Markdown contracts using Python 3.12 Unicode 15 semantics),
`oms-failure` (deterministic failure taxonomy and recovery metadata),
`oms-work-inspect` (work projections with lossless timestamp handling),
`oms-context-source` (bounded read-only ledger/transcript acquisition),
`oms-graph-python` (native extraction through one private persistent helper),
`oms-output-capture` (retained-output validation, safe capture acquisition, paging
and search),
`oms-graph` (parser selection, Config/Markdown extraction, and shared-helper
Python/Shell routing), `oms-graph-resolve` (ordered graph resolution),
`oms-experiment` (scenario validation, normalization and canonical digests), and
`oms-scrub` (bounded secret scanning and retained-output redaction).

The standalone binary implements manifest inspection, comparison and current-file
verification, plus injections, session reports and timelines, through
`oms runtime context-inspect`. It preserves raw OS paths and lossless session
Read labels. Descriptor-backed capture acquisition in `oms-safe-fs` is connected
to the retained-output projection adapter. The graph facade explicitly
rejects notebooks until their native parser is integrated. The installed `oms`
command uses a staged native context payload when the trusted owner supplies
the exact-revision artifacts before Python activation. Missing current payloads
retain transitional legacy selection. No legacy domain is retired.

The graph helper is a private libexec artifact, not another PATH command. Its
malformed-input crashes are contained on Linux; macOS memory and dump-collector
guarantees remain limited. Packaging must exclude the test-only
`oms-graph-python-fixture` executable. Capture acquisition and projections are
locally integrated; their public CLI and MCP consumers still use the legacy
implementation. Experiment comparison and notebook parsing remain incomplete.

## Toolchain and checks

The workspace pins the build toolchain and native library minimum to Rust
**1.99.1**, using edition **2024**. The dependency-free preparation tool
`oms-migrate` separately retains its tested Rust **1.85** minimum; it uses only
the Rust standard library and Git. The
build toolchain pin does not establish the final binaries' glibc or macOS
deployment requirements: those must be validated against the supported hosts,
including the Ubuntu 20.04 floor. It does not prove runtime or cross-host
readiness. An exact baseline CLI and production helper were built in an Ubuntu
20.04 image and executed there with no Python or PATH tools. This proves those
two artifacts only; changed release binaries must repeat the checks. Native
macOS, WSL2 and installed lifecycle evidence remain pending. Run these from the
repository root:

```bash
cargo +1.99.1 fetch --manifest-path rust/Cargo.toml --locked
cargo +1.99.1 fmt --manifest-path rust/Cargo.toml --all -- --check
cargo +1.99.1 check --locked --offline --manifest-path rust/Cargo.toml --workspace --all-targets
cargo +1.99.1 test --locked --offline --manifest-path rust/Cargo.toml --workspace
cargo +1.99.1 clippy --locked --offline --manifest-path rust/Cargo.toml --workspace --all-targets -- -D warnings
cargo +1.99.1 run --locked --offline --manifest-path rust/Cargo.toml -p oms-migrate -- check --repo .
```

Alternatively, change directory to `rust` so its toolchain file is selected and
use `--repo ..`. Merely specifying `--manifest-path` from the repository root
does not select the nested rustup toolchain file; the explicit `+1.99.1` above
keeps the compiler choice clear. These are validation commands, not a statement
that a particular development environment has already executed them.
The compiler and its formatting/lint components must already be available;
Cargo's `--offline` controls dependency access, not rustup toolchain installation.
The explicit locked fetch seeds the dependency cache before the offline checks.
A toolchain pin alone does not prove the Ubuntu 20.04 or macOS deployment floor.

The existing [test workflow](../.github/workflows/test.yml) selects one scoped
Ubuntu/macOS Rust matrix for Rust/migration changes or changes on the inventory's candidate
surfaces. Scheduled/manual runs and an unknown comparison base also select it.
The stable gate requires this lane to pass when selected and permits a skip only
when the scope decision excludes it. This supplements existing
legacy checks and executes native filesystem tests on both host families when
selected. The native public-entry tests also require the existing Bash 5
contract. The Rust job reuses the lifecycle job's macOS Homebrew Bash setup; its
30-minute budget covers that bounded setup plus toolchain, dependency fetch and
native checks. No extra job or matrix is added. A local configuration change is
not a completed CI run or proof of the oldest supported deployment targets.

## Legacy Python inventory

```bash
cargo +1.99.1 run --locked --offline --manifest-path rust/Cargo.toml -p oms-migrate -- inventory --repo .
cargo +1.99.1 run --locked --offline --manifest-path rust/Cargo.toml -p oms-migrate -- check --repo .
```

`inventory` prints deterministic TSV to stdout, with header
`path`, `kind`, `units` separated by tabs. `check` compares the current inventory
against [migration/python-inventory.tsv](../migration/python-inventory.tsv).
It accepts removals and reductions, and rejects new rows, kind changes, or unit
increases. `--baseline PATH` selects another explicitly supplied baseline.
The tool never silently regenerates the checked baseline.

| Row kind | What is counted |
|---|---|
| `python-source` | Every tracked `.py`/`.pyw` file; units are physical LF-delimited lines, and even an empty file retains a row |
| `python-reference` | Lines containing case-insensitive literal `python` on the selected non-Python surfaces |

The reference surfaces are tracked `.sh`, `.bash`, `.zsh`, `.fish`, `.js`, `.mjs`,
`.cjs`, `.json`, `.yaml`, `.yml`, `.toml`, and extensionless files, including scoped
files inside `rust/`. Extension matching is case-insensitive; trailing-dot names
are treated as extensionless. Markdown, Rust `.rs` sources (including `build.rs`),
and the TSV inventory are outside the reference classifier by file type; there is no
blanket `rust/` exemption. Every `.py`/`.pyw` source still counts wherever it lives.
Comments and configuration text count conservatively; a marker is not proof of
an interpreter invocation.

The file set comes from `git ls-files -z`; contents come from the tracked working
tree. Stage new candidate files before checking them so Git lists them. The tool
rejects ambiguous TSV paths and unreadable, invalid-UTF-8, symlink, or nonregular
scoped files instead of silently treating them as absent. A missing Git command
or failed repository read is an error.

For a deliberate baseline change, inspect `inventory` output and the source
diff, then commit the reviewed change to the TSV. Record the temporary reason,
owning migration milestone, and removal condition for increases. Do not add a CI
step that automatically refreshes the baseline before checking it.
Retirement PRs must also lower the baseline to the remaining measured footprint;
otherwise the old upper bound could permit a later reintroduction below that cap.

## Strict zero check and its limits

```bash
cargo +1.99.1 run --locked --offline --manifest-path rust/Cargo.toml -p oms-migrate -- check --repo . --require-zero
```

The additional flag requires no inventory rows. It is expected to fail while
the legacy implementation remains. It is not the default preparation gate.
Data-only labels can match the conservative reference scan; any later classifier
refinement must identify and justify those cases without hiding executable code.

This lexical tool cannot detect every dynamically assembled interpreter command,
transitive build dependency, or Python invocation introduced in Rust code.
It cannot establish that production is Python-free. Final acceptance requires
the isolated runtime, source-build, test, evaluation, packaging, and lifecycle
checks defined in [the migration specification](../docs/RUST-MIGRATION.md).
Python project input support remains a requirement; OMS must parse it in Rust
without executing an interpreter.
