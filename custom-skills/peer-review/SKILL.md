---
name: peer-review
description: >
  Multi-agent code review. Use when the user explicitly requests independent
  verification, cross-agent/council review, or a release go/no-go or requested
  ML pre-training gate needs several independent reviewers. High-risk changes
  alone do not require this skill.
---

# Peer Review

Get independent findings without outsourcing judgment. The parent reviews the
evidence and owns fixes, verification, landing, commit, and release.

## Use

- Review the current status and diff before sending context.
- Use the smallest relevant diff/base and exclude untracked content unless it
  was intentionally added to the review boundary.
- Ask for concrete bugs, regressions, unsafe behavior, and missing interface
  tests; omit style preferences.
- Use `--verify CMD` as a mechanical backstop. Reviewer consensus cannot turn a
  failing command green.
- Use one provider through `oms agent-run --mode read` when three independent
  signals are unnecessary.

```bash
oms peer-review --repo . --prompt "Review this diff for blocking findings."
oms peer-review --repo . --base origin/main --verify "bash scripts/check.sh"
oms peer-review --repo . --ml   # only for an explicit ML gate
```

## Safety Boundary

Never send secrets, credentials, private keys, env files, private paths,
machine/cluster details, raw datasets, checkpoints, or generated scratch state.
If sensitive-looking content occurs inside an otherwise relevant diff, keep
that portion local instead of line-redacting it into external context.

Local Codex, Claude Code, and Antigravity review CLIs may receive sanitized
task context when this skill is explicitly invoked. This does not authorize
installation, authentication, write mode, permission bypass, destructive
commands, connectors, commits, or pushes.

## Route to One Reference

- Context selection, exclusions, provider availability, local fallback:
  [context-safety.md](references/context-safety.md)
- Gate/verdict behavior, debate, synthesis, mechanical verification:
  [gate-loop.md](references/gate-loop.md)
- Policy-restricted export and result import:
  [export-import.md](references/export-import.md)

Summarize findings first by severity with file/line evidence. Separate reviewer
claims from parent-verified facts. Report provider failures/skips and the local
verification result. State the final parent decision explicitly.
