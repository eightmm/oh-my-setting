# Bounded Autonomous Progress

Use autonomy to continue safe work, not to widen authority.

## Task Loop

1. Orient: inspect repository instructions, `oms state`, the active task/plan,
   the worktree, and failures relevant to the intended command.
2. Contract: state the objective, constraints, observable completion criteria,
   and mechanical verification. Infer reversible details from local evidence.
3. Act: perform the smallest useful in-scope step. Use a plan only for genuine
   dependencies or parallel work.
4. Verify: check the final relevant tree. Provider prose, a generated artifact,
   task status, and worker-worktree verification are context, not proof.
5. Recover: preserve the failure, change the hypothesis or implementation, and
   retry only within a declared bound. Do not repeat an unchanged known failure.
6. Report or stop: continue while a safe action remains. Stop for new authority,
   irreversible/high-impact choices, exhausted repair, or a material ambiguity.

## Deciding Without the User

A reversible fork is yours to take: name the options in one line in the task
packet's Decisions, take the stronger one, and keep moving — asking the user
to pick is a stop, and stops are for authority, not preference. When peers
disagree, weigh verdicts by stated confidence and evidence, give the author
exactly one repair round against the concrete failure, then break the tie
with `oms advise` — a different model family, not a louder same-family voice.
Record the losing position as an open dissent instead of erasing it, so the
next session acknowledges it (agree, override with reasons, or escalate)
rather than silently re-deriving consensus. Face the user with results: what
was decided, what changed, what was verified, and the one thing that
genuinely needs their authority — never a menu of options you could have
resolved yourself.

## One-Task Plan Driver

For an existing plan task with non-empty scope and verification:

```bash
oms plan-run --repo . --to codex --next
```

This atomically claims and delegates exactly one task, then leaves the patch in
`review`. Landing is a separate authority decision:

```bash
oms plan-run --repo . --to codex --next --land
```

`--land` still uses patch admission, the current lease, and the task's verify
contract. It never commits, pushes, publishes releases, adds dependencies, generates more
tasks, or recursively delegates. Use `--repair N` for bounded worker correction;
an unchanged known failure is refused unless `--retry-known` is explicit.

Do not use a multi-task `while ready` loop. The parent re-orients between tasks
so user edits, changed authority, blocked dependencies, and new evidence remain
visible.
