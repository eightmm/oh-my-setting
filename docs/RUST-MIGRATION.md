# Complete Rust migration

Decision date: 2026-11-12. Status: staged native context entry implemented locally; production deployment and legacy retirement remain pending.

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
  first native feature, now replayed by native library tests.
- The migration order, compatibility obligations, retirement criteria, and final
  acceptance requirements in this document.
- Focused Rust validation alongside the existing repository checks.

The workspace now also contains seventeen implemented native libraries: context
validation/session-byte input, descriptor-relative filesystem reads, domain JSON
encodings, output selection, Shell graph extraction, context text reports,
injection/session/timeline projections, task/project Markdown contracts,
deterministic failure taxonomy, work projections with lossless timestamp
handling, bounded read-only ledger/transcript acquisition, native Python graph
extraction through a persistent private helper, retained-output validation,
acquisition, paging and search, graph parser selection with
Config/Markdown extraction and shared-helper Python/Shell routing, ordered graph
resolution, experiment validation/normalization/digests, and bounded secret
scanning with retained-output redaction. A standalone native
`oms runtime context-inspect` binary handles manifest inspection, comparison,
current-file verification, injections, session reports and timelines. These
products have scoped native tests and independent review; they remain local
integration work until release.
The native compiler floor is 1.99.1; the preparation inventory tool separately
retains its 1.85 minimum. Lossless raw Read-label handling and the descriptor-backed
capture read producer and projection adapter are implemented. Experiment template/validate/compare, normal/grouped artifact telemetry, role
commands, parent input attachments and attempt bundles now have locally reviewed
native handlers. These handlers are registered in the native workspace; public routing is
separate. Notebook parsing and graph pipeline consumers remain in
progress.

Parent runtime probes verified SIGINT delivered on the real transcript read
syscall returns exit 130 with empty stdout, actual socket rejection, and readable
capture acceptance across file owners. A host-built native CLI failed the
Ubuntu 20.04 runtime probe on newer glibc requirements. A subsequent floor-native
build of exact baseline `9a5fcafa347a11f28dbb9111d073c4296c2e07c1` passed CLI
help, session, timeline, NSS root expansion and child denial in an Ubuntu 20.04
image with Python and PATH tools absent; the production helper passed a framed
parse. Required glibc maxima were 2.17 for the CLI and 2.16 for the helper.
These are baseline artifacts, not release binaries; the final changed artifacts
must repeat this proof. The host kernel was newer than the Ubuntu 20.04 kernel.
Darwin target checks do not establish native macOS runtime compatibility.
No installed command, MCP transport, hook, lifecycle operation or state writer
has switched to Rust, and no legacy source domain has yet been deleted.
Library counts and local commits do not satisfy M1 or M6 retirement gates.

