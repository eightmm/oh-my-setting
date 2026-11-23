# oms-lock

Internal Unix component for the M4 mixed Bash/Rust transition. No CLI dispatch,
writer cutover, root workspace membership or installation changes are included.
The only dependency is the already-cached rustix 1.1.6 family, with safe APIs;
unsafe Rust is forbidden. Linux and WSL2 use procfs; macOS uses one fixed native
process query. Other Unix process-identity backends fail closed; Windows is refused.

## API and authority

| Consumer | Constructor and selection | Required caller invariant |
| --- | --- | --- |
| Existing Bash file-lock writers and future equivalent native writers | `Lock::for_file`, `Settings::from_env`, `Mode::LegacyAuto` | Same supplied state spelling, environment and stable backend availability across every writer |
| Future runtime/common, queue, approval, admission and supervisor writers that currently use the Python runtime bridge | `Lock::at_path`, `Mode::Mkdir` | Supply the existing `runtime-` plus first 32 SHA-256 hex characters and `.lock`, derived from the caller's existing resolved-path byte contract; this crate does not substitute shell naming |
| Timeout diagnostics and lock inspection | `Lock::probe` | Use the same authority and selection as acquisition; a probe grants no write authority |
| Nested callbacks inside one critical section | Borrow `&Guard` | Do not reacquire the same lock; guards are deliberately non-reentrant |

`Settings` snapshots HOME, TMPDIR, cwd, PATH, OMS_LOCK_DIR,
OMS_LOCK_FORCE_MKDIR, OMS_LOCK_TIMEOUT and OMS_POLL_MAX_SECONDS. It never mutates
process environment, cwd or umask. Typed timeouts and poll maxima must be positive;
ambient decimal seconds use Bash's default for empty, zero, malformed or overflowing
values. An unrepresentable monotonic deadline is rejected before creating anything.
Only the exact force-mkdir value `1` overrides `LegacyAuto`. Auto selection checks
for an executable `flock` on the captured PATH, including effective access checks;
it does not execute that command. Explicit `Flock`/`Mkdir` require the consumer to
know the shared domain's backend. Installing/removing flock or changing settings
while any writer is active requires a coordinated authority decision.

Shell naming preserves original Unix bytes, including non-UTF8, repeated separators,
trailing separators and newlines. Relative paths prepend the captured cwd and `/`.
The leaf is sanitized with the C ASCII allowlist, followed by POSIX cksum checksum
and byte length, then `.lock`. It intentionally does not canonicalize the state leaf:
legacy aliases can select different authorities. Consumers must use the same spelling
or establish physical identity before selecting their shared contract. Lock-root
identity is independently checked against retained descriptors on acquisition,
reclaim, guard validation and release. Existing ancestor aliases can resolve to the
same physical directory; symlink lock roots and lock entries are refused.

Without an override, state under TMPDIR uses its per-user temporary lock directory;
state under HOME otherwise uses the cache lock directory; other `/tmp` state uses
its per-user temporary lock directory. A missing, writable temporary root is only
created during acquisition. Probe and construction never create it. Unsafe temporary
candidates fall back to the cache, matching legacy routing. No XDG variables are used.
A missing root that cannot subsequently be created causes an acquisition error;
there is no retry through another authority after an I/O failure.

`acquire` waits with monotonic deadlines and canonical 1/2/5/10-second backoff at
5/30/120 seconds, capped by remaining time and the configured poll maximum.
`try_acquire` never waits for contention; it may perform bounded stale recovery.
Cancellation is checked before writes, between attempts and during the wait (50 ms
wake granularity). Blocking filesystem syscalls cannot be preempted by that flag.
`with_lock` preserves the callback's return value; unwinding drops the guard.
`Guard::check` lets a caller verify authority again before writing after long work.
`Guard::release` reports failures; Drop attempts cleanup without hiding a panic.

Errors are path-free typed values. `InvalidTarget` and `InvalidSettings` map to status
2; `Busy`, `Cancelled`, `Unsafe`, `Changed`, `Io` and `UnsupportedIdentity` map to 75.
The consumer owns shell-compatible diagnostic framing and signal exit codes.
`Status` renders exactly `held`, `free`, or `unknown`. Probes never create, truncate,
flock, reclaim or delete anything. Existing flock files without a matching Linux
kernel row stay unknown, because absence from procfs is not evidence of freedom.
macOS existing flock observations stay unknown. Missing safe entries are free.

