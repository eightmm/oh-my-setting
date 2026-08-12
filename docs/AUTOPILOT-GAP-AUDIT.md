# Autopilot Gap Audit

Audit snapshot: product source at `7573a01`, 2026-08-12. This was a real local
`propose -> parent review -> run -> recovery` exercise in a dedicated worktree.
Provider calls were made through the harness; no remote branch, PR, merge, tag,
release, dependency, product code, or main-worktree change was authorized.
Three role-separated read-only audits and three targeted cross-checks were used
to challenge the findings independently of the document-producing worker.

> Resolution note (2026-08-12): the hardening release following this audit
> addresses G1–G11. The durable outer receipt, frozen acceptance-file manifest,
> explicit branch-ref CAS, strict worker guard, complete-diff semantic gate,
> and adversarial regressions are summarized in `CHANGELOG.md`. The numbered
> sections below are retained as the evidence and threat model that motivated
> those controls; wording such as “remain” describes the audited snapshot, not
> the hardened release. Hard token/cost enforcement and effects outside the
> repository still require provider/OS support and remain explicit limits.

Severity is relative to the documented trusted-local agent boundary. High can
cross an OMS authority or repository boundary. Medium can falsely weaken a
gate, lose a safe continuation, or make bounded progress unreliable. Low is
primarily discoverability or evidence quality.

## Executive summary

The composition has strong foundations: a proposal cannot apply itself; its
bytes, source `HEAD`, spec digest, plan CAS, and path envelope are bound; workers
return isolated patches; commits are local; final acceptance and semantic review
precede the optional create-only Draft PR path.

It is not yet safe enough to describe as unattended autonomous coding. Three
High defects remain in `goal-drive`: a harness child can inherit landing/commit
authority, commit publication is not bound to the symbolic branch identity, and
the progress ledger follows a symlink outside the repository. Acceptance and
whole-branch scope also have incomplete end-to-end binding. Recovery and
evaluation are safe-stopping but too lossy for reliable long runs.

The live exercise did not reach semantic review. It parked safely, but only
after exposing fixed provider timeouts, oversized task decomposition, a missing
top-level known-failure retry path, and an output-only continuation contract.

## Actual flow and observed run

1. `oms autopilot ... propose` validates a clean tree and confirmed
   `PROJECT.md`, then asks `plan-from-spec` for JSON without applying it.
2. The default Claude planner reached the shared five-minute provider timeout.
   A Codex planner fallback produced one SHA-bound, docs-only task; the parent
   reviewed and approved its exact continuation.
3. `run` atomically applied that proposal and created the deterministic local
   `oms/autopilot-<spec-digest>` branch.
4. The Codex worker spent the default five minutes inspecting the repository
   and parked with no patch. Raising the documented environment timeout to 15
   minutes required manually resolving the unchanged-failure ledger because
   `autopilot` cannot forward `plan-run --retry-known`.
5. The longer attempt produced a 257-line patch just before timeout, but failed
   the 220-line contract and was not applied. A bounded lower-effort recovery
   produced a 208-line patch and passed its quick verification, but the provider
   outcome classifier returned a non-success result, so the plan correctly
   released the task instead of trusting the patch.
6. Independent patch admission confirmed apply, secret, scope, structure,
   syntax, test-reduction, and verifier-surface checks. Its full repository gate
   reached its ten-minute admission timeout, so the patch remained rejected.

These stops protected the tree, but the operator needed provider substitution,
an advisor, a timeout override, ledger resolution, and a lower-level plan-run
recovery. That is too much manual protocol knowledge for an autopilot surface.

## Confirmed gaps

### G1 — High — a harness child can invoke parent commit authority

`autopilot` rejects `OMS_HARNESS_CHILD=1` at
`scripts/autopilot.sh:156-158`, but `goal-drive` has no equivalent guard. A
child can adopt an already reviewed task without another provider call
(`scripts/goal-drive.sh:1652-1662`), land it, and update `HEAD`
(`scripts/goal-drive.sh:1704-1705,1188-1190`). This was independently
reproduced. Add the parent-only guard to every commit-capable entrypoint and a
reviewed-task child regression.

