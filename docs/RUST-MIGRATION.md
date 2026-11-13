# Complete Rust migration

Decision date: 2026-11-12; priority confirmed 2026-11-13. Status: preparation plus bounded legacy decision-proof recovery; native production cutover has not started.

## Decision and completion boundary

Keep `eightmm/oh-my-setting` as the repository and migrate OMS-owned executable
logic to Rust through small, independently reviewable changes. The final product
has **no OMS-owned Python requirement** for installation, runtime, maintenance,
source builds, tests, evaluation, or release. This supersedes the earlier option
of retaining Python for AST analysis or evaluators indefinitely.

Python implementations may serve as temporary compatibility references during
migration. They are not permanent backends, optional production fallbacks, or a
second maintained implementation. Wrapping CPython, embedding an interpreter,
or executing the old code through a Rust-hosted interpreter does not complete
the migration. Substantive Bash orchestration also moves into Rust. A minimal
platform bootstrap may download, verify, and execute the native binary; it
must not contain OMS state logic or provision Python.

Rust is the default for all OMS code, including CLI and panel implementation.
The important shared core moves completely to Rust first: state, plans/tasks,
evidence, authority/approvals, locks, execution/admission, context and graph.
CLI, panel, MCP and provider adapters use those same core APIs and canonical
records. TypeScript may serve presentation when useful. This priority changes
work order without narrowing the complete migration boundary.

This boundary concerns OMS itself. A user's Python project, an explicitly
requested project test command, or an independently installed provider may use
Python. OMS must still understand Python source as data using a Rust parser.
Fixtures containing Python source text are permissible inert inputs, not an
excuse to retain an interpreter-dependent test harness.

The current [host and contribution contracts](../AGENTS.md) remain in force
until their replacement is explicitly validated: Linux with the Ubuntu 20.04
floor, WSL2, macOS, and behavior across Codex, Claude Code, and Antigravity.
Native Windows remains unsupported. Existing Bash 5 requirements apply to the
legacy implementation during transition; they are not the target runtime design.

## What preparation establishes

- A small [Rust workspace](../rust/README.md) with a Rust development tool.
- A checked [legacy Python inventory](../migration/python-inventory.tsv),
  preventing unreviewed growth in the measured footprint.
- Frozen [context projection vectors](../migration/contracts/README.md) for the
  first native feature; they are reference data, not completed Rust parity tests.
- The migration order, compatibility obligations, retirement criteria, and final
  acceptance requirements in this document.
- Focused Rust validation alongside the existing repository checks.

Preparation does not switch `oms`, MCP, hooks, installation, or state writers to
Rust. The initial workspace contains a migration tool rather than empty
production crates. Create the shared core and native command entry point when
the first real feature is implemented.

