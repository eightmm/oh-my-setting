# Roles and Executors

Use one strategy role per worker. Resolve project override, then global
override, then bundled default:

```bash
oms agent-role --repo . --name repo-auditor resolve
```

Standard roles: `repo-auditor`, `implementation-worker`, `test-designer`,
`patch-reviewer`, and `decision-advisor`. Prepend the resolved role text to a
native subagent brief; the parent still supplies task, paths, constraints,
success criteria, and expected output.

Use a task-scoped executor soul only for substantial bounded write delegation:

1. Ask a read-only child to propose behavior using `prompts/executor-soul.md`.
2. Create metadata with provider, strategy, plan task, path scope, base commit,
   and verify command.
3. Validate and freeze the soul hash.
4. Inject `oms agent-executor brief --id ID` into the write worker.

```bash
oms agent-executor --repo . create --id ex1 --provider codex \
  --strategy implementation-worker --plan-task t1 --model-class auto \
  --soul-file proposal.md
oms agent-executor --repo . validate --id ex1
oms agent-executor --repo . freeze --id ex1
```

`SOUL.md` controls behavior only. `meta.json` is authoritative for provider,
resolved model class/model/fallback, mode, task/lease, base SHA,
allowed/forbidden paths, and verification. The executor must not widen those
fields or delegate again. Repair rounds reuse the same frozen soul and route.
