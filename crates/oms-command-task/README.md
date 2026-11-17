# Native task command

This standalone crate implements the active `agent-task` packet over the
existing `.oms/task/current.md` and adjacent `archive/` records. The parent
registers it in the workspace and CLI; this crate does not install itself.
There is no Python or Bash implementation bridge. Bash is invoked only for
the user's stored Verify command, with its declared `bash -c` semantics.

## Public command and internal consumers

`owns`, `usage`, `run_with_services`, and `run_with_io` cover `path`, `init`,
`show`, `context`, `status`, `update`, `append`, `verify`, `rotate`, and `close`.
`src/usage.txt` retains every option in `scripts/agent-task.sh`'s usage,
including status JSON, stdin append, source-session hashes, all loop fields,
goal-replacement consent and verification skip reasons. `run` is a read-only
convenience: mutation requires the real injected services.

The implementation follows `scripts/agent-task.sh` and
`scripts/lib/agent-task-common.sh`; durable writers use the exact level-two
section grammar while preserving unknown packet bytes and section ordering.
`packet` exposes the pure metadata, section, append, pruning, receipt, stale
and verification-contract operations for native callers.

The internal adapters have these existing consumers:

| Native API | Existing consumer being replaced |
| --- | --- |
| `projection::task_packet` | `oms_runtime.projection._task_packet`, runtime envelope and context |
| `lifecycle::lifecycle` | `goal_lifecycle.py`, task status, goal-drive lifecycle overlay |
| `assignment::{validate,resolve,fields,validate_collaboration,validate_collaboration_plan}` | task-assignment.py; model-routing, agent-plan, goal-drive, autopilot, plan-from-spec |
| `consumers::loop_warnings` | peer-common's peer context and budget reminders |
| `consumers::record_outcome` | agent-call and peer-delegate outcome notes |
| `consumers::{parent_guard,reference_root}` | internal task writers and file-only reference ownership |
| `observe::{scan_text,scan,version,fingerprint,lock_digest,note_lines,verify_output}` | verify_observe.py output recognizers, native lock/environment fingerprints and receipt extraction |

Projection inputs must be fresh bounded canonical reads and physical paths.
`oms-contract-markdown` supplies projection parsing and scope extraction;
`oms-work-inspect` validates the canonical status snapshot. Original JSON
number spellings, unknown object fields and WTF-8 strings are retained by the
JSON adapters. Assignment validation rejects controls and transport aliases;
an explicit assignment overrides all route defaults, including empty models.
No adapter authorizes acceptance, provider launches or task execution.

## Parent service contracts

`services::Services` is an explicit in-process dependency, supplied to
`run_with_io` by the parent CLI. Its methods return the crate's typed `Result`.
No implementation in this crate duplicates another domain's writers.

| Method | Arguments and required producer/result |
| --- | --- |
| `state_repository` | Fresh physical work root and interruption flag; return the registered canonical state root using native scratch-worktree validation. |
| `begin_reference` | State root and actual packet path; start the real local `agent-task` operation and return its owned `ReferenceOperation`. |
| `ReferenceOperation::validate` | Under the canonical packet lock: operation name, original argument bytes, target, before/proposed packet bytes and source-session hash. Validate live managed-reference spellings, capture ownership/indexes and evidence namespaces before any publication. |
| `ReferenceOperation::finish` | Actual exit status after releasing the packet lock. Preserve terminal no-write refusals. A failed or interrupted operation must remain unresolved; Drop must stop/reap only its owned monitor generation. |
| `observe_journal` | `JournalObservation` with repo, actual source path and typed update/verify/close operation; only close supplies passed/not-verified from pre-archive freshness. Inject `oms-command-journal`'s native source observer. It derives the existing journal payload, stable source ID, correlation, decision, blocker and next-action fields, and owns journal admission, dedupe, locks and projections. |
| `promote_memory` | `MemoryOutcome`: project repo, agent bytes, canonical outcome text, `task_close` source tag, task identity and actual pre-close verification status. Inject `oms-command-memory`'s native project-append API. It owns memory source/provenance, scrub, locking and indexing. |
| `observe_failure` | Actual command bytes, exit and bounded summary; record a verify failure or resolve that exact command after success. Return only intended advisory stderr bytes. |
| `observe_verify` | Repo and at most the last 16 MiB of actual merged command output; call the native `observe` adapters and return advisory note lines and zero-test runner names. Version collection uses the parent's bounded read-only process owner; `fingerprint` takes a concrete version-probe callback. Failure drops observations, never fabricates a verdict. |
| `skill_contracts` | Repo; return read-only name/command hint rows. These commands are never executed at close. |

Reference/state service absence refuses mutation. Missing journal/memory
producers produce the existing explicit degradation warnings; they never claim
that an event or memory note was recorded. Journal suppression and recursion
guards retain the shell environment contract. Verify-observer service wiring
and version collection, failure-ledger and skill-contract producers remain
parent connections. Output recognition, arbitrary-size counts, native uname,
lock hashing (8 MiB per file, 64 files) and note assembly are implemented here.
`consumers::loop_warnings` takes an optional current add/delete count from the
parent's native Git inspector; absence omits only that reminder. It never
executes configured Git clean filters to compute a reminder.

