# oms-command-peer

Native peer commands and the root CLI's execution adapters use this crate.
The boundaries below distinguish implemented paths from remaining capabilities;
compilation alone does not establish complete compatibility.

The workspace uses Rust 1.99.1, edition 2024, resolver 3 and forbids
unsafe code. Dependencies resolve through the root workspace lockfile; no crate-local
lockfile or new dependency is needed for the embedded view assets.

## Entrypoints

- `owns(tool, args)` classifies the native command names without IO, including
  malformed invocations. Unknown names are unowned and return status 75.
- `run(tool, args, cwd, interrupted)` uses the checkout as development source.
- `run_with_source(tool, args, cwd, source_root, interrupted)` accepts the
  separately trusted installation root. Installed callers should use this.
- `run_with_executor(...)` accepts the public `process::Executor` execution
  boundary. `process::Native` uses the existing supervisor reactor and never
  changes the process-global cwd or environment.

All command entrypoints return `(status, stdout_bytes, stderr_bytes)`. The
library does not invoke the legacy Bash/Python command implementations. Git,
explicit verification commands and provider executables run through the
execution boundary. Online citation lookup additionally needs `curl`; an
unavailable request remains unverified.

### Council rounds

The native OMS host runs opening answers and rebuttals through scoped workers
with at most four active seats. On Linux each concurrent provider call uses
the existing isolated whole-tree supervisor, which acknowledges the actual
payload spawn before the parent releases launch custody and reports completion
only after reaping its descendants. The parent retains its own opaque
process-group cleanup proof; it does not deserialize or invent a whole-tree proof.
Single seats remain inline. Embedded executors, including `process::Native`,
run seats serially unless they explicitly opt into `Executor::council_parallelism`
with a concurrency-safe cleanup owner. `Sync` alone is not that capability.
All selected routes are checked before the first seat starts;
each seat keeps its own account route, disposable worktree, physical-call
receipts and completed-turn publication. Every seat in a round receives the
same immutable prompt, and the next round uses only the preceding round's
settled answers. Larger councils run bounded groups with the same round input.
Every started worker is joined before a round error returns. A failed thread
publication can therefore coexist with completed sibling artifacts; it grants
no retry or invented cleanup acknowledgement.

Typed synthesis may follow the opening round directly, or one through three
explicit rebuttals. The complete required provider/round matrix remains
mandatory before synthesis and before automatic decomposition.

### Session handoff prompt

Explicit `session-handoff prompt` calls the existing autonomy handoff renderer
through `native::Services`. It reads the selected repository's plan and task
records, then requests five typed native projections: task status, the runtime
evidence matrix, explained next actions, journal decisions and open review findings.
`process::Executor::handoff_projection` defaults to unavailable; the root host
uses its existing staged native launcher and supervised execution. Each request
has a 65,536-byte combined output cap and uses the remaining shared 40-second
read budget. Process cleanup may extend return time beyond that budget.

The Markdown draft contains Goal, State, Decisions, Open findings, Verified /
Not verified, Next step and Pointers with source labels. Missing, failed,
incomplete or oversized sources remain labelled gaps. The public prompt scans
complete plan/task snapshots and bounded producer output before projection.
A source that fails the sensitive-content check is withheld while other sources
remain available. The completed draft is scanned again before printing or
writing. Collection sources contribute up to six complete records under the
2,048-byte per-row budget. The final UTF-8 output respects `--max-bytes` and
counts omitted records reported by a producer, records dropped by the row
budget and lines dropped by the draft budget. Recorded verification claims
retain their scope as claims.

These projections run only for an explicit `prompt` request. Session-start
hooks, historical transcript capture, pickup receipts and native resume keep
their existing paths. No provider or model call is requested. The existing
`peer_contract` fixture exercises the public prompt, incomplete and unavailable
reads, malformed JSON, output bounds, cancellation and withholding one source.

## Modules and implemented paths

