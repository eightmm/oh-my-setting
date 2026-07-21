# Global Coding Rules

Default: concise, scoped, evidence-driven. Keep procedures in skills or project
contracts.

## Communication

- Reply in the user's language; keep technical text in its original form.
- Keep detail for safety, ambiguity, specifications, and failures.

## Execution

- Inspect first. Infer reversible details locally; ask when authority, interface,
  or risk changes.
- Preserve unrelated work and fail explicitly. Continue inspect -> act -> verify
  while safe in-scope work remains; bound retries.

## Safety

- Ask before destructive or irreversible work, contract/schema/dependency/model
  changes, or expensive compute unless approved. Minimize blast radius; never
  expose secrets.

## Context and Tools

- Use relevant skills; prefer local files, `rg`, shell, and `git`.
- Batch independent calls and parallel file discovery; serialize dependencies.
  Bound output, reuse unchanged results, and re-read after relevant state changes.

## Specification

- Read `PROJECT.md` when present. Resolve consequential choices; specific rules
  override defaults.

## Verification

- Verify proportionally: syntax, focused behavior, broader checks.
- Batch edits, diffs, and tests at feature/file boundaries. Avoid per-edit checks
  and repeated suites; run the final gate once unless risk/failure warrants more.
  Verify risky or cross-cutting changes early.
- Report every skipped, failed, or impossible check. State changed behavior and
  verification evidence.

## Multi-Agent Work

- Give each worker one bounded strategy profile, scope, and success criteria;
  the parent owns admission, verification, commit, push, and synthesis.
- Route harness and native workers by phase: deep planning/gates, balanced
  implementation/review, fast routine analysis. Override mismatched inherited
  model/reasoning settings.
- Run commands/tests directly; do not spawn agents merely to execute them.
  Delegate independent judgment or disjoint writes without duplicating work.
- Use a task-scoped executor only for substantial writes. Workers cannot widen
  authority or recursively delegate.
- Use advisors for irreversible decisions, repeated failures, or release gates.
- Use `agent-harness` for detailed workflows; do not edit `.oms/` manually.

## Project Rules

- Keep language, ML/data, and HPC policy in templates or domain skills.
