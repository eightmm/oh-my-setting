# oms-command-telemetry

The native CLI owns artifact-index views and mutations. The reusable `store`
module provides repository-confined transactions without a Python subprocess.
All code forbids unsafe Rust.

`store::register_panel_result(repo, owner, task_id, artifact_relative,
interrupted)` publishes a successful `panel-result` and returns its event id.
`store::append_artifact_index(repo, request, interrupted)` accepts an
`AppendArtifactRequest`: kind, provider, exit_code, artifact, patch, prompt,
verify_exit, source_artifact, task_id, optional index and task_goal,
env_overrides, inherit_env, and append options. Defaults inherit environment;
an explicit empty override suppresses a variable without changing the process.
Overrides use canonical OMS variable names, including run, operation, attempt,
parent, plan, model, reasoning, trace, review and criterion-coverage context.
Panel registration clears inherited plan, active-task, parent, attempt and
operation context, while retaining clean run and trace context. A valid
`.oms/runs/CURRENT` supplies a missing run id within `OMS_RUN_CURRENT_TTL`
(default 86400 seconds).

Internal artifact, patch and source paths are repo-relative or absolute under
the logical/physical repository root. Each must be a readable, owned,
single-link regular file without symlink traversal. The writer hashes their
bytes under the index lock, records relative paths and SHA-256 digests, and
uses the artifact or patch digest for artifact_id. Without either payload it
hashes event_id. External references record only basename, ownership false,
and a digest when safely readable. Prompt metadata contains only hash and byte
count. Existing metrics helpers best-effort cache duration_s, tokens,
provider_usage, served_model, configured_model, model_attribution and cost_usd.
Invalid typed review or coverage payloads refuse publication.

`store::append_rows` accepts exact JSONL bytes plus `AppendOptions` (keep,
high_water and an optional expected snapshot). Every new row requires a unique
nonempty string event_id. `parse_index_line` and `read_index` reject partial
rows, blank/non-object JSON, invalid UTF-8, NUL, bare CR, and non-finite JSON
constants; CRLF is preserved. Each row is at most 1 MiB; input and stored index
bounds are 256 MiB. Empty append batches only read. `replace_bytes` performs
bounded replacement with the same snapshot comparison and locking authority.

Retention reads `OMS_ARTIFACT_INDEX_KEEP` (default 1000) and
`OMS_ARTIFACT_INDEX_HIGH_WATER` (default keep + keep / 5); malformed environment
integers use defaults, while nonpositive keep or high below keep refuse.
`retained_lines` takes `RetentionOptions`. It preserves complete earlier,
unique resolution-target closures, required events, completion pins, and
unresolved failures. A resolution only settles a failure with matching
operation_id/artifact_id, matching parent_event_id/resolves_event_id, a complete
schema-1 outcome, resolution resolved, and exit 0. Recent panel decisions
(within 30 days and at most five minutes in the future, newest 3000) are
preferred within the byte budget. Protected rows do not consume the ordinary
keep floor. Required lineage exceeding keep or any hard protection exceeding
the byte ceiling refuses the transaction.

`completion_references` / `completion_pins` scan at most 4096 content-addressed
receipts, each at most 4 MiB and at most 64 MiB total. Invalid names, hashes,
duplicate keys, non-finite constants, shapes, unsafe files or observed changes
return None. None protects every index row, subject to the byte ceiling.

`canonical_index` rebases root aliases and validates confined paths;
`ensure_oms_ignore` initializes `.oms/.gitignore` to `*` plus LF only when
missing. Existing contents are preserved. Writers use `oms_lock::Lock::for_file`
with environment settings and Bash-compatible lock mode selection. The lock spans index reads,
payload hashes, snapshot CAS, retention and atomic replacement. Creation uses
0700 parents and 0600 files subject to umask; existing index permissions follow
`oms-safe-fs` replacement semantics. The temporary body is fsynced before
rename and its directory is fsynced where supported.

`salvage_index` plans or applies repair, returning `SalvageReport`. Apply first
publishes and verifies an exact, content-addressed, mode-0600 quarantine, then
replaces the corrupt snapshot with recovered rows and a required salvage
receipt. A second serialized call sees the repaired healthy index. A kill
between writes leaves the quarantine and old index; after index publication,
the quarantine already exists. A mismatch at an existing quarantine refuses.

