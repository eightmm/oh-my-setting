# oh-my-setting

One setup that gives Codex, Claude Code, and Antigravity the same rules,
skills, and agent harness on every machine. After install, everything is used by
talking to your coding agent — there is nothing to run in a terminal.

[한국어](README.ko.md)

## Install

Install the latest version from `main`:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

This installs the safe default profile and connects the providers already on
the machine. After that, ask your coding agent to check, update, or customize
the installation.

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

Multi-agent work:

```text
Run a peer review of the current diff.
Ask all three models with one debate round: vector DB or pgvector?
Delegate this to codex: add input validation to scripts/train.py.
```

ML and HPC:

```text
Check this molecular dataset's split for leakage before I train.
Frame this as a hypothesis-driven experiment before I launch the run.
Wait for Slurm job 12345, then digest its log and report.
Queue this training run on the single-GPU box.
```

Maintenance:

```text
Check the oh-my-setting install status.
Update oh-my-setting and re-run its doctor.
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
| Agent state & autonomous handoff | Shared memory, mechanically verified task packets, a subtask DAG, and bounded `plan-run` execution that claims one scoped task, delegates in isolation, and stops in review unless landing is explicit — all anchored at the repo root |
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