| Module | Responsibilities |
| --- | --- |
| `lib` | Command ownership, routing, interruption and output adapters |
| `cli` | Byte-preserving argv values, options, limits and command separator |
| `process` | Owned process execution, cancellation, output completeness, sanitized Git environment |
| `storage` | Physical roots, descriptor-anchored reads, locks, atomic files and stable-inode logs |
| `safety` | Shared scrubber, outbound refusal, redaction, hidden Unicode markers and untrusted quotation fences |
| `providers` | Targets, families, configured models, account validation/ranking, bounded capacity fallback and native result envelopes |
| `usage` | Canonical pre-invocation liabilities and provider usage receipts before native output decoding |
| `peer` | `peer-ask`, `agent-call`, `consult`, `advise`, prompt artifacts, threads, debate and synthesis |
| `delegate` | Detached worktrees, role defaults, patch capture before verification, repair and lifecycle review state |
| `plan_delegate` | Issued Plan snapshot, per-attempt lease fences and verified native result capture through the owning host |
| `review` | Criterion coverage, singleton confirmation, incremental review outcomes, finding anchors/dispositions, pinned verification scopes and advisory security pass |
| `review_red` | Advisory bare-base control and bounded individual test-patch checks in disposable worktrees |
| `native` | Runtime criterion projections through native task status, verified plan snapshots, room projection and lifecycle projection; typed handoff read adapter |
| `context_pack` | Bounded supplied graph-pack validation and reference-only orientation |
| `ml_context` | Sanitized ML project fields, manifest identifiers, entrypoint names, verification hints and recent experiment summaries |
| `thread` | Thread creation, typed Room append, context, schema-1 listing/statistics, stale-close preview/apply, current pointer, cursors, updates and consumption receipts |
| `session` | Repository-bound transcript discovery/search/open, origin-only bookmarks/reopen, guarded resume planning, source-labelled handoff prompts and prompts-only Antigravity capture |
| `digest` | Streamed file digests, capture redaction, retrieval descriptors, collision-resistant capture names and command exit propagation |
| `receipts` | External receipts, hash drift, DOI/arXiv normalization, offline/online verification and BibTeX export |
| `manifest` | CSV/TSV/line IDs, hashes, key mappings, provenance, drift, ID/key overlap and ordered time checks |
| `board` | Claims, stale recovery, heartbeat, finish/abort, owner-preserving replay and tag validation |
| `view` | Local result normalization, offline assets, capsules, source-bound metadata and idempotent feedback imports |

Path-scope normalization and bounded glob matching are shared through
[`oms-contract-markdown`](../oms-contract-markdown/src/scope.rs).

Core execution transports currently cover Codex, Claude, Antigravity, Pi,
Gemini and Qwen. Other registered transports are refused. Read calls in a Git
repository use a detached disposable worktree with the current tracked diff;
their tracked content and untracked files are checked before successful return.
This is an execution boundary check, not a whole-machine containment claim.

Every physical invocation in `providers::execute_env` now registers and
completes a canonical `oms-command-telemetry::store::usage` receipt. Account
overflow, model fallback, interactive turns and repair calls each receive a
distinct call identity while retaining their logical operation identity.
Artifacts link that operation; known task and attempt identities travel in the
explicit invocation environment. `OMS_STATE_REPO` selects the existing shared
state root, otherwise the owner repository is used. No credential or raw
provider output is copied into the receipt: only counts, known identities and
the output digest are stored. A receipt publication failure stops continuation
and leaves the registered liability for reconciliation.
Schema-2 receipts separately retain the executor's native cleanup evidence;
timeout or incomplete provider usage never manufactures zero spend. Missing
execution acknowledgements retain unknown cleanup. Schema-1 history stays readable.
`Invocation::physical_call` additionally carries the exact physical-call tag to
the native reactor. `Completed::owned_cleanup` retains its opaque, actual-scope
proof separately from the legacy Boolean observation. Synthetic executors leave
that proof absent. Resource-bound calls settle invocation and process custody
only with that exact call's allowed-scope proof. Usage, persistent sessions,
Team slots and candidate WIP remain separate; native fanout bounds are unknown.
Registration failure prevents that physical invocation. Completion-persistence
failure stops automatic account overflow and model fallback, even when provider
output is already available. Ordinary and budget-enforced calls share this
strict contract so an untracked retry cannot disappear from budget admission.

