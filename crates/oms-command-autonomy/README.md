# oms-command-autonomy

Native command adapter and orchestration domain for `intent`, `plan-from-spec`,
`plan-run`, `autopilot`, `goal-drive`, and the write-free `install-plan` helper.
This Rust 2024 crate has a Rust 1.99.1 floor. Root dispatcher integration
is outside this crate's write scope.

The library does not invoke a shell/Python implementation of an OMS command.
Acceptance and campaign verifier commands explicitly supplied by a contract
remain executable shell programs, run through native bounded supervision.

## Entry points and host wiring

`owns(tool: &str, args: &[OsString]) -> bool` classifies all six command families,
including malformed arguments and help, without filesystem access.

`run(tool, args, cwd, interrupted) -> (i32, Vec<u8>, Vec<u8>)` supplies the local
read-only projections and native supervisor adapter. Provider-dependent work
refuses when its required service is unavailable.

`run_with_services(tool, args, cwd, interrupted, services)` is the host adapter.
Arguments exclude the public verb. File arguments resolve against `cwd`;
source-file intake resolves against the selected repository. `Services` extends
[`oms-command-plan::Services`](../oms-command-plan/src/services.rs), retaining
that producer's approval, lifecycle, room, publication and acceptance interfaces.

| Hook | Required host behavior |
| --- | --- |
| `work_repository`, `state_repository` | Resolve physical execution and canonical state roots, including managed scratch checkouts. |
| `resolve_route` | Return the native registry's exact `ModelRoute` without a provider call. Its class, primary/fallback and effort fields bind unchanged-failure identity. |
| `invoke` | Dispatch the requested native OMS command and preserve exit/stdout/stderr. Honor the invocation's cwd, environment, wall and output bounds. Never fall back to a script backend. |
| `validate_context_pack` | Validate the typed graph orientation pack before any task claim. The pack grants no scope. |
| `provider_names` | Return canonical transport names for campaign manifest validation. Local defaults cover Codex, Claude and Antigravity. |
| `read_issue` | One bounded GitHub GET, without linked-content traversal. The local implementation uses `gh api` with a 20-second, 512-KiB bound. |
| `repair_feedback`, `revalidate_repair_feedback` | Native landing feedback observations and fresh exact-PR/head/run-attempt or comment-revision checks. Defaults refuse; evidence never grants task, provider or publication authority. |
| `discovery_section` | Supply native metrics, review and CI projections. Missing sections carry explicit incomplete coverage. |
| `observe_journal` | Forward durable goal/autopilot observations to the existing native Work Journal producer; observation failures do not replace canonical outcomes. |
| inherited plan services | Supply native approval records, producer locks/projections, acceptance execution, controller identity, and publication products where those plan operations require them. |

`services::invoke_binary` supports a host-supplied native binary prefix, usually
the current `oms` executable. It passes argv without a shell, propagates
interruptions, and enforces byte/deadline bounds. Its private whole-tree reactor
requires exclusive Linux child ownership. A multithreaded host, an existing
supervisor owner, or a macOS host must inject its supported native execution
service rather than silently degrading containment. `LocalServices::new` borrows
the caller's interruption flag.

The invocation producer supplies canonical state, goal run/cycle and autopilot
owner labels as child environment entries. Role timeouts also reach provider
commands through their existing timeout environment interface. Services are
trusted host integrations; no CLI argument selects an alternate backend.

## Modules and authority

| Module | Responsibility |
| --- | --- |
| `cli`, `lib` | Strict option parsing, public family routing, buffered output and exit codes. |
| `value`, `storage` | Duplicate-key refusal, native canonical JSON/digests, descriptor-relative reads, CAS writes, locks and durable JSONL through existing crates. |
| `contract` | Shared adoption/planning acceptance parser, required verifier inputs, scope defaults, stable criteria, section budgets and Roadmap milestones. |
| `intent` | Pinned file/issue source intake; draft/show/adopt/amend; candidate snapshots, pre-passing-check refusal, predecessor archives and amendment recovery. |
| `planning` | Provider/council decomposition, proposal validation, scope/dependency checks, advisory waves/size and native atomic proposal apply. |
| `discovery` | Bounded evidence packets and candidate filtering. Speculative, duplicate, rejected-hypothesis and unresolved findings cannot become tasks. Current tracked source citations are checked without executing target code. |
| `selectors`, `verify` | Static suite selectors, verifier-base preconditions, existing native floor lint, acceptance reuse identity and receipt freshness. |
| `runner` | One task, reviewed assignment, exact lease, failure-ledger preflight, isolated delegation, explicit patch landing and one durable same-lease repair. |
| `goal` | Fresh acceptance, vacuity/stall checks, lifetime cycle reservations, serial landing and exact-tree Git publication with restartable commit intents. |
| `receipt` | Schema-1 frozen autopilot receipts, exact proposal approval, parked/exhausted refusal, reentry/shadow evidence, one durable remainder reservation, semantic review and optional Draft PR producer calls. |
| `campaign` | Digest-bound campaign manifests, locked lifetime reservations/settlements, bounded candidate artifacts, isolated verification, consolidation and double-verified handoff evidence. Execution is serial within the declared concurrency ceilings. |
| `campaign_usage` | Optional soft token/cost admission against canonical provider receipts, historical settlement and retained unknown liabilities. |
| `advisory`, `handoff`, `conflicts` | Lifecycle overlays, stall/repair projections, baseline and allocation arithmetic, mechanical handoffs, pickup markers and read-only pairwise conflict previews. These observations grant no authority. |
| `install` | Preview calls to the owning native installation producers; unavailable previews print `not-previewed`. No install or provider-registration command is run. |

