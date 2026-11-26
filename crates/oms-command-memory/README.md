# Native agent memory

This domain crate implements the `oms agent-memory` command in the root Rust
workspace. The [root CLI](../oms/src/main.rs) routes it through `run_with_source`;
it does not register a second PATH command or invoke an interpreter or shell backend.
`source_root` is supplied by the validated parent adapter; records cannot select
an executable installation. Git is the only subprocess, for repository identity,
committed citations, and provenance.

## Entry points and consumers

- `owns` classifies every invocation, including invalid arguments.
- `run_with_source` implements the public command with captured stdout, stderr,
  and exit status. Its source is `scripts/agent-memory.sh`.
- `run_database` replaces direct `agent-memory-db.py` calls, including `sync`,
  `validate`, raw `context --view full|pins|summary`, and ranked recall with
  `--scoped drop`, `--target-path`, and optional `--handoffs`.
- `run_review` replaces the review helper's `import`, `review`, and private
  `capture` routes. Capture is not added to the public command grammar.
- `append_project_note` preserves the caller's kind for task-close and journal
  producers. Opt-in task-close proposals use `task_close`; journal
  distillation also remains recognizable by its journal author and body prefix.
- `append_journal_batch` publishes at most five journal proposals into project
  memory. Candidate/source/payload identities share the source publication;
  retries retain the original event IDs, including retired and redacted records.
  Its `distill` notes still require the existing review acceptance.
- `provider_context` composes global and project memory, compact/full/relevant
  modes, query-ranked recall, path scopes, screening, and existing byte budgets.
  It replaces `ma_write_shared_memory_context` used by peer context assembly.
- `resume_context` selects up to three eligible pins and three objective-ranked
  notes before dividing its 1200-byte budget between them. It omits unrelated
  recent summaries, uses a transient in-memory index, and leaves persistent
  indexes and access receipts untouched. Project recall precedes global fallback;
  review, retirement, citation, path-scope and child restrictions still apply,
  and each source degrades independently when oversized, unsafe or invalid.
- `pending_review` projects the existing project review queue as a count,
  skipped invalid-row count, and candidate fingerprints, without copying note
  text or making a decision. Its no-follow reads cap each source at 4 MiB and
  the queue at 4096 candidates; unsafe or oversized sources return unavailable,
  never zero pending, while invalid receipt rows are skipped and counted.

Automatic task-close, distill and imported notes wait for `review --accept`
before compact summaries or provider context can include them. The same rule
applies to full context, query-ranked injection and legacy journal-distill
notes. Explicitly authored notes and pins keep their existing eligibility;
`show`, `search` and `recall` retain stored proposals for inspection. Acceptance
does not verify a claim, and retirement/supersession still remove accepted notes
from context. A correction copied before an interrupted acceptance also waits
for its acceptance receipt. Review reads the existing receipt log; no notes
are deleted or new database schema introduced.

Current consumers include peer consultation/delegation through `provider_context`,
resume hooks through `resume_context`, state/inbox and prompt attention through
`pending_review`, the task service through `append_project_note`, and both the
root journal adapter and prompt hook through `append_journal_batch`. They are wired in [peer](../oms-command-peer/src/peer.rs),
[delegate](../oms-command-peer/src/delegate.rs), [task services](../oms/src/services_control.rs)
and the root CLI. Script/helper names in the entry-point descriptions are
historical porting anchors, not current runtime backends or missing router work.

## Public mode checklist

| Modes | Native implementation |
| --- | --- |
| `path`, `db-path`, `show` | Lazy resolution and source reads without creating state |
| `init`, `append`, `pin` | Owned source records, attribution, screening, citations, path scopes, summary and index refresh |
| `supersede`, `retire` | Validated active target and complete append-only link record |
| `redact` | Parent-only dry run or fixed tombstone, stable ID, unchanged neighboring bytes, derived-copy invalidation and rebuild |
| `context`, `compact` | Active pins and uncited/unscoped summaries; full debug view; sensitive sections withheld |
| `search`, `recall` | Schema-three SQLite, substring/full Unicode folding, FTS5 BM25, fallback ranking, access decay, citations and stale/superseded filters |
| `recall --all-repos` | Read-only registered indexes, per-repository limits, bounded hit report, no recall receipts |
| `health`, `rebuild` | Read-only source/index diagnostics; derived SQLite regeneration |
| `sources` | Provider-native instruction/memory locations, stat-only sizes/counts/times |
| `import` | Candidate bounds, source labels and hashes, duplicate/withheld status, explicit selection and append-only imported notes |
| `review` | Pending automatic notes/corrections; accept, retire, or reject; append-only decision receipts |
| `promote` | Parent-only project-to-global dry run/apply; portable rescan, provenance marker and retry deduplication |
| `graduate` | AGENTS.md/PROJECT.md diff preview; never edits the target document |
| `hygiene` | Read-only cold-note receipts, instruction duplication and provider index-cap report |

