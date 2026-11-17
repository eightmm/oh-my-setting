# oms-command-room

The native room adapter exposes `owns`, `run` and `run_with_services`, backed by
`Room`. `owns` accepts malformed argv for this command family. Room creation is
`new` at the CLI and `Room::create` in process. Membership, scope declarations,
updates cursors and acknowledgments confer no approval or admission authority.

`tests/room_contract.rs` exercises ownership/help, creation/show/join/send,
`updates --after` boundary semantics, explicit idempotent acknowledgment, scope
validation/clear and `LocalServices::state_repository`. Every scenario runs in
an isolated child environment and a unique canonicalized initialized Git
repository. Its empty operations read guard is a test fixture only.

## Wave 3 integration

`Services` is the parent-owned boundary for canonical shared repository
resolution, provider names, current Claude identity, a consistent operations
projection and app delivery. `LocalServices` uses a fixed Git root query and
native provider normalization; no Python/script runtime is called. It refuses
recognized managed scratch roots outside a child session and inherited Git
redirection. Current Claude identity, operations and delivery remain unavailable
by default. Production must supply real producers via `run_with_services`.
Operations guards must fence concurrent lifecycle writes through publication,
with room-before-producer lock ordering. The tests keep those unavailable
services fail-closed and inject an empty read guard for local lifecycle cases.

The installed native `oms room` entry and panel room calls now supply the
compiled collaboration services. Operations are projected through the canonical
producer while holding its writer lock, in original creation order; the guard
also checks the snapshot before room publication. Shared-state resolution uses
the same explicit state-root contract as the control plane.

`join --replaces PARTICIPANT --native-session ID` is an explicit Main resume.
Under one Room append lock it validates the previous joined Main, the same
provider/session digest, a terminal exact previous lifecycle and a fresh working
Main lifecycle in original creation order. The operations guard remains pinned
through publication. Both lifecycles must name this Room and native session.
Joined descendants refuse replacement so their parent and scope blockers remain
intact. One `main-resume` event marks the previous participant left and enrolls the new
attempt; replay is idempotent. This release proof grants no usage settlement,
whole-tree cleanup, team admission or publication authority. Historical joins
without `replaces` retain their existing interpretation. The new event kind makes
older writers refuse the log rather than silently retain two joined owners.

Room publication uses the existing native Codex/Claude notification producers.
Codex targets its saved receiver; Claude requires the enrolled recipient and
explicit `--wakeup` authority. Submission, uncertainty, turn identity, persistence
and UI-observation fields pass through unchanged. An acknowledgment is not proof
that an agent consumed the text or that the user approved it. The library's
`LocalServices` defaults, and a bare binary without validated source metadata,
still refuse integrations that have no producer. Tests do not send live messages.

Panel result notification uses the saved Codex receiver without granting Claude
wakeup authority. Panel publication finalization retrieves the exact raw receipt
through the plan producer's existing gate/push/CI and target-reachability checks,
using the required SHA-256; a commit alone never becomes a receipt.

No room production writes changed in W2. Existing room logs use locked append;
a second caller revalidates under the lock. A kill can leave an incomplete tail,
which subsequent writes refuse pending reconciliation. The CURRENT pointer uses
atomic publication; interruption can leave its private temporary file. Updates
are reads, not acknowledgments; repeated ack deduplicates consumed identities.
Test fixtures use unique temporary roots and non-panicking removal; a killed
test may leave its private temporary repository.

## Scoped append and polling optimizations

`append_batch_at` stages carry rows and validates each against earlier staged
rows while retaining producer guards through one locked append write and data
sync. A validation failure publishes no staged rows. Concurrent cooperating
writers revalidate after acquiring the same lock. A kill during publication can
leave a complete prefix or incomplete final turn; prior bytes are preserved and
incomplete evidence still requires reconciliation.

Cached snapshots read only appended JSONL on growth after checking the first
256 bytes and prior cursor boundary. A polling reader retains its descriptor:
unchanged ticks perform metadata checks without byte reads, JSON parsing or
new delivery allocations. Replacement, truncation or changed boundaries reload
the stream; cursors still refuse stale generations. The 50 ms poll cadence and
read-only cursor contract are unchanged. Polling never creates log/cache files.

Outbound scans reuse one bounded process-environment snapshot via `OnceLock`.
Append scans original text before truncation and avoids a redundant text scan
when the published text is identical; metadata and changed text still pass the
clean-content boundary. Embedders must establish their environment before the
first outbound scan.

The `src/storage.rs` unit module measures idle and incremental read bytes and covers
replacement, malformed tails and file-size boundaries. the `src/core.rs` unit module
covers staged refusal and append idempotence; these are the previously untested
private storage/batch paths. Existing `tests/room_contract.rs` remains intact.