Completion, approval, cancellation, proposal SHA/CAS enforcement, acceptance
receipts and retirement remain owned by `oms-command-plan::Plan::execute`.
The crate reexports native completion/decision/retirement APIs, task packet and
consumer APIs, task verification observations/fingerprints, and work-inspection
projections instead of creating alternate authorities for those records.

## Campaign usage admission

Campaigns may add `usage` inside the existing immutable manifest `budget`:

```json
{
  "max_tokens": 100000,
  "max_cost_microusd": 5000000,
  "per_call": { "tokens": 20000, "cost_microusd": 1000000 }
}
```

At least one total cap is required; each enabled cap requires its matching
positive per-call reservation no larger than the total. Omitting `usage`
preserves the existing lifetime call-count, concurrency and deadline behavior.
These are soft admission limits. They do not enforce a provider hard spending
cap or predict prices, and an admitted call can exceed its reservation.

The campaign ledger lock spans usage projection, admission and reservation.
Provider calls carry the reservation as `OMS_OPERATION_ID` and the campaign
digest as `OMS_GOAL_ID`. Each admission counts settled usage from all previous
attempts, active reservations and unresolved liabilities. Each unknown
physical call in an account retry or fallback retains its own reservation;
measured spend above a reservation is also retained. Once every call supplies
final measured evidence, its exact reported amount replaces the reservation.
A timeout, crash, abandoned controller or unpriced/subscription cost never
refunds a liability. Only a pre-invocation refusal with no calls proves no
provider spend. Later canonical receipt revisions are reconciled on the next
admission or status read; conflicting cumulative observations block admission.
The same projection includes physical intents whose usage receipt was never
acknowledged. They retain their per-call liability after settlement or refusal
and appear separately as `intent_only_calls`; matching receipts are counted once.
These file observations do not provide a serialized physical-call spending cap
or canonical owner-generation proof.

`goal-drive --campaign FILE --status` includes `spent.usage`, with measured
amounts and report coverage, historical settled amounts, active and unresolved
reservations, admission commitments and provider estimate counts. Missing
measured cost is `null`. Client-reported costs may be estimates and are not
authoritative billing. The existing provider hard-budget refusal remains
unchanged.

## Durable effects and interruption

Contract, plan and orchestration mutations require the native parent guard.
Advisory handoff pickup records confer no authority. Records and source paths are
read without following symlinks; existing ownership and permission checks remain
in the shared stores. Proposal apply requires the exact lowercase SHA-256 before
any apply, followed by the plan producer's locked digest/CAS check.

| Written paths or objects | Concurrent call and interruption behavior |
| --- | --- |
| `.oms/intents/*.md`, provenance and amendment bindings | Unique immutable publication prevents clobbering. A stop between paired files can leave an orphan candidate/binding; it grants no adoption or execution authority. |
| `PROJECT.md`, intent archives, publication intents, `amendments.jsonl` | Receipt/intake locks and exact predecessor bytes fence publication. Archive and publication intent precede replacement. A stop after replacement can be reconciled from the exact publication intent without repeating acceptance or losing the predecessor. A partial ledger append refuses mutation until reconciled. |
| Proposal, discovery and candidate-report files | Unique immutable artifacts are published only after validation. Generated proposals never apply themselves; missing review authority refuses. |
| `.oms/plan/tasks.json` and native plan records | The native plan producer owns locks, CAS and replay. A failed worker cleanup releases only this call's exact active lease. Review/repair evidence and another claimant's lease are preserved. |
| `progress.jsonl`, frozen commit patches, Git objects/ref/index | Attempts are recorded before calls. Frozen patches precede commit intents. A standard exclusive `index.lock` fences immutable-tree publication through expected-old ref CAS and exact index synchronization. A stop after ref publication retains an open intent for parent/tree reconciliation. A hard kill can leave Git's lock: it is never stolen automatically. Unreferenced objects and orphan immutable patches confer no publication authority. |
| `autopilot-run.json`, archives, abandonment/approval/reentry/shadow/replan records | A foreground controller lock serializes native controllers, with exact receipt-byte CAS. Frozen fields cannot change. Archive/retirement evidence precedes removal. Parked receipts cannot silently resume; cycle and sole-remainder reservations survive restart. Reentry uses native dead-owner lease recovery. |
| Campaign manifest, `events.jsonl`, artifacts and handoff | Manifest bytes bind the directory digest. A controller lock and ledger lock serialize mutations. Reservations remain spent after a kill; unsettled reservations become abandoned under the next controller, never refunded. Candidate/handoff publication is immutable. Solved/stopped ledgers do not restart. |
| Temporary indices, merge object directories and candidate checkouts | Private indices/merge objects are removed on ordinary exit. Hard-kill leftovers grant no ref/index authority. Managed candidate checkout creation/removal belongs to the injected native scratch producer and its ownership records. |
| `.oms/handoffs/consumed.jsonl` | Advisory pickup records use the shared durable writer under a file lock. Distinct sessions are counted by projection. No pickup hides a handoff or grants resume authority. |
| Work Journal | The existing producer owns its files, deduplication and locks. Observation failure does not undo a completed canonical operation. |
| `.oms/feedback/repairs.jsonl`, `repair-evidence/*.json` | A durable one-attempt claim and bounded untrusted packet precede Repair and dispatch. Event, plan/task, lease, route, head and failure signature bind the claim. Restart never refunds claims or resends an unknown acknowledgement. |

