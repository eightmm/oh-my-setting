# Security

oh-my-setting installs shell tooling, symlinks agent config into your home
directory, and lets local agent CLIs (Codex, Claude Code, Antigravity) call
each other. This file explains the threat model and how to report issues.

## Reporting a vulnerability

Email the maintainer (see the repo owner's GitHub profile) with:

- a description and impact,
- steps to reproduce,
- affected script(s) and commit.

Do not open a public issue for a secret leak or a supply-chain concern; report
privately first. Please allow time for a fix before public disclosure.

## What the installer touches

- Symlinks `~/.codex/AGENTS.md`, `~/.claude/CLAUDE.md`, the Antigravity config,
  and skills/workflows to files in this checkout. Pre-existing regular files are
  moved to `*.backup.<timestamp>` before linking (see `scripts/link.sh`).
- Optionally installs external toolchains (`OH_MY_SETTING_INSTALL_TOOLS=1`,
  default on): nvm, uv, the Antigravity installer, and global npm packages
  `@openai/codex` and `@anthropic-ai/claude-code`. These run vendor install
  scripts. Set `OH_MY_SETTING_INSTALL_TOOLS=0` to skip and install them yourself.
- Optionally writes a local machine/Slurm snapshot under `local/` and generated
  Slurm references (gitignored). Skip with `OH_MY_SETTING_GENERATE_MACHINE=0`
  and `OH_MY_SETTING_GENERATE_SLURM=0`.
- Auto-update: the trigger is **check-only by default** — it records when the
  checkout is behind upstream but does not relink. Opt in to automatic
  fast-forward + relink with `OH_MY_SETTING_AUTO_UPDATE_MODE=apply`.

## Secret handling

- Never commit tokens, API keys, private data, or cluster/machine details.
  `.gitignore` blocks `.env`, `*.key`, `*.pem`, `local/`, and generated state.
- Outbound prompts/diffs to external agent CLIs are scanned for secrets and
  machine/cluster details; a match blocks the call (`scripts/lib/agent-memory-common.sh`).
- Delegated patches pass a sensitive-content scan before they can be admitted
  (`scripts/patch-admit.sh`).
- Git-tracked records (the run ledger, gate skip reasons, data manifests) are
  scanned before writing; a sensitive gate skip reason is refused outright.

## Hardening recommendations for users

- Review `install.sh` before piping it to a shell, or clone and run it locally.
- Run with `OH_MY_SETTING_INSTALL_TOOLS=0` if you manage toolchains yourself.
- Keep auto-update in check-only mode unless you trust unattended fast-forwards.
- Run `scripts/doctor.sh` after install to verify the linked state.
