# Native agent-plan domain

This crate owns the schema-3 plan domain and `agent-plan` command in the root
Rust workspace. Native parent services supply lifecycle, Room, permissions,
verification and publication producers. A successful compilation alone grants
no task completion, release or publication authority. Legacy source names below
are historical porting anchors.

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
| [Native panel control](../oms/src/services_control.rs) and [landing](../oms/src/services_landing.rs) | `EvidenceSnapshot`, `Claim`, `Start`, `Review`, `Land`, `Finish`; `json::projection` / `json::receipt_digest` implement the shared landing fence |
| Room carry / reviewed research | `AcceptResearch`, `ApproveMainResearch`, `AcceptMainResearch`, `Satisfy`; receipt validation rechecks immutable stream prefixes and native ownership |
| Native runtime/work projections | `Plan::work_status_snapshot(&[&str])` calls the existing `oms-work-inspect` status validator; it cannot grant a mutation |
| State verification / landing recovery | `retirement::validate_ledger`, `validate_receipt`, `validate_active_lineage`, `completion::validate_done` return validated domain evidence |

The native parent wires these domains directly. There is no legacy command
backend or fallback.

`Claim --json` and `Next --claim --json` return the issued canonical task digest,
plan id and lease tuple in the same transaction that grants the claim. Projection
fields are never stored in the task itself. Native `DelegateBinding` admits only
that snapshot's Start/Review through `execute_bound_task`, retaining the ordinary
request, parent, context and writer-lock guards. It samples actual time after the
lock is acquired, so an expired lease cannot reuse an earlier Context timestamp.

Bound Review requires the actual native delegate index and its verified
artifact/patch bytes. Under the Plan lock, the owner checks the index and hashes,
captures immutable files under the existing Plan artifact owner, and saves
their references and original indexed provenance with Review. When the plan id,
lease, epoch, task digest and contracts still match, bound Review admits a
fast-forwarded checkout (`base_sha` is an ancestor of `HEAD`) while keeping the
retained index bound to the original `base_sha`; landing re-verifies the patch
against the current `HEAD` and refuses non-ancestor rewrites. Stale Review
bindings append `stale_delegate_binding` to `.oms/plan/late-results.jsonl` and
name the task, repository-relative artifact/patch paths and failed condition.
The selected immutable paths and digests also enable the ordinary exact
review-evidence checks at admission, Land and Finish. Neither this bridge nor
an indexed worker exit grants completion or publication; changed and late
results remain evidence without replacing a newer claimant.

`reviewed_context` validates the optional context-pack `lead_findings` contract
used by the desktop preparation tool and managed Plan execution. It bounds a
quoted excerpt to 4096 UTF-8 bytes, retains its exact source/page/range and
reviewed ready-task SHA, and re-verifies `source.answer_sha256` against the
current Lead record in the state repository at pre-claim and dispatch time.
`task_contract` owns the existing plan-run frozen-field identity so a claimed
task can be checked after its full task SHA changes. The original ready SHA
remains the Claim CAS input; the issued binding still owns Start/Review. Context
adds no task, model, scope, Room or completion authority and never changes the
canonical task record.

`discussion` is the shared producer/consumer contract for native advisory
councils. A proposal may carry `discussion: {path, sha256}` pointing to an
immutable `decision::retain_native` manifest. Apply validates strict JSON,
spec/plan/base/scope bindings, the exact question, complete rounds and synthesis
artifact/output digests. Each new task retains that reference; replay refuses
a different or missing reference. The optional field preserves legacy proposal
compatibility and adds no approval or acceptance authority. Autonomy requires
this evidence in its explicit automatic collaboration mode and rechecks it on
resume without rerunning the council.
The same native output validator backs `agent-call --output-json`, so planning
parses the exact provider stdout range instead of searching prompt/diagnostic text.

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
| `lifecycle_writer_before`, `LifecycleGuard::check` | Admission shares one absolute acquisition deadline and rechecks the native writer generation; unavailable bounded adapters refuse |
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
| `group_membership_before` | The same complete ownership evidence through bounded, cancellable native account probes, sharing the admission deadline |

Panel review accepts both the existing schema-1 single-candidate record and a
schema-2 composed record. The latter binds a content-addressed manifest containing
2–16 reviewed workers for the same task, lease and full base. Plan matches each
component to its indexed delegate and review evidence, verifies the original
artifact and patch bytes, and checks their ordered concatenation. A successful
native landing row binds the manifest, owning participant, tree and exact
verifier digest to the passing admission event and report. Plan independently
reconstructs the aggregate tree in a private index and checks the accepted
commit's parent and tree.

`Plan::team_admission_before(AdmissionRequest, deadline)` returns an opaque
contract guard. It pins the canonical writer order, expected immutable spec,
member generation, root creation event, exact task/criterion/base/policy and
actual requested route/access. Its `check` repeats producer validation with a
fresh clock and new binding observations. Same-SHA cancellation, stale owners,
dependency blockers, changed policy and expiry during projection refuse.
Member generations retain their existing i128 range.

One validation pass shares complete native group proofs across descriptor
checks, then re-queries every used group before success. No TTL or persistent
permission cache grants authority. Fixed Git proofs receive the outer deadline
and use the existing supervisor's process-group capture/cleanup. Ordinary Git
callers keep their existing defaults with absent deadline/group snapshot fields.

This guard supplies contract eligibility only. Resource reservation, actual
member enrollment, usage, cleanup and publication remain separate producers.
Callers acquire it before campaign/dispatch/usage locks, register through the
physical producer, release those inner locks, fence again and drop producer
guards before execution. The canonical Plan fixture covers delayed Git,
cancellation, expiry, route/generation changes and late permission/policy
changes; the native Root fixture verifies the real lifecycle writer and release.

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

The implementation creates no persistent projection cache or permission sidecar.
Canonical plan, receipt and lifecycle producers retain their separate writers.

## Integration and verification limits

The root's native control services supply these production adapters and CLI
routing. Other embeddings must supply the same producers. Native controller
identity and completed-approval capture remain coupled; an arbitrary controller
manifest is not a grant.

Group-writable authority components require the native account resolver above.
Without it they refuse explicitly. Legacy private-group permissions are accepted
only after checking both resolved explicit members and the complete primary-user
list against this account. That resolver must use the actual host account
database, including its configured providers; parsing only local account files
would not prove completeness on an NSS-backed host.

Focused checks use the root workspace lockfile and pinned Rust 1.99.1:
`cargo +1.99.1 test -p oms-command-plan --all-targets --locked --offline`.
`tests/cli.rs` covers plan transitions, canonical team contracts, scope,
generation, expiry and producer fencing; `tests/completion_gc.rs` covers retained
completion evidence. Native parent fixtures exercise actual lifecycle/Room and
permission adapters. A Linux fixture pass alone does not prove macOS, WSL2,
all provider paths, crash recovery or release/install correctness; their
required gates remain separate.

## CLI entrypoint

`owns("agent-plan", args)`, `run`, `run_with_services` and `run_with_io` drive
the full `agent-plan` surface (including `-h`/`--help`) for a caller-supplied
`cwd`: relative `--repo`, `--file`, `--proposal` and `--markers-dir` resolve
against that `cwd`, never the process cwd. Retirement and decision paths resolve
against the declared state/execution repository. Interruption is checked before
any state is opened; the exit code, stdout and stderr stay separate. The main
research approval command is rendered as `oms agent-plan approve-main-research`.
