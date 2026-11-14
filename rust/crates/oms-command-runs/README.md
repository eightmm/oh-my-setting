# Native run ledger and reconciliation

This isolated crate implements the full local `oms run-ledger` and
`oms run-reconcile` command families. The owning CLI must register it and
inject native services before public routing changes. The temporary child
workspace and lockfile are for isolated admission; the parent owns root
workspace/lock registration, documentation, CI and release integration.

`run(command, args, context, services, stdout, stderr)` streams user output.
`execute(args, context, services, stdout, stderr)` returns an `ExecutionRecord`
with the authoritative schema-1 ledger row, path and command exit for native
trusted-local experiment consumers. Neither API changes process cwd or env.
`Context` supplies raw OS cwd/child environment overrides, canonical lock
settings, cancellation, an optional actual signal number, a 30-second default
query deadline and an optional command timeout. The parent registers HUP,
INT and TERM handling. Cancellation kills the child process group, escalates
after 250 ms, drains output and reaps its direct child. A waitid NOWAIT
observation keeps the child identity reserved through group cleanup.
Deliberately detached sessions are outside a portable process-group guarantee;
Linux subreaper ownership remains a parent supervisor responsibility.

The ledger supports run options, exact raw argv/cwd, commit and staged/working
diff identity (including unborn HEAD), duplicate warnings, project preflight,
unsafe skips with scrubbed reasons, seed/group inference, finite scalar metrics,
started research rows and final rows. Capsule companions preserve pre-command
contract and input identity through the existing captured-metadata envelope.
Read-only list/top/table preserve append-order reruns, contract/data filters,
explicit mixed ranking, distinct replicate scopes, min/max order, final-split
advisories and md/tsv/tex output. Tables also enforce the documented data filter;
the old table helper omitted that filter despite claiming the same selection
as top. Numbers use four significant digits; bootstrap uses Python's seeded
MT19937 choice algorithm and the existing 2,000-sample percentile definition.

Reconciliation supports scan/apply/list/usage, ledger/job selection, memory and
agent options. It handles legacy and full allocation rows, newest array/hetjob
records, reset-on-zero restart counts, requeues, terminal classification,
submission-time receipt identity, exact unavailable-digest placeholders and
allocation usage. A failed queue query cannot establish that a preempted or
node-failed job is finished. Query deadlines return 124 without a terminal
receipt. The native transport executes only fixed read-only sacct/squeue argv;
it never submits, requeues or cancels a scheduler job.

The public help contracts are in [ledger-help.txt](src/ledger-help.txt) and
[reconcile-help.txt](src/reconcile-help.txt). Typed metrics and usage adapters
are public modules. `make_input_identity` and `capture_for_ledger` perform
metadata arithmetic only; they never reopen captured configs or datasets.
Unknown JSON fields, large integer spellings and Unix surrogate-escaped argv
are retained by the shared JSON representation and compatible codecs.

## Injected service contracts

The parent implements `Services` and accurately advertises each available
service. An applicable missing service refuses before its side effect; there
is no Python/Bash fallback and no local substitute writer.

| Service | Request and native producer |
| --- | --- |
| Journal | `Observation { repo, source_type, record_path, row, event_type }`; parent calls oms-command-journal's native observer. Existing journal disable/active/suppress settings apply. An actual observer failure degrades with the canonical warning after the primary record is durable. |
| Memory | `MemoryNote { repo, agent, text }`; parent calls oms-command-memory's native append API. Required only with reconcile `--memory`; actual append failures print the canonical skipped-note warning. |
| Run link | `RunLink { repo, run_id, record_path, code }`; parent owns the existing run spine. Explicit run id wins, otherwise CURRENT's timestamp/TTL is checked. Actual link failures remain best effort. |
| Digest | `DigestRequest { repo, job, accounting, interrupted }`; parent calls the existing native job-digest engine with captured accounting. Failure produces exactly `# job ID digest unavailable` plus newline; cancellation/deadline errors propagate. |

The accounting snapshot explicitly identifies
`AccountingFormat::SacctAllocationParsable2`, its exact ordered columns,
availability, process status and unmodified stdout/stderr. The request is
`sacct -X -D -j ID --format=JobID,State,ExitCode,Elapsed,Cluster,Submit,Start,End,AllocTRES,NodeList,Timelimit,AllocCPUS,TotalCPU,JobName,Restarts -n -P`.
These bytes are not job-digest's usual formatted accounting table. The parent
must deliberately render/preserve this shape when passing its native
`run_with_accounting` snapshot; it must never reinterpret it as that other
shape or issue another scheduler query. Failed queries also stay failed.

