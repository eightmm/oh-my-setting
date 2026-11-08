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
`oms peer-delegate --to PROVIDER --read-only --role repo-auditor` for a clean-HEAD audit.

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
lookup and cited summaries. It uses the light tier's newest configured/catalog
Haiku or Luna at low effort, pinned to its exact model ID. No write scopes, implementation, review
gate, continuation or automatic model escalation are permitted. Catalog discovery
does not prove live access or authorize spending; the main retains all decisions.

A panel `scholar` is read-only and uses the deep tier at high effort for multi-source literature research with verified citations. Use `--workload frontier` for the hardest surveys.

Reuse `test-designer`, `patch-reviewer` and `repo-auditor` for tests, patch judgment
and scoped audits. Document drafting is an ordinary scoped write worker. Do not
create a paid seat merely to run a deterministic verification command.

## Built-in task strategies

A role is a task mandate, not an agent instance, provider or model. Select a
bundled profile with `oms agent-role --repo . --name builder resolve` or
`oms peer-delegate --role builder` within existing delegation authority. Role
selection alone neither starts a permanent instance nor grants execution or
publication authority. Project and global overrides retain precedence.

| Role | Mandate | Output | Access | Tier | Independent |
|---|---|---|---|---|---|
| `worker` | Explain, investigate or implement the assigned task | Findings or patch | Read or write | routine | No |
| `builder` | Implement a bounded feature or fix | Patch | Read or write | routine | No |
| `refactorer` | Improve structure while preserving behavior | Patch | Read or write | routine | No |
| `documenter` | Record behavior and decisions | Documentation | Read or write | light | No |
| `release-engineer` | Prepare release and rollback | Release plan | Read | routine | No |
| `reviewer` | Find defects in the diff | Findings | Read | deep | Yes |
| `verifier` | Compare request and acceptance criteria with final result | Requirement evidence | Read | deep | Yes |
| `security-auditor` | Assess trust boundaries and security risks | Findings | Read | frontier | Yes |
| `architect` | Design components and interfaces | Structure | Read | deep | No |
| `critic` | Identify failure modes | Critique | Read | deep | Yes |
| `integrator` | Prepare integration and resolve assigned conflicts | Integration patch | Read or write | deep | No |
| `performance-engineer` | Analyze performance and measure authorized changes | Evidence or patch | Read or write | deep | No |
| `advisor` | Recommend a direction | Recommendation | Read | deep | No |
| `researcher` | Perform bounded source lookup | Cited summary | Read | light | No |
| `scholar` | Research across sources and verify citations | Research synthesis | Read | deep | No |

Independent means the model family must differ from the writer's, unless explicitly pinned.
Builder creates new behavior; refactorer preserves behavior.
Reviewer finds diff defects; verifier compares the original request with the final result.
Advisor chooses a direction; architect designs structure; critic identifies failure modes.
Researcher performs bounded lookup at the light tier; scholar performs multi-source research with verified citations.

Per-role models: tier routing stays until a paired experiment shows an advantage.
After the settlement fix lands, compare 20 paired routine builder tasks on the same
snapshot and brief (claude-sonnet-5-5 versus gpt-6.1-terra), with blind owner judgment.
Move one candidate ahead in its tier list only for a >= 15 point first-submission
acceptance gap, exact McNemar p <= 0.05, and no new P1 defect. Automatic role
routing reserves claude-fable-5-1 for security-auditor; explicit pins are unchanged.

In the terminal panel, `oms panel --dispatch ROLE` accepts `worker`, `researcher`, `scholar`
and the built-in strategies below. Existing standard names remain valid for
`peer-delegate --role`; use this panel mapping when dispatching from the panel:

| Standard role | Panel front door |
|---|---|
| `repo-auditor` | `researcher` (bounded lookup) or `reviewer` (diff review) |
| `implementation-worker` | `worker --access write` |
| `test-designer` | `advisor` (test strategy) or `worker --access write` (test implementation) |
| `patch-reviewer` | `reviewer` |
| `decision-advisor` | `advisor` |

Other built-in strategies are accepted directly. The model comes from the
role's capability tier (light, routine, deep, frontier) across installed
providers, not from a provider choice.
Only `worker`, `builder`, `refactorer`, `documenter`, `integrator` and
`performance-engineer` may take `--access write`.

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

For panel tasks, choose the role from source evidence: security-auditor for
risky authentication, permissions, sensitive data, signals, install, deletion,
destructive operations, scripts or hooks; verifier before declaring completion
(original request against final result); architect for structural decisions;
integrator for merging/conflicts; performance-engineer when measurement is
needed; documenter at completion; release-engineer for deployment. Use each
when its trigger applies, one at a time, rather than invoking every specialty.

Measurements needing scratch writes use `worker --purpose investigate --access
write` with a brief and verifier. Scratch stays outside the repository;
repository patches fail, and findings await owner finalization as `completed`,
never `accepted`. Builders write experiment scripts/configs; only the main
authorizes launches within the user's paid-compute authority. A worker reports
finished outputs; `oms job-digest` condenses training/Slurm logs. Wait with one
background wait or notification, not repeated polls.

Panel `advisor` and `reviewer` dispatches load `roles/advisor.md` and
`roles/reviewer.md` through the same project override, global override, bundled
default resolver used by `oms agent-role`, except review gates: `peer-review --gate`
uses `agent-role --global` resolution (global override, then bundled default),
excluding worker-mutable project reviewer profiles. The standalone advisor defaults to
`decision-advisor`; select `--strategy advisor` when the panel strategy is
desired.
