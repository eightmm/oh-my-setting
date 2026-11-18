# oms-panel-core

Native library for the non-TUI panel control plane. Public command routing,
host launchers and app integration remain owned by the parent. This crate is
an isolated workspace: edition 2024, Rust 1.99.1 and `#![forbid(unsafe_code)]`.
It adds no external dependencies and does not enable
`serde_json/arbitrary_precision`.

## Public integration surface

| Module | Principal entrypoints |
| --- | --- |
| `services` | `Services`, `PeerExecutor`, `NativeCommand`, `ExecutionRequest`, `ActivityHeartbeat`, `CommandOutput`, `SubcommandServices` |
| `dispatch` | `DispatchRequest`, `CarriedPatch`, `Overlap`; `dispatch`, `command`, `continuation`, `scope_overlaps`, `check_wip`, `check_wip_with_limit`, `effective_wip_limit` |
| `results` | `FinalizeRequest`, `AcceptRequest`, `ResultsRequest`; `results`, `results_page`, `outcomes`, `finalize`, `finalize_snapshot`, `accept`, `accept_native`, `recover_accept`, `prior_findings`, `retry_delivery`, `verified_patch`, `safe_summary`, `evidence_reference` |
| `review` | `Candidate`, `Settlement`, `CouncilRequest`; `verdict`, `automatic_review_workload`, `council_metadata`, `council_plan`, `council_execute` |
| `release` | `MainSwapRequest`; `swap_main`, `release`, `classify`, `heartbeat_fresh`, `identity_gone`, `prove_native_absent` |
| `salvage` | `SalvageRequest`, `Capture`; `managed`, `capture`, `salvage`, `payload`, `encoding_warnings`, `record_provenance`, `empty_provenance`, `continuation_patch` |
| `terminals` | `PrivateStore`, `SessionProof`; `open_terminal`, `read_terminal`, `stop_terminal`, `terminal_gone`, `forget_terminal`, `session_proof`, `controller_store`, process identity/signaling and bounded preview helpers |
| `workspaces` | `list_workspaces`, `workspace_proof`, `open_workspace`, `focus`, `workspace_gone`, `forget_workspace` |
| `jobs` | `Jobs::{open,run_job,restart_job,poll,interrupt_job,stop_job,list_jobs}`, `job_logs`, `archivable`, `forget_job` |
| `history` | `capacity`, `transfer`, `archive`, `restore`, `forget_record` |
| `routing` | `Policy`, `Candidate`, `Route`, `RouteSnapshot`, `ProviderEvidence`, `RoutingInputs`, `Accounts`; allocation, catalog/model-family and quota helpers |
| `state` | `Snapshot`; acquisition, save/load, main selection and work-report projections |
| Other projections | `activity`, `cache`, `eventlog`, `export`, `files`, `health`, `host`, `metrics`, `since` |

`Store` and `Transaction` are root reexports of private storage primitives.
`native` reexports the existing producer crates. There is no CLI parser or TUI
in this crate.

## Lifecycle and selection contracts

* `forget_record` holds the active allocator and the selected record's lock,
  accepts active or archived records, checks repository/uid/kind/ID binding,
  runs the caller's positive disappearance/completion predicate, and requires
  exactly `lock` plus the expected files. Every entry must be an owned,
  single-link private regular file before deletion starts. Terminal forget
  proves global pane disappearance on the recorded owned server; workspace
  forget requires a positively absent session or server. Query failures do
  not prove absence. Job forget requires the exact positive generation and
  confirmed reaped exit. Unexpected files and stale generations preserve data.
* Restart requires an active, confirmed exited generation, matching recipe
  digest/repository/uid, canonical directory, valid explicit argv, and a log
  within `LOG_LIMIT`. Under allocator and record locks it publishes the next
  starting generation, clears the log and spawns. Spawn/setup failure restores
  both old log bytes and the old record. Successful starts clear prior exit,
  completion and pending-control fields and advance the generation once.
  Only the spawning reactor can signal its jobs. `Jobs::open` requires exclusive
  dedicated Linux subreaper ownership; a threaded host must supply its native
  job integration instead of constructing a competing reactor.
* Candidate selection follows explicit `panel_continues` chains. Fresh failed
  retries are superseded only by a later successful/admitted round with the
  same task, purpose, eligible parent lineage and verbatim covering scope.
  Multiple historical workers therefore need not imply multiple candidates.
  Independent candidates remain ambiguous for single-patch acceptance.
