# Native slurm-submit domain

This crate implements [`oms slurm-submit`](../../docs/COMPONENTS.md) in the root
Rust workspace. The [shell compatibility entry point](../../scripts/slurm-submit.sh)
routes to the same native command.

## Public adapters and modes

`owns(args)` accepts every leaf argument after the router selects this verb.
`run_with_source(args, cwd, source_root, interrupted)` provides the conventional
`(status, stdout, stderr)` buffered adapter. For the real public route use
`run_with_source_streaming` with separate output writers: it flushes scheduler
stdout and stderr as they arrive and retains no full scheduler-output capture.
`source_root` must come from parent-validated source metadata. The library neither
mutates process environment/cwd nor installs signal handlers. The parent sets the
interruption flag from its existing signal handling.

| Mode | Behavior |
| --- | --- |
| `-h`, `--help` | Canonical usage and status 0; no repository observation or writes |
| `-- sbatch ARGS... SCRIPT` | Derive key, native advisory lint, durable intent, one scheduler invocation, matching result |
| `--key KEY`, `--key=KEY` | Caller key; a script is optional for `--wrap`; sensitive keys are refused |
| `--dry-run` | Read-only status/key/command display, including advisory lint; no directories, locks, files, or submissions created |
| `--force-resubmit --reason TEXT` | Override submitted/unknown refusal, storing forced/reason/overrides; sensitive reason withheld |
| `--reason=TEXT` | Same value handling as the separate option; force still requires a nonempty reason |
| Submitted repeat | Prior job/time, reconciliation advice, status 3; no scheduler invocation |
| Unknown attempt | Comment-search or untagged-attempt advice, status 3; never infer scheduler failure |
| Failed attempt | Retry permitted; a failed forced attempt leaves the previous window authoritative |
| `reconcile (--key KEY \| --job-id ID) [--repo PATH] [--json]` | Query squeue/sacct (bounded), classify, persist a recovered `submitted` and terminal `reconciled` row; status 0 active/succeeded, 1 other terminal, 75 unknown |
| `--task-id`, `--attempt-id`, `--exec-snapshot`, `--idempotency-key` | Optional provenance on `intent` rows (env `OMS_TASK_ID`, `OMS_ATTEMPT_ID`, `OMS_EXEC_SNAPSHOT_SHA256`); `--idempotency-key` is a `--key` alias |
| Invalid options/command/script | Canonical usage diagnostic, status 2 |
| Scheduler failure | Append failed result and preserve scheduler status; scheduler status 3 maps to 1 |
| Interruption or lost observation before a process answer | Kill/reap the local scheduler client, retain intent as unknown, status 130 or 1; no automatic retry |
| Known answer followed by interruption/output failure | Persist the submitted/failed result, emit outcome diagnostics, and retain the scheduler status (3 maps to 1); incomplete output is warned about |
| Known answer whose result cannot be confirmed durable | Report the known job/status and ledger uncertainty; do not claim that a retry is allowed |

Both command-line comment forms and the canonical `#SBATCH` comment pattern
suppress the added `--comment oms-key=<12 chars>`. The exact supplied executable
and raw argv are passed directly to the scheduler, including empty arguments.
Digest/retained argument lists drop empty elements to match legacy `read_args`.
Verbose and parsable scheduler output select the last matching job/cluster line.
Only a potentially matching line is retained; unrelated diagnostics are streamed
and discarded. A matching job/cluster line can itself be arbitrarily long.

The child remains unreaped while capture pipes are drained. `waitid(NOWAIT)`
observes its answer without releasing its PID; group cleanup happens before the
sole owning Child handle reaps it, and no signal is sent through a reaped PID.
Parent integrations must not independently reap this handle's child. Cancellation
after a known answer drains available output after cleanup and still records the
answer; an incomplete last line is not promoted to a parsed job identifier.

