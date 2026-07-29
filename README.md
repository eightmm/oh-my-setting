# oh-my-setting

One setup that gives Codex, Claude Code, and Antigravity the same rules,
skills, and agent harness on every machine.

**You never run any of it.** Install is the only command you type; from then on
the harness is the agent's, not yours. Its tools (`oms ...`) are invoked by an
agent mid-task, its state (`.oms/`) is written by agents, and its setup steps —
updates, health checks, even the permissions another agent CLI needs — are
things you ask your agent to do. If a document here shows a command, it is
showing you what the agent will run.

[한국어](README.ko.md)

## Install

Install the latest version from `main`:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

This installs the harness and the CLIs it coordinates: Claude Code, Codex,
Antigravity, plus Node (via nvm), uv, and the GitHub CLI. A council with one
installed peer is not a council, so the peers are part of the install rather
than a flag to remember. Nothing here needs root — npm globals go to the nvm
prefix and `gh` lands in `~/.local/bin`. Pass `--no-tools` on a machine that
cannot have them, and run `gh auth login` once, since that step is interactive.
After that, ask your coding agent to check, update, or customize the
installation.

Supported hosts:

- Linux and WSL: Bash 3.2+, Git, and Python 3.9+.
- macOS: the stock Bash 3.2, Git, and Python 3.9+.
- Windows: Git Bash, Git, and Python 3.9+. Managed files are verified copies
  because Git for Windows cannot assume symlink privileges. Install Node 20+
  and the provider CLIs with their native Windows installers, or use
  `curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash -s -- --no-tools`
  for the harness-only install.

Native PowerShell is not an execution surface; use Git Bash or WSL. The
selected `symlink`/`copy` mode is persisted in the install receipt, so update,
doctor, and uninstall use the same ownership contract. It can be overridden
with `OH_MY_SETTING_LINK_MODE=auto|symlink|copy`.
A `--no-tools` install does not edit shell startup files; add `~/.local/bin` to
PATH yourself when needed. On Windows, if only `python` or `py -3` is available,
the installer creates a conflict-safe managed `python3` shim and removes it
during uninstall.

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
Keep the agent files out of this public repo's git history.
```

Multi-agent work:

```text
Run a peer review of the current diff.
Ask all three models with one debate round: vector DB or pgvector?
Delegate this to codex: add input validation to scripts/train.py.
Ask another agent what it thinks about this split policy, and keep the thread.
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
Check the installed AI model routes and provider CLI compatibility.
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
| Clean public repos | The per-project harness (`AGENTS.md`, `CLAUDE.md`, `PROJECT.md`) is hidden from git locally via `.git/info/exclude` — every agent still reads it, your commits and your `.gitignore` stay free of harness residue |
| Cross-agent conversation | Ask a peer mid-task and keep the exchange: one shared thread any provider can read and extend, so codex, claude, and antigravity build on each other's answers across calls and sessions instead of one-shot questions |
| Multi-agent review & delegation | Ask/review across three local models and delegate write tasks to isolated worktrees — with sensitive-prompt scrubbing, run artifacts/index, change-scope guards, and patch admission before anything lands |
| Agent state & autonomous handoff | Project-local SQLite-indexed shared memory with reversible Markdown sources, mechanically verified task packets, a subtask DAG, and bounded `plan-run` execution that claims one scoped task, delegates in isolation, and stops in review unless landing is explicit — all anchored at the repo root |
| ML experiment tracking | Run ids, ledger, reproducibility capsules, pre-registered research runs, and metric/verdict records — with a gate that won't burn a run on a failing contract |
| ML data & leakage | Dataset-split manifests that flag train/eval overlap on IDs and declared group keys (entity/pair/scaffold/family/assay/donor/batch/time), detect split drift, and never store raw rows |
| ML/HPC support | Slurm job reconcile, a single-machine GPU queue, log digests, and local hardware/cluster context (see [docs/COMPONENTS.md](docs/COMPONENTS.md)) |
| Reusable code sources | A local registry and GitHub fetch for trusted reusable files (see [docs/COMPONENTS.md](docs/COMPONENTS.md)) |
| Maintenance | Install/update/doctor, provider/model capability checks, a verification gate wired as a pre-push hook, and cleanup/uninstall with restore |

## Notes

- Local-first: use local files and CLIs by default. Connectors are allowed when explicitly requested or local sources cannot answer reliably.
- Shared harness writes use per-file locks; `OMS_LOCK_TIMEOUT` sets wait/stale recovery seconds (default `300`).
- Never commit tokens, API keys, private data, or cluster/machine details.
- The per-project agent files are local-only, so a fresh clone or `git clean -x`
  will not have them: re-apply the template there (one sentence to your agent).
- The scripts the agent runs live in `~/.oh-my-setting/scripts/`, also
  reachable as `oms <tool>` via the dispatcher on PATH (`oms list` prints the
  catalog) — documented for transparency and recovery, not for manual use.

## Star

If this helped: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
