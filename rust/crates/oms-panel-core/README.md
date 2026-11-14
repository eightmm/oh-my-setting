# oms-panel-core

**Implementation draft; blocked before parity completion. Do not wire this crate
into production panel mutations yet.** The standalone crate compiles, but that
is not a claim that the Python panel can be retired or that every contract in
its approximately 17,700 lines has been ported.

The implementation stopped at the brief's missing-native-API condition. Result
registration needs the canonical artifact-index writer. The only public
`oms-command-telemetry` entrypoints are `owns` and `run`; `run` explicitly owns a
read-only report. The reference writer is
[`panel_results._register`](../../../scripts/lib/panel_results.py), which calls
`ma_append_artifact_index` from Bash. No nonexistent `artifact-index emit`
command is invoked by this crate. `Services::register_result` refuses until the
owner provides the native writer. `Services::accept` also refuses until the
reviewed-publication producer is wired.

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
| `services` | `Services`, `PeerExecutor`, `NativeCommand`, `ExecutionRequest`, `CommandOutput`, `SubcommandServices` |
| `routing` | `Policy`, `Candidate`, `Route`, `RoutingInputs`, `Accounts`, `Account`, `Council`, `RoleTier`, `TieBand`; `allocate`, `kind`, `valid_route`, `route_family`, `routable_route_model`, `model_family`, `weekly_used`, `account_weekly_used` |
| `dispatch` | `DispatchRequest`, `Overlap`, `WIP_LIMIT`; `dispatch`, `command`, `scope_overlaps`, `check_wip` |
| `results` | `FinalizeRequest`, `AcceptRequest`; `results`, `outcomes`, `finalize`, `finalize_snapshot`, `accept`, `verified_patch`, `safe_summary`, `evidence_reference` |
| `review` | `Candidate`, `Settlement`; `verdict`, `automatic_review_workload`, `council_metadata` |
| `salvage` | `SalvageRequest`, `Capture`; `managed`, `capture`, `salvage`, `payload`, `encoding_warnings`, `record_provenance` |
| `release` | `release`, `classify`, `heartbeat_fresh`, `identity_gone`, `prove_native_absent` |
| `state` | `Snapshot`; `repository`, `snapshot`, `active_mains`, `hold_state`, `save`, `load`, `evidence_matrix`, `action_inbox`, `return_brief` |
| `cache` | `Cache`, `TickReads`; `fingerprint` |
| `metrics` | `MetricInputs`; `number`, `percent`, `stamp`, `duration`, `five_hour`, `quota_bar`, `percentile`, `aggregate`, `metrics_text`, `collect` |
| `health` | `project`, `work_health`, `alerts` |
| `activity` | `Event`; `events`, `summarize`, `record_summary`, `read_summary`, `summary_lines`, `background` |
| `files` | `Change`; `patch_paths`, `patch_changes`, `file_diff`, `overlaps`, `read_live`, `tree_rows` |
| `eventlog` | `EventLog`; `clean`, `line` |
| `since` | `Counts`; `since_counts`, `since_line` |
| `host` | `Host`; `herdr_active`; host request, pane, read, focus and zoom methods |
| `terminals` | `PrivateStore`, `SessionProof`; `process`, `process_gone`, `ordinary_environment`, `state_home`, `valid_id`, `preview`, `tmux`, `session_proof`, `open_terminal`, `read_terminal`, `controller_store` |
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
classification and positive host absence. A caller cannot obtain acceptance
from an exit code alone. Default host absence remains unproven.

`SubcommandServices` resolves the sibling executable named `oms`. Events,
attempt bundles, failure reports, telemetry reports and task reads use native
crate entrypoints directly. Other fixed verbs include `peer-delegate`,
`peer-review`, `patch-admit`, `patch-land`, `room`, `agent-plan`, `agent-task`
and `advise`. It performs no interpreter fallback, no shell concatenation of
arguments, no global cwd/environment mutation and no automatic replay.
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
  returns an uncertain-publication error. The current default writer always
  refuses; it cannot finish this transaction. Notification remains pending.
* Salvage writes `.oms/salvage-provenance/DIGEST.json` under `panel-salvage.lock`
  before `.oms/artifacts/delegate/salvage-TASK-ID.patch` and its `.patch.json`
  sidecar. Repeated payloads retain prior warning records. A kill may leave
  provenance only, or a patch without a sidecar; neither is evidence of an empty
  recovery. Each complete file is atomic. Capture uses isolated Git metadata,
  raw blobs, disabled external filters/hooks and temporary state removed on normal
  return/unwind. A hard kill can leave that scratch directory.
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
* Reviewed acceptance, plan finish and publication remain the owning native
  producer's responsibility. This implementation does not write Git commits or
  refs, and supplies no substitute completion proof.

## Unresolved implementation and compatibility work