`delete_orphans` returns `OrphanReport` and owns only `.oms/artifacts` regular,
owned, single-link bodies. It preserves indexed/completion paths, output-digest,
the canonical panel-dispatch namespace, lock names and standard index/ignore
names. Dispatch reservations and receipts belong to their native writers; age
and an absent artifact-index row cannot settle their execution custody.
It validates the kept snapshot
under the lock and refuses uncertain receipts. Scans exceeding 65536 entries
or depth 64 refuse before unlinking. Nonnegative grace is measured in seconds;
zero allows immediate deletion. Dry runs report candidates without unlinking.
Each unlink rechecks file identity and descriptor-relative ancestry. Deletion
is incremental: a kill or later error may leave a deleted prefix, which a
second call safely skips. This is a conservative bounded sweep, not a cleanup
transaction spanning every body.

StoreError preserves storage MutationPhase and exposes exit_code. Validation
returns 2, lock errors retain oms-lock status, observed snapshot changes and
uncertain/published replacement failures return 75, and cancellation returns
130. Concurrent writers serialize; readers see the old or new complete index.
A kill before rename may leave an exclusive temporary file or empty parents;
subsequent calls do not remove another operation's temporary file. Ignore
initialization can remain after an append failure. A publication error requires
state reconciliation before retrying; an identical subsequent write is not
proof of durability after the original failure.

Verification uses Rust 1.99.1 offline checks and focused CLI parity tests.
The owning agent retains dispatcher integration and full admission checks.

Native CLI ownership now covers every `artifact-index` argument vector,
including help and parse errors. `run` retains its existing signature. The
caller supplies `telemetry` as the action for both standard reports and
`telemetry --waste [--top N]`; top accepts 1 through 100 and cannot combine
with `--by`. Waste adds the schema-1 `waste` block with five categories,
reported shares and deterministic rankings. The selected index window only
limits delegate categories; model switches, injected bytes and stored outputs
use the retained hook ledger and capture directory. No report writes files.

The CLI implements list (default 20), latest, latest-run, failures, unresolved,
resolve, resolve-superseded, resolve-recovered, validate, migrate, salvage,
prune and import. Unresolved annotations and automatic resolution share one
join, with provider and complete-lineage checks before mechanical writes.
Explicit resolution validates every target before publishing one batch; a
second call reports already resolved. Migration and prune replace a checked
snapshot atomically. Prune supports --files, --stale and --dry-run; stale
cleanup preserves completion and recent decision pins plus resolution closure.
File deletion rechecks the current index after acquiring its lock, including
when a dry run reports a hypothetical retained body. Interruption can leave
an already deleted prefix; repeated sweeps skip bodies already removed.

For legacy parity, positional N is rejected for mechanical resolvers, and
latest-run emits the compact text summary even with --json. Empty read views
return schema/action/rows in JSON and fail in text; empty mechanical resolvers
report zero. Validate reports structural corruption without repairing it.
Normal views and mutations remain strict. Git worktree roots are resolved
using `git -C ... rev-parse --show-toplevel`; explicit logical aliases are
rebased before descriptor-relative index validation.

Import accepts --kind, --provider, --file, --prompt-file, --repo,
--artifact-dir and --exit, with the legacy provider aliases and custom adapter
discovery. It publishes an exclusive artifact before recording its index row.
A kill between the two writes leaves an unindexed artifact that can be
inspected or swept after the grace window; a second same-second import refuses
a filename collision rather than overwriting an existing answer. Ignore
initialization can remain after failure. Artifact destinations remain confined
to the repository, and external input files are read with the existing safe
producer helper. Sensitive content and unusable pasted answers produce
warnings. Index-accounting failure preserves the imported artifact and warns.

CLI writers and store producers use `oms-lock` LegacyAuto mode, matching
Bash's flock selection when flock is available and mkdir otherwise, with
OMS_LOCK_FORCE_MKDIR honored. Unsafe writable-by-others lock paths refuse;
interoperability fixtures create legacy locks under umask 077. Atomic index
publication leaves readers seeing either the old or new complete body. A
kill before publication can leave temporary bodies or empty directories;
after publication, retrying resolution is idempotent. Salvage always
publishes quarantine before the repaired index.

The crate tests compare native views, waste, argument failures, migration,
retention and mechanical resolution against the public legacy oracle, and
exercise batch atomicity, concurrent resolution, salvage and native import.
JSON comparison ignores object ordering; telemetry's existing golden-byte
fixtures continue to check its exact sorted output.

## Lifetime provider usage receipts

