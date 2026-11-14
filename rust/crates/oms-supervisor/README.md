# Native agent supervisor and owned-process engine

This standalone crate supplies native process execution and implementations of
all `agent-supervisor` modes. `capture.rs` and `process.rs` are reusable engine
modules; `command.rs`, `cli.rs`, `lifecycle.rs`, and `storage.rs` implement the
command domain. The binary is a native runner entry point. No implementation
invokes Python, Bash, provider CLIs, a scheduler, or a container engine. An
explicit payload may itself be a user-selected shell. Fixed Git and `ps`
queries are native external producers, driven by the same process engine.

This is an intermediate migration patch. Compilation does not establish
behavioral/security parity. The gaps below block a claim of full migration
completion. Root workspace/lock registration, CLI routing, service adapters,
global signal installation, integration tests, release, and global documentation
remain with the parent.

## Reusable API

```rust,ignore
Request::new(argv: Vec<OsString>) -> Request
Reactor::new() -> Reactor
Reactor::dedicated() -> Result<Reactor, String>
Reactor::start(&mut self, request: Request) -> Result<JobId, StartError>
Reactor::stop(&mut self, id: JobId, reason: StopReason) -> Result<(), String>
Reactor::poll(&mut self, id: JobId, budget: Duration) -> Result<Progress, String>
Reactor::snapshot(&self, id: JobId) -> Result<Snapshot, String>
Reactor::pending(&self) -> usize
```

`Request` retains raw OS argv and cwd, optional stdin bytes, inherited or cleared
environment, environment additions and removals, merged/separate streams,
stdout and optional independent stderr capture policy, optional monotonic
`Instant` deadline, signal scope, and termination grace. Environment removals
win over supplied additions. `None` installs no execution deadline. An already
expired deadline refuses execution with `StartError::DeadlineExpired`.
`StartError` distinguishes invalid requests, unavailable capabilities, setup
failures, and actual payload launch errors.

`CaptureOptions` carries a byte limit, `Drain`/`Terminate` overflow policy and
optional line cutoff. A byte limit is exceeded only after the limit; equal is
allowed. Zero retains no bytes. A zero line cutoff requests immediate consumer
cutoff. Separate stderr inherits the byte policy but no stdout line cutoff,
unless explicitly configured. The two separate captures have independent byte
bounds. Merged mode uses one kernel pipe, preserving that pipe's byte order;
separate mode makes no global stdout/stderr ordering claim. `Captured` reports
observed/dropped counts, a hash of the observed selected stream, actual EOF,
byte limiting, and consumer cutoff. Tiny generic limits unable to hold the
canonical truncation marker retain a prefix; supervisor logs require at least
1024 bytes and retain the canonical head/marker/tail layout.

`Progress::Pending` retains the child inside the concrete reactor. `Complete`
contains the leader outcome, numeric terminating signal, stop reason, captures,
duration, signal records, reaping evidence and explicit supervision error.
`Complete` alone is not successful cleanup: callers must inspect
`cleanup_complete()`, output completeness/cutoff, requested scope and stop reason.
Process-group completion never claims whole-tree containment.

The state machine is spawn -> observe/drain/feed -> freeze completion or stop
reason -> TERM/grace/KILL as needed -> observe quiescence -> nonblocking reap ->
optional adopted-child cleanup -> result. Every leader observation uses portable
rustix `waitid(EXITED | NOWAIT | NOHANG)` until its group signaling decisions are
finished. There is no reader thread, blocking wait, reader join, default execution
timeout, post-leader drain timer, or inference of EOF from missing `POLLIN`.
Leader exit with a live pipe keeps draining until actual read-zero, an explicit
consumer cutoff, or the requested deadline/cancel. Once ordinary completion is
frozen, cleanup cannot change it into a timeout.

`Scope::Leader` waits/signals only the exact unreaped child. `ProcessGroup`
additionally observes Linux group quiescence while the leader is pinned; escaped
sessions are outside that scope. `WholeTree` requires an explicit dedicated
Linux reactor, initialized before spawning, with one thread, no existing
children, and no existing subreaper owner. The caller must exclusively own all
subsequent child creation and SIGCHLD disposition. Reactors are not Send/Sync.
Only this dedicated mode uses wait(All); it individually reserves/reaps adopted
children and requires actual ECHILD before `all_reaped`. A library embedded with
other child-spawning code must not claim this exclusive capability.

A queued kill never proves exit. Kernel D-state can retain a job indefinitely.
Bounded callers keep the reactor and drive it again; there is no detached reaper.
Dropping a reactor drives cleanup without a time bound. Thus dropping a pending
reactor is explicitly not a bounded-return operation. CLI shutdown with failed
identity persistence drives cleanup in the foreground; persisted pending cleanup
retains canonical `cleanup_failed` identity for recovery rather than terminal
success. A durable detached runner is handed to the OS reaper at CLI exit.

