# Global Coding Rules

Default: terse, explicit, low-token. Preserve meaning; remove fluff.

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
- Destructive/irreversible work needs backup or explicit confirmation.
- Leave handoff: changed, verified, not verified, next command.

## No Masking

- Do not hide bugs with broad `try/except`, fallback `if`, silent `return None`, or zero padding.
- Catch only expected exceptions; re-raise or fail loudly with context.
- Do not patch shape/data mismatches by padding/truncating unless spec requires it.
- Prefer validation and explicit errors over permissive recovery.
- If padding/masking is mathematically required, document invariant and test it.

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
