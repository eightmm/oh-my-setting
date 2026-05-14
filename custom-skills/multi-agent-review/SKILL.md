---
name: multi-agent-review
description: >
  Multi-agent review workflow for important code changes. Use when the user
  asks for independent verification, cross-agent review, council review, or
  when a change touches high-risk surfaces such as APIs, auth, data pipelines,
  model training, Slurm jobs, dependency/toolchain changes, or releases.
---

Goal: get independent review signals, then synthesize evidence. Do not outsource judgment.

## When

Use for high-risk diffs or explicit requests: `multi-agent review`, `cross-check`,
`ask another agent`, `council`, `verify with codex/claude/gemini/pi`.

## Reviewer Lenses

- Correctness: bug, edge case, contract break.
- Tests: missing behavior/interface tests, fake green risk.
- Safety: destructive ops, secrets, auth, dependency/toolchain risk.
- ML/HPC when relevant: leakage, metrics, reproducibility, Slurm resources/logs.

## Tool Preference

If available, prefer existing orchestrators:

1. `mco` for parallel model/agent review.
2. Pi Agent Suite council/ask-llm when using Pi.
3. `agmsg` for agent-to-agent requests.
4. Otherwise run current-agent review and clearly say multi-agent tooling unavailable.

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