* Main lineage validates lifecycle shape and positive prior-host/native exit.
  Cross-provider ownership requires canonical room `main-swap` events read
  across sealed predecessors. Acceptance inherits only room-recorded swap
  anchors. Lifecycle refs alone cannot grant that ownership.
* Plan allowed paths can be extended only through the same lease's authenticated
  private dispatch launch, exact public file generation/bytes, and a matching
  retained brief digest. Repository launch projections cannot grant extra scope.
* Results retain participant/task paging, whole-main unassigned-child adoption,
  superseded/continued failure retirement, covering retry stage retirement,
  numeric review-run ordering, reason/guard classifications, verified answer
  excerpts, and bounded round deltas. `outcomes` suppresses decisions superseded
  by a same-named task's current plan generation. Retired tasks keep decisions.
* Finalization snapshots descendant ownership, task binding, admissions,
  supersession and evidence before and after verification. Live or uncertain
  checkout readers defer it with code 75. Confirmed stopped workers in review
  can settle through lifecycle compare-and-set after result registration;
  settlement errors are returned alongside the saved result. Missing explicit
  evidence can use uniquely verified matching admission artifacts. Owner decisions
  use canonical telemetry registration and schema-1 failure resolution.
* `recover_accept` explicitly reconciles an interrupted acceptance using its
  exact saved patch digest, task contract, review gate, base, reconstructed
  tree, actual index/worktree and HEAD. It never reapplies the patch. It can
  finish the exact staged tree or an already-created exact commit, then record
  plan review and telemetry idempotently. A changed authority or tree refuses.
* `--finalize --landed-commit` is represented by `FinalizeRequest.landed_commit`.
  It validates a private native plan-finish proof before canonical review/finish,
  checks the selected publication receipt digest and push base, and requires
  accepted-commit ancestry or exact patch replay. Reviewed batch landings also
  require the introduced bytes to remain in the published tree. Done-task
  retries reprove the exact publication. Verifier failure cannot finish the plan.
* Council default threads match Python UUIDv5 in `NAMESPACE_URL`, truncated to
  24 hexadecimal characters after `panel-council-`. The SHA-1 implementation is
  pure safe Rust and is used only for UUID name identity. Lenses use the explicit
  thread or `panel-council`, truncated to 126 characters, plus a unique suffix.
* Salvage imports schema-1 private controller envelopes and their immutable
  identity, boot, launch generation, nonce and process bindings. It requires a
  matching subreaper `all_reaped` receipt. Missing delegate worktrees can recover
  verified indexed patch bytes at their recorded base. Empty recovery also
  inventories indexed/unindexed patches, retained provenance and matching
  registered worktrees. Continuation rechecks an empty receipt against current
  evidence; a marker alone cannot discard reappearing work.

## Route identity snapshots

Routing policy remains schema 2 and rejects unsupported fields, versions and
candidate efforts. No configured model, tier, Main effort or catalog-refresh
behavior changes. Allocation now adds a schema-1 `Route.snapshot`; legacy routes
without it remain readable and receive explicit unknown policy/catalog provenance
when dispatch freezes them. Configured, explicitly requested, resolved and served
identities are separate. Selected/requested account aliases remain distinct from
unreported served account. Catalog account/endpoint bindings remain null. Unreported
served model/effort/source and unproven single/composite execution profile remain
null. Identifier-derived family is
marked `identifier_derived_unverified`, never served-family or native fanout proof.

`RoutingInputs.provider_evidence` retains bounded fields from the existing
`oms models --json` result through `ProviderEvidence::from_catalog_row`: reported
probe, catalog/probe ages, harness version when supplied, effort mechanism,
provider-reported efforts and exact-model effort scales. List hashes identify the
actual resolver input, including an empty or display-subset list; they establish
neither freshness nor live support. Read time is local receipt time. Missing age,
probe or exact-model scale stays unknown; provider-wide efforts do not prove that
a selected model supports them. The snapshot keeps the exact-model scale and
reported ages, so stale/failed cached evidence remains visibly bounded evidence.
Metadata text is limited to 256 bytes, effort sets to 16 entries, scale maps to
2048 entries and launch snapshots to 32 KiB. The existing model-list selection
and read-only catalog query are preserved; no refresh or provider call is added.

