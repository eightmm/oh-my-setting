# Native reference producer

This standalone crate supplies native local reference operations, managed
capture evidence and production, and a shared state-repository resolver. It
uses the existing operations lifecycle producer and task packet renderer;
it never invokes the Python/Bash implementation or executes stored commands.
The parent owns workspace/CLI registration, integration, review and release.

The checked lifecycle producer is connected to the actual operations API.
macOS execution remains unverified, and its default executing-artifact proof
requires the parent's authenticated native bootstrap composition.
Capture collection is a separate gap described below; optional reuse awaits
the parent's authenticated plan/panel producer composition.
Successful compilation does not establish behavioral or security acceptance.

## Callable consumers

| Consumer | Native entrypoint and contract |
| --- | --- |
| Task `Services::begin_reference` | `scope::native_task_operation` uses the production native factory; `task_operation_with_controller` shares a command controller snapshot. Existing `task_operation` accepts an explicitly supplied protocol. All return the existing task `ReferenceOperation` trait object. The task crate remains independent of this crate. |
| Task `ReferenceOperation::validate` | Validates old and proposed packet bytes, arguments, source session, live capture ownership and the actual evidence namespace under the caller's packet lock. The first proven no-write refusal is terminal; a refusal after possible publication remains unresolved. |
| Task `ReferenceOperation::finish` | Preserves an existing terminal refusal, records actual completion or nonterminal failure through operations, then cancels and joins the monitor before closing its owned scope. Drop performs owned cleanup without inventing completion. |
| Task, plan and room state routing | `repository::Resolver::{state_repository,native_repository}` separates the physical working checkout from its validated shared state checkout. `reference_repository` handles FILE-only packets and explicit nested OMS namespaces. |
| Patch admission | `CaptureIndex::{admit,indexed_bytes,read_patch}`, `AdmissionRecords::load`, `record_admission` and `admission_proof` bind retained native anchors, the exact report/index receipt and the actual local producer lifetime. The caller holds the artifact-index lock when stamping admission. |
| Managed capture management | `producer::Producer::open` holds the canonical landings lock. `allocate`, `publish`, `seal` and `usage` reuse its fresh snapshot. Creation refuses child calls; quota counts uncertain generations conservatively. |
| Admission binding/reuse | `binding::Domain::{admission_binding,batch_prefix,bind_admission,current_admission,require_current_admission}` and `binding_digest` implement binding and freshness checks. `Producer::{intake,refresh_admission}` retain the canonical fresh path when optional context is unavailable. `AllocationError::exit_code` preserves quota status 3 versus input status 2. |
| Internal task append | `publication::append_deduplicated_unlocked` requires the actual local task operation and the caller's packet lock. It reuses task packet normalization, byte caps, pruning and receipt validation; note and receipt publish together. |
| Helper routing | `actions::{run,run_with_services}` construct the actual native protocol only for scope actions when no protocol is supplied. `reference_action` retains the explicitly injected route. All actions keep flushed registration/readiness output, the `landing-reference:` error prefix, error status 2 and stop status 0/3. Invalid native argument counts have explicit native diagnostics rather than Python exception formatting. |

`scope::native_checked_append` calls the actual
`oms-command-operations::append_lifecycle_checked`. `Protocol::production()`
captures native controller identity and selects that adapter plus
`NativeIdentity`; `production_with_controller` shares a previously captured
command snapshot. The operations producer takes its own canonical lock, loads
rows/projection once, calls the validator before idempotent reuse or append,
then preserves its normal sequence/transition/write behavior. Scope callbacks
do not reacquire this lock. Ordinary lifecycle writes still use
`create_attempt`/`append_lifecycle`; there is no second writer.

The existing `CheckedAppend`, `Protocol::{new,for_executable,native}` and
`task_operation` injection signatures remain available. `Protocol::new` is a
trusted service-injection boundary: a hex string is not native authentication.
Production bootstrap should use `with_controller`/`production_with_controller`
with an actual authenticated retained descriptor, never a constant digest.

The additive production signatures are:

