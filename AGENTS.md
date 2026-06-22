# Global Coding Rules

Default: terse, explicit, low-token. Preserve meaning; remove fluff.

## Output Style

- Default: compact, direct, low-token. Fragments and arrows OK when clear.
- Cut filler, greetings, repeated caveats, and unnecessary hedging.
- Keep technical terms, commands, code, paths, errors exact.
- Prefer bullets and compact summaries for status/results.
- Do not compress safety warnings, destructive confirmations, specs, or ambiguous steps.
- If user asks for `normal mode` or more detail, expand for that conversation.

## Language

- Reply to the user in the language they wrote in; match it per conversation.
- Everything durable stays English: code, comments, docstrings, identifiers,
  commit messages, PR text, and script output (`echo`/`printf`/usage). These
  are read by tools, tests, and the other agents — keep them machine-stable.
- Reasoning/scratch work: English.
- Exception — intentional bilingual data: keyword/trigger strings that must
  match a non-English user prompt (e.g. intent-classification regexes, skill
  trigger phrases) and example prompts in localized docs. Keep the surrounding
  code and comments English.
- Localized docs live in their own file (e.g. `README.ko.md`), never mixed
  into the English source.

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
- When asked to check, audit, or explain the current implementation, inspect the
  relevant source files/scripts directly before answering. Do not rely only on
  docs, status output, memory, or prior summaries.

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

- Match the format to the turn type:
  - Delegated/implementation work: report compactly; end with changed / verified / not verified. Add next only when a concrete follow-up action exists.
  - Questions, design discussion, tradeoffs, anything the user needs explained: write clear prose; no contract block.
- "not verified" is mandatory whenever any check was skipped or impossible — never omit it to look done.
- Do not emit empty contract lines ("changed: none", "next: none"); drop the line instead.

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

## Run Provenance & Coordination

When the oh-my-setting harness is present, prefer its run tools over ad-hoc
launches so work is reproducible and not duplicated across agents:

- Claim an experiment on the study board (`experiment-board.sh`) before a
  long/expensive run; it refuses a duplicate already-active claim.
- Wrap a run worth reproducing in `run-capsule.sh` (commit + diff + env/seed +
  result); use the run ledger for lightweight rows. Mint one `OMS_RUN_ID`
  (`oms-run.sh new`) so the tools join under one run.
- For ML data, check `data-manifest.sh leakage` before training when splits exist.
- Reconcile long Slurm jobs (`run-reconcile.sh`) into shared state at session
  start when work may have finished while away.
- Admit a delegated patch through `patch-admit.sh` before applying it.
- Do not put secrets in commands, notes, or metrics — outbound context is
  scrubbed and these records are agent-shared.

## Test Strategy

- Prefer behavior/interface tests over tiny per-function tests.
- Test behavior at module/interface boundaries.
- Add narrow unit tests only for fragile pure logic or past bugs.

## Project Rules

- Prefer project `AGENTS.md`/`CLAUDE.md` over global defaults.
- Put programming-language, ML, data, and HPC rules in project templates
  (natural-language policy is global: see Language above).
- Use `templates/project-general-AGENTS.md` for non-ML repos.
- Use `templates/project-ml-AGENTS.md` for ML repos.
- Use `templates/project-slurm-AGENTS.md` as an extra overlay for Slurm/HPC repos.
