# GitHub Workflow

Work moves from an **issue** to an **OMS plan**, then to a **verified PR**,
and ends with **merge evidence**. Back to [Home](https://github.com/eightmm/oh-my-setting/blob/main/docs/wiki/Home.md).

Source of truth: this page summarizes [CONTRIBUTING.md](https://github.com/eightmm/oh-my-setting/blob/main/CONTRIBUTING.md) and [Verification and release](https://github.com/eightmm/oh-my-setting/blob/main/docs/COMPONENTS.md#verification-and-release), and they win if they differ.

## 1. Issue

Choose [Work item](https://github.com/eightmm/oh-my-setting/issues/new/choose)
with a Goal, Scope, Acceptance criteria and Verification. Risk and Related are
optional. If you open an issue through the API or `gh`, write the same `###`
headings by hand.

## 2. Plan

[`.github/oms-project.md`](https://github.com/eightmm/oh-my-setting/blob/main/.github/oms-project.md)
turns on GitHub project mode when its slug matches the github.com `origin`.
`oms agent-plan add`, `add-batch` and `apply-proposal` each require an issue per
new task. Use `--issue REF` on `add` or inherit an issue with `--discovered-from
TASK`. Set `issue` on each task row in batch and proposal input. `oms agent-task
init|update|rotate --issue REF` binds the handoff. Use `N`, `#N` or `owner/repo#N`
from the same repository. See [project mode](https://github.com/eightmm/oh-my-setting/blob/main/docs/COMPONENTS.md#ci-selection-and-execution).

Here, **GitHub project mode means repository and Issue binding**. The declaration
contains an `owner/repo` slug; it does not configure a GitHub Projects v2 board,
item IDs or field mappings. The current `oms github` actions are `status`,
`refresh`, `adopt-merged` and `post-evidence`, as described in
[GitHub commands](https://github.com/eightmm/oh-my-setting/blob/main/docs/COMPONENTS.md#ci-selection-and-execution).

### Optional Project intake and synchronization

Use [OMS Development](https://github.com/users/eightmm/projects/3) as the shared
development backlog. A Project is useful when several providers, UI hosts and
implementation phases compete for attention; a small, explicitly selected task
can still use the Issue/Plan/PR path directly. The
[control surface roadmap](https://github.com/eightmm/oh-my-setting/blob/main/docs/CONTROL-SURFACE-ROADMAP.md)
defines the initial fields and views. The integration below is **planned work**:
read-only candidate intake is tracked by
[#447](https://github.com/eightmm/oh-my-setting/issues/447) (P1, Foundation),
followed by opt-in metadata synchronization in
[#439](https://github.com/eightmm/oh-my-setting/issues/439) (P2, Extensions).
Neither is an existing automatic OMS scheduler or synchronization command.

| Record | Responsibility | OMS use |
|---|---|---|
| Project | Priority, Area, Phase and development Status across Issues/PRs | Suggest the next Issue and show progress |
| Issue | Goal, scope, dependencies and acceptance criteria | Bind each adopted task to its source requirement |
| Adopted OMS Plan and runtime | Task decomposition, claims, leases, execution scope, approvals and verification evidence | Decide whether an authorized task can execute |
| Git and PR | Exact changes, review, CI and merge evidence | Prove what changed and whether it landed |

`Ready` makes an Issue a candidate. Moving a card never starts a run, spends
budget, creates approval or merges a PR. A GitHub assignee or `In progress` card
does not acquire an OMS lease. Project data is external input: descriptions and
comments cannot introduce executable instructions or widen an adopted scope.

**Phase A: read candidates ([#447](https://github.com/eightmm/oh-my-setting/issues/447)).**
Start with an explicit project binding to owner
`eightmm`, project number `3`, and repository `eightmm/oh-my-setting`. Resolve
the Project node ID and inspect its field and option IDs before use. Read all
required pages and record the source, fetch time and completeness. Present
eligible open `Ready` Issues with priority, phase, dependencies and the reason for
selection; exclude PR-only cards, draft ideas, archived items and work already
claimed in OMS. Re-read the selected Issue and recheck prerequisites before
using the existing Issue-bound plan and claim flow. Explicitly adopted scope
and the current execution authorization still govern that flow.

Unknown fields, redacted items, unavailable Project permissions or stale reads
must remain visible. They cannot mean an empty backlog, completed dependency or
permission to execute. Continue through an explicitly selected Issue when the
optional Project is unavailable; do not silently choose another task. Board
membership and presentation order alone do not establish a dependency graph.

**Phase B: synchronize selected metadata ([#439](https://github.com/eightmm/oh-my-setting/issues/439)).**
After the read path in #447 is proven, add
opt-in writes to agreed fields. Preserve human-owned Priority, Area and Phase;
define one writer for each automated field so native Project workflows and OMS
do not compete. For example, an accepted runtime claim can propose `In progress`,
a non-draft PR explicitly ready for review can propose `Review`, and a blocker
can propose `Blocked`. A draft PR stays `In progress`. Mark an
Issue `Done` only after its acceptance criteria are satisfied and the relevant
completion evidence is recorded. For code delivery, this includes the required
merge evidence; a merged PR may cover only part of an Issue. Closing an
unmerged PR is not completion.

Serialize OMS writes, re-read current values, compare with the recorded snapshot,
and stop on a detected conflict. A read followed by a GitHub mutation is not an
atomic compare-and-swap, so verify the result and surface races instead of
claiming strict exclusion. Keep a bounded, sanitized receipt for each intended
transition; after a timeout, reconcile the current value before retrying. Handle
pagination, rate limits, field renames/deletions and duplicate delivery without
overwriting unrelated human changes or creating repeated cards/comments.

The official [`gh project`](https://cli.github.com/manual/gh_project) commands
can inspect projects and edit items; [`gh api graphql`](https://docs.github.com/en/issues/planning-and-tracking-with-projects/automating-your-project/using-the-api-to-manage-projects)
can query Project node IDs and apply typed mutations. These are transport
choices inside the adapter, with the same OMS contracts. For read-only access,
GitHub documents `read:project`; write access needs the appropriate Project
permissions, with `project` documented for classic-token scopes. Confirm the
installed CLI and authentication type rather than expanding credentials as part
of discovery. Automation should resolve IDs instead of assuming visible field
names are stable. Project custom fields and Issue/PR properties have different
API owners; updating a Project field does not update an Issue's labels or
assignees.

## 3. Verified PR

1. Name OMS work branches `oms/*` and write commits as `<type>: <description>`.
2. Run `bash scripts/check.sh --preflight`, the nearest tests (for example
   `cargo +1.99.1 test --locked -p CRATE --test TARGET`, or `--focused-only` for
   shell suites), then `bash scripts/check.sh --affected --changed-from BASE --changed-to HEAD`.
3. Fill in every heading of the [PR template](https://github.com/eightmm/oh-my-setting/blob/main/.github/pull_request_template.md):
   - Linked issue (`Closes #N` or `Refs owner/repo#N`)
   - Summary
   - Decision and evidence
   - Verification (commands run, their results, and any skipped checks)
   - Remaining follow-up
   - Risk and rollback

   Write "None" under a heading rather than deleting it. No tool validates
   the headings yet.
4. CI runs on Linux only. The stable `gate` check covers the selected jobs.
   See [CI](https://github.com/eightmm/oh-my-setting/blob/main/docs/COMPONENTS.md#ci-selection-and-execution).
5. Publish:
   - `oms land` gates the exact committed tree, pushes that SHA and waits for
     CI on it.
   - `oms draft-pr` can open a create-only Draft PR instead.
   - Merging stays an explicit owner action.

## 4. Merge evidence

After the merge, run `oms github refresh --pull REF`. Then the parent (the
Main or owner, never a worker) runs
`oms github post-evidence --issue REF --pull REF --yes`. It needs a fresh
cache row showing the PR as merged. It posts one idempotent comment that names
the land receipt when one is recorded. See the
[GitHub commands](https://github.com/eightmm/oh-my-setting/blob/main/docs/COMPONENTS.md#ci-selection-and-execution).