```rust,ignore
native_checked_append(&Path, V, &AtomicBool, &mut Validator<'_>) -> Result<V>
Protocol::production() -> Result<Protocol>
Protocol::production_with_controller(Controller) -> Result<Protocol>
native_task_operation(&Path, &Path, Arc<AtomicBool>) -> TaskResult<Box<dyn ReferenceOperation>>
task_operation_with_controller(Controller, &Path, &Path, Arc<AtomicBool>) -> TaskResult<Box<dyn ReferenceOperation>>
actions::run(&str, &Path, &[OsString], &mut dyn Write, &mut dyn Write, &AtomicBool) -> i32
actions::run_with_services(&str, &Path, &[OsString], Option<&Protocol>, &mut dyn Write, &mut dyn Write, &AtomicBool) -> i32
```

Here `TaskResult` denotes `oms_command_task::Result`. Task construction checks
controller identity before private scope/lifecycle publication, registers the
actual local attempt, starts the owned monitor and requires real readiness
before returning the trait object. Validation still runs under the caller's
packet lock before publication; finish and Drop retain their existing cleanup
and nonterminal uncertainty rules.

## Reference action/source checklist

Every action below is callable through `actions::{run,run_with_services}` or
the explicit `reference_action` route. Scope actions require positive native
controller and owner proofs; other actions do not invent that prerequisite.

| Legacy action | Native implementation | Canonical source |
| --- | --- | --- |
| `begin` | `publication::begin` | `scripts/lib/landing-reference.py:31` |
| `begin-owned` | `Protocol::begin_owned` | `scripts/lib/landing-reference.py:115` |
| `monitor` | `Protocol::monitor` | `scripts/lib/landing-reference.py:182` |
| `await-monitor` | `Protocol::await_monitor` | `scripts/lib/landing-reference.py:248` |
| `stop-monitor` | `Protocol::stop_monitor` | `scripts/lib/landing-reference.py:373` |
| `close-scope` | `Protocol::close_scope` | `scripts/lib/landing-reference.py:295` |
| `finish` | `publication::finish` | `scripts/lib/landing-reference.py:414` |
| `task-append` | `append_deduplicated_unlocked` | `scripts/lib/landing-reference.py:503` |
| `admission` | `capture::record_admission` | `scripts/lib/landing-capture.py:625` |
| `task` | `CaptureIndex::{validate,validate_write}` | `scripts/lib/landing-reference.py:452` |
| `admit` | `CaptureIndex::admit` | `scripts/lib/landing-reference.py:585` |

Additional capture anchors are `landing-capture.py:58` (bounded reads),
`:156` (reservations), `:187` (private owner certificate), `:276` (quota),
`:348` (allocation), `:553` (retained anchors), `:560` (publication),
`:597` (report/index records), `:660` (terminal admission proof),
`:690` (sealing provenance), `:748` (publication reads), `:777` (managed
index reads) and `:797` (reference spellings). The native quota reader
validates existing collector release receipts; it grants no deletion right.

Resolver anchors are `scripts/lib/room_repository.py:13,77,104,135,143`,
`scripts/lib/agent-memory-common.sh:39`, `scripts/lib/harness-residue.sh:125`
and `scripts/scratch-worktree.sh:134`. Legacy and suffix-bound worktree
names are recognized. Marker type, bounded single-link bytes, managed
location, physical worktree binding, actual Git registration/common directory
and the original main checkout's ownership of that common directory must
all agree. Inherited room/execution variables alone never establish ownership;
foreign reads require explicit opt-in and children keep local state.
The source resolver does not check marker process ancestry; the parent
explicitly confirmed retaining that contract in room message
`msg-bb595ab73fd94097918a33ee6282178b`.

## Ownership, concurrency and interruption

