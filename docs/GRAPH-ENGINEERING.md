# OMS Graph Engineering

This guide is for OMS users inspecting repository relationships and execution
routes, and implementers maintaining both graph engines.
The Project Graph describes code relationships; the Execution Graph describes
legal next actions. Both sit above the control plane and do not own authority;
neither replaces `agent-plan`, `plan-run`, `patch-admit`, `patch-land`,
`goal-drive` or `autopilot`. The schemas, module boundaries and function
signatures below retain the original design and migration context.

For the current native implementation, use the
[graph crate map](../crates/oms-command-graph/README.md) and
[retrieval comparison](GRAPH-RETRIEVAL-COMPARISON.md). The
[archived guide at 20b775f6](https://github.com/eightmm/oh-my-setting/blob/20b775f6/docs/GRAPH-ENGINEERING.md)
retains the Python layout and test inventory for migration history.
The native retrieval benchmark entrypoint is `oms graph project benchmark`.
Language-specific extraction and resolution limits must be checked before
treating a sparse graph as complete.

## In this guide

- [Quick start](#quick-start)
- [Ownership boundaries](#ownership-boundaries)
- [Execution lifecycle and GraphSpec](#execution-lifecycle-vs-semantic-outcome)
- [Project Graph](#project-graph)
- [CLI reference](#cli)
- [MCP and tests](#mcp)
- [Non-goals](#non-goals-v1)

## Quick start

Common commands build or check the Project Graph, inspect a bounded map, and
validate or inspect an Execution Graph:

```bash
oms graph project ensure
oms graph project map --limit 40 --depth 1
oms graph project context --task "TASK_DESCRIPTION"
oms graph exec validate config/graphs/coding-change.json
oms graph exec status
```

The full command syntax and output options are in the [CLI reference](#cli).
For `oms graph`, `--repo` and `--pretty` work before or after a group/action; a later repeated value wins.

```text
                             OMS
                              |
             +----------------+----------------+
             |                                 |
       Project Graph                    Execution Graph
   symbol / call / import          agent / tool / gate / router
   test / config / document        route / repeat / join / terminal
             |                                 |
   context pack  -----> planner -----> GraphSpec + facts
                                               |
                                         route evaluator
                                               |
                                  existing OMS primitives
                       agent-plan  plan-run  patch-land  receipts
```

Principles (from the design conversation, kept verbatim in spirit):

- What the model does not need to decide moves into deterministic code.
- External evidence outranks a model's claim. `completed` without proof is
  `unverified`.
- Context is discovered when needed, never accumulated.
- Current reality outranks execution history.
- The graph owns orchestration ("what is legal next?"); the harness owns
  authority and execution safety ("may this actually execute?").

### External design anchors

OMS adopts the useful invariants, not another graph database or agent
framework:

- [Aider repository maps](https://aider.chat/docs/repomap.html) rank graph-
  connected definitions against the active request and fit them to a context
  budget. OMS keeps its graph local and deterministic, but follows the same
  progressive-disclosure rule: task entries first, then a small neighborhood,
  then explicit expansion.
- [RepoGraph](https://arxiv.org/abs/2410.14684) retrieves task-centered ego
  graphs; its reported direct flattened two-hop variant performed worse than
  the bounded alternatives. OMS therefore opens a task view at one
  `EXTRACTED` hop and leaves two hops or the complete visual slice opt-in.
- [Graft](https://github.com/trailhq/Graft) demonstrates query-time freshness,
  a signatures-only file API, symbol-grouped exhaustive search, and diff blast
  radius. OMS adopts those interfaces over its existing regenerable graph. It
  does not copy Graft's npm/tree-sitter/LSP runtime, model enrichment,
  host-level wiring, telemetry, or viewer server into the portable core;
  unsupported-language coverage is explicit and external parsing remains an
  optional adapter boundary.
- [Sourcegraph precise code navigation](https://sourcegraph.com/docs/code-navigation/precise-code-navigation)
  distinguishes SCIP-backed precise results from search-based fallback, while
  [GitHub Stack Graphs](https://github.github.com/stack-graph-docs/) builds
  per-file partial graphs and stitches paths across files. OMS likewise keeps
  `EXTRACTED`, `INFERRED`, and `AMBIGUOUS` provenance separate and incrementally
  rebuilds file extractions instead of relabeling search resolution as fact.
- [Graphiti](https://github.com/getzep/graphiti) keeps source provenance,
  incremental updates, temporal validity, and hybrid retrieval explicit. OMS
  maps those ideas to repo-relative path/line/source-digest evidence,
  incremental parser cache, Git history, and lexical-plus-edge retrieval. A
  regenerable code graph does not need Graphiti's graph database or LLM
  extraction, and durable run history remains append-only execution events.
- [LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence)
  and [interrupts](https://docs.langchain.com/oss/python/langgraph/interrupts)
  make thread identity, checkpoints, replay, and idempotent resume explicit.
  OMS uses a frozen GraphSpec, `run_id`, append-only events, derived
  projections, parent gates, and reconciliation against the canonical plan;
  conversation memory is never the recovery source.
- [Nx affected](https://nx.dev/docs/features/ci-features/affected) combines
  changed files with the project graph and reverse dependents, while
  [Bazel query](https://bazel.build/query/language) defines reverse-dependency
  closure and test expansion precisely. OMS therefore follows the complete
  reverse closure of source-extracted dependency evidence for test selection.
  An explicitly bounded traversal that can see more dependents fails open to
  the full gate.
- [Graphite](https://graphite.com/docs/intro-to-graphite) is a stacked-PR and
  review workflow, not a code or execution graph engine. Its small,
  independently testable change slices already map to OMS plan tasks,
  isolated patches, admission receipts, and serialized landing; OMS does not
  add Graphite's remote PR authority or CLI dependency.
- [D3 force simulation](https://d3js.org/d3-force/simulation) recommends a
  worker for large static layouts, so the optional browser view stays capped
  and pre-ticks only its small projection. Following
  [W3C SVG accessibility guidance](https://www.w3.org/TR/SVG/access.html), the
  fragment also exposes keyboard controls and an always-present text listing;
  the renderer is self-contained (no CDN or external script), and the listing stays available regardless.

## Ownership boundaries

The native graph uses existing control-plane owners for lifecycle, admission,
and evidence.

| Invariant | Owner | Read surface for the graph layer |
|---|---|---|
| Task DAG, dependencies, lifecycle | [Native Plan](../crates/oms-command-plan/README.md) | `agent-plan status --json`, `show --id ID`, `evidence-snapshot --id ID` |
| Lease, heartbeat and expiry | `agent-plan` claim, touch and recovery actions | `show` includes `claim_expired` and `claim_age_s` |
| `allowed_paths` and `forbidden_paths` | Task record and native [admission scope](../crates/oms-command-landing/src/admission.rs) | `agent-plan show --id ID` |
| Verify command and verifier floor | [Native patch admission](../crates/oms-command-landing/src/admission.rs) | `patch-admit` rows in `.oms/artifacts/index.jsonl` |
| Serialized landing and receipts | [Native patch landing](../crates/oms-command-landing/src/landing.rs) | `.oms/landings.jsonl` and indexed `patch-land` rows |
| Plan acceptance | `agent-plan accept` | `acceptance` rows in `.oms/plan/progress.jsonl` |
| Lifecycle events | [Operations crate](../crates/oms-command-operations/src/events.rs) | `agent-events list` |
| Failure ledger | [Failure command](../crates/oms-command-failure/src/lib.rs) | `fail-ledger list --unresolved --json` |
| Typed projection and evidence | [Runtime guide](OMS-RUNTIME.md) | `oms runtime envelope show`, `evidence show` and `next` |
| Bounded context bundles | [Native runtime context](../crates/oms-runtime/src/context.rs) | `oms runtime context` |
| Locks and atomic file writes | [Native locks](../crates/oms-lock/src/lib.rs) and [safe file operations](../crates/oms-safe-fs/src/lib.rs) | Owning command's storage API |

Adapter rules

- **Front doors are `plan-run` and `patch-land`.** The native Plan and landing
  owners check lease, review and receipt bindings before authority transitions.
- `plan-run` has no `--json`; read the task afterwards with `agent-plan show`.
  `agent-plan next --json` is FIFO by `created`; a concurrent scheduler selects
  by `--id`.
- The dependency field is `depends`. `show` output is a *view*
  (`claim_expired`, `claim_age_s`, `project_contract` are read-time only);
  never hash or persist it.
- `plan-run` exit 3 = no actionable task (not a failure). `patch-land`
  exit 75 = landing lock held (retryable). Parked tasks are `state: blocked`
  with `reason`.
- On a contract-bound plan `add` is refused and `apply-proposal` cannot carry
  `forbidden_paths` or `role`.
- Harness children (`OMS_HARNESS_CHILD=1`) cannot `init`/`add`/`apply-proposal`
  and must pass `--lease-id` for leased mutations.
- `.oms/project-graph/` is ambient for the `check.sh` state guard because a
  graph reader can refresh the regenerable cache independently of the gate.
  The explicitly written `.oms/graph/shadow.jsonl` is ambient too; the rest of
  `.oms/graph/` (runs, events, projections) is **not**. Every test still
  writes into a temporary `--repo`.
- Graph records are ordinary private OMS state. The graph uses the shared
  [outbound scanner](../crates/oms-scrub/src/lib.rs) and native file protections.

## Execution lifecycle vs semantic outcome

These stay separate enums.

| Execution lifecycle (owned by `agent-plan`) | Semantic outcome (graph layer) |
|---|---|
| `ready claimed running review landing done blocked` | `completed failed unverified partial blocked changes_requested approved skipped` |

Example: plan task `state=review` with a stored patch and a `mode: run` node
is outcome `completed`; `state=done` with a landing receipt for a `mode: land`
node is `completed`; `state=blocked` is `blocked`; `state=claimed` after the
process died is `unverified`.

## Package layout

| Source | Responsibility |
|---|---|
| [Native graph command](../crates/oms-command-graph/README.md) | CLI, project build/cache/index/context, execution validation/routing/scheduling, records and rendering |
| [Graph extraction](../crates/oms-graph/src/lib.rs), [Python helper](../crates/oms-graph-python/src/lib.rs), [shell extraction](../crates/oms-graph-shell/src/lib.rs), [resolver](../crates/oms-graph-resolve/src/lib.rs) | Per-language extraction and relationship resolution |
| [Bundled GraphSpecs](../config/graphs) | Explicit coding and goal-drive examples |

The [native crate map](../crates/oms-command-graph/README.md) describes the
buffered entrypoints and host service adapters.

## Execution GraphSpec (schema 1)

JSON is canonical; there is no YAML dependency.

```json
{
  "schema": 1,
  "id": "coding-change",
  "entry": "inspect",
  "budget": {"max_steps": 20, "max_repeats": 3},
  "stop_facts": [],
  "nodes": {
    "inspect":    {"kind": "tool",  "tool": "project_context", "task": "${goal}", "max_files": 12, "cacheable": true},
    "implement":  {"kind": "agent", "effect": "write", "plan_task": "next", "bind_task": "work_item", "mode": "run",
                   "context": {"task": "${goal}", "max_files": 12},
                   "proof": ["binding.work_item.patch_present", "binding.work_item.state=review"]},
    "review":     {"kind": "gate",  "authority": "parent",
                   "decisions": ["approved", "changes_requested"]},
    "land":       {"kind": "agent", "effect": "write", "plan_task_from": "work_item", "mode": "land",
                   "proof": ["binding.work_item.receipt.land.present", "binding.work_item.state=done"]},
    "commit":     {"kind": "tool",  "tool": "commit_bound", "binding": "work_item", "proof": ["!git.dirty"]},
    "acceptance": {"kind": "tool",  "tool": "plan_acceptance",
                   "proof": ["receipt.acceptance.latest=pass", "receipt.acceptance.fresh"]},
    "done":       {"kind": "terminal"},
    "replan":     {"kind": "terminal"},
    "parked":     {"kind": "terminal"}
  },
  "edges": [
    {"from": "inspect",    "to": "implement",  "outcomes": ["completed", "unverified", "skipped", "failed"]},
    {"from": "implement",  "to": "review",     "outcomes": ["completed"]},
    {"from": "implement",  "to": "implement",  "outcomes": ["failed", "unverified"], "kind": "repeat"},
    {"from": "implement",  "to": "parked",     "outcomes": ["blocked"]},
    {"from": "review",     "to": "replan",     "outcomes": ["changes_requested"]},
    {"from": "review",     "to": "land",       "outcomes": ["approved"]},
    {"from": "land",       "to": "commit",     "outcomes": ["completed"]},
    {"from": "land",       "to": "land",       "outcomes": ["partial"], "kind": "repeat"},
    {"from": "land",       "to": "parked",     "outcomes": ["failed", "unverified", "blocked"]},
    {"from": "commit",     "to": "acceptance", "outcomes": ["completed"]},
    {"from": "commit",     "to": "parked",     "outcomes": ["failed", "unverified"]},
    {"from": "acceptance", "to": "done",       "outcomes": ["completed"]},
    {"from": "acceptance", "to": "implement",  "outcomes": ["failed", "unverified"], "kind": "repeat"}
  ]
}
```

`changes_requested` ends at the `replan` terminal on purpose. `plan-run --id`
accepts a `review` task only with `--land`; the canonical way back to `ready`
is the parent's `agent-plan release`, which the graph layer never calls, so
the graph does not pretend a repair transition exists.

Normalized node fields (`spec.normalize_spec` fills defaults):

| field | kinds | default | meaning |
|---|---|---|---|
| `kind` | all | required | `agent tool gate router subgraph terminal` |
| `title` | all | node id | display only |
| `effect` | agent, tool | `read` | `read write none`; `write` nodes are never cached and are scope-checked by the scheduler |
| `plan_task` | agent | — | a literal `agent-plan` task id, or the selector `next`; exactly one of `plan_task` / `plan_task_from` |
| `bind_task` | agent | — | name under which the resolved concrete task is recorded for the run (`binding.<name>.*` facts) |
| `plan_task_from` | agent | — | run the task another node bound under this name; blocked with `required_resources: [{"kind": "task_binding"}]` until it is bound |
| `mode` | agent | `run` | `run` stops in review; `land` continues through `patch-land` |
| `provider` | agent | run option `--worker` | write-capable transport |
| `tool` | tool | — | a registered capability (`plan_acceptance`, `project_context`, `commit_bound`); the runner builds the exact argv, no shell; exactly one of `tool` / `command` |
| `task`, `max_files` | tool `project_context` | `${goal}`, 12 | the pack's query (`${goal}` substituted by the runner) and file bound |
| `binding` | tool `commit_bound` | required | the task binding whose landed patch is committed |
| `command` | tool | — | run with `bash -c` from the repo root, bounded by `timeout`; its `effect` is a declaration the validator warns about |
| `timeout` | tool | 600 | seconds |
| `proof` | agent, tool | `[]` | fact predicates that must hold for a claimed `completed` to stand |
| `requires` | all but terminal | `[]` | fact predicates that must hold before the node is actionable |
| `cacheable` | tool with `effect: read` | `false` | result cache keyed per "Caching" |
| `authority` | gate | `parent` | `parent` or `human`; only `exec decide` records a gate outcome |
| `decisions` | gate | `["approved","changes_requested"]` | outcomes a decision may record |
| `join` | any node with ≥2 incoming edges | `all` | `all` waits for every incoming source, `any` for the first |
| `graph` | subgraph | required | key in `subgraphs`; one level deep |
| `context` | agent | `null` | `{"task": str, "max_files": int}` — build a project-graph context pack before the node runs and hand it to `plan-run --context-pack`; `${goal}` in `task` is substituted by the runner, never by a shell |

Normalized edge fields: `from`, `to`, `outcomes` (non-empty list of
outcomes), `kind` (`normal` or `repeat`), `priority` (int, default 0, lower
wins), `fanout` (bool, default false), `when` (fact predicates, default `[]`).
Two edges from the same node matching the same outcome at the same priority
are a fan-out set only when both declare `fanout: true`; otherwise the
validator reports `ambiguous_routes`.

Router nodes carry no work: their outgoing edges are evaluated by `when`
predicates in priority order and the router's own outcome is `completed`.

Fact predicate grammar (`execution.rs`): `key` (truthy), `!key` (falsy or
missing), `key=value`, `key!=value`. Keys match
`^[A-Za-z0-9_][A-Za-z0-9_.:-]{0,199}$`; values compare as strings, with
booleans rendered `true`/`false`.

## Validation

`validate.validate_spec(spec) -> {"ok": bool, "errors": [...], "warnings": [...]}`
where each item is `{"code": str, "where": str, "message": str}`.

Error codes (exact strings): `invalid_schema`, `duplicate_node`,
`unknown_endpoint`, `missing_entry`, `unreachable_node`, `missing_terminal`,
`terminal_outgoing_edge`, `invalid_outcome`, `invalid_repeat_edge`,
`unbounded_cycle`, `unknown_subgraph`, `recursive_subgraph`,
`invalid_fact_reference`, `invalid_plan_task_reference`, `ambiguous_routes`,
`invalid_effect`, `invalid_kind`, `invalid_join`, `invalid_command`,
`invalid_budget`, `invalid_gate`, `invalid_task_binding`,
`unknown_task_binding`, `duplicate_task_binding_writer`,
`unreachable_task_binding`, `invalid_context`, `unknown_tool_capability`.

Cycles are legal. Every cycle must contain at least one `repeat` edge (which
`max_repeats` bounds) or the spec must declare `stop_facts`; `max_steps`
must be a positive integer regardless. Warnings: `unrouted_outcome` (a
non-terminal node has no edge for `failed`/`unverified`), `dead_end`,
`unverified_effect_declaration` (a `command` tool node declares
`effect: read`; nothing verifies the shell text, so prefer a capability).

### Tool capabilities

`execution_validate.rs` holds the registry: a tool node that says `tool: <name>`
carries no shell text, takes its `effect` from the registry (a conflicting
declaration is `invalid_effect`), and runs the registry's exact argv.

| capability | effect | parameters | runs |
|---|---|---|---|
| `plan_acceptance` | read | — | `agent-plan --repo R accept` (the plan's own acceptance contract; records its receipt) |
| `project_context` | read, cacheable | `task` (`${goal}` substituted), `max_files` | `graph --repo R project context --task T --max-files N --json` |
| `commit_bound` | write | `binding` (must have a writer) | `graph --repo R exec commit --binding B --run RUN` |

The registry names exactly two scripts (`agent-plan.sh`, `graph.sh`) and no
lifecycle verb; a test pins that. `command` nodes stay legal for
operator-authored specs — a planner that generates GraphSpecs should be held
to capabilities only. The bundled specs use capabilities only.

Binding rules: `bind_task` and `plan_task_from` are identifiers
(`^[A-Za-z_][A-Za-z0-9_-]{0,63}$`); `plan_task` and `plan_task_from` are
mutually exclusive and one is required on every agent node; only an agent
node may bind; a name has exactly one writer node (a repeating writer may
rebind its own name); every reader names a written binding and must be
reachable from that writer. `plan_task: next` on a write node is legal.

## Selector versus identity: task bindings

`plan_task: "next"` is a *selector* — a rule for choosing a task — not a task.
The runner resolves it to one concrete id before anything is scheduled, through
a read-only `agent-plan next --json` request (never `--claim`; see
[execution_runner.rs](../crates/oms-command-graph/src/execution_runner.rs)):
the graph selects, `plan-run` claims. The concrete id is written onto the
`node_started` row as `task_id`, together with `binding: <bind_task>` when
the node declares one, and only then does `plan-run --id <id>` run. Because
the durable row precedes the subprocess, a runner that dies immediately
afterwards still knows which task it meant.

`events.project()` folds those rows into `projection["bindings"]`
(`{"work_item": {"task_id": "t1", "node": "implement", "attempt": 1}}`);
the latest row for a name wins, so a repeating writer rebinds and replay
reproduces the sequence. There is no second store. A node that says
`plan_task_from: work_item` executes exactly that task — `implement(next)`
choosing `t1` means `land` lands `t1` even after the plan's own `next` has
moved on to `t2`. The invariant: **once an attempt has chosen a concrete
task, its identity does not change during that attempt**; a task claimed by
another session between the peek and `plan-run` is recorded as that task's
`unverified`/`failed` outcome, never silently replaced.

`binding.<name>.*` facts are derived on every evaluation from the projection
plus `collect_facts` (`binding.augment_binding_facts`): `binding.<n>.task_id`,
`plan.task.<id>.<field>` → `binding.<n>.<field>`, and
`receipt.<kind>.<id>.<rest>` → `binding.<n>.receipt.<kind>.<rest>`. The adapter
never sees a binding: the runner hands it an *effective node* whose
`plan_task` is the id and whose proof predicates name that id
(`binding.effective_node`). The legacy `plan.task.next.*` alias remains for
old specs; bundled specs prove `binding.work_item.*`.

A selector with nothing to select (`agent-plan next` exit 3) is recorded as
the node's `blocked` outcome with detail `no-actionable-task` — the plan's
verdict, routed like any other — and a reader whose binding is missing is
`blocked` with `required_resources: [{"kind": "task_binding", "name": ...}]`,
never an anonymous scheduler conflict.

The route evaluator orders attempts by event `seq`: a repeat edge whose
target already ran *after* the source's latest outcome is history the route
walks past, and a finished node older than the node that feeds it is due
again (a gate then awaits a fresh decision). Order-free states (fixtures)
keep the plain repeat semantics. A terminal reached by several alternative
edges is never a join.

## Facts, outcomes, instructions

- **Fact**: externally observable reality, collected by `facts.collect_facts`.
- **Outcome**: the recorded result of one node execution.
- **Instruction**: prompt/resource for an agent. Instructions never declare
  facts, and an agent's prose never produces an outcome.

`facts.collect_facts(repo, *, include=("git","plan","receipts")) -> Dict[str, Any]`
returns a flat dict with these keys (missing keys are absent, not `None`):

| key | value |
|---|---|
| `git.head`, `git.branch`, `git.dirty` | str, str, bool |
| `plan.present`, `plan.all_done`, `plan.has_unfinished`, `plan.contract.satisfied` | bool |
| `plan.actionable` | list of task ids |
| `plan.task.<id>.state` | lifecycle string |
| `plan.task.<id>.patch_present`, `.artifact_present`, `.lease_present`, `.claim_expired` | bool |
| `plan.task.<id>.repair_count` | int |
| `plan.task.<id>.reason` | str |
| `receipt.admit.<task>.latest` | `verified` or `failed` (latest `patch-admit` row for the task) |
| `receipt.land.<task>.present` | bool (`patch-land` row, `exit == 0`) |
| `receipt.acceptance.latest`, `.base_sha`, `.fresh` | `pass\|fail\|error`, str, bool (`base_sha == git.head`) |

`route.effective_outcome(node_spec, claimed, facts) -> (outcome, missing)`:
a claimed `completed` (or `approved`) stands only when every `proof`
predicate holds; otherwise the effective outcome is `unverified` and
`missing` lists the failed predicates. Other claimed outcomes pass through.

## Route evaluator (pure)

```python
route.evaluate(spec, state, facts) -> dict
route.state_from_outcomes(spec, outcomes, *, gates=None, repeats=None) -> state
route.run_fixture(fixture) -> (ok: bool, detail: dict)
```

`state` is the events projection (below) or a fixture-built equivalent:
`{"nodes": {id: {"status": "pending|active|finished", "outcome": ..., "claimed_outcome": ..., "attempts": n}}, "steps": n, "repeats": {"from->to": n}, "gates": {node: decision}}`.

`evaluate` never spawns a subprocess and never reads disk. Output:

```json
{
  "schema": 1,
  "status": "actionable | waiting | gate | blocked | exhausted | terminal | invalid",
  "primary": "review",
  "alternatives": [],
  "reason": "implement completed; review is the only route",
  "required_resources": [{"kind": "plan_task", "id": "implement"}],
  "gate": null,
  "effective_outcomes": {"implement": "completed"},
  "downgrades": [{"node": "implement", "claimed": "completed", "effective": "unverified", "missing": ["plan.task.implement.patch_present"]}],
  "budget": {"steps_used": 1, "max_steps": 20, "repeats": {"implement->implement": 0}, "max_repeats": 3},
  "trace": ["..."]
}
```

Status semantics: `actionable` (primary may execute now; `alternatives` are
fan-out siblings that may run concurrently), `waiting` (a node is active
with no outcome yet), `gate` (primary is a gate node awaiting
`exec decide`), `blocked` (no edge for the effective outcome, `requires`
unmet, or a `stop_facts` predicate holds), `exhausted` (`max_steps` or a
repeat budget spent), `terminal`, `invalid` (validator failed).

Two rules keep "current reality first" from contradicting history:

- Proofs are re-verified against current facts **only at the frontier** —
  the finished node whose route leads to unfinished work. A node whose
  recorded route already led to further finished nodes keeps the outcome
  the runner recorded after its own proof check. Otherwise the spec's own
  happy path would fail: `implement` is proved by `state=review`, and after
  landing the task is `done`.
- An edge that leads back into the current path (for example
  `review --changes_requested--> implement`) re-runs finished work exactly
  like a `repeat` edge and spends the same `max_repeats` budget under the
  key `from->to`.

Fixture shape (`tests/fixtures/graph-routes/*.json`):

```json
{"name": "claimed completion without receipt is unverified",
 "spec_ref": "coding-change",
 "facts": {"plan.task.implement.state": "claimed"},
 "outcomes": {"inspect": "completed", "implement": "completed"},
 "gates": {},
 "expect": {"status": "actionable", "primary": "implement", "downgrades": ["implement"]}}
```

`spec_ref` names a bundled spec in `config/graphs/`; `spec` may inline one.
Optional `repeats` (`{"from->to": n}`) and `steps` (int) pre-load the budget
counters. `expect.downgrades` is a list of node ids; every other `expect`
key is compared verbatim, and keys absent from `expect` are not checked.

## Run events

```text
.oms/graph/runs/<run-id>/
    graph.json        frozen normalized GraphSpec (no timestamps)
    events.jsonl      append-only history
    projection.json   derived, rewritten from events
    artifacts/        node outputs (tool stdout tails, context packs)
```

Run ids match `^run-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{8}$`. Event row:

```json
{"schema": 1, "seq": 3, "ts": "2026-09-01T00:00:00Z", "run_id": "run-...",
 "event": "node_outcome", "node": "implement", "attempt": 1,
 "claimed_outcome": "completed", "outcome": "unverified",
 "facts": {"plan.task.implement.state": "claimed"},
 "idempotency_key": "outcome:implement:1",
 "actor": {"kind": "adapter", "name": "plan-run"}, "detail": "..."}
```

`event` ∈ `run_started node_started node_outcome gate_decision route_evaluated run_finished note`.
The [native event writer](../crates/oms-command-graph/src/execution_events.rs)
allocates `seq` under the run lock, refuses duplicate `idempotency_key` values,
checks sensitive text, and bounds `detail` before appending through the Plan store.
The module's `project` is pure and idempotent. A `node_started` without a later
`node_outcome` leaves the node `active`. Duplicate keys are folded once. `projection.json` is a cache of `project()` and never an input.

Optional row fields (schema 1, old rows still read): `task_id` and
`binding` on `node_started`/`node_outcome` (the frozen identity — see
"Selector versus identity"), `wave_id`/`wave_index` (which scheduler wave
launched the attempt), and context provenance on `node_started`:
`project_graph_revision`, `context_pack_sha256`, `context_file_count` (or
`context: {"status": "unavailable", "reason"}` when the pack could not be
built). The projection carries `bindings`, per-node `task_id`, and per-node
`seq` (the row order of the latest attempt).

Recovery: `runner.resume` rebuilds state from `graph.json` + `events.jsonl`,
then reconciles every `active` node against current reality, appending the
missing `node_outcome` with key `outcome:<node>:<attempt>` and actor
`resume`; the next route follows from that. An agent node's task is the id
its own `node_started` row froze: `review` with a patch (mode run) or `done`
with a landing receipt (mode land) reconstruct `completed`; `blocked` is
`blocked`; a task still `claimed`/`running` under a **live** lease is left
`active` and the run reports `waiting` (the graph never restarts a live
worker and never releases a lease); an **expired** claim is `unverified`
with detail `claim-expired`, for the repeat or recovery route to judge. A
read tool that died is `unverified` and may be re-run by a repeat edge; a
write tool that died is `blocked` (`resumed-write-tool-uncertain`) — rerunning
it blindly could repeat a side effect, so that decision is the operator's.
Conversation memory is never an input.

The execution runner mirrors a finished run to Work Journal as a best-effort
`phase_outcome` event. The native journal owns event validation and materialized
views. The graph's events retain the execution record.
[Native graph runner](../crates/oms-command-graph/src/execution_runner.rs)

## Plan adapter

Native plan dispatch is part of the [graph command](../crates/oms-command-graph/README.md)
and is exercised by the [host adapter fixtures](../crates/oms-command-graph/tests/host_adapter.rs).
The retired Python `adapters/plan.py` signatures and outcome table remain in the
[archived guide at 20b775f6](https://github.com/eightmm/oh-my-setting/blob/20b775f6/docs/GRAPH-ENGINEERING.md#plan-adapter).

## Scheduler

```python
scheduler.scopes_overlap(a, b) -> bool
scheduler.eligible(spec, state, facts, *, route, task_scopes, capacity=1, active=(),
                   resolved_tasks=None, selectors=()) -> {"eligible": [...], "deferred": [{"node": ..., "reason": ...}], "conflicts": [...]}
```

Task identity is an input: the runner resolves every candidate agent node
(literal id, bound name, or one read-only peek per wave shared by all
selector nodes) and passes `resolved_tasks` plus the `selectors` list; the
scheduler never reads the raw `plan_task` string as an identity, and
`task_scopes` is keyed by concrete task id.

Eligibility: route says the node may run **and** `requires` holds **and** it
is not a gate **and** no active or selected node holds the same task
(`same-task` — one lifecycle lease is one resource whatever the path scopes
say) **and** at most one selector-resolved node per wave
(`dynamic-selector-exclusive`) **and** its write scope does not overlap any
selected or active write scope **and** capacity remains. Overlap is
conservative: an empty or unknown write scope conflicts with everything; a
literal directory overlaps a glob whose static prefix falls under it; two
globs overlap unless their static prefixes diverge. `mode: land` nodes and
any `effect: write` tool node are exclusive — they run only when nothing
else is active. Worker execution parallelism never implies landing
parallelism: landing is serialized by `patch-land` itself and the scheduler
keeps it that way.

### Waves and real concurrency

`--jobs N` bounds the number of concurrent jobs in a wave. The coordinator
records `node_started` with `wave_id` and `wave_index` before launching the
eligible jobs on scoped Rust threads. It records outcomes as jobs finish
and owns the event writes and post-run proof checks. The scheduler checks
task identity, scope overlap and mutation eligibility when forming a wave.
[Native scheduler](../crates/oms-command-graph/src/execution_scheduler.rs) ·
[Wave runner](../crates/oms-command-graph/src/execution_runner.rs)

Tool nodes see the run in their environment: `OMS_GRAPH_RUN_ID`,
`OMS_GRAPH_NODE`, `OMS_GRAPH_ATTEMPT`, `OMS_GRAPH_GOAL`, and one
`OMS_GRAPH_TASK_<NAME>` per binding (`OMS_GRAPH_TASK_WORK_ITEM=t1`).

### Exact commit of a bound task

`patch-land` applies a reviewed patch and never commits, and it refuses to
land onto a dirty tree, so a multi-task run needs a commit between landings.
`oms graph exec commit --binding NAME [--run ID]` is the narrowest step that
provides one (`execution_commit.rs`): the bound task must be `done` with a
landing receipt (both read through the adapter's read verbs), the paths are
taken from the task's stored patch headers, the tree may hold no other
change (strangers are refused, never swept), and the commit is made with
`--no-verify` — the same choice the canonical goal-drive driver makes for
its exact commits: admission plus the task verifier are the gate for
autonomous work. It is parent-only, it is not a landing, and it never touches
plan state. The bundled specs run it as a `tool` node with `effect: write`
(exclusive, never cached) proved by `!git.dirty`.

### Context handoff

An agent node with `context` gets a project-graph context pack before it
starts: `project.build.ensure` (a stale or absent graph is rebuilt — it is a
regenerable cache), `project.context.context_pack(task, max_files)`, and the
pack's path goes to `plan-run --context-pack FILE`, which validates it as a
typed input (regular file, not a symlink, bounded size, JSON with
repo-relative `files`/`tests` only — no absolute paths or `..` — and no
secret-shaped text) and forwards it to `peer-delegate --context-pack`, which
renders a "Project Graph orientation" section into the worker brief
(file and test names with reasons; never source bytes — the worker reads its
own isolated worktree). Entries are deduplicated by path, ambiguous edges are
excluded, extracted edges rank ahead of inferred ones, and related tests and
selectable cases rank by confidence and graph distance rather than filename.
The pack carries the graph revision and a capped blast view instead of an
unbounded subgraph. It never widens `allowed_paths` and is written
into no plan state; only `project_graph_revision`, `context_pack_sha256`, and
`context_file_count` are recorded on the node's `node_started` row. The
existing `--context-manifest` bundle stays opt-in; when both are on, every
pack file (after the task's own first allowed path) becomes a required direct
target of the bundle (`oms runtime context --target` is repeatable and the
manifest lists `targets`) — orientation says where to look, the manifest
delivers the bounded bytes.

## Caching

Project-graph extraction cache (required): key =
`sha256(path, content sha256, parser version, PROJECT_SCHEMA)`; unchanged
files reuse their extraction verbatim.

Execution node cache (limited): only `tool` nodes with `effect: read` and
`cacheable: true`. Key v2 = `sha256(EXEC_SCHEMA, normalized node (command
included), git.head, workspace fingerprint, upstream finished outcomes)`,
stored under `.oms/graph/cache/<key>.json`. The workspace fingerprint
(`execution_workspace.rs`) covers what HEAD cannot: tracked modified paths,
staged blobs (`git diff-index --cached`), and untracked non-ignored paths,
each with a content digest; `.oms/` and `.git/` are excluded (the run's own
event appends must not invalidate the cache). It fails closed — a symlink,
an unreadable or oversized dirty file, or more than 200 dirty paths yields no
key, and the cache is then neither read nor written — instead of degrading
to HEAD. Only a *proved* completion is cached: a cached failure would replay
forever behind a repeat edge, because a retry changes nothing in the key.
Write, land, gate, and agent nodes are never cached.

## Project Graph

State lives in `.oms/project-graph/` (git-ignored through `.oms/`):
`graph.json`, `manifest.json`, `cache/`, `context/`. Build is local-first,
deterministic, regenerable, and needs no model, key, or network.

Node:

```json
{"id": "symbol:src/storage.py::append_jsonl", "kind": "function",
 "name": "append_jsonl", "path": "src/storage.py", "language": "python",
 "source_digest": "<sha256 of the file>", "summary": null, "crux": null,
 "metadata": {"line": 288, "qualname": "append_jsonl"}}
```

Ids are repo-relative and stable within one working tree:
`file:<path>`, `module:<path>` (Python packages/modules, shell libraries),
`symbol:<path>::<qualname>` (`kind` is `class`, `function`, `method`, or
`symbol`), `test:<path>`, `config:<path>`, `document:<path>`,
`concept:<slug>` (semantic layer only). Never an absolute path.

Edge:

```json
{"source": "symbol:a.py::f", "target": "symbol:b.py::g", "relation": "calls",
 "confidence": "EXTRACTED", "evidence": {"path": "a.py", "source_digest": "...", "line": 12}}
```

Relations: `contains imports calls references depends_on uses produces
configures validates tests part_of`. Confidence: `EXTRACTED` (the source
states it: an `import`, a `def`, a `source` line, a literal repo path),
`INFERRED` (resolved by name to exactly one candidate), `AMBIGUOUS` (several
candidates; one edge is emitted for every candidate and carries the site's
bounded `evidence.candidate_count`). Candidate ids are reconstructed by
grouping edges on source/path/line/relation instead of copying N-1 ids onto all
N edges. Inferred and ambiguous edges are never presented as facts. They remain
available for orientation and diagnostics, while affected test selection
traverses only `EXTRACTED` edges and reports ignored frontier confidence counts
without calling those relationships proof.

Resolution by name reaches only what the call could name: a Python call
through a parameter, local, or module variable (`node.get()`,
`parser.add_argument()`) yields no edge at all; a member of an import binding
must exist in the bound file (`events.project()` with no `project` there stays
unresolved rather than guessing); a bare Python name never resolves to a
method (only a `self.`/`cls.` member lookup does) and a builtin's name
resolves in-file only; a shell command word (`git`, `wait`, `command`) reaches
only shell functions, never a Python symbol that happens to share the word.
`Class.method()` through a same-file class or an imported one resolves to the
method node.

Parsers (`parsers/`): Python via `ast` (classes, functions, methods, imports
resolved to repo files — `from pkg import name` binds the submodule file when
`name` is one — calls resolved through import bindings and local
definitions; a `self.x()`/`cls.x()` call to a sibling method is an `EXTRACTED`
`calls` edge, an inherited one resolves by name, and every base class becomes a
`depends_on` edge when it lives in the repository); shell via bounded regexes (`name()` / `function name`
definitions, `source`/`.` includes, invocations of repo scripts by literal
path, bare-name calls to functions defined in sourced files); Markdown
(document nodes, `references` to literal repo paths); JSON/YAML/TOML config
nodes; Jupyter notebooks (`.ipynb`, `language: notebook`) by reading the
`source` of code cells with stdlib `json` and handing it to the Python
extractor, so a cell's classes, functions, methods, imports and calls are the
same nodes and edges a `.py` file yields. Nothing is executed and cell outputs
are never inspected, so text that only appears in an output is not a symbol.
Each symbol carries the 1-based `cell` it came from, and `line`/`end_line` are
lines of the `.ipynb` file itself (a cell stored as one JSON string sits on one
line). IPython line magics and shell escapes (`%matplotlib inline`, `!pip ...`)
are blanked when they are what keeps a cell from parsing (a `!=` or `%`
continuation line that parses as Python stays); a cell magic, even after
leading comments (`%%bash`, `%%time`), or any other cell that is still not Python is skipped, as
is every code cell of a notebook whose kernel language is not Python, and the
notebook's `module` node reports `code_cells` and `skipped_cells`. A notebook's
bare-name call resolves like a Python one, to Python symbols only; nothing
imports a notebook, so its symbols are never a candidate for another file's
call. Python docstrings, the contiguous comment immediately above a shell
function, and a Markdown document's first prose paragraph become optional,
bounded deterministic summaries after the shared sensitive-text guard. Python
and shell symbols also carry a bodies-free signature and source span for
`project api`. Test files in conventional test/e2e/integration directories or
with conventional Python/shell test names are `kind: test` and get `tests`
edges to the repo paths they name. Each parser
declares `language`, `extensions`, `version`; `parsers.registry()` maps
extensions to parsers and adding a language is one new module.

Discovery and safety (`extract.discover_files`): `git ls-files --cached
--others --exclude-standard` when inside Git (so `.gitignore` is honored),
`os.walk` otherwise; always skip `.git/`, `.oms/`, `node_modules/`,
`vendor/`, `dist/`, `build/`, `target/`, `__pycache__/`; skip symlinks
(recorded with reason), files over `max_bytes` (default 2 MiB), files with a
NUL byte, and secret-shaped names (`.env*`, `*.pem`, `*.key`, `id_rsa*`).
Source text is data: nothing in a file is ever executed or interpreted as an
instruction, and any `crux` excerpt is dropped when `sensitive_text()` matches.
`--include` / `--exclude` globs override the defaults.

Orientation: every build also writes `orientation.json` (`schema`, `revision`,
`generated_at`, `text`) beside the manifest: a brief of at most 1536 UTF-8 bytes
(areas, file hubs, structural-evidence count, entry commands). `project map
--brief` prints the cached text when its revision matches the graph, otherwise
regenerates it from the loaded graph; it cannot combine with `--json`,
`--ui-model` or `--html-fragment` (exit 2). The text records the build time in
UTC, not an age, because the cache is not rewritten between builds.

Determinism: schema 2 `graph.json` contains `schema`, `revision`, `nodes` (sorted by
id), `edges` (sorted by `source, target, relation`) and **no timestamps**;
the same working tree produces identical bytes. It is written as canonical
one-line JSON and read under its own 256 MiB ceiling: dense ambiguous evidence
can make a repository graph tens of MiB, past the runtime's 8 MiB state-file
default. `map_summary()`
returns `{"revision", "counts": {"kind", "language", "confidence"}, "hubs": [{"id",
"kind", "degree"}], "file_hubs" (top 8 file nodes), "areas" (file count and
language mix per top-level path), "groups": {top-level dir: [module ids]}}`, the shape
the text renderer consumes. The CLI adds the bounded canonical structural-
assurance summary; it never returns the whole graph.

Shell extraction skips literal static case-arm patterns only at an arm head
(the first nonblank line after a clear `case ... in` or arm terminator), while
scanning same-line arm bodies and later body lines normally. Quotes, command
substitutions, continuations, nested cases, or other uncertain syntax keep the
existing conservative extraction until a clear top-level case; Python stdin
heredocs emit top-level module imports like Python files, scoped to the shell
function when present; these add module dependency edges without binding shell
command words to Python symbols. `<<-` strips body tabs while long skipped lines
preserve import line numbers. Other heredocs remain skipped.
Embedded Python functions are not shell symbol nodes. `revision = sha256` over the
sorted `path\tsha256` lines plus `PARSER_VERSION` and `PROJECT_SCHEMA`.
`manifest.json` carries `generated_at`, per-file digests, parser names,
cache hits, skipped entries, and a path-free coverage summary (`parsed`,
`unparsed`, unsupported extensions, skipped reasons). `map`, `check --json`,
and context packs project that same coverage. `build.check` compares working-tree bytes
against the manifest (unstaged edits count) and reports `stale`, `missing`,
and `new` paths plus `outdated` when the manifest's parser or schema version
no longer matches the code (a parser upgrade re-parses everything on the next
`ensure`); a commit alone is never the freshness criterion.

Queries (`query.Graph`): `find(query, kinds, limit)` normalizes snake_case and
CamelCase before scoring name, path, qualname, and summary matches;
`file_api(path)` projects ordered signatures and summaries without bodies;
`search(repo, query)` exhaustively scans indexed files for a case-insensitive
literal, groups hits by enclosing symbol, and ranks them by incoming coupling.
Search previews are bounded, secret-shaped lines are withheld, and source
previews are labelled untrusted data. Notebooks are not searched (they count
in `skipped_files`): their raw lines are JSON that includes cell outputs. The overview verbs (`map`, `find`)
drop test files and their edges by default because tests name many paths and
otherwise dominate hubs — `--include-tests` (or `--kind test`)
brings them back, while `blast` and `context` always keep them; `neighbors(id, relation, direction)`;
`trace(id, direction, depth, relations, confidences)` returns at most 128 nodes
and 160 slim edges by default, with `limits`, `truncated`, and omitted-edge
metadata instead of a broken partial JSON value; `map_summary()` returns counts by
kind and language, top hubs, and module groups. Analytics
(`project_analytics.rs`) are native: evidence degrees and structural assurance.

Ranked search (`project_index.rs`): `oms graph project search --ranked "WORDS"`
reads the query as words, not a literal, and ranks the blocks about them.
Each indexed line belongs to its innermost symbol, or to its file when none
encloses it; that block is one document, scored with BM25 (k1 1.2, b 0.75)
over bodies, comments and identifiers split as `find` splits them. A row
carries the block id, score, the query words it matched and the one line
with the most of them. It is stdlib arithmetic computed per call: no
embedding, no model call and nothing stored. Stale files, notebooks and
secret-shaped previews are handled as in `search`. It is not wired into
`context` ranking; use the recall benchmark's `--snapshot`/`--compare`
before making it a default.

Symbol view (`project_index.rs`): `oms graph project symbol NAME [--limit 1..500]
[--json]` answers "where is it and who uses it" in one read and keeps three
zones apart. `extracted` holds the definitions (path, line, signature) plus
the callers and tests whose edges a parser read from source; `inferred` holds
INFERRED and AMBIGUOUS edges, a resolver's guess; `text_only` holds
whole-word matches of the bare name that no edge accounts for (an import
line, a method call the resolver could not bind, prose). NAME is a node id, an
exact name or qualified name, else a case-insensitive one (`match` says
which). The text zone reuses `search`, so previews are bounded, secret-shaped
lines are withheld, notebooks are skipped, and `text_scan` reports stale or
skipped files. Each zone is cut to `--limit` with the cut counted in
`omitted`. It refreshes the graph like the other readers, a harness child may
run it, and MCP exposes it as `oms_project_graph_symbol`. An empty zone is
not proof that code is unused.

Blast radius (`project_affected.rs`): `changed_paths(repo, base)` compares the working
tree with `merge-base(base, HEAD)` plus untracked files. Without a base it
compares with `HEAD`, or combines staged and unstaged paths before the first
commit. `blast_radius(graph, paths, depth,
relations, confidences)` walks reverse
`imports/calls/references/tests/uses/depends_on` edges and returns seeds,
dependents with distance/confidence, affected files, tests, selectable test
cases, path coverage, and whether a finite depth truncated the closure.
`project affected` uses `depth=0` for the complete reverse closure. A positive
explicit depth is diagnostic-only for selection: if it truncates, the plan is
`mode: full`, never a silently incomplete affected set. Selection follows only
`EXTRACTED` edges; `INFERRED`/`AMBIGUOUS` frontier counts are diagnostic. A
consumer reached only through a variable-prefixed call, such as
`bash "$ROOT/scripts/lib/platform.sh"`, is an `INFERRED` edge and is
not selected, and neither are tests that reach it the same way. A file reached
both at top level and inside a test function runs only that case, though a
shell suite's top-level statements still run under `--only`; only assertions
that depend on another case's top-level output can be skipped. Joining
`INFERRED` consumers would need a whole-versus-case redesign of `blast_radius`.
So a change to a JSON key, CLI option, exit code or file format read through
such a call needs its consumer tests named in `--verify`, or a full gate before
landing. An unmatched executable/data/policy path or a non-document change with
no extracted runnable test still falls back to the complete gate. Ordinary README/CHANGELOG/
CONTRIBUTING prose and Markdown/reStructuredText/AsciiDoc under `doc/` or `docs/`
are listed separately as `documentation_paths` (text: `documentation:`).
Missing prose nodes do not widen otherwise covered code changes to the full
suite; the caller still runs its documentation checks. Extracted dependents of
documents are retained. Policy boundaries, data fixtures, prompts, dirty state,
deletions and renames remain conservative; a `.txt` suffix is not proof of prose.
Each non-document changed path needs its own extracted route to a selected
test; one covered file does not hide another changed file's missing evidence.

Co-change coupling (`project_history.rs`): `oms graph project coupling` reads the last
N commits (`--commits`, default 500, `--no-merges`, commits touching more than
`--max-changeset` 50 files skipped as bulk moves and counted) and scores every
pair of tracked paths by Code Maat's temporal-coupling measure,
`degree = shared_revs / average(revs_a, revs_b) × 100`, keeping pairs with at
least `--min-shared` 5 shared commits and `--min-degree` 30. Each pair says
whether the Project Graph holds any non-`contains` edge between the two paths:
`structural: false` is the coupling no parser can see (config to code, fixture
to module, shell to Python) and `hidden` counts it. History is evidence for
orientation and review, never a dependency: nothing is written to `graph.json`,
deleted paths are dropped, `--path` focuses on pairs touching a path, and
`--limit` bounds the JSON with `truncated`/`omitted`. Harness children may run
it (read-only, regenerable, like the other readers).

Signature map (`context.signature_map`): `oms graph project map --signatures
--task TEXT [--max-bytes N]` ranks files exactly as `context` does
(`select_context`, no pack written) and prints each ranked file's `api`
signatures until the byte budget (default 8192, 256–262144) is spent. Every
line is a declaration the parsers extracted: no body, no comment block, no
Markdown or notebook output cell. A file cut at a line boundary is named in
`truncated`, a ranked file that got no room in `omitted`, and once one file is
cut no lower-ranked file takes the leftover bytes. `--entry-path` anchors it
to exact files, `--max-files` bounds the ranking, and `--task`/`--entry-path`
without `--signatures` are refused. It is deterministic and makes no model call.

Impact brief (`project_affected.rs`): `oms graph project impact --base REF [--head REF]`
joins what a reviewer or worker brief needs for one range: the `affected`
plan (mode, reasons, tests, cases, documentation paths), the files that depend
on the changed ones through EXTRACTED edges (`--depth`, default 3; changed
files and tests are not repeated there), the paths history says usually move
with a changed path but this range left alone (`structural: false` is the
partner no parser can see), and, only when the repository has a `CODEOWNERS`
file (the first of `.github/CODEOWNERS`, `CODEOWNERS`, `docs/CODEOWNERS`, the
order hosts read them; a symlink or a file over 1 MiB is treated as absent), the owners of each changed path by the
last matching rule. The CODEOWNERS reader is stdlib and read-only; negations,
character classes, escapes and section headers are skipped, never guessed.
Every list is cut to `--limit` (default 20, 1–500) with the cut counted in
`omitted`. The brief is labelled orientation: it says where to look and is
never evidence that a change is safe, tested or approved.

Unreferenced symbols (`project_index.rs`): `oms graph project unreferenced
[--limit N] [--include-tests] [--json]` lists function, method and class nodes
whose only incoming edges are `contains`. `main` and dunder methods are skipped
and tests are hidden by default. Rows are review candidates, not dead code: a
dynamic call, decorator, entrypoint table, framework hook or unparsed file
leaves no edge, and the output says so. It reads the graph only and may run in a
harness child.

Context pack (`project_context.rs`): lexical query → path-deduplicated entry nodes →
EXTRACTED/INFERRED neighborhood (depth 2; AMBIGUOUS omitted) → confidence-
and-distance-ranked files, tests, selectable test cases, capped blast view,
hubs, canonical structural assurance, graph revision, and parser coverage,
bounded by
`max_files` (default 12), `max_nodes`
(default 40), and 40 test cases; written to
`.oms/project-graph/context/<digest>.json`. Size is reported as
`byte_estimate` (never called tokens): raw candidate file bytes versus pack
bytes. With `--base`, a separate change projection marks changed/untracked
paths and their `EXTRACTED` reverse-dependency impact; inferred or ambiguous
edges never become impact proof. `--bundle` passes the selected files as
explicit targets through the [native context compiler adapter](../crates/oms-command-graph/src/project_context.rs).
It uses the runtime's existing bounded bundle format.

Interactive view (`project_visual.rs`): Project Graph views select a deterministic
hub neighborhood or the production entries/files in a context pack, then cap
the payload at 200 nodes and 1 MiB. Schema 3 hides test files, cases, and their
symbols from the normal topology while retaining their edges as assurance
evidence. `supported` means the production path has an `EXTRACTED` cross-test
link, `needs-evidence` means its path has no such link, and `attention` means
both that no test link exists and the node itself emits an `AMBIGUOUS`
production relation. Ambiguous candidate fan-out remains navigable, but all
candidates from one unresolved source occurrence count as one source site for
assurance and hub ranking. `INFERRED` relations remain visible in the node detail but
do not by themselves turn a node red. Candidate targets remain edge evidence
but are not colored as weak merely because a caller could not resolve between
them. Test evidence is shared by the production path because file-level imports
are common; uncertainty is not spread to unrelated symbols in that file. These
states do not assert that a test passed or a feature is complete. The same
canonical projection appears in `map --json`, `find --json`, context JSON, and
HTML, so every provider sees the same reason and signals. `map --include-tests`
remains an explicit diagnostic
override; context HTML is production-only while context JSON keeps tests and
test cases. The first view shows only `EXTRACTED` edges; relation, confidence,
and assurance filters can reveal weak areas. A task view opens at its
`EXTRACTED` one-hop neighborhood; focus-only, two-hop, all-node, and
changed/impacted views are explicit controls. Search, node selection, keyboard
zoom/reset, zoom/pan, drag, and semantic label reveal stay local to the
fragment. Selecting a node fills and focuses an editable instruction draft;
the user may revise it before the explicit `Send to chat` action, or use the
clipboard fallback when the host bridge is absent. An escaped text-node listing
remains usable by assistive technology without the canvas. The draft
retains the current user request, identifies the selected node as untrusted
repository metadata, asks the agent to refresh graph context, and strengthens
implementation or the narrowest existing verification without adding a new
test by default.
Execution views use the same renderer with projected status, outcomes,
bindings, gates, and repeat edges. The output contains graph metadata only,
never source bytes or `.oms` event details. The renderer is self-contained and
loads no external script; the graph build has no runtime dependency on JavaScript.
On narrow stages the viewport-capped stage height is separate from the world
extent: the world grows with node density, and fit-all zooms out to show it.
Every execution state differs from the others in glyph or stroke pattern, and
an unrecognized status renders as Unknown, never Pending.

## CLI

`oms graph` is the public entrypoint for the
[native graph command](../crates/oms-command-graph/README.md). Text is the default output; `--json` is available
everywhere; `--mermaid` on `exec render`/`exec status`. The selected
readers also accept `--html-fragment ABSOLUTE_PATH`; the explicit file write
is parent-owned and produces an inline agent-UI fragment, not a standalone
website. `project map`, `project context`, and `exec status` can return JSON
and write that fragment in one call; their JSON then includes the exact
`html_fragment` path. This keeps machine evidence and the displayed view on
one graph revision.

```text
oms graph project build  [--force] [--include GLOB]... [--exclude GLOB]... [--max-bytes N]
oms graph project ensure [--max-files N]
oms graph project check
oms graph project map    [--json] [--html-fragment ABS|--ui-model] [--limit 1..200] [--depth 0..4]
oms graph project map    --signatures --task TEXT [--entry-path P]... [--max-bytes 256..262144] [--max-files 1..200] [--json]
oms graph project find   QUERY [--kind KIND] [--limit N]
oms graph project api    PATH [--limit 1..2000]
oms graph project search TEXT [--limit 1..500]
oms graph project search --ranked "WORDS..." [--limit 1..500] [--json]   # BM25 over symbol blocks; words, not a literal
oms graph project symbol NAME [--limit 1..500] [--json]   # definitions, extracted callers/tests, inferred edges, text-only matches
oms graph project neighbors NODE [--relation R] [--direction in|out|both]
oms graph project trace  NODE [--direction in|out] [--depth N]
oms graph project blast  [--base REF] [--path P]... [--depth N] [--limit N]   # --limit bounds every JSON list; `truncated`/`omitted` report the cut
oms graph project coupling [--path P]... [--commits N] [--max-changeset N] [--min-shared N] [--min-degree PCT] [--limit N]
oms graph project unreferenced [--limit N] [--include-tests] [--json]   # candidates only, never proof of dead code
oms graph project affected --base REF [--head REF] [--depth N] [--guard GLOB]... [--runnable GLOB]...  # 0 = complete closure; runnable globs add test paths
oms graph project impact --base REF [--head REF] [--depth N] [--commits N] [--limit 1..500] [--json]   # orientation brief; lists report `omitted`
oms graph project context --task TEXT [--max-files N] [--bundle] [--base REF] [--json] [--html-fragment ABS|--ui-model]
oms graph exec validate  SPEC
oms graph exec render    SPEC [--mermaid|--html-fragment ABS]
oms graph exec route     SPEC|--run ID [--facts FILE] [--outcomes JSON] [--json]
oms graph exec run       SPEC --worker PROVIDER [--model M] [--reasoning-effort E] [--max-steps N] [--jobs N] [--goal TEXT] [--dry-run]
oms graph exec resume    --run ID --worker PROVIDER
oms graph exec decide    --run ID --node NODE --outcome OUTCOME [--note TEXT]
oms graph exec status    [--run ID] [--json] [--mermaid|--html-fragment ABS]
oms graph exec events    --run ID [--limit N]
oms graph exec test      PATH   (route fixtures; a file or a directory)
oms graph exec commit    --binding NAME [--run ID] [--message TEXT]   (parent-only exact commit of the bound task's landed patch)
```

`exec status` prints `bindings:` (`work_item -> t1 (bound by implement#1)`)
and labels agent nodes `[work_item=t1]` (writer) or `[task=work_item→t1]`
(reader); `--json` carries `projection.bindings`. `exec run --dry-run` reports
`resolved_tasks` and `unavailable` selectors beside the eligible set.

`SPEC` is a path or the name of a bundled spec in `config/graphs/`. Common
options: `--repo PATH`, `--pretty`. `build` and `check` accept `--json`;
`check` exits 3 when the graph is stale or absent. `OMS_PROJECT_GRAPH_STATE`
(absolute path) relocates the project-graph state directory, which is how
tests and dogfood runs keep a cache out of the inspected tree.

`project api PATH --json` includes optional `data_contract` and
`callable_contract` objects on Python symbols; text output prints the same
objects after each signature. These are declared source data with the file
digest, path, declaration line and individual field/parameter lines. Annotated
class bodies expose direct field annotations (including `ClassVar` and
`InitVar` declarations), whether a default is present, and exact base spelling.
Direct marker spellings distinguish dataclasses, Pydantic models, TypedDict,
NamedTuple and `nn.Module`/`torch.nn.Module`; classification explicitly says
`direct_marker_spelling_only`. Imports and aliases are not resolved, so a
shadowed marker can match, while an alias or indirect subclass with annotated
fields has `class_kind: "unknown"`. Classes with neither direct markers nor
annotated fields have no data contract. Function-form factories, inherited or
conditional fields, and runtime-generated schemas are outside this view.
Annotated functions and methods, including `forward`, expose every parameter
kind, declared annotation and return annotation; unannotated signatures retain
their existing API row. Annotations preserve source spelling, including quoted
forward references, unless withheld in full. The existing sensitive-text filter
checks raw expressions and decoded string/byte literals; sensitive declarations use
`[redacted: sensitive declaration]`. Expressions exceeding 1024 bytes use
`[omitted: declaration exceeds 1024 bytes]`, rather than a clipped annotation.
F-string/template-string expressions use `[omitted: dynamic string declaration]`.
The existing source and helper-response limits also bound total output.
`basis: "declared_source"` never means runtime validation,
successful type checking, inferred data flow or inferred tensor shapes; shape
comments are not interpreted. Older graphs without these optional objects
remain readable, and parser/build versions force refreshed metadata on the
next normal graph read.

The graph exists wherever it is read. `project ensure` runs `check` and then
builds when the graph is absent, refreshes it through the cache when the
working tree moved, and writes nothing when it is current; it prints `graph:
fresh revision=...` or the `build` summary line with `built`/`refreshed`.
Every reader (`map find api search neighbors trace blast affected coupling impact unreferenced context`) calls it
before loading the graph and sends that summary to **stderr**, so `--json`
stdout stays parsable. `--no-refresh` on any reader, or `OMS_GRAPH_AUTOBUILD=0`
globally, reads the graph as it stands — an absent one then fails with the
old hint. `check` and `build` never auto-refresh: one reports, the other is
the explicit form. Because an auto-build is a side effect of somebody's read,
a *first* build is bounded by `--max-files` (default 20000) and refuses over
it with the verb to run; the explicit `build` (which bounds bytes, not files)
has no such bound, and a refresh of an existing graph is never refused.

Harness children (`OMS_HARNESS_CHILD=1`) may run `project
build|ensure|check|map|find|api|search|neighbors|trace|blast|affected|coupling|impact|unreferenced`
and `exec validate|render|route|status|events|test`. `build`/`ensure` are the exception
to the runtime core's `context` precedent: the project graph is a regenerable
cache that carries no authority, and a delegated worker in an isolated
worktree has no other way to get one. `project context`, whose pack the
parent's brief owns, and every `exec` writer stay parent-only and fail closed
with exit 2.

`SessionStart` intentionally does no graph work. Project-graph readers call
`ensure` lazily; `project ensure` is also available explicitly. Session hooks
never run an execution-graph comparison. The regenerable `.oms/project-graph/`
cache and historical `.oms/graph/shadow.jsonl` ledger are ambient to the
[native state inventory](../crates/oms-command-state/src/inventory.rs). Graph
runs, events and projections remain in the state inventory.

## Retired comparison diagnostic

`exec shadow` was removed: `oms runtime next` owns the control-plane next
action, and `exec route/status` describe actual execution graphs. Historical
comparison rows are preserved, not treated as authority or regenerated.

## MCP

The [MCP tool registry](../crates/oms-mcp-server/src/tools.json) advertises
bounded graph readers. The [native discovery and argv fixtures](../crates/oms-mcp-server/tests/native.rs)
check the descriptors and the graph viewer resource.

The default `core` discovery profile lists graph render, query, trace, and
affected alongside shared-state, panel snapshot, peer and output-reader tools (14 total). Configure the MCP
server environment with `OMS_MCP_TOOL_PROFILE=full` and reconnect to list all
33 tools, including all 12 graph tools. Both profiles accept calls to
every registered tool; only discovery is filtered, with unchanged input/output
contracts and viewer resources. Invalid profiles fail at startup.

| Tool | Equivalent OMS command or response |
|---|---|
| `oms_project_graph_map` | `oms graph project map --json` |
| `oms_project_graph_render` | bounded `project map|context --ui-model`; returns `structuredContent.graph` and links `ui://oms/project-graph/v1.html` |
| `oms_project_graph_query` | `oms graph project find --json --limit 40` + positional `query` |
| `oms_project_graph_trace` | `oms graph project trace --json --depth 2 --direction out` + positional `node` |
| `oms_project_graph_blast` | `oms graph project blast --json --limit 120` |
| `oms_execution_graph_status` | `oms graph exec status --json` |
| `oms_execution_graph_route` | `oms graph exec route --json --run` + positional `run` |
| `oms_execution_graph_events` | `oms graph exec events --json --limit 40 --run` + positional `run` |

The project-graph tools wrap readers, so a call against a stale or absent
graph refreshes the regenerable cache first (summary on stderr, JSON on
stdout); no tool mutates plan, receipt, or run state.

The server advertises `resources/list`/`resources/read` and serves the viewer
as `text/html;profile=mcp-app`. Only the render tool carries
`_meta.ui.resourceUri` (plus the OpenAI compatibility alias), keeping data
queries decoupled from presentation. The resource consumes the render tool's
bounded structured graph; it never reads source bytes in the browser.

Positional values are validated by a per-tool `positional_pattern` (node ids
contain `/` and `:`; the default bare-name rule stays for `oms_handoff_show`);
a value may never start with `-`. Output stays under the server's
`OUTPUT_LIMIT`; no tool returns the whole graph.

## Tests

| Area | Current fixtures |
|---|---|
| Project discovery, cache, query, impact and evidence projections | [Project engine](../crates/oms-command-graph/tests/project_engine.rs) |
| CLI service bindings, bundled graph loading and plan dispatch | [Host adapter](../crates/oms-command-graph/tests/host_adapter.rs) |
| Python, shell and cross-language resolution | [Python graph contract](../crates/oms-graph-python/tests/graph_contract.rs), [shell graph contract](../crates/oms-graph-shell/tests/graph_contract.rs) and [resolver contract](../crates/oms-graph-resolve/tests/resolve_contract.rs) |
| Native MCP discovery, argv and resources | [Native MCP suite](../crates/oms-mcp-server/tests/native.rs) |
| Context pack and write-worker boundary | [Native entry contract](../crates/oms/tests/native_entry_contract.rs) |

```bash
cargo +1.99.1 test --locked -p oms-command-graph
```

Fixtures exercise local repositories and service adapters. The archived
test inventory records checks from the retired Python engine.

## Non-goals (v1)

LangGraph or any framework dependency; Neo4j or a required database; a
required daemon; embeddings or vector stores; model-decided routing;
removing `agent-plan` or `plan-run`; bypassing receipts; unbounded loops;
runtime self-modifying graphs; unconditional parallelism; a web frontend;
tree-sitter in core (the parser registry is the seam for an optional adapter
later); semantic enrichment as a correctness requirement (Phase 9 is an
optional `summary`/`crux`/`concept` pass through existing provider routing).
