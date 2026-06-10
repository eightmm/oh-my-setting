---
name: agent-harness
description: >
  Use when the user asks about shared agent memory, cross-model harness state,
  or calling one local agent CLI (Codex, Claude Code, or Antigravity) for an
  independent read-only pass or isolated write task from the current agent
  session.
---

Goal: keep agent state portable across Codex, Claude Code, and Antigravity, and
let the current agent call one provider explicitly when useful. The owning
agent decides whether the request is read-only advice or a write task.

## Shared Memory

- Use harness memory for stable preferences, recurring workflows, project
  pitfalls, and durable context that should be visible to all three agents.
- Do not store secrets, credentials, private keys, local machine paths, cluster
  details, or project-private paths.
- Required rules still belong in `AGENTS.md`, `CLAUDE.md`, checked-in docs,
  scripts, or hooks. Memory is soft recall only.
- Project memory lives under `.oms/memory/`:
  - `shared.md`: human-readable source log
  - `pins.md`: short high-signal notes always eligible for context
  - `summary.md`: compact recent notes generated from `shared.md`
- Global memory uses `~/.oh-my-setting/local/agent-memory.md` as the source log
  and companion compact files in the same directory.
- Provider calls inject compact memory by default, not the full markdown log.

Common actions:

```bash
~/.oh-my-setting/scripts/agent-memory.sh --repo . show
~/.oh-my-setting/scripts/agent-memory.sh --repo . context
~/.oh-my-setting/scripts/agent-memory.sh --repo . append --agent codex --text "Prefer scripts/check.sh fast before done."
~/.oh-my-setting/scripts/agent-memory.sh --repo . pin --agent codex --text "Current task: keep agent-run as the single provider entrypoint."
~/.oh-my-setting/scripts/agent-memory.sh --global append --agent claude --text "User prefers compact Korean status."
```

## Individual Agent Runs

Use `agent-run.sh` as the single entrypoint for one provider. In `--mode auto`,
it routes read-only questions to `agent-call.sh` and write tasks to
`multi-agent-delegate.sh`. The current/owning agent should override with
`--mode read` or `--mode write` when intent is already clear.

```bash
~/.oh-my-setting/scripts/agent-run.sh --to codex --repo . --prompt "Assess this plan."
~/.oh-my-setting/scripts/agent-run.sh --to claude --repo . --prompt "Implement the focused fix described above."
~/.oh-my-setting/scripts/agent-run.sh --to antigravity --repo . --mode write --prompt "Refactor this helper and return a patch."
```

Read mode writes artifacts to `.oms/artifacts/call/`. Write mode runs the worker
in an isolated git worktree and writes artifacts/patches to
`.oms/artifacts/delegate/`; the worker cannot commit or push. Use `--apply` only
when the owning agent has decided the returned patch should be applied and the
main tree is clean.

`agent-run.sh` attaches compact shared harness memory by default. Use
`--no-memory` when memory should not be sent, or set `OMS_AGENT_MEMORY_MODE=full`
only for debugging full source-tail prompts.

## Output

Report which provider was called, the artifact path, and the useful conclusion.
If the provider CLI is unavailable or blocked, say that plainly and continue
with current-agent reasoning when appropriate.
