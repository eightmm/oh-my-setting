# oms-panel-core

**Implementation draft; blocked before parity completion. Do not wire this crate
into production panel mutations yet.** The standalone crate compiles, but that
is not a claim that the Python panel can be retired or that every contract in
its approximately 17,700 lines has been ported.

The canonical telemetry writer is now wired in process. Finalization registers
results with `register_panel_result`, then resolves prior schema-1 failed events
with the canonical writer's compare-and-set append. Durable notification intents
and explicit retry are implemented; the parent must inject app delivery through
`Services::deliver_result`.

`Services::accept` now runs a native acceptance workflow: owner/main/room/lease
and candidate/review preflight, an isolated product index, `patch-land
--panel-accept`, exact-tree commit plumbing, HEAD compare-and-set, plan review
compare-and-set, and recovery records. This has only compile verification and
conservatively refuses several legacy cases listed below. It is not a claim of
full Python acceptance parity or permission to enable production mutations.

The package is an isolated workspace with resolver 3, edition 2024, Rust 1.99.1,
forbidden unsafe code and the existing pinned external versions. It does not
enable `serde_json/arbitrary_precision`. The repository workspace, repository
lockfile, dispatcher, public panel command and migration contracts are untouched.

## Public integration surface

The following modules exist. Items describe implemented code, not verified
behavioral parity. Methods on public structs are part of the callable surface;
there is no CLI parser or TUI in this crate.

| Module | Principal public types and entrypoints |
| --- | --- |
| `services` | `Services`, `PeerExecutor`, `NativeCommand`, `ExecutionRequest`, `ActivityHeartbeat`, `CommandOutput`, `SubcommandServices` |
| `routing` | `Policy`, `Candidate`, `Route`, `RoutingInputs`, `Accounts`, `Account`, `Council`, `RoleTier`, `TieBand`; `allocate`, `kind`, `valid_route`, `route_family`, `routable_route_model`, `model_family`, `weekly_used`, `account_weekly_used` |
| `dispatch` | `DispatchRequest`, `CarriedPatch`, `Overlap`, `WIP_LIMIT`; `dispatch`, `command`, `continuation`, `scope_overlaps`, `check_wip` |
| `results` | `FinalizeRequest`, `AcceptRequest`, `ResultsRequest`; `results`, `results_page`, `outcomes`, `prior_findings`, `retry_delivery`, `finalize`, `finalize_snapshot`, `accept`, `verified_patch`, `safe_summary`, `evidence_reference` |
| `review` | `Candidate`, `Settlement`, `CouncilRequest`; `verdict`, `automatic_review_workload`, `council_metadata`, `council_plan`, `council_execute` |
| `salvage` | `SalvageRequest`, `Capture`; `managed`, `capture`, `salvage`, `payload`, `encoding_warnings`, `record_provenance`, `empty_provenance`, `continuation_patch` |
| `release` | `release`, `classify`, `heartbeat_fresh`, `identity_gone`, `prove_native_absent` |
| `state` | `Snapshot`; `repository`, `snapshot`, `heartbeat`, `active_mains`, `hold_state`, `save`, `load`, `evidence_matrix`, `action_inbox`, `return_brief` |
| `cache` | `Cache`, `TickReads`; `fingerprint` |
| `metrics` | `MetricInputs`; `number`, `percent`, `stamp`, `duration`, `five_hour`, `quota_bar`, `percentile`, `aggregate`, `metrics_text`, `collect` |
| `health` | `project`, `work_health`, `alerts`, `provider_catalog` |
| `activity` | `Event`; `events`, `summarize`, `record_summary`, `read_summary`, `summary_lines`, `background` |
| `files` | `Change`; `patch_paths`, `patch_changes`, `file_diff`, `overlaps`, `read_live`, `tree_rows`, `export_jsonl`, `export_bundle` |
| `eventlog` | `EventLog`; `clean`, `line` |
| `since` | `Counts`; `since_counts`, `since_line` |
| `host` | `Host`; `herdr_active`, `absent`; host request, pane, read, focus and zoom methods |
| `terminals` | `PrivateStore`, `SessionProof`; `process`, `process_gone`, `ordinary_environment`, `state_home`, `valid_id`, `preview`, `tmux`, `session_proof`, `open_terminal`, `read_terminal`, `controller_store`, `signal_process`, `signal_process_group`, `stop_terminal` |
| `workspaces` | `list_workspaces`, `workspace_proof`, `open_workspace`, `focus` |
| `jobs` | `Jobs`; `job_logs`, `archivable`; owned start, poll, interrupt, stop and list methods |
| `history` | `capacity`, `transfer`, `archive`, `restore` |
| `export` | `write` |