## Opt-in repair from external feedback

The landing observer automatically ingests PR/CI observations through the
existing `ci-status record|tick` entries. It starts no repair. The parent reads
`ci-status feedback --json`, includes exactly one whitespace-delimited
`feedback:<event_id>` token in an existing task's reviewed title, and retains
that task's allowed paths, forbidden paths and mechanical verifier. The title
is part of the native proposal and rendered task brief; external comment text
cannot add this selector or amend the task.

An already reviewed task can consume that event only when its parent invokes
`plan-run --to PROVIDER --id TASK --land --auto-repair`. The consumer checks
the current task digest, scope, live review lease, cancellation and one-repair
cap. It reads fresh GitHub PR head and source revision before claiming work,
again before the existing `plan repair` transition, and immediately before
`peer-delegate`. The delegate receives a bounded packet marked untrusted through
the existing review-artifact boundary, with no repair loop of its own. The
result returns through the existing `patch-land` admission front door. None of
these steps commits, pushes, merges, posts or resolves comments, or increases
publication authority.

Only exact-head comments within the reviewed scope and fresh CI regressions
qualify. CI requires complete exact-attempt job evidence and successful matching
jobs at the PR's current base SHA. Missing comparisons, mixed results, base
failures, cancellation, environment failures, moved heads and edited comments
stop. A repeated failed-check/step signature or spent plan-task cap also stops,
including after restart. A lost provider acknowledgement stays unknown and is
never automatically resent; ordinary native task/worker evidence must be
reconciled before further work. After an acknowledged repair whose admission
is held, its reviewed patch can still use the existing explicit admission path.

Completed tasks stay completed. Feedback on a completed/published PR requires a
separately reviewed task under current authority; the observer never reopens a
done task or creates a repair task from an external instruction.

## Static selectors and installation previews

Selector validation reads suite definitions as data. When
`tests/operator/runner.py` is present, its static `SECTIONS` manifest retains
its existing section and case behavior, including refusal of shell cases.
Only an absent registry enables the shell fallback: inline
`# OPERATOR_SECTION_BEGIN` sections and `tests/operator/*.sh` section files
supply names, with underscores normalized to hyphens. Shell case names come
from `# BEGIN ` markers, removing sentence and parenthetical annotations.
Unknown sections and cases refuse; unsafe or non-UTF-8 registry/case files
remain errors. No target suite or Python interpreter is executed.

`install-plan --install` and `--update` retain the `STEP: STATUS: detail`
format. The `native-runtime` step reports readiness as `not-previewed`;
artifact availability belongs to the native installation producer. The preview
neither downloads an interpreter nor writes installation state.

## Verification

The integration suite covers ownership/help and parse errors for all six
families, interruption, selectors with and without the legacy registry,
continuation digest binding, install/update previews and native callback argv,
contract parsing, proposal JSON/digests, frozen receipt validation and budgets,
and unavailable service refusal. Run it with:

```text
cargo +1.99.1 test --manifest-path Cargo.toml -p oms-command-autonomy --test autonomy_contract --locked --offline
cargo +1.99.1 fmt --manifest-path Cargo.toml -p oms-command-autonomy -- --check
cargo +1.99.1 clippy --manifest-path Cargo.toml -p oms-command-autonomy --test autonomy_contract --locked --offline -- -D warnings
```

These commands run from the repository root. Cross-provider execution, native
macOS supervision and end-to-end host integration are not established by these
fixture tests. The owning host must inject native services before selecting
this family for live orchestration. In particular, `Services` extends the
plan producer's interfaces; the default unavailable callbacks grant no provider,
approval, publication, routing, context-validation or controller authority.
