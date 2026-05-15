---
name: multi-agent-review
description: >
  Multi-agent review workflow for important code changes. Use when the user
  asks for independent verification, cross-agent review, council review, or
  when a change touches high-risk surfaces such as APIs, auth, data pipelines,
  model training, Slurm jobs, dependency/toolchain changes, or releases.
---

Goal: get independent review signals, then synthesize evidence. Do not outsource judgment.
Use local CLI tools only. Do not use MCP servers, app connectors, or plugin connector tools.

## When

Use for high-risk diffs or explicit requests: `multi-agent review`, `cross-check`,
`ask another agent`, `council`, `verify with codex/claude/gemini/pi`.

## Reviewer Lenses

- Correctness: bug, edge case, contract break.
- Tests: missing behavior/interface tests, fake green risk.
- Safety: destructive ops, secrets, auth, dependency/toolchain risk.
- ML/HPC when relevant: leakage, metrics, reproducibility, Slurm resources/logs.

## Local Tool Preference

Before choosing tools, read `~/.oh-my-setting/local/machine.md` if present and
use the recorded `Local Agent CLI Paths` for Codex, Claude Code, and Gemini.

If available, prefer installed local agent CLIs:

1. `codex`
2. `claude`
3. `gemini`
4. `pi` when useful.
5. Otherwise run current-agent review and clearly say multi-agent tooling unavailable.

Do not install tools, authenticate CLIs, push branches, or use connector APIs
unless the user asked for that action.

## Before Running

- Read `git status --short` and the relevant `git diff`.
- Check available CLIs with recorded paths or `command -v codex claude gemini pi`.
- Skip unavailable CLIs without failing the review.
- Include the task goal, changed files, relevant diff, test command/result, and known risks in each review request.
- Ask for findings only: bugs, regressions, missing tests, unclear contracts, unsafe operations.
- Main agent keeps implementation ownership and integrates only findings backed by evidence.

## Output

Compact synthesis:

```md
Consensus:
Must-fix:
Optional:
Disagreement:
Verification:
```

Accept only findings tied to code, logs, tests, docs, or reproducible commands.