`run_with_io` streams merged command stdout/stderr to its output writer, in
the explicitly selected repository directory, even when the default task
belongs to an enclosing Git root. User commands inherit stdin/environment.
The parent supplies interruption plus an optional observed HUP/INT/TERM
number. The crate forwards that signal to the owned process group, permits
500 ms for termination, then kills/reaps remaining owned processes. It never
installs handlers or changes global cwd/environment. No deadline is invented
for the user's verification command.

## Durability, concurrency and interruption

| Written record | Second call / interruption residue |
| --- | --- |
| Current packet, metadata, section or note | Uses `oms-lock`'s exact legacy file-lock spelling and `oms-safe-fs` atomic replacement. Concurrent cooperating calls serialize and re-read. A kill before publication leaves the old complete packet and possibly a private staging file; after rename it leaves the complete new packet. Publication/durability errors require reconciliation, not blind retry. |
| Append receipt | Note and raw-input digest receipt publish together. Exact replay preserves packet bytes, even at 256 receipts. Conflicting, duplicate, malformed or excess receipts refuse before publication. A second call cannot publish only the note or only its receipt. |
| `.oms/.gitignore` | Best-effort exclusive initialization with the canonical `*` body. Existing files are preserved. A kill can leave created private directories or its own staging file. |
| Archive | Closed metadata is atomically published before a descriptor-relative no-replace rename. Concurrent calls cannot overwrite an archive; a name collision takes a numeric suffix. A kill can leave a complete closed current packet before rename, or a complete archived packet after it. Both directories are fsynced where supported. |
| Rotation successor | Archive publication precedes new current-packet publication. A kill between them leaves the prior closed archive with no current packet. No successor is implied until its own publication succeeds. A replacement note is a separate operation with a task-identity comparison; interruption may leave that note absent and returns failure. |
| Verification | Snapshot sets active under lock, then the user command runs unlocked. Outcomes compare task identity; successful receipts also recheck command digest and fresh Git state. Rotation or a changed contract cannot give its successor a pass. Interruption leaves active state and no successful receipt. A successful run preserves the source's two Verification bullets before setting verified. |
| Memory and journal | These are written only by the injected native producers. Memory promotion precedes close/archive, and journal close follows archive. A kill between those domains can leave memory without an archive, or an archive without its journal observation; their existing producers own dedupe/reconciliation. This crate does not claim cross-domain atomicity. |
| Reference lifecycle/monitor | Owned entirely by the injected reference service; a kill or unproven monitor shutdown must preserve unresolved operation evidence. Membership or an operation record grants no new authority. |

Canonical packet reads refuse symlinks, multiple hard links, foreign owners,
non-regular files, changed generations and records over 32 MiB. The existing
scrubber checks note/section inputs and context, including matching ambient
values. Context retains its 6000-byte default body budget, per-section shares,
newest accumulating entries and stable truncation marker. Loop note/current
caps retain their environment overrides; zero note bytes appends no bullet,
zero current-bullet cap disables pruning, and zero context bytes emits no body.

## Compatibility detail and integration boundary

On a positively proven unborn Git repository the fingerprint uses single-line
`unborn`. The old helper accidentally captures `HEAD` on stdout before its
fallback, yielding a newline-bearing `HEAD`/`unborn` value that cannot form a
usable metadata receipt. Other unresolved Git/ref errors remain errors rather
than fabricated missing state. The owner approved this bounded correction in
room message `msg-d7d24efb90ad4bb4991d8aea097c1fb2`. Canonical anchors are
`scripts/lib/oms-common.sh:810`, `scripts/agent-task.sh:716`, and
`scripts/lib/agent-task-common.sh:861`; native production is
`src/git.rs::state_fingerprint` and consumption is `Task::verification`.
An existing malformed receipt remains stale; only a new actual user-command
verification can replace it. No schema or implicit migration was added.
Unsafe or unavailable plan evidence yields an explicit unknown lifecycle,
rather than silently treating an unreadable record as absent.

Parent work still required: workspace/lock/CLI registration, real service
injection, native caller cutovers, integration/security review, canonical
tests, CI and installation. Those files and records are outside this crate's
write scope. Standalone compilation and disposable fixtures are implementation
evidence, not lifecycle acceptance or publication approval.

Implementation evidence retained from disposable fixtures: Rust 1.99.1
`cargo build --lib --locked --offline` and `cargo fmt -- --check` passed;
all ten modes, CLI validation, concurrent append, dedupe capacity/conflict,
rotation CAS, failure/skip/freshness, signal forwarding/termination, unsafe
paths and collision archives were exercised. Normalized init/update bytes
matched the shell helpers, including legacy unknown metadata/sections.
Twenty-one output-recognizer cases and lock digests at empty, zero-byte,
8 MiB and 64-file boundaries matched the canonical observer. The committed
HEAD fingerprint matched the source exactly in a read-only comparison;
the unborn fixture matched its tracked/untracked components while removing
only the approved `HEAD` newline prefix. Reverse-applying the non-test patch
made the scoped probe build fail for the absent crate, and restoration built
successfully. The outbound diff scan was clean. Canonical suites, the
owner-run `cargo check`, macOS/WSL2 execution, CI and installation were skipped
under the worker brief; no acceptance claim follows from these probes.
