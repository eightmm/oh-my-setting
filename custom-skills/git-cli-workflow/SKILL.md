---
name: git-cli-workflow
description: >
  Local git and GitHub CLI workflow. Use for repository inspection, commits,
  branches, pull requests, issues, CI checks, and review feedback when MCP,
  app connectors, or plugin connector tools are not used — e.g. "make a
  commit", "open a PR", "check CI", "커밋해줘", "PR 올려줘", "푸시해줘",
  "CI 확인해줘", "브랜치 정리".
---

Default: local CLI first. No MCP, app connectors, or plugin connector tools.

## Rules

- Start with `git status --short` before edits, commits, pushes, or PR work.
- Inspect local context with `rg`, `git diff`, `git log`, `git show`, and `gh`.
- Do not run destructive git commands unless the user explicitly confirms them.
- Preserve unrelated user changes; stage only task-relevant files.
- Use `gh` for GitHub PRs, issues, checks, reviews, and workflow logs.
- If `gh` is not authenticated or missing, report the exact command the user can run.
- Do not hide failed checks. Report command, result, and relevant lines.

## Common Commands

```bash
git status --short
git diff
git diff --cached
git log --oneline -n 20
gh pr status
gh pr view --comments
gh pr checks
gh run list
gh run view --log-failed
```

## Commit Flow

1. Inspect: `git status --short`, then relevant `git diff`.
2. Verify: run the narrowest useful check.
3. Stage: only task-relevant files.
4. Commit: concise subject; body only when why is not obvious.
5. Push/PR: use `git push` and `gh pr create` only when requested.

## Output

Use compact prose. For implementation, commit, push, or PR work, report changed
files and checks run. For questions or design discussion, do not use a fixed
status block.
