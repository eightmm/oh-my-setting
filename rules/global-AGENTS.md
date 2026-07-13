# Global Coding Rules

Default: terse, explicit, and scoped. Keep this file below 600 words; move
reusable procedures to skills, project contracts, hooks, or scripts.

## Communication

- Reply in the user's language. Keep code, identifiers, comments, docs, commit
  text, and script output in English; localized trigger examples may match input.
- Preserve detail for safety, specifications, ambiguity, and failed checks.
  Keep commands, paths, errors, and technical terms exact.

## Execution

- Inspect relevant source before auditing or changing behavior. Infer reversible
  details from local evidence; ask only when scope, authority, interface, or risk
  materially changes.
- Make the smallest task-scoped change, match local style, preserve unrelated
  work, and fail explicitly rather than silently falling back.

## Autonomous Progress

- For non-trivial work, continue through orient -> contract -> act -> verify
  while another safe, in-scope action can make progress.
- On resume, reuse harness task or plan state only when it still matches the
  current objective. Use a plan DAG only for real dependencies or parallelism.
- Do not repeat an unchanged failure; change the hypothesis or approach. Keep
  repair attempts bounded and preserve failure evidence.
- Only verification against the final relevant tree proves completion. A claim,
  task status, artifact, or worktree-only check does not.

## Safety

- Ask before destructive or irreversible work. Also ask before changing public
  APIs, schemas, model architectures, dependencies, checkpoint formats, or
  resource-heavy allocations unless that exact scope was approved.
- Limit blast radius for auth, databases, configuration, dependencies, and
  production or compute resources. Never expose secrets in prompts or state.

## Context and Tools

- Use the smallest installed skill that covers the task. Start repository search
  with `rg`; prefer local files, shell, `git`, and `gh` over external connectors.

## Specification

- For a new project, confirm `PROJECT.md` before coding. For broad work or a
  task-relevant draft, resolve only choices that affect the implementation.
- More specific project instructions override these defaults.

## Verification

- Verify in proportion to risk: syntax -> focused behavior -> broader suite.
  Prefer interface tests and add narrow regression tests for fragile behavior.
- Report every skipped, failed, or impossible check. For implementation,
  summarize what changed and what was verified.

## Multi-Agent Work

- Give each worker one bounded strategy profile and an explicit scope, paths,
  constraints, success criteria, and expected output. The parent owns admission, final
  verification, commit, push, release, and synthesis.
- Use task-scoped executor souls only for substantial writes. Executors cannot
  widen scope or delegate again; provider, lease, paths, base, and verification
  remain authoritative metadata.
- Use advisors for irreversible/high-risk decisions, repeated failures, or
  release gates, not routine completion.
- Use the `agent-harness` skill for detailed harness work. Resume with `oms state`,
  land delegated changes with `oms patch-land`, and clean stale state with
  `oms gc`. Do not edit `.oms/` manually.

## Project Rules

- Keep language, ML, data, and HPC policy in project templates or domain skills.
- Use `templates/project-general-AGENTS.md` for general repositories,
  `templates/project-ml-AGENTS.md` for ML, and
  `templates/project-slurm-AGENTS.md` as the Slurm/HPC overlay.
