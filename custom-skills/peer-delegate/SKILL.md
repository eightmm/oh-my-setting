---
name: peer-delegate
description: >
  Delegate a write task to another local agent CLI (Codex, Claude Code, or
  Antigravity) from inside the current agent session. Use when the user says
  "have codex do it", "delegate to", "ask another agent to implement",
  "codex한테 시켜", "위임해줘", "다른 에이전트한테 맡겨", or wants a second
  agent to execute work in parallel — including working a shared plan task
  (--plan-task) or driving the worker with a reusable persona (--role). The
  worker runs in an isolated git worktree and returns a patch that should go
  through patch-admit/patch-land before touching the main tree.
---

Goal: hand a well-scoped task to another agent, get back a reviewable patch.
The current agent stays the owner: it writes the brief, reviews the diff, and
decides whether to apply. Use local CLI tools only; no MCP or connectors.

## When

- User explicitly asks to delegate ("codex한테 시켜", "have claude implement X").
- Parallelizable side task while the current agent works on something else.
- Small, well-specified units. Do not delegate broad refactors, dependency or
  API changes, or anything destructive without explicit user confirmation.

## Brief Contract (required)

The worker cannot see this conversation. Write the brief from it. Include all
sections; thin briefs produce wrong patches.

```md
## Task
One-sentence request.

## Context
Decisions and background from the conversation the worker needs: why this is
being done, constraints already agreed, related prior changes.

## Constraints
Do-not-touch paths, style rules, no new dependencies, interface contracts.

## Files
Paths the worker is expected to change.

## Success criteria
What done looks like, concretely.
```

Save it to a temp file and pass with `--brief-file`. The worker also reads
`AGENTS.md`/`CLAUDE.md`/`PROJECT.md` inside the worktree, so shared rules apply
automatically; the brief only needs conversation-specific context.

## CLI

```bash
brief="$(mktemp)" && cat > "$brief" <<'EOF'
## Task
...
EOF
~/.oh-my-setting/scripts/peer-delegate.sh \
  --to codex \
  --brief-file "$brief" \
  --verify "uv run pytest tests/"
```

The worker runs non-interactively in a detached git worktree: it cannot touch
the main tree, commit, or push. Output lands in `.oms/artifacts/delegate/` as
a log plus a `.patch` against HEAD. For a one-provider write task, prefer
`agent-run.sh --mode write`; it records task outcomes and routes read/write
automatically.

Key flags beyond `--to`/`--brief-file`/`--verify`:

- `--plan-task ID` — couple the delegation to an `agent-plan.sh` task: the
  brief hydrates from the task, `--verify` defaults to the task's stored
  verify command, and the task moves to review/done on success or is released
  on failure. `--to codex --plan-task t3` is a complete one-liner.
- `--role NAME` — prepend a reusable worker persona from `.oms/roles/NAME.md`
  (see `agent-role.sh`); wins over the plan task's `role` field.
- `--repair N` — allow up to N verify-fail repair rounds (0-3): the worker
  gets the failing output back and retries before the delegation is failed.
- `--no-verify` — skip verification (rarely right; the verify contract is what
  makes the patch trustworthy).
- `--task-id ID` — stamp artifact-index rows for lineage without plan coupling.

When policy forbids sending repo context to an external provider, use the
ask/review wrappers with `--export-only`; run the exported prompt where allowed,
then import the answer with `import-agent-result.sh`. To recover a recent run,
use `artifact-index.sh latest` or `artifact-index.sh failures`.

## After the Worker Returns

1. Read the artifact log: did the worker report blockers or unverified steps?
2. Review the patch content before anything else.
3. Land through the admission gate, not by hand: `patch-land.sh --patch <p>`
   (or `--plan-task ID` alone — it reads the patch path the task stores). It
   re-applies the patch in a throwaway worktree, runs the checks ladder and
   verify contract, applies only on ADMIT to a clean tree, and records the
   land. A rejection is remembered in the fail-ledger, so a later attempt to
   land the same patch warns first. Use raw `--apply`/`git apply` only for
   trivial diffs you have fully read.
4. Run the project's own checks after applying; the worker's `--verify` ran in
   the worktree, not the main tree.
5. Report to the user: what was delegated, worker/verify status, what was
   applied, artifact paths.

## Safety

- Never use permission-bypass flags on the worker.
- Worker failure or a refused question is a result, not an error to hide —
  report the blocker and either refine the brief or do the work directly.
- Do not chain --apply blindly in scripts; a human or the owning agent reviews
  every patch.
