---
name: multi-agent-delegate
description: >
  Delegate a write task to another local agent CLI (Codex, Claude Code, or
  Antigravity) from inside the current agent session. Use when the user says
  "have codex do it", "delegate to", "ask another agent to implement", or wants
  a second agent to execute work in parallel. The worker runs in an isolated
  git worktree and returns a patch for review.
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
~/.oh-my-setting/scripts/multi-agent-delegate.sh \
  --to codex \
  --brief-file "$brief" \
  --verify "uv run pytest tests/"
```

The worker runs non-interactively in a detached git worktree: it cannot touch
the main tree, commit, or push. Output lands in `.oms/artifacts/delegate/` as
a log plus a `.patch` against HEAD.

## After the Worker Returns

1. Read the artifact log: did the worker report blockers or unverified steps?
2. Review the patch content before anything else.
3. Apply only when the diff matches the brief: re-run with `--apply`, or
   `git apply --binary <patch>`.
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
