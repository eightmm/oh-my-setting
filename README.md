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

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

This installs the harness and the CLIs it coordinates — Claude Code, Codex,
Antigravity, Node (via nvm), uv, GitHub CLI, and Notion CLI. Nothing needs
root; add `--no-tools` on a machine that cannot have them. In an interactive
terminal the installer delegates browser login to `gh auth login` and
`ntn login` and discovers the Work Journal's Notion target; a non-interactive
install prints the follow-up commands and continues. Claude Code gets compact
main and subagent HUDs (model/effort, context, rate-limit countdowns, cost, Git
state); Codex gets the equivalent native footer when it has no user footer.

| Host | Needs | Managed files |
|---|---|---|
| Linux, WSL | Bash 3.2+, Git, Python 3.9+ (or uv) | symlinks |
| macOS | stock Bash 3.2, Git, Python 3.9+ (or uv) | symlinks |
| Windows Git Bash | Git, Python 3.9+; Node 22+ and provider CLIs from their native installers | verified copies |

The awkward cases — Windows copy mode, Antigravity's headless permissions,
Notion data-source selection — live in
[docs/COMPONENTS.md](docs/COMPONENTS.md) and
[docs/WORK-JOURNAL.md](docs/WORK-JOURNAL.md).

## Start

Open your coding agent in any directory — empty, mid-project, or ongoing — and
say:

```text
Start this project.
```

The agent detects the state and routes: an empty directory goes through a spec
interview to `PROJECT.md`, a template, and a doctor; an existing repo gets
inspected first and interviewed only for gaps; an ongoing project gets a status
report and the next step.

## What You Can Say

```text
Start this project.
Apply the oh-my-setting ml template.              # or: general, slurm
Run a peer review of the current diff.
Ask all three models with one debate round: vector DB or pgvector?
Delegate this to codex: add input validation to scripts/train.py.
Ask another agent about this split policy, and keep the thread.
Check this dataset's group split for leakage before I train.
Frame this as a hypothesis-driven experiment before I launch the run.
Wait for Slurm job 12345, then digest its log and report.
Update oh-my-setting and re-run its doctor.
```

## What's Inside

Your agent picks these up on its own when a task calls for them. One front
door per capability; the full catalog is `oms list`, documented in
[docs/COMPONENTS.md](docs/COMPONENTS.md).

- **Project setup** — start router, spec interview, `general`/`ml`/`slurm`
  templates, a doctor that verifies all three agents read the same rules,
  agent files hidden from git locally
- **Asking other agents** — durable peer threads, three-model councils and
  review gates backstopped by the project's own checks, decision-point
  advisors, outbound scrubbing
- **Delegating writes** — isolated worktree delegation, an admission ladder,
  one mutation boundary, hash-frozen executor souls and worker-authority
  fingerprints
- **Agent state and handoff** — Work Journal with daily summaries and an
  optional Notion mirror, a prioritized attention inbox, pre-compaction
  handoffs, source-validated shared memory, reversible tracked-state
  checkpoints, task packets with executed verification, and a failure ledger
  that names an advisor after repeats
- **ML and HPC** — run spine and reproducibility capsules, pre-registered
  hypothesis runs, dataset leakage manifests, Slurm reconcile and GPU queue
- **Providers and models** — capability probing, fast/balanced/deep tier
  routing with fallback, family-diversity diagnostics, and content-free native
  activity/token telemetry when providers expose it
- **Maintenance** — transactional update with rollback, doctor, one full
  verification gate plus a protected-branch quick pre-push mode

Skills load in three layers: five general-purpose skills everywhere,
machine-conditional skills only where their command exists (`slurm`,
`gpu-workstation`), and per-repo skills forged into `.oms/skills/`.

## Notes

- Local-first: use local files and CLIs by default. Connectors are allowed when explicitly requested or local sources cannot answer reliably.
- Never commit tokens, private data, or cluster/machine details — per-project
  agent files stay out of git by design, so re-apply the template on a fresh
  clone.
- Scripts live in `~/.oh-my-setting/scripts/`, reachable as `oms <tool>`
  (`oms list` prints the catalog). Documented for transparency and recovery,
  not for manual use.

## Star

If this helped: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
