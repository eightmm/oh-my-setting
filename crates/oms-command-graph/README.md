# oms-command-graph

Native implementation for the `graph` command. This crate can be compiled
independently of the root dispatcher. Its source contracts do not by themselves
establish retrieval quality or complete parity with the retired Python engine.
See the [retrieval comparison and work packets](../../docs/GRAPH-RETRIEVAL-COMPARISON.md)
for the current language coverage, source-backed gaps and evaluation plan.

The buffered entrypoints are `owns(args)`, `run(args, cwd, interrupted)`, and
`run_with_helper(args, cwd, interrupted, python_helper)`. Arguments start at
`project` or `exec`, after the dispatcher removes `graph`. Calls preserve the
process working directory and environment. `cwd` and `--repo` resolve to a
physical repository; later `--repo` options win. The cancellation flag is
borrowed for the duration of a call.

## Integration

The parent supplies the absolute packaged `oms-graph-python-helper` path to
`run_with_helper`. The fallback is the explicitly configured
`OMS_GRAPH_PYTHON_HELPER` environment value. Both require an absolute regular
executable owned by the current user or root, with no group/other write bits.
The session owns and reuses the helper, and receives cancellation while a
parse is pending. Parser failures propagate rather than becoming empty graphs.

`run_with_services` additionally accepts the host's `oms-runtime::Services`
and a thread-safe `oms-command-plan::Services`. Existing buffered entrypoints
retain their signatures and refuse unavailable producer projections. Runtime-only
hosts can use `run_with_runtime_services`; direct bundle consumers can call
`project::context::compile_bundle_with_services` with explicit execution and
state roots. The host still owns lifecycle, room, approval and evidence services.

`OMS_INSTALL_ROOT` identifies the trusted packaged installation. A directory
is accepted as a root when it contains `config/graphs`, `scripts/oms` or the
native checkout anchor `crates/oms/Cargo.toml` (`execution::is_root`). An
executable installed directly beneath that root can also discover it from its
own location. This installation is
used for bundled specs and the canonical plan-run, acceptance and exact-commit
front doors. The graph command does not implement their authority transitions.

`OMS_PROJECT_GRAPH_STATE` selects an absolute project-state directory;
otherwise state lives in `.oms/project-graph`. `OMS_GRAPH_AUTOBUILD=0` and the
supported `--no-refresh` flags disable reader refreshes. The native child-action
policy is enforced in `src/lib.rs`; `execution_commit.rs` also checks its
exact-commit boundary.

## Implementation map

| Modules | Responsibility and reused APIs |
| --- | --- |
| `cli.rs`, `cli.json` | Parsed CLI grammar, defaults, required options, help and usage captured from the Python parser |
| `project_build.rs`, `project_sqlite.rs` | Safe source discovery, SHA256 cache, freshness, atomic JSON and SQLite publication |
| `project_index.rs`, `project_analytics.rs`, `project_unicode.rs` | Find, API, neighbors, trace, search, ranked symbol blocks, symbol zones, unreferenced candidates, assurance, recall metrics and full Unicode case folding |
| `project_benchmark.rs` | Fix-parent exports, history recall@8, content-free snapshots and comparison |
| `project_context.rs`, `project_history.rs`, `project_affected.rs`, `project_cases.rs` | Context packs, runtime bundles and signature maps, Git coupling, hunk/case selection, runner-membership cuts, fail-open affected plans and CODEOWNERS impact briefs |
| `project_visual.rs`, `graph.html` | Schema-3 project visual models and the existing interactive HTML template |
| `notebook.rs` | JSON code-cell extraction and physical line remapping through the isolated Python parser |
| `execution.rs`, `execution_validate.rs`, `execution_route.rs` | Lossless specs, predicates, validation, bindings, proofs, joins, nested routing and budgets |
| `execution_events.rs`, `execution_scheduler.rs`, `execution_workspace.rs` | Locked event append, event replay, run storage, task/scope scheduling and workspace-bound read caches |
| `execution_runner.rs`, `execution_commit.rs`, `execution_render.rs` | Plan projections, tool execution, recovery, gate decisions, exact-commit adapter, text/Mermaid rendering and interactive execution HTML |
| `util.rs` | Physical paths, safe storage adapters, bounded process-group supervision, scrubbing and glob matching |

`oms-graph::parse`, `oms-graph-python::Session`, the shell callback selected by
`oms-graph`, and `oms-graph-resolve::resolve` supply parsing and resolution.
`oms-command-plan::{Context, Plan, Request}` supply plan reads and its `Store`
supplies state writes. The operations value type uses `oms-context` and
`oms-json` for Python JSON numbers; decimal ordering reuses
`oms-command-task::packet::decimal_cmp`. Source acquisition uses
`oms-safe-fs::Root`, state exclusion uses `oms-lock`, and displayed source and
artifact tails use `oms-scrub`. All path dependencies requested by the worker
brief are declared. `oms-runtime::context::plan` owns bundle compilation,
phase rules and evidence-envelope digests; the native journal store
owns journal event validation, deduplication and materialized views. The adapter
holds the journal's canonical state lock during admission and materialization.
No workspace sibling source or dependency feature was changed.

