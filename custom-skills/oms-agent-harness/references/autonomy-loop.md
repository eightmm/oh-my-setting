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

Do not hand-roll a multi-task `while ready` loop. The parent re-orients
between tasks so user edits, changed authority, blocked dependencies, and new
evidence remain visible. The one sanctioned mechanization of that
re-orientation is `oms goal-drive`, whose between-cycle checks — acceptance
first, stuck detection, park-with-reason — are the re-orientation, bounded by
a hard cycle cap.

## Goal Drive

For a HUMAN-APPROVED plan whose definition of done is executable:

```bash
oms agent-plan --repo . init --goal "..." --accept "bash scripts/check.sh"
oms agent-plan --repo . add --id t1 --title "feat: ..." --allowed src/ --verify "..."
oms goal-drive --repo . --to codex --max-cycles 3
```

`oms plan-from-spec` decomposes a confirmed PROJECT.md into that plan — but
it only ever PROPOSES: review the printed task list, then `--apply` the
proposal file. Generated plans enter the board through approval, never
silently; a `State: draft` spec is refused outright.

For the full bounded path, use the two-step approval boundary:

```bash
oms autopilot --repo . --allowed 'src,tests,docs' propose
oms autopilot --repo . --allowed 'src,tests,docs' --base main \
  --proposal .oms/plan/proposal-...json \
  --expected-proposal-sha256 <digest printed by propose> --draft-pr run
```

The first command exits with a proposal for the parent to review and prints
the proposal's sha256 plus the exact continuation; the second requires that
digest back and atomically applies only bytes that still match it, drives the
existing loop, permits at most
one `r1-` remainder proposal, re-runs acceptance, and uses a different-family
semantic review in shadow mode. `--review-mode gate` makes that review blocking.
`--draft-pr` is the only built-in remote-write path: an immutable local intent
creates a new branch and Draft PR and can be replayed after interruption. It has
no branch update, merge, ready, tag, or release operation. If the run starts on the base
branch, autopilot first creates `oms/autopilot-<spec-digest>` locally so
implementation commits never land on that base; from any other branch it
parks unless the branch is that deterministic name or one of its `-suffix`
recovery branches.

Each cycle: acceptance command (pass = done) → one `plan-run --next --land` →
commit of exactly the admitted patch's paths, task title as subject. It
refuses a dirty tree, parks on task exhaustion, an acceptance command edited
mid-run, tracked changes beyond the admitted patch, or an unchanged tree with
the same failing acceptance twice — every terminal leaves a reason row in
`.oms/plan/progress.jsonl` and a fail-ledger entry when parked. The driver
never generates or re-plans tasks, never pushes, and never reclaims leases:
task decomposition and recovery stay with the parent. Acceptance pass means
"the recorded command passed on this tree", nothing more — keep the command
honest and out of worker reach, and spot-check the goal yourself before
shipping.