`store::usage::{begin, record, load}` owns `.oms/usage/receipts.jsonl` under the
same confined path validation, writer lock, atomic replacement and durability
rules as the artifact store. This ledger is independent of artifact retention:
it is never silently pruned. The existing 256 MiB storage ceiling refuses a
write rather than forgetting lifetime spend. A pending receipt is durable
before each physical provider invocation; a crash can therefore leave an
unresolved liability without needing a completed artifact.

Usage schema 3 and physical-call schema 4 add optional `account_scope_sha256`
evidence to the immutable call identity. This evidence describes private
account-home continuity. The reader accepts usage schemas 1–3 and physical-call
schemas 1–4, validates mixed histories as a whole and preserves existing
scope evidence on replay and settlement. A changed or missing proof breaks
cumulative token and cost attribution.

During the reader rollout, new headless calls retain the existing encoding and
validate the selected account home in memory. A named call's independently
measured usage is still recorded. Counters from a resumed cumulative session
remain unknown because the legacy receipt cannot prove continuity of its
account home. Terminal and cleanup evidence remains available. Keep this
encoding until all readers of the shared state support scoped receipts, including
any pinned Main or MCP processes. Rollback on already-scoped history requires a
compatible reader and preserves the original scope and history.

`store::budget` projects those receipts with canonical logical operations and
outstanding physical intents. A matching call/operation identity is counted
once, including an intent whose pending receipt was never published. The first
physical claim replaces its logical escrow; a retry adds its own liability.
Unknown dimensions retain the per-call floor after operation settlement, while
later measured corrections replace earlier amounts. Proven pre-invocation
refusal releases a no-call escrow only when no physical intent or receipt exists.
The candidate check compares the complete after-projection with explicit caps.
Campaign admission reuses this calculation and preserves its existing logical
call limits and output fields. This API performs no writes and grants no launch
or cleanup authority; callers must bind snapshots and serialize admission through
their canonical reservation writer. Native descendant coverage and resource
reservations require separate execution evidence.

`store::dispatch::Transaction` shares the existing physical repository's
`.oms/artifacts/panel-dispatch.lock` using direct Flock, independently of the
legacy lock selector. It provides strict legacy/schema-2 resource snapshots and byte
compare-and-swap at the existing 4 MiB registry path, plus immutable schema-2
launch and schema-1 process artifacts bounded to 64 KiB. Callers retain unknown
fields when patching their own records. Present empty or malformed state refuses
instead of becoming an empty registry. Reads and mutations recheck the guard and
anchored namespace, and waiting is bounded and cancellable. A prior artifact
publication remains an uncertain effect if the subsequent registry update fails.
The panel dispatch producer uses this shared writer and releases its guard before
calling external execution or room producers. This storage API supplies no
resource policy or launch permit by itself.

`store::physical` registers repository-local observed calls through that same
writer. Its versioned `physical.calls` extension preserves existing dispatch
records. A reserved intent precedes the pending usage receipt; an exact CAS then
links the receipt ordinal as `ready`. A ready record proves registration only.
The shared deadline bounds nested lock waits, and every lock is released before
returning to execution. The caller rechecks its launch fence after registration.
An identical call ID returns existing status without resending or rewriting; a
changed request refuses. Partial publication retains its intent and liability.
Read-only loading creates no state. These legacy observations prove neither
owner generations nor hard resource limits or native descendant containment.

`store::campaign` validates the original canonical saved manifest, its full digest
and the bounded complete events ledger. Logical admission and physical admission
share its usage-policy parser and operation-state projection. It preserves the
existing logical call/concurrency units; its active-reservation/deadline check is
an observed budget binding, without controller or owner-generation authority.
The canonical campaign producer remains the only manifest/event writer.

`physical::begin_checked` runs the caller's policy check under the shared dispatch
writer before any new intent. The caller retains its canonical campaign lock
across this registration and rechecks that outer guard after nested writes. New
campaign-bound call records use schema 2 and retain the full manifest digest in
their immutable request; schema-1 legacy calls remain readable. Replays with
changed binding bytes refuse. Partial records and receipts retain their liability,
and resource/usage failure never authorizes resend. `lock_file_before` reuses the
existing LegacyAuto lock for a target already validated by its owning store;
the lock itself supplies no file-access or launch policy.