The inspected production baseline is commit
`74f87f654a314961420be2aa8ab6dfc1999151e1`. Rebase implementation slices onto the
then-current main and record the actual comparison revision. In particular,
[PR #216](https://github.com/eightmm/oh-my-setting/pull/216) and
[PR #220](https://github.com/eightmm/oh-my-setting/pull/220) change output
processing contracts. They are not dependencies of this preparation change;
their accepted behavior must be incorporated before that domain's parity baseline
is frozen. Do not recreate an older digest implementation in Rust and call the
lost behavior a performance improvement.

## Target architecture

| Boundary | Target responsibility | Dependency rule |
|---|---|---|
| Shared core | Domain types, validation, projections, serialization rules | No CLI spawning for internal transformations |
| Native `oms` entry point | Arguments, output rendering, exit status, command routing | Calls the shared core directly; one public command spelling |
| MCP and provider adapters | Transport, provider-specific framing, capability projection | Reuse the core; no separate interpretation of domain state |
| Storage and supervision | Files, locks, journals, admission, children, recovery | Explicit ownership and platform behavior; one active writer per domain |
| Development and distribution | Inventory, tests, evaluation, packaging, lifecycle | Rust-owned implementation; no interpreter bootstrap |

These are logical boundaries, not a requirement to create five crates now.
Start with the smallest useful package structure and extract packages when a real
dependency or reuse boundary requires it. The single user-facing PATH entry
remains `oms`; the migration utility is a development tool, not a second public
runtime command family.

## Contracts that a port must preserve

### Commands and observations

Record each command's arguments, stdout/stderr framing, exit status, ordering,
limits, environment inputs, and state effects. Compare complete outputs, not
only their most convenient fields. Where timestamps or identifiers vary, name
the specific volatile fields and compare their invariants; never normalize away
meaningful errors, omissions, stale state, or authority information.

The current [dispatcher](../scripts/oms) performs Python runtime checks before
most Bash command entry points. A native feature is complete only when its
public execution path also bypasses that Python startup. Appending a Rust helper
behind the existing Python preflight is an interim bridge, not the target path.
The [MCP server](../scripts/oms-mcp-server.py) is persistent but launches tool
subprocesses; measure those boundaries separately from server startup.

### State, bytes, and locks

- Preserve each digest's original byte contract, including encoding, escaping,
  separators, key ordering, numeric bounds, and rejection of non-finite values.
  Do not substitute one generic canonical-JSON serializer for every domain.
  [Agent event bytes](../scripts/lib/agent-events.py) use compact UTF-8 JSON,
  while its projection cache uses ASCII escaping and the
  [supervisor encoding](../scripts/lib/dispatch_supervisor.py) retains spaces.
- Preserve physical repository identity, descriptor-relative reads, link and
  ownership checks, atomic updates, size limits, and change-during-read failures.
  A read failure must remain unknown/error rather than an empty success.
- Use one lock authority for a state domain. The
  [runtime lock bridge](../scripts/lib/oms_runtime/common.py) deliberately invokes
  [the canonical lock protocol](../scripts/lib/file-lock.sh); its comments record
  stale-holder and release bugs from a previous duplicate implementation.
  A generic Rust mutex or `flock` call is not an equivalent replacement.
- Before a writer cutover, prove mutual exclusion, owner generation, process
  identity, stale reclaim, crash recovery, and atomic publication under the
  supported platforms. Either preserve the existing protocol while both readers
  coexist, or perform an explicit drained cutover to a versioned replacement.
- Never dual-write operational state for comparison. Do not automatically retry
  through Python after a Rust command may have performed a side effect.
  Reconcile uncertain results from durable state before any further execution.

### Processes and providers

Port launch, admission, cancellation, timeout, descendant cleanup, signal-derived
exit codes, and reaping as one ownership contract. The
[Linux supervisor](../scripts/lib/dispatch_supervisor.py) establishes a subreaper
before spawning and reports completion only after adopted children are reaped.
Its platform-specific guarantee must remain explicit; a portable process API
alone does not supply it. Dropping a handle is not a cancellation protocol.

Local process termination and remote job cancellation remain separate events.
Preserve Codex, Claude Code, and Antigravity adapter behavior and capability
limits. Tests must not claim live-provider or remote-job coverage from fixtures.
The [input queue](INPUT-QUEUE.md), approval, admission, evidence, and publication
boundaries retain their existing authority semantics throughout migration.

## Domain retirement ledger

Each implementation PR expands its row into a bounded work item listing concrete
old files, new entry points, tests, temporary bridges, and deletion criteria.
This table is the initial dependency map, not a claim that every source file has
already received a semantic audit.

| Domain and current anchors | Required Rust result | Retirement evidence |
|---|---|---|
| CLI: [dispatcher](../scripts/oms), [runtime wrapper](../scripts/runtime.sh), [core](../scripts/lib/oms_core.py) | Native routing and shared argument/output contracts | Public migrated commands run without interpreter lookup; wrappers and Python preflight removed after complete coverage |
| Context/output: [inspector](../scripts/lib/oms_runtime/context_inspect.py), [digest](../scripts/lib/output_digest.py) | Same bounds, errors, omission/retrieval semantics, and byte rules | Shared fixtures match; optimized accepted Python baseline compared; obsolete Python implementations deleted |
| Work/evidence: [projection](../scripts/lib/oms_runtime/projection.py), [work CLI](../scripts/lib/oms_runtime/work_cli.py), [events](../scripts/lib/agent-events.py) | One interpretation of task, plan, failure, evidence, and freshness | Canonical-state tests pass; internal Python/Bash query bridges removed without independent state readers drifting |
| Graph/source analysis: [graph package](../scripts/lib/oms_graph/), [Python parser](../scripts/lib/oms_graph/project/parsers/python.py) | Rust parser and graph pipeline, including Python input support | Symbol/import/call/scope/span and invalid-syntax fixtures match; no Python subprocess or embedded interpreter |
| Evaluation/experiments: [harness](../scripts/lib/codex_harness.py), [experiment modules](../scripts/lib/oms_runtime/experiment.py), [harness regression](../tests/harness-eval-smoke.sh) | Rust scenarios, grading, budgets, measurements, and reports | Baselines retain their meaning; evaluator and test helper Python deleted; requested external experiment commands remain explicit |
| State/admission: [runtime common](../scripts/lib/oms_runtime/common.py), [file locks](../scripts/lib/file-lock.sh), [inputs](../scripts/lib/oms_inputs/) | Rust storage and a single protocol/owner per domain | Contention, stale recovery, interruption, and corruption tests pass before writer switch; old lock holders and writers removed |
| Supervisor/panel: [attempt runner](../scripts/lib/attempt-runner.py), [supervisor](../scripts/lib/dispatch_supervisor.py), [terminal panel](../scripts/lib/terminal_panel.py) | Rust process lifecycle, queue coordination, and panel behavior | No orphan or duplicate work under cancellation/restart; OS-specific guarantees preserved; old launch/control path removed |
| MCP/hooks/providers: [MCP server](../scripts/oms-mcp-server.py), [hook state](../scripts/lib/hook_state.py), [provider registry](../scripts/lib/provider-registry.sh) | Native transport, hook handlers, provider adapters | Shared protocol/security fixtures pass for every supported provider; Python handlers and substantive Bash glue retired |
| Lifecycle: [installer](../install.sh), [update](../scripts/update.sh), [uninstall](../scripts/uninstall.sh), [Python runtime manager](../scripts/python-runtime.sh) | Native install/update/repair/uninstall and verified artifacts | Clean-host install/upgrade/rollback/uninstall pass; Python installation/shims/precompile logic retired through ownership-aware cleanup |
| Tests/templates/CI: [tests](../tests/), [templates](../templates/), [workflow](../.github/workflows/test.yml) | Rust-owned checks, fixture drivers, generated executable helpers, and release tasks | Source build, test, evaluation, package generation, and installed fixtures work without Python; remaining executable Python and invocation sites removed |

Preserve inert Python example/source data required to test Python-project support.
Do not delete that capability merely to make an inventory counter reach zero.

## Milestones and removal gates

### M0 — Preparation

Establish the workspace, inventory guard, this decision, and Rust CI. Keep the
production runtime unchanged. Exit when the new tool builds and its targeted
tests and inventory check pass on the reviewed commit. M0 is not a runtime port.

### M1 — First native command

Create the shared core and native CLI with `context-inspect` as the first bounded
slice. Use the existing [context tests](../tests/test_context_inspect.py) as the
contract source and the initial [projection vectors](../migration/contracts/context-inspect-v1.json)
as a seed. Wire the vectors into the native tests and extend the existing coverage
rather than duplicating a second full test suite. Cover strict JSON, path safety, bounds,
invalid inputs, and metadata-only versus source-verification behavior.

Exit with a native invocation, output/error parity, representative timings, and
installation selection that reaches Rust before the old Python preflight. Keep
Python only while comparison is necessary; the slice is not retired until its
Python implementation and bridge are removed without losing coverage.

### M2 — Output and read models

Port output processing after the accepted #216/#220 behavior is present. Then
port work/evidence read models against their canonical state sources. Shadow
comparison is allowed only on the same immutable captured input, with no writes
or hidden reruns of external commands. Sensitive captures must remain local and
subject to existing output limits and scanning.

Exit with compatible complete responses, explicit unknown/error behavior, and
measured public-command costs. Retire corresponding legacy implementations and
duplicate readers. A smaller lossy response is not equivalent to a complete one.

### M3 — Parsers, evaluation, and development tools

Select a Rust parser for the required Python grammar after running the existing
[parser corpus](../tests/test_oms_graph_parsers.py); a parser dependency is a
reviewed decision, not an assumed drop-in replacement for `ast`.
Cover import resolution, binding/shadowing, decorators, async definitions,
signatures, docstrings, locations, Unicode, and invalid syntax. Port graph,
evaluation, affected-check selection, and fixture drivers as coherent slices.

Exit when these functions operate on Python-free hosts. Delete Python parser,
grader, runner, and test-helper implementations once equivalent Rust coverage
exists. M3 may proceed alongside M2; its semantic acceptance criteria remain
independent of performance.

### M4 — Writers and process ownership

Cut over one state/writer domain at a time after M2 readers and the relevant
concurrency fixtures are ready. Drain or reject incompatible in-flight work;
record which implementation owns the domain. Port the supervisor and admission
path with restart, cancellation, signal, and descendant tests.

Exit with one authoritative writer and proven recovery for the exact state and
protocol version. Remove legacy writers/lock-holder processes. Rollback must
verify state compatibility; reverting the binary alone is not sufficient after
an incompatible state migration.

### M5 — Provider, hook, MCP, and lifecycle completion

Move remaining adapters and public entry points onto the shared Rust core.
Build and verify release artifacts for the supported host/architecture matrix;
record libc and deployment-target requirements from the binaries actually
tested. An Ubuntu CI runner name alone does not prove the Ubuntu 20.04 floor.

Exercise fresh install, existing-install upgrade, interrupted update, repair,
rollback, copy/symlink ownership, and uninstall. Remove interpreter provisioning,
Python shims, and precompile steps only when their lifecycle ownership is proven.
Never delete a user's independently owned interpreter or environment.

### M6 — Final removal and enforcement

Remove all remaining OMS-owned executable Python, Python-invoking templates,
transitional bridges, obsolete interpreter management, and substantive Bash
runtime implementations. Replace remaining OMS build/test/evaluation drivers.
Update installation requirements and architecture documentation to the tested
native contract. Enable the final acceptance gates below and remove legacy
comparison jobs only after their replacement coverage is established.

## Inventory and change policy

The [inventory tool guide](../rust/README.md) defines the exact lexical scope and
commands. It records Python sources and Python-bearing lines on selected legacy
executable/configuration surfaces. It is a footprint guard, not a call graph or
proof that an interpreter is invoked. It intentionally does not inspect Rust
source semantics or arbitrary dynamically generated commands.

During migration, decreases/removals are allowed; new rows, kind changes, or
increased units require an explicitly reviewed baseline change. CI never
regenerates that baseline automatically. The PR must name the compatibility need,
owning milestone, and removal condition for any temporary increase. New work in
an already migrated domain belongs in Rust. Do not add permanent Python islands.
Every retirement PR must lower the committed baseline to the measured remaining
footprint. Leaving an old upper bound in place would allow later reintroduction
below that obsolete bound; the guard alone does not ratchet its own budget.

The strict zero-inventory option is a useful milestone. Conservative markers can
include inert language labels or fixture data. Resolve those with a reviewed,
specific classifier improvement and evidence that the content is not executed;
do not hide executable paths behind broad exclusions or remove Python-project
support to satisfy the counter. Dynamic and transitive requirements still need
the clean-host evidence below.

### Temporary decision-proof recovery footprint (2026-11-13)

Migration supervision encountered archived cancellations that pin different
historical versions at the same decision path. Overwriting that path to repair
one version would invalidate another. The bounded legacy compatibility repair
adds parent-only `oms agent-plan snapshot-decision --decision-artifact PATH
--expected-decision-artifact-sha256 SHA`. It captures fresh raw bytes (at most
64 KiB), returns JSON `snapshot_path`, `sha256`, `created` with exit 0, and refuses
unsafe or mismatched input with exit 2. It changes no task, approval or receipt.

New cancellations capture the fresh reviewed source durably before saving the
plan. Historical validation may resolve only its exact pinned digest from the
private 0700/0600 `.oms/plan/decision-artifacts-v1/<sha256>` catalog if the source
changed or its final leaf is missing. Unsafe ancestors, links, ownership, modes,
oversize or corrupt bytes remain errors. A stale snapshot cannot authorize a new
cancellation. Legacy records without an authentic snapshot still refuse changed
bytes. Existing parent-only relocation preserves CAS, live-worker and original
task/lease evidence while moving a still-valid active decision pointer.

Publication uses an exclusive temporary file, fsync, exclusive link, temporary
unlink, exact readback and directory fsync. A crash between link and unlink leaves
two links and fails closed; orphan cleanup is not automatic. A later-refused
cancellation can leave a non-authoritative valid blob. Cache invalidation tracks
only referenced original and exact digest paths, including missing leaves; no
catalog scan or cached mutation authority is added. Retirement validation uses
the declared physical repository even when invoked from another directory.

The measured baseline becomes 348 Python source files / 178,366 physical lines
and 194 conservative reference paths / 4,788 lines: 542 rows. The new source
`scripts/lib/plan_decisions.py` has 220 lines; existing cancellation, retirement,
cache and canonical fixtures also grow. This explicit temporary increase is owned
by M4, not a permanent Python backend or a native migration result. Remove it
when native cancellation, immutable proof/relocation/archive validation, cache
invalidation, contention and crash recovery replace these consumers and tests.
The inventory guard remains unchanged and is never auto-refreshed by CI.

## Verification and final acceptance

Use the existing affected-check policy throughout migration. A Rust job does
not replace legacy regression gates while those implementations are active.
Run Rust formatting, locked builds/tests, linting, and the inventory guard for
the relevant changes. Initial tooling can be validated on a bounded CI lane;
expand platform coverage when runtime, filesystem, process, or packaging behavior
actually enters that lane. Preserve the stable required gate's failure semantics.

Before claiming complete migration, all of the following must be demonstrated
on the exact release candidate and its declared supported platforms:

- OMS installs, starts, serves MCP, handles hooks, maintains state, recovers
  interrupted work, updates, repairs, rolls back, and uninstalls in an environment
  where no Python interpreter is installed or obtainable through an OMS fallback.
- Rust source builds, tests, evaluation, packaging, and release preparation also
  pass without Python, including dependency build scripts and generated helpers.
- Installed assets contain no OMS Python program, embedded interpreter, or
  interpreter-dependent plugin/helper. Review transitive dependencies and spawn
  paths; PATH hiding alone cannot rule out absolute-path interpreter launches.
- Native Python-source analysis still passes the supported grammar and graph
  corpus; inert input text is never executed to recover missing parser behavior.
- Codex, Claude Code, and Antigravity fixtures cover their supported capabilities.
  Any live-provider evidence is separately identified; external project/provider
  Python requirements are not mislabeled as OMS requirements or silently hidden.
- State and hash compatibility, ownership, admission, signal handling, descendant
  cleanup, sensitive-output handling, and rollback contracts have preserved or
  explicitly versioned behavior. No uncertain side effect triggers a legacy retry.
- Required legacy implementations, bridges, and their now-superseded tests are
  deleted. Retained coverage runs through the native implementation.
- Inventory/classification evidence, clean-host logs, artifact hashes, and CI
  results are attached to the exact candidate. No claim relies on a prior SHA's
  success or on the absence of a compiler/interpreter in a developer session.

Python execution sentinels and process traces provide additional regression
signals, but neither substitutes for isolated environments and transitive build
inspection. Unrelated host Python installations need not be removed to test this;
use controlled disposable environments.

## Performance and progress reporting

Report current/native LOC, removed legacy LOC, temporary bridges, process launches,
wall time, and peak memory separately. Compare identical accepted algorithms and
outputs under a stated compiler profile, input corpus, repetition count, and
cache condition. Include public-command latency and persistent-MCP request costs,
not just a fast leaf function. No Rust speedup is established by preparation.

Each follow-up PR records: entry point/domain, pinned reference revision, behavior
preserved or explicitly changed, tests reused/replaced, native validation,
temporary dependencies, and exact retirement criteria. Merge bounded changes
through the existing repository rather than maintaining a long-lived rewrite
branch. Completion means removed dependencies and proven native behavior, not
the number of Rust files added.
