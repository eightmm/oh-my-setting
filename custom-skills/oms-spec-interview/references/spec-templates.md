# Spec Shapes

Adapt this project-contract shape to the work. Preserve durable goals, scope,
decisions, acceptance criteria, and runtime field names. Include narrative
sections only when relevant; do not invent details to fill the template.

```md
# PROJECT.md
## Status
- State: draft | confirmed
## Project
- Goal:
- Users/workflow:
- Scope:
- Non-goals:
## Interface and Data
- Public API/CLI/config:
- Persistence/schema:
- Inputs/outputs:
## Commands
- Setup:
- Test:
- Run:
## Verification
- Success criteria:
- Affected checks:
- Always-run checks:
- CI scope/runtime:
- Required checks:
- Required check files:
## Decisions
- Confirmed:
- Open:
```

Keep the canonical `State` field and existing state-transition/adoption rules.
Keep `State: draft` while task-relevant decisions remain. Record paths,
resources, security constraints, and do-not-touch boundaries only when they
apply; `n/a` may describe an inapplicable narrative fact, with a reason, but
must not replace executable verification.

`Success criteria` is one line. When separate criteria need their own evidence,
an optional `## Acceptance criteria` list makes each bullet a runtime criterion.
An explicit ID keeps evidence bound when the wording changes; the condition and
response form is recommended, not required:

```md
## Acceptance criteria
- [id:REQ-1] WHEN <condition> THE SYSTEM SHALL <observable response>
```

`oms plan-from-spec --validate-spec FILE` (also run by `oms intent adopt`) prints
advisory `warning: spec:` lines for unsettled open decisions, missing
criteria, and missing or reused IDs. Warnings never block adoption;
only the reviewer or owner may mark a criterion satisfied.

`Affected checks` names the repository's narrow changed-file or test-selector
entrypoint. `Always-run checks` lists the small portability, schema, security,
or lifecycle floor that selection may not remove. Leave either `n/a` when the
repository has no supported distinction; never infer safety from an empty or
partial selector.

`CI scope/runtime` records routine PR/push checks and their expected elapsed
time, plus affected-risk/release/scheduled triggers for expensive checks.
Prefer the existing workflow and one primary test layer. Do not copy OMS's
own multi-platform release matrix into a project by default or create CI
merely to complete onboarding. Existing project workflows are user-owned;
changing their required checks needs task authority.

Before broad automation, provide an executable verification command.
`Required checks` takes precedence; when absent or empty, the runtime falls
back to `Test` under `Commands`. Keep either command to one executable Markdown
line. One complete inline code wrapper is allowed (for example,
`` `bash scripts/check.sh` ``). For a custom or composed command, list every
repo-relative regular non-symlink file that defines the verifier under
`Required check files` (comma-separated; at most 64 normalized paths,
240 bytes each). Recognized conventional commands such as `bash scripts/check.sh`,
`make test`, or `pytest` allow automatic verifier discovery; the harness still
enforces the verifier floor when the explicit file list is empty.

When a decision needs its alternatives, interface or risks written down, an
optional [design note](design-note.md) holds them and `Decisions` points at it
with a `- References:` line. It is never a required step.

For routine or non-project work, keep necessary decisions and verification in
the existing task context; no separate spec is required. Ask only about
unresolved material choices or missing authority.
