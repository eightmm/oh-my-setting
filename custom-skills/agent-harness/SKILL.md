---
name: agent-harness
description: >
  Use when the user asks about shared agent memory, cross-model harness state,
  or calling one local agent CLI (Codex, Claude Code, or Antigravity) for an
  independent read-only pass from the current agent session.
---

Goal: keep agent state portable across Codex, Claude Code, and Antigravity, and
let the current agent call one provider explicitly when useful.

## Shared Memory

- Use harness memory for stable preferences, recurring workflows, project
  pitfalls, and durable context that should be visible to all three agents.
- Do not store secrets, credentials, private keys, local machine paths, cluster
  details, or project-private paths.
- Required rules still belong in `AGENTS.md`, `CLAUDE.md`, checked-in docs,
  scripts, or hooks. Memory is soft recall only.
- Project memory lives at `.oms/memory/shared.md`; global memory lives at
  `~/.oh-my-setting/local/agent-memory.md`.

Common actions:

```bash
~/.oh-my-setting/scripts/agent-memory.sh --repo . show
~/.oh-my-setting/scripts/agent-memory.sh --repo . append --agent codex --text "Prefer scripts/check.sh fast before done."
~/.oh-my-setting/scripts/agent-memory.sh --global append --agent claude --text "User prefers compact Korean status."
```

## Individual Agent Calls

Use `agent-call.sh` for read-only independent opinions from one provider. For
write tasks, use `multi-agent-delegate.sh` so edits happen in an isolated
worktree and return as a patch.

```bash
~/.oh-my-setting/scripts/agent-call.sh --to codex --repo . --prompt "Assess this plan."
~/.oh-my-setting/scripts/agent-call.sh --to claude --repo . --prompt "Find holes in this API design."
~/.oh-my-setting/scripts/agent-call.sh --to antigravity --repo . --prompt "Review this implementation direction."
```

`agent-call.sh` attaches shared harness memory by default and writes artifacts to
`.oms/artifacts/call/`. Use `--no-memory` when memory should not be sent.

## Output

Report which provider was called, the artifact path, and the useful conclusion.
If the provider CLI is unavailable or blocked, say that plainly and continue
with current-agent reasoning when appropriate.