An optional catalog-row `capability_evidence` object carries schema 1, a bounded
`source`, nullable `account_alias`, `endpoint`, `observed_at` and `expires_at`
(Unix seconds). Its expiry must follow observation; no TTL is supplied by OMS.
Only complete scope/time metadata can yield `reported_fresh` or `reported_stale`
at the recorded read time. Missing scope, missing expiry or future observation
stays `unknown`. The snapshot separately records whether the reported alias
matches the selected account. A fresh observation for another account remains
visibly mismatched; it grants no eligibility, authentication or entitlement.
Endpoint credentials, query strings and fragments are refused. Legacy snapshots
without these fields retain unknown freshness and the same serialized bytes and
digest; unreported fields are omitted. Alias/endpoint claims without source
evidence are refused.

The current native `oms models` producer provides no such scoped observation;
its normal path therefore remains unknown. Known-scope examples in the existing
Core/Panel fixtures test catalog ingestion, projection and rendering, not native
account/endpoint capability. The next producer must bind the actual model-list context and exact
cache generation to an observed registry alias/endpoint and explicit expiry.

Requested effort is applied before allocation and snapshot capture. Known
exact-model effort exclusions, including a reported native-fixed scale, refuse
that candidate. Final resolved candidates recheck valid route, daily-cap and
Fable explicit-use restrictions before existing account selection checks the
resolved model. Public-only, role/access, account and independence checks remain
separate from tier ranking. These checks do not implement new billing approval,
shared quota pools, native-child caps or automatic escalation authority.

`Route::freeze_snapshot` binds the final candidate/account/role/access selection;
changing it after allocation refuses. A launch cannot reuse served observations
from an earlier call. The existing dispatch preview, immutable launch artifact
and reservation retain the snapshot and SHA-256; the process receipt retains the
same snapshot/hash. Settlement checks their admitted bytes before acknowledging
execution. Policy/catalog changes and rollback do not rewrite prior snapshots.
`panel --routes` exposes each chosen route's snapshot without launching a model.
Actual served observations must still come from the existing physical receipt
producer; this selection snapshot does not manufacture them from process exit.

## Native adapter prerequisites

`PeerExecutor` receives explicit argv, cwd, environment changes, input, timeout
and output bounds. Explicit environment values override removals of the same
name, including isolated `GIT_INDEX_FILE`. Only the existing verifier contract
uses `bash -c`; fixed native operations never concatenate shell commands.

`SubcommandServices` uses direct native events, room, attempt-bundle, telemetry,
failure and task readers/writers where available, and fixed sibling `oms`
verbs elsewhere. It has no interpreter fallback or automatic replay. The
current external dispatcher must still implement the invocation forms for
`peer-delegate`, `peer-review`, `peer-ask`, `patch-admit`, `patch-land`, `models`,
`agent-plan`, `advise` and `cleanup` before those routes can run.

The parent must additionally provide:

* `Services::swap_main_host`: the native pane-generation fence, bounded handoff,
  old-main exit, canonical `Room::swap_main` writer with its operations read
  guard, replacement launch and interrupted-host reconciliation. Core serializes
  calls and independently validates the resulting native/room lineage. A failed
  handshake is uncertain and never authorizes blind replay.
* `Services::publication_receipt`: the exact receipt bytes selected by native
  plan finish, plus native plan/landing services that authenticate publication,
  accepted-prefix and approval evidence. A zero command exit is insufficient;
  core reloads the finish result and independently checks digest/base/replay.
* `Services::patch_encoding_warnings` for non-ASCII patches: the lossless Unicode
  normalization/transliteration producer. ASCII has a dependency-free default;
  missing Unicode inspection refuses publication instead of silently dropping
  warnings. Non-UTF-8 paths still require the parent's lossless path adapter.
* A native dispatch supervisor that writes the authenticated private launch,
  process and whole-tree receipts. The default process-group executor does not
  claim containment of escaped sessions. Draft repository reservations are not
  supervisor proof. Native session/account provenance remains required before
  enabling explicit resume; fresh continuations can carry verified patches.
* `Services::deliver_result` for app delivery. Pending delivery intents survive
  failure, and only explicit retry may resend an uncertain notification.

These integrations are outside this crate's write scope. Library tests do not
authorize replacing the public Python panel or enabling unimplemented producers.