`Store` and `Transaction` are root reexports of the private `storage` module.
Root helpers include `decode`, `digest`, `now`, `timestamp`, `outbound`, `Error`
and `Result`. `native` reexports the shared producer crates for later parent
wiring; the decoded work-report entrypoints are called directly, not recreated.
Background and work-health projections currently live in `activity::background`
and `health::work_health` respectively.

## Services and execution boundary

`PeerExecutor::execute` receives explicit argv, cwd, environment changes, input,
timeout and output bounds. It never interprets an argv string as shell code.
The verifier is the explicit exception: its existing Bash command contract is
represented as `bash -c` with a separate argument.

`Services` supplies fixed native commands, contextual commands, artifact result
registration, reviewed acceptance, finalization authority, model-family
classification, app delivery and positive host absence. Tmux absence checks bind
server generation, physical socket identity and global pane/window tags. Herdr
query failure remains unproven, matching the Python refusal. A caller cannot
obtain acceptance from an exit code alone.

`SubcommandServices` resolves the sibling executable named `oms`. Events,
attempt bundles, failure reports, telemetry reports and task reads use native
crate entrypoints directly. Other fixed verbs include `peer-delegate`,
`peer-review`, `patch-admit`, `patch-land`, `room`, `agent-plan`, `agent-task`,
`peer-ask`, `cleanup`, `models` (the verified catalog verb) and `advise`. Room
commands call `oms-command-room` directly. The native executable in the current
repository does not own every remaining producer verb; an injected service or
later dispatcher migration is still required for those operations. It performs
no interpreter fallback, no shell concatenation of arguments, no global cwd/environment mutation and no automatic replay.
A native executable is an adapter precondition; the adapter does not attest
that an arbitrary file named `oms` is a reviewed native build.

The default process adapter supervises a process group. It is **not whole-tree
containment** against session escape. `Jobs::open` explicitly requests the
supervisor's exclusive dedicated Linux subreaper; this may refuse in a threaded
host or when another reactor already owns children. The parent should inject
its existing supervisor instead of creating competing global ownership.

## Concurrent calls and interruption

The following are implementation properties inferred from the source and shared
storage primitives. No concurrent or crash tests were run in this phase.

* All `Store` mutations require a held `oms-lock` guard. `oms-safe-fs` performs
  bounded, descriptor-backed reads and copy-on-write publication. A second call
  using the same lock waits or returns a lock error. Readers see complete old or
  new bytes. Publication/directory-sync failures carry `publication_uncertain`;
  they are not automatic retry permission. A kill can retain private temporary
  files and created parent directories. Existing unsafe objects are refused.
* `.oms/artifacts/panel-state/KEY.json` is replaced under `panel-state.lock`.
  Concurrent saves serialize; the last successful save is visible. Snapshot
  persistence grants no authority and failed acquisition marks retained readings
  stale.
* Dispatch writes `.oms/artifacts/panel-dispatch/CALL-launch.json`, then
  `reservations.json` under `panel-dispatch.lock`. A kill between them leaves
  an orphan launch record before execution. Concurrent reservations serialize.
  Return handling updates the registry before `CALL-process.json`; interruption
  can leave a reservation without its completion receipt. These draft files are
  not the legacy private supervisor envelopes and cannot prove whole-tree exit.
* Review claims live in `.oms/artifacts/panel-dispatch/settlement-DIGEST.json`
  under `panel-call-settle.lock`. The same member/status is claimed once while
  its owner is active; conflicting statuses refuse. A kill retains the owner
  generation and completed step bits. Reclaim requires positive old-owner
  disappearance. Side effects and step publication are separate: the producer
  must reconcile a completed effect before repeating it.
* `.oms/artifacts/panel-results/result-TASK-REVISION.json` is immutable under
  `panel-results.lock`. Verification occurs between two authority snapshots.
  A concurrent authority/evidence change refuses publication. The artifact
  precedes index registration; a writer failure leaves a local artifact and
  returns an uncertain-publication error. Prior failed rows are resolved only
  after registration. Notification writes an indexed pending intent before app
  delivery, then an immutable receipt and its index row. A kill between delivery
  and receipt publication leaves delivery unknown; only explicit retry may
  resend. Parent app delivery is unavailable in the default adapter.
* Salvage writes `.oms/salvage-provenance/DIGEST.json` under `panel-salvage.lock`
  before `.oms/artifacts/delegate/salvage-TASK-ID.patch` and its `.patch.json`
  sidecar. Repeated payloads retain prior warning records. A kill may leave
  provenance only, or a patch without a sidecar; neither is evidence of an empty
  recovery. Each complete file is atomic. Capture uses isolated Git metadata,
  raw blobs, disabled external filters/hooks and temporary state removed on normal
  return/unwind. A hard kill can leave that scratch directory. Telemetry
  registration follows the sidecar; a kill can leave a complete unindexed pair.