1. **Blocker: native artifact-index writer.** Implement or supply the canonical
   writer through `Services::register_result`. Dependency/source changes outside
   this new crate were not authorized. Source anchors:
   `scripts/lib/panel_results.py:664` and
   `rust/crates/oms-command-telemetry/src/lib.rs:413`.
2. **Reviewed acceptance.** `Services::accept` needs the complete native exact-tree,
   lease, reviewed-assignment, recheck, commit/CAS, publication and recovery
   producer. The default adapter refuses it. `--finalize --landed-commit` does
   not yet implement the reference's plan review/finish recovery sequence.
3. **Dispatch parity.** Existing plan claim/start/observation fences, allowed-path
   merging, canonical backlog accounting per main, supersession/settlement of
   reservations, from-task findings composition, continuation carried patches,
   native session provenance, alias inheritance, fallback/account overflow and
   room rollover remain incomplete. `check_wip` implements the threshold but the
   draft reservation accounting is not the legacy per-main backlog projection.
   Dry-run input scrubbing and preliminary overlap checks exist; that is not
   proof of complete `--dry-run`, `--scope`, `--continue` or `--from-task` parity.
4. **Private supervisor parity.** Dispatch currently writes draft repository
   records. Salvage requires a private supervisor receipt and cannot consume
   those draft records. The full identity/envelope, boot generation, descendant
   ownership, legacy read-only import and launch reconciliation protocol remains
   to be ported. Do not treat repository receipts as release authority.
5. **Results/settlement parity.** Result selection, delivery retry, independent
   auto-review/rework, candidate supersession, admission binding and index repair
   are not complete. Finalization authority is a conservative draft projection,
   not the complete reference acceptance proof. `Settlement::step` records a
   caller's completed effect; it does not implement each effect itself.
6. **Salvage parity.** Missing-worktree indexed recovery, independently proven
   empty salvage, complete encoding/transliteration warnings, approval flags,
   paired publication reconciliation and non-UTF-8 path handling remain open.
   Non-UTF-8 paths refuse rather than being silently substituted.
7. **Read projections.** Metrics are a bounded subset of the reference report;
   cohort, gate, handoff, overlap and usage statistics are incomplete. Health,
   activity/provider usage, cache invalidation, snapshot lineage, retention and
   since/event display still need reference parity checks. Local time and Unicode
   display-width behavior differ in the current draft.
8. **Managed controls.** Full launcher/pane proof, workspace control/key tables,
   automatic reclamation, forget/restart, log identity and legacy private-record
   compatibility remain incomplete. The new job API owns foreground generations;
   it is not a complete detached registered-job CLI replacement.
9. **Inherited dependency discrepancy.** Required `oms-reference` declares
   macOS-only `proc_pidinfo` version 0.1.4 in its manifest and standalone lock,
   but that registry package is absent from `rust/Cargo.lock`. Standalone offline
   resolution includes it. No version or dependency source was changed; the
   strict root-lock-only requirement needs owner reconciliation before admission.
10. **Routing/account parity.** Catalog registration, public repository proof,
   exact family classification, complete config validation, account-home
   permission rechecks and refreshed quota collection require further work.
   `RoutingInputs` explicitly supplies observations; absence is not discovery.

These are exact remaining implementation items, not merely a request for more
runtime testing. No legacy executable was deleted and no production cutover is
claimed.

## Verification and source coverage

Ran with Rust 1.99.1, offline:

```sh
cargo +1.99.1 check --manifest-path rust/crates/oms-panel-core/Cargo.toml --all-targets --offline
cargo +1.99.1 fmt --manifest-path rust/crates/oms-panel-core/Cargo.toml -- --check
```

The final check and format check both exited 0. The first compile found two
reference-clone type errors in routing; both were repaired before the final
successful run. No dependency version was widened. No tests, reversed-patch
red/green runs, runtime fault probes or full repository gate ran: the task's
implementation-first instructions explicitly prohibit running tests in this
phase. Generated `target` output is removed before returning the worker patch.

Source inspection was scoped: graph API outlines covered all 22 named panel
modules and dispatch-supervisor orientation; decisive ranges were read for
routing, dispatch, results, salvage, review, release, private stores and native
producer APIs. It was not a full contiguous reading of every reference module.
Existing reference coverage includes `tests/operator/dispatch_results.py`,
`tests/operator/room_protocol.py`, `tests/operator/salvage.py`,
`tests/operator/auto_review.py`, `tests/panel-workspaces-smoke.sh` and
`tests/terminal-control-smoke.sh`. Those tests were located but not executed. A bounded contract search submitted
363 exported-name, file-name, option, JSON-key and message patterns across
`tests/`, `scripts/`, `docs/` and root README files; 618 reference files matched.
The source scan found no added lines combining the specified sensitive terms
with an assignment sign, and no literal absolute home paths.
