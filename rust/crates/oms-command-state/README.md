# Native state commands — reviewed integration draft

This crate provides native routes for all fourteen state/run/lease command
names. It remains an integration candidate: source checks do not establish
behavioral admission, exact output parity, process recovery or NFS durability.
The original draft's private `process` module blocker is resolved.

Recipe host execution remains refused before any spawn or recipe state write.
`oms-command-runs::process::{ProcessPolicy, Output}` does not expose the
post-spawn PID receipt needed to refuse replay of a still-live prior child.
Recipe validation, planning, resources, worksets and request creation do not
require that hook and are implemented separately.

## Public interfaces and native reuse

Existing `owns`, `run`, `run_with_source` signatures remain unchanged.
`run_with_services` accepts Runs services; `run_with_runtime` accepts a
canonical schema-2 parent runtime envelope for state views. `usage` reads
help without filesystem access. `run_queue_worker` is the dedicated internal
entry point for `src/bin/oms-state-queue-worker.rs`, not a recipe launcher.

Public helper modules are `deps`, `helpers`, `inventory`, `job_metrics` and
`lease`. `inventory::{inventory, render}` returns the ambient-filtered tuple
inventory and JSONL. `job_metrics::{gather, render}` reads JSONL/DVCLive TSV,
retains ten recent points per series and reports observations, not liveness.

Native reuse includes Runs merged execution, `execute`, `execute_with_policy`,
`ProcessPolicy`, `Output`, `Services::rlimit_helper`, captured metrics,
completed-status ledger execution and exact `statistics::{mean, stdev}`.
Task, plan, retirement, lifecycle, approvals, room, journal and memory remain
owned by their existing native crates. Journal storage/cold replay are reused
for storage health and sealing advice. The crate uses `oms-toml`, `oms-lock`,
`oms-safe-fs`, `oms-scrub` and the lossless shared Plan/`oms-json` value API.

Edition 2024, Rust 1.99.1 and `forbid(unsafe_code)` are preserved. `oms-toml`
is the only dependency added by the combined candidate. External versions
and the root lockfile are unchanged; no arbitrary-precision serde feature
is enabled. The standalone queue worker must be installed beside the parent
executable, or selected explicitly by `OMS_TSP_WORKER`.

## Carbon findings and source changes

