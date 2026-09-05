# Minimal Change

Use this when choosing an implementation, reviewing a diff for avoidable
complexity, or auditing a repository for duplicated facilities. It guides
judgment only; it creates no mode, hook, state, or mutation authority.

## Evidence before output

Apply the global evidence hierarchy, debugging workflow, safety floors and
completion record. This reference adds implementation tradeoffs, not another
mandatory checklist.

Read enough source, callers, contracts and canonical tests to choose the change
boundary and verification plan; stop when further context cannot change either.
Keep contradictory evidence. Fetch missing relevant source when a context pack
is truncated; do not repeat whole logs or mistake a bounded pack for completeness.
Use `oms-trace` for regressions or anomalies before choosing a rewrite.

## Decide after understanding

Trace the affected flow, callers, contracts, and verification first. Then stop
at the first option that fully satisfies the task:

1. No implementation: current behavior, configuration, or deletion already
   solves it.
2. Reuse an existing repository helper, type, pattern, or decision owner.
3. Use the language standard library.
4. Use a portable native platform feature.
5. Use an already-declared dependency.
6. Write the smallest correct new implementation.

Higher on the list is preferred only when behavior stays equivalent. A short
but clever expression, hidden state, shifted cost, duplicated authority, or
new compatibility branch is not minimal.

## Spend test coverage once

Before adding a test, inspect the related tests surfaced by repository search,
the Project Graph context pack, or a native affected-test tool. Extend the
canonical interface test, table, or fixture when it already owns the contract.
Create a test only for an uncovered observable contract, a reproduced bug, or
a safety boundary; a changed line, refactor, document, or wiring task does not
earn a new test by itself.

Run affected checks and every check required by the project. After they pass,
repeat or expand only for changed inputs, failures, or unresolved risk. A
documentation-only edit does not justify unrelated test runs, but cannot waive
a declared full or release gate. Report any required check not run.

Keep one primary test layer per behavior and use thinner smoke checks only for
real serialization, process, install, platform, or trust boundaries. Graph
relationships are positive reuse evidence, not proof that an omitted test is
irrelevant: an empty, partial, ambiguous, stale, or unsupported selection must
fall back to the project's broader verification contract. Consolidate coverage
only with an explicit reviewed reduction and passing before/after verification.

## Floors that always win

Never trade away an explicit requirement, trust-boundary validation, data-loss
protection, security, accessibility, project portability, backwards
compatibility, a behavior regression, or the declared verification contract.
For a bug, inspect sibling callers and fix the shared cause when that is the
narrowest correct boundary; a symptom-only patch is usually more code later.

If a deliberately simple implementation has a real ceiling, report the
ceiling and the observable trigger for revisiting it. The parent records that
decision through the existing project, journal, memory, or handoff path when
future agents need it; do not create tool-branded comments or another debt
ledger by default.

## Document what code cannot carry

When writing or materially changing comments and docstrings, keep public
contracts established by the task or repository when code and types do not
make them clear: units, ranges, failure behavior, ownership, side effects, and
similar caller obligations. Keep non-obvious rationale, invariants,
compatibility constraints, and workarounds whose removal would change risk.

Omit narration that restates names, types, control flow, literals, or tests.
Do not infer a public contract from naming or visibility alone, delete useful
existing documentation merely to reduce volume, or sweep untouched prose.
Review stale or misleading documentation and lost established contracts as
real risks; documentation density and style alone are not findings.

## Review through existing routes

- Current diff: use `oms peer-review --gate`. A complexity finding needs a
  concrete duplicate facility, unnecessary dependency or layer, speculative
  flexibility, or a narrower behavior-equivalent replacement.
- Whole repository: use `oms peer-delegate --read-only --role repo-auditor`
  with a bounded surface. Rank evidence-backed removals; omit code-golf and
  style.
- Implementation: the shared delegate prompt carries the compact doctrine;
  roles and executor souls refine strategy without changing authority.

File/line evidence, the maintenance or behavioral failure, and the replacement
are required. Line count alone is not evidence, and complexity cannot override
a mechanical verification failure.

## Measure without another benchmark

Use `oms skill-forge eval` for baseline/treatment routing and task value,
`oms semantic-eval` for a reviewed patch, and `oms runtime benchmark compare`
for existing effectiveness, duplicate-work, correction, defect, reversion,
refusal, token, cost, and duration trends. Correctness and required checks are
the gate; fewer files, dependencies, lines, tokens, or seconds are secondary.

This adapts the minimal-solution ladder popularized by
[Ponytail](https://github.com/DietrichGebert/ponytail) (MIT) to OMS's existing
authority and verification contracts; it does not vendor Ponytail's hooks,
modes, commands, or state.

The [GPT-6 Astra prompting guide](https://developers.openai.com/api/docs/guides/latest-model?model=gpt-6-astra#prompting-best-practices)
and [Codex customization guidance](https://learn.chatgpt.com/docs/customization/overview)
(reviewed 2026-09-05) also support auditing conflicting instructions, concise
output, selective reference loading and proportional verification. OMS applies
these principles across providers; it does not adopt model-specific defaults,
recursive delegation, or additional publication authority from guide examples.
