# oh-my-setting

Sync agent rules, skills, and project templates across machines.

[한국어](README.ko.md)

## Local-First Agents

oh-my-setting keeps agent work local and shell-visible by default:

- No MCP servers, app connectors, or plugin connector tools.
- Use local files, shell commands, `git`, and `gh` CLI.
- Multi-agent review stays local: Codex, Claude Code, and Antigravity CLI when available.
- If local multi-agent tools are missing, run a single-agent review and report that limitation.

Everything below the install step is used by talking to your coding agent.
The installed rules and skills teach the agent which local script to run; you
never call the scripts yourself.

## Quickstart

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

Then open your coding agent in any directory — empty, mid-project, or an
ongoing oh-my-setting project — and say:

```text
Start this project.
```

The agent detects the state and routes: empty dir → spec interview →
`PROJECT.md` → template → safe skeleton → doctor; existing repo → inspect the
code, apply the template, fill `PROJECT.md` from the code, interview only for
gaps; ongoing project → read `PROJECT.md`, run the doctor, report status and
the next step. Nothing typed in a shell. The fuller version is in
[Agent Prompts](#agent-prompts).

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

Check status, update, or clean stale skill links by asking the agent:

```text
Check the oh-my-setting install status.
Update oh-my-setting and re-run its doctor.
Clean old oh-my-setting skill links and fix duplicate $skill entries.
Run the oh-my-setting skill doctor.
```

After install, normal operation is chat-first: the user asks, and the agent runs
local scripts such as `status.sh`, `doctor.sh`, `cleanup.sh`, and
`skill-doctor.sh`. Script paths are documented for transparency and recovery,
not because the user is expected to run them manually.

## Shared Harness Memory

Codex, Claude Code, and Antigravity have different product-specific memory
surfaces. oh-my-setting does not try to merge those private stores directly.
Instead, it provides a harness-owned memory file that all three agents can read
when the harness calls them.

- Project memory source log: `.oms/memory/shared.md`
- Project prompt context: `.oms/memory/pins.md` + `.oms/memory/summary.md`
- Global memory source log: `~/.oh-my-setting/local/agent-memory.md`
- Provider prompts inject compact memory by default, not the full source log.
- Safety: append/pin rejects sensitive-looking notes such as credentials,
  private keys, local machine paths, cluster details, and project-private paths.
- Rules that must always apply still belong in `AGENTS.md`, checked-in docs,
  scripts, or hooks. Shared memory is soft recall.
- Active task handoff lives at `.oms/task/current.md`; provider prompts attach
  it by default so Codex, Claude Code, and Antigravity can continue the same
  work without replaying the full chat.
- Outbound prompts are scrubbed before provider CLI calls; sensitive-looking
  credentials, private keys, absolute machine paths, cluster details, raw logs,
  datasets, and checkpoints block the external call.

Ask the agent to manage it:

```text
Remember for this repo: run scripts/check.sh fast before claiming done.
Pin for this repo: current task is preserving cross-agent context with compact memory.
Show compact shared harness memory for this repo.
```

Call one provider directly; the agent chooses read-only call or isolated write delegation:

```text
Ask only Codex to assess this plan.
Ask only Claude Code to implement this focused fix and return a patch.
Ask only Antigravity to review this implementation direction.
```

The agent uses `agent-memory.sh`, `agent-task.sh`, and `agent-run.sh` under the
hood. `agent-run.sh` routes read-only questions to `agent-call.sh` and write
tasks to `multi-agent-delegate.sh`, so patches are isolated in a git worktree.
ML repos also get a compact `agent-ml-context.sh` digest by default.

## Multi-Agent Workflows

Two workflows with opposite defaults:

| | review | ask |
|---|---|---|
| Purpose | Verify a diff (gate) | Explore a question (advice) |
| Repo context | Attached by default | Omitted by default (attach on request) |
| Reviewer contract | Findings / Risks / Missing tests / Recommendation | Answer / Tradeoffs / Risks / Recommendation |
| Extras | base-ref review, ML checklist, synthesis, debate | debate |
| Failure means | Review gate failed (block the change) | Not enough independent opinions |

Use review before merging or training; use ask while deciding what to build.

Example prompts:

```text
Run a multi-agent review of the current diff.
Run a multi-agent review of this branch against origin/main.
Run the ML pre-training review gate on this diff.
Ask all three models: should this project use a vector DB or pgvector?
Ask all three models with one debate round: compare RAG and fine-tuning for this project.
```

The ML gate checks the diff for silent ML bugs before burning GPU time
(leakage, split integrity, loss, eval mode, reproducibility, DDP); the
checklist is injected into every reviewer prompt automatically.

Providers run in parallel with a per-provider timeout
(`OMS_MULTI_AGENT_TIMEOUT`, default `5m`). Per-provider artifacts plus a
`_synthesis-*.md` summary are written to `.oms/artifacts/review/` (review) and
`.oms/artifacts/ask/` (ask). The synthesis is model-written
(Consensus/Must-fix/Optional/Disagreement) by default. Debate (1-3 rounds)
makes each provider see the others' previous answers, critique them with
evidence, and revise its position before the synthesis — useful for killing
false positives on high-stakes diffs. Round artifacts are saved as `*-rN.md`;
cost scales with providers × (1+rounds), and 1-2 rounds is usually the sweet
spot. Debate rounds exchange answers only — repo context is attached to
round-1 prompts only. Sanitized diff/status context goes to the local Codex,
Claude Code, and Antigravity CLIs; secret paths and secret-like added lines
are excluded before external review. The final outbound prompt is also scanned
and blocked if sensitive-looking context remains.

Delegate a write task to another agent:

```text
Delegate this to codex: add input validation to scripts/train.py.
Verify with `uv run pytest tests/`.
```

The worker runs in an isolated git worktree and cannot touch the main tree,
commit, or push. Artifacts (log + `.patch` against HEAD) land in
`.oms/artifacts/delegate/`. The host agent writes the brief
(Task/Context/Constraints/Files/Success criteria) from conversation context,
reviews the returned patch with you, and applies it only after approval.

## Verification And Experiment Tools

ML projects get a verification contract at `scripts/check.sh` (scaffolded by
the ml template): `fast` is CPU-only and under 60s, `ml-smoke` is a one-batch
ML interface smoke, and `gpu` is a short GPU smoke, srun-wrapped on Slurm
machines. Delegated workers prefer `check.sh ml-smoke` for detected ML repos
when that mode exists; otherwise they run `check.sh fast`.

Launch experiments through the run ledger so every agent remembers what was
already tried:

```text
Launch this training run through the run ledger, note "lr sweep".
Show the last 10 ledger entries.
```

Rows (git SHA, dirty-diff hash, Slurm job id, exit code, duration) append to
`docs/EXPERIMENTS.jsonl`. The command line is recorded verbatim — keep secrets
out of arguments.

Digest long training/Slurm logs instead of pasting them raw:

```text
Digest outputs/train.log.
Digest Slurm job 12345 and its log.
```

## Project Setup

Ask the agent inside the project:

```text
Apply the oh-my-setting project template (auto-detect).
Apply the oh-my-setting ml template.        # or: general, slurm
Remove the oh-my-setting project rules.
Run the oh-my-setting project doctor.
```

What applying does:

- Adds/updates managed blocks in `AGENTS.md` and `CLAUDE.md`.
- Creates `PROJECT.md` if missing.
- For `ml` projects, scaffolds standard ML doc templates under `docs/` (existing files are never overwritten).
- For `ml` projects, ensures `.gitignore` covers `data/`, `outputs/`, `checkpoints/`, `wandb/`, `runs/`, `.venv/`.
- Does not overwrite user content outside managed blocks.
- For ML projects on Slurm machines, adds `ml` plus separate `slurm` rules.

Removal only deletes managed blocks. `PROJECT.md` and scaffolded `docs/` files
may contain user content and are intentionally left in place.

The project doctor fails when `AGENTS.md` and `CLAUDE.md` managed blocks
differ, blocks are stale against current templates, or `PROJECT.md` is
missing. It warns on draft `PROJECT.md`, missing ML doc scaffold, or missing
`.gitignore` entries. Run it after updating oh-my-setting to find projects
that need a template re-apply.

## Agent Prompts

One prompt for every case — new, existing, or ongoing:

```text
Use the local oh-my-setting project workflow. Do not code yet.

Start this project from the current state of the directory. Detect the state
yourself and route:
- new project: interview me, write PROJECT.md, and after I confirm it,
  bootstrap (template, safe skeleton, doctor) in one go
- existing repo without oh-my-setting: inspect the code, configs, and git
  history first; apply the template; fill PROJECT.md from the code and
  interview me only for what the code cannot answer
- ongoing oh-my-setting project: read PROJECT.md, run the project doctor,
  report status, and propose the next step

Always:
- write PROJECT.md and wait for my confirmation before broad work
- wait for separate confirmation before feature code or anything beyond the confirmed spec
- report template type, changed files, and doctor result
```

The short trigger `Start this project.` does the same thing — the routing
lives in the installed spec-interview skill, not in the prompt.

## ML Projects

Create an empty directory, open the agent in it, and say:

```text
Start a new ML project here.
```

Expected agent flow:

1. interview
2. fill/confirm `PROJECT.md`
3. apply the ml template, scaffold the safe skeleton, run the project doctor
4. code after confirmation

ML projects use:

- `uv sync`
- local `.venv`
- `uv run ...`
- machine snapshot only when compute, GPU/CUDA, Slurm, memory, or environment details affect the task

The ml template also scaffolds standard doc templates under `docs/`
(`DATA.md`, `MODEL.md`, `TRAINING.md`, `EVALUATION.md`, ...). Fill them as the
project takes shape; existing files are never overwritten.

## Local Snapshots

The installer generates these; regenerate by asking the agent:

```text
Regenerate the machine snapshot.
Regenerate the Slurm cluster snapshot.        # add "include raw outputs" if needed
```

Written paths:

```text
~/.oh-my-setting/local/machine.md
~/.oh-my-setting/custom-skills/slurm-hpc/references/cluster.generated.md
```

The machine snapshot also records local agent CLI paths for Codex, Claude
Code, Antigravity, and `gh` when found.

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
