# Native agent-plan domain

This crate implements the schema-3 plan domain inside an isolated workspace.
The root workspace, native CLI/selector routing, production adapters, migration
inventory, and deployment remain owned by the parent. Existing Python and Bash
sources and tests are unchanged. Compiling this crate does not establish parity
or authorize retirement of the legacy implementation.

The implementation phase intentionally adds no tests and runs no stored
acceptance/verifier commands, behavioral probes, repository gates, or CI. The
parent must independently review and verify the implementation before routing
public traffic to it.

## Entry points and consumers

`Request::parse(&[OsString]) -> Result<Request>` recognizes the existing command
and option spellings. Options can precede or follow the command. `USAGE` retains
the existing shell usage text for the parent CLI's help adapter. Path arguments
remain OS strings; textual contracts require the same bounded text checks as
their evidence producers.

`Context::from_environment(&Request, &Path) -> Result<Context>` snapshots caller,
repository, lineage, clock, and shared-lock settings. `Plan::open(Context,
&dyn Services, &AtomicBool) -> Result<Plan>` opens the physical source/state
repositories. `Plan::execute(&Request) -> Result<Response>` owns command
validation, canonical writer locks, fresh reads, transitions, durable writes,
and completion/retirement orchestration. `Response` contains the exit code and
separate raw stdout/stderr bytes. `Error` carries the refusal code and message;
`Response::failure` renders it. Help handling belongs to the CLI adapter.
The parent's repository resolver must supply the verified shared state root
when a managed scratch worktree shares its owner's plan. The convenience
environment constructor honors `OMS_STATE_REPO`; it does not independently
grant authority from a scratch marker or infer a canonical owner checkout.

The internal consumers have one authority entry point:

| Existing consumer | Native calls / returned contract |
|---|---|
| `scripts/plan-from-spec.sh` | `Plan::execute` with `ApplyProposal`; all tasks and the reviewed project contract publish in one plan CAS; exact replay is read-only |
| `scripts/goal-drive.sh`, `scripts/autopilot.sh` | `Plan::status_snapshot`, `Plan::execute` with `Next`, `RecoverOwner`, `Reclaim`, and `Accept`; schema-1 status retains verified/closed distinction |
| `scripts/lib/panel_results.py`, panel dispatch/admission/landing | `EvidenceSnapshot`, `Claim`, `Start`, `Review`, `Land`, `Finish`; `json::projection` / `json::receipt_digest` implement the shared landing fence |
| Room carry / reviewed research | `AcceptResearch`, `ApproveMainResearch`, `AcceptMainResearch`, `Satisfy`; receipt validation rechecks immutable stream prefixes and native ownership |
| Native runtime/work projections | `Plan::work_status_snapshot(&[&str])` calls the existing `oms-work-inspect` status validator; it cannot grant a mutation |
| State verification / landing recovery | `retirement::validate_ledger`, `validate_receipt`, `validate_active_lineage`, `completion::validate_done` return validated domain evidence |

These are integration targets, not a claim that existing scripts already call
the crate. There is no legacy command backend or fallback.

## Mode/source checklist

All modes enter the same request/domain layer. Source ranges below identify the
legacy behavior ported; they are reading evidence, not runtime test results.

| Modes | Native source | Legacy source |
|---|---|---|
| `init`, `ensure-lineage`, `add` | `engine.rs` | `agent_plan_core.py:993–1011,1354–1427`; replacement history in `plan-retire.py:719–754` |
| `apply-proposal` | `proposal.rs` | `agent_plan_core.py:1013–1313` |
| `claim`, `start`, `touch`, `review`, `repair`, `land`, `finish` | `engine.rs`, `product.rs` | `agent_plan_core.py:1600–1869`, `plan-receipt.py:30–39`, `land_receipts.py:14–159` |
| `amend`, `cancel`, `relocate-decision` | `engine.rs`, `decision.rs` | `agent_plan_core.py:156–193,1333–1352,1441–1614`; `plan_decisions.py:27–199` |
| `snapshot-decision` | `decision.rs` | `plan_decisions.py:164–218`; independent of loading broken plan history |
| `block`, `release`, `recover-lease`, `recover-owner`, `reclaim`, `reopen` | `engine.rs` | `agent_plan_core.py:1869–2045`; worker validation/liveness at `648–834` |
| `show`, `evidence-snapshot`, `list`, `ready`, `discovered`, `brief`, `next`, `status` | `query.rs` | `agent_plan_core.py:894–915,1931–1949,2047–2284`; `plan_waves.py` |
| `retro` | `query.rs` | `plan_retro.py:23–176`; tolerant bounded progress tail, advisory counts |
| `lint-verify` | `floor.rs` | `verify-floor-lint.py:24–172`; wrapper/redirect/compound/recursive shell handling |
| `accept` and `--reuse-pass` | `acceptance.rs` | `agent-plan.sh:980–1501`, `acceptance-reuse.py:19–124` |
| `accept-research`, `approve-main-research`, `accept-main-research`, `satisfy` | `completion.rs`, `provenance.rs`, `approval.rs`, `product.rs` | `plan-completion.py:49–2405`, `main_reviewer.py:577–744`, native lifecycle/room producers |
| `retire` check/apply, `completed-external`, `superseded`, interrupted recovery | `retirement.rs` | `plan-retire.py:148–977`, `agent-plan.sh:922–977` |

