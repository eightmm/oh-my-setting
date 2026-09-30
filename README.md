<p align="center">
  <img src="docs/assets/banner.svg" alt="oh-my-setting — one control plane for Codex, Claude Code and Antigravity" width="100%">
</p>

<p align="center">
  <a href="https://github.com/eightmm/oh-my-setting/actions/workflows/test.yml"><img src="https://github.com/eightmm/oh-my-setting/actions/workflows/test.yml/badge.svg?branch=main" alt="CI"></a>
  <img src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-0ea5e9" alt="Linux, macOS, Windows">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-22c55e" alt="MIT license"></a>
</p>

<p align="center"><a href="README.ko.md">한국어</a></p>

**oh-my-setting** lets Codex, Claude Code and Antigravity work on the same
project from the same rules, state, plans, evidence and handoffs, so any of
them can pick up where another left off.

**You never run any of it.** Install once; after that the harness belongs to
your agent. It calls the `oms` tools, writes the `.oms/` state, and handles
updates and health checks when you ask. A command shown here is what the
agent will run, not something you type.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

| Host | Needs |
|---|---|
| Linux, WSL (glibc) | Bash, curl. Git, tar, gzip and find are installed if missing. **No Python needed.** |
| macOS | Bash, curl, Command Line Tools (`xcode-select --install`). **No Python needed.** |
| Windows (Git Bash) | Git, Python 3.9+, and native Node.js when a Node-based agent is selected |

The default `core` profile installs the harness and one coding agent.
musl-based Linux such as Alpine is not supported. Profiles and Windows notes
are in [docs/INSTALL.md](docs/INSTALL.md).

### What the installer changes

- **Global agent rules.** `~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md` and
  `~/.gemini/AGENTS.md` become links to the OMS rules. An existing file is
  moved to `<file>.backup.<timestamp>` and comes back on uninstall.
- **Agent settings.** Claude Code gets hooks, a status-line HUD and a `high`
  effort default for Opus 5.5 if you have none. Codex gets the OMS plugin, a
  footer and a few usage defaults. Antigravity gets the OMS plugin. Both
  Claude Code and Codex register the OMS MCP server.
- **Files in your home.** `oms-*` skills for all three CLIs, the `oms`
  command in `~/.local/bin` (added to your shell's PATH), and a private
  Python under `~/.local/share/oh-my-setting`.
- **A daily update.** Clean checkouts fast-forward once a day. To skip it,
  end the install command with `| bash -s -- --no-auto-update`.

### Update and uninstall

Ask your agent to update or uninstall oh-my-setting; it runs `oms update` or
`oms uninstall`. Uninstall restores what it replaced and leaves the agent
CLIs installed. Release-specific changes are in the
[migration notes](docs/MIGRATION-0.7.md).

## How it works

<p align="center">
  <img src="docs/assets/how-it-works.svg" alt="You talk to any coding agent; Codex, Claude Code and Antigravity share one oms layer of rules, state, councils, delegation and verified landing that works on your repository" width="100%">
</p>

Open your coding agent in any directory (empty, mid-project or ongoing) and
say:

```text
Start this project.
```

An empty directory gets a short spec interview, a `PROJECT.md` and a
template. An existing repository is inspected first and interviewed only for
gaps. An ongoing project gets a status report and the next step. From a
confirmed `PROJECT.md`, the agent can carry work through reviewed tasks,
checks and a Draft PR. Generated work never approves itself, and merge and
release stay your call.

## What you can say

```text
Start this project.
Run a peer review of the current diff.
Ask all three models with one debate round: vector DB or pgvector?
Delegate this to codex: add input validation to scripts/train.py.
Take this confirmed PROJECT.md through implementation to a Draft PR.
Check this dataset's group split for leakage before I train.
Wait for Slurm job 12345, then digest its log and report.
Update oh-my-setting and re-run its doctor.
```

## What's inside

| | |
|---|---|
| **Shared rules** | One global rule set for all three CLIs, plus `general`, `ml` and `slurm` project templates |
| **Councils** | Independent multi-model reviews and debates; judging seats get a read-only, bounded view |
| **Delegation** | Isolated worktree workers, an admission gate and one landing path; no recursive delegation |
| **State and handoff** | Work Journal, attention inbox, handoffs before compaction, failure ledger |
| **Verified landing** | One gate, push only while the tree is unchanged, CI, then the install updates to that commit |
| **ML and HPC** | Reproducible runs, leakage checks, Slurm and GPU queue helpers |
| **Typed runtime** | Task contracts, evidence coverage, bounded context, portable capsules ([docs/OMS-RUNTIME.md](docs/OMS-RUNTIME.md)) |

Your agent finds these on its own; the full catalog is in
[docs/COMPONENTS.md](docs/COMPONENTS.md).

## Notes

- **Local-first.** Local files and CLIs by default; connectors only when asked.
- **Nothing private is committed.** Tokens, private data and machine details
  stay out of git; per-project agent files are ignored locally.
- **Reversible.** `oms uninstall` restores the files it replaced.

If it helps, a ⭐ on [GitHub](https://github.com/eightmm/oh-my-setting) is appreciated.