## Command integration and canonical mode checklist

`CommandHost::new(runner_argv)` supplies the native runner executable and any
frontdoor prefix. The adapter appends `--repo REPO _run-one --attempt ID`.
`run(argv, default_repo, interrupted, &mut host)` returns status/stdout/stderr.
`run_streaming` writes and flushes live dispatch IDs to supplied writers. Library
callers retain `CommandHost`, call `drive()`, and inspect `pending()`; a dropped
host waits for runner protocol completion without killing the runner owner;
that destructor also has no time bound. The optional
`received_signal` callback supplies actual numeric signal provenance from the
parent's signal owner. A Boolean interruption invents no received signal number.
The standalone entry point creates its runner session using safe `setsid` inside
the new native runner; it installs no process-global signal handlers.

Canonical anchors below refer to
[`attempt-runner.py`](../../../scripts/lib/attempt-runner.py), read through EOF.
The implementation exists for every row; behavioral verification is deferred.

| Mode | Source lines | Implemented behavior / limitation |
| --- | --- | --- |
| submit | 452–531 | Git-common-dir cwd authority, immutable argv digest in lifecycle/spec/job, trusted-local-only claim, log bounds, budgets, queued publication under queue -> event lock order; failure blocks incomplete submission. |
| dispatch | 1038–1160 | Strict job snapshot, queue ordering, global/provider/running/job/reservation limits, launching reservation/nonce, cancellation recheck, native runner launch identity, failed-launch convergence and streamed accepted IDs. |
| status | 1163–1173 | Canonical public JSON keys, lifecycle/runtime/provider/profile/exit text, usage/budget/lineage and honest scope fields. |
| wait | 1176–1197 | Terminal settlement; review settles only a review contract, never a done contract; interval/interruption and timeout state diagnostic. |
| attach | 1200–1214 | Bounded log read, UTF-8 replacement, universal newlines and requested tail. Concurrent modification may refuse a snapshot rather than return unstable bytes. |
| cancel | 1217–1302 | Queue-locked monotonic request, pre-launch cancellation, stale identity failure. The owning runner forwards; an external caller does not race numeric group reuse. |
| resume | 1305–1358 | Terminal/blocked parent, base-HEAD fence, new child attempt/spec, preserved command/budget/cwd intent, reset runtime ownership and reservations. |
| reconcile | 1361–1490 | Ten-second orphan/launch grace, queued-only orphan block, live/zombie runner checks, identity mismatch handling, dry-run selection, lost-runner convergence; live external groups retain cleanup_failed because safe group signaling authority is unavailable. |
| gc | 1493–1546 | Dry-run default, terminal lifecycle AND terminal runtime only, retention bounds, liveness checks, malformed-time retention, managed log then job deletion; lifecycle evidence is retained. |
| serve | 1549–1572 | Wall/job/running bounds, counts IDs returned by dispatch, provider/budget routing, until-idle behavior, one job snapshot per iteration and native runner reaping. |
| _run-one | 769–1035 | State claim, unsupported hard-spending refusal before payload, argv/profile/completion/cwd revalidation, numeric identity publication, merged bounded log, heartbeat/duration usage, cancellation/timeout, verifying/review/done transitions and explicit cleanup failure ownership. Whole-tree runner action currently requires Linux. |

The parser implements every option in canonical lines 1575–1652, including
repeated provider limits, submit remainder arguments, choices, help, and unique
long-option prefixes. All entry points apply the child read-only mutation guard.
There is no fallback to the legacy implementation.

## Caller/producer map

- Plan `services::SupervisedCommand`: map argv/cwd/wall/output_limit/environment
  fields into `Request`; derive exit/runner_exit/timed_out/output_limited/
  launch_error/supervision_error/output/duration from typed start/progress/result
  fields. Successful leader exit alone is insufficient for acceptance.
- Runtime's typed execution boundary: use the same request/cleanup engine; this
  crate contains no profile/container/remote recipe or receipt domain logic.
- JobDigest sacct/squeue query: use raw paths/argv, an optional deadline, explicit
  cancel, and `CaptureOptions::lines` for the 20-line consumer cutoff. Select
  merged kernel ordering or independent streams explicitly. No hidden requery
  or arbitrary timeout is installed by the engine.
- Runs/Slurm/helper queries: the engine launches the caller's fixed native argv.
  The command adapter's own Git/ps queries have explicit three-second deadlines.
- Lifecycle writes use the actual
  [`oms-command-operations`](../oms-command-operations/README.md) APIs:
  `create_attempt`, `new_event`, `append_lifecycle`, projection, spec and lock
  helpers. Numeric canonical schema/sequence/idempotence/CAS remain with that
  producer. This crate creates no second ledger.

## Persistence and concurrent/crash residues

