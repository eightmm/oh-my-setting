---
name: peer-ask
description: >
  Ask the same conceptual or planning question to Codex, Claude Code, and
  Antigravity, then synthesize the independent perspectives. Use when the user asks
  for a council, cross-model opinions, independent viewpoints, or conceptual
  comparison rather than a code diff review.
---

Goal: get three independent perspectives on the same question, then synthesize.
Do not outsource judgment. Use local CLI tools only. Do not use MCP servers, app
connectors, or plugin connector tools.

## When

Use for conceptual questions, design tradeoffs, planning, alternatives, or
"ask codex/claude/antigravity" requests that are not primarily code review.

For code diff review, use `peer-review` instead. For a one-provider
question, prefer `agent-run.sh`; it records task outcomes and routes read/write
automatically.

## Automatic Context Selection

The user should be able to invoke this skill in natural language. Choose the
smallest context that answers the question:

- General concept, comparison, tradeoff, architecture pattern, or planning
  question: run without repo context.
- Mentions `current implementation`, `this repo`, `current state`, `현재 구현`,
  `현재 상황`, or asks whether the repo's structure/design is appropriate: use
  `--repo . --repo-context`.
- Mentions `current changes`, `diff`, `uncommitted`, `이번 수정`, `현재 변경`,
  or asks about work not yet committed: use `--repo . --diff`.
- Mentions specific files and they are needed for the answer: read those files
  locally first, summarize only relevant non-sensitive context into the prompt,
  then run without `--diff` unless the diff itself matters.
- If the user says `do not edit`, `수정하지 말고`, `opinion only`, or asks only
  for explanation/evaluation, do not edit files; run ask and synthesize only.

If optional repo context would include secrets, private paths, machine details,
or generated cluster details, omit that context and say so in `Verification`.

## External Ask Policy

Sending the prompt and optional sanitized repo context to `codex`, `claude`, and
`antigravity` local CLIs is allowed by default for this skill. Do not include secrets,
private keys, local machine details, generated cluster details, or private paths.

By default no repo context is attached. Use `--repo-context` only when current
repo status matters, and `--diff` only when the diff is needed.

## CLI

```bash
~/.oh-my-setting/scripts/peer-ask.sh \
  --prompt "Compare RAG and fine-tuning tradeoffs for this project."
```

Optional repo context:

```bash
~/.oh-my-setting/scripts/peer-ask.sh \
  --repo . \
  --repo-context \
  --prompt "Given this repo state, what are the next implementation risks?"
```

Optional sanitized diff:

```bash
~/.oh-my-setting/scripts/peer-ask.sh \
  --repo . \
  --diff \
  --prompt "What design alternatives does this diff suggest?"
```

Artifacts are written under `.oms/artifacts/ask/`.

When policy forbids sending repo context to an external provider, use
`--export-only`; run the exported prompt where allowed, then import the answer
with `import-agent-result.sh`. To recover a recent run, use
`artifact-index.sh latest` or `artifact-index.sh failures`.

## Output

Synthesize compactly:

```md
Consensus:
Divergence:
Best answer:
Caveats:
Verification:
```

List unavailable, blocked, or skipped providers under `Verification`.