Registration first publishes a repository-local observed physical intent in the
shared dispatch registry, then its pending usage receipt, then a CAS linking the
receipt ordinal. All admission locks are released before the executor runs, and
cancellation or expiry at the final launch fence retains the pending liability.
A repeated physical ID returns status without resending. Partial publication
stops continuation until reconciliation. This observed registration does not
establish team authority, canonical owner generations or hard native limits.

An explicit `OMS_CAMPAIGN_MANIFEST_SHA256` binds a physical call to the original
canonical manifest and an active matching provider reservation. Empty, changed,
expired or settled bindings refuse before execution. Admission holds the existing
campaign events lock before the shared dispatch, usage and ignore-file locks,
using one monotonic deadline. The shared budget projection includes the candidate
physical call, previous receipts and outstanding intents; every fallback repeats
this check. The outer ledger guard and campaign deadline are checked again after
nested registration. All locks are released before execution. This observed soft
usage policy does not grant controller/root authority or a provider hard cap.

`session resume --apply` uses the same intent, pending receipt and final launch
fence before its native executor call; previews create no call liability.
Registered account homes supply an account ID only on an exact registry match.
Interactive terminal output remains unmeasured usage even after a successful
exit. Its digest and the executor's separate cleanup acknowledgement are retained;
an execution error without acknowledgement keeps cleanup unknown. The result
includes `provider_call_id`. Registration or completion-persistence failures stop
continuation and never authorize automatic resend.

The typed `admission` facade lets interactive Main use that same producer without
capturing or altering its TTY streams. It verifies the selected canonical state
root, carries the actual attempt and explicit resume identity, and returns an
opaque handle for the final fence and unknown settlement. Dropping a handle
does not refund or resend. Uncaptured output has no source digest; missing cleanup
acknowledgement stays unknown.

Explicit Team calls pass all four locators exactly once: `--team-id`,
`--team-spec-sha256`, `--team-member` and `--team-member-generation`. Ambient
Team metadata supplies no admission authority. The root native executor pins the
canonical Main creation/Room and Team/spec/member generation, task dependencies,
actual selected account/model/access, execution base and active observed host
policy around physical registration. A Main can qualify before a Plan exists.
Explicit Team effort constraints match only the literal the transport adds to
its argv. Missing or different effort refuses before registration. An unspecified
contract or interactive effort remains unknown, without applied/support evidence.
Actual child/depth guards still apply. Unsupported native-team, required native
capabilities, resource requests and Team budget references refuse.

After the resource writer wait, native qualification checks the original call
again without registering another call. Both acknowledgements are required;
missing acknowledgement, expiry or changed policy retains Pending liabilities
and prevents execution. Outer producer guards end before the provider runs.
Every account/model retry repeats this path with a distinct physical call.
Accounting or cleanup-settlement failure prevents retry. Owned cleanup can be
persisted after cancellation through an independent bounded settlement writer.
Interactive Main retains its TTY and therefore keeps unobserved cleanup and
usage unknown. These paths establish neither authenticated TeamRequest control
nor provider-account capacity, hidden-child coverage or a global hard cap.

