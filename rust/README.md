# Rust migration workspace

This workspace prepares the [complete Rust migration](../docs/RUST-MIGRATION.md).
It currently contains `oms-migrate`, a development inventory tool. It does not
replace the installed `oms` command, change existing state, or provision Python.
Production core/CLI packages will be added with the first implemented native
feature rather than as empty placeholders.

## Toolchain and checks

The workspace pins Rust **1.85.1**, uses edition **2024**, and declares Rust
**1.85** as the minimum supported compiler. The initial tool uses only the Rust
standard library and Git. Run these from the repository root:

```bash
cargo +1.85.1 fmt --manifest-path rust/Cargo.toml --all -- --check
cargo +1.85.1 check --locked --offline --manifest-path rust/Cargo.toml --workspace --all-targets
cargo +1.85.1 test --locked --offline --manifest-path rust/Cargo.toml --workspace
cargo +1.85.1 clippy --locked --offline --manifest-path rust/Cargo.toml --workspace --all-targets -- -D warnings
cargo +1.85.1 run --locked --offline --manifest-path rust/Cargo.toml -p oms-migrate -- check --repo .
```

Alternatively, change directory to `rust` so its toolchain file is selected and
use `--repo ..`. Merely specifying `--manifest-path` from the repository root
does not select the nested rustup toolchain file; the explicit `+1.85.1` above
keeps the compiler choice clear. These are validation commands, not a statement
that a particular development environment has already executed them.
The compiler and its formatting/lint components must already be available;
Cargo's `--offline` controls dependency access, not rustup toolchain installation.

The existing [test workflow](../.github/workflows/test.yml) selects one Ubuntu
Rust lane for Rust/migration changes or changes on the inventory's candidate
surfaces. Scheduled/manual runs and an unknown comparison base also select it.
The stable gate requires this lane to pass when selected and permits a skip only
when the scope decision excludes it. This supplements existing legacy checks;
it is not yet native-runtime or cross-platform compatibility evidence.

## Legacy Python inventory

```bash
cargo +1.85.1 run --locked --offline --manifest-path rust/Cargo.toml -p oms-migrate -- inventory --repo .
cargo +1.85.1 run --locked --offline --manifest-path rust/Cargo.toml -p oms-migrate -- check --repo .
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
cargo +1.85.1 run --locked --offline --manifest-path rust/Cargo.toml -p oms-migrate -- check --repo . --require-zero
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
