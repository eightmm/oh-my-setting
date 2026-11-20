# Native landing commands

This standalone crate owns parsing, help and dispatch for `patch-admit`,
`patch-land`, `land`, `change-guard`, `checkpoint`, `candidate`,
`scratch-worktree`, `cleanup`, `sync-shared`, `draft-pr`, `ci-status` and
`report`. Malformed invocations remain owned and fail locally. No command
dispatches to a legacy shell entrypoint or Python domain helper. Fixed Git,
GitHub CLI, Bash/ShellCheck and Python syntax probes are subprocess features.

The implementation phase checks compilation and formatting. This crate is
not a verified command-selector cutover: production producer integration and
behavioral parity testing remain necessary before enabling ownership in `oms`.
No workspace, selector, migration contract or existing source is changed here.

## Public adapters

```rust,ignore
pub fn owns(tool: &str, args: &[OsString]) -> bool;
pub fn run(tool: &str, args: &[OsString], cwd: &Path,
           interrupted: &AtomicBool) -> (i32, Vec<u8>, Vec<u8>);
pub fn run_with_source(tool: &str, args: &[OsString], cwd: &Path,
                       source: &Path, interrupted: &AtomicBool)
                       -> (i32, Vec<u8>, Vec<u8>);
pub fn run_with_services(tool: &str, args: &[OsString], cwd: &Path,
                         source: &Path, interrupted: &AtomicBool,
                         services: &dyn Services) -> (i32, Vec<u8>, Vec<u8>);
```

`source` is the executing OMS metadata root, distinct from the target project.
It supplies `VERSION` and the CI suggestion rule table. All adapters preserve
the process working directory, environment and signal handlers. The caller
sets the interruption flag. `run` uses `cwd` for both roots. Buffered report
publication requires explicit `--yes`; interactive confirmation belongs to
the embedding UI. A missing Python executable is reported as unavailable
metadata rather than becoming a native report runtime dependency.

With `Unavailable`, `checkpoint`, `scratch-worktree`, `change-guard`,
`candidate`, `land status|conflicts|notes|blame-session` and plain
`report --dry-run` run locally. `patch-admit` also runs locally (`ADMIT`)
when no `--plan-task`, `--covers`, `--verify` or `scripts/check.sh` applies;
under `Unavailable`, `--plan-task` refuses at `Services::plan`, `--covers` at
`Services::coverage`, and `--verify` records the missing
`Services::supervise` as `REJECT` (exit 1). `report --from-failure` needs
`Services::failure_report`. `land` itself never calls `admission_context`: after
Git/remote validation (a missing `origin` fails first) it reaches `supervise`
for the gate, `wait_siblings`, `publication_guard` and `after_land`.
`cleanup`, `patch-land`, `sync-shared` and `draft-pr` refuse at their service
boundary. Group-writable Git state needs the private-group account proof.

`Services`, `PublicationGuard`, `VerifyRequest`, `LandingRequest`, `Execution`,
`PublicationScan` and `Snapshot` are the integration interfaces. `Unavailable` refuses missing
authority; it does not infer authority from room membership, environment
labels, user-provided receipts or a successful worker exit.

| Service | Producer obligations |
| --- | --- |
| `plan` | The existing native `oms-command-plan::Services`, including native lifecycle/room projection, completion and approval producers |
| `admission_context` | Authenticated allowed paths, accepted first-parent publication and batch-prefix checks through `oms-reference::binding::Context` |
| `controller` | The actual executing image; its default is `Controller::capture_current` |
| `supervise` | Execute the declared argv in its cwd, enforce wall/output limits, cancel descendants, drain and reap the entire tree, and propagate launch/signal/timeout failures. A process-group-only success is insufficient for verification |
| `publication_scan` | Launch `review::scan_worker` as a dedicated native process. It lowers RLIMIT_AS; the parent must enforce wall/output limits and reap the whole process tree. Never invoke it in an embedded CLI thread |
| `publication_guard` | The canonical shared-state/canonical-sync fence, retained through publication; busy/unknown is a refusal, with lock contention reported as 75 |
| `landing_review` | Native main-reviewer and peer review receipts, exact reviewed patch/verifier/lease, salvage provenance/consent and mandatory security-review checks. Return the canonical review/done receipt digests |
| `panel_accept` | Internal `--panel-accept` bridge. Bind the exact patch, base, execution repository and verifier policy, then reread the live Main, Room, Plan lease, candidate and review evidence before admission and again immediately before apply. Caller JSON is an expected snapshot, never a grant; an unwired service refuses. |
| `landing_approval` | Request or read/validate an exact private approval; validation does not consume it. Bind execution policy, parameters, controller, base, digest, task, lease and attempt |
| `fence_landing` | After durable intent, perform plan review-to-landing CAS and reserve the approved grant. The intent stores the consuming approval version |
| `complete_landing` | Finalize actual plan completion and consumed approval from the exact durable intent; no blanket success flag may substitute |
| `recover_landing` | Revalidate current/historical plan lineage, supersession, cancellation/retirement, immutable receipts and approval consumption. Reconcile lineage/completion or abandon without applying a patch |
| `coverage` | Validate criterion IDs before work and bind executed verification evidence afterward; structural admission alone is inconclusive |
| `cleanup` | Native install ownership/residue producer, including additional-child, nested-Git, regenerable-tree and preserved-patch proofs. This host lifecycle portion is deliberately a required integration callback |
| `wait_siblings` | Observe actual sibling campaigns/worktrees; zero budget still probes and refuses live work |
| `after_land` | Install/handoff integration only after exact-SHA CI and full proof. Compare the installation target again; return the actual `rc` and authority |
| `failure_report` | Only allowlisted failure metadata, excluding command arguments and free text |
| `review_findings` | Fresh native findings projection. Unavailable findings cannot yield readiness advice |
| `ci_comparison` | Native read-only base-job comparison by default for explicit suggestion refresh; failure remains unknown |