The native Codex JSONL decoder takes the latest `turn.completed.usage` as a
thread cumulative snapshot, keyed by `thread.started.thread_id`; resumed
sessions without a tracked baseline retain unknown per-call usage. This follows
the [Codex exec event producer](https://github.com/openai/codex/blob/main/codex-rs/exec/src/event_processor_with_jsonl_output.rs),
whose current implementation reads the thread total. Claude `result` usage is
the total for one query, so assistant-step events are not added again.
`total_cost_usd` is labeled a provider client estimate, following the
[Claude SDK cost contract](https://code.claude.com/docs/en/agent-sdk/cost-tracking).
Other transports, incomplete envelopes, timeouts and missing measurements
retain explicit unknowns. Arbitrary answer text is not parsed as accounting
evidence, and an absent amount is never replaced with zero.

## Installation data read

The trusted source root supplies:

- `config/panel-routing.json`
- `config/role-contracts.json`
- `config/review-depth.json`
- `roles/*.md`, through `oms-command-agent-role` resolution

Bundled view defaults live only in `assets/`, embedded with `include_str!`.
The optional `scripts/lib/artifact_view_assets/` directory under the trusted
source root can override `analysis.js`, `view.js`, `view.css` and `view.html`;
the source distribution no longer carries duplicate defaults there. Each absent
file independently uses the embedded asset.
Existing empty files remain overrides. Invalid UTF-8, symlinks, unsafe ancestors
and other read errors refuse rendering instead of silently selecting a fallback.
The source root itself must still be a valid directory.

Role resolution retains repository and global overrides through the existing
role crate. Account configuration is read from the existing user configuration
location, independently of the trusted source root. Model caches and bounded
session logs supply account model eligibility and reported quota. No account
authentication contents are copied into artifacts. Native transcript roots
are independently checked and bound by provider metadata to the repository.

Provider accounts are limited to providers whose home is one verified environment
variable: `codex` (`CODEX_HOME`), `claude` (`CLAUDE_CONFIG_DIR`) and `pi`
(`PI_CODING_AGENT_DIR`, with `PI_CODING_AGENT_SESSION_DIR` removed from the child so
sessions stay in the account). Any other provider named in
`provider-accounts.json` or by `--account`/`PROVIDER:account=ID` is refused as
unproved; Antigravity stays fail-closed. `provider-accounts.json` `providers` accepts
`codex`, `claude` and `pi`; without `providers.claude`, `provider-accounts.claude.json`
adds Claude accounts after an implicit `default` (`$HOME/.claude`).
Canonical homes are unique across all providers; ids are provider-local.
Unpinned calls rank usable accounts by cooldown, recent-error penalty, 10%
weekly-quota bucket (Codex session logs only), running count when the policy is
`spread`, then registry order. A caller-set home variable is respected unless an
account is pinned. A pinned account never overflows. An unpinned capacity failure
records a 300s cooldown on `provider:id` and retries once on another account of the
same provider before any model fallback; with none left the first result stands.
Binding a non-default Claude account is refused while an Anthropic credential
variable is set. An unpinned native resume is refused as ambiguous when multiple
accounts exist or the sole registered home is not the default. Session discovery
retains registered account ownership when launching a native resume. Health and
`panel_account` use `provider:id`.

The files in `src/help/` preserve the public shell help as migration reference
text. They describe the target contract, including options which this snapshot
still refuses. Their presence is not evidence that every branch is implemented.

## W5b native boundaries

The following paths are implemented and compile checked; behavioral parity is
not established by compilation:

- `peer-review --covers` validates every requested ID against the runtime's
  project/task/plan criterion projections before reviewers start. Native task
  status and plan `Snapshot`/status validate captured bytes; work-inspect validates
  task status. Criterion digests are pinned in review scopes and rechecked before
  and after mechanical verification. Only its exit-judged `review-verify` index
  receipt carries coverage, task/plan lineage, observed HEAD and contract source
  digests; changed scopes cannot publish passing coverage. The executed command
  also derives its plan-acceptance criterion ID without claiming an unmatched ID.
- `--confirm` selects blocking findings reported by exactly one provider and
  excludes the confirming model family from its own claims. It sends at most
  20 findings, records missing/overflow labels as unverified, and appends typed
  `review-finding-confirmation` events separately from dispositions. Labels
  cannot change the gate or finding status.
- `--since-last` reads completed `review-outcome` index rows, pins the original
  base, checks ancestry and includes open blocking findings. Missing, moved,
  incomplete or rebased history explains the full-review fallback. These are
  advisory review scopes, not admission records.
- `peer-review advise --red-check` runs the declared command on the bare base
  and applies one test-file patch per disposable worktree, up to eight files,
  within the command time budget. Bad control, unknown paths, deleted tests,
  exhausted budget and incomplete execution remain `could not run`. This is
  advisory, never admission evidence. `{file}` expansion is shell quoted.
- Supplied `peer-delegate --context-pack` input is read no-follow at 256 KiB,
  parsed as strict JSON and scanned both encoded and decoded. Lists have a
  200-entry cap; paths, digest and bounded-line fields are checked before
  artifacts/worktrees exist. Rendered pointers are untrusted orientation and
  do not widen write scope. A supplied pack takes precedence over automatic
  graph orientation.
- Account spread ties now use native operations projections for attempts with
  explicit `panel_account` references, using the source repository rather than
  the disposable worker checkout. Quota ordering and explicit-route behavior
  remain in the existing selector. Provider-default Codex results record the
  selected account's valid configured model through `oms-toml`, respecting
  `OMS_CODEX_CONFIG`. Explicit correlated writer/reviewer families warn.
  Answer classification handles paired transport footers, strict stop markers,
  timeouts, refusal-only output and empty heading-only output.
- `thread append --room-event` enters `oms-command-room::Room::append`, retaining
  native identity, schema, rollover and authorization checks. Required producer
  callbacks still refuse when unavailable. `list --json` and `stats --json`
  use the legacy schema-1 wrappers. Staleness uses the newest parseable turn,
  strict idle-greater-than-TTL semantics and current/closed/unknown distinctions.
  `close --stale` previews; `--apply` locks and rechecks each non-room thread,
  including every parseable timestamp, before appending its close record.
- Bookmarks store source metadata and exact line/part hashes without transcript
  excerpts. Reopen validates repository/source identity and the original line;
  later transcript appends do not stale an unchanged line. Resume rejects custom
  ID-based stores and managed room identities pending the native panel adapter.
  Pi roots honor both native directory variables. Antigravity handoffs select
  repository-bound prompts from history and explicitly omit unavailable assistant
  output. Handoff prompts read the canonical `tasks.json` plan path.
- `output-digest --file` streams the complete observed file through
  `oms-output::build_reader`, retaining bounded source bytes and checking file
  generation and parent identity before returning. Command capture names include
  a fixed-width process-local sequence to distinguish same-second calls.

- `--ml-context` attaches a bounded ML digest to peer and delegate prompts.
  It includes only selected project fields, manifest identifiers, entrypoint
  names, verification hints and recent experiment summaries; raw data, logs,
  checkpoints and machine paths are omitted. Non-ML repositories emit no
  digest; sensitive derived output is withheld. Writes also attach the active
  task by default and honor explicit memory/context exclusions. This is the
  prompt summary path, not an execution or experiment-validation receipt.

### Native Plan delegation

`plan-run` passes its original issued task/lease/epoch, canonical task digest and
captured HEAD in `--plan-binding` beside `--plan-task`. Claim and Next return that
snapshot under the owning Plan lock. The host's typed Plan-services hook is
unavailable by default; the root host provides its existing ControlServices.
The peer read projections retain their read-only service surface.

The adapter derives the prompt, verifier, allowed/forbidden paths and role from
the frozen task. Assignment model/effort/workload conflicts refuse. The issued
base pins the source HEAD, worktree HEAD and index at the provider spawn fence.
Start advances the binding digest, and launch rechecks the current lease and
deadline. Capacity text cannot authorize automatic account overflow or model
fallback for bound work. Codex, Claude and Antigravity JSON transports require
their native terminal result. Incomplete delivery retains the worktree for
reconciliation.
A returned result must match the native index’s operation, attempt, provider, base and exact file
digests. The Plan owner then captures the artifact (at most 4 MiB), patch (at most
16 MiB) and index using its existing immutable store, rechecks the lease, and
records Review with both exact evidence digests. Admission, Land and Finish verify
those retained bytes again. A native worker
success does not grant landing, task completion or goal acceptance.

Canonical one-shot repair requires `repair_count=1`, the original reviewed
lease and recorded repair artifact. The calling plan-run owner retains cleanup
and known-failure terminalization; absent acknowledgement remains held. Replaying
the pre-Start binding cannot dispatch a second worker.

## Remaining compatibility work and integration blockers

The historical W5b dependency blockers no longer describe this workspace:
`oms-output` does not enable `arbitrary_precision`, and `proc_pidinfo` is in the
root lockfile. The remaining public peer executor returns buffered process
results and has no terminal control-input or streamed durable sink interface.
Interactive delegation and complete streamed command capture need that interface.

The remaining behavior is still refused or incomplete:

- Observed panel bindings, native provider-session relocation, candidate-patch
  admission and automatic apply remain refused by peer-delegate. The bounded
  Plan delegation path above does not implement those modes. `patch-admit`, `patch-land`,
  `candidate`, `change-guard` and `draft-pr` remain owned by the landing crate;
  the peer command ownership set is unchanged.
- Automatic `--graph-context` selection remains incomplete. The opt-in
  `--context-manifest` path uses the native Runtime compiler once from detached
  HEAD, including validated pack file targets and graph dependencies. Initial and
  repair prompts retain the exact bounded UTF-8 bundle; checked saved-manifest
  and policy digests bind it to per-call attribution. Valid partial bundles carry
  debt warnings; compilation or outbound failure starts no worker. This does not
  enable plan-task claims, candidate admission or automatic apply.
- Session checkpoint/fork/launch/compare/file restore, managed panel relocation,
  and full status projection need the native checkpoint/panel interfaces.
  Native resume uses physical registration and its
  final fence as described above. Prompts-only Antigravity capture is bounded by
  the general 64 MiB reader and is not a streaming history implementation.
- [Native artifact views](src/view.rs) refuse remote reads. The retired
  `artifact_view.py::ssh_read` used a remote no-follow reader. Any future
  transport must preserve remote no-follow validation. View embedding still needs frozen
  fixture comparison.
- Command digest capture remains bounded in memory, and retention is not complete.
  General acquisition remains 64 MiB, threads 16 MiB and native transcripts
  32 MiB; supplied packs and model configuration have their tighter bounds above.
- Dispatch-anchor fallback for account counts, complete config warning/fallback
  behavior, routing history/statistics, `telemetry_waste`, recorded search,
  prompt/instruction audit, job-digest/experiment attachment, export/import receipts,
  semantic deliberation enforcement, until-agreement stop reports, advice
  triggers, task observation and persistent failure recovery remain incomplete.

The original implementation-first brief deferred behavioral verification.
Current native fixtures cover the implemented paths listed below; they do not
establish every compatibility item above or live provider/platform behavior.

## Concurrent calls and interrupted publication

All state mutations use canonical `oms-lock` derivation. Locks serialize a
second writer, which re-reads the current body after acquiring the lock.
Interruption is checked while waiting and before publication.

| Written files or records | Second call and kill behavior |
| --- | --- |
| Thread logs, experiments, external/literature receipts, artifact index, review findings/events and view feedback | Locked stable-inode append. A second call observes the preceding record. A kill during append may leave an incomplete tail; subsequent writes refuse it until reconciliation. Acknowledgments and feedback imports deduplicate their complete records. |
| `CURRENT`, prompt/output artifacts, patches, manifest JSON, scopes/verdicts, bookmarks and handoffs | Locked atomic replacement. A reader sees old or complete new bytes. Create-only writes refuse an existing file, including an empty file. A kill can leave a private temporary file; a publication/durability error requires reconciliation before retry. Manifests and the current pointer deliberately allow a later replacement. |
| Delegation lifecycle events and artifact index | Existing native operations own lifecycle append. Patch bytes are published before verification and final output. A kill between these writes can leave a review-state attempt without its final artifact/index row; admission stays pending and the owner must reconcile it. No child report grants admission or publication. |
| Advisory red-check worktrees, owner marker and Git registration | Unique per-call names isolate concurrent calls; Git owns its metadata locking. A kill may leave a marker, partial registration or detached worktree. Cleanup requires the original worktree path and pinned HEAD; identity changes preserve the worktree. Owner markers are retained. |
| Origin-only bookmark records and Antigravity handoffs | Unique create-only names and canonical file locks preserve concurrent captures. A reader sees complete old/new bytes; interruption can leave an unreferenced private temporary. Reopen revalidates the transcript line and grants no launch or room authority. |
| Confirmation events and review outcomes | Confirmation labels append as one locked batch, separate from dispositions; a repeated run/provider refuses. Outcomes append after verdict publication. A kill can leave verdicts without an outcome (incremental discovery cannot use it), or a torn event/index tail that refuses further appends. Coverage scopes precede their index receipt; a kill can leave an unindexed scope. |
| Streamed external digest inputs | Read only. Concurrent generation/namespace drift refuses the digest. Process capture names isolate calls; a kill during capture publication follows the atomic-file rule. No file input is restored or modified. |
| View HTML and view metadata | A registry lock serializes publication of both files. HTML is published first. A kill between writes leaves an unregistered HTML file, which cannot authorize feedback import. Reconcile it before an explicit replacement. |

The execution reactor drains and reaps its owned child during normal cleanup.
Filesystem locks, process groups and post-execution comparisons do not confer
authority over concurrent owner work or prove whole-tree containment.

## Validation for this phase

```sh
cargo +1.99.1 check --manifest-path Cargo.toml -p oms-command-peer --all-targets --locked --offline
cargo +1.99.1 test -p oms-command-peer --all-targets --locked --offline
cargo +1.99.1 fmt --manifest-path Cargo.toml -p oms-command-peer -- --check
```

`tests/peer_contract.rs` covers ownership/help, bare-source view rendering,
installed overrides and refusal paths, ML prompt summaries, peer/delegate dry
runs and validation, review verdicts/risk classification, and unavailable runtime
services. It uses isolated subprocess environments and unique initialized Git
repositories; no provider executable or online service is needed.
The owner must still run canonical behavioral suites for full migration parity.

Current source owners include [thread](src/thread.rs), [review](src/review.rs),
[context packs](src/context_pack.rs), [session handoff](src/session.rs),
[ML context](src/ml_context.rs), and [peer requests](src/peer.rs).

Historical migration sources (paths from before native routing): `scripts/thread.sh`,
`scripts/lib/review_confirm.py`, `scripts/lib/review_since.py`,
`scripts/lib/red_check.py`, `scripts/lib/answer-quality.py`,
`scripts/lib/oms_runtime/context_pack.py`, `scripts/lib/oms_sessions/`,
`scripts/session-handoff.sh`, `scripts/agent-ml-context.sh`,
`scripts/detect-project-style.sh`, `scripts/lib/peer-common.sh`, and `path_scope.py`.
The retired Python tests are historical references. Current native fixtures and
the affected root integration checks supply behavioral evidence; platform, live
provider and release/install verification remain separate.

## Asset and dependency audit (W2)

The scoped audit of `delegate`, `ml_context`, `review`, `receipts`, `thread` and
`lib` found no Python command bridge. Delegation uses Git, provider executables
and Bash for selected verification commands. Its optional `scripts/check.sh`
fast-mode discovery remains a repository check contract. ML context reads Python
source as data and emits check hints; it never imports or executes that source.
Citation lookup uses curl. Review path globs are classification data, including
legacy script names; removing them would lower existing review tiers.
`plugins/*` includes plugin `hooks.json`; there is deliberately no blanket
`hooks.json` or `crates/oms*` risk glob. Native paths still match the existing
permission/policy/scanner name patterns and risky patch-content rules. Changing
the overall tier policy belongs to the owning migration task.

For Wave 3, the peer execution boundary is `process::Executor`, not a public
peer `Services` trait. `native::Services` implements read projections for runtime
and plan; it does not supply approval, landing, lifecycle writer or app delivery
authority. `thread append --room-event` currently passes room `LocalServices`;
operations-dependent room events must keep refusing until the owner wires the
native operations guard and managed repository resolver. Explicit plan/apply
requests remain fail-closed. No callback contract was widened in W2.

Asset lookup adds no durable writes. View HTML and metadata retain the registry
lock and publication order described above. The bundled files are immutable;
concurrent builds remain serialized and a kill between HTML and metadata still
leaves an unregistered HTML file. Contract-test fixtures are unique per PID and
atomic sequence and removed by non-panicking teardown; a killed test may leave
only its private temporary repository.
