---
name: oms-panel-orchestrator
description: >
  Use when asked to act as the OMS panel control tower: split work across
  native provider leads or bounded workers with oms panel, or
  "control tower", "panel orchestrat", "패널로 분배", "일 분배", "직접 다하지말고",
  "oms panel로", "멀티 계정 병렬", $oms-panel-orchestrator or
  /oms-panel-orchestrator. Skip single self-contained edits.
---

# Panel Orchestrator

The user chooses the sole Overall Main from the installed panel launch
options. Main scopes and assigns the work, reviews evidence, verifies the
integrated result and owns acceptance. It limits direct edits to necessary
integration and unblocking. Commit, push, install or publish only with explicit
user authority.

## Choose the execution path

- In an OMS-managed tmux panel, open or reuse an explicitly assigned native
  Provider Lead with `oms panel --orchestrator PROVIDER --owner OWNER --room ROOM_ID`
  (`codex`, `claude`, or `antigravity`). Use the same command with
  `--task "ASSIGNMENT"` to send the role, scope and completion checks to the
  connected, idle session.
- Leads assign independent work to native children when the provider supports it,
  with disjoint paths, checks and least permissions. They wait for results,
  review evidence and verify their integrated scope. Limit direct Lead edits
  to necessary integration and unblocking. Leaf children return work to their
  parent without delegating further.
- Use managed dispatch below for a bounded leaf task or when a native Lead is
  unavailable, including inline and Herdr sessions. Main retains admission
  and publication authority. Verify native returns against the task before
  accepting them. See the
  [delegation contract](../oms-agent-harness/references/delegation-artifacts.md)
  for recovery and evidence.

## Decompose and dispatch

- Split work into disjoint write slices; give each `--scope PATH` (repeat per
  path). Overlapping scopes serialize; do not dispatch them together.
- Follow the effective dispatch WIP limit and available review capacity.
  Start independent ready work as capacity frees up. Respect native capacity
  and permission limits when coordinating through a Lead.
- Dispatch independent write slices concurrently with explicit write, brief, and
  verifier flags:
  `oms panel --dispatch builder --access write --owner OWNER --room ROOM_ID --task-id ID --brief-file BRIEF.md --verify "CMD" --scope PATH`
- Dispatch read-only lookups concurrently without `--access write`:
  `oms panel --dispatch researcher --owner OWNER --room ROOM_ID --prompt "..."`
- Briefs state goal, scope, out-of-scope, a checkable done condition, and stay
  outbound-scrubber clean: no home/temp absolute paths, no secret-word
  assignments (build such fixtures at runtime).

## Providers and accounts

- Preserve the user's Main and account choices. Inspect the installed session
  and cached `oms models` catalog before selecting an exact model and its
  supported effort. Check account access and spend authority separately.
- Choose the worker role separately from the provider. Use configured tier routing
  by default and inspect `oms panel --routes` before explicitly pinning a provider
  or model. `provider-accounts.json` has `spread` and `ordered` policies.
- Use `oms connect-services --account codex:ID` or
  `oms connect-services --account claude:ID` to register an additional account.
  Claude account verification reports that credential origin cannot be attested.
  Report the actual account only when the runtime has verified it.
- Continue a managed Worker with `--continue TASK_ID` (add `--to` when switching
  providers). Reuse the connected Lead for its follow-up.
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
