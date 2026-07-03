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

When the oh-my-setting harness is installed, prefer its tools over ad-hoc
launches so work is reproducible and not duplicated across agents. Invoke a
tool as `oms <tool>` (dispatcher on PATH) or `~/.oh-my-setting/scripts/<tool>.sh`;
`oms list` prints the full catalog, `docs/COMPONENTS.md` the details.

- `oms init` seeds `.oms/` and prints a next-actions checklist when you land in
  a fresh repo; `oms state` (repo-state) is the read-only dashboard: active
  task/plan/board, in-flight delegations, open runs, latest CI, and unresolved
  failures. Run one of these first when starting or resuming a repo.
- Before retrying a command that may be a known dead end, `oms fail-ledger
  check --cmd "..."`; record a new dead end with `record`. `oms gc` (dry-run by
  default) reclaims aged `.oms/` state.
- Shared state — all three agents read/write the same repo-local `.oms/`:
  shared memory (`oms agent-memory`, incl. `search`), active task packet
  (`oms agent-task`), subtask DAG (`oms agent-plan`: `ready`,
  `next --claim --provider NAME`, `touch` to heartbeat a long claim, `reclaim`).
- Cross-agent work: route one provider through `oms agent-run --to NAME`
  (read-only pass vs isolated write worktree); give the worker a reusable
  persona with `oms multi-agent-delegate --role NAME` (roles live in
  `.oms/roles/`, managed by `oms agent-role`); land a delegated patch through
  `oms patch-land` (clean-tree → admission gate → apply → record), or gate it
  alone with `oms patch-admit`.
- Claim an experiment on the study board (`oms experiment-board`) before a
  long/expensive run; it refuses a duplicate already-active claim.
- Wrap a run worth reproducing in `oms run-capsule` (commit + diff + env/seed +
  result); use the run ledger (`oms run-ledger`) for lightweight rows. Mint one
  run id (`oms run new`); other shells join it via `oms run current`, and
  `oms run timeline` answers "what happened in this repo".
- For ML data, check `oms data-manifest leakage` before training when splits exist.
- Reconcile long Slurm jobs (`oms run-reconcile`) into shared state at session
  start when work may have finished while away.
- Set `OMS_AGENT` (codex|claude|antigravity) in the CLI's env for state
  attribution; Claude Code is auto-detected and spawned workers inherit it.
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
