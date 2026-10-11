# Worker Roles

Use one strategy role per worker. Resolve project override, then global
override, then bundled default:

```bash
oms agent-role --repo . --name repo-auditor resolve
```

Standard roles: `repo-auditor`, `implementation-worker`, `test-designer`,
`patch-reviewer`, and `decision-advisor`. Prepend the resolved role text to a
native subagent brief; the parent still supplies task, paths, constraints,
success criteria, and expected output.

Put task-specific behavior directly in the brief; do not launch another model
to generate instructions. Roles specialize the task, never its authority.
Use `oms consult --to PROVIDER` for an independent read, or
`oms peer-delegate --read-only --role repo-auditor` for a clean-HEAD audit.

## Brief and return

Exploration, implementation and research are task responsibilities, not a
mandatory team. Reuse the standard roles where they fit; a bounded research
question can live directly in a read-only brief. The parent chooses the model
separately using [model-routing.md](model-routing.md).

A brief identifies the missing fact or observable result, allowed paths and
actions, relevant source references, constraints and verification criteria.
Include only the context needed for that task. A worker returns:

- Findings or changed paths, with source or patch evidence.
- Checks actually run, their outcomes, and checks skipped or unavailable.
- Unresolved decisions or blockers requiring the parent.

The parent compares the return with the brief and verifies the integrated
tree. Worker completion, patch admission and final acceptance are separate
states; a worker's passing check does not establish final acceptance.

Soul executors are retired. Old records remain evidence, not runnable contracts.
Preserve their patches and approvals; create a fresh reviewed plan task rather
than stripping the old executor binding. Normal plan leases, scoped admission,
one-shot repair and one-use approval checks remain in force.

## Cheap research and existing specialties

A panel `researcher` is a read-only child of a main for bounded document/source
lookup and cited summaries. Use the newest configured/catalog Luna or Haiku at
low effort, pinned to its exact model ID. No write scopes, implementation, review
gate, continuation or automatic model escalation are permitted. Catalog discovery
does not prove live access or authorize spending; the main retains all decisions.

Reuse `test-designer`, `patch-reviewer` and `repo-auditor` for tests, patch judgment
and scoped audits. Document drafting is an ordinary scoped write worker. Do not
create a paid seat merely to run a deterministic verification command.

## Built-in task strategies

A role is a task mandate, not an agent instance, provider or model. Select a
bundled profile with `oms agent-role --repo . --name builder resolve` or
`oms peer-delegate --role builder` within existing delegation authority. Role
selection alone neither starts a permanent instance nor grants execution or
publication authority. Project and global overrides retain precedence.

| Role | Use and purpose | Default scope |
|---|---|---|
| `builder` | Implement a bounded feature or fix | Assigned code paths |
| `refactorer` | Improve structure while preserving behavior | Assigned code paths |
| `reviewer` | Independent correctness and regression review | Read-only |
| `architect` | Decide how components and interfaces fit together | Read-only |
| `advisor` | Recommend which direction to take | Read-only |
| `critic` | Identify when a design fails | Read-only |
| `security-auditor` | Review security, permissions and hazardous changes | Read-only |
| `verifier` | Compare the original request with the final result before closure | Read-only; authorized deterministic checks |
| `integrator` | Prepare branch integration and resolve assigned conflicts | Assigned conflicts; merge needs separate authorization |
| `performance-engineer` | Analyze performance and benchmark when necessary | Authorized bounded measurements and assigned paths |
| `documenter` | Record behavior, decisions and completion handoff | Assigned documentation paths |
| `release-engineer` | Prepare packaging, deployment and rollback when releasing | Read-only preparation; execution needs separate authorization |

Resolve only the roles needed for the task. Advisor asks which direction is good,
Architect asks how to structure it, and Critic asks where it fails; these are
distinct judgments and do not require three simultaneous agents. A Builder
session may become a Refactorer after its implementation. Independent Reviewer
work must use a separate context from the implementer; re-labeling an
implementation session does not establish independence. Independent verification
uses the same separation. Select a compatible reusable session or create an
authorized instance only when needed; the main retains final acceptance.

Each profile returns source or patch evidence, actual checks, and unresolved
limits. No profile permits recursive delegation or expands existing authority.
