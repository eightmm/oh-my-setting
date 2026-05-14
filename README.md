# oh-my-setting

LLM agent, prompt, workflow, and local skill settings for new servers or machines.

This repo is intended to work like dotfiles:

1. Keep the source of truth here.
2. Install it on each machine with one command.
3. Use symlinks so future `git pull` updates the active settings.

## Quick Install

After replacing the repository URL in `install.sh`, a new machine can run:

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

## Layout

```text
AGENTS.md                 Global coding and agent behavior rules
custom-skills/            Skills owned by this repo
prompts/                  Reusable prompts
workflows/                Repeatable operating procedures
templates/                Starter files for new projects
scripts/link.sh           Create symlinks into agent config locations
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
