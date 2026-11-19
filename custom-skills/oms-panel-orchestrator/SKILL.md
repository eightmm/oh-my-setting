---
name: oms-panel-orchestrator
description: >
  Use when asked to act as the OMS panel control tower: split work across
  native Codex, Claude and AGY leads or bounded workers with oms panel, or
  "control tower", "panel orchestrat", "패널로 분배", "일 분배", "직접 다하지말고",
  "oms panel로", "멀티 계정 병렬", $oms-panel-orchestrator or
  /oms-panel-orchestrator. Skip single self-contained edits.
---

# Panel Orchestrator

The user chooses the sole Overall Main from Codex, Claude Code or AGY. Main
owns task scope, admission, verification, landing and final acceptance. It does
not hand-edit paths assigned to an active Lead or Worker.
Authority comes from the user: never commit, push, install or publish without
explicit user authority.

## Choose the execution path

- A Provider Lead is an explicitly assigned native session under Overall Main.
  Open it with `oms panel --orchestrator PROVIDER --owner OWNER --room ROOM_ID`
  (`codex`, `claude`, or `antigravity`), then use the same command with
  `--task "ASSIGNMENT"` to submit work to the connected, idle session.
- A Lead may complete its assigned scope directly. Use native subagents or team
  tools only for independent useful work, with bounded scopes and completion
  checks. Respect effective native limits; unused capacity is not a reason to
  spawn. Leaf children do not recursively delegate.
- Native orchestration requires a verified Main in an OMS-managed tmux panel,
  outside a managed campaign. For unsupported hosts or a bounded leaf task,
  use `--dispatch` below. A dispatched Worker is not a Provider Lead and cannot
  recursively delegate or use OMS parent-only actions.
- Main retains ownership and publication authority across both paths. Native
  child completion does not establish OMS admission, review or final acceptance.
  Load [delegation-artifacts.md](../oms-agent-harness/references/delegation-artifacts.md)
  for continuation, recovery and evidence details.

## Decompose and dispatch

- Split work into disjoint write slices; give each `--scope PATH` (repeat per
  path). Overlapping scopes serialize; do not dispatch them together.
- Stay within the runtime's effective WIP limit and available review capacity.
  Start independent ready work when useful; do not fill slots to reach a target
  or wait for an unnecessary wave barrier. The dispatch WIP cap does not bound
  native children.
- Dispatch independent write slices concurrently with explicit write, brief, and
  verifier flags:
  `oms panel --dispatch builder --access write --owner OWNER --room ROOM_ID --task-id ID --brief-file BRIEF.md --verify "CMD" --scope PATH`
- Dispatch read-only lookups concurrently without `--access write`:
  `oms panel --dispatch researcher --owner OWNER --room ROOM_ID --prompt "..."`
- Briefs state goal, scope, out-of-scope, a checkable done condition, and stay
  outbound-scrubber clean: no home/temp absolute paths, no secret-word
  assignments (build such fixtures at runtime).

## Providers and accounts

- Honor the user's Main choice; there is no preferred provider or fixed model
  ranking. Inspect the installed session and cached `oms models` catalog before
  choosing an exact model and its supported effort. Discovery does not prove
  live access or authorize a model, account or billing change.
- For bounded Workers, use configured tier routing by default; inspect
  `oms panel --routes` before an explicit provider/model pin.
- Additional accounts can be registered with `oms connect-services --account
  codex:ID` or `--account claude:ID`. Registration is not proof of credential
  isolation: Claude `--verify` currently reports that credential origin cannot
  be attested. Preserve explicit account choices and report that limitation.
- Keep a dispatched Worker's native context and fixes with `--continue TASK_ID`
  (add `--to` when switching providers); reuse a connected Lead for its follow-up.
- After an interrupted write attempt, run `--salvage TASK_ID` first, before
  any fresh dispatch of that slice.

## Room and board

- Record task switches for the shared board:
  `oms room send --id ROOM --participant PART --to all --kind status --text "..."`
- At safe points read `oms room updates`; after consuming a message run
  `oms room ack --message ID`. Membership grants no authority.

## Admission, verification, isolation

- Integrate in a scratch worktree (`oms scratch-worktree add`); never in the
  shared checkout. Set `OMS_ARTIFACT_INDEX_KEEP=4000` so artifact indexes
  survive compaction.
- Use separate `CARGO_TARGET_DIR` values for `oms patch-admit` and for direct
  worktree verification so they never contend for one build lock.
- Gate the whole change with an independent cross-family
  `oms peer-review --gate`; the author family does not review itself.

## Landing and finalizing

- Land slices using one of two valid sequences:
  - Commit slice-by-slice in order with `oms panel --accept TASK --repo WORKTREE --message "type: summary" --verify "CMD"` or `oms patch-land`.
  - When deferring commits in a single scratch worktree, run `oms patch-admit` for every disjoint slice against the clean base before applying those patches into the scratch worktree, then run `oms peer-review --gate` on the combined diff.
- Close each reviewed worker run immediately with all required flags:
  - Local or non-acceptance closure (`completed` before publication, including after local-only `oms panel --accept`, or `failed` / `rejected`):
    `oms panel --finalize --owner OWNER --task-id ID --summary-file SUMMARY.txt --outcome completed`
  - Published or native-completed acceptance closure (`accepted` requires the native plan task to be `done` or `--landed-commit SHA` with a validated publication receipt, plus a passing `--verify "CMD"`; `--evidence PATH` only adds artifact references when automatic artifact evidence is absent):
    `oms panel --finalize --owner OWNER --task-id ID --summary-file SUMMARY.txt --outcome accepted --verify "CMD" [--landed-commit SHA] [--evidence PATH]`
  - Keep `SUMMARY.txt` free of private paths and secrets.
- Report per slice: changed paths, checks actually run, remaining risk.
