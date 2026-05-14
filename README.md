# oh-my-setting

LLM agent, prompt, workflow, and local skill settings for new servers or machines.

This repo is intended to work like dotfiles:

1. Keep the source of truth here.
2. Install it on each machine with one command.
3. Use symlinks so future `git pull` updates the active settings.

## Quick Install

New machine:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

Safer inspect-first version:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh -o /tmp/oh-my-setting-install.sh
less /tmp/oh-my-setting-install.sh
bash /tmp/oh-my-setting-install.sh
```

You can override the destination or repo URL:

```bash
OH_MY_SETTING_REPO_URL=git@github.com:eightmm/oh-my-setting.git \
OH_MY_SETTING_DIR="$HOME/.oh-my-setting" \
bash install.sh
```

The default bootstrap clone uses HTTPS so fresh servers do not need GitHub SSH keys. Use `OH_MY_SETTING_REPO_URL` when you specifically want SSH.

Skip tool installation and only link settings:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | \
  OH_MY_SETTING_INSTALL_TOOLS=0 bash
```

## Installed Tools

`install.sh` bootstraps `git` when possible, clones this repo, then runs:

```bash
scripts/install-tools.sh
```

That script installs or verifies:

- Node.js through `nvm` when the current Node is missing or older than 18
- uv for Python development
- Claude Code: `npm install -g @anthropic-ai/claude-code`
- Codex CLI: `npm install -g @openai/codex`
- Gemini CLI: `npm install -g @google/gemini-cli`
- Pi Coding Agent: `npm install -g @earendil-works/pi-coding-agent`
- caveman: official installer from `JuliusBrussee/caveman`

Useful overrides:

```bash
OH_MY_SETTING_NODE_VERSION=22 \
OH_MY_SETTING_INSTALL_CAVEMAN=0 \
./scripts/install-tools.sh
```

## Layout

```text
AGENTS.md                 Global coding and agent behavior rules
custom-skills/            Skills owned by this repo
prompts/                  Reusable prompts
workflows/                Repeatable operating procedures
templates/                Starter files for new projects
scripts/link.sh           Create symlinks into agent config locations
scripts/install-tools.sh  Install Node, uv, agent CLIs, Pi, and caveman
scripts/doctor.sh         Check expected files and links
scripts/backup.sh         Copy current local agent files into backups/
skills.manifest.json      List of external/curated skills to install or enable
```

## Secrets

Do not commit real tokens or API keys. Keep only variable names in `.env.example`.

## Typical Update

```bash
cd "$HOME/.oh-my-setting"
git pull --ff-only
./scripts/link.sh
./scripts/doctor.sh
```

## Project Templates

- `templates/project-general-AGENTS.md`: non-ML repos
- `templates/project-ml-AGENTS.md`: ML repos
- `templates/project-slurm-ml-AGENTS.md`: ML repos on Slurm/HPC

Apply a project template without overwriting existing instructions:

```bash
~/.oh-my-setting/scripts/apply-project-template.sh ml /path/to/project
```

Behavior:

- If `AGENTS.md` or `CLAUDE.md` exists, append/update a fenced managed block at the end.
- If neither exists, create `AGENTS.md`.
- Re-running is idempotent: the managed block is replaced, handwritten content stays.
- Compatible with Pi: Pi loads project `AGENTS.md`/`CLAUDE.md`, global `~/.pi/agent/AGENTS.md`, and skills from `~/.pi/agent/skills/`.

Available styles: `general`, `ml`, `slurm-ml`.