### G2 — High — commit CAS is not bound to the branch ref

Publication checks only the old `HEAD` object, then runs `update-ref HEAD`
(`scripts/goal-drive.sh:1145-1190`). If a concurrent writer switches symbolic
`HEAD` to a sibling branch at the same old SHA, that sibling advances. The
outer branch check parks only after the local side effect
(`scripts/autopilot.sh:676-680`). Freeze the full refname in the intent and CAS
that explicit ref, then verify symbolic `HEAD` before and after publication.

### G3 — High — progress evidence follows an external symlink

`.oms/plan/progress.jsonl` is a fixed pathname
(`scripts/goal-drive.sh:105-106`), but terminal and intent rows use ordinary
append (`scripts/goal-drive.sh:162-177,256-270`); acceptance does the same at
`scripts/agent-plan.sh:1130-1163`. A pre-existing symlink therefore appends OMS
JSON outside the repository. Require a regular non-symlink leaf and use a
no-follow durable writer bound to the physical plan directory.

### G4 — High — acceptance text is frozen, not its oracle

Autopilot hashes and rechecks only the command string
(`scripts/autopilot.sh:662-689`), while `agent-plan accept` executes current
worktree bytes with unrestricted `bash -c` and no wall/output/side-effect bound
(`scripts/agent-plan.sh:1113-1169`). If allowed work includes a custom verifier
or transitive input not covered by patch admission's heuristic floor, a worker
can change the oracle without changing the hash. Declare and freeze the
plan-level verifier surface, run a base-owned floor, and bound the process.

### G5 — Medium — the final branch envelope is not revalidated

The complete NUL-delimited `base..HEAD` path check runs only before
`goal-drive` (`scripts/autopilot.sh:618-660`). Final checks bind branch, spec,
acceptance text, HEAD, tree, and plan, but not the allowed paths
(`scripts/autopilot.sh:676-843`). Normal patch admission is a strong defense;
same-UID commits or a faulty parent helper remain outside it. Re-run the whole
scope check after drive, around review, and before publication, and bind its
changed-path receipt to the Draft PR intent.

### G6 — Medium — unattended worker surface detection is not strict

`plan-run` invokes `peer-delegate` without `OMS_WORKER_GUARD_STRICT=1`
(`scripts/plan-run.sh:450-475`). The default guard reports some tracked,
untracked, or ignored primary-repository changes but does not always fail them;
final `git status --porcelain` also omits ignored external effects. Autopilot
should enable strict detection and document that process isolation is still
required for files outside the repository or reverted writes.

### G7 — Medium — parked recovery points at the wrong layer

The prompt surface tells every parked plan to resume with `oms goal-drive`
(`scripts/skill-router.sh:86-110`). Only the outer autopilot can interpret
`tasks-exhausted`, generate the one remainder proposal, run semantic review, or
prepare a Draft PR (`scripts/autopilot.sh:758-857`). In addition,
`--retry-known` exists only on `plan-run`; neither `goal-drive` nor `autopilot`
can forward it. Persist caller lineage and emit the exact outer continuation.

### G8 — Medium — the approved run envelope exists only in stdout

Planner/worker/reviewer, base, remote, caps, repair, review mode, Draft PR flag,
proposal path, and digest are assembled only in the printed command
(`scripts/autopilot.sh:457-471`). The durable plan stores spec digest and scope,
while `status` shows only plan, latest terminal, and a weak proposal summary
(`scripts/autopilot.sh:160-203`). Persist a CAS-backed outer run receipt and
make `status` render its one safe next command.

### G9 — Medium — provider budgets and routes are not an autopilot contract