`cli.json` was captured by walking `build_parser()` and serializing each
parser's options, children, help and usage. `graph.html` extends the legacy
interactive template (staying synchronized with
`oms-mcp-server/src/project-graph.html`) with precomputed search haystacks,
zoom-tier label thresholds, and `/` + `Escape` search shortcuts. Without D3,
a deterministic SVG grid uses the same search, filters, node inspection and
zoom/reset controls; the CDN script loads asynchronously.
The captured grammar is migration history; current CLI changes update the native
schema, assets and callers together. The native schema exposes `project
benchmark --count N --revision REF --output FILE --snapshot FILE` and `project
benchmark --compare LEFT RIGHT`. At least one output is required for a run;
comparison runs no extraction and exits 1 for incomparable cases or regressions.
`project_benchmark.rs` and `project_analytics.rs` own the current result,
snapshot and comparison contracts. The native benchmark uses uncompressed JSON
for its private parent cache. From an installed native entrypoint, run:

```bash
oms graph project benchmark --count 10 --revision HEAD --output /tmp/oms-graph-recall.json --snapshot /tmp/oms-graph-recall-snapshot.json
oms graph project benchmark --compare /tmp/oms-graph-before.json /tmp/oms-graph-after.json
```

The comparison inputs must have been recorded separately for the same cases.
History recall measures entry files selected from each fix's parent tree;
it does not measure caller accuracy, delivered signature coverage or latency.

Affected selection narrows modified indexed Python/shell cases only when both
revisions and every diff hunk prove containment. Unindexed modified runnable
suites remain selectable; the two canonical shell suites also support individual
case selection. Shared helpers, malformed ranges and ambiguous shell structure
fall back to the suite file. Runner membership edges are cut only when every
changed code path belongs to an invoked test. CODEOWNERS uses the first safe
file in host precedence order, with the last matching rule owning each path,
including explicit rules with no owners. There is no legacy `--oversized` CLI
option: oversized suite selection is automatic.

Build discovery retains source digests, lengths and optional `mtime_sec`,
`mtime_nsec` and `ino` metadata in `manifest.json` file entries. Freshness reuses
a digest only when all four stat fields match and the mtime second precedes
`build_started_sec`. Legacy or malformed metadata and racy timestamps trigger
full hashing; `build --force` always hashes and reparses. A stat-cache hit is
not a fresh byte comparison: the canonical engine fixture explicitly preserves
that shortcut when same-size bytes and the old mtime are restored. Non-code files are
classified from at most 8192 bytes without hashing. Unchanged binary/unparsed
classifications are cached in skipped rows. Source size caps still apply.
Cache misses reread and verify one source at a time, bounding discovery memory
by file metadata plus one capped source. Python sessions grow from the default
source bound up to the helper's ceiling.

`symbol`, `api`, `neighbors` and bounded `trace` use prepared SQLite lookups
when the read-only immutable projection has matching revision/schema metadata.
Missing, corrupt, old-schema, stale or linked projections fall back to JSON.
Immutable projection reads create no SQLite journals or sidecars; an automatic
refresh can write the separate extraction cache. Shared query algorithms preserve
ordering, duplicate edges, Unicode case folding and result limits. Symbol text
scans still verify source digests. Other queries reuse the graph and manifest
loaded during freshness rather than parsing them again. The compact graph
shares one JSON arena through integer CSR adjacency views, with path/test
indexes and inverted lexical postings. Context BM25 builds term frequencies
once per query and scores matching postings, preserving its existing field
weights. That preprocessing is not currently persisted between context queries.
Canonical JSON writes traverse the existing value directly with sorted object
keys and the shared Python-compatible float formatter.
Normalization and validation traverse nested graph definitions iteratively, so
validation reports nested node errors rather than refusing at depth two.
Fixture gate values, repeat counters and task identities follow Python coercions.
CLI execution step budgets retain arbitrary-size decimal integers through
`oms-json`; bounded collection operations use checked or saturated machine sizes.

## Concurrency and interruption

