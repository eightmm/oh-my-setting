---
name: peer-ask
description: >
  Ask Codex, Claude Code, and Antigravity the same conceptual or planning
  question, then synthesize independent perspectives. Use for councils,
  cross-model opinions, or design tradeoffs rather than diff review.
---

# Peer Ask

The current agent keeps judgment and synthesis. Use local provider CLIs only;
use `peer-review` for a code diff and `agent-run --mode read` for one provider.

Choose the smallest context:

- Concept or plan: no repository context.
- Current repository state: `--repo-context`.
- Current uncommitted change: `--diff`.
- Specific files: inspect locally and summarize only the relevant evidence.

```bash
oms peer-ask --prompt "Compare the two designs for this constraint."
```

Use `--debate 1` only when independent answers materially disagree. Use
`--export-only` when policy forbids a direct provider call, then import the
answer with `oms import-agent-result`. Inspect `oms peer-ask --help` for uncommon
flags instead of loading them into every prompt.

Never include secrets, private paths, machine/cluster details, raw logs, data,
or checkpoints. Report consensus, meaningful disagreement, the best supported
answer, caveats, and unavailable or skipped providers.
