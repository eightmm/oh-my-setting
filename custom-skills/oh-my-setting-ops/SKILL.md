---
name: oh-my-setting-ops
description: >
  Maintain an oh-my-setting installation from chat: status, update, doctor,
  duplicate-skill cleanup, local snapshots, rollback, or uninstall. Use
  `agent-harness` instead for repository task state.
---

# oh-my-setting Operations

Run the local scripts for the user; do not send them to external connectors.

1. Orient with `~/.oh-my-setting/scripts/status.sh` and use the canonical
   receipt owner it reports.
2. Use `update.sh` for updates, `doctor.sh` for install health, and
   `skill-doctor.sh` for duplicate/missing skill entries. The standard doctor
   automatically reports model/CLI capability drift when a provider is present;
   use `--strict-diversity` or `--live-models` only when the task needs those
   checks to become a blocking gate. `--no-model-doctor` is a recovery-only
   escape hatch, not a normal operating mode.
3. For legacy links, run `cleanup.sh --dry-run`, inspect its exact scope, then
   use `--apply` when cleanup is authorized. Never delete unrelated plugins,
   caches, regular files, or directories.
4. After update, repair, cleanup, or snapshot regeneration, run `doctor.sh` and
   report remaining warnings.

Machine and Slurm snapshot generators support `--dry-run` and `--check`; their
output is private local state. A stale UI skill list may require a new agent
session after the installed files are already correct.

Report the commands run, version/owner, and doctor result. Use compact prose;
do not force an implementation-style report for a status question.