Git receives closed stdin. Scheduler pipe/file stdin is inherited. Foreground
terminal stdin is forwarded through a bounded 16 KiB buffer to a child pipe,
using an independently reopened, identity-checked nonblocking terminal descriptor.
This keeps the caller's terminal modes, descriptor flags and foreground group
unchanged, and prevents the isolated child group from reading the controlling
terminal through inherited stdin. The child sees a pipe in this case; terminal
read-ahead is bounded by that buffer and the pipe. A background/unreopenable
terminal is refused before spawning the client. Wrappers that bypass stdin and
attempt direct controlling-terminal I/O are stopped by the kernel; those stops
are detected and cleaned up as an uncertain attempt instead of waiting forever.
Interactive authentication and native terminal behavior still need independent
acceptance; this implementation does not install terminal or signal handlers.

The advisory uses `oms-command-slurm-lint` in process and redirects both outputs
to stderr. No legacy lint script, shell bridge, Python, or scheduler query is
invoked. The two fixed read-only Git metadata queries preserve the canonical
repository/HEAD observations. On macOS the shared `oms-lock` implementation retains its
existing fixed process-identity query; this command adds no such subprocess.

## Internal helper consumers

The [native command](src/lib.rs) uses [`ledger`](src/ledger.rs) for request keys
and submission state. Native callers share these operations and the same ledger.

| Existing operation | Native function |
| --- | --- |
| `read_args` | `ledger::read_args(path)`: NUL-separated Unix bytes, excluding empty elements |
| `key` | `ledger::key(&KeyOptions)`, then `Key::json()`; all key/digest/path fields |
| `status` | `ledger::status(absolute_ledger_path, &key)`; sorted JSON with state and optional job_id/ts/attempt |
| `begin` | `ledger::begin(path, absolute_cwd, &key, &BeginOptions, interrupted)`; Started(attempt, JSON) or Refused(diagnostic) |
| `finish` | `ledger::finish(path, absolute_cwd, key, attempt, Outcome, interrupted)`; append submitted or failed |

Root/cwd inputs are absolute; relative scripts resolve against cwd. Helpers
return path-free errors to their caller. Unlike the Python helper, native begin
and finish acquire the compatible lock internally: remove the caller's enclosing
`oms_with_file_lock`, or borrow these operations at the native adapter boundary.
Do not nest locks. `store_args` remains opt-in and also requires a clean scan;
begin independently screens custom keys and forced reasons at its write boundary.
Public force requires a reason; the lower-level begin preserves the helper's
explicit force/reason fields, including an empty reason supplied by its caller.
`finish` accepts an already-known Outcome, so its interruption parameter does not
cancel persistence. It acquires the same compatible lock with the configured
finite `OMS_LOCK_TIMEOUT` and emits no scheduler retry. A lock/write/fsync failure
remains uncertain; stderr delivery itself cannot be guaranteed to a broken sink.

Canonical key hashing uses the version-1 sorted compact ASCII Python JSON
document. The separate argument digest uses Python's default comma spacing.
Unix argv/path invalid bytes survive as surrogateescaped code points, with the
same custom-key byte hashing. Rows use sorted default-spaced ASCII JSON and UTC
second timestamps. Unknown row fields are ignored, malformed/non-object rows are
skipped, and schema-1 fields and append-only authority remain unchanged.

