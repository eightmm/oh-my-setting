# Plans and Recovery

Use `agent-plan` only when work genuinely has parallel or dependent subtasks.
Each claim has a lease; a reclaimed lease invalidates stale workers.

For a reviewed plan, inspect `oms agent-plan --repo . status --json` before
claiming. `contract.satisfied=false` means the current `PROJECT.md` no longer
matches the reviewed bytes/state. Restore or review the contract; do not bypass
the empty `actionable` list. A worker holding an existing exact lease may still
advance or release it, because cleanup authority is distinct from a new claim.

```bash
oms agent-plan --repo . add --id t1 --title "Fix parser" --verify "bash scripts/check.sh"
oms agent-plan --repo . next --claim --provider codex
oms agent-plan --repo . touch --id t1
oms agent-plan --repo . reclaim
oms agent-plan --repo . reclaim --include-review
```

When the active plan is stale because the same goal was completed outside its
task transitions, retire its exact projection instead of calling `finish`,
`block`, or `init` to make the display look current:

```bash
oms agent-plan --repo . retire
oms agent-plan --repo . retire --apply \
  --expected-plan-sha256 <sha-from-check> \
  --disposition completed-external \
  --reason "equivalent implementation is committed on the current HEAD"
```

The apply call runs a new plan acceptance itself; an earlier passing row is not
proof. It requires a clean committed tree and fails closed around active task
authority or worker markers. For a reviewed replacement contract, only an old
plan whose tasks are all `done` may use `--disposition superseded`; that receipt
states that it did not verify completion. Replay the identical command after
an interruption. The content-addressed archive/receipt pair makes replay
idempotent and prevents an old replay from unlinking a newly-created live plan.
Do not mutate the same plan around a residual retirement intent: canonical
mutation commands fail closed until exact replay finishes. A later reviewed
topology apply cleans only a fully validated post-unlink intent when no active
plan remains (or a different nonempty lineage is already active).

Check known dead ends before repeating them:

```bash
oms fail-ledger --repo . check --cmd "bash scripts/check.sh"
oms fail-ledger --repo . record --cmd "..." --exit 1 --summary "root cause"
oms fail-ledger --repo . resolve --fingerprint <id>
```

Delegation liveness markers couple a worker PID to its captured plan lease.
Never refresh a reclaimed lease from current shared state.

Recovery sequence:

1. Run `oms inbox --repo .` for the ranked attention queue; use `oms state
   --repo .` when the full underlying state is needed.
2. Run `oms gc --repo .` and review the dry-run.
3. Run `oms gc --repo . --apply` when the listed cleanup is in scope.
4. Run `oms agent-plan --repo . reclaim --include-review` if review work needs
   explicit requeueing.

GC must preserve active tasks, live workers, open runs, frozen/running
executors, and unresolved failures.
