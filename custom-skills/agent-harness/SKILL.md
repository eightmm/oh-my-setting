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

## Active Task Handoff

Use active task state for the current short-lived work item. This is separate
from shared memory: memory is durable soft recall, task is the current handoff
packet that lets Codex, Claude Code, and Antigravity continue the same work
without replaying the whole chat.

- Project task file: `.oms/task/current.md`
- Do not store secrets, private paths, cluster details, raw logs, datasets, or
  checkpoints.
- Provider calls attach the active task by default. Use `--no-task` only when
  the current task should not be sent.

Common actions:

```bash
~/.oh-my-setting/scripts/agent-task.sh --repo . init --goal "Ship the focused fix" --verify "bash scripts/check.sh fast"
~/.oh-my-setting/scripts/agent-task.sh --repo . update --state "Patch drafted; tests still pending" --next "Run smoke tests"
~/.oh-my-setting/scripts/agent-task.sh --repo . append --agent codex --text "Review found one missing test."
~/.oh-my-setting/scripts/agent-task.sh --repo . context
~/.oh-my-setting/scripts/agent-task.sh --repo . close
```

Loop hardening fields are optional but should be used when work repeats or
moves across providers:

```bash
~/.oh-my-setting/scripts/agent-task.sh --repo . update \
  --loop-attempts 2 --loop-max 3 --diff-budget 200 \
  --verify-level "focused-test" \
  --last-failure "bash tests/scripts-smoke.sh exit=1" \
  --verification "bash -n passed" \
  --hypothesis "failure is from stale generated state" \
  --result "same failure after narrowing"
```

`agent-run.sh` warns before write delegation when attempts are exhausted, the
same failure appears repeatedly, or the current git diff exceeds the task's
line budget. The warning is advisory; the owning agent decides whether to stop
and revise the hypothesis or continue.

`close` archives the packet and promotes a one-line outcome (goal + next
step) into project shared memory, so the next session starts from the
conclusion. Disable with `OMS_AGENT_TASK_CLOSE_MEMORY=0`.

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

Every provider call/delegation appends a compact row to
`.oms/artifacts/index.jsonl`. Use `artifact-index.sh latest`, `list`, or
`failures` when resuming work or looking for the latest provider result; the
index is append-only, so `artifact-index.sh prune [N]` trims it to the most
recent N rows, and `prune [N] --files` also removes unreferenced regular files
under `.oms/artifacts/`. When an active task exists, `agent-run.sh` also appends
a one-line outcome with artifact and patch paths to `## Current State`.

When the current agent must not send repo context to another external provider,
use the same commands with `--export-only`. This writes provider-specific prompt
artifacts but does not call Codex, Claude Code, or Antigravity. Run the exported
prompt wherever policy allows, then import the answer back into the same artifact
index:

```bash
~/.oh-my-setting/scripts/multi-agent-review.sh --repo . --diff --providers claude --export-only --prompt "Review this change."
~/.oh-my-setting/scripts/import-agent-result.sh --kind review --provider claude --prompt-file .oms/artifacts/review/claude-...export.md --file claude-answer.md
```

This export/import path is provider-neutral and should behave the same from
Codex, Claude Code, and Antigravity sessions because the current agent only
writes or reads local artifacts.

`agent-run.sh` attaches compact shared memory, the active task packet, and an
ML context digest for detected ML repos by default. Use `--no-memory`,
`--no-task`, or `--no-ml-context` to omit those layers. Set
`OMS_AGENT_MEMORY_MODE=full` only for debugging full source-tail prompts.

Every outbound provider prompt is scanned before the CLI is called.
Sensitive-looking credentials, private keys, absolute machine paths, cluster
details, raw logs, datasets, and checkpoints block the external call instead of
being silently sent.

For ML repos, the digest comes from `agent-ml-context.sh`: entrypoint file names,
verification contract hints, and recent `docs/EXPERIMENTS.jsonl` rows. Delegated
workers prefer `bash scripts/check.sh ml-smoke` when the project is detected as
ML and that mode exists; otherwise they fall back to `fast`.

## Output

Report which provider was called, the artifact path, and the useful conclusion.
If the provider CLI is unavailable or blocked, say that plainly and continue
with current-agent reasoning when appropriate.
