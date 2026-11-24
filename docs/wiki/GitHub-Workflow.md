# GitHub Workflow

Work moves from an **issue** to an **OMS plan**, then to a **verified PR**,
and ends with **merge evidence**. Back to [Home](https://github.com/eightmm/oh-my-setting/wiki).

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

Optionally, view the same issues and PRs in a GitHub Projects
[table, board or roadmap](https://docs.github.com/en/issues/planning-and-tracking-with-projects/customizing-views-in-your-project/changing-the-layout-of-a-view).

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