* `.oms/artifacts/panel-dispatch/CALL-summary.json` is immutable under
  `panel-activity.lock`. Concurrent differing summaries refuse. A kill leaves
  either complete summary bytes or the preceding/missing file.
* Private terminals/workspaces/jobs use the physical-repository digest under
  `XDG_STATE_HOME/oh-my-setting/panel-control`. Allocation and updates hold the
  active kind's `lock`. Record publication precedes external launch: a kill can
  retain a starting record and a possibly created tmux target. Nothing treats
  that as confirmed completion. A second allocation has a distinct random ID.
* Jobs publish `record`, then `recipe` and `output` before spawn. Polling writes
  bounded `output` before the updated `record`. A kill can leave an incomplete
  starting generation or older completion metadata beside a newer log. Only the
  in-memory spawning reactor signals its job; a restarted reader must classify
  unowned live generations unknown. Exit is published only after reaping proof.
* History holds the active allocator lock across marking and renaming a private
  record directory. A kill between marker and rename leaves the archive marker
  at the source; ordinary readers refuse the location mismatch. Transfer can
  inspect and reconcile it. Concurrent transfers serialize. Unexpected contents,
  a destination collision, capacity exhaustion or unproven absence refuse.
* Export uses exclusive creation with mode 0600 and no symlink following. A
  second call for the same name refuses. A killed writer can leave a partial new
  export; the file is never silently replaced on retry. Export does not upload.
* Host requests run once with bounded socket IO and response ID matching.
  Connection loss after sending a mutation leaves its outcome unknown and never
  triggers a replay. Tmux operations may have happened before a later binding
  record fails; starting records and existing targets must be inspected.
* Acceptance serializes per physical execution checkout under the canonical
  panel acceptance lock. `.oms/panel/acceptances/TASK-DIGEST.json` is written
  before application and updated at commit, plan-review and recorded stages.
  A second call refuses an existing recovery record; it cannot reapply blindly.
  Git's exclusive `index.lock` protects exact index bytes, and `update-ref`
  compares the original HEAD. A kill can leave applied changes, a stale index
  lock, an unreachable commit, a locally advanced HEAD, a plan in review, or
  an acceptance whose index row is missing. The record and Git/patch-land
  evidence require reconciliation; no automatic rollback or push occurs.
* Council process polling emits canonical operations heartbeats every 60
  seconds in `SubcommandServices`. Start precedes PeerAsk execution; confirmed
  exits transition to done/failed/cancelled. Unconfirmed execution/cleanup
  preserves a live lifecycle requiring reconciliation. A kill can retain that
  live attempt. Custom executors must honor `ExecutionRequest.heartbeat`.
* External briefs and summaries are owned, single-link regular files opened
  with NOFOLLOW/NONBLOCK, read within the byte bound and rechecked on the same
  descriptor. UTF-8, NUL and outbound validation apply before use. Dispatch
  snapshots brief bytes in a private temporary directory; concurrent calls use
  distinct scratch paths. Normal return/unwind removes them; hard kill may
  leave scratch files. Intermediate directory symlinks on caller-selected
  external paths are not rejected; the opened final file must satisfy policy.
* PID signaling uses Linux pidfds when available. The POSIX fallback and group
  signaling recheck the observed generation before signaling, but retain a
  check-to-signal race. Terminal stop proves pane disappearance on the same
  server/session, not the exit of escaped descendants. Its record explicitly
  leaves cleanup unconfirmed. A kill after host mutation can leave an older
  running record beside an absent pane.

## Implemented contracts and exact remaining work

* `DispatchRequest` carries `base`, `carried_patch` (`patch`, `sha256`, `base`,
  `attempt`) and `allow_stale_base`. The resolver selects this main's terminal
  worker, checks eligible verification failures, paired salvage/provenance or
  an empty-salvage receipt, verifies patch bytes/base, checks reverse/forward
  application using an isolated index and supplies carried-patch argv and
  canonical continuation environment. Native delegates must implement the
  carried-patch flags; the legacy Bash delegate only reads that environment.
  Like Python `_continuation_base_note`,
  a differing base that applies cleanly gives a diagnostic; a conflicting base
  requires `allow_stale_base`. Explicit native resume is refused without session
  and account proof. Room/main, label, alias, no-web and continuation metadata
  use the reference environment interface; scopes use the room join writer.
  `from_task` adds at most five bounded, scrubbed read-only findings.
