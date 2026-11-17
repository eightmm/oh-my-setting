# oms-command-peer

Native peer command migration with the W5b integrations described below.
This crate compiles independently but is **not yet a complete compatibility replacement**.
Do not wire it into production routing until the remaining contracts below
have been implemented and the canonical behavior suites have run.

The standalone workspace uses Rust 1.99.1, edition 2024, resolver 3 and forbids
unsafe code. Dependencies resolve through the root workspace lockfile; no crate-local
lockfile or new dependency is needed for the embedded view assets.

## Entrypoints

- `owns(tool, args)` classifies the fifteen command names without IO, including
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
| `review` | Criterion coverage, singleton confirmation, incremental review outcomes, finding anchors/dispositions, pinned verification scopes and advisory security pass |
| `review_red` | Advisory bare-base control and bounded individual test-patch checks in disposable worktrees |
| `native` | Runtime criterion projections through native task status, verified plan snapshots, room projection and lifecycle projection |
| `context_pack` | Bounded supplied graph-pack validation and reference-only orientation |
| `ml_context` | Sanitized ML project fields, manifest identifiers, entrypoint names, verification hints and recent experiment summaries |
| `thread` | Thread creation, typed Room append, context, schema-1 listing/statistics, stale-close preview/apply, current pointer, cursors, updates and consumption receipts |
| `session` | Repository-bound transcript discovery/search/open, origin-only bookmarks/reopen, guarded resume planning and prompts-only Antigravity handoff capture |
| `digest` | Streamed file digests, capture redaction, retrieval descriptors, collision-resistant capture names and command exit propagation |
| `receipts` | External receipts, hash drift, DOI/arXiv normalization, offline/online verification and BibTeX export |
| `manifest` | CSV/TSV/line IDs, hashes, key mappings, provenance, drift, ID/key overlap and ordered time checks |
| `board` | Claims, stale recovery, heartbeat, finish/abort, owner-preserving replay and tag validation |
| `view` | Local result normalization, offline assets, capsules, source-bound metadata and idempotent feedback imports |

Path-scope normalization and bounded glob matching reuse the existing
`oms-contract-markdown` port of `path_scope.py`.

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
Registration failure prevents that physical invocation. Completion-persistence
failure stops automatic account overflow and model fallback, even when provider
output is already available. Ordinary and budget-enforced calls share this
strict contract so an untracked retry cannot disappear from budget admission.

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

Optional view overrides are read from `scripts/lib/artifact_view_assets/`:
`analysis.js`, `view.js`, `view.css` and `view.html`. Each absent file falls back
independently to its exact bundled copy in `assets/`, embedded with `include_str!`.
Existing empty files remain overrides. Invalid UTF-8, symlinks, unsafe ancestors
and other read errors refuse rendering instead of silently selecting a fallback.
The source root itself must still be a valid directory.

Role resolution retains repository and global overrides through the existing
role crate. Account configuration is read from the existing user configuration
location, independently of the trusted source root. Model caches and bounded
session logs supply account model eligibility and reported quota. No account
authentication contents are copied into artifacts. Native transcript roots
are independently checked and bound by provider metadata to the repository.

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

## Remaining compatibility work and integration blockers

W5b is incomplete. The owner must resolve these concrete dependency boundaries:

1. Existing `oms-output/Cargo.toml` still enables the JSON precision feature
   prohibited by this brief. Runtime itself no longer enables it. No such feature
   was added here, but a zero-feature dependency closure requires an upstream
   repair outside this crate; preserving native digest reuse cannot fix that
   manifest from the allowed directory.
2. Canonical admission/checkpoint APIs live in `oms-command-landing`, whose
   `oms-reference` dependency requires `proc_pidinfo` 0.1.4. That package is absent
   from the permitted root lockfile. Importing that dependency would violate the
   external-package constraint. No alternate admission implementation is invented.
3. The public peer executor returns buffered process results and has no terminal
   control-input or streamed durable sink interface. Interactive delegation and
   complete streamed command capture need the owner's interface decision.

The remaining behavior is still refused or incomplete:

- Plan claim/start/touch/review/release, observed panel bindings, resumed repair,
  candidate-patch admission and automatic apply need authenticated marker,
  shared-state, lease and landing producer wiring. `patch-admit`, `patch-land`,
  `candidate`, `change-guard` and `draft-pr` remain owned by the landing crate;
  the peer command ownership set is unchanged.
- Automatic `--graph-context`/`--context-manifest` compilation is not wired to
  `oms-command-graph`. Only the supplied typed pack path above is implemented.
- Session checkpoint/fork/launch/compare/file restore, managed panel relocation,
  full status projection and complete handoff-prompt synthesis need the native
  checkpoint/panel interfaces. Resume launch serialization/receipts are not yet
  complete. Prompts-only Antigravity capture is bounded by the general 64 MiB
  reader and is not a streaming history implementation.
- Remote artifact views still refuse. Legacy `artifact_view.py::ssh_read` uses
  SSH plus a remote no-follow reader; the requested private curl-config transport
  is a different interface. Neither transport may silently lose remote no-follow
  validation. View embedding still needs frozen fixture comparison.
- Command digest capture remains bounded in memory, and retention is not complete.
  General acquisition remains 64 MiB, threads 16 MiB and native transcripts
  32 MiB; supplied packs and model configuration have their tighter bounds above.
- Dispatch-anchor fallback for account counts, complete config warning/fallback
  behavior, routing history/statistics, `telemetry_waste`, recorded search,
  prompt/instruction audit, job-digest/experiment attachment, export/import receipts,
  semantic deliberation enforcement, until-agreement stop reports, advice
  triggers, task observation and persistent failure recovery remain incomplete.

The canonical behavioral suites, failure/edge cases, adversarial namespace
cases and cross-provider fixtures were not run under the implementation-first
brief. No production selector, commit, push or install was performed.

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
cargo +1.99.1 check --manifest-path Cargo.toml -p oms-command-peer --all-targets --offline
cargo +1.99.1 fmt --manifest-path Cargo.toml -p oms-command-peer -- --check
```

`tests/peer_contract.rs` covers ownership/help, bare-source view rendering,
installed overrides and refusal paths, ML prompt summaries, peer/delegate dry
runs and validation, review verdicts/risk classification, and unavailable runtime
services. It uses isolated subprocess environments and unique initialized Git
repositories; no provider executable or online service is needed.
The owner must still run canonical behavioral suites for full migration parity.

Behavior sources inspected: `scripts/thread.sh`,
`scripts/lib/review_confirm.py`, `scripts/lib/review_since.py`,
`scripts/lib/red_check.py`, `scripts/lib/answer-quality.py`,
`scripts/lib/oms_runtime/context_pack.py`, `scripts/lib/oms_sessions/`,
`scripts/session-handoff.sh`, `scripts/agent-ml-context.sh`,
`scripts/detect-project-style.sh`, and `scripts/lib/peer-common.sh`.
Existing `tests/test_sessions.py`, `tests/test_oms_runtime_context_pack.py`,
`tests/test_artifact_view.py`, operator suites and shell smoke coverage remain
for the owner's behavioral validation phase.

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
