# Bounded Autonomous Progress

Use autonomy to continue safe work, not to widen authority.

## Parent control-plane contract

Every command below is for the top-level parent agent, not the end user. The
user supplies the goal, constraints, and material authority; the parent invokes,
reviews, resumes, and recovers OMS in the same task. Never hand the user a
command, proposal digest, `.oms` path, or parked-run procedure.

When using autopilot for an authorized coding request, confirm that `PROJECT.md`
captures the goal and that scope and completion are materially clear. Run `propose`,
treat its tasks, dependencies, scope, and verifier command as untrusted data,
then pass back only the digest of bytes you actually reviewed. Exit 4 is an
internal parent-review boundary, not a user handoff. Apply the same
review to an `r1-` proposal and use `status` plus its validated continuation
after an interruption. Never mechanically chain planner output into `run`.
Ordinary bounded edits need no proposal or autonomous runner.

When a panel main is asked to complete a project, or an autopilot contract
already exists for its repository, the main supervises the same campaign rather
than starting a competing panel plan driver. It inspects autopilot status and
the receipt before acting, reviews the exact proposal bytes and digest before
run, and uses only `oms autopilot propose`, `run --review-mode gate`, `status`,
or validated receipt re-entry. It never dispatches panel write workers for
campaign-owned tasks, releases or reclaims them, resets cycle exhaustion, or
overrides a parked receipt. Read-only reviewer, critic, or security-auditor
judgment may inform proposals, parks, and final changes. Acceptance requires
the gated whole-change review and fresh acceptance. Without a campaign, the
ordinary panel plan lifecycle remains unchanged. To start with a panel, ask the
panel main to complete the project; without a panel, use `oms autopilot propose`
and then run the exact reviewed proposal with `--review-mode gate` and its
`--expected-proposal-sha256`.

For a parked campaign, `oms autopilot status` leads with the recorded phase,
reason, semantic verdict and evidence path.

An analysis request does not authorize writes. Local implementation and commits
may follow a clear implementation request; a Draft PR requires explicit current
or standing repo authority. Existing-branch updates, push, merge, ready, tag,
and release remain separate authority decisions.

## Automatic collaborative planning

When the user authorizes automatic discussion plus implementation, select
`oms autopilot --collaboration auto` for a multi-step goal. The parent handles
propose, exact proposal review, run and recovery within the authorized task.
A capability question alone does not start the loop. Keep small, clear edits
local instead of paying for a planning council.

The native runtime calls its current fixed Codex, Claude and Antigravity council
at initial planning and the single permitted remainder proposal. It does not
inherit panel council customization. Each seat gives an initial answer and one
rebuttal; every required seat and round must complete. Registry and physical
transport checks still apply. Missing, refused or malformed responses stop
planning; inspect the retained artifacts before retrying. Native mode does not
automatically restate an incomplete answer. Omission of `--collaboration`
selects `off`.

The caller explicitly requests synthesis on the planner's resolved advisor
route and validates the `--synthesis-json` request/run/round/artifact contract.
Planner model and effort apply to this synthesis and decomposition, without
overriding other council seats. The response sections preserve alternatives,
evidence, counterargument, verification, deltas and disagreements. They are
structure checks, not proof of sound reasoning, consensus or model independence.
Requested models must not be reported as observed served models.

The immutable native decision manifest records the discussion's exact inputs and
artifact references. The reviewed proposal digest covers its `discussion`
reference; applied tasks retain the same reference. Plan admission, replay and
autopilot resume revalidate those retained bytes without provider calls. The
parent resolves disputed claims against source/tests and reviews the exact
resulting proposal. Agreement never replaces the acceptance command.

Choose the implementation transport from task and repository evidence. Routes
come from the existing registry and transport capabilities; there is no extra
GPT-generation name restriction. Explicit model/effort selections stay bound.
Keep one implementation transport per campaign and a different gated reviewer;
do not force mixed assignments that make the reviewer an author. Removing a
naming restriction does not grant an unsupported transport write capability.

The mode binds into the durable receipt, requires gated review, and survives
re-entry; it cannot be silently disabled while resuming. Existing scope, timeout,
cycle, one-shot repair and remainder limits still apply. `--auto-repair` remains
an explicit bounded option. After a failure, inspect its evidence before retrying;
do not auto-enable `--retry-known`, expand scope, weaken verification, or ask the
council to override a policy denial. Completion requires acceptance and review.
Report completion or the material blocker; do not emit unchanged heartbeats.