The inspected production baseline is commit
`74f87f654a314961420be2aa8ab6dfc1999151e1`. Rebase implementation slices onto the
then-current main and record the actual comparison revision. In particular,
[PR #216](https://github.com/eightmm/oh-my-setting/pull/216) and
[PR #220](https://github.com/eightmm/oh-my-setting/pull/220) change output
processing contracts. They are not dependencies of this preparation change;
their accepted behavior must be incorporated before that domain's parity baseline
is frozen. Do not recreate an older digest implementation in Rust and call the
lost behavior a performance improvement.

## Current context evidence (2026-11-13)

[PR #223](https://github.com/eightmm/oh-my-setting/pull/223) is frozen at
`32f8517653d5c6cb41e6d7f40a6dcaee66e739e6`; its production reference is
`74f87f654a314961420be2aa8ab6dfc1999151e1`. The
[handoff](RUST-CONTEXT-HANDOFF.md) and [corpus guide](../migration/contracts/README.md)
remain unchanged verification inputs. Local native tests now consume all 40
inspection, 12 comparison and 12 raw-read cases, preserving per-case arguments,
JSON types, expected-null presence and verbatim hex input bytes. The source digest
and all three corpus blobs were independently checked. Native binary tests also
exercise the 39 inspection and 11 comparison cases representable as CLI options;
the two Boolean JSON-limit cases belong to the native typed boundary.

| Public context-inspect surface | Local implementation evidence | Retirement status |
|---|---|---|
| Manifest, --limit, JSON | Native 40-case library corpus and 39 applicable binary cases | Native-comparable; Python retained |
| --compare | Native 12-case library corpus and 11 applicable binary cases | Native-comparable; Python retained |
| --verify-current | Existing native current-source status/path tests | Native-comparable; filesystem gaps remain |
| --format text, --pretty | Existing native report and public binary cases | Native-comparable; complete frozen comparison pending |
| --injections, --session-hash | Native acquisition/projection and binary cases | Native-comparable; complete frozen comparison pending |
| --session, --timeline | Native acquisition/projection, child denial and binary cases | Native-comparable; complete frozen comparison pending |
| Invalid combinations and child policy | Native parser/binary cases; transcript denial precedes sensitive reads | Native-comparable; no whole-command retirement |

Native-comparable identifies implemented native paths and scoped matching tests;
it does not mean every public combination or installation path is accepted.
The separate frozen reference suite passed 27 methods. Its first candidate run
stopped at a stale root Cargo.lock under --locked, before producing a binary.
This batch reconciles that lock and workspace membership without changing the
external package versions/checksums. The candidate comparison must run again from
the unchanged reference checkout; its reference guard is not relaxed.

Existing source/symlink/rendered-copy fixtures with Python hidden are additional
regression evidence. They do not substitute for actual disposable installations,
inspection of the older copied-dispatcher receipt bootstrap, a Python-free host,
or dependency/build-helper audit on the exact current artifact. All remain
required. No production context Python implementation or temporary comparison
runner has been removed. The 34-line inventory reduction here is only PR #223's
replacement of duplicate Python test methods with shared corpus replay.

## HPC I/O work

The target includes busy XFS HDD storage shared over NFS to Slurm nodes. No
particular OMS hotspot was measured on that system. Prefer one snapshot/read per
command for derived reports, streaming bounded log/metrics processing and
immutable configuration reuse; avoid extra small-file metadata probes. Mutable
authority, approvals, delivery receipts and locks must still be revalidated at
action boundaries, with existing durability and crash guarantees retained.

A local unoptimized 41,288,816-byte debug artifact produced matching public and
private outputs for failure catalog and 100/12,000-row injection/session fixtures.
Two warm elapsed observations per case showed roughly half a second of extra
public-entry time. These observations do not isolate disk cost or prove an NFS
speedup. The worker could not collect syscall counts because ptrace was refused;
zero syscall fields in its raw records are placeholders, not measurements.
Source inspection identifies three executable hashing passes in the transitional
entry. Removing checks is not an authorized optimization. Native job-digest work
is eliminating the remaining full log rescan and adding streaming cancellation;
its final I/O and integration evidence is still pending.

## Target architecture

| Boundary | Target responsibility | Dependency rule |
|---|---|---|
| Shared core | Domain types, validation, projections, serialization rules | No CLI spawning for internal transformations |
| Native `oms` entry point | Arguments, output rendering, exit status, command routing | Calls the shared core directly; one public command spelling |
| MCP and provider adapters | Transport, provider-specific framing, capability projection | Reuse the core; no separate interpretation of domain state |
| Storage and supervision | Files, locks, journals, admission, children, recovery | Explicit ownership and platform behavior; one active writer per domain |
| Development and distribution | Inventory, tests, evaluation, packaging, lifecycle | Rust-owned implementation; no interpreter bootstrap |

The Python graph parser has one containment exception at the OS adapter boundary:
a caller may start a private native parser worker once and reuse it for its graph
session. The shared transformation core receives that session as a capability;
individual transformations do not spawn CLI commands. The helper is a private
libexec artifact, never a second PATH entry. Malformed source that aborts the
third-party parser must produce an infrastructure error without aborting OMS.
The adapter must bound framing, drain pipes, own cancellation/timeouts, and reap
its child; crashes cannot silently become empty successful graphs. This design
was selected after repeated malformed-input stack-abort reproductions and source
inspection found recursive helpers outside Ruff's optional stack-growth guards.
Implementation, portable resource guarantees and packaging remain subject to
independent review and runtime verification. A source-only design decision does
not establish M3 completion.

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

### Temporary recovery footprint (2026-11-12)

The reviewed preparation baseline includes a bounded legacy recovery change:
`oms agent-plan relocate-decision` moves a valid cancelled task's decision-file
pointer to a separate copy of the already-bound bytes. It keeps parent authority,
plan/task compare-and-set, link safety, live-worker checks, receipt digests and
archived plans intact. Keep the old path valid until every active reference has
moved; restore its historical bytes afterward. The command help gives the full
required arguments. Changed old bytes still fail closed.

This adds 78 physical Python lines in
[agent_plan_core.py](../scripts/lib/agent_plan_core.py) (2202 to 2280) and four
Python-reference lines in the existing
[cancellation smoke fixture](../tests/scripts-smoke.sh) (1047 to 1051).
The inventory has been reconciled to those measured values, with no new paths.
M4 owns retirement of this temporary growth: equivalent native cancellation,
relocation, archive validation, contention and crash-recovery coverage must pass
before deleting the legacy engine and its interpreter-dependent fixture driver.
This recovery work remains preparation for the complete migration.

### Native fixture footprint (2026-11-13)

The first native Shell graph corpus adds five inert JSON fixture paths under
`rust/crates/oms-graph-shell/tests/`: `fixtures.json`, `python_env.json`,
`python_fragment.json`, `python_long.json` and `python_other.json`. Together they
contain 31 conservatively counted Python-reference lines (24, 2, 2, 1 and 2).
Native tests deserialize these source/expected-result records; they never execute
the embedded Python text. Their exact projections were compared with the legacy
parser. This is required Python-project input coverage owned by M3, not an
additional interpreter invocation. Keep the records until the corresponding
native corpus is replaced or a reviewed data-only classifier refinement proves
those exact files inert; do not exclude `rust/` wholesale.

The native Python parser adds three further reference rows: its workspace
member (1 line), its Cargo manifest (8 lines naming native parser dependencies
and helper targets), and its 149-case JSON corpus (149 lines). The corpus is
deserialized by native tests; embedded Python source is parsed by the native
helper and never executed. M3 owns these 158 reference lines. Retain them until
the corpus is replaced or an M3/M6 classifier review proves these exact
dependency names and fixture records inert; no blanket Rust-tree exemption is
permitted. The helper is private and packaging must exclude its test fixture
binary. No Python interpreter or per-transform process fallback was added.

The graph facade adds one further reference row: its Cargo dependency on the
native Python parser (1 line). M3 owns this inert dependency-name reference until
a reviewed exact-path classifier refinement or dependency replacement removes
it. This is not an interpreter invocation.

The graph resolver adds one further inert native JSON corpus row containing 37
Python-reference lines. Native tests deserialize graph records and expected
edges; no embedded source is executed. M3 owns this coverage until equivalent
corpus replacement or a reviewed exact-path data classifier removes the row.

The measured executable footprint is now 347 Python source files and 178,121
physical lines, including the bounded 29-line M5 transition below. The conservative reference inventory rises from the original
194 paths / 4,782 lines to 204 paths / 5,026 lines for the Shell corpus and native
Python-parser names/data. The combined inventory now has 551 rows. No legacy
deletion or M6 zero result is claimed. Future retirement batches must lower the
baseline again.

### Temporary native-entry lifecycle footprint (2026-11-13)

The context command can now enter its staged native payload before Python
activation through source, symlink and rendered-copy entries. The trusted owner
stages an exact-revision manifest and fixed-size checksum; corruption fails
before executable dispatch. An absent current-revision payload keeps the
transitional legacy selection. This is not installed deployment or retirement.

Reversible copies add 29 physical lines to existing
[managed-target.py](../scripts/lib/managed-target.py) (246 to 275), with no new
Python source path. The optional schema-1 source hash keeps rendered copies
recognizable by older ownership readers. Existing shell/fixture reference rows
increase by 17 lines: install-contract 15 to 18, link 1 to 2, and lifecycle smoke
83 to 96. The install contract also contains temporary validator code for safe
native-cache quarantine; validator execution failure is distinct from a proven
invalid payload. Foreign or uninspectable content remains in place.

M5 owns removing this transition code after native installation, ownership,
repair, rollback and equivalent fixtures are accepted. The parent adjusts these
measured rows explicitly; CI never regenerates its own budget. The combined
inventory remains 551 rows. No Python deletion or M6 zero result is claimed.

Two demonstrated legacy defects remain explicit native-port requirements:

- [Artifact indexing](../scripts/lib/peer-common.sh) truncates the task goal with
  `cut -c1-200`; splitting a valid UTF-8 character can create a lone surrogate
  that the [index writer](../scripts/lib/artifact-index-store.py) cannot encode.
  An ASCII task header is the temporary operational workaround. M2/M4 must
  preserve the byte bound at a UTF-8 boundary and verify durable review receipts
  for non-ASCII goals.
- [Retirement validation](../scripts/lib/plan-retire.py), when reached through
  [state-verify](../scripts/state-verify.sh) from another working directory, can
  resolve archived decision paths against CWD rather than the supplied repository.
  M4 must use the declared physical repository identity and test that invocation.

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