| Written record | Concurrent call or kill residue |
| --- | --- |
| Lifecycle attempt/spec and rows | Only the existing operations producer writes these. Crashes between registration or state transitions retain a queued/starting/working/verifying/review or blocked attempt. Only proven no-write refusal or actual completion requests terminal state. The producer owns its existing lock, spec and trace protocol. |
| Private scope, pending fence, result, ready and proof | An immutable scope binds owner birth, directory/channel inode, pending-fence inode, helper digest, input and lifecycle registration. Readiness links the exact pending inode before removing the fence; readers wait for that removal. A partial startup or replaced auxiliary remains private residue. Scope removal and pulses share the lifecycle lock. Native in-process scope actions cannot be used to signal the owning process. |
| Monitor | Shell/helper mode proves the native foreground parent and its exact monitor generation. Linux forced shutdown uses a revalidated pidfd, never a bare PID/group. In-process mode retains and joins its OS thread; it cannot survive the owning process. Cancellation stops acquisition; the one-second budget applies to acquisition only. No timer truncates normal command output. |
| Task packet and append receipt | The caller's packet lock serializes cooperating calls. Original bytes and inode generation are compared before atomic replacement; note content is rechecked. A kill leaves the complete old or new packet and possibly the safe-fs producer's own private staging file. Publication errors remain uncertain and must not be blindly retried. Exact replay is byte-preserving; conflict, malformed metadata and receipt exhaustion refuse. |
| Landing reservation journal | The landings guard serializes cooperating producers. One bounded append is fsynced without rewriting the ledger; the opened generation and resulting size must match the command snapshot. A short write or sync failure is uncertain. A reservation is durable before its directory or payload exists, so interruption remains charged. |
| Private generation, payload and retained anchors | Exclusive mode-0700 directories and mode-0600 files are recorded by device/inode. A kill before inventory publication leaves an incomplete charged generation. Matching bytes or filenames never retroactively grant ownership. Unknown link counts and changed owners/namespaces refuse. |
| `owner.json` and admission witness | Exact old bytes and generation are compared before safe-fs atomic replacement and directory fsync. Concurrent stale saves fail; a kill leaves the old or new complete certificate and possibly a private staging file. A durable terminal row can precede its certificate seal; that gap grants no collection right. |
| Shared publication | The private publication anchor and its certificate record precede a no-clobber hardlink into the checked repository namespace. A collision leaves the existing occupant intact. A kill can leave a recorded single-link anchor or a complete two-link publication; both remain charged until independently proven collectible. |
| Controller snapshot | Read-only retained descriptors and one digest live only in command memory. Clones share the snapshot, and positional reads avoid duplicated-descriptor cursor races. A second command obtains its own fresh snapshot; there is no persistent/TTL cache or file to clean up. A changed generation or kernel identity refuses scope construction/use and disables optional reuse. A kill closes descriptors and cannot publish a successful receipt. |

Reads and writes check named namespaces before/after access and on absence.
Leaf reads use no-follow descriptors, owner/link/type/size checks and full
generation comparisons. Repository reads/mutations reuse `oms-safe-fs`.
Same-owner group write is allowed; world write is refused. External reader
ancestry aliases are physically resolved and rechecked, while private scope
and capture roots must keep their exact physical spelling.

## Remaining interfaces and coverage gaps

Linux ownership uses native procfs state, owner UID, boot identity and start
ticks. macOS uses the safe `proc_pidinfo` 0.1.4 binding, explicitly authorized
by the parent in room message `msg-c4033d8d5b694bab9f40c3deb5e2742e` as the
initial new dependency exception, only for macOS. Its sealed BSD-info flavor
rejects a returned-size mismatch before initializing the result. Native code
checks PID, own UID, zombie status, positive start seconds and microseconds
below one million, retaining canonical `darwin:seconds:microseconds` birth.
The additionally authorized macOS-only existing pin `nix` 0.31.3 uses no
default features and provides safe errno access. Thread errno is cleared
immediately before the BSD-info call and captured immediately after it, before
any result processing. On zero return, only fresh ESRCH means exited;
unknown, permission, malformed and partial-record outcomes remain unproven.
No short-BSD/ps/PID-only fallback is used. Other hosts explicitly refuse native
ownership. This crate still forbids unsafe code; the approved dependency
provides the platform ABI; the standalone lock retains the existing libc
0.2.191 and the existing pins needed by nix. macOS compilation, native linking,
errno behavior and runtime partial-record checks remain independent parent
verification work.

The actual operations producer is now linked through the adapter above
(`oms-command-operations/src/events.rs:119`). Its callback runs after the
canonical lock and fresh projection, before idempotent reuse or mutation.