## Producer services and exact evidence

`services.rs` defines narrow parent adapters. Their default implementations
refuse operations requiring missing authority. The plan validates returned
records, identities, digests, state, and binding itself; no service takes or
returns a generic success boolean as completion authority.

| Service | Required producer / result |
|---|---|
| `lifecycle_writer` | Actual native agent-events writer guard. Completion lock order is landings → room thread → marker set → canonical plan → optional custom plan → native lifecycle writer |
| `project_lifecycle` | Complete input stream digest and exact native attempt projection. The plan also validates row framing, event identities, per-attempt sequence and relevant immutable ancestry/refs |
| `project_room` | Complete input stream digest and native room projection. The plan checks canonical timestamps/sequence, membership epochs, unique native owner, swap chain and successor membership |
| `supervise` | Whole-tree native supervisor: raw argv/cwd, wall/output bounds, environment removals, separate command/runner exits, timeout/launch/supervision flags and raw captured output. Output formatting/capture/failure adapters belong to the parent producer; plan does not duplicate those stores |
| `capture_main_reviewer` | Actual completed native Desktop approval producer. Returns exact schema-2 captured proof bytes. The domain revalidates native consumer/session, completed execution, output envelope, policy answer, marker nonce, source/verifier/controller and invocation bindings |
| `landing_approval` | Exact public approval projection and complete native private approval rows. The domain checks the request action digest, patch/base/task/lease, approved grant binding, monotonically versioned consume transitions and matching terminal public/private state |
| `controller_manifest` | Fresh manifest for the actual executing native controller. Path/hash/mode entries must match the source worktree; a bundle cannot supply its own executing controller identity |
| `publication_product` | Native panel publication producer's commit, accepted tree/base, patch digest and raw product proof. Domain checks review/lease/plan/evidence bindings and independently reconstructs the accepted patch tree in a private index |
| `normalize_provider`, `verify_selector_warnings` | Native registry and selector services. Three known canonical transports have defaults; other transports require the registry adapter. Selector diagnostics do not establish verification |
| `acceptance_reuse_context` | Optional fresh native context of raw tracked bytes, environment/config and executing controller. Missing context disables reuse and runs acceptance fresh |
| `group_membership_resolver` | Fresh native account-database lookup for an observed gid. Domain checks every explicit member uid and the complete primary-member uid list; a primary gid or group membership boolean cannot prove private ownership |

Persistent plan JSON preserves unknown fields and object insertion order. The
shared `oms-context` parser preserves JSON numbers and surrogate strings;
`oms-json` supplies Python-compatible sorted digest encodings. Strict evidence
rejects duplicate keys and nonfinite values. Plan writes use two-space UTF-8
JSON without a trailing newline. Typed completion objects use sorted compact
UTF-8 JSON followed by LF; their SHA-256 names bind the raw bytes. Decision
snapshots retain raw bytes with no encoding conversion or added newline.

Completion captures retain the exact room/lifecycle EOF in 256 KiB immutable
segments, content-addressed manifests and a trusted capture object. Live
streams must retain the complete captured prefix; later valid appends do not
invalidate an older receipt. Transaction pins and lease indexes retain
evidence while the verifier holds no writer locks. A missing evicted research
artifact yields an unverified blocked read view and cannot satisfy dependencies.

Source and verifier bytes, executable modes, absences/deletions, exact task
obligation projection, requirements map, native owner/attempt/lease, immutable
producer and reviewer records are revalidated. Acceptance snapshots bind HEAD,
symbolic/detached ref, index, diff, tracked modes and untracked leaves. Retirement
adds exact active-file generation, clean committed HEAD, per-call acceptance
proof and independent archive/ledger validation. User commands are supervised
features; fixed Git queries use argv directly and disable fetching.

## Concurrent calls and interruption residues

