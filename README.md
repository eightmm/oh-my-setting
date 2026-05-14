# oh-my-setting

Distribute LLM agent settings, skills, and project `AGENTS.md` templates across machines.

[한국어](README.ko.md)

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

Inspect first:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh -o /tmp/oh-my-setting-install.sh
less /tmp/oh-my-setting-install.sh
bash /tmp/oh-my-setting-install.sh
```

Link settings only:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | \
  OH_MY_SETTING_INSTALL_TOOLS=0 bash
```

## What It Installs

- Global rules: `~/.codex/AGENTS.md`, `~/.claude/CLAUDE.md`, `~/.gemini/GEMINI.md`, `~/.pi/agent/AGENTS.md`
- Custom skills: symlinked into Codex/Claude/Pi/shared skill paths
- Tools: Node, `uv`, Claude Code, Codex, Gemini CLI, Pi Agent, caveman
- Slurm reference: auto-generated when `sinfo` exists
- Output style: global caveman-ultra rules enabled for all linked agents

## Update

```bash
cd ~/.oh-my-setting
git pull --ff-only
./scripts/link.sh
./scripts/doctor.sh
```

## Project Rules

Auto-detect:

```bash
~/.oh-my-setting/scripts/apply-project-template.sh auto /path/to/project
```

Choose explicitly:

```bash
~/.oh-my-setting/scripts/apply-project-template.sh general /path/to/project
~/.oh-my-setting/scripts/apply-project-template.sh ml /path/to/project
~/.oh-my-setting/scripts/apply-project-template.sh slurm-ml /path/to/project
```

Behavior:

- Never overwrites existing `AGENTS.md`/`CLAUDE.md`/`GEMINI.md`.
- Appends/updates only the `oh-my-setting` managed block.
- Creates/updates `AGENTS.md`, `CLAUDE.md`, and `GEMINI.md` by default.
- Creates `PROJECT.md` for project-specific details when missing.
- Works with Codex, Claude, Gemini, and Pi.

Remove:

```bash
~/.oh-my-setting/scripts/remove-project-template.sh all /path/to/project
```

Detect only:

```bash
~/.oh-my-setting/scripts/detect-project-style.sh /path/to/project
```

## Templates

- `general`: non-ML projects
- `ml`: ML projects
- `slurm-ml`: Slurm/HPC ML projects

Files:

```text
templates/project-general-AGENTS.md
templates/project-ml-AGENTS.md
templates/project-slurm-ml-AGENTS.md
```

## Scripts

```text
install.sh                         Full install
scripts/install-tools.sh           Install Node/uv/agent CLIs
scripts/link.sh                    Symlink global settings
scripts/doctor.sh                  Check install status
scripts/backup.sh                  Back up existing settings
scripts/apply-project-template.sh  Add/update project rules
scripts/remove-project-template.sh Remove managed project block
scripts/detect-project-style.sh    Detect project style
scripts/generate-slurm-skill.sh    Generate local Slurm cluster reference
```

## Slurm

Install auto-generates local Slurm reference when `sinfo` exists. Manual refresh:

```bash
~/.oh-my-setting/scripts/generate-slurm-skill.sh
```

This writes `custom-skills/slurm-hpc/references/cluster.generated.md`.
Generated cluster details are gitignored.

Disable auto-generation:

```bash
OH_MY_SETTING_GENERATE_SLURM=0 \
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

## Secrets

Do not commit tokens or API keys. Keep variable names only in `.env.example`.
