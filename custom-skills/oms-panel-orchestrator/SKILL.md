---
name: oms-panel-orchestrator
description: >
  Use when asked to act as the OMS panel control tower: split work across
  parallel workers with oms panel, balance claude and codex accounts, or
  "control tower", "panel orchestrat", "패널로 분배", "일 분배", "직접 다하지말고",
  "oms panel로", "멀티 계정 병렬", $oms-panel-orchestrator or
  /oms-panel-orchestrator. Skip single self-contained edits.
---

# Panel Orchestrator

The sole panel `main` is a control tower: it decomposes, dispatches, reviews,
lands and finalizes. Workers write; main does not hand-edit sliced paths.
Authority comes from the user: never commit, push, install or publish without
explicit user authority.

## Decompose and dispatch

- Split work into disjoint write slices; give each `--scope PATH` (repeat per
  path). Overlapping scopes serialize; do not dispatch them together.
- Keep WIP <= 6. Refill a free slot as soon as one slice finishes; do not wait
  for a wave barrier.
- Dispatch independent write slices concurrently with explicit write, brief, and
  verifier flags:
  `oms panel --dispatch builder --access write --owner OWNER --room ROOM_ID --task-id ID --brief-file BRIEF.md --verify "CMD" --scope PATH`
- Dispatch read-only lookups concurrently without `--access write`:
  `oms panel --dispatch researcher --owner OWNER --room ROOM_ID --prompt "..."`
- Briefs state goal, scope, out-of-scope, a checkable done condition, and stay
  outbound-scrubber clean: no home/temp absolute paths, no secret-word
  assignments (build such fixtures at runtime).

## Providers and accounts

- Let tier routing balance `claude` and `codex` by quota and room spread by
  default, or pin a provider explicitly with `--to claude` or `--to codex`.
- Register additional isolated Codex accounts with
  `oms connect-services --account codex:ID` (`provider-accounts.json` holds
  `policy: spread` or `ordered`; `--account claude:ID` is refused while native
  Claude account isolation is unavailable).
- Keep a worker's native context and fixes: `--continue TASK_ID` (add `--to`
  when switching providers).
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
