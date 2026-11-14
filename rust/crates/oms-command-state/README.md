# Native state command implementation — blocked draft

This crate is incomplete and must not be wired into the native dispatcher.
The implementation stopped at the brief's required dependency API stop
condition. Compilation is expected to fail until the owning agent resolves
the native child-execution interface. No command parity or acceptance is claimed.

## Blocker

`src/capsule.rs` needs merged child execution without a ledger side effect.
`oms-command-runs` implements `process::merged`, but `src/lib.rs` declares
`mod process` privately. Rust reports E0603. The public
`oms_command_runs::execute` enters ledger execution and does not provide the
independent execution operation needed by `run-capsule --no-ledger`.

Changing that dependency is outside this worker's write scope. The owner can
choose a public native execution adapter or explicitly authorize a different
reusable executor. This draft does not duplicate process supervision.

## Draft inventory

- `src/lib.rs`: `owns`, `run`, `run_with_source`, shared parsing, physical
  repository resolution, JSON responses, timestamps and random identities.
  `owns` reserves all fourteen assigned command names, including commands
  whose execution is still explicitly incomplete.
- `src/storage.rs`: bounded no-follow reads, canonical locks, atomic JSONL
  replacement and protected file publication through `oms-safe-fs`.
- `src/lease.rs`: draft `add`, `list`, `check`, `release`, TTL parsing,
  advisory overlap projection and staged-path checking.
- `src/checkpoints.rs`: draft `run-checkpoints` read-only audit with directory
  and file limits, PROJECT.md defaults and strict/JSON outputs.
- `src/failures.rs`: draft failure replay, fingerprints, hook expiry,
  record/check/resolve/list/show and a partial stall projection.
- `src/capsule.rs`: blocked adapter to native merged child execution.

`state`, `state-verify`, `gc`, `inbox`, `dashboard`, `deps`, `run`,
`run-capsule`, `run-claims`, `tsp-queue`, `recipe`, remaining shared helpers,
and full stall/advice/hook behavior still require implementation. The draft
lease, checkpoint and failure surfaces also need compatibility review.

## Storage and interruption

The draft writes only requested repository state at runtime. Each lease or
failure mutation holds the canonical `oms-lock` for its ledger. Two cooperating
calls serialize their read/modify/publication work. JSONL is currently replaced
atomically; compatibility with existing readers retaining an inode is not yet
established and should be addressed before admission.

The shared byte writer publishes whole old or new bytes. A kill before rename
may leave a private temporary or created parent directories; after rename,
new bytes may be visible without confirmed directory durability. Errors tell
the caller to reconcile before retrying. Release of several leases writes one
complete release row at a time: a kill can leave only a prefix released, and
another `release --mine` computes the remaining targets. Checkpoints only reads
directory metadata and never writes checkpoint files. No command execution,
ledger companion, recipe, queue or garbage-collection writes are wired yet.

## Verification scope

The dependency-only scaffold compiled offline on Rust 1.99.1. The implementation
check is blocked by the private `oms-command-runs::process` API. Formatting is
checked separately. Tests, including a reverted-implementation negative
control, were not run because the brief explicitly forbids tests in this phase.
Compilation and formatting alone would not establish behavioral compatibility,
cross-platform behavior, concurrency guarantees or NFS crash recovery.
