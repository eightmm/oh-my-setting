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

No room production writes changed in W2. Existing room logs use locked append;
a second caller revalidates under the lock. A kill can leave an incomplete tail,
which subsequent writes refuse pending reconciliation. The CURRENT pointer uses
atomic publication; interruption can leave its private temporary file. Updates
are reads, not acknowledgments; repeated ack deduplicates consumed identities.
Test fixtures use unique temporary roots and non-panicking removal; a killed
test may leave its private temporary repository.
