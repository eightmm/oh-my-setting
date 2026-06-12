# oh-my-setting

One setup that gives Codex, Claude Code, and Antigravity the same rules,
skills, and workflows on every machine. After install, everything is used by
talking to your coding agent — there is nothing to run in a terminal.

[한국어](README.ko.md)

## Install

The only shell step:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

Open a new shell once if freshly installed CLIs are not found. From here on,
your agent runs everything.

## Start

Open your coding agent in any directory — empty, mid-project, or ongoing —
and say:

```text
Start this project.
```

The agent detects the state and routes:

- empty dir → spec interview → `PROJECT.md` → template → safe skeleton → doctor
- existing repo → inspect the code, apply the template, fill `PROJECT.md` from
  the code, interview only for gaps
- ongoing project → read `PROJECT.md`, run the doctor, report status and the
  next step

No code is written before the spec is confirmed.

## What You Can Say

Project:

```text
Start this project.
Apply the oh-my-setting ml template.        # or: general, slurm
Run the oh-my-setting project doctor.
```

Review and advice (three local models in parallel):

```text
Run a multi-agent review of the current diff.
Run the ML pre-training review gate on this diff.
Export a Claude review prompt for this diff instead of calling Claude directly.
Import this Claude answer back into the artifact index.
Ask all three models with one debate round: vector DB or pgvector?
Delegate this to codex: add input validation to scripts/train.py.
```

Reusable code sources:

```text
Profile my GitHub and find reusable equivariant GNN code.
Register flowfrag/equivariant.py as flowfrag-equivariant.
Fetch flowfrag-equivariant into this project.
```

Experiments (ML):

```text
Launch this training run through the run ledger, note "lr sweep".
Record this eval's metrics.json into the ledger row.
Show the last 10 ledger entries.
Wait for Slurm job 12345 to finish, then digest it and report.
Check this molecular dataset's split for leakage before I train.
Frame this as a hypothesis-driven experiment before I launch the run.
Launch this as a registered research run with metric val_auc/scaffold.
Have all three models attack this hypothesis and experiment design before I train.
```

Memory and handoff:

```text
Remember for this repo: run scripts/check.sh fast before claiming done.
Pin for this repo: current task is the dataloader refactor.
Show the active task packet.
```

Maintenance:

```text
Check the oh-my-setting install status.
Update oh-my-setting and re-run its doctor.
Unlink oh-my-setting.                        # or: uninstall it completely
```

## What's Inside

Everything below is invoked by your coding agent on its own when the task
calls for it — you describe intent in chat, the agent picks the right script
or skill. Nothing here is meant to be run by hand.

| Area | Feature | What it does |
|---|---|---|
| Project | Start router + spec interview | Detects empty/existing/ongoing state, interviews in stages, confirms `PROJECT.md` before any code is written |
| Project | Templates (`apply-project-template.sh`) | Managed rule blocks for general/ml/slurm projects; ml adds a docs scaffold, `check.sh` verification contract, and `ml_smoke.py` one-batch contract |
| Project | Project doctor (`project-doctor.sh`) | Verifies every agent sees the same rules, spec state, and scaffold; warns on ML structure drift (stray root files, tracked data, missing `src/` layout) |
| Multi-agent | Review (`multi-agent-review.sh`) | Three local models review the diff in parallel; ML pre-training gate (`--ml`), debate rounds, per-finding verdicts |
| Multi-agent | Ask (`multi-agent-ask.sh`) | Same question to all three models for independent opinions; optional debate rounds and hypothesis design-attack preset |
| Multi-agent | Delegate (`multi-agent-delegate.sh`) | Runs a write task in an isolated git worktree, verifies it there, returns a reviewable patch; `--apply` only on a clean tree |
| Multi-agent | Single-agent router (`agent-run.sh`) | Routes one prompt to one provider: read-only questions to a call, write tasks to a delegate worktree |
| Multi-agent | Artifact index (`artifact-index.sh`) | Every cross-agent run lands under `.oms/artifacts/` with a JSONL index — list, latest, prune |
| Multi-agent | Safety rails (built-in) | Outbound prompts are scrubbed before any external CLI call (credentials, keys, machine/cluster details block the call); injected context is fenced; diffs are sanitized |
| Code sources | Registry (`code-source.sh`) | Local registry of trusted reusable files (e.g. personal model blocks); fetch by name into the current project |
| Code sources | GitHub fetch (`github-source.sh`) | Profile/discover/fetch via `gh`; no overwrite by default, provenance appended to `.oms/code-sources.jsonl` |
| Experiments | Run ledger (`run-ledger.sh`) | Wraps training runs: pre-flight `check.sh` gate, duplicate-run warning, one JSONL row per run in `docs/EXPERIMENTS.jsonl`, `--metrics` records eval scalars |
| Experiments | Research runner (`research-runner.sh`) | Registered research runs: hypothesis, pre-registered metric, and baseline recorded before launch, verdict after |
| Experiments | Job digest (`job-digest.sh`) | Compresses long logs or Slurm jobs into a compact digest; `--wait` blocks until the job finishes |
| Experiments | ML context (`agent-ml-context.sh`) | Compact ML digest (spec, ledger tail, configs) attached to cross-agent calls |
| Experiments | Cluster snapshots | Machine snapshot and a generated Slurm reference skill so agents know the local hardware and queues |
| Experiments | Domain skills | `ml-training` (optimizer/LR/DDP defaults), `chem-bio-ml` (splits, leakage, metrics), `research-method` (falsifiable-hypothesis loop), `slurm-hpc` |
| Memory | Shared memory (`agent-memory.sh`) | Compact cross-agent facts in `.oms/memory/`; sensitive content is rejected at write time |
| Memory | Task handoff (`agent-task.sh`) | Active task packet in `.oms/task/current.md` so any of the three agents continues the same work; closing promotes the outcome into memory |
| Maintenance | Install / update / doctor | One-line install symlinks the same rules and skills into all three agents; doctor checks links, tools, and manifest sync |
| Maintenance | Auto-update (`auto-update.sh`) | systemd timer or cron; check-only or apply mode (fast-forward + relink) |
| Maintenance | Backup / unlink / uninstall | Snapshot agent configs before changes; clean removal that restores what it replaced |

## Notes

- Local-first: no MCP servers, app connectors, or plugin connector tools.
- Never commit tokens, API keys, private data, or cluster/machine details.
- The scripts the agent runs live in `~/.oh-my-setting/scripts/` — documented
  for transparency and recovery, not for manual use.

## Star

If this helped: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
