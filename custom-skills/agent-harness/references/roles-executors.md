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

Use a task-scoped executor soul only for substantial bounded write delegation.
Executors do not have a read mode; use `oms agent-run --mode read` for an
independent read-only pass.

1. Ask a read-only child to propose behavior using `prompts/executor-soul.md`.
2. For a plan task, claim it with the intended provider first; executor creation
   rejects an empty or different-provider lease.
3. Create metadata with provider, strategy, plan task, path scope, base commit,
   and verify command.
4. Validate and freeze the soul hash.
5. Inject `oms agent-executor brief --id ID` into the write worker. A
   plan-bound executor runs with `oms plan-run --id TASK --executor ID`; it
   cannot use `--next` because its task and lease are already frozen.

```bash
oms agent-plan --repo . claim --id t1 --provider codex
oms agent-executor --repo . create --id ex1 --provider codex \
  --strategy implementation-worker --plan-task t1 --model-class auto \
  --soul-file proposal.md
oms agent-executor --repo . validate --id ex1
oms agent-executor --repo . freeze --id ex1
```

`SOUL.md` controls behavior only. `meta.json` is authoritative for provider,
resolved model class/model/fallback, reasoning/fallback effort, mode,
task/lease, base SHA,
allowed/forbidden paths, and verification. The executor must not widen those
fields or delegate again. Repair rounds reuse the same frozen soul and route.
The mode is always `worktree-write`. The old explicit `--mode worktree-write`
argument is accepted only for caller compatibility and is no longer advertised.
Legacy `mode: read` rows remain available to `show` and `fail`, but validation
and execution reject them with migration guidance.
