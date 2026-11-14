# Native reference producer

This standalone crate supplies native local reference operations, managed
capture evidence and production, and a shared state-repository resolver. It
uses the existing operations lifecycle producer and task packet renderer;
it never invokes the Python/Bash implementation or executes stored commands.
The parent owns workspace/CLI registration, integration, review and release.

The migration is not complete: the checked lifecycle producer connection is
an outstanding prerequisite, and macOS execution remains unverified.
Capture collection is a separate gap described below; optional reuse awaits
the parent's authenticated plan/panel producer composition.
Successful compilation does not establish behavioral or security acceptance.

## Callable consumers

| Consumer | Native entrypoint and contract |
| --- | --- |
| Task `Services::begin_reference` | `scope::task_operation` constructs an actual `OwnedReference` and returns the existing task `ReferenceOperation` trait object. The task crate remains independent of this crate. |
| Task `ReferenceOperation::validate` | Validates old and proposed packet bytes, arguments, source session, live capture ownership and the actual evidence namespace under the caller's packet lock. The first proven no-write refusal is terminal; a refusal after possible publication remains unresolved. |
| Task `ReferenceOperation::finish` | Preserves an existing terminal refusal, records actual completion or nonterminal failure through operations, then cancels and joins the monitor before closing its owned scope. Drop performs owned cleanup without inventing completion. |
| Task, plan and room state routing | `repository::Resolver::{state_repository,native_repository}` separates the physical working checkout from its validated shared state checkout. `reference_repository` handles FILE-only packets and explicit nested OMS namespaces. |
| Patch admission | `CaptureIndex::{admit,indexed_bytes,read_patch}`, `AdmissionRecords::load`, `record_admission` and `admission_proof` bind retained native anchors, the exact report/index receipt and the actual local producer lifetime. The caller holds the artifact-index lock when stamping admission. |
| Managed capture management | `producer::Producer::open` holds the canonical landings lock. `allocate`, `publish`, `seal` and `usage` reuse its fresh snapshot. Creation refuses child calls; quota counts uncertain generations conservatively. |
| Admission binding/reuse | `binding::Domain::{admission_binding,batch_prefix,bind_admission,current_admission,require_current_admission}` and `binding_digest` implement binding and freshness checks. `Producer::{intake,refresh_admission}` retain the canonical fresh path when optional context is unavailable. `AllocationError::exit_code` preserves quota status 3 versus input status 2. |
| Internal task append | `publication::append_deduplicated_unlocked` requires the actual local task operation and the caller's packet lock. It reuses task packet normalization, byte caps, pruning and receipt validation; note and receipt publish together. |
| Helper routing | `actions::reference_action` implements all reference helper actions, with flushed registration/readiness output, the `landing-reference:` error prefix, error status 2 and stop status 0/3. Invalid native argument counts have explicit native diagnostics rather than Python exception formatting. |

`scope::Protocol` requires a concrete checked lifecycle append function and a
native `IdentitySource`. `CheckedAppend` receives repository, event,
interruption flag and a validator over fresh rows/current attempt. The parent
must adapt `oms-command-operations::append_lifecycle_checked`, executing the
validator under that producer's own canonical lock before any idempotent
return or append. Ordinary lifecycle writes use `create_attempt` and
`append_lifecycle` directly. There is no second lifecycle writer.

## Reference action/source checklist

Every action below is callable through `actions::reference_action`; scope
actions require the checked producer and native identity prerequisites.

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
sole new dependency exception, only for macOS. Its sealed BSD-info flavor
rejects a returned-size mismatch before initializing the result. Native code
checks PID, own UID, zombie status, positive start seconds and microseconds
below one million, retaining canonical `darwin:seconds:microseconds` birth.
On zero return, thread errno is captured immediately: only ESRCH means exited;
unknown, permission, malformed and partial-record outcomes remain unproven.
No short-BSD/ps/PID-only fallback is used. Other hosts explicitly refuse native
ownership. This crate still forbids unsafe code; the approved dependency
provides the platform ABI. Its only transitive dependency is the already
pinned libc 0.2.191. macOS compilation, native linking, errno behavior and
runtime partial-record checks remain independent parent verification work.

This execution snapshot lacks `append_lifecycle_checked`. The parent supplied
and reviewed its exact two-file producer patch in commit
`896178854a668058a03b6f0f94defe73b1ebb7b4`; its source was inspected. Concrete
linkage requires the parent's combined source continuation. Until connected,
`Protocol` cannot be constructed with that native producer. An absent protocol
explicitly refuses owned helper actions. Ordinary lifecycle actions and
capture validation remain concrete producers.

Binding depends on authenticated panel dispatch grants and goal-batch lineage
(`panel_results.py:1279,1546`, `autopilot-receipt.py:1710`), whose native
producers are absent from the snapshot. The read-only typed `binding::Context`
methods expose exactly effective allowed paths, accepted prefix and goal-batch
prefix. Missing producers refuse a binding for real tasks; only an actually
empty task has the canonical empty prefix. Intake then allocates fresh and
still requires the full native admission witness. This integration boundary
was confirmed by the parent in `msg-eac66359fe6a42cab4652824ba0df303`.
Binding inventory retains the original source roots, environment exclusions,
seed files, Git base/tree/config/exclude and byte/count limits. It also binds
the actual executable digest into the opaque harness hash so native code
updates cannot reuse an old result merely because legacy files are unchanged.
That native hash-domain choice awaits the owner's explicit confirmation.

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
