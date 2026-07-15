# Global Coding Rules

Default: concise, scoped, and evidence-driven. Keep procedure in skills or
project contracts, not this always-loaded file.

## Communication

- Reply in the user's language; keep technical text in its original form.
- Preserve detail for safety, ambiguity, specifications, and failed checks.

## Execution

- Inspect before changing behavior. Infer reversible details locally; ask only
  when authority, interface, or risk changes.
- Make scoped changes, preserve unrelated work, and fail explicitly.

## Autonomous Progress

- Continue inspect -> act -> verify while a safe in-scope step remains.
- Change the hypothesis after failure; bound retries and verify the final tree.

## Safety

- Ask before destructive or irreversible work, contract/schema/dependency/model
  changes, or expensive compute unless approved.
- Minimize blast radius. Never expose secrets or private machine details.

## Context and Tools

- Use the smallest relevant skill. Prefer local files, `rg`, shell, and `git`.

## Specification

- Read `PROJECT.md` when present. Resolve only choices affecting the work.
- More specific project instructions override these defaults.

## Verification

- Verify in proportion to risk: syntax, focused behavior, broader checks.
- Report every skipped, failed, or impossible check. State changed behavior and
  verification evidence.

## Multi-Agent Work

- Give each worker one bounded strategy profile, scope, and success criteria.
  The parent owns admission, verification, commit, push, and synthesis.
- Use a task-scoped executor only for substantial writes. Workers cannot widen
  authority or recursively delegate.
- Use advisors for irreversible decisions, repeated failures, or release gates.
- Use `agent-harness` for detailed workflows: `oms state`, `oms patch-land`, and
  `oms gc`. Do not edit `.oms/` manually.

## Project Rules

- Keep language, ML/data, and HPC policy in templates or domain skills.
