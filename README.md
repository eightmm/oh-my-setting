# oh-my-setting

Sync agent rules, skills, and project templates across machines.

[한국어](README.ko.md)

## Local-First Agents

oh-my-setting keeps agent work local and shell-visible by default:

- No MCP servers, app connectors, or plugin connector tools.
- Use local files, shell commands, `git`, and `gh` CLI.
- Multi-agent review stays local: Codex, Claude Code, Gemini, or Pi CLI when available.
- If local multi-agent tools are missing, run a single-agent review and report that limitation.

## Quickstart

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
cd /path/to/project
~/.oh-my-setting/scripts/apply-project-template.sh auto .
```

Then paste the matching prompt from [Agent Prompts](#agent-prompts) into the
agent.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

Existing installs update to the latest checkout before setup continues.

Options:

```bash
OH_MY_SETTING_STAR_PROMPT=0       # skip GitHub star prompt
OH_MY_SETTING_GENERATE_MACHINE=0  # skip machine snapshot
OH_MY_SETTING_GENERATE_SLURM=0    # skip Slurm snapshot
OH_MY_SETTING_DIR=/path/to/dir    # install location
```

When running a local installer, `--no-star` is equivalent to
`OH_MY_SETTING_STAR_PROMPT=0`.

Installed paths:

```text
~/.codex/AGENTS.md
~/.claude/CLAUDE.md
~/.gemini/GEMINI.md
~/.pi/agent/AGENTS.md
~/.oh-my-setting/local/machine.md
```

Check current install status:

```bash
~/.oh-my-setting/scripts/status.sh
```

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

## Agent Prompts

New project:

```text
Use the local oh-my-setting project workflow. Do not code yet.

Start a new project by creating only the safe skeleton, then interview me to
fill PROJECT.md before implementation.

Success criteria:
- clarify goal, users, non-goals, interface, data, paths, commands, risks, and verification
- write or update PROJECT.md with confirmed answers
- wait for confirmation before source code, dependency, API, data, or compute changes
- report changed files and checks
```

Existing project:

```text
Read local project files first. Start by inspecting AGENTS.md/CLAUDE.md,
PROJECT.md if present, README, pyproject/configs, and git status. Do not edit yet.

Goal: understand this existing project and propose the smallest safe next step
to onboard or continue it with oh-my-setting rules.

Report:
- project type and current structure
- setup/test/run commands you can infer
- missing or draft PROJECT.md fields
- risks before editing
- recommended next prompt
```

## ML Projects

```bash
mkdir my-project
cd my-project
~/.oh-my-setting/scripts/apply-project-template.sh ml .
```

Expected agent flow:

1. create only the safe skeleton
2. interview
3. fill/confirm `PROJECT.md`
4. code after confirmation

ML projects use:

- `uv sync`
- local `.venv`
- `uv run ...`
- machine snapshot only when compute, GPU/CUDA, Slurm, memory, or environment details affect the task

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

## Unlink

Remove oh-my-setting symlinks and restore the latest matching
`*.backup.TIMESTAMP` files when present:

```bash
~/.oh-my-setting/scripts/unlink.sh
```

It only removes symlinks that point to the current oh-my-setting checkout.
Existing regular files and unrelated symlinks are skipped.

Preview unlink:

```bash
OH_MY_SETTING_DRY_RUN=1 ~/.oh-my-setting/scripts/unlink.sh
```

## Safety

Do not commit tokens, API keys, private data, generated cluster details, or local machine details.

## Star

If this helped:

```bash
gh api --method PUT /user/starred/eightmm/oh-my-setting
```
