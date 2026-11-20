# oms-runtime

The native runtime shares one parser and router across CLI, panel and MCP
consumers. Host adapters supply canonical producer facts; missing adapters
refuse without running a legacy fallback.

`oms runtime resources host` reads the actual kernel boot generation SHA-256.
`resources status` observes the selected shared state repository's explicit
`repository-host-local-observed` policy and separate Team, invocation, open
process/session and write-WIP counts. `configured: false` has null policy and
occupancy; it does not activate a default. `active` is the recorded domain state,
and `host_matches_policy` separately identifies a stale host binding. Neither
observation supplies a launch permit.

Status also returns the exact dispatch `snapshot_sha256`, retained Team slots
and write operations with their custody revisions. `execution_pending` means
invocation/process custody remains unresolved; `candidate_pending` is separate
write WIP. These observations do not infer cleanup from a missing PID or a
withdrawn Team contract.

`resources activate --policy PATH` explicitly activates a policy before Main
starts. Relative policy paths use the caller's directory, then must remain
inside the resolved physical execution repository. The native producer accepts
only an owned, single-link regular file without symlink traversal, at most
64 KiB, and requires its host generation to match `resources host`. The policy
must explicitly specify every finite limit and reviewer headroom; the CLI
supplies no capacity defaults. A dispatch snapshot CAS refuses changed state
or unreconciled legacy execution/usage history and never deletes it. The boot
proof and mutation writer share an absolute deadline; root resolution happens
before that producer deadline.

Policy JSON requires `schema: 1`, `kind: "oms-resource-policy"`, the scope above,
nonempty `domain` and `epoch`, and the observed `host_generation_sha256`.
`max_teams`, `max_invocations`, `max_open_processes` and `max_write_wip` must be
positive integers. `reviewer_headroom` is a nonnegative integer strictly below
`max_invocations`. Unknown fields, unsupported scopes and malformed JSON refuse.

`resources hold --expected-policy-sha256 SHA` blocks new admission for the exact
current policy while preserving independent custody, usage and settlement.
All mutations refuse harness children and delegated descendants. Host and
status remain read-only for children. The common `--repo` and `--pretty` options
are supported.

`resources discard-write --operation-id ID --expected-revision N --reason TEXT`
records an explicit candidate disposition from the currently enrolled Main.
The native producer validates the live Main creation generation, Room, retained
Team contract and exact physical write binding under producer guards. It refuses
while any call in the operation retains execution custody. Success releases WIP
without deleting dirty worktrees, partial patches, logs or usage receipts, and
without marking a Plan task accepted or done. The returned `decision` names an
immutable native record; callers cannot submit an evidence file as authority.

`resources retire-team --team-id ID --team-spec-sha256 SHA --expected-revision N`
releases the exact contract generation only after canonical withdrawal or
supersession by a newer retained TeamSpec and independently drained execution
and candidate custody. `agent-plan team-cancel` automatically tries this after
publishing withdrawal and releasing its producer locks. Its response adds
`resource_retirement` to that Team's returned record: `retired`, `pending`, or
`unknown`. The projection is not persisted as a new Plan lifecycle phase.
Successful `discard-write` retries retirement for its withdrawn/superseded Team
and reports `team_retirement`; an active contract keeps its Team slot.

Both release commands preserve exact owner/host generations and use revision
CAS. An identical decision/revision retry is idempotent; a changed disposition,
stale revision or foreign owner refuses. Old-Main or previous-host unknown
custody cannot be taken over by these commands.

`resources reconcile --policy PATH --expected-snapshot-sha256 SHA --reason TEXT`
resumes an exact held, fully drained domain into a new policy epoch on the
observed host. The existing checked store rejects reused epoch names, a changed
domain or nonzero Team/invocation/process/WIP occupancy. Previous policies,
physical intents, retirement decisions and usage liabilities are retained.
This command does not reset unknown execution or refund unknown cost.

Active member cancellation still belongs to the original Supervisor/transport.
`team-cancel` withdraws admission and does not promise to kill processes or
produce cleanup evidence. After the owning execution producer supplies sealed
cleanup, use the native release flow for remaining candidate/Team custody.
Automatic accepted-candidate retirement, lost-owner/host recovery and persistent
session settlement remain unsupported until those producers expose exact owned
bindings; this API never accepts caller-supplied done/cleanup Booleans.