A campaign review covers only its frozen base-to-HEAD diff. `peer-review` writes
an exact diff digest and path list in its `review-scope` artifact, explicitly
excluding external recovery changes. Before reporting the whole user request
complete, the parent must include every parent-authored recovery patch in a
separate peer review at its exact current diff, or a combined final review.
Keep each patch's base/diff digest and review outcome; tests alone do not make
an external recovery patch peer-reviewed. Never widen a live campaign's scope
or treat its `done` receipt as coverage of another checkout.

For demonstrably snapshot-pure acceptance, a project may explicitly put
`- Acceptance reuse: same-run` in PROJECT.md's Verification section. This asserts
all result-affecting inputs are tracked or manifest-bound bytes/modes, with external tools
stable for the run; network, clock, hardware, other filesystem metadata and mutable ignored inputs do not
qualify. Autopilot may reuse only the latest supervised pass immediately after
the same goal-drive run, for at most ten minutes from acceptance start, matching command, plan, spec,
manifest, repository state, runner and environment. Missing/stale/mismatched
proof runs fresh; default contracts always run fresh. Peer-review's verifier
and post-review acceptance remain fresh, as do release/publication gates.
Raw tracked bytes are bound independently of Git's text normalization; trees
over the 64 MiB reuse budget or containing submodules stay fresh.
Reuse prints its source receipt and never refreshes that receipt's timestamp.

Serialize repository-local peer calls and a full `check.sh` gate: even read-only
peers append artifacts under `.oms/`, which the gate correctly treats as changes
to protected user state. Finish model calls before starting that gate.

Keep known one-line fixes local unless exercising collaboration is itself the
request. Spend a council on a material unresolved decision, and use one bounded
rebuttal round when it discriminates alternatives. A completed call, consensus,
and a valid response format are separate from substantive reasoning and tests.

This is an agent-driven loop while a parent is running, not an always-on daemon
or an app notification service. Session wakeups need a separately configured
scheduler. Publication and expensive compute retain their own authorization.

An idle main with no ready task does not just wait: its plan hint names a waiting
`proposal-review` proposal (review the exact bytes, apply by digest), else, while
acceptance is unmet, suggests `oms plan-from-spec --discover` at most once per plan
generation per UTC day. Review and gates still apply; nothing applies automatically.

The one built-in wakeup is opt-in: with `OMS_TICK_WAKE=1`, the hourly `oms tick`
types a single prompt into an idle panel main (30 minutes idle, no live child
call, panel lock free) when plan tasks are ready or an autopilot proposal waits
in `proposal-review`. The prompt is ordinary text, not `/goal`, so any provider's
main receives it; it names that reason and asks the main to continue the shared
plan within its existing authority. It is typed only when the live pane proves
the input line empty, so a user's draft is never submitted (keys typed in the
about 0.8 s between that check and Enter still can be); it never starts a
main or worker, and is capped at one per main per run and 6 per UTC day.

## Task Loop

1. Orient: inspect repository instructions and the worktree; consult existing
   task/plan state and failures when relevant to the intended command.
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
to pick is a stop, and stops are for authority, not preference. When authorized
peers disagree, resolve claims against source evidence and discriminating checks.
Bound repairs to the concrete failure. Use `oms advise` only when another model
call is within explicit user authorization and adds needed evidence; disagreement
alone does not require a new call. Otherwise decide locally or report the blocker.
Record the losing position as an open dissent instead of erasing it, so the
next session acknowledges it (agree, override with reasons, or escalate)
rather than silently re-deriving consensus. Face the user with results: what
was decided, what changed, what was verified, and the one thing that
genuinely needs their authority — never a menu of options you could have
resolved yourself.

## One-Task Plan Driver

Before admitting an autopilot proposal, check task allocation as well as scope:
each task should own one observable behavior and its required regression;
dependencies must represent real ordering, and independent tasks should avoid
overlapping writable paths. Keep architecture/security/scientific decisions in
the parent until the resulting implementation task is bounded.

Tasks may carry an optional reviewed `assignment`, for example
`{"provider":"claude","workload":"routine"}` or
`{"provider":"codex","model":"<exact cached model>"}`. The provider is the
worker transport, not the parent: Claude Code can coordinate Codex, Codex can
coordinate Claude, and Antigravity follows the same contract.

An assignment owns the complete task route: required `provider`, optional
`model`, `fallback_model`, `reasoning_effort` (default `auto`), and `workload`
(`standard` or `routine`, default `standard`). Omitted model/fallback values
are empty; another provider's run defaults never leak into this route.
An absent or empty assignment preserves the existing run defaults
(`--worker`, `--worker-model`, effort/fallback). Review assignments before
admitting proposal bytes; exact replay checks them and retries retain them.
No live assignment mutation or automatic provider failover is provided.