All provider calls default to `OMS_PEER_TIMEOUT=5m`
(`scripts/lib/peer-common.sh:175-177`). `plan-run` exposes model, effort, and
known-failure retry controls, but `autopilot` exposes none. The live audit showed
that one repository-wide task can consume 5–15 minutes before writing. Add
per-stage wall/token/cost budgets, model/effort pins, task-size guidance, and a
bounded retry receipt to the outer contract.

### G10 — Medium — semantic gate composition and coverage are incomplete

Draft PR review defaults to advisory `shadow`; failure or incomplete review can
still publish. Prompt diff defaults to 65,536 bytes and marks truncation, but
gate mode does not require chunk/path coverage and its receipt hashes the
presented slice rather than the complete diff. Provider names differ, but exact
model family is not bound. The live gate also exposed self-interference:
`peer-review` appends its `_verify` artifact while the verifier runs
(`scripts/peer-review.sh:777-800`), so this repository's `.oms` purity check saw
the review's own record mutate and failed. Run verification against isolated
runtime state, make Draft publication gate-by-default, bind actual model routes
and the full-diff digest, and require chunk/path coverage receipts.

### G11 — Low — input and feature discovery are brittle

The skill router uses literal trigger substrings
(`scripts/lib/hook_state.py:1193-1209`), but the harness manifest includes only
indirect confirmed-spec and Draft-PR phrases, not `autopilot`, `오토파일럿`, or
`자율 코딩` (`skills.manifest.json:83-90`). Add direct multilingual triggers.
The plan parser also accepts the common Markdown form
``- Required checks: `command` `` without stripping or rejecting the backticks
(`scripts/plan-from-spec.sh:270-306`); this live plan then used command
substitution and failed with exit 127 after trying to execute check output.
Validate executable fields and add both regressions. Starting the bounded
workflow must remain an explicit parent action.

## Protections confirmed, not gaps

- Proposal bytes are digest-bound, regular-file-only, size-capped, schema
  checked, and atomically tied to plan/spec/source/scope.
- Foreign and malformed recovery branches, stale proposals, dirty trees, and
  pre-existing out-of-scope branch history fail closed.
- Worker patches are isolated, scope-admitted, verifier-checked, frozen, and
  committed locally with Git hooks suppressed inside `goal-drive`.
- Final HEAD/tree/plan/spec and acceptance are rechecked around a different
  provider's semantic review.
- Draft publication is create-only and cannot update an existing branch, merge,
  mark ready, tag, or release.

## Evaluation coverage

`tests/autopilot-smoke.sh` and `tests/goal-drive-recovery-smoke.sh` both passed
on the audit snapshot. They strongly cover proposal CAS/replay, malformed input,
pre-drive scope, recovery branches, forged parked receipts, frozen patch and
commit recovery, hooks, and several races. The worker's required quick check
also passed.

Missing regressions include child-to-`goal-drive` authority, symbolic-branch
swap at commit, progress-leaf symlinks, successful-drive terminal receipts,
post-drive scope, parked outer continuation, large-diff coverage, and real
planner/worker/reviewer composition. The admission full gate was attempted but
timed out after ten minutes. A staged semantic gate was also attempted: Claude
timed out and its mechanical backstop self-failed on the concurrently written
review artifact; Antigravity completed its read pass but emitted no required
typed gate verdict. The stored acceptance command was also executed directly
and reproduced the Markdown-backtick failure in G11. No live GitHub publication
was attempted.

## Recommended order

1. Close G1–G3: parent-only commit entrypoints, explicit-ref CAS, no-follow
   durable state.
2. Close G4–G6: frozen bounded acceptance, final scope rechecks, strict worker
   surface detection.
3. Close G7–G9: durable outer state, exact recovery, propagated retry and budget
   controls, smaller task decomposition.
4. Close G10: gate-by-default Draft PRs, model provenance, full-diff coverage.
5. Add G11 discovery plus the missing adversarial and live-canary evaluations.