## Shared format and invariants

Mkdir acquisition uses atomic mkdir, then creates the legacy files in this order:

| File | First-line bytes | Concurrent call / crash result |
| --- | --- | --- |
| `owner` | Opaque process/generation discriminator | Second contender is held; a crash before PID publication can leave an indefinitely held partial generation |
| `pid` | Positive decimal native PID | Live owner remains authoritative regardless of age; confirmed missing PID can be recovered after generation revalidation |
| `process-start` | Linux `proc:` start time, macOS `ps:` normalized start time | A live PID with a positively different identity is stale; failed observation never establishes mismatch |
| `started` | Unix seconds | Legacy partial metadata with an owner and valid old timestamp can be recovered; malformed/missing timestamps do not imply expiration |

The directory and owner files must belong to the effective user, have no group or
other write bits, and metadata must be regular single-link files. Symlinks, hardlinks,
FIFOs, foreign ownership, unreadable/moving metadata and unexpected entries refuse
mutation. These are intentionally stricter refusals than unsafe legacy opens, so a
legacy group-writable root or owner file needs an explicit repair outside this crate.
Each first line has a 65,536-byte bound; larger or ambiguous input stays unknown/error,
never an empty successful observation. Unrecognized entries are checked before
moving any stale directory. No recursive directory deletion is used.

Stale recovery uses the same bakery queue suffix `.reclaim-queue`. Claim names begin
with decimal PID, POSIX cksum of the saved process-start bytes (or legacy `none`),
and unique generation material. Each claimant creates `choosing`, `generation` and
`ticket`, then publishes choosing zero. Numeric ticket and PID ordering matches the
legacy protocol; native recovery elections in the same process serialize per physical
root and lock name because native threads share a PID. Other state domains remain
independent. Missing/malformed live claim metadata blocks; known dead claims are
ignored, never deleted by another claimant. Ticket overflow refuses recovery.
If a live claimant withdraws its directory or unlinks metadata during observation,
the attempt returns `Changed` internally and acquisition retries with fresh root,
canonical generation and stale-owner proof, using the normal bounded backoff.
Nonblocking acquisition may return `Busy` for that invalidated attempt. Disappearance
never establishes death or grants permission to remove another claim. Claim directory
descriptors must match the named inode and pass ownership checks before metadata use.

Only the elected observer can rename the canonical generation to a unique `.stale.`
quarantine, after fresh inode, full owner metadata and stale-owner checks. Known
metadata is removed descriptor-relatively and the empty quarantine directory removed.
An interruption before rename leaves the old generation plus an ignorable dead claim;
a crash after rename can leave quarantine residue while a new canonical owner proceeds.
Queue roots and dead claim/quarantine residue are intentionally not garbage-collected.
Release checks both inode and owner generation and only removes known files from its
own directory. A changed root or generation leaves that directory untouched. A second
release cannot remove the next generation. As in the canonical protocol, namespace
mutation by another same-user actor between the last check and rename/unlink is not
an atomic compare-and-swap; arbitrary hostile same-user replacement needs a different
protocol and is outside a cooperative mixed transition.

Publication syncs each new file and its directory, and syncs the parent after successful
acquisition, reclaim and release. This does not turn advisory locks into durable state
transactions: the future writer still owns atomic data publication. A publication I/O
failure may leave conservative partial metadata. No persistent positive permission
cache or global umask change is used. Retained descriptors avoid reopening root and
lock directories during polling; stale-owner observations remain fresh on every attempt.
No NFS/HPC throughput or measured syscall-count claim is made.

Legacy records have no host or boot fields. PID/starttime proof is only appropriate
for local/cooperating hosts sharing the same process-identity namespace. This crate
adds no incompatible host fields and cannot prove foreign-host death. In particular,
it is not an admission for cross-host shared-storage writers. Legacy ownerless recovery
uses the `find -mmin +1` boundary (120 seconds), including the accepted stopped-acquirer
tradeoff. An owner-only crash record stays held indefinitely, exactly as legacy does.
These protocol limits require a parent architecture decision before any broader cutover.
An EPERM response to the PID existence query also stays ambiguous: acquisition returns
`Io` and probing returns `unknown`, even if a saved start identity might differ. Such
a record can remain wedged until observation becomes possible or an external authorized
repair resolves it; this component never infers death from a permissions error.