| Finding | Source response |
| --- | --- |
| `f-4c7f2dc7076d` | `spine.rs` returns a clean absent inventory without opening a missing `.oms`; JSON includes `adopted: false`. Malformed plan JSON/tasks shape becomes findings/exit 1. Landing event enums and `DRIFT` labels are checked. `state-verify` returns the unadopted report before collectors. |
| `f-4ec7b93b5c16` | `storage.rs` anchors explicit absolute overrides without removing no-follow descriptor traversal. Separate capsule input readers permit hardlinks, and hashing streams through a 64 KiB buffer. Outputs cannot fail solely because they are external or hardlinked after execution. |
| `f-c85c9fff1bf0` | `state_projection.rs` derives native source health instead of unconditional runtime failure; the parent may supply the authoritative envelope with `run_with_runtime`. An unhealthy native task/plan/project/evidence read remains unhealthy. Health never grants acceptance authority. |
| `f-966cb2794a7a` | State collectors now read artifacts, CI, experiment replay, guards, souls, threads and actual delegation markers. Each view has a human renderer; watch flushes snapshots. Command-specific parsers reject sibling flags. Safe repairs reclaim stale plan claims, refresh stale CI and resolve exact same-provider successful patch bytes. Verification includes levels, ignore marker, dangling pointers, task contradictions, journal publication and native retirement validation. |
| `f-00ed1c4b3ba4` | Capsule manifests use `OMS_MANIFEST_DIR` or `.oms/manifests`, and unnamed manifests produce an advisory. |
| `f-64d6b34e5e36` | Capsule listing has its schema-1 envelope. Whence scans full capsule records and returns 1 on no match. Reproduction includes cwd, dirty patch, environment locks, conda/modules/data and omits nonexistent commits. Configs flatten bounded JSON with status, missing configs warn, console streams into log/stderr, environment probes use the native executor, and capture notifies Journal services. |
| `f-04655dda2864` | Claims print numeric values, preserve malformed-row exit 1, confirm additions on stderr, render recorded comparisons and include figure capsule inputs. Figure paths accept normalized in-repo absolute/`./` spellings; the input limit is 32 and file hashing is capped at 512 MiB. |
| `f-0e66e4aa538e` | Link/close map no-current to exit 2; close observes Journal. Diff renders field/config-key/data/metric comparisons. Timeline includes the experiment ledger and reconciliation timestamps, accepts fractional/offset ISO dates and returns the schema/omitted/events envelope. |
| `f-bd6aaf2e47c6` | Missing dependencies return 1. Human cards include environment, location, summary, file line numbers and exact version-disagreement notes. |
| `f-935d45a6b0d0` | Discovery selects Python dependency/lock files explicitly; arbitrary TOML/lock and Cargo files are excluded. Poetry and dependency-group declarations are parsed through `oms-toml`. |
| `f-6c1d284be4ee` | Queue metadata is stored at the numeric job name with the panel header, socket and command attribution plus safe native argv data. Guard refusals return 3. Terminal status parsing handles success/failure states; wait prints the code. Logs tail the `tsp -o` file without an 8 MiB capture cap. Explicit fallback uses the native worker and cancellation receipts. Legacy shell metadata is parsed as data, never sourced. |
| `f-eb41c1d4f09b` | Recipe output specs are validated, expanded, duplicate checked and confined against traversal/private state. Cwd and dependency templates receive the same refusal. |
| `f-ff4757aa8a68` | Static validation checks environment names/references without reading live variables. Planning resolves allowed references, pins executable/dependency/environment identities, preserves cooperative child controls, computes canonical plan signatures and includes source/execution metadata. Workspace overlays and existing-command replacement are refused. |
| `f-77851f9bef4b` | `recipe_catalog.rs` implements immutable pack inspection/import and exact id/version marker listing, workset publication/live drift views and trigger add/enable/disable/tick/event/requests/plan/cancel. Requests never confer execution authority. Companion planning is independent of refused companion spawning. |
| `f-814cf30307b2` | **Partial:** GC now recovers dead exact leases, collects supported transient families, reuses lifecycle reconciliation/compaction, preserves malformed hook diagnostics, compacts aged retired failure rows individually, handles usage/guards/closed threads and checks durable references for opted-in loose orphan patches. `gc_artifacts.rs` prunes ordinary index history while preserving completion pins, unresolved failures, resolution lineages and recent panel decisions. Inventory and job-metrics helpers are added. The protected proof-dependent families below remain retained. |

The associated shell/Python contracts and existing smoke cases were inspected
as source. The review's repeated prior diff was already in context; the actual
review findings and complete 15-row findings ledger were read. No behavioral
suite or negative-control test was run in this implementation-only phase.

## Remaining owner integration and admission

- `f-814cf30307b2` remains open for managed capture generations, completion
  capture/transaction-pin retention and dispatch-call artifact settlement.
  Their writer seals, quarantine ownership,
  namespace/generation identity, reference and settlement proofs must be
  preserved through a native adapter. Neither terminal hints nor equal bytes
  authorize unlinking these objects. They are retained by this candidate.
- Recipe execution/resume/request execution and companion spawning need the
  Runs post-spawn PID/start receipt interface. The current refusal is
  intentional and happens before recipe mutation.
- The parent owns installation of the queue worker and injection of canonical
  runtime acceptance data. Native health probes establish readability of
  captured sources, not a replacement for the complete parent runtime engine.
- Frozen human-text/JSON/digest, Unicode width, timezone, malformed-input,
  boundary, concurrency, interrupted-publication and Linux/WSL2/macOS corpus
  comparisons still require behavioral admission. The terminal renderers and
  metrics renderer have not been byte-compared with those corpora.

## Concurrent calls and interruption inventory