`controller::Controller::capture_current` opens a real procfs directory on
Linux, verifies its filesystem type and kernel `self` PID-namespace binding,
then opens the process's fixed kernel executable link relative to retained
directory descriptors. It hashes that retained image once and checks its
generation and relationship to a fresh kernel executable descriptor before
use. A named executable replacement is never opened; an already unlinked
executing image can be proven by its kernel descriptor without adopting a
filename. Metadata changes during the command conservatively invalidate the
snapshot. The snapshot rejects reuse in another process after a fork.

On macOS, native process birth proof is implemented separately from executing
artifact proof. No available safe API here proves that an ordinary named file
is the loaded controller image, so default controller capture refuses.
`Controller::from_authenticated_descriptor(File, Arc<dyn ExecutingImage>)`
is the typed native bootstrap seam: its provider must establish the actual
descriptor-to-executing-image relationship using kernel facts on every check.
It is not a path/hash assertion or peer authority. Without such a genuine
provider, optional intake allocates fresh, while mandatory scope construction
explicitly refuses before registration. macOS bootstrap proof, native linking
and runtime behavior remain parent composition/verification gaps.

Binding depends on authenticated panel dispatch grants and goal-batch lineage
(`panel_results.py:1279,1546`, `autopilot-receipt.py:1710`), whose authenticated
production composition remains parent-owned. The typed `binding::Context`
methods expose exactly effective allowed paths, accepted prefix and goal-batch
prefix. Missing producers refuse a binding for real tasks; only an actually
empty task has the canonical empty prefix. Intake then allocates fresh and
still requires the full native admission witness. This integration boundary
was confirmed by the parent in `msg-eac66359fe6a42cab4652824ba0df303`.
Binding inventory retains the original source roots, environment exclusions,
seed files, Git base/tree/config/exclude and byte/count limits. It also binds
the verified executing-artifact digest into the opaque harness hash so native code
updates cannot reuse an old result merely because legacy files are unchanged.
The owner-approved recipe is exactly
`SHA256(encoded([canonical_inventory, verified_native_digest]))`; all JSON
keys/version and canonical inventory/proofs remain intact. Old bindings miss.
Compound native commands can capture one `Controller`, clone it into
`Protocol::production_with_controller`, and pass the same snapshot to
`Domain::{admission_binding_with_controller,bind_admission_with_controller,require_current_admission_with_controller}`
and `Producer::intake_with_controller`. Digest access rechecks kernel identity
and generation without hashing the image again. Default convenience methods
capture one snapshot for their command; no controller failure is a reuse pass.

This crate does not implement verified Git application, collection/quarantine
or retirement/delegation projections. No existing object is quarantined or
deleted by its capture producer. Those wider migration domains remain with
their actual owners and must not be mistaken for native producer completion.
Legacy reference/capture and their direct durable/process/retirement imports
were read through EOF. Batch/panel dependencies were outlined and the binding
call sites inspected; their wider implementations and retirement's dynamic
decision dependency have not been ported or exhaustively read.

Existing coverage was inspected in `tests/functional-evolution-smoke.sh`
(managed ownership, quota, pins and producer receipts) and
`tests/scripts-smoke.sh` (readiness fencing, generation changes, contended
shutdown, foreign readiness and dedupe receipts). These tests remain unchanged.
The worker phase permits compile/fmt, source/reference checks and outbound
hygiene only: no test additions, suites, behavior probes/reversal rounds,
cross-host execution, CI or installation have run. Parent integration and
independent security review remain required.

Deferred checks specific to this continuation: real checked-append refusal
before idempotent reuse, callback sequence/lock ordering, interruption and
contended cleanup; production/injected action and task factories; stale ESRCH
versus freshly cleared zero/ESRCH/permission/partial macOS results; executing
image rename/unlink/replacement, absent or wrong procfs/PID namespace,
inherited/forked snapshots, descriptor cursor races, empty/truncated/growing
image reads and exact generation-size boundaries; one digest shared across
compound consumers; controller/context failure taking fresh intake while
mandatory scope construction refuses. Existing source tests are unchanged;
none of these runtime cases was executed in this implementation-only phase.
