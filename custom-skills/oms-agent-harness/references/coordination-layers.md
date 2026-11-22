# Coordination Layers

A team has three layers. A layer says who manages, who integrates and who
produces. A [worker role](roles.md) says how one leaf does its task. Choose
them separately: a layer comes from the assignment, never from a role label,
provider or model.

| Layer | One line | Count |
|---|---|---|
| Overall Main | Manager. Plans, assigns, reviews, decides. Produces nothing. | One per Room and integration target |
| Provider Lead | Team lead. Runs one assignment with native children and integrates it. | Native sessions Main opens, per provider |
| Leaf Worker | Producer. Does one bounded task in one role and returns evidence. | As capacity allows |

These layers apply while a team runs: a panel Main, or a session the user asked
to act as control tower. A solo session with no team does its own work and
follows the ordinary allocation in
[delegation-artifacts.md](delegation-artifacts.md).

## Who does what

| Function | Overall Main | Provider Lead | Leaf Worker |
|---|---|---|---|
| Goal, scope, acceptance criteria | Owns, with the user | Follows the assignment | Follows the brief |
| Split and order work | Across teams and providers | Inside its assignment | Proposes a split to its parent |
| Assign work | Leads and managed workers | Native children only | None |
| Read source | Returned evidence and decisive text | Its assigned scope | Its task |
| Edit repository files | None | Small integration or unblocking edits in scope | Assigned paths, write roles only |
| Review results | Evidence review, independent gate | First review of child results | Reviewer roles, in a separate context |
| Verify | Integrated result, recorded checks | Its integrated scope | Checks named in the brief |
| Admit, accept, finalize | Owns | None; reports to Main | None |
| Commit, push, land, install, publish | With user authority | None | None |
| Talk to the user | Owns | Through Main | Through its parent |

## Overall Main manages

Main decides; workers produce. Main runs OMS commands, reads returned results
and the decisive part of a diff, runs deterministic verification commands,
admits and applies worker patches with `oms patch-admit`, `oms panel --accept`
or `oms patch-land`, and reports to the user. It writes no code, test, document or
configuration and makes no repository edit, including small ones.

| Main needs | Route |
|---|---|
| A code, test, document or configuration change | `oms panel --dispatch builder`, `refactorer` or `documenter` with `--access write`, or a Lead `--task` |
| A fix for a failed check or review finding | `oms panel --continue TASK_ID --brief-file F` to the original writer |
| A merge conflict or glue between slices | `oms panel --dispatch integrator --access write --scope PATH` |
| Source reading beyond the returned evidence | `researcher`, or `scholar` for literature |
| A design choice or its failure modes | `architect`, `advisor` or `critic` |
| Requirement-by-requirement closure | `verifier`, then `oms peer-review --gate` |
| A deterministic check | Run it directly; it needs no paid seat |

Two exceptions, both visible:

- The user tells Main to make a specific change itself. Main makes that change
  only and records it in the Room as a `--kind status` message. A Claude Code
  Main's edit tools are refused inside the managed repository unless the user
  started the panel with `OMS_MAIN_WRITE_GUARD=0`; otherwise the user edits.
  Brief files go to session scratch or `TMPDIR`, outside the checkout.
- No worker can start (no provider, capacity or authority). Main reports the
  blocker to the user and waits; it does not do the work quietly.

Small edits cost less on a routine or light worker than in Main's context, so
size alone is no reason for Main to edit.

## Provider Lead leads one team

A Lead owns one assignment from Main at a time: plan the slice, brief native
children with a role, disjoint paths and checks, wait, review their evidence,
integrate inside the assigned scope, run the scope's checks and report.

- Substantive work goes to children. The Lead's own edits stay small:
  integration and unblocking inside its scope.
- Without native children on that provider, the Lead does the slice itself as
  its worker and says so in the report.
- A Lead returns the decision to Main when the assignment needs a wider scope,
  paths another team holds, another provider or an OMS-managed worker, new
  permission or spending, or acceptance and landing.
- Report changed paths, evidence, checks run and skipped, blockers and
  unverified child access. A report is not acceptance.

## Leaf Worker produces

One role, one bounded task, the access that role allows. A worker returns
findings or a patch with evidence, the checks it ran and what it could not
decide. It does not delegate, widen scope, commit, push, land, install or
publish, even when its native CLI could.

## Escalation

Each layer asks only the layer above: Worker to its parent, Lead to Main, Main
to the user. Main asks the user about scope, authority, interface or risk, not
about work it can route.

## Declared, not enforced

These duties are prompt and skill contracts. The runtime still enforces the
parts it always did: parent-only managed dispatch, worker worktree isolation,
patch admission, scopes, leases and landing gates. No guard yet refuses an
edit typed in the Main pane, so treat a Main edit without a recorded user
instruction as a contract violation to report, not as a blocked action.