Native flock descriptors are CLOEXEC. There is no automatic inherited/borrowed lock
recovery from environment or arbitrary descriptors, and no global signal handler.
Unlike Bash's inherited descriptor, an unrelated exec child cannot extend native
ownership. Hold the guard until every protected child completes; thread panic unwinds
release, process abort/SIGKILL releases kernel flock and leaves a stale mkdir owner.
Do not move a guard across a raw fork; PID checks prevent child cleanup, but only a
consumer lifetime design can establish fork safety. Signal-driven graceful native
shutdown must arrange cancellation/unwinding in its own process; signal handlers
are not silently installed by a library. These explicit limits have parent approval
for this isolated API, not for any public consumer switch.
Replacing a flock inode while a contender retains its descriptor can return `Changed`;
the component fails closed instead of reopening through a potentially separate authority.

On macOS the only external query is `/bin/ps` with argv `-p`, validated decimal PID,
`-o`, `lstart=` and child-only LC_ALL C. Stdin and stderr are null; stdout is nonblocking
and capped at 256 bytes. The query has a two-second ceiling capped by the acquisition
(or probe) deadline, checks cancellation, and kills and reaps on failure, timeout or
cancellation. It removes CR bytes and normalizes the first line's C whitespace like
the canonical shell pipeline. Failed/empty/oversized observations never prove death.
No shell, Python, generic command execution or transform pipeline runs in production.

## Evidence and verification scope

Source anchors: canonical shell routing at `scripts/lib/file-lock.sh:43-150`, process
identity at `:190-253`, publication/stale rules at `:255-374`, bakery recovery at
`:376-507`, release/acquire at `:509-550`, callbacks/held locks at `:552-726`,
probe at `:728-794`, and waiting flock at `:796-843`.
[Native runtime state locking](../oms-runtime/src/io.rs) uses a separate lock
name. Its historical port anchor is `scripts/lib/oms_runtime/common.py:208-289`.
Canonical timing lives in `scripts/lib/poll.sh:5-75`. This implementation was checked
against the complete shell source and `tests/file-lock-boundary-smoke.sh`, plus the
nested callback and append/rewrite contention cases in `tests/scripts-smoke.sh:207-365`.

The native tests use temporary fixtures and compiled children in isolated process
groups. They cover actual flock and mkdir exclusion, SIGKILL recovery, lost-update
races, same-PID threads, generation/root replacement, non-inherited exec lifetime,
PID reuse, staged metadata crash residue, dead/live/malformed bakery claims and
numeric limits, permission/link refusal, metadata size edges, timeout/cancellation,
raw-byte routing and adaptive backoff. A compiled ps oracle checks exact argv and
locale, empty/exact-limit/oversized/malformed output, failure, hang, cancellation and
reaping. The host's real fixed ps query also runs on Linux, without claiming Darwin
identity parity. Direct macOS compilation and behavior remain unrun on this Linux host.

`tests/interop.sh NATIVE_FIXTURE_BINARY REPOSITORY_ROOT` is a development-only Bash
oracle using `examples/lock_fixture.rs`. It creates one synthetic temporary root,
checks byte-exact paths, bidirectional contention, native SIGKILL followed by legacy
recovery, and concurrent native/Bash updates (including initial stale mkdir recovery).
It never invokes OMS/provider commands or touches production locks. The example is
not a public command and requires a temporary-root sentinel.

The crate is new, so a complete implementation reversal removes its API and cannot
provide a meaningful pre-existing behavioral baseline. A new safety regression test
was observed failing behaviorally against the initial implementation: an unexpected
stale entry was renamed before refusal. Pre-rename validation fixes that path. No
compile-only reversal is claimed as mutual-exclusion evidence. Exact per-instruction
SIGKILL during native metadata writes and native recovery rename has not been fault
injected; staged residue and live child crash tests establish those bounded cases.
The disappeared-claim regression uses private test-only observation hooks at ten
open/read windows across both election passes, with and without a fresh live canonical
replacement. It fails behaviorally on the earlier implementation (`Io` or `Unsafe`)
and passes with retry handling; a reversed repair also reproduces that failure.
A separate compiled-fixture development proof releases four processes through an
observable start barrier against an initial stale mkdir generation; all must finish
80 updates without overlap, lost updates or acquisition errors. The same proof also
runs against flock. This stress proof supplements the deterministic regression;
stress success alone cannot distinguish the earlier race from its repair.
Full root workspace/admission checks remain the owning agent's responsibility.
