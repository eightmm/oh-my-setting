# oms-command-peer

Implementation snapshot for the native peer command migration. This crate
compiles independently but is **not yet a complete compatibility replacement**.
Do not wire it into production routing until the remaining contracts below
have been implemented and the canonical behavior suites have run.

The standalone workspace uses Rust 1.99.1, edition 2024, resolver 3 and forbids
unsafe code. No parent manifest, parent lockfile, dispatcher or other crate is
changed. The local lockfile records the offline dependency resolution.

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
| `peer` | `peer-ask`, `agent-call`, `consult`, `advise`, prompt artifacts, threads, debate and synthesis |
| `delegate` | Detached worktrees, role defaults, patch capture before verification, repair and lifecycle review state |
| `review` | `peer-review`, verdict collection, finding anchors/dispositions, pinned verification scopes, advisory security pass and review-depth advice |
| `thread` | Thread creation, append, context, show/list/stats, current pointer, compatible cursors, updates and consumption receipts |
| `session` | Bounded repository-bound transcript discovery/search/open, bookmarks, native resume planning and handoff capture/list/show/prompt |
| `digest` | `oms-output` construction, capture redaction, retrieval descriptor and command exit propagation |
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

## Installation data read

The trusted source root supplies:

- `config/panel-routing.json`
- `config/role-contracts.json`
- `config/review-depth.json`
- `roles/*.md`, through `oms-command-agent-role` resolution
- `scripts/lib/artifact_view_assets/analysis.js`
- `scripts/lib/artifact_view_assets/view.js`
- `scripts/lib/artifact_view_assets/view.css`
- `scripts/lib/artifact_view_assets/view.html`

Role resolution retains repository and global overrides through the existing
role crate. Account configuration is read from the existing user configuration
location, independently of the trusted source root. Model caches and bounded
session logs supply account model eligibility and reported quota. No account
authentication contents are copied into artifacts. Native transcript roots
are independently checked and bound by provider metadata to the repository.

The files in `src/help/` preserve the public shell help as migration reference
text. They describe the target contract, including options which this snapshot
still refuses. Their presence is not evidence that every branch is implemented.

## Remaining compatibility work and integration blockers

The following are incomplete, even though the command names are owned:

- Plan-lease observation/transitions, automatic patch admission/application,
  resumed sessions, interactive delegation and typed graph/context compilation
  refuse before starting a worker. These need the owner's native integrations;
  this crate cannot change the owning crates or their public contracts.
- `peer-review --covers`, candidate-patch admission, `--confirm` and advisory
  red-check refuse. Criterion coverage needs the current native envelope. The
  existing runtime crate enables the JSON precision feature forbidden by this
  migration brief, so it is not added as a dependency.
- `--since-last`, complete export/import receipts, semantic deliberation
  enforcement, until-agreement stop reports, advice triggers, task observation,
  persistent failure recovery and all provider usage/routing-history helpers
  are not fully ported. ML digest attachment refuses.
- The account selector's pure API supports quota ordering and spread tie
  breaking; native live-running counts still need producer integration.
  Configuration failures currently refuse rather than adopting every legacy
  warning/fallback behavior. The requested historical `.codex-credits` fallback
  was not found in the inspected tracked source and is not invented here.
- Session file-state fork/launch/compare/restore and stored bookmark reopen
  refuse. Native resume plans do not yet include managed-panel relocation or
  interactive terminal handoff. Antigravity handoffs refuse. The full session
  status projection still needs migration; handoff listings read pickup history.
- Remote artifact views refuse pending a bounded native remote reader. View
  normalization/embedding needs fixture parity with the original renderer.
- `thread close --stale` refuses; statistics/listing and full room-event append
  compatibility remain incomplete. Typed room events must use the existing
  room adapter instead of bypassing its validation through plain thread append.
- Digest retention and streaming capture beyond the current memory bound are
  incomplete. General file acquisition is bounded to 64 MiB, thread logs to
  16 MiB and native transcripts to 32 MiB; large dataset streaming needs work.
- `telemetry_waste`, recorded search, the complete prompt/instruction audit,
  routing statistics and all remaining result-lint cases still need migration.

Behavior parity, cross-provider guards and all failure/edge cases remain
unverified. Canonical tests were intentionally not run under the
implementation-first brief. This snapshot must not be described as the full
native implementation requested by the owner.

## Concurrent calls and interrupted publication

All state mutations use canonical `oms-lock` derivation. Locks serialize a
second writer, which re-reads the current body after acquiring the lock.
Interruption is checked while waiting and before publication.

| Written files or records | Second call and kill behavior |
| --- | --- |
| Thread logs, experiments, external/literature receipts, artifact index, review findings/events and view feedback | Locked stable-inode append. A second call observes the preceding record. A kill during append may leave an incomplete tail; subsequent writes refuse it until reconciliation. Acknowledgments and feedback imports deduplicate their complete records. |
| `CURRENT`, prompt/output artifacts, patches, manifest JSON, scopes/verdicts, bookmarks and handoffs | Locked atomic replacement. A reader sees old or complete new bytes. Create-only writes refuse an existing file, including an empty file. A kill can leave a private temporary file; a publication/durability error requires reconciliation before retry. Manifests and the current pointer deliberately allow a later replacement. |
| Delegation lifecycle events and artifact index | Existing native operations own lifecycle append. Patch bytes are published before verification and final output. A kill between these writes can leave a review-state attempt without its final artifact/index row; admission stays pending and the owner must reconcile it. No child report grants admission or publication. |
| Worktree owner marker and Git registration | Unique per-call names isolate concurrent calls; Git owns its metadata locking. A kill may leave a marker, partial registration or detached worktree. Cleanup requires the original worktree path and pinned HEAD; identity changes preserve the worktree. Owner markers are retained. |
| View HTML and view metadata | A registry lock serializes publication of both files. HTML is published first. A kill between writes leaves an unregistered HTML file, which cannot authorize feedback import. Reconcile it before an explicit replacement. |

The execution reactor drains and reaps its owned child during normal cleanup.
Filesystem locks, process groups and post-execution comparisons do not confer
authority over concurrent owner work or prove whole-tree containment.

## Validation for this phase

```sh
cargo +1.99.1 check --manifest-path rust/crates/oms-command-peer/Cargo.toml --all-targets --offline
cargo +1.99.1 fmt --manifest-path rust/crates/oms-command-peer/Cargo.toml -- --check
```

No tests are added or changed in this implementation-first snapshot. The
test-reversion protocol therefore has no new/changed test to exercise. The
owner must run the canonical behavioral suites after closing the gaps above.
