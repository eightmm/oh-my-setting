# oh-my-setting

Sync agent rules, skills, and project templates across machines.

[한국어](README.ko.md)

## Local-First Agents

oh-my-setting keeps agent work local and shell-visible by default:

- No MCP servers, app connectors, or plugin connector tools.
- Use local files, shell commands, `git`, and `gh` CLI.
- Multi-agent review stays local: Codex, Claude Code, and Antigravity CLI when available.
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
After first-time nvm setup, open a new shell if newly installed CLIs are not found.
Installer-managed Node uses Node 20 or newer.

Options:

```bash
OH_MY_SETTING_STAR_PROMPT=0       # skip GitHub star prompt
OH_MY_SETTING_GENERATE_MACHINE=0  # skip machine snapshot
OH_MY_SETTING_GENERATE_SLURM=0    # skip Slurm snapshot
OH_MY_SETTING_INSTALL_TOOLS=0     # skip Node/uv/agent CLI install
OH_MY_SETTING_REQUIRE_TOOLS=0     # do not fail doctor on missing CLIs
OH_MY_SETTING_DIR=/path/to/dir    # install location
```

When running a local installer, `--no-star` is equivalent to
`OH_MY_SETTING_STAR_PROMPT=0`.

Installed paths:

```text
~/.codex/AGENTS.md
~/.claude/CLAUDE.md
~/.gemini/AGENTS.md
~/.oh-my-setting/local/machine.md
```

`~/.gemini/AGENTS.md` is the Antigravity global customizations root; skills are
also linked under `~/.gemini/antigravity/skills/`, so all three agents read the
same rules and skills.

Check current install status:

```bash
~/.oh-my-setting/scripts/status.sh
```

Update the local checkout, refresh symlinks, and re-run doctor:

```bash
~/.oh-my-setting/scripts/update.sh           # git pull + tools + link + doctor
~/.oh-my-setting/scripts/update.sh --no-tools --no-doctor
```

Two multi-agent commands with opposite defaults:

| | `multi-agent-review.sh` | `multi-agent-ask.sh` |
|---|---|---|
| Purpose | Verify a diff (gate) | Explore a question (advice) |
| Repo context | Attached by default (`--no-diff` to omit) | Omitted by default (`--repo-context`/`--diff` to attach) |
| Reviewer contract | Findings / Risks / Missing tests / Recommendation | Answer / Tradeoffs / Risks / Recommendation |
| Extras | `--base`, `--synthesize`, `--ml`, `--debate` | `--debate` |
| Non-zero exit means | Review gate failed (block the change) | Not enough independent opinions |

Use `review` before merging or training; use `ask` while deciding what to build.

Run a three-model review of the tracked staged + unstaged repo diff:

```bash
~/.oh-my-setting/scripts/multi-agent-review.sh \
  --prompt "Review the current diff for bugs, regressions, missing tests, and unsafe operations."
```

Review a branch against a base ref (PR-style):

```bash
~/.oh-my-setting/scripts/multi-agent-review.sh \
  --base origin/main \
  --prompt "Review this branch against origin/main."
```

ML pre-training gate — review the diff for silent ML bugs before burning GPU
time (leakage, split integrity, loss, eval mode, reproducibility, DDP):

```bash
~/.oh-my-setting/scripts/multi-agent-review.sh --ml
```

`--ml` injects the checklist into every reviewer prompt and supplies a default
prompt, so it works with no other arguments.

Providers run in parallel with a per-provider timeout (`OMS_MULTI_AGENT_TIMEOUT`, default `5m`). Per-provider artifacts plus a `_synthesis-*.md` summary are written to `.omc/artifacts/review/`. Pass `--synthesize [codex|claude|antigravity]` (default `claude`) to append a model-written synthesis (Consensus/Must-fix/Optional/Disagreement) to the summary instead of raw concatenation only. `--debate N` (1-3) makes reviewers critique each other's findings before the synthesis — useful for killing false positives on high-stakes diffs. The wrapper sends sanitized diff/status context to the local Codex, Claude Code, and Antigravity CLIs; secret paths and secret-like added lines are excluded before external review.

