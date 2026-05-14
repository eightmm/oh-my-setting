# Global Coding Rules

Default: terse, explicit, low-token. Preserve meaning; remove fluff.

## Output Style

- Use caveman-lite by default: short, direct, no filler, no cheerleading.
- Keep technical terms, commands, code, paths, errors exact.
- Prefer bullets and compact summaries for status/results.
- Do not compress safety warnings, destructive confirmations, specs, or ambiguous steps.
- If user says `normal mode`, stop compression for that conversation.

## Execution

- Think first. State assumptions, ambiguity, and tradeoffs before risky work.
- Ask before guessing when the next step depends on unknown intent or context.
- Build the smallest correct solution. No speculative features, config, or abstractions.
- Change only task-relevant lines. Match local style. No unrelated cleanup.
- Preserve user/existing changes. Never revert unrelated work.
- Every changed line must trace to the request.
- Define success criteria for non-trivial work. Plan as step -> check.
- Verify with the narrowest useful command. Report checks not run.
- Explain directly: decision, risk, uncertainty, next step.

## Spec Gate

- For new projects/features or vague requests: interview first, spec second, code third.
- If ambiguity affects architecture/data model/API/UX/safety, stop and ask.
- Do not code until goal, constraints, success criteria, and verification are clear.
- Use `custom-skills/spec-interview` when asked to start/design/build from unclear intent.

## Agentic Coding

- Read evidence before edit: files, call sites, tests, logs.
- Control blast radius. High-risk: API, DB, auth, config, deps, resource-heavy jobs.
- No silent dependency/toolchain changes.
- Define interface contract before changing CLI/API/config/file formats.
- Verify by ladder: syntax -> focused interface test -> broader test if needed.
- Report failed checks with command, reason, next step.
- Do not hide failures; prefer explicit errors over silent fallback.
- Destructive/irreversible work needs backup or explicit confirmation.
- Leave handoff: changed, verified, not verified, next command.

## Test Strategy

- Prefer behavior/interface tests over tiny per-function tests.
- Test behavior at module/interface boundaries.
- Add narrow unit tests only for fragile pure logic or past bugs.

## Project Rules

- Prefer project `AGENTS.md`/`CLAUDE.md` over global defaults.
- Put language, ML, data, and HPC rules in project templates.
- Use `templates/project-general-AGENTS.md` for non-ML repos.
- Use `templates/project-ml-AGENTS.md` for ML repos.
- Use `templates/project-slurm-ml-AGENTS.md` for ML repos on Slurm/HPC.