`store::usage::begin_before` accepts the admission request's monotonic deadline.
Receipt and ignore-file lock acquisition share that deadline and retain their
existing LegacyAuto lock namespaces. A refused lock wait or failed preparation
check appends no receipt and consumes no ordinal. Filesystem persistence may finish
after the deadline; the caller must recheck its launch fence before executing.
Ordinary `begin` keeps the existing configured timeout contract.
Direct usage registration keeps its existing receipt writer and checks the domain
while holding it. Activation holds dispatch then that same receipt writer across
its history check and CAS, so a racing direct receipt is either retained before
activation refuses or refused after activation. An active resource domain requires the
physical producer's private registered-receipt path; a receipt-only registration
cannot bypass capacity. Normal receipt revisions remain independent.

`store::resources` adds an explicit `repository-host-local-observed` policy to
that same registry. Activation compares exact expected bytes and refuses all
legacy dispatch, physical or receipt history whose custody has not been
reconciled. It never deletes history or starts a new empty epoch over unknown
work. Schema 2 blocks schema-1-only writers; bound physical records use schema 3,
or schema 4 when their immutable identity includes account-home evidence.
Missing domains/claims, changed immutable bindings and unsupported outer legacy
dispatch custody refuse while preserving bytes. No policy activates by default.

Team slots, running invocations, open processes and write WIP are independent
counts derived from retained claims. Reviewer headroom bounds ordinary admission;
WIP is shared by exact owner/operation retries and outlives process exit. A new
physical intent and its claims publish in one CAS before the separate pending
receipt/ready acknowledgement. A later failure retains the reservation and
liability. Exact replay grants no resend. Holding a domain blocks new work while
allowing observations and settlement. Capacity refusal returns 3; held returns 75.

`begin_bound_checked` requires the native parent to validate the actual host,
root creation, Team/member generation, role and selected route. `Binding` itself
is correlation data. Execution settlement accepts only the sealed reactor proof
for the expected call and an explicitly pinned cleanup scope, with revision CAS
and idempotent replay. Usage/spend, persistent sessions, Team slots and candidate
WIP remain separate; a usage cleanup Boolean cannot release a resource claim.

`retire_team_checked` and `retire_write_checked` require exact owner/generation
and custody revision, and preserve a canonical decision reference and digest.
They refuse while the target retains independent execution/session custody;
Team retirement also waits for its candidate WIP. Member completion never
retires the Team. Legacy rows without a revision remain readable as revision 1.
Repeated exact retirement is idempotent and a changed decision refuses.

`DecisionEvidence` is correlation, not a public authorization payload. The parent
adapter must validate the actual canonical owner/result/acceptance decision for
the supplied `DecisionTarget` under retained producer guards before calling a
checked API. Its callback checks only pinned evidence and owned lock generations
while the dispatch writer is held; it must not acquire producer locks or run
native/Git proof subprocesses there. The store safely rereads the exact owned,
regular canonical source (at most 16 MiB), verifies its SHA-256 and checks the
callback again after CAS. The adapter revalidates its full outer authority after
inner locks drop. Failure after CAS remains publication-uncertain. A matching
file hash alone and free-form done/cleanup flags cannot supply the producer's
decision semantics.

`idle_session_checked` requires the transport producer's exact scoped
turn-terminal decision and releases only invocation custody. The process/session
remains counted. `close_session` requires the owning reactor's sealed cleanup
for the expected physical call and pinned scope, with revision CAS. It releases
that OS scope and makes no claim about an unobserved remote/native session.
Neither path changes candidate WIP, Team custody or usage liability.

`reconcile_checked` takes an exact registry snapshot and a producer-validated
epoch decision. It permits a new epoch only from held state with all four
custody counts drained. Resource schema 2 retains all prior policies and epoch
decision references alongside the unchanged physical intents, claims and usage.
The domain stays fixed; old epoch names and policy digests cannot be reused.
Inactive historical claims retain their original policy and host bindings.
Unknown usage remains a liability and is never reset or refunded by an epoch
transition. Unsupported versions refuse; resource schema 1 remains readable.

The native runtime exposes policy `host`, `status`, explicit `activate --policy`,
exact-policy `hold`, live-owner `discard-write`/`retire-team`, and held/drained
`reconcile` through `oms runtime resources`; see the
[resource CLI contract](../oms-runtime/README.md). The native Peer/Main factory
qualifies canonical Root/Team bindings around physical registration; unsupported
Team capability/resource/budget requests refuse. Team withdrawal and explicit
candidate disposition use canonical producer guards before/after the checked
retirement APIs; their callbacks check only pinned guards under dispatch. Team
withdrawal never supplies an OS cleanup proof. Epoch reconciliation preserves
unknown usage liabilities and requires independently drained custody.
The panel acceptance callback also uses `retire_write_checked` after reopening
canonical acceptance and each original candidate operation. It requires sealed
execution settlement, preserves candidate/usage evidence, and replays exact
decisions independently for composed workers. A failed custody write leaves
acceptance intact and is retried by exact panel acceptance recovery.
Unknown native descendants, provider-account capacity and cross-host global hard
bounds are unsupported. Calls without a qualifying native owner refuse in an
active domain. The existing usage fixture covers mixed-call concurrency, partial writes,
headroom, direct-receipt activation races, tampering, independent custody,
decision/revision conflicts, retained history and epoch reuse, scoped session
idle/close, and post-CAS producer-guard failure.