* `results` now returns canonical top-level `rows`, `coverage`, `truncated`,
  `offset`, `omitted` and `omitted_range`. `results_page` adds task/participant
  selection, offset/limit paging and preflight inclusion. Rows contain the
  requested owner outcome, stage, evidence, delivery and action fields, with
  at most eight calls. Artifact/result/delivery digests are checked. This is
  schema implementation, not full selection/settlement parity: complete
  supersession/continuation retirement, whole-main child adoption, all reason
  and guard classifications, full answer rendering and round deltas remain.
* Council plans support repeated-provider policy seats, late duplicate-seat
  dropping within the 12-primary-call budget, catalog availability/partial
  policy, independent families, lenses with zero rounds, normal rounds 1..2,
  and until-agreement with a 1..6 cap (default 4). Lenses require unique names,
  2..12 seats, and three families for 3+ lenses unless partial is enabled.
  `council_plan` returns `oms-panel-council-plan`; `council_execute` owns the
  lifecycle and PeerAsk invocation. Deterministic default thread IDs use
  SHA-256 rather than Python UUIDv5, so cross-language thread continuity is
  not yet equivalent. The older `council_metadata` signature remains the
  normal-round helper; lenses and until-agreement use `CouncilRequest`.
* Acceptance no longer has an unconditional unavailable stub, but currently
  requires exactly one historical write worker and current same-provider main.
  Superseded retries, main swaps, authenticated extra scope grants, full live
  review-reader fences, accepted-prefix/publication recovery and automatic
  recovery execution remain incomplete. `--finalize --landed-commit` still
  needs the reference plan review/finish recovery sequence. Do not enable
  production acceptance based on compilation alone.
* Salvage publishes provenance, patch, sidecar and a canonical telemetry row.
  Provenance/sidecar/header/payload binding is checked before continuation;
  incomplete pairs refuse. An independently captured empty stopped worktree
  can receive an immutable empty-salvage receipt when no indexed/published
  patch exists. Missing-worktree recovery, exhaustive registered-worktree
  comparison, full encoding/transliteration policy and the private legacy
  supervisor envelope/identity import remain incomplete. Non-UTF-8 paths
  continue to refuse. Dispatch's draft repository records do not supply those
  private supervisor proofs.
* Heartbeats belong to `agent-events`, as in `panel_state.update_activity`;
  `oms-command-room` has no heartbeat verb. Room reads/writes use the native
  room crate. Catalog probes use `Services` and `models --providers ... --json`.
  File inventories can export bounded JSONL or a JSON bundle via the existing
  exclusive 0600 export writer. These helpers do not complete every private
  workspace, detached-job, retention, metric, account or routing projection
  gap recorded by the original draft.
* The standalone lock only gained the already-declared telemetry writer's
  transitive `oms-lock` and `sha2` edges; no package version or manifest
  dependency changed. The inherited macOS-only `proc_pidinfo` root-lock
  discrepancy remains owner work outside this crate.

The crate is **not ready to replace the Python panel**. In particular, the
current repository's native dispatcher does not own `peer-delegate`,
`peer-review`, `peer-ask`, `advise`, `patch-land`, `models` or `agent-plan` in
this standalone adapter's invocation form. Those producers and app delivery
need owner wiring outside the allowed write scope. Declaring a fixed verb here
does not claim its implementation exists in that executable.

## Verification and source coverage

Ran with Rust 1.99.1, offline:

```sh
cargo +1.99.1 check --manifest-path rust/crates/oms-panel-core/Cargo.toml --all-targets --offline
cargo +1.99.1 fmt --manifest-path rust/crates/oms-panel-core/Cargo.toml -- --check
```

Both final commands exited 0. Formatting was applied with the same pinned
formatter. No unit/operator tests, reverse-patch red/green runs, process fault
probes, runtime mutations, commits, pushes, full repository gates or macOS
checks ran: this brief explicitly restricts this phase to compile/format
verification. The added branches and negative/limit paths therefore remain
runtime-unverified. Generated build output is removed before handoff.

Source review was scoped to current core modules, native producer APIs and
specific Python dispatch, council, result, acceptance, host and salvage
contracts. It was not a contiguous reading of all Python modules. Graph API
queries for these new Rust files reported them unindexed, so direct source
reads were used. Reference searches cover changed symbols, options, JSON keys,
messages and filenames across tests, scripts, docs and README files; external
prose is outside the allowed write scope. Existing operator coverage includes
`dispatch_results.py`, `auto_review.py`, `room_protocol.py`, `salvage.py`,
`panel-workspaces-smoke.sh` and `terminal-control-smoke.sh`; none ran here.
