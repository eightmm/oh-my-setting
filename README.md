# oh-my-setting

Sync agent rules, skills, and project templates across machines.

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

Settings only, no tool install:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | \
  OH_MY_SETTING_INSTALL_TOOLS=0 bash
```

## Local-First Agents

- No MCP servers, app connectors, or plugin connector tools.
- Use local files, shell commands, `git`, and `gh` CLI.
- Multi-agent review stays local: Codex, Claude Code, Gemini, or Pi CLI when available.
- If local multi-agent tools are missing, run a single-agent review and report that limitation.

## Project Setup

Auto-detect:

```bash
~/.oh-my-setting/scripts/apply-project-template.sh auto .
```

Choose explicitly:

```bash
~/.oh-my-setting/scripts/apply-project-template.sh general .
~/.oh-my-setting/scripts/apply-project-template.sh ml .
~/.oh-my-setting/scripts/apply-project-template.sh slurm .
```

What it does:

- Adds/updates managed blocks in `AGENTS.md`, `CLAUDE.md`, and `GEMINI.md`.
- Creates `PROJECT.md` if missing.
- Does not overwrite user content outside managed blocks.
- For ML projects on Slurm machines, adds `ml` plus separate `slurm` rules.

Remove project rules:

```bash
~/.oh-my-setting/scripts/remove-project-template.sh all .
```

Detect only:

```bash
~/.oh-my-setting/scripts/detect-project-style.sh .
```

## ML Workflow

```bash
mkdir my-project
cd my-project
~/.oh-my-setting/scripts/apply-project-template.sh ml .
```

Then ask the agent to start the project. It should:

1. create only the safe skeleton,
2. interview,
3. fill/confirm `PROJECT.md`,
4. code after confirmation.

ML projects use:

- `uv sync`
- local `.venv`
- `uv run ...`
- machine snapshot from `~/.oh-my-setting/local/machine.md`

## Local Snapshots

Machine snapshot:

```bash
~/.oh-my-setting/scripts/write-machine-snapshot.sh
```

Writes:

```text
~/.oh-my-setting/local/machine.md
```

Also records local agent CLI paths for Codex, Claude Code, Gemini, Pi, and
`gh` when found.

Slurm snapshot:

```bash
~/.oh-my-setting/scripts/generate-slurm-skill.sh
```

Writes:

```text
~/.oh-my-setting/custom-skills/slurm-hpc/references/cluster.generated.md
```

Include raw Slurm outputs:

```bash
OH_MY_SETTING_SLURM_WRITE_RAW=1 ~/.oh-my-setting/scripts/generate-slurm-skill.sh
```

## Update

```bash
cd ~/.oh-my-setting
git pull --ff-only
./scripts/link.sh
./scripts/write-machine-snapshot.sh
./scripts/doctor.sh
```

## Unlink

Remove oh-my-setting symlinks and restore the latest matching
`*.backup.TIMESTAMP` files when present:

```bash
~/.oh-my-setting/scripts/unlink.sh
```

It only removes symlinks that point to the current oh-my-setting checkout.
Existing regular files and unrelated symlinks are skipped.

Preview first:

```bash
OH_MY_SETTING_DRY_RUN=1 ~/.oh-my-setting/scripts/unlink.sh
```

## Install Flags

```bash
OH_MY_SETTING_INSTALL_TOOLS=0      # link settings only
OH_MY_SETTING_GENERATE_MACHINE=0  # skip machine snapshot
OH_MY_SETTING_GENERATE_SLURM=0    # skip Slurm snapshot
OH_MY_SETTING_DIR=/path/to/dir    # install location
```

## Installed Paths

```text
~/.codex/AGENTS.md
~/.claude/CLAUDE.md
~/.gemini/GEMINI.md
~/.pi/agent/AGENTS.md
~/.oh-my-setting/local/machine.md
```

## Secrets

Do not commit tokens, API keys, private data, generated cluster details, or local machine details.

## Star

If this helped:

```bash
gh api --method PUT /user/starred/eightmm/oh-my-setting
```