| Written artifact | Concurrent call or interruption behavior |
| --- | --- |
| `.oms/.gitignore` | The atomic preserve-existing operation keeps an existing marker unchanged. A failed creation can leave private parent directories. |
| `cache.sqlite3` and its WAL/SHM sidecars | Regenerable per-file extractions are keyed by path, source digest, parser version and schema. One transaction under the project build lock inserts and prunes entries; it commits and checkpoints after graph/manifest publication. Losing an uncommitted cache transaction requires re-extraction, not new authority. Legacy file-cache entries are not imported: misses re-extract source, and the legacy `cache/` directory is removed after the transaction commits. |
| `graph.json`, `index.sqlite3`, `manifest.json` | Each publication is atomic, under the project build lock. SQLite is constructed in memory. Manifest publication follows graph and SQLite publication; a kill between them leaves a revision mismatch that freshness checking rejects. Readers verify the projection revision against the manifest and otherwise use JSON; immutable SQLite readers retain an opened inode across atomic replacement. |
| `orientation.json` | Derived after manifest publication. A missing orientation can be regenerated; it grants no authority. |
| `context/<digest>.json` | Content-addressed atomic writes can be repeated. A kill leaves an older complete file or no published file. |
| `runs/<run>/graph.json`, `events.jsonl` | The event lock protects new-run exclusion, sequence allocation and event append. A kill between spec publication and its first event leaves a frozen run that can be inspected and resumed. |
| `runs/<run>/projection.json` | Atomic, derived from the authoritative event stream, and replayable after a kill. |
| `runs/<run>/artifacts/*.txt` | Atomic bounded output tails. A kill before the outcome event may leave an orphan artifact; recovery reads plan state and blocks uncertain write tools. |
| `.oms/graph/cache/<digest>.json` | Atomic regenerable read-tool results. The key covers the complete safe dirty-workspace fingerprint and upstream outcomes. Unreadable, oversized or linked dirty paths disable cache reads and writes. |
| `.oms/runtime/context/<digest>.txt`, `<digest>.json` | The runtime owns bounded atomic publication. The manifest follows the bundle; interruption can leave an unreferenced complete bundle. Concurrent identical requests may safely repeat writes; timestamped requests can have different digests. |
| `.oms/work-journal/` | The native journal store owns events and projections under its canonical state lock. Mirroring is best effort and skips a busy lock. A kill after journal admission can leave views requiring materialization; graph events remain authoritative. |
| Benchmark cache and output/snapshot paths | Private cache entries and explicitly selected outputs are atomic. Each call uses a distinct temporary export tree. Concurrent calls replace only complete output files; the output/snapshot pair can mix results from different calls sharing those paths. A kill between them leaves one complete result. A shared extraction cache revision mismatch refuses the benchmark instead of publishing a mixed parent snapshot. A killed process may leave its temporary tree; normal returns remove it. |
| HTML output path | Atomic replacement of the explicitly selected output. The child-action policy rejects this write for harness children. |
| Exact commit | The existing goal-drive transaction owns index/ref writes and recovery; this adapter checks the bound task and landing receipt before invoking it. |

Native runner calls hold a separate run lock while coordinating concurrent
waves; only the coordinator appends events, in work-completion order. Gate
decisions serialize their event append, and duplicate idempotency keys refuse
a conflicting concurrent decision. Process supervision observes an unreaped
child before stopping its process group; it does not signal an already reaped
process identity. Cancellation stops owned subprocess groups and the parser
session.

## External boundaries

- `oms-graph-python/src/model.rs::ParseResult` includes `syntax_valid` alongside
  `nodes`, `edges` and `refs`. The helper frame protocol is version 2; the Ruff
  producer sets validity after its parse checks, and `notebook.rs` consumes it.
  An absent field defaults to false. Syntax validity does not establish complete
  reference resolution or notebook retrieval coverage. Future protocol changes
  still require coordinated helper/session/model fixtures.
- The helper's `protocol.rs::MAX_SOURCE_BYTES` is 64 MiB and
  `MAX_RESPONSE_BYTES` is 128 MiB. Changing those ceilings requires coordinated
  helper protocol, request-length and memory-containment changes outside this
  crate. The discovery layer no longer imposes its own 64 MiB ceiling.
- The host must supply canonical runtime and plan services through
  `run_with_services`. Default entrypoints provide a real Git snapshot for
  source-only bundles, but refuse task/plan/failure projections requiring
  unavailable services. Root dispatcher and packaging wiring are outside scope.
- Native capability deadlines propagate cancellation to graph builds, parser
  sessions and owned subprocesses and reject results after the node deadline.
  In-process cancellation is cooperative: strict wall-time preemption of a
  blocking native operation requires a host process-isolation/supervision
  boundary. No thread is abandoned while it can still write state.

These are implementation boundaries. Behavioral and platform claims require
test evidence tied to the evaluated commit; compilation and source inspection
alone do not establish them.

## Verification phase

The engine optimization checks live in `tests/project_engine.rs`, with frozen
lexical/BM25 reference cases in `tests/project-engine-reference.json`, and the
offline renderer check runs with `node tests/offline_graph.cjs`. The reference
records the existing native duplicate-query document-frequency behavior, which
differs from the Python query's duplicate-token counting. Existing tests remain;
the new cases cover previously untested stat caching, SQL-only querying, CSR
payload sharing and offline SVG interaction. Native macOS/WSL2 execution and
the complete repository gate remain parent checks.
Legacy inspection covered context, affected selection, suite cases, benchmark,
CODEOWNERS, validation, routing, parser, freshness, event, journal, rendering and
cache contracts. It was source inspection, not behavioral test-pass evidence.

Current canonical suites include this crate's `tests/project_engine.rs` and
`tests/host_adapter.rs`, `oms-graph/tests/parser_contract.rs`,
`oms-graph-resolve/tests/resolve_contract.rs`, and the Python/shell parser crates'
`tests/graph_contract.rs`. Consumer coverage lives in `oms-command-peer` and
`oms-mcp-server`. Run affected native checks through the repository's pinned
toolchain and CI; the retired `tests/test_oms_graph_*.py` and `graph-smoke.sh`
paths are historical references, not runnable suites in this tree.
