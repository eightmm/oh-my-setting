---
name: agent-harness
description: >
  Shared multi-agent harness state and coordination (Codex, Claude Code,
  Antigravity). Use when the user asks about shared agent memory; resuming a
  repo or checking what other agents did ("what's the current state",
  "resume", "oms state", "이어서 해줘", "현재 상태 확인", "다른 에이전트가 뭐
  했어"); splitting subtasks across agents (plan DAG, claim/reclaim, "작업
  나눠서"); checking a known dead end before retrying a failed command
  ("have we tried this before", fail-ledger, "이미 실패한 명령이야?");
  reviewing or landing a delegated patch (patch-admit, patch-land, "패치
  검토/적용해줘"); cleaning up stale harness state (oms gc, stuck claims,
  orphaned delegations, ".oms 정리"); handing a prior session to another
  agent ("continue what Codex was doing", "Codex 세션 이어받아"); or calling
  one local agent CLI for a read-only pass or isolated write task.
---

Goal: keep agent state portable across Codex, Claude Code, and Antigravity, and
let the current agent call one provider explicitly when useful. The owning
agent decides whether the request is read-only advice or a write task.

Every command below can also be invoked as `oms <tool>` (dispatcher on PATH,
e.g. `oms agent-memory --repo . show`); `oms list` prints the full catalog.
State is attributed to the calling agent: set `OMS_AGENT` (codex, claude,
antigravity) in the CLI's env, or rely on auto-detection (Claude Code) and
the worker env injected by delegation.

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
~/.oh-my-setting/scripts/agent-memory.sh --repo . search --text pgvector   # recall by entry, not full cat
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
Provider prompts also receive the active loop warnings, so delegated workers can
see the same "do not repeat the same approach" signal instead of only the host
stderr warning.

Codex and Claude Code installs also get lightweight hook support when the user
has not opted out: `skill-router.sh` records a hash-only workflow/risk route for
normal prompts and auto-creates/appends the active task packet from normal user
prompts. The task writer still scans content and refuses sensitive-looking
entries. `turn-guard.sh` can block one final response when a guarded coding turn
with repo changes or high risk omits verification status. Disable with
`OMS_SKILL_ROUTER_OFF=1`, `OMS_AUTO_TASK_OFF=1`, or `OMS_TURN_GUARD_OFF=1` for a
session.

For live edits in the owning agent's tree, use `change-guard.sh` when scope
drift or user edits are likely. It snapshots the current dirty files and
declared path scope, then warns if a pre-existing dirty file changed or the diff
escapes scope. It is advisory by default; `--strict` makes warnings fail.

```bash
~/.oh-my-setting/scripts/change-guard.sh --repo . --allow scripts/ begin
~/.oh-my-setting/scripts/change-guard.sh --repo . check
~/.oh-my-setting/scripts/change-guard.sh --repo . end
~/.oh-my-setting/scripts/agent-task.sh --repo . update --constraint "allowed_paths: scripts/, README.md"
~/.oh-my-setting/scripts/change-guard.sh --repo . --from-task begin
```

`close` archives the packet and promotes a one-line outcome (goal + next
step) into project shared memory, so the next session starts from the
conclusion. Disable with `OMS_AGENT_TASK_CLOSE_MEMORY=0`.

## Task Plan (shared subtask DAG)

Where the task packet holds ONE active work item, `agent-plan.sh` holds a DAG
of subtasks that can be split across the three agents: dependencies, path
scope, and a verify command per task, with a
ready -> claimed -> running -> review -> done lifecycle under a file lock.

```bash
~/.oh-my-setting/scripts/agent-plan.sh --repo . add --id t1 --title "Fix parser" --verify "bash scripts/check.sh fast"
~/.oh-my-setting/scripts/agent-plan.sh --repo . ready
~/.oh-my-setting/scripts/agent-plan.sh --repo . next --claim --provider codex
~/.oh-my-setting/scripts/agent-plan.sh --repo . touch --id t1      # heartbeat a long claim so it is not reclaimed
~/.oh-my-setting/scripts/agent-plan.sh --repo . reclaim            # requeue expired claims (dead workers)
~/.oh-my-setting/scripts/agent-plan.sh --repo . finish --id t1
```

Provider names are canonical (`agy` normalizes to `antigravity`; unknown names
are rejected). `multi-agent-delegate.sh --plan-task ID` couples a delegation to
a plan task: released on failure, review/done on success. Without an explicit
`--prompt`/`--brief-file` it hydrates the worker brief from the task, and
without `--verify` it uses the task's stored verify command — so
`multi-agent-delegate.sh --to codex --plan-task ID` is a complete one-liner.

## Failure Memory, Liveness, GC, Onboarding

- `oms init` seeds `.oms/` and prints a next-actions checklist for a fresh repo;
  `oms state` is the read-only dashboard (now also: in-flight delegations with
  live-vs-orphan pid, latest CI conclusion, unresolved failures).
- `fail-ledger.sh` is durable cross-session failure memory. Before retrying a
  command that may be a known dead end, `check --cmd "..."` (exit 3 if it is a
  known-unresolved failure); `record --cmd ... --exit N --summary ...` a new
  one; `resolve --fingerprint FP` when fixed. Sensitive commands are refused.
- `multi-agent-delegate` writes a `.oms/delegations/<id>.json` liveness marker
  while a worker runs (removed on exit); `oms state` shows live workers and
  flags dead-pid orphans. No polling/daemon.
- `oms gc` (dry-run by default, `--apply` to act, `--days N`) reclaims aged
  transient `.oms/` state; it never touches open runs, the active task, or
  unresolved failures.

## Role Profiles

A role is a reusable worker persona (a reviewer, a refactorer, a test-writer)
kept as markdown in `.oms/roles/<name>.md` (global fallback
`~/.oh-my-setting/local/roles`). It is data, not an orchestrator: the owning
agent chooses a role and injects it into a delegated worker. The same role
drives any of the three providers. To run several workers by role, the
orchestrator delegates once per (role, task) — heavier isolated workers via the
harness, or lighter native sub-agents where the current CLI supports them.

```bash
~/.oh-my-setting/scripts/agent-role.sh --repo . --name reviewer init   # scaffold
~/.oh-my-setting/scripts/agent-role.sh --repo . list
~/.oh-my-setting/scripts/multi-agent-delegate.sh --to codex --role reviewer --prompt "review the diff"
~/.oh-my-setting/scripts/agent-plan.sh --repo . add --id t1 --title "review" --role reviewer  # task carries the role
```

`--role` on delegate wins over a plan task's `role` field; an unknown role name
fails fast.

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
main tree is clean. Workers receive `OMS_STATE_REPO` (so agent-memory/task/plan
resolve to the primary repo's shared `.oms`, not the empty throwaway worktree)
and `OMS_AGENT=<provider>` for attribution.

Every provider call/delegation appends a compact row to
`.oms/artifacts/index.jsonl`. Use `artifact-index.sh latest`, `latest-run`,
`list`, or `failures` when resuming work or looking for the latest provider
result; `latest-run` groups rows from the newest timestamp-PID run into a
compact summary. The index is append-only, so `artifact-index.sh prune [N]`
trims it to the most recent N rows, and `prune [N] --files` also removes
unreferenced regular files under `.oms/artifacts/`. When an active task exists,
`agent-run.sh` also appends
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

Single-provider read calls use the same handoff path: pass `--export-only` to
`agent-run.sh --mode read` or `agent-call.sh`, then import with
`import-agent-result.sh --kind call --provider PROVIDER --prompt-file
.oms/artifacts/call/PROVIDER-...export.md --file answer.md`. Write delegation
cannot be exported; it needs the isolated worktree worker.

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
For debate rounds, prior provider output is treated as untrusted quoted data:
paths are masked and sensitive-looking lines are redacted before the quote is
sent to the next provider.

For ML repos, the digest comes from `agent-ml-context.sh`: entrypoint file names,
verification contract hints, and recent `docs/EXPERIMENTS.jsonl` rows. Delegated
workers prefer `bash scripts/check.sh ml-smoke` when the project is detected as
ML and that mode exists; otherwise they fall back to `fast`.

## Admitting Delegated Patches

A worker patch from `multi-agent-delegate.sh` can be stale (its base moved),
partial, or pass only under the worker's own assumptions. Before landing one on
the main tree, run it through the admission gate: it applies the patch in a
throwaway worktree off the current HEAD and runs a checks ladder — applies
cleanly (not stale) → changed shell/python/json files parse → the patch does not
modify its own verifier → the verification contract passes — then emits a verdict
and a report. Exit is nonzero unless every gate passes, so it composes with `&&`.

Prefer `patch-land.sh` to actually land a patch: it runs the admission gate,
then (only on ADMIT and a clean tree) applies the patch, records the land in the
artifact index, and optionally finishes the coupled plan task. Use bare
`patch-admit.sh` only to gate without landing.

```bash
~/.oh-my-setting/scripts/patch-land.sh --patch .oms/artifacts/delegate/<worker>.patch --plan-task t3
~/.oh-my-setting/scripts/patch-admit.sh --patch <p> --verify "bash tests/scripts-smoke.sh" --ml
```

The report (changed files, ladder, verify tail) lands under
`.oms/artifacts/admit/` and is recorded in the artifact index. Use it as the
trust boundary between delegation and applying the result.

## Session Handoff

Shared memory and the task packet are forward-looking and curated. When you
instead need to hand a PRIOR session from one agent to another — "continue what
Codex was doing", "pick up my last Claude session" — distill that session's
transcript into a compact digest. Raw transcripts are huge and tool-noisy, so
the digest captures goal, recent user turns, files touched, and the last
assistant summary. Extraction is mechanical (no model call): fast, free, and
deterministic.

- Digests land in `.oms/handoffs/` (git-ignored) and are local artifacts. The
  content is scanned; if it looks sensitive the capture is REFUSED by default
  (transcripts carry pasted secrets and the digest is meant for another agent).
  Override with `--allow-sensitive` only when you are sure. Loading a digest
  into another agent is an explicit step you take.
- Source sessions: Claude (`~/.claude/projects/<cwd>/<id>.jsonl`, full),
  Codex (`~/.codex/.../rollout-*.jsonl`, goal/turns/last reply), Antigravity
  (`~/.gemini/antigravity-cli/history.jsonl`, prompts only — assistant output
  is not recoverable from history).

```bash
~/.oh-my-setting/scripts/session-handoff.sh capture --agent codex --cwd .
~/.oh-my-setting/scripts/session-handoff.sh list
~/.oh-my-setting/scripts/session-handoff.sh show <file>   # print to feed another agent
```

## Output

Report which provider was called, the artifact path, and the useful conclusion.
If the provider CLI is unavailable or blocked, say that plainly and continue
with current-agent reasoning when appropriate.