Named consumers are runtime `experiment_run._run_through_ledger`, research
`experiment_launch`, run-capsule's completed-command companion, agent ML
context's ledger reader, experiment budget's reconcile usage, task-spooler/job
queue launch observations, slurm-submit's references and journal's run-ledger/
run-reconcile sources. The slurm-submit submission ledger and the journal have
one producer each; this crate writes neither. The parent retains their routing
and typed adapter integration.

## Storage and interruption

Writers retain oms-lock's exact Bash authority and original pathname spelling.
Append opens the existing inode with O_APPEND and never reads, clones, truncates
or replaces the ledger body. Its data I/O is proportional to the new row plus
at most one existing final byte for torn-tail separation. The metadata work
depends on path depth and fresh alias/namespace proof, not history length.
Command-level duplicate detection, rankings and usage reports still read history
according to their existing contracts; this is not an O(new row) claim for every
whole command. No physical HDD/NFS speedup has been measured.

Existing append leaves must be regular, single-link, owned by the effective
user, writable and not world-writable. Owner-controlled group-write modes such
as 0664 and 0660 are accepted. Existing mode, owner, group and inode are retained;
there is no chmod/chown. New leaves use exclusive creation with 0600 subject to
umask and the parent directory's normal group inheritance. Missing parents use
the existing private 0700 creation policy; existing project/setgid directories
are left intact. The checked final-byte read requires read access to a nonempty
ledger as well as append access. Read-only source/data reads retain the regular
single-link policy and do not acquire a private-owner or group-write prohibition.
Append syncs the file and parent directory before reporting success. Directory
fsync uses the existing safe-fs unsupported-operation policy on supported hosts.
Write, sync, guard or namespace failure after mutation reports explicit partial
append/durability uncertainty; no rollback, truncation or blind retry occurs.

Streaming readers retain root/ancestor descriptors and leaf identity until the
completed read is checked. Missing returns revalidate both the opened prefix
and the absent name. Completed reads reject in-place changes, replaced leaves
and renamed/replaced ancestors. Bounded metadata reads distinguish proven
absence from present empty content; metric contracts no longer infer absence
through a later pathname exists check. Evaluation-code hashing performs the same
completion proof. Physical line bounds remain unchanged.

The path anchor accepts a supplied logical caller-root spelling, or a proved
alias of its containing root, by binding and rechecking the physical target and
the named alias entries. On macOS, unrelated /tmp and /var inputs additionally
accept only their validated /private/tmp and /private/var aliases. Physical
ancestors and protected descendants retain descriptor/name identity checks;
descendant symlinks and leaf symlinks are refused. Parent components are opened
and retained as actual directories, never lexically cancelled before proof.
Other absolute override paths are walked from the filesystem root without
following arbitrary aliases. A non-caller, non-system ancestor alias therefore
requires a supplied caller-root authority; it is not silently accepted.
These checks establish observations at validation boundaries, not atomic
compare-and-swap against a hostile same-user namespace actor. Cooperating writers
must still use the same lock spelling; no cross-alias or cross-host lock guarantee
is introduced. Root/alias or NFS observation failures stay errors, never absence.

Digest/artifact replacement continues through oms-safe-fs atomic publication,
rebased to the proved physical parent with the original external lock retained.
It keeps the bounded replacement contract and publication error phases. An
unchanged-body return is not a new fsync or recovery proof after an earlier error.

| Written artifact | Second call / interruption residue |
| --- | --- |
| Ledger JSONL | Cooperating calls serialize on the existing lock and append to the same inode. A kill before write leaves old bytes plus possible newly created parents/an empty private leaf. During write, a separator-only or partial record can remain; after write but before sync, visible bytes have uncertain durability. Old evidence is never rolled back. A research started row remains evidence of an unfinished attempt until a complete final row is established. |
| Reconcile JSONL | The receipt lock still covers duplicate admission, digest and receipt append. Concurrent applies admit one visible `(job id, submit)` receipt; missing submit retains the legacy id-only rule. A kill between digest replacement and receipt append leaves an orphan digest. A partial receipt is retained, not treated as completion; a complete visible receipt after a failed sync still requires durability reconciliation before claiming recovery. |
| Reconcile digest `ID.md` | Its own lock plus safe atomic publication preserves a complete old or new digest. Reused job ids retain the canonical filename and may replace the digest referenced by an older receipt. |
| Replacement temporary bodies and missing parents | Atomic replacement retains safe-fs cleanup of its own temporary file; SIGKILL may leave an exclusive temporary name. Append creates no temporary ledger copy. Either path can leave newly created private parents, and append can leave an empty new leaf on pre-write refusal. No automatic broad cleanup or blind publication retry occurs. |
| Lock metadata | Owned by oms-lock; flock releases on process death, mkdir generations retain its documented stale/recovery behavior. No new lock schema or owner cleanup is introduced. |

