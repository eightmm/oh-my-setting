---
name: peer-delegate
description: >
  Delegate an explicitly requested write task to Codex, Claude Code, or
  Antigravity in an isolated worktree and return a reviewable patch. Use for
  "delegate to", "codex한테 시켜", "위임해줘", or parallel implementation.
---

# Peer Delegate

The current agent remains owner of scope, review, landing, verification,
commit, and push. Use local provider CLIs only.

## Run

Use `oms agent-run --mode write` for normal delegation. Give the worker one
bounded task with the conversation-specific context it cannot discover, allowed
paths, constraints, observable success criteria, and a verification command.
Repository rules are already visible in the isolated worktree.

```bash
oms agent-run --to codex --mode write --role implementation-worker \
  --prompt "Implement the bounded change." --verify "bash scripts/check.sh fast"
```

Use `--plan-task ID` for an existing claimed plan task. Use `--executor ID` only
for a substantial write with an already frozen task-scoped soul. Inspect
`oms peer-delegate --help` for uncommon model, repair, or export flags instead
of copying them into the prompt.

## Land

Read the worker artifact and patch. Apply accepted work through
`oms patch-land --patch <path>` or `oms patch-land --plan-task ID`, then rerun
verification on the final main tree. A worker result is evidence, not authority
to mutate the main tree.

Report the provider, useful result, artifact/patch, landing decision, and any
failed or skipped check. Never bypass permissions, recursively delegate, or
silently broaden dependencies, public contracts, or destructive scope.