`plan-from-spec --to` selects the planner; `--worker-provider` supplies the
authorized worker transport for routine assignment suggestions. Autopilot
passes its existing `--worker` value. Inspect cached capabilities before
naming models; the planner must not invent models or provider authorization.
For a manual plan, `agent-plan add --assignment '{"provider":"claude"}'`
stores the same contract. Contract-bound plans still require reviewed proposals.
Unpinned catalog selection and bounded recovery follow model-routing.md; the
stored request is not proof that every call serves an identical model.
`plan-run` is one-shot and serial in goal-drive. Standalone interactive sessions
are for parent-driven clarification, not a way around plan leases or admission.
Scope, dependencies, regression ownership and parent review remain unchanged;
model allocation does not grant workers delegation, installation or publication
authority. Mixed-risk work must not all be sent to the cheapest model.

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

For a plan the parent verified against an authorized goal and executable done
definition:

```bash
oms agent-plan --repo . init --goal "..." --accept "bash scripts/check.sh"
oms agent-plan --repo . add --id t1 --title "feat: ..." --allowed src/ --verify "..."
oms goal-drive --repo . --to codex --max-cycles 3
```

`oms plan-from-spec` decomposes a confirmed PROJECT.md into that plan, but only
ever PROPOSES. The parent reviews the task list before `--apply`; generated
plans never enter silently, and a `State: draft` spec is refused.
In GitHub project mode every added or applied task needs an issue (`add
--issue N`). `oms plan-from-spec --issue REF` / `oms autopilot propose --issue
REF` stamp the reviewed issue; an adopted `intent draft --from-issue` contract
carries its same-repo issue while PROJECT.md is unchanged. Restamp a proposal
pending an issue with `oms plan-from-spec --restamp PATH
--expected-proposal-sha256 SHA --issue REF` (autopilot: `oms autopilot propose
--restamp PATH --expected-proposal-sha256 SHA --issue REF`).

For the full bounded path, the parent uses this two-step admission boundary:

```bash
oms autopilot --repo . --allowed 'src,tests,docs' --base main propose
oms autopilot --repo . --allowed 'src,tests,docs' --base main \
  --proposal .oms/plan/proposal-...json \
  --expected-proposal-sha256 <digest printed by propose> --draft-pr run
```

For longer work, the parent pins route and wall clock in the reviewed envelope,
for example `--worker-model MODEL --worker-reasoning-effort high
--worker-timeout 20m --retry-known`. After interruption, the parent runs
`status`, validates the durable outer receipt, and executes its exact safe
continuation internally.

The first command exits with a proposal for the parent to review and prints
the proposal's sha256 plus a shell-safe continuation containing every effective
option. The second accepts only a regular non-symlink proposal of at most 1 MiB,
requires that digest back, atomically applies only matching bytes, drives the
existing loop, permits at most
one `r1-` remainder proposal, re-runs acceptance, and uses a separate semantic
reviewer. Gate mode refuses a reviewer that authored any completed task;
shadow mode reports that overlap. Different transports alone do not establish
model-family independence, especially with multi-model carriers. Review actual
model provenance before claiming independent judgment.
Draft PR publication defaults to a blocking gate; use
`--review-mode shadow` only as an explicit advisory choice.
`--draft-pr` is the only built-in remote-write path: an immutable local intent
creates a new branch and Draft PR and can be replayed after interruption. It has
no branch update, merge, ready, tag, or release operation. If the run starts on the base
branch, autopilot first creates `oms/autopilot-<spec-digest>` locally so
implementation commits never land on that base; from any other branch it
parks unless the branch is that deterministic name or one of its strict `-rN`
recovery branches. Raw Git paths in the complete branch diff must stay inside
the reviewed envelope. A unique final result binds the private run receipt,
status, and reason to this invocation and must match the durable terminal row.
The selected work branch stays fixed through review, and a matching recovery
branch found from the base must be resumed explicitly rather than forked.

For a custom/composed acceptance command, `PROJECT.md` must also name every
repo-relative verifier file under `Required check files`; those bytes are bound
to the proposal and protected during admission. Each cycle: acceptance command
(pass = done) → one `plan-run --next --land` →
commit of exactly the admitted patch's paths, task title as subject. It
refuses a dirty tree, parks on task exhaustion, an acceptance command edited
mid-run, tracked changes beyond the admitted patch, or an unchanged tree with
the same failing acceptance twice — every terminal leaves a reason row in
`.oms/plan/progress.jsonl` and a fail-ledger entry when parked. The driver
never generates or re-plans tasks, never pushes, and never reclaims leases:
task decomposition and recovery stay with the parent. Acceptance pass means
"the recorded bounded command and frozen verifier files passed without changing
the bound Git-visible/index/declared-file surface", nothing more. Ignored paths
and effects outside the repository remain a host-isolation concern; spot-check
the goal before shipping.
