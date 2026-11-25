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
Panel acceptance now reconciles accepted write WIP through its native service
after canonical review and component registration. The service revalidates the
saved acceptance, original operation/attempt/task binding and current owner/host;
sealed execution cleanup remains required. Its optional `resource_retirement`
observation distinguishes pending or unknown custody from the already accepted
commit. For a recorded acceptance, repeat `oms panel --accept TASK` with its
original `--repo`, `--owner`, `--message`, `--verify` and exact
`--expected-patch-sha256`. The CLI prints that retry command for pending or unknown
custody. It revalidates canonical acceptance and current commit/tree/landing
evidence, then retries only settlement; incomplete acceptance still refuses.
See the [panel acceptance contract](../oms-panel-core/README.md#composed-acceptance)
for the complete command and composition digest. It also retries withdrawn Team
retirement after releasing its guards;
active contracts retain their slots. Lost-owner/host recovery and persistent
session settlement remain unsupported until those producers expose exact owned
bindings; this API never accepts caller-supplied done/cleanup Booleans.


## Exact external waits and experiment continuation

The native autonomy host uses `external::observe` for one schema-1 wait relation
in the existing autopilot receipt. CI, Slurm, SSH and experiments remain the
record owners; there is no second operation ledger. A relation binds these
identities:

| Source | Exact identity |
| --- | --- |
| CI | repository, local HEAD, workflow ID, run ID and run attempt |
| Slurm | submission key and attempt, cluster and allocated job (or empty job before allocation) |
| SSH | operation ID/tag, command digest and execution snapshot |
| Experiment | experiment/run ID, frozen contract digest and request digest |

Pending/running/unknown observations suppress generic unfinished-task wakeups
for the waiting owner. A fresh confirmed terminal result gives each currently
verified Main attempt one cue, across days; only the autopilot receipt CAS
consumes that exact result. Main replacement preserves result identity and
consumption. Parent stop suppresses both external and generic wakes while
keeping evidence and spent budgets. Existing idle, live-child, pane custody and
six-wakes-per-day guards still apply. Missing or unsafe observations never
become a ready signal, and refresh never submits or cancels work.

CI uses bounded GET-only run/attempt/jobs reads, validates repository/head/job
identity, checks the run again after the jobs and rechecks local HEAD/origin
before retaining a compact row in `.oms/ci.jsonl`. All GETs share a 20-second
observation deadline; lock contention refuses. A newer rerun invalidates the
old attempt. It does not claim all workflows are healthy. SSH `exec`,
`reconcile`, `cancel` and `probe` use `--timeout-seconds` as a transport wall
limit, including a connected peer that hangs. A local timeout leaves remote
custody unknown; it is never proof that a remote job was cancelled.

`runtime experiment next --id ID` is a read-only projection over the registered
contract and existing run/budget streams. It prioritizes an unresolved exact
run, then the latest evidence for each preregistered arm/seed. Missing cells
produce Main planning input; no command from another cell is replayed. The
adaptive consumer carries that exact evidence into its existing reviewed
proposal and Lead task path. Invalid/missing/NaN metrics remain inconclusive;
a valid negative result completes its registered hypothesis and can inform a
next-hypothesis proposal. Contracts and original budgets remain immutable.
A cell earns useful progress once per experiment/contract/arm/seed, even when
a repeat generates a new run ID or slightly different metric values. Exact
result hashes still distinguish the measurements.

`runtime experiment checkpoint --id ID --run-id RUN --manifest FILE` checks
registered run identity, frozen code/data/config/evaluation declarations,
seed, declared step/RNG digest and the exact regular checkpoint bytes. It never
deserializes a model or proves application optimizer/RNG restoration. The
checked Slurm template requires OMS and the canonical/shared project state on
the execution host; Git-only bundle deployment does not supply those. See
[the checkpoint contract](../../templates/ml-docs/CHECKPOINTS.md#checked-slurm-resume)
for manifest fields, bounds and the separate remote staging path.

Before a detached autopilot wake, tick retains an exact launch ID and input
receipt digest. The real CLI records matching started/finished markers in the
existing progress stream. A missing acknowledgement or start without finish
never causes another send, even if the receipt advanced or the day changed.
Those markers prove CLI entry/completion only, not provider delivery or goal
acceptance.

Planning and handoff consume source-checked journal outcomes and the existing
query-relevant memory view as advisory context. Facts must match the current
repository HEAD/tree, PROJECT digest and a clean source/current worktree;
external facts are revalidated against the exact retained result. Changed
code/contract or unavailable source evidence withholds the fact. Memory's own
source/curation/retirement rules remain in force. These observations never
change provider policy, scope, acceptance or resource authority.