Journal, memory and run-link receipts are owned by their injected native
producers. A kill after a primary row but before a callback can leave that
observation absent; an apply retry does not duplicate a primary receipt to
manufacture callback success. Their replay/recovery policies belong to those
producers and parent integration.

Bounds: metric/contract/capture metadata 64 KiB by default (metrics honor the
existing explicit byte setting), research metadata 16 KiB, 50 metric fields,
20 evaluation files, 200 characters per metric text, 128 input/seed declarations,
4 MiB per ledger line/digest, 16 MiB accounting stdout, 64 KiB query stderr,
256 MiB per requested in-memory tail/ranking, with no whole-ledger append ceiling.
New append bodies are bounded to 4 MiB; an optional separator adds one byte.
Existing history is untouched regardless of its size, subject only to checked
native file-offset arithmetic and filesystem errors. Oversized
observations refuse instead of silently truncating evidence. Large evaluation
code keeps the canonical size plus first/last 64 KiB sample above 8 MiB.
The 200 task-detail output bound still counts all observed tasks and attempts
for state and usage. No NFS throughput or measured speedup is claimed.

## Source coverage and verification

The complete canonical command sources were read: scripts/run-ledger.sh
(763 lines), scripts/run-reconcile.sh (409), run_ledger_metrics.py (527),
run_ledger_table.py (150), and run_usage.py (245). Native consumers and shared
APIs were inspected in bounded source ranges. Mode coverage is run/list/top/
table and scan/apply/list/usage, including all canonical option parsers.
Relevant existing coverage was located in scripts-smoke (ledger, gate,
metrics, top and reconcile cases), test_oms_runtime_run_identity, BSD portability,
work-journal and runtime budget tests. Those canonical suites remain deferred
by the brief; none is claimed to validate this isolated native routing.

Earlier-round focused native tests use disposable files, fake Git and scheduler executables,
and dummy project gates only. They cover output/exit, exact argv, metadata
bounds, digests, comparisons and tables, started/final rows, service admission,
arrays/requeues/usage, duplicate receipts, concurrent append/apply, cancellation,
query output ceilings, changed/torn input and link refusal. Python helpers were
used once as development-only pure hash/bootstrap oracles, with bytecode writes
disabled; production has no interpreter/backend calls.

Root registration, public CLI signal wiring, native journal/memory/run service
adapters, the captured-accounting native digest adapter, canonical regression
suites, full source/install/CI/platform and independent security review remain
parent integration criteria. Linux fixture success is not macOS, WSL2, NFS,
real scheduler or provider evidence. Compilation is not migration acceptance.

Earlier-round verification reported 21 passing scoped native tests and formatting.
Those results apply to the earlier implementation, not this storage repair.
The earlier source-level change-during-read repair was saved as a non-test git diff,
reversed, and its exact test failed behaviorally (`result.is_err()`); the patch
was then reapplied byte-for-byte. A complete non-test patch reversal removes
this entirely new crate's manifest/API. All 21 exact-filter cargo invocations
were attempted in that state and refused for the absent manifest. That is not
a behavioral baseline or evidence that each new assertion detects an old bug.
The parent must retain that explicit earlier new-crate reversal limitation in admission.

The storage-repair phase runs only the scoped pinned cargo check, formatting,
whitespace and outbound/source hygiene. Existing tests are preserved unchanged;
no test suite, parity probe, implementation reversal, real project gate, job,
provider, CI, install, toolchain change, commit or push runs in this phase.
Runtime proof remains deferred for same-inode appends above 256 MiB, group-write/
setgid compatibility, syscall/data-I/O accounting, missing/replaced namespace
faults, alias/parent traversal, partial write/fsync failures, concurrent legacy
writers, and macOS/WSL2/NFS behavior. No existing assertion was identified as
requiring the removed group-write refusal or copy-on-write append behavior;
the existing storage and read-change tests remain read-only inputs for that
later test phase, not fresh passing evidence.

Internal caller changes are root-aware storage adapters for ledger list/top/
table/run, reconcile scan/apply/list/usage, metric contract/split reads and
evaluation-code hashing, and usage aggregation. Public metric/usage APIs keep
their signatures; entry points without an explicit root use the current caller
root, while existing explicit-root entry points retain that supplied authority. The
execution Context, service callbacks, accounting snapshot shape, JSON schemas,
flags and filenames are unchanged. No additional ledger or recovery writer is
introduced. The combined candidate contains both implementation rounds against
the original base; the parent owns its exact source-only patch admission.
