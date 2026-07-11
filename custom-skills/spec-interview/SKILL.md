---
name: spec-interview
description: >
  Specification-first interview workflow. Use when starting or onboarding a
  project, resuming unresolved spec work, or planning a broad, ambiguous, or
  architecture-shaping feature, refactor, workflow, CLI, app, or research
  pipeline. Detect project state first, then interview only for decisions that
  materially affect implementation. Clear bounded changes do not need a spec
  interview; the user never runs setup commands in a shell.
---

Default: no coding before the task-relevant contract is clear. Bounded changes
with a clear local contract do not require an interview.

Bounded changes with a clear local contract do not require an interview.

## Trigger

Use when the request is a new project, unresolved spec resume, vague, broad,
architecture-shaping, or explicitly asks for a spec/interview. Do not trigger
from generic words such as `feature`, `refactor`, `build`, or `make` alone.

## Start Router

Any project start/resume trigger enters here. Detect state read-only first —
never ask the user which case applies:

| State | Signals | Route |
|---|---|---|
| New project | no oh-my-setting managed blocks, no/trivial source tree | full interview -> `PROJECT.md` -> confirm -> Project Bootstrap |
| Existing repo, not onboarded | source files present; no managed blocks or no `PROJECT.md` | inspect code/configs/git first; apply template (`auto`); fill `PROJECT.md` from the code; interview only for gaps the code cannot answer; confirm; doctor |
| Ongoing, `PROJECT.md` draft | managed blocks + `PROJECT.md` with `State: draft` | inspect Open decisions; resume the interview only when they affect the requested change |
| Ongoing, confirmed | managed blocks + `State: confirmed` | read `PROJECT.md` (Current Task, Verification), run project doctor, report status, propose next step — no interview unless spec and reality have drifted |

Signals: `<!-- oh-my-setting:*:begin -->` blocks in `AGENTS.md`/`CLAUDE.md`,
`PROJECT.md` `- State:` field, presence of tracked source files.

## Flow

For clear bounded work, stop after read-only detection and proceed using the
existing local contract. Otherwise:

1. Stage 1 intent: ask goal, users/workflow, non-goals. No implementation.
2. Stage 2 scope: ask interface/API/CLI, data/files, paths, constraints.
3. Stage 3 execution: ask commands, verification, risks, resources.
4. Spec: write/update `PROJECT.md` for project work; otherwise write compact spec.
5. Gate: list assumptions and unresolved ambiguity.
6. Proceed only when user confirms. For project starts, `PROJECT.md` state must be `confirmed`.
7. For new-project starts, run Project Bootstrap right after confirmation — do not wait for another prompt.

## Question Rules

- Ask only questions that change implementation or verification.
- Native question UI is MANDATORY when the harness provides one: Claude Code
  `AskUserQuestion`, Codex `request_user_input`. Do not ask interview
  questions as plain chat text in those harnesses.
- Markdown questions are the fallback only when no question UI tool exists
  (Antigravity, `codex exec`/non-interactive runs).
- Prefer multiple-choice when options are known.
- Offer 2-4 choices for each question when practical.
- If one default is clearly best, mark it `(recommended)` and explain why in
  one short phrase.
- Always leave a free-form escape hatch: `Other: ...`.
- Let the user answer compactly, such as `1A 2B 3D: custom detail`.
- Move stage by stage; do not ask every possible question at once.
- Stop asking when remaining unknowns are local and low risk.

## Question Format

When native question UI is available, present one question at a time with
choices and an `Other` option.

When using Markdown, format questions like:

```md
1. Phase 1 exit metric?
   A. Pearson >= 0.65 on cold-protein CV (recommended) - matches current goal.
   B. Spearman >= 0.65 - rank-focused.
   C. RMSE <= 0.8 - error-focused.
   D. Other: specify metric and threshold.
```

## Blockers

Must ask before coding if unclear:
- core user/workflow
- project goal, scope, or non-goals
- data model or persistence
- public API/CLI contract
- auth/security/privacy
- destructive or expensive operations
- Slurm/HPC resources for heavy jobs
- acceptance criteria or verification
- missing or draft `PROJECT.md` for a project start, or unresolved choices that
  affect the requested change

## Output

For project start, create/update:

```md
# PROJECT.md

## Status
- State: draft | confirmed

## Interview
- Stage 1 intent:
- Stage 2 scope:
- Stage 3 execution:
- Open decisions:

## Project
- Name:
- Type:
- Goal:
- Users/workflow:
- Scope:
- Non-goals:

## Commands
- Setup:
- Test:
- Run:
- Lint/typecheck:

## Paths
- Data:
- Config:
- Outputs/logs:
- Checkpoints:

## Verification
- Success criteria:
- Required checks:
- Baseline/metric:
```

Keep `State: draft` while questions remain. Set `State: confirmed` only after user confirmation.

## Project Bootstrap

New-project starts only. Runs after `PROJECT.md` is `confirmed`. Goal: the
user starts a project entirely in chat — no shell commands typed by hand.

1. Pick the template type from interview answers, not auto-detect (an empty
   dir has nothing to detect):
   - training/ML/research pipeline -> `ml`
   - everything else -> `general`
   - onboarding an existing repo -> `auto`
   - Slurm overlay is added by the script on Slurm machines.
2. Apply: `~/.oh-my-setting/scripts/apply-project-template.sh <type> .`
3. Scaffold the safe skeleton only — structure, no feature logic:
   - `git init` if not a repo.
   - Python: `uv init`, `uv add` confirmed deps, `uv sync`, src layout.
   - Directories from `PROJECT.md` Paths (e.g. `src/<pkg>/`, `scripts/`,
     `tests/`, `configs/`; ML adds `data/`, `outputs/` — gitignored by the
     template).
   - Never overwrite existing files.
4. Verify: `~/.oh-my-setting/scripts/project-doctor.sh .`; fix template/sync
   issues it reports, rerun, then report the result.
5. Report: template type, created/changed files, doctor result, and the next
   step (first feature interview or implementation start).

Feature code, API/data/compute changes, and dependency additions beyond the
confirmed spec still require separate confirmation.

For non-project work, keep compact:

```md
Spec:
- Goal:
- Non-goals:
- Users:
- Scope:
- Constraints:
- Interface:
- Data:
- Success:
- Verification:
- Assumptions:
- Open questions:
```

Then ask: `Confirm PROJECT.md/spec, or answer open questions.`