The landing callbacks run under the canonical landings writer during the
intent/apply/lineage/completion transaction and must not reacquire that lock.
Capture allocation/publication/sealing use the reference producer's own lock
outside that interval. The separate native execution fence serializes native
intake while the canonical writer serializes apply with existing shell callers.

## Modules and reusable domain APIs

| Module | Responsibility |
| --- | --- |
| `admission` | `Policy`, isolated candidate/floor verification, scope, syntax, test-reduction and encoding checks, batch-prefix verification, regression observations, authenticated report/index/capture outcome |
| `landing` | Approval identity validation, frozen publication, canonical `receipt_digest`, intent/apply/lineage/completion and non-applying recovery |
| `delivery` | Exact-HEAD gate clone, request-stamped host receipts, push CAS, lock status, exact-SHA CI/full selection, shared checkout fast-forward and conflict preview |
| `guard` | Dirty-byte snapshots, shared path matcher, committed changes since begin, stale-owner/TTL observations |
| `checkpoint` | Staged/unstaged snapshots, integrity/reconstruction checks, mandatory restore backup, bounded PAX/USTAR untracked capture and exclusive missing-file restore |
| `candidate` | Linked results, finite numeric values, per-link supersession, conflicts, condition markers and single-tool/direction ranking |
| `worktree` | Marker-first scratch registration, physical root and `.git`/gitmeta/backpointer checks, exact-entry recovery/removal, branch sweep and independent gate clones |
| `review` | Exact Draft PR intent schema/digests, one-way push boundary, create-only remote CAS, exact Draft PR recovery and untrusted comments/merge observations |
| `ci` | `query_repo`, exact `conclusion`, `full_scope`, status/record/tick/history/flaky/protection/triage and restricted public repository API fallback |
| `suggestions` | Rule-table advice from existing CI, PR and review facts; never executes suggested actions |
| `report` | Strict `github_remote`/`github_slug`, allowlisted metadata, privacy refusal, bounded URL fallback, explicit issue/comment publication |
| `notes` | Scanned notes skeleton, bounded `receipt_bytes`, exact-byte `handoff` with its explicit recorded range boundary published first |
| `attribution` | `record_landing`, `record_commits`, `author_kinds`, blame-session and explicit local notes export; observations grant no authority |
| `red` | `split_patch`, bounded per-file advisory `check`, `Request` and `render`; never changes admission authority |
| `helpers` | Exact transient/panel-call classifiers, bounded sanitized worker guard details, local gate timing parser, recorded/range-base helpers |
| `managed` | Managed copy `marker_path`, `path_hash`, `read_marker`, `intact_copy`, `inspect_copy` and `owned_source_under`; markers require current-byte proof |
| `cli`, `git`, `storage` | Private parser, supervised fixed argv, raw tracked bytes/modes/index fingerprints, no-follow CAS/fsynced storage and canonical locks |

JSON records reuse the operations value type and the shared Python-compatible
JSON encoders. Plan reads go through `Plan`, including its receipt/decision
validation. Scope matching uses `oms-contract-markdown`. Task constraints use
the native packet parser. Reference scopes use `Protocol`, the actual native
checked lifecycle append, `Controller`, capture certificates and producer
witness validation. The CPython cached-main execution shim has no native
domain operation to reproduce.

## Concurrency and interruption residues