`repository::discover` uses the canonical fixed `git -C ... rev-parse
--show-toplevel` and `git -C ... rev-parse HEAD` observations, once each per command.
Git retains authority over all its supported ref backends, configuration includes,
environment, discovery and linked-worktree behavior. Git stderr remains suppressed.
Non-Git commits use `nogit`; unborn commits retain the shell's `HEAD\nnogit` value
(Git's stdout before its failure, followed by the fallback). Raw output bytes and
Bash's trailing-newline removal are preserved. `discover_with_interrupt` cancels
and reaps queries using the caller's flag. A parent that already has this fresh
snapshot can use `run_with_repository` and `Repository { root, commit }` to avoid
duplicate queries. No additional Git library or ref-format dependency is added.

## Authority, concurrency, and interruption residue

Duplicate detection is inside one repository's `.oms` only: **not exactly-once
across machines or clones**. `OMS_SLURM_SUBMITS_FILE` overrides the ledger;
relative overrides retain the supplied logical cwd spelling. When the router
supplies native getcwd's physical path, `PWD` is accepted only after directory
device/inode and physical-path identity validation. Explicit logical cwd takes
precedence; Git/key derivation still observes the corresponding physical cwd.
Invalid ledger path shapes are rejected before creating lock artifacts.
`oms-lock::Lock::for_file`
uses the original supplied state spelling, Bash cksum name, ambient lock settings,
and LegacyAuto backend. All writers must retain the same spelling/backend.
File aliases do not acquire new cross-alias or cross-host guarantees.
For I/O, a validated caller-root alias is opened through its physical directory;
the original alias and all held descendants are revalidated at missing returns
and before publication. Descendant directories and ledger leaves remain no-follow.
On macOS, absolute `/tmp` and `/var` overrides additionally accept only root-owned
system symlinks to `/private/tmp` and `/private/var`, retaining alias identity proof.
Arbitrary symlinked ledger directories/leaves and multiple-linked leaves remain
refused. Lock names continue to use the original supplied spelling, so cooperating
writers must not mix aliases or change their targets while submitting.

| Written object | Concurrent call / kill between writes |
| --- | --- |
| Ledger parent directories | Missing parents are created as 0700 and directory-synced; interruption may leave empty directories |
| `.oms/.gitignore` | Shared safe-fs atomic mutation under that file's compatible lock preserves an existing file; a kill can leave its documented temporary publication residue; ledger authority is unaffected |
| Legacy lock files/directories | Shared oms-lock ownership, stale recovery and crash residue remain authoritative; no approval-lock protocol is substituted |
| Intent row | Lock serializes check-and-append; another normal call observes unknown and refuses. The row and parent are synced before scheduler spawn |
| Scheduler client | Runs outside the lock after intent. A kill before observing its answer leaves unknown; owned group cleanup precedes leader reaping; no scheduler query or inferred retry |
| Submitted/failed result | Same lock, same attempt, append and fsync even after late cancellation. A partial/absent result can leave unknown; only a confirmed failed row establishes retry permission |
| Unterminated ledger tail | A newline is appended before the next row. A kill between those writes retains prior records and prevents swallowing a later durable intent/result |

Ledger operations retain and revalidate directory/file descriptors, refuse
unsafe symlinks, hardlinks, foreign ownership and world-writable ledger authority,
and never replace the authoritative ledger inode. Fresh checks happen under the
lock immediately before append. Existing owner-controlled group-write modes such
as 0775/2775 directories and 0664 ledgers are accepted and preserved. Only a new
ledger receives mode 0600; this command never repairs/chmods existing state.
Same-user hostile namespace replacement and
cross-machine PID identity retain the limitations documented by `oms-lock`.
No measured NFS/XFS speedup or global transaction guarantee is claimed.

## Source and acceptance boundaries

The native sources cover [public parsing and execution](src/lib.rs),
[keys, schema and submission windows](src/ledger.rs),
[repository resolution](src/repository.rs), and [scheduler reconciliation](src/reconcile.rs).
The [contract tests](tests/reconcile_contract.rs) exercise the ledger and fake
scheduler. Public usage is documented in [Components](../../docs/COMPONENTS.md).

Historical port references include the former Bash implementation of
`scripts/slurm-submit.sh`, `scripts/lib/slurm_submit.py` and
`test_slurm_submit_detects_repeat_submissions` in the retired Bash smoke suite.
The fake scheduler tests cover local behavior. Installed activation, macOS
execution and a live Slurm deployment require separate evidence.
