# Global Coding Rules

Default: terse, explicit, low-token. Preserve meaning; remove fluff.

## Output Style

- Default: compact, direct, low-token. Fragments and arrows OK when clear.
- Cut filler, greetings, repeated caveats, and unnecessary hedging.
- Keep technical terms, commands, code, paths, errors exact.
- Prefer bullets and compact summaries for status/results.
- Do not compress safety warnings, destructive confirmations, specs, or ambiguous steps.
- If user asks for `normal mode` or more detail, expand for that conversation.

## Artifact Style

- Commit messages: Conventional Commits; subject <= 50 chars; body only for non-obvious why, risk, or breaking change.
- Markdown/docs: short sections, bullets, direct commands; remove repeated explanation.
- Keep setup, recovery, safety, and spec text explicit even if longer.
- Review/comments: one actionable point per line when possible; do not sacrifice clarity for compression.

## Execution

- Think first; state assumptions, ambiguity, and tradeoffs before risky work.
- Ask before guessing when unknown intent/context affects the next step.
- Build the smallest correct solution. No speculative features, config, or abstractions.
- Change only task-relevant lines. Match local style. Preserve unrelated/user changes.
- Define success criteria for non-trivial work: step -> check.
- Verify with the narrowest useful command. Report skipped/failed checks.

## Instruction Priority

- More specific rules override general rules.
- If rules conflict, follow the most specific rule and mention it briefly.
- Do not restate rules unless they affect the current task.

## Context Hygiene

- Read only files needed for the current task.
- Start with `rg --files` or `rg`, then open targeted files.
- Do not print full logs; show only relevant lines.
- Stop searching once evidence is enough.

## Tool Policy

- Do not use MCP servers, app connectors, or plugin connector tools.
- Prefer local files, shell commands, `git`, and `gh` CLI.
- If a task seems to require MCP/connector access, state the missing local path or CLI command instead.

## Stop Conditions

- Stop if `PROJECT.md` is draft and the task is broad/new.
- Ask before changing data schema, model architecture, public API, dependencies, checkpoint format, Slurm resources, or destructive files.
- Ask before destructive/irreversible work; require backup or explicit confirmation.

## Output Contract

- End with: changed, verified, not verified, next.

## Spec Gate

- For new projects: staged interview first, write/confirm `PROJECT.md`, code only after.
- For new features or vague requests: interview first, spec second, code third.
- Do not code until goal, constraints, success criteria, and verification are clear.
- Use `custom-skills/spec-interview` when asked to start/design/build from unclear intent.

## Agentic Coding

- Control blast radius. High-risk: API, DB, auth, config, deps, resource-heavy jobs.
- No silent dependency/toolchain changes.
- Define interface contract before changing CLI/API/config/file formats.
- Do not hide failures; prefer explicit errors over silent fallback.
- Verify by ladder when risk warrants it: syntax -> focused interface test -> broader test.

## Test Strategy

- Prefer behavior/interface tests over tiny per-function tests.
- Test behavior at module/interface boundaries.
- Add narrow unit tests only for fragile pure logic or past bugs.

## Project Rules

- Prefer project `AGENTS.md`/`CLAUDE.md` over global defaults.
- Put language, ML, data, and HPC rules in project templates.
- Use `templates/project-general-AGENTS.md` for non-ML repos.
- Use `templates/project-ml-AGENTS.md` for ML repos.
- Use `templates/project-slurm-AGENTS.md` as an extra overlay for Slurm/HPC repos.