Ask a conceptual question to all three models:

```bash
~/.oh-my-setting/scripts/multi-agent-ask.sh \
  --prompt "Compare RAG and fine-tuning tradeoffs for this project."
```

Per-provider ask artifacts plus a `_synthesis-*.md` summary are written to `.omc/artifacts/ask/`. No repo context is attached unless `--repo-context` or `--diff` is passed.

Let the models debate each other before answering:

```bash
~/.oh-my-setting/scripts/multi-agent-ask.sh \
  --debate 1 \
  --prompt "Should this project use a vector DB or pgvector?"
```

`--debate N` (1-3) adds N rounds after the independent first round: each
provider sees the others' previous answers, critiques them with evidence, and
revises its own position. Round artifacts are saved as `*-rN.md`; the
synthesis carries each provider's final answer. Cost scales with
providers × (1+N) calls; 1-2 rounds is usually the sweet spot. Debate rounds
exchange answers only — repo context is attached to round-1 prompts only.

Delegate a write task to another agent (worker runs in an isolated git
worktree, result comes back as a reviewable patch):

```bash
~/.oh-my-setting/scripts/multi-agent-delegate.sh \
  --to codex \
  --brief-file /tmp/brief.md \
  --verify "uv run pytest tests/"
```

The worker cannot touch the main tree, commit, or push. Artifacts (log +
`.patch` against HEAD) land in `.omc/artifacts/delegate/`. Review the patch,
then re-run with `--apply` or `git apply --binary <patch>`. The
`multi-agent-delegate` skill tells the host agent how to write a brief
(Task/Context/Constraints/Files/Success criteria) from conversation context.

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

- Adds/updates managed blocks in `AGENTS.md` and `CLAUDE.md`.
- Creates `PROJECT.md` if missing.
- For `ml` projects, scaffolds standard ML doc templates under `docs/` (existing files are never overwritten).
- For `ml` projects, ensures `.gitignore` covers `data/`, `outputs/`, `checkpoints/`, `wandb/`, `runs/`, `.venv/`.
- Does not overwrite user content outside managed blocks.
- For ML projects on Slurm machines, adds `ml` plus separate `slurm` rules.

Remove project rules:

```bash
~/.oh-my-setting/scripts/remove-project-template.sh all .
```

Removal only deletes managed blocks. `PROJECT.md` and scaffolded `docs/` files may contain user content and are intentionally left in place.

Detect only:

```bash
~/.oh-my-setting/scripts/detect-project-style.sh .
```

Check that every agent sees the same project rules:

```bash
~/.oh-my-setting/scripts/project-doctor.sh .
```

Fails when `AGENTS.md` and `CLAUDE.md` managed blocks differ, blocks are stale
against current templates, or `PROJECT.md` is missing. Warns on draft
`PROJECT.md`, missing ML doc scaffold, or missing `.gitignore` entries. Run it
after `update.sh` to find projects that need a template re-apply.

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

`apply-project-template.sh ml` also scaffolds standard doc templates under
`docs/` (`DATA.md`, `MODEL.md`, `TRAINING.md`, `EVALUATION.md`, ...). Fill them
as the project takes shape; existing files are never overwritten.

## Local Snapshots

Machine snapshot:

```bash
~/.oh-my-setting/scripts/write-machine-snapshot.sh
```

Writes:

```text
~/.oh-my-setting/local/machine.md
```

Also records local agent CLI paths for Codex, Claude Code, Antigravity, and
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

## Uninstall

Remove symlinks (same as `unlink.sh`) and optionally delete the checkout:

```bash
~/.oh-my-setting/scripts/uninstall.sh           # unlink only
~/.oh-my-setting/scripts/uninstall.sh --purge   # also delete the checkout (prompts)
~/.oh-my-setting/scripts/uninstall.sh --purge --yes --dry-run
```

`--purge` refuses to delete `$HOME` or `/`. nvm, uv, and CLI binaries installed by `install-tools.sh` are not removed.

## Safety

Do not commit tokens, API keys, private data, generated cluster details, or local machine details.

## Star

If this helped:

```bash
gh api --method PUT /user/starred/eightmm/oh-my-setting
```