`oms-lock` supplies the existing Bash file-lock identity/protocol. State bytes
use `oms-safe-fs` no-follow reads and fsynced atomic mutation; descriptor-relative
ledger append and unlink enforce regular single-link files, anchored ancestry
and observed generation checks. Read and write bounds apply to complete inputs;
there is no freshness TTL or persistent projection cache.

| Written object | Concurrent second call / kill residue |
|---|---|
| Active/custom plan | Writer lock and byte CAS serialize cooperating writers. Old or new complete JSON survives; an exclusive safe-fs temporary can remain after a kill. Publication/directory-sync errors require reconciliation, never blind retry |
| Decision catalog | Same digest/exact bytes is idempotent; different bytes refuse. A failed cancellation may leave an authentic unreferenced capture. Legacy two-link remnants refuse rather than become historical authority |
| Cancellation relocation / manifest refreeze ledger | Row is durable before the single plan publication. A kill can leave a row whose resulting plan was never published; that row alone grants no transition |
| Completion segments/manifests/captures | Exact immutable objects are reused. A kill may leave unreachable captures; canonical GC remains owned by the parent and must keep current plans, receipts and pins as roots |
| Completion pin lease index / pin | Index publishes first. A kill may leave an index entry without a pin; after pin publication the objects remain retained. Lease settlement validates cleanup before publishing the plan and removes pins afterward |
| Completion receipt / plan | Immutable receipt publishes first. A kill can leave a receipt with an open original task. Exact bundle replay revalidates the frozen evidence and reuses the receipt; it cannot mint a different completion |
| Desktop approval capture | Leaves task open. Concurrent approvals get separate immutable nonces. Only an exact completed native approval can later consume one; no room/UID/environment label substitutes for producer identity |
| Acceptance progress/log | One O_APPEND row and file/directory fsync retain concurrent rows on supported local filesystems. A kill during a short append can leave a rejected partial tail. A row may exist without its diagnostic body; at most the newest 20 bodies are retained |
| Retirement intent/archive/receipt/active plan | Ordered intent → exact archive → receipt → generation-checked active unlink → intent removal. Every interrupted prefix is retained. Exact replay recovers receipt-before-unlink and post-unlink intent cases; unrelated fresh lineage stays untouched. Only the deterministic legacy same-inode two-link pair is repaired |
| Ephemeral Git product index/objects | Private TMPDIR directory and private object namespace; no repository index/object writes. Normal return/error removes it. An external hard kill can leave only that disposable directory |

No live plan, receipt, event, provider state or Git ref was changed while
implementing this crate. The implementation writes no new persistent cache or
projection sidecar.

## Integration and verification limits

The parent must supply production services above, attach native CLI/help and
selector routing, reconcile the root workspace/lock, and update central docs,
inventory and release artifacts. No callers outside this crate are changed.
Native controller identity and native completed-approval capture must be wired
together; an arbitrary controller manifest is not a grant.

Group-writable authority components require the native account resolver above.
Without it they refuse explicitly. Legacy private-group permissions are accepted
only after checking both resolved explicit members and the complete primary-user
list against this account. That resolver must use the actual host account
database, including its configured providers; parsing only local account files
would not prove completeness on an NSS-backed host.

Focused compilation uses the standalone manifest, its own Cargo.lock, the
existing pinned dependencies, and Rust 1.99.1. Formatting and outbound/source
hygiene are implementation checks only. Functional parity, failure/edge cases,
concurrent-writer/kill recovery, all provider paths, Ubuntu floor, WSL2 and macOS
runtime behavior are deliberately unverified until the parent schedules tests.

Nearest existing coverage was inspected in scoped ranges and left unchanged:
`tests/scripts-smoke.sh` covers the DAG/ready and single-winner lock cases,
lifecycle landing/main research, read validation, receipt history, satisfaction,
cancellation/amendment/relocation, retirement proof/ledger/recovery, next/brief,
lint and reclaim. `tests/operator/dispatch_results.py` covers reviewed-result
acceptance composition; `tests/functional-evolution-smoke.sh` covers managed
capture retention. Those cases, native service integration and platform/failure
edges remain scheduled verification work. The phase instruction forbids running
them or reversing the implementation for behavioral probes now.

## CLI entrypoint

`owns("agent-plan", args)`, `run`, `run_with_services` and `run_with_io` drive
the full `agent-plan` surface (including `-h`/`--help`) for a caller-supplied
`cwd`: relative `--repo`, `--file`, `--proposal` and `--markers-dir` resolve
against that `cwd`, never the process cwd. Retirement and decision paths resolve
against the declared state/execution repository. Interruption is checked before
any state is opened; the exit code, stdout and stderr stay separate. The main
research approval command is rendered as `oms agent-plan approve-main-research`.