| Written state | Second call / kill between publications |
| --- | --- |
| Lease/failure streams and their compaction | The canonical stream lock serializes cooperating writers. Atomic replacement retains whole old/new bytes. Releasing several leases can leave a released prefix; the next release recomputes remaining targets. GC retains undated or unresolved failure rows. |
| Run spine and CURRENT | New/close take CURRENT then spine lock. A mint can exist before CURRENT points to it; a close can exist before pointer clearing. Repeated close may append another close, and another run's pointer is preserved. |
| Capsule diff, environment locks, log, capsule, index, companion ledger and spine link | Random IDs and a per-capsule lock isolate captures. Log bytes stream while execution runs. A kill can leave a partial bundle, an unindexed capsule or missing companions. No publication retry reruns the command. A forced kill can leave a child without a native start receipt; it never authorizes replay. |
| Claims/figure streams | Their stream locks span reads, pinning and publication. IDs project last-row wins. Files changed later are stale; existing rows are not silently repinned. Capsule cleanup holds both provenance-stream locks before deciding retention. |
| CI records and artifact resolution rows | Independent canonical stream locks serialize each write. A safe-repair sequence can leave a completed prefix, and rerunning recomputes pending work. Artifact resolution requires exact stored patch identity and the same provider. |
| Native tsp metadata / last pointer | A queue job may exist before metadata or pointer publication. Errors name this uncertainty; resubmission requires reconciliation. Repeated wait may add another completed ledger row, never another execution. |
| Fallback request / metadata / last / ready / log / status / cancellation receipts | The worker waits for the exact ready receipt before executing. A kill before ready leaves no launched user command; after ready the detached worker owns supervision. Cancellation is a typed request to that supervisor. A worker lost without status is uncertain, not eligible for replay. Request/ready files may remain for reconciliation. |
| Pack resources then pack marker then history | The id/version marker lock serializes imports. A kill before marker publication can leave unreferenced resource files; list refuses incomplete imports. A repeated same-digest import validates the old selection before extending it. Executable permissions are set through retained file descriptors. |
| Workset record then history | The per-id lock protects replacement rules. A kill can leave the new record without its advisory history event. Show rechecks the pinned specification and current repositories. |
| Trigger/request/event records and history | The trigger coordinator serializes definitions, ticks and event selection. Requests publish before schedule indexes, so a kill retries the same deterministic request ID. Events persist selected triggers before queuing requests; the same event ID/data resumes idempotently and different data refuses. A history event may be missing after a durable record. |
| GC candidates | Canonical writer locks and no-follow directory descriptors fence cooperating changes. Candidate generation changes refuse unlink. Recursive cleanup can leave an already-removed prefix after interruption; a retry examines remaining state. Private managed captures and unsettled evidence are retained. |
| Artifact-index pruning | The canonical index lock spans the snapshot, completion-pin scan and atomic replacement; another cooperating index writer waits. A kill leaves the old or new complete index. Missing, corrupt, oversized or ambiguous completion pins retain all rows. Opted-in loose orphan deletion rechecks receipt readability and keeps every durable reference. |
| Read-only state/dependency/recipe/inventory/metrics paths | They publish no state. Cross-family snapshots are observations at different instants, not a global transaction. |

Atomic replacement changes the inode; readers retaining an old descriptor
keep its old snapshot. Rename/fsync errors can leave private temporaries or
published bytes whose durability is uncertain. Reconcile before retrying an
uncertain effect. These are static implementation invariants, not measured
XFS/NFS or crash-test claims.

## Verification scope

The permitted source checks are:

```
cargo +1.99.1 check --manifest-path rust/crates/oms-command-state/Cargo.toml --all-targets --offline
cargo +1.99.1 fmt --manifest-path rust/crates/oms-command-state/Cargo.toml -- --check
```

Tests, boundary executions and the reverted-implementation negative control
are skipped because this phase explicitly permits only the source checks.
No existing test is changed. No commit, push, dispatcher wiring or root
workspace/dependency-version change is part of this worker result.