Only canonical runtime directories, `jobs/ID.json`, `logs/ID.log`, queue lock
namespace, lifecycle events/specs and atomic temporary files are written.
Runtime root hashing uses the canonical physical repository's raw path bytes
under XDG state or the default state home. State-home ownership and ancestor
permissions are checked; OMS directories require private ownership/modes.
Directory components are opened without following links, retained, and checked
against their named identities. Files must be regular, single-link, privately
owned, and bounded; reads/writes/deletes use their retained directory descriptor.
These checks detect observed races, not hostile same-UID atomic transactions.

| File/record | Second concurrent call | Kill between writes / uncertainty |
| --- | --- | --- |
| Runtime directories | Validates the same anchored private namespace. | May leave canonical empty directories after mkdir; no arbitrary cleanup. |
| Canonical queue lock owner record | Existing Operations owner/start/nonce protocol serializes writers; order is queue then events. | Owner death/stale-generation recovery remains Operations-owned. |
| Lifecycle event stream and attempt spec | Canonical writer allocates sequences and checks idempotence/CAS; independent submissions get distinct IDs. | Creation can precede enriched spec/job publication; orphan reconcile waits ten seconds before blocking. |
| Job JSON and job-name temporary | Queue lock serializes state claims/reservations; new-job publication also holds the event lock. | Before rename leaves prior record and possibly the operation's exclusive temporary; after rename/fsync error reports publication uncertainty. No blind durability-success retry. |
| Log | Only the claimed runner opens its managed log; descriptor/name checks detect replacement. | Running log appends only new captured prefix bytes up to its cap. Final head/tail rewrite happens once; a kill during that rewrite can leave a partial bounded log, never authority for success. |
| GC log then job deletion | Queue serialization and terminal selection prevent legitimate concurrent writers. | A kill after log deletion leaves the terminal job; a repeated GC can finish the same deletion. No lifecycle deletion. |
| Live process ownership | A second _run-one cannot claim starting/running; cancel only publishes a request. | In-memory pending reactor survives library return. Process death between spawn and child-identity publication remains a canonical crash window; no launch gate protocol is implemented. |
| Final job then lifecycle transition | Canonical transitions fence conflicting/duplicate events. | A kill after finished runtime publication but before lifecycle settlement can leave a nonterminal lifecycle row; existing reconcile does not repair that finished-state gap. |

## Unimplemented seams and admission blockers

- Reconcile cannot safely signal a dead runner's recorded process group. The
  pinned rustix `pidfd_send_signal` interface addresses one process and exposes
  no descriptor-bound group flags; a separate identity check plus killpg does
  not reserve PID/PGID ownership. Such groups retain `cleanup_failed`, identity
  and capacity reservation. No false terminal cleanup is published.
- Linux dedicated subreaper mode provides the lifetime mechanism, subject to its
  explicit exclusive-process contract and proc observation. macOS can compile
  ordinary/direct-child operation, but whole-tree mode refuses before payload;
  group-quiescence observation is also unavailable there. WSL2 depends on its
  Linux proc/subreaper capability. No sandbox/access-control claim is made.
- Exact argparse help/error bytes and Python's extreme arbitrary-integer/
  nonfinite-float behavior are not fully ported. The adapter refuses integers
  outside native ranges, nonfinite/negative polling values, and runtime records
  over 16 MiB. Existing unsafe modes/links are refused rather than healed.
- Late inherited pipe bytes drain after leader exit, as required by the worker
  brief. This intentionally differs from the old smoke case that expects a
  background child to be killed at successful leader exit before its delayed
  output. The brief takes priority; the parent must reconcile that test contract.
- The inherited spawn/publication and final-job/lifecycle crash windows above
  remain. A native launch-gate/recovery transaction would require further work.
- Operations' public append API currently reads/projects the prior ledger on
  each append (`events.rs` 119–184). It appends new bytes, but end-to-end append
  work is not O(newrow). This crate neither rewrites ledger history nor claims
  an unmeasured filesystem/HPC speed improvement.

## Verification boundary

Rust 1.99.1 offline `cargo check --all-targets` and rustfmt have been run during
implementation. The first compiler run caught a Drop-type constructor move;
that source error was repaired. Final check details are in the worker report.
All resolved dependency versions already occur in the root lockfile.

No tests were added, edited, deleted or executed. Canonical reference reading
covered all 1673 supervisor lines, all 514 dispatch-supervisor lines, all 289
runtime execution lines, capture_verifier 2105–2161, all 997 supervisor smoke
lines, and the nearest provider lifecycle case (244–275). Repository searches
also located GC, dashboard, inventory and CLI consumers. Existing tests remain
unchanged. Tests, integration scenarios, behavioral probes, cross-platform
execution, baseline-reversal tests, CI/install/publication and paid/provider/
scheduler/container/remote launches were explicitly deferred by the brief.
Consequently empty/zero/equal/malformed boundaries and failure exits are source
implementations, not runtime evidence. Independent security review and parent
admission are still required.