Each new registration or observation reads and validates the complete history,
then atomically replaces the ledger with that history plus the new receipt.
Work and replacement bytes therefore grow with ledger size. This bound includes
every historical revision; there is no automatic pruning, compaction or rotation.
Reaching the ceiling or finding malformed history refuses further accounting
and provider continuation. Deleting history would erase spend and unresolved
liabilities, so storage failure must be reconciled before another call.

Schema 2 records immutable `call_id`, `operation_id`, provider and any known
selected-model/account/goal/task/attempt/run identities. `selected_model` names
the selected route and makes no claim about an unreported served model.
The store allocates `ordinal`
under its lock before invocation. Each `revision` is an absolute observation
with phase `pending`, `partial`, `final` or `unknown`, nullable integer input,
output, cache-read, cache-write and total tokens, and the cache inclusion basis.
Cost is `reported`, `unpriced`, `subscription` or `unknown`; only reported cost
has integer `microusd`. Reported amounts identify `provider-reported` or
`provider-estimate` provenance. Zero is accepted only as an explicitly reported
amount. Decimal conversion uses integer arithmetic, rounding a sub-microUSD
remainder upward. OMS never computes a rate-card price from tokens.

The nullable `cleanup_complete` field carries separate native process-owner
evidence. A timeout may have confirmed cleanup while usage remains unknown;
final usage does not prove cleanup. Cleanup is limited to the executor's
supervision scope and does not prove bounded native fanout. Pending registrations
and schema-1 history retain null. Both schemas remain readable without rewriting
historical rows.
Once cleanup is confirmed, later usage corrections must preserve that fact.
This field alone grants no budget, dispatch or publication authority.

Repeated identical receipts are idempotent; changed data at the same revision
refuses publication. A higher revision is an explicit correction, including a
downward correction, and supersedes rather than adds to prior observations.
Older revisions remain auditable without replacing newer evidence. Calls with
conflicting immutable identity or ordinal refuse publication.
An identical receipt still validates the complete history under the writer
lock, but does not replace or fsync the ledger. That no-op is not durability
reconciliation after an earlier publication error.

Cumulative snapshots retain provider/account/session identity. Projection
differences successive snapshots in invocation order, not completion order,
and refuses to infer a per-call amount for a first-seen resumed session with an
unknown baseline. A session may first be discovered from an unmeasured pending
registration; later revisions cannot remove or change its scope or baseline.
All monetary snapshots in one provider/account/session must use the same known
cost provenance, including revisions: client estimates cannot be differenced
against reported charges. These conflicts refuse publication before mutation.
A decreasing cumulative total is exposed as a conflict and
unknown delta. Overlapping starts of a known session with pending/partial
usage refuse because the snapshot ordering would be uncertain. A later
measured correction can reconcile an unknown completion; timeout, process exit
and artifact resolution alone do not establish zero spend.

The existing `artifact-index telemetry` JSON adds `lifetime_provider_usage`
when receipts exist, and ungrouped text includes the same summary. This scope
is explicitly independent of index retention, N and date filters. Known totals,
unknown coverage, unsettled calls, conflicts and provider estimate counts stay
separate. Legacy artifact metrics remain the advisory retained-window view;
they are not imported as authoritative lifetime receipts.

`lifetime_provider_usage.storage` reports `retained_bytes`, `receipt_rows`,
`max_bytes`, `remaining_bytes` and `update_mode` from that same validated receipt
snapshot; ungrouped text prints a `usage storage:` line. Rows include superseded
revisions and legacy receipts. The 256 MiB ceiling and available bytes describe
the receipt file, excluding the optional reasoning supplement; they do not
predict how many more calls fit or measure device I/O. Each changed update still
uses `full-ledger-atomic-replacement`. Reports read no provider transcripts and
neither prune nor rewrite usage history. Missing receipts keep the existing
absence of a lifetime summary; unsafe or malformed history refuses the report
instead of becoming zero-sized history.