| Written object/ref | A second call or a kill between writes |
| --- | --- |
| `.oms/.gitignore` | Canonical lock, exclusive absence/CAS creation; existing contents are preserved. A kill may leave safe-fs staging/parent directories |
| Candidate ledger | One writer lock covers validation, prior bytes and replacement. Existing prefix is retained; old or new complete ledger survives, with a possible private staging leaf |
| Guard TSV | Writer lock and byte CAS publish a complete snapshot. End removes only the selected state. An interrupted begin leaves old/new complete state or staging |
| Checkpoint patches/archive/metadata | Checkpoint lock serializes calls. Metadata is published last; a kill can leave an incomplete directory that cannot validate. `--reuse` compares actual state digests |
| Tracked checkpoint restore | Recovery checkpoint precedes every tracked mutation. Interruption can leave an intermediate tracked state and its immutable backup; failed cancellation recovery requires owner inspection |
| Restored untracked file / notes output | Descriptor-relative O_EXCL preserves any concurrent existing file. A kill during writing can leave a new partial file; a second restore leaves it present rather than replacing newer work |
| Reference scope/result/readiness/proof and lifecycle rows | Native producer owns identity and monitor fences. Failure remains nonterminal/blocked; a kill can leave an authentic unfinished scope/reservation rather than a fabricated terminal result |
| Capture generation/certificate/private input/publication anchor | Reference producer quotas, locks, inode/certificate checks and terminal proofs apply. Publication-before-intent failure remains reserved for reconciliation/GC |
| Frozen landing patch, landings journal, artifact index, task/approval completion | Intent precedes apply; lineage and completion follow. Canonical writer excludes another apply/recovery. A kill retains the exact prefix; recovery applies nothing and keeps ambiguous/changed inputs outstanding |
| Attribution ledger | Observations append with canonical locks; commit projection holds the writer through deduplication. An interrupted append can leave a refused partial tail; attribution failure does not authorize or undo a landing |
| Gate clone/index/objects | Independent private TMPDIR clone leaves the source worktree registry alone. Normal return/error removes it; hard kill can leave the disposable clone. Explicit keep-worktree retains the admission clone |
| Scratch marker/worktree registry/gitmeta | Marker is durable before registration and removed last. Exact registry lock/backpointer checks serialize removal. Interruption leaves a vouched-for pending or partially removed scratch; unrelated entries are never pruned |
| Host receipt/log/active-holder | Land lock honors `OMS_LOCK_TIMEOUT`; contention exits 75. Receipt/log replacements are CAS/fsynced. A stale holder is inert unless its PID birth identity and a fresh held lock agree; status probes the lock directly |
| Remote target/source refs | Gate binds HEAD, raw tracked bytes and index SHA256. Explicit expected old ref/absence is used at push. A killed response is unknown and must be observed before any retry |
| Full CI dispatch | Exact target is checked, then dispatch uncertainty is persisted before the request. A later call observes it and never blindly dispatches again |
| PR intent/blocked/push-attempted sidecars | Intent writer and schema/digest checks retain one-way phases. Arming precedes push; an armed missing branch is never recreated. PR recovery first queries exact remote state |
| Comment packets/merge observations | GET-only evidence is explicitly untrusted. Append/publication failure retains the prefix and grants no action. No comment is posted/resolved by these modes |
| Report issue/comment | Preview and scan precede explicit publication. Each call sends at most once; a failed response requires remote inspection before repeating |
| Attribution notes ref | Only explicit `--export-notes` writes local notes. A second call replaces the selected commit's observation, as the existing export does |
| Shared checkout remote-tracking ref/branch/HEAD/index | Canonical publication fence excludes live calls; clean/detached/diverged checks and a second snapshot fence precede `merge --ff-only`. An interrupted Git operation needs ordinary Git inspection; nothing is pushed |
| Install links/copies/residue | Required native cleanup/install callbacks own their proofs, locks and reversible ownership transitions; this crate never deletes an unproven host target |
| CI ledger/tick marker | Tick is bounded and throttled, including after a completed run so same-head reruns remain observable. Writer locks preserve ledger prefixes; an interrupted lookup may leave only a throttling timestamp, never a successful CI observation |
| `.oms/feedback/events.jsonl` | Polling and comment reads normalize untrusted observations under one writer lock with a complete-ledger CAS. Duplicate observations are suppressed across process restarts; run attempts and edited comments retain distinct identities. A partial/corrupt or full ledger refuses further writes |

## External feedback observations

`ci-status record` and the existing `ci-status tick` entry poll workflow runs
and, when one open PR can be identified for the branch, bounded review comments.
The comment window contains the 50 most recently updated comments, requested
with GitHub's `sort=updated&direction=desc`; it is not a complete PR archive.
`draft-pr comments` feeds the same journal. `ci-status feedback --json` exposes
the latest 20 observed events (or `--limit N`) and their `feedback:<event_id>` identities without making
another network request. No daemon, webhook, provider call or publication is
introduced by observation.