## Concurrent calls and interruption

* Store writes use held `oms-lock` guards and bounded descriptor-backed atomic
  publication. Concurrent writers serialize; readers see complete old/new bytes.
  A publication/directory-sync error is uncertain and cannot be blindly retried.
* Private allocations/updates/transfers/forget/restart share the active allocator
  lock; forget/restart/transfers additionally hold the record lock. A second
  destructive call observes either the changed record or absence and refuses.
  Forget is a sequence of verified unlinks, not a transaction: a kill during
  deletion can retain an incomplete directory requiring reconciliation.
* Restart writes `record`, then `output`, then starts the reactor and publishes
  running state. A kill can retain a starting generation with an old or empty
  log and possibly a child. No reader treats starting as confirmed exit. Rollback
  restores `output` before `record`; a kill during rollback remains unconfirmed.
* History marks then renames under the allocator/record locks. A kill between
  them leaves a source marker; ordinary readers reject the location mismatch.
  Source and destination directory synchronization completes publication.
* Dispatch writes a launch reservation before execution and updates its registry
  before its process receipt. A kill may retain an orphan launch or unresolved
  reservation. It uses the shared telemetry dispatch transaction with the same
  exact Flock namespace and byte limits. Settlement rechecks the admitted member
  and immutable launch identity; missing, malformed or changed entries preserve
  existing state and report uncertain effects. Only cleanup-confirmed awaiting
  calls enter lifecycle WIP reconciliation. A terminal attempt alone cannot
  settle reserved, running or unknown native custody. No reservation proves
  execution or native exit.
* Scheduled tick resumes append a deterministic `launch_intent` before the wake
  ledger, `launching` before the host call, and `fired` only after a positive pid
  acknowledgment. An intent without `launching` proves the host was not called:
  the next tick revalidates approval, contract, main liveness, pause/expiry and
  budget, then safely retries that occurrence without spending its wake twice.
  A missing acknowledgment or generic host error remains unknown; tick output
  and `tick schedule --list` expose the launch identity and reconciliation need.
  Such occurrences are never automatically respawned, even after a day rollover.
  The current detached host has no durable identity lookup or positive
  not-started receipt, so kills after `launching` remain an explicit recovery
  limitation. Legacy `fired` events stay consumed. This preserves at-most-once
  external effects and makes no exactly-once guarantee.
* Acceptance writes `.oms/panel/acceptances/TASK-DIGEST.json` before application,
  then records commit, plan-review and recorded stages. `index.lock` fences index
  bytes; HEAD publication compares the original ref. A kill can leave an applied
  tree, unreachable/already-published commit, reviewed task or unindexed accept.
  Explicit recovery validates actual bytes before completing remaining effects.
* Result artifacts are immutable under `panel-results.lock` and precede index
  registration and lifecycle settlement. A kill may leave a complete unindexed
  result or unsettled worker; registration/settlement are reconciled separately.
  Notification intent precedes app delivery and its immutable receipt; a kill
  between delivery and receipt leaves delivery unknown.
* Salvage writes provenance before patch, then sidecar and telemetry. A kill can
  leave provenance alone, a patch without its sidecar, or an unindexed pair;
  these cannot be reclassified as empty recovery. Temp Git metadata and private
  shadow finish plans are removed on normal return; hard kill can retain them.
* Export uses exclusive 0600 creation and does not replace an existing target.
  A killed export may remain partial. Host connection loss after a mutation
  leaves an unknown outcome and never triggers automatic replay.

## Verification

Focused checks use the pinned toolchain and offline dependencies:

```sh
cargo +1.99.1 test --manifest-path Cargo.toml -p oms-panel-core --offline --test parity
cargo +1.99.1 test --manifest-path Cargo.toml -p oms-panel-core --offline --lib url_namespace_vectors
cargo +1.99.1 fmt --manifest-path Cargo.toml -p oms-panel-core -- --check
```

`parity` is a single-threaded contract runner because dedicated Linux child
ownership refuses libtest's worker threads. Fixture environment changes apply
only to child processes; unit tests never call `std::env::set_var`. Fixtures
and isolated Git repositories live under TMPDIR and are removed after each run.
Tests exercise real job spawning/reaping and real fixture Git index/HEAD recovery;
provider, host, publication and app producers remain injected. macOS host/process
behavior and live external producers require owner verification.
