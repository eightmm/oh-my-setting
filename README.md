# oh-my-setting

One setup that gives Codex, Claude Code, and Antigravity the same rules,
skills, and agent harness on every machine. After install, everything is used by
talking to your coding agent — there is nothing to run in a terminal.

[한국어](README.ko.md)

## Install

The only shell step:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

The default install is minimal: rules, skills, dispatcher, and provider hooks
for CLIs already present. It does not install provider CLIs, modify `.bashrc`,
write a machine snapshot, register an update timer, or show a star prompt.
For the former all-in-one setup:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash -s -- --full
```

Open a new shell once if `--full` installed new CLIs. From here on, your agent
runs everything.

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

Architecture-shaping work waits for the relevant specification decisions;
clear bounded changes may proceed from inspected local contracts.

## What You Can Say

Project:

```text
Start this project.
Apply the oh-my-setting ml template.        # or: general, slurm
Run the oh-my-setting project doctor.
```

Review and advice (three local models in parallel):

```text
Run a peer review of the current diff.
Run a gated peer review of this diff — pass or fail.
Run the ML pre-training review gate on this diff.
Export a Claude review prompt for this diff instead of calling Claude directly.
Import this Claude answer back into the artifact index.
Ask all three models with one debate round: vector DB or pgvector?
Delegate this to codex: add input validation to scripts/train.py.
Delegate this to codex with up to 2 repair rounds if verification fails.
Create a task-scoped executor soul for this work, freeze it, then delegate it.
Admit codex's patch — run the checks ladder before applying it.
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
Show the top runs by val_auc.
Show today's harness timeline — what did the agents do here?
Wait for Slurm job 12345 to finish, then digest it and report.
Capture this run in a reproducibility capsule with config.yaml and seed 7.
Which run produced ckpt/best.pt?
Diff run A against run B and show the metric deltas.
Check this molecular dataset's split for leakage before I train.
Claim this experiment on the board so the other agents don't rerun it.
Reconcile my Slurm jobs and write their final state into shared memory.
Queue this training run on the single-GPU box so it waits its turn.
Frame this as a hypothesis-driven experiment before I launch the run.
Launch this as a registered research run with metric val_auc/scaffold.
Have all three models attack this hypothesis and experiment design before I train.
```

Memory and handoff:

```text
Remember for this repo: run scripts/check.sh fast before claiming done.
Pin for this repo: current task is the dataloader refactor.
Show the active task packet.
Reclaim expired agent claims (and abandoned reviews) on the shared plan.
Hand off my last Codex session here so you can continue it.
```

Maintenance:

```text
Check the oh-my-setting install status.
Update oh-my-setting and re-run its doctor.
Fix duplicate skill-picker entries and clean legacy oh-my-setting links.
Unlink oh-my-setting.                        # or: uninstall it completely
```

## What's Inside

Everything is invoked by your coding agent on its own when the task calls for
it — you describe intent in chat, the agent picks the right script or skill.
Nothing here is meant to be run by hand. These are the capability groups; the
full per-script catalog is in [docs/COMPONENTS.md](docs/COMPONENTS.md).

| Capability | What it gives you |
|---|---|
| Project bootstrap | Start router + staged spec interview, general/ml/slurm templates, `PROJECT.md` gate, and a project doctor that keeps all three agents in sync |
| Multi-agent review & delegation | Ask/review across three local models and delegate write tasks to isolated worktrees — with sensitive-prompt scrubbing, run artifacts/index, change-scope guards, and patch admission before anything lands |
| Agent state & handoff | Shared memory, the active task packet, a subtask plan DAG (`agent-plan`) for splitting work across agents, and session-transcript handoff — all attributed to the writing agent and anchored at the repo root so every agent (from any subdirectory) sees one state |
| ML experiment tracking | Run ids, ledger, reproducibility capsules, pre-registered research runs, and metric/verdict records — with a gate that won't burn a run on a failing contract |
| ML data & leakage | Dataset-split manifests that flag train/eval overlap on IDs and declared group keys (entity/pair/scaffold/family/assay/donor/batch/time), detect split drift, and never store raw rows |
| ML/HPC support | Slurm job reconcile, a single-machine GPU queue, log digests, and local hardware/cluster context (see [docs/COMPONENTS.md](docs/COMPONENTS.md)) |
| Reusable code sources | A local registry and GitHub fetch for trusted reusable files (see [docs/COMPONENTS.md](docs/COMPONENTS.md)) |
| Maintenance & release | Install/update/doctor, a verification gate wired as a pre-push hook, cleanup/uninstall with restore, and a tag-driven release ([docs/RELEASE.md](docs/RELEASE.md)) |

## Notes

- Local-first: use local files and CLIs by default. Connectors are allowed when explicitly requested or local sources cannot answer reliably.
- Shared harness writes use per-file locks; `OMS_LOCK_TIMEOUT` sets wait/stale recovery seconds (default `300`).
- Never commit tokens, API keys, private data, or cluster/machine details.
- The scripts the agent runs live in `~/.oh-my-setting/scripts/`, also
  reachable as `oms <tool>` via the dispatcher on PATH (`oms list` prints the
  catalog) — documented for transparency and recovery, not for manual use.

## Star

If this helped: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