Project, global, and explicit memory sources retain their established selection
rules. Filesystem paths remain OsString/PathBuf values: path/status text retains
Unix bytes, source snapshots bind their exact byte spelling as SQLite TEXT,
and source inventories/import labels/citation provenance use lossless JSON
surrogate escapes for non-UTF-8 filename bytes. Textual diagnostics use explicit
byte escapes rather than replacement characters. Python-helper-only options remain separate from the ordinary shell
parser's positional text rules. Unknown metadata is not interpreted as write
or executable authority. SQLite remains schema three; citation/path metadata
stays in the source log. No new authoritative database is introduced.

## Writes, concurrency, and interruption

All source, receipt, and derived-file mutations use the existing `oms-lock`
protocol. File replacement uses `oms-safe-fs` with same-directory publication,
file synchronization and directory synchronization. Owned single-link regular
files and no-follow reads protect source leaves. `oms-scrub` supplies the existing
sensitive-content policies. Neighboring source bytes survive explicit redaction,
including invalid UTF-8. Full folding expansions use a local Unicode table;
contradiction normalization reuses the workspace's pinned Unicode normalizer.

- `shared.md` and `pins.md`: two appends serialize, preserving the old bytes as
  a prefix. A killed copy-on-write operation publishes either the old source or
  a whole new record. Metadata is captured before the source lock. Supersession
  and retirement validate their target while holding that lock.
- Journal batches prepare provenance before the source lock, reconcile keys and
  append the accepted records in one atomic source publication. The journal's
  existing `distill.json` fixes the batch before that call. A changed source can
  reconcile a prior append but cannot publish an obsolete proposal. Conflicting
  keys and individual refusals do not become completed candidates. One summary
  and index refresh follows publication, with warnings returned separately from
  durable outcomes; replay also repairs interrupted derived refreshes.
- `summary.md`: writers serialize on the summary lock. It is derived, and a
  source append can survive with a missing/stale summary; the command warns.
  Redaction locks both logs and the derived outputs, invalidates derived copies
  before source replacement, then regenerates them.
- SQLite and journals: command writers serialize on the database lock; source
  replacement is one SQLite transaction. Access receipts are best effort.
  Rebuild constructs a private temporary database and publishes only after an
  integrity check. Explicit rebuild starts fresh access statistics. A failed
  refresh leaves the Markdown record authoritative.
- `reviews.jsonl`: decision eligibility, optional correction append, and receipt
  share the decision lock. A second decision refuses an already decided item.
  Decisions refresh the derived summary; context also checks current receipts
  when reading, so a stale summary cannot bypass review.
  A kill between a correction note and its receipt leaves the note's
  `accepted_correction` metadata for retry deduplication; retirement itself
  removes a note from the pending set.
- `pending.jsonl`: capture locks and rechecks IDs and the pending cap before
  appending. A second capture keeps no duplicate. Capture remains fail-open.
- Global promotion: the marker check and copy share the global source lock.
  A second application reports already promoted; a kill after source publication
  can leave derived outputs stale, and a retry does not copy the note twice.
- Import: each selected note is an individual durable append. A kill can leave
  a selected prefix imported. A later import recognizes those lines as
  duplicates; simultaneously selected independent imports can each append the
  same candidate, matching the existing per-note import transaction boundary.
- `.oms/.gitignore`: initialization serializes and preserves an existing file,
  including an empty one. A killed creation leaves it absent or complete.

A hard kill can leave private same-directory temporary files or lock-protocol
recovery state. Multi-file operations are not an atomic filesystem transaction;
redaction can leave a tombstone with missing derived outputs and must be retried.
No unrelated file, document, ref, or registry is written. Read-only health,
hygiene, sources, graduation, import preview, redaction preview and all-repository
recall create no OMS record or synchronization receipt.

## Evidence and verification boundary

The following records describe the original isolated migration phase. They do
not establish fresh CI, installation or live-provider results for the current
revision; the native source wiring above is a separate claim.

The implementation was compared with the full public parser and the database,
review, promotion, hygiene, sources, contradiction, and handoff helpers. Relevant
common-library ranges cover source selection, scanner contracts, normalization,
metadata, source citations, source writes, redaction and provider context.
Unrelated common-library verification-hint and scanner-report presentation
helpers were outside this command port. Existing read-only behavior evidence
includes the memory sections of `tests/scripts-smoke.sh` and
`test_memory_redaction_and_native_sources` / memory hygiene in
`tests/functional-evolution-smoke.sh`.

Rust 1.99.1 formatting and the supplied locked offline cargo check ran in this
isolated worktree. Disposable local command probes exercised normal mode paths,
redaction/promotion retry, correction capture and acceptance, retirement,
malformed limits, child refusals, byte preservation, and stale-summary warnings.
Their launcher and fixture files are removed from the delivered source. The final
filename-byte follow-up received formatting and compile checking only, per the
owner's minimum-verification instruction; the earlier probes predate that follow-up.

Canonical regression tests, red/green acceptance, full boundary/fault-injection
coverage, provider integration, macOS/WSL execution, CI, installation, and backend
retirement remain deferred to the owner. Compilation and these small probes are
not claims of global acceptance, measured HPC performance, or completed rollout.
