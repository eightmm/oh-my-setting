# Worker Roles

Use one strategy role per worker. Resolve project override, then global
override, then bundled default:

```bash
oms agent-role --repo . --name repo-auditor resolve
```

Standard roles: `repo-auditor`, `implementation-worker`, `test-designer`,
`patch-reviewer`, and `decision-advisor`. Prepend the resolved role text to a
native subagent brief; the parent still supplies task, paths, constraints,
success criteria, and expected output.

Put task-specific behavior directly in the brief; do not launch another model
to generate instructions. Roles specialize the task, never its authority.
Use `oms consult --to PROVIDER` for an independent read, or
`oms peer-delegate --read-only --role repo-auditor` for a clean-HEAD audit.

Soul executors are retired. Old records remain evidence, not runnable contracts.
Preserve their patches and approvals; create a fresh reviewed plan task rather
than stripping the old executor binding. Normal plan leases, scoped admission,
one-shot repair and one-use approval checks remain in force.
