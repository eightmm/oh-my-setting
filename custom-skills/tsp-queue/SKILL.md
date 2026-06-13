---
name: tsp-queue
description: >
  Single-workstation GPU job queue helper using tsp/task-spooler. Use for
  non-Slurm background training, sequential local runs, single-machine GPU
  queueing, or "줄 세우기" on a shared box.
---

Default: one local GPU queue, sequential unless the user asks otherwise.

## Rules

- Prefer the queue over launching many GPU jobs at once on a shared workstation.
- Default to `--slots 1` to avoid OOM; raise slots only when resource use is clear.
- Never put secrets on the command line or in ledger notes; pass them via environment or files.
- Use `~/.oh-my-setting/scripts/tsp-queue.sh` as the entrypoint. It records completions to the run ledger.
- Cluster jobs go through `slurm-hpc`; single-box jobs go through `tsp-queue`.

## Commands

| Command | Use |
| --- | --- |
| `tsp-queue.sh enqueue [--label L] [--slots N] [--ledger-note NOTE] -- CMD...` | Queue a run and print the job id. |
| `tsp-queue.sh list` | Show the queue table. |
| `tsp-queue.sh cancel <id>` | Remove one queued job. |
| `tsp-queue.sh cancel --all` | Remove listed jobs and clear finished rows. |
| `tsp-queue.sh wait [<id>]` | Wait for completion and append a ledger row. |
| `tsp-queue.sh logs <id>` | Print job stdout/stderr. |