These observations are optional for the ordinary CI and comment views.
`record`/`tick` publish the normal CI ledger first and retain their CI-derived
exit status if feedback collection or storage fails. `draft-pr comments`
publishes its existing comment packet first and still returns the comments.
Both report feedback failures on stderr with a fixed diagnostic and numeric
error code, without raw storage paths or remote error text. Interruption still
propagates. Comment JSON adds a response-only `feedback_observation` object:
`status: recorded` with `added`, or `status: unavailable` with `error_code`.
This attempt status is separate from the stored comment packet and its identity.

A failed poll can have preserved available CI observations before its PR or
comment lookup failed; the warning means durable feedback may be incomplete
or stale. Full or corrupt journals retain their prior bytes and are not
repaired automatically. Explicit `ci-status feedback` reads still fail on
invalid evidence. A successful ordinary view does not prove a fresh feedback
journal or relax validation by feedback consumers.

Each record binds the GitHub repository, PR number when known, exact source
head and either workflow run ID plus attempt or comment ID plus update/content
digest. An edit beyond the 600-character displayed comment excerpt still
changes its identity. Poll time does not affect identity. Records are bounded
and scrubbed; withheld content, unavailable run attempts, cancelled runs,
environment failures and stale heads cannot be inferred to be repairable.
An ordinary failed run remains `unknown` until a separate fresh comparison
establishes an actionable regression. External text is always untrusted
evidence, never a command, approval, scope change or execution grant.

The native autonomy adapter may explicitly select a reviewed event using
`feedback::repair_candidate` and `feedback::revalidate_repair`. Both use fresh
exact PR-head/source-revision reads. CI classification compares complete jobs
from the observed run attempt with the same workflow at the PR's current base
SHA; absent or mixed counterpart evidence stays unknown. Fresh diagnoses are
retained in the observation journal. The adapter supplies evidence only; the
autonomy runner owns the reviewed task scope, durable attempt claim, existing
one-shot Repair transition, delegation and admission.

## Implementation verification and remaining scope

Only Rust 1.99.1 offline compilation and formatting are authorized for this
phase. No tests, red/reverse-patch probes, remote publication or installation
were run. Existing tests were inspected as reference material and unchanged.
Runtime parity, output fixtures, every failure/edge, concurrent mixed writers,
kill recovery, Linux floor/WSL2 and macOS remain unverified.

Dependency blocker for the owner: the required existing `oms-reference`
manifest and its standalone lock already require macOS-only
`proc_pidinfo = 0.1.4`. That version is absent from the root `Cargo.lock`.
Cargo includes it in this standalone lock even on Linux. All other external
package/version/source triples match the root lock. Reconciling this mismatch
requires the owner to change the root lock or reference crate; both are outside
this worker's write scope. Linux compilation does not resolve this constraint.

The native `oms` binary supplies every service above
(`crates/oms/src/services_landing.rs`). `LandingRequest::frozen_patch` names
the published landing patch the intent records, and `LandingRequest::patch`
carries the admitted bytes, so the done receipt binds that exact path and
review can check controller salvage provenance. A `--covers` admission record also
carries top-level `covers`, `status` and `scope_digest` for the runtime
evidence projection. Isolated and remote execution profiles always require an
approval. Its admission context produces the task's allowed paths and fails closed on
panel acceptance prefixes, goal batches and dispatch rework grants it cannot
prove. Its cleanup removes only dead, clean, registered harness worktrees of
the owning repository and exact legacy install links. On Linux the whole-tree
`supervise` refuses while admission's reference monitor thread runs, so a
native verify (and therefore every `--plan-task` landing) is rejected until
verification moves to a dedicated process.

Known integration/parity work to retain in the parent's review: main/peer
review receipts are not checked natively (legacy `patch-land` did not check
them either); host cleanup does not yet reap dead locks, prune worktrees or
migrate backed-up skills; buffered report
confirmation is explicit-only; admission's verification-surface and encoding
classification needs comparison against the richer legacy selector/salvage
fixtures; branch/history/triage text renderers need fixture parity, including
base-job comparisons; automatic background delivery scheduling belongs to
the embedding supervisor; receipt handoff currently requires an explicit
recorded `push.base`; bounded archive capture requires UTF-8 names; the dedicated native scan-worker launch and complete end-to-end namespace
adversary coverage need the parent supervisor/verification lane. These are not claimed
as passing behavior based on compilation.

Nearest existing contracts include
[`patch-admit.sh`](../../scripts/patch-admit.sh),
[`patch-land.sh`](../../scripts/patch-land.sh),
[`land.sh`](../../scripts/land.sh),
`draft-pr-smoke.sh`,
`durable-writers-contract-smoke.sh`,
`scripts-smoke.sh`,
`dispatch_results.py` and
`auto_review.py`.
