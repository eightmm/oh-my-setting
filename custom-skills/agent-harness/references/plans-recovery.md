# Plans and Recovery

Use `agent-plan` only when work genuinely has parallel or dependent subtasks.
Each claim has a lease; a reclaimed lease invalidates stale workers.

```bash
oms agent-plan --repo . add --id t1 --title "Fix parser" --verify "bash scripts/check.sh"
oms agent-plan --repo . next --claim --provider codex
oms agent-plan --repo . touch --id t1
oms agent-plan --repo . reclaim
oms agent-plan --repo . reclaim --include-review
```

Check known dead ends before repeating them:

```bash
oms fail-ledger --repo . check --cmd "bash scripts/check.sh"
oms fail-ledger --repo . record --cmd "..." --exit 1 --summary "root cause"
oms fail-ledger --repo . resolve --fingerprint <id>
```

Delegation liveness markers couple a worker PID to its captured plan lease.
Never refresh a reclaimed lease from current shared state.

Recovery sequence:

1. Run `oms state --repo .` and inspect stale claims, reviews, guards, workers,
   open runs, CI, and unresolved failures.
2. Run `oms gc --repo .` and review the dry-run.
3. Run `oms gc --repo . --apply` when the listed cleanup is in scope.
4. Run `oms agent-plan --repo . reclaim --include-review` if review work needs
   explicit requeueing.

GC must preserve active tasks, live workers, open runs, frozen/running
executors, and unresolved failures.
