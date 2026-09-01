# OMS Graph Engineering

Two graphs, one harness. The **Project Graph** answers "how is this code
connected?" and the **Execution Graph** answers "what is legal to run next?".
Both sit *above* the existing control plane; neither replaces `agent-plan`,
`plan-run`, `patch-admit`, `patch-land`, `goal-drive`, or `autopilot`, and
neither owns authority. This document is the implementation contract: the
schemas, module boundaries, and function signatures below are what every
implementer codes against.

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

## Phase 0 — who owns what today

Audited on `main` at `3d1f5a1`. The graph layer calls these owners; it never
re-implements them and never keeps a second copy of their state.

| Invariant | Owner | Read surface for the graph layer |
|---|---|---|
| Task DAG, dependencies, lifecycle FSM | `scripts/agent-plan.sh` (`.oms/plan/tasks.json`, schema 3) | `agent-plan --repo R status --json`, `show --id ID`, `evidence-snapshot --id ID` |
| Lease / heartbeat / TTL reclaim | `agent-plan` (`claim`, `next --claim`, `touch`, `reclaim`, `recover-*`) | `show` view carries `claim_expired`, `claim_age_s` |
| `allowed_paths` / `forbidden_paths` | declared in the plan task; enforced by `patch-admit` gate 2 through `scripts/lib/path_scope.py` — never at claim | `show --id` |
| Verify command, verifier floor | `patch-admit` ladder (`apply, secrets, scope, structure, syntax, tests, verifier, verify, verify-floor`) | admission rows in `.oms/artifacts/index.jsonl` (`kind: patch-admit`, `exit`) |
| Landing (serialized, CAS-fenced) | `scripts/patch-land.sh` holding the `.oms/landings.jsonl` lock; refuses, never rebases; exit 75 when another landing holds the lock | `.oms/landings.jsonl`, index rows `kind: patch-land` |
| Plan acceptance | `agent-plan accept` → `.oms/plan/progress.jsonl` rows `kind: acceptance`, `status: pass\|fail\|error`, exit 0/3/2 | `progress.jsonl` |
| Executor contract | `scripts/agent-executor.sh` (frozen souls under `.oms/executors/<id>/meta.json`); launch is `peer-delegate` | `agent-executor show` |
| Lifecycle events | `scripts/lib/agent-events.py` (`.oms/lifecycle/events.jsonl`, four event types) | `agent-events list` |
| Failure ledger | `scripts/fail-ledger.sh` (`.oms/failures.jsonl`) | `fail-ledger list --unresolved --json` |
| Typed projection / evidence coverage | `scripts/lib/oms_runtime/{projection,evidence}.py` | `oms runtime envelope show`, `evidence show`, `next` |
| Bounded context bundles | `scripts/lib/oms_runtime/context.py` (`oms runtime context`) | `plan_context()` |
| Atomic writes, locks, JSONL | `scripts/lib/oms_runtime/common.py` (`atomic_write_*`, `append_jsonl`, `file_lock` over `file-lock.sh`), `scripts/lib/durable-jsonl.py` | import |

Adapter rules that follow from the audit (do not relearn these the hard way):

- **Front doors are `plan-run` and `patch-land`.** `agent-plan land`/`finish`
  are CAS-fenced with six `--expected-review-*` values and a
  `plan-receipt.py` digest that only `patch-land` can compute. The graph layer
  has no code path that calls `land` or `finish`.
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
- `.oms/project-graph/` is ambient for the `check.sh` state guard (the
  session-start hook refreshes it on the session's own schedule); `.oms/graph/`
  is **not**. Every test still writes into a temporary `--repo`.
- Every durable writer into `.oms` is under the durable-writers contract:
  secret-shaped values are refused and absolute home paths never persist.
  Free text goes through `oms_runtime.common.bounded_line` and is refused
  when `sensitive_text()` matches.

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

```text
scripts/lib/oms_graph/
    __init__.py            constants (schemas, vocabularies)
    errors.py              GraphError(CoreError)
    predicates.py          fact predicate grammar (pure)
    spec.py                load/normalize/digest GraphSpec, bundled specs
    validate.py            deterministic validator
    facts.py               fact collectors (plan, receipts, git) -> flat dict
    route.py               pure route evaluator + fixture runner
    events.py              run store: events.jsonl, projection, resume
    scheduler.py           eligibility + write-scope conflicts
    runner.py              `exec run` step loop, gates, tool nodes, caching
    shadow.py              evaluator-vs-control-plane comparison ledger
    render.py              text / mermaid renderers
    child_policy.py        harness-child allowlist
    cli.py                 argparse front door (`oms graph ...`)
    adapters/
        __init__.py
        plan.py            agent node <-> plan-run adapter
    project/
        __init__.py
        model.py           node/edge shapes, ids, sort order
        extract.py         file discovery, safety, per-file extraction
        cache.py           content-addressed extraction cache
        build.py           incremental build, cross-file resolution, freshness
        query.py           Graph index, find/neighbors/trace/map
        analytics.py       degrees, hubs, components, cycles, paths, communities
        blast.py           changed paths -> transitive dependents
        context.py         task-specific context pack (+ optional bundle)
        parsers/
            __init__.py    registry
            base.py        Parser protocol, ParseResult
            python.py      ast-based
            shell.py       regex-based
            markdown.py    documents + path references
            config.py      json/yaml/toml nodes
scripts/lib/oms_graph_core.py   thin entrypoint (mirrors oms_core.py)
scripts/graph.sh                front door: exec python3 .../oms_graph_core.py
config/graphs/*.json            bundled GraphSpecs
tests/test_oms_graph_*.py       unittest modules
tests/fixtures/graph-routes/    route fixtures (no model calls)
tests/graph-smoke.sh            focused suite registered in scripts/check.sh
```

Python 3.9 syntax, standard library only (the gate compiles with
`feature_version=9`; no `match`, no `X | Y` annotations at runtime, no
third-party imports). Reuse `oms_runtime.common` for hashing, JSON, atomic
writes, locks, `run_json`, `install_root`, `repo_root`, `relative_path`,
`bounded_line`, `sensitive_text`, `parse_path_list`, `safe_id`. Reuse
`path_scope` for scope predicates. Reuse `oms_runtime.evidence.artifact_rows`
for receipt rows and `oms_runtime.context.plan_context` for bundles.

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
    "inspect":    {"kind": "tool",  "effect": "read",  "command": "oms graph project context --task \"$OMS_GRAPH_GOAL\"", "cacheable": true},
    "implement":  {"kind": "agent", "effect": "write", "plan_task": "next", "bind_task": "work_item", "mode": "run",
                   "context": {"task": "${goal}", "max_files": 12},
                   "proof": ["binding.work_item.patch_present", "binding.work_item.state=review"]},
    "review":     {"kind": "gate",  "authority": "parent",
                   "decisions": ["approved", "changes_requested"]},
    "land":       {"kind": "agent", "effect": "write", "plan_task_from": "work_item", "mode": "land",
                   "proof": ["binding.work_item.receipt.land.present", "binding.work_item.state=done"]},
    "commit":     {"kind": "tool",  "effect": "write", "command": "oms graph exec commit --binding work_item",
                   "proof": ["!git.dirty"]},
    "acceptance": {"kind": "tool",  "effect": "read",  "command": "oms agent-plan accept",
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
| `command` | tool | required | run with `bash -c` from the repo root, bounded by `timeout` |
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

Fact predicate grammar (`predicates.py`): `key` (truthy), `!key` (falsy or
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
`unreachable_task_binding`, `invalid_context`.

Cycles are legal. Every cycle must contain at least one `repeat` edge (which
`max_repeats` bounds) or the spec must declare `stop_facts`; `max_steps`
must be a positive integer regardless. Warnings: `unrouted_outcome` (a
non-terminal node has no edge for `failed`/`unverified`), `dead_end`.

Binding rules: `bind_task` and `plan_task_from` are identifiers
(`^[A-Za-z_][A-Za-z0-9_-]{0,63}$`); `plan_task` and `plan_task_from` are
mutually exclusive and one is required on every agent node; only an agent
node may bind; a name has exactly one writer node (a repeating writer may
rebind its own name); every reader names a written binding and must be
reachable from that writer. `plan_task: next` on a write node is legal.

## Selector versus identity: task bindings

`plan_task: "next"` is a *selector* — a rule for choosing a task — not a task.
The runner resolves it to one concrete id before anything is scheduled, through
`adapters.plan.peek_next_task` (`agent-plan next --json`, never `--claim`):
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
route.evaluate(spec, state, facts, *, authority=None) -> dict
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
`events.append_event` assigns `seq`, refuses a duplicate `idempotency_key`,
bounds `detail` with `bounded_line`, refuses `sensitive_text`, and writes
through `oms_runtime.common.append_jsonl` (locked, atomic).
`events.project(events, spec)` is pure and idempotent: a `node_started`
without a later `node_outcome` leaves the node `active`; duplicate keys are
folded once. `projection.json` is a cache of `project()` and never an input.

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

Work Journal: `run_finished` is mirrored best-effort through the journal's
own observer (`work_journal_observe` in `scripts/lib/work-journal.sh`, event
type `phase_outcome`, source `projection.json`); the journal keys one row per
(source type, source id), so gate decisions are not mirrored separately and
the events file stays the only authoritative record.

## Plan adapter

`adapters/plan.py`:

```python
plan_status(repo) -> dict                       # agent-plan status --json
task_view(repo, task_id) -> dict                # agent-plan show --id
peek_next_task(repo) -> dict | None             # agent-plan next --json (read-only; None = nothing actionable)
scope_of(task_view) -> {"allowed": [...], "forbidden": [...]}
outcome_from_task(node_spec, task_view, facts) -> (outcome, proof_missing)
build_command(repo, node_spec, *, provider, model="", reasoning_effort="", repair=0, dry_run=False, context_pack="") -> List[str]
execute(repo, node_spec, *, provider, model="", reasoning_effort="", repair=0, timeout=2700, dry_run=False, context_pack="") -> dict
```

The adapter accepts only a concrete node: a `plan_task` selector or a
`plan_task_from` reference is refused, and `_plan_argv` refuses `--claim`
outright. `execute` runs
`plan-run --repo R --to P --id T [--land] [--repair N] [--context-pack FILE]`,
then rereads the task. Outcome mapping:

| mode | condition after `plan-run` | outcome |
|---|---|---|
| run | exit 0, state `review`, patch present | `completed` |
| run / land | state `blocked` | `blocked` |
| run / land | exit 3 (no actionable task) | `blocked` (reason `no-actionable-task`) |
| land | exit 0, state `done`, `receipt.land.<task>.present` | `completed` |
| land | exit 75 (landing lock held) | `partial` |
| any | exit ≠ 0 otherwise | `failed` |
| any | state `claimed`/`running` (process gone) | `unverified` |

The adapter has, by construction, no code path to `agent-plan land`,
`finish`, `claim`, or `review`; a regression test asserts the built argv.

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

`--jobs N` is real fan-out. Each loop iteration is one wave: route → resolve
tasks → scheduler → `node_started` for every eligible node (coordinator
thread, in order, `wave_id`/`wave_index` recorded) → the nodes' subprocesses
on a `concurrent.futures.ThreadPoolExecutor` (stdlib; agent work is a
subprocess, so threads suffice) → one `node_outcome` per node appended by the
coordinator as each finishes. Threads run subprocesses only; every
`events.jsonl` write, every proof check against post-run facts, and every
cache write happens on the coordinator. A wave that cannot even launch a node
records that node as `unverified` (`runner-error: ...`) rather than leaving it
active. Two disjoint explicit tasks may share a wave; the same task, an
overlapping or unknown scope, a landing, a write tool, and a second selector
never do. `tests/test_oms_graph_runtime.py` proves overlap with a barrier
worker, not a wall clock: under `--jobs 2` both workers pass, under
`--jobs 1` the first times out.

Tool nodes see the run in their environment: `OMS_GRAPH_RUN_ID`,
`OMS_GRAPH_NODE`, `OMS_GRAPH_ATTEMPT`, `OMS_GRAPH_GOAL`, and one
`OMS_GRAPH_TASK_<NAME>` per binding (`OMS_GRAPH_TASK_WORK_ITEM=t1`).

### Exact commit of a bound task

`patch-land` applies a reviewed patch and never commits, and it refuses to
land onto a dirty tree, so a multi-task run needs a commit between landings.
`oms graph exec commit --binding NAME [--run ID]` is the narrowest step that
provides one (`oms_graph/commit.py`): the bound task must be `done` with a
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
own isolated worktree). The pack never widens `allowed_paths` and is written
into no plan state; only `project_graph_revision`, `context_pack_sha256`, and
`context_file_count` are recorded on the node's `node_started` row. The
existing `--context-manifest` bundle stays opt-in; when both are on, the
pack's files become the manifest's targets — orientation says where to look,
the manifest delivers the bounded bytes.

## Caching

Project-graph extraction cache (required): key =
`sha256(path, content sha256, parser version, PROJECT_SCHEMA)`; unchanged
files reuse their extraction verbatim.

Execution node cache (limited): only `tool` nodes with `effect: read` and
`cacheable: true`. Key v2 = `sha256(EXEC_SCHEMA, normalized node (command
included), git.head, workspace fingerprint, upstream finished outcomes)`,
stored under `.oms/graph/cache/<key>.json`. The workspace fingerprint
(`oms_graph/workspace.py`) covers what HEAD cannot: tracked modified paths,
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
{"id": "symbol:scripts/lib/oms_runtime/common.py::append_jsonl", "kind": "function",
 "name": "append_jsonl", "path": "scripts/lib/oms_runtime/common.py", "language": "python",
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
candidates; the edge targets the first in sorted order and lists the rest in
`evidence.candidates`). Inferred edges are never presented as facts.

Parsers (`parsers/`): Python via `ast` (classes, functions, methods, imports
resolved to repo files, calls resolved through import bindings and local
definitions; a `self.x()`/`cls.x()` call to a sibling method is an `EXTRACTED`
`calls` edge, an inherited one resolves by name, and every base class becomes a
`depends_on` edge when it lives in the repository); shell via bounded regexes (`name()` / `function name`
definitions, `source`/`.` includes, invocations of repo scripts by literal
path, bare-name calls to functions defined in sourced files); Markdown
(document nodes, `references` to literal repo paths); JSON/YAML/TOML config
nodes. Test files (`tests/**`, `test_*.py`, `*_test.py`, `*-smoke.sh`) are
`kind: test` and get `tests` edges to the repo paths they name. Each parser
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

Determinism: `graph.json` contains `schema`, `revision`, `nodes` (sorted by
id), `edges` (sorted by `source, target, relation`) and **no timestamps**;
the same working tree produces identical bytes. It is written as canonical
one-line JSON and read under its own 256 MiB ceiling: this repository alone
is ~7 MB, past the runtime's 8 MiB state-file default. `map_summary()`
returns `{"revision", "counts": {"kind", "language"}, "hubs": [{"id",
"kind", "degree"}], "groups": {top-level dir: [module ids]}}`, the shape
the text renderer consumes.

Known limit: code embedded in shell heredocs (this repository's own
`python3 - <<'PY'` blocks) is skipped by the shell parser, so the Python
functions inside `agent-plan.sh` are not symbol nodes; the file and its
shell functions are. `revision = sha256` over the
sorted `path\tsha256` lines plus `PARSER_VERSION` and `PROJECT_SCHEMA`.
`manifest.json` carries `generated_at`, per-file digests, parser names,
cache hits, and skipped entries. `build.check` compares working-tree bytes
against the manifest (unstaged edits count) and reports `stale`, `missing`,
and `new` paths plus `outdated` when the manifest's parser or schema version
no longer matches the code (a parser upgrade re-parses everything on the next
`ensure`); a commit alone is never the freshness criterion.

Queries (`query.Graph`): `find(query, kinds, limit)` scores name, path,
qualname, and summary matches; the overview verbs (`map`, `find`, `analyze`)
drop test files and their edges by default because tests name many paths and
otherwise dominate hubs and communities — `--include-tests` (or `--kind test`)
brings them back, while `blast` and `context` always keep them; `neighbors(id, relation, direction)`;
`trace(id, direction, depth, relations)`; `map_summary()` returns counts by
kind and language, top hubs, module groups, and communities. Analytics
(`analytics.py`) are stdlib: degrees, hubs, connected components,
bounded simple cycles, shortest path, deterministic label-propagation
communities labelled by their dominant top-level directory.

Blast radius (`blast.py`): `changed_paths(repo, base)` = `git diff
--name-only <base>` plus untracked files; `blast_radius(graph, paths, depth,
relations)` walks reverse `imports/calls/references/tests/uses/depends_on`
edges and returns seeds, dependents with distance, affected files, and tests.

Context pack (`context.py`): lexical query → entry nodes → neighborhood
(depth 2) → files, tests, blast radius, hubs, bounded by `max_files`
(default 12) and `max_nodes` (default 40); written to
`.oms/project-graph/context/<digest>.json`. Size is reported as
`byte_estimate` (never called tokens): raw candidate file bytes versus pack
bytes. `--bundle` compiles the selected files through
`oms_runtime.context.plan_context(explicit=...)` so the existing bounded
bundle format is reused, not duplicated.

## CLI

`oms graph` is one public verb (`scripts/graph.sh` → `oms_graph_core.py`
→ `oms_graph.cli.main`). Text is the default output; `--json` is available
everywhere; `--mermaid` where a diagram makes sense.

```text
oms graph project build  [--force] [--include GLOB]... [--exclude GLOB]... [--max-bytes N]
oms graph project ensure [--max-files N]
oms graph project check
oms graph project map    [--json|--mermaid]
oms graph project find   QUERY [--kind KIND] [--limit N]
oms graph project neighbors NODE [--relation R] [--direction in|out|both]
oms graph project trace  NODE [--direction in|out] [--depth N]
oms graph project blast  [--base REF] [--path P]... [--depth N]
oms graph project analyze [--hubs N] [--cycles] [--communities] [--path FROM TO]
oms graph project context --task TEXT [--max-files N] [--bundle] [--base REF]
oms graph exec validate  SPEC
oms graph exec render    SPEC [--mermaid]
oms graph exec route     SPEC|--run ID [--facts FILE] [--outcomes JSON] [--json]
oms graph exec run       SPEC --worker PROVIDER [--model M] [--reasoning-effort E] [--max-steps N] [--jobs N] [--goal TEXT] [--dry-run]
oms graph exec resume    --run ID --worker PROVIDER
oms graph exec decide    --run ID --node NODE --outcome OUTCOME [--note TEXT]
oms graph exec status    [--run ID] [--json|--mermaid]
oms graph exec events    --run ID [--limit N]
oms graph exec shadow    [--spec NAME]
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

The graph exists wherever it is read. `project ensure` runs `check` and then
builds when the graph is absent, refreshes it through the cache when the
working tree moved, and writes nothing when it is current; it prints `graph:
fresh revision=...` or the `build` summary line with `built`/`refreshed`.
Every reader (`map find neighbors trace blast analyze context`) calls it
before loading the graph and sends that summary to **stderr**, so `--json`
stdout stays parsable. `--no-refresh` on any reader, or `OMS_GRAPH_AUTOBUILD=0`
globally, reads the graph as it stands — an absent one then fails with the
old hint. `check` and `build` never auto-refresh: one reports, the other is
the explicit form. Because an auto-build is a side effect of somebody's read,
a *first* build is bounded by `--max-files` (default 20000) and refuses over
it with the verb to run; the explicit `build` (which bounds bytes, not files)
has no such bound, and a refresh of an existing graph is never refused.

Harness children (`OMS_HARNESS_CHILD=1`) may run `project
build|ensure|check|map|find|neighbors|trace|blast|analyze` and `exec
validate|render|route|status|events|test`. `build`/`ensure` are the exception
to the runtime core's `context` precedent: the project graph is a regenerable
cache that carries no authority, and a delegated worker in an isolated
worktree has no other way to get one. `project context`, whose pack the
parent's brief owns, and every `exec` writer stay parent-only and fail closed
with exit 2.

The `SessionStart` hook (`scripts/resume-hook.sh`) reports the graph in the
resume block — `- graph: fresh (<revision>)`, `- graph: building in the
background (oms graph project ensure)` after detaching an `ensure` with
`setsid` (never `nohup`, which makes `SIGHUP` unignorable by a trap for
descendants), or `- graph: absent (OMS_GRAPH_AUTOBUILD=0)`. It never blocks
on the build and never fails session start. `.oms/project-graph/` is
therefore ambient to the check gate (`scripts/lib/oms-state-inventory.py`),
like `hooks/` and `work-journal/`.

## Shadow mode

The evaluator never takes authority from `goal-drive`/`autopilot` in this
round. `oms graph exec shadow` (parent-only) evaluates the bundled
`goal-drive` spec against current facts, maps the route to the control
plane's own canonical next action (`oms runtime next` → `next_actions[0]`,
the deterministic transition `inbox`/`state`/`runtime` already share), and
appends one typed row to `.oms/graph/shadow.jsonl`:
`{"schema":1,"kind":"graph-route-shadow","ts","spec_id","spec_digest","route":{...},"control_plane":{"action":"execute_ready_task"},"agree":true|false,"reason"}`.
Mapping: `execute_ready_task`→`implement`, `review_or_land_patch`→`land`,
`finish_landing`→`land`, `record_verified_completion`/`inspect_completed_plan_retirement`→`done`,
`resolve_blocker`/`inspect_plan_contract`→`blocked`, `orient`→`inspect`.
Disagreements are evidence for the next round, never an action.

## MCP

Read-only, argv-only tools appended to `TOOLS` in `scripts/oms-mcp-server.py`
**after** `oms_peer_operations` (the tool order is pinned by
`tests/state-surfaces-smoke.sh`, which is updated in the same change):

| tool | argv |
|---|---|
| `oms_project_graph_map` | `bash scripts/graph.sh project map --json` |
| `oms_project_graph_query` | `bash scripts/graph.sh project find --json --limit 40` + positional `query` |
| `oms_project_graph_trace` | `bash scripts/graph.sh project trace --json --depth 2 --direction out` + positional `node` |
| `oms_project_graph_blast` | `bash scripts/graph.sh project blast --json` |
| `oms_execution_graph_status` | `bash scripts/graph.sh exec status --json` |
| `oms_execution_graph_route` | `bash scripts/graph.sh exec route --json --run` + positional `run` |
| `oms_execution_graph_events` | `bash scripts/graph.sh exec events --json --limit 40 --run` + positional `run` |

The project-graph tools wrap readers, so a call against a stale or absent
graph refreshes the regenerable cache first (summary on stderr, JSON on
stdout); no tool mutates plan, receipt, or run state.

Positional values are validated by a per-tool `positional_pattern` (node ids
contain `/` and `:`; the default bare-name rule stays for `oms_handoff_show`);
a value may never start with `-`. Output stays under the server's
`OUTPUT_LIMIT`; no tool returns the whole graph.

## Tests

- `tests/test_oms_graph_exec.py`: validator (every error code), evaluator
  (initial, completed, failed, unverified, partial/repeat, budget exhaustion,
  unknown node, unreachable, cycle without stop policy, terminal correctness,
  join all/any, subgraph, recursion rejection, missing fact, fact-backed
  completion, claimed completion without receipt → unverified), events
  (resume from events, crash between execution and append, duplicate event
  idempotency, authority/gate preservation), fixture runner over
  `tests/fixtures/graph-routes/`.
- `tests/test_oms_graph_project.py`: same source → same bytes; single edit →
  incremental rebuild with cache hits; unchanged file → cache reuse; new/
  deleted symbol; import, call, cross-file resolution; test relation; blast
  radius; cycle detection; stale detection; symlink rejection; binary skip;
  large-file limit; untrusted source text stays inert; secret-shaped name
  skipped; `ensure` absent → built, stale → refreshed through the cache,
  current → no write at all (graph.json mtime), the first-build file bound
  refused and an existing graph's refresh never refused, and a refresh that
  keeps the discovery options the graph was built with.
- `tests/test_oms_graph_analytics.py`: degrees, hubs, components, cycles,
  shortest path, communities, renderers.
- `tests/test_oms_graph_integration.py`: fake `codex` provider on PATH (the
  `autonomy-plan-run-smoke.sh` pattern) driving `exec run` through
  `plan-run`; regression guards: no lease/scope/admission/review/acceptance
  bypass, no fake landing receipt, no `done` from prose; sequential landing
  of two patches from different bases; exit 75 → `partial`.
- `tests/test_oms_graph_runtime.py`: the bundled `goal-drive` on a real
  two-task plan through an `oms` shim (evidence A: implement bound `t1`,
  the plan's `next` became `t2`, resume landed `t1`, the next cycle rebound
  `t2`, both landed and committed exactly); a selection race (t1 claimed by
  another session after the peek → recorded against `t1`, `t2` untouched);
  the barrier fan-out proof (evidence B) and the same-task / landing /
  selector exclusions; workspace-aware cache hits and misses (unstaged,
  staged, untracked, unsafe → no cache); every resume reconciliation case
  (review, done+receipt, blocked, live lease → waiting, expired lease →
  unverified, dead write tool → blocked, binding preserved); the exact
  commit step's refusals.
- `tests/test_oms_graph_workspace.py`: the fingerprint's relations (clean,
  modified, staged blobs with unchanged bytes, untracked, ignored, `.oms/`,
  deleted, renamed) and its fail-closed reasons.
- `tests/graph-smoke.sh`: unittest discovery of `test_oms_graph_*.py`, CLI
  smoke through `scripts/oms`, dogfood build of this repository from a
  `git archive` copy (revision stable across two builds; `--include`/
  `--exclude` honored), MCP tool calls, child-policy refusals, auto-build on
  a graph-less fixture (summary on stderr, stdout clean), `--no-refresh` and
  `OMS_GRAPH_AUTOBUILD=0`, the `--max-files` bound, and the session-start
  hook's three graph lines.
  Registered in `scripts/check.sh` as `stage graph`.
- Every test uses a temporary repository; nothing writes into this
  checkout's `.oms`.

## Work split

| worker | files (disjoint) |
|---|---|
| W1 exec-IR | `oms_graph/{errors,predicates,spec,validate,facts,route,events}.py`, `config/graphs/{coding-change,goal-drive}.json`, `tests/test_oms_graph_exec.py` |
| W2 project | `oms_graph/project/{model,extract,cache,build,query,blast,context}.py`, `oms_graph/project/parsers/*`, `tests/test_oms_graph_project.py` |
| W-G analytics | `oms_graph/project/analytics.py`, `oms_graph/render.py`, `tests/fixtures/graph-routes/*.json`, `tests/test_oms_graph_analytics.py` |
| W3 integration | `oms_graph/{cli,child_policy,scheduler,runner,shadow}.py`, `oms_graph/adapters/plan.py`, `scripts/graph.sh`, `scripts/lib/oms_graph_core.py`, `scripts/oms-mcp-server.py`, `scripts/check.sh`, `tests/graph-smoke.sh`, `tests/test_oms_graph_integration.py`, `tests/state-surfaces-smoke.sh`, docs |
| Runtime v2 | `oms_graph/{binding,workspace,commit}.py`, binding-aware `spec/validate/events/route/scheduler/runner`, concrete-only `adapters/plan.py`, `plan-run --context-pack` + `peer-delegate --context-pack` (`oms_runtime/context_pack.py`), `tests/test_oms_graph_runtime.py`, `tests/test_oms_graph_workspace.py` |

## Non-goals (v1)

LangGraph or any framework dependency; Neo4j or a required database; a
required daemon; embeddings or vector stores; model-decided routing;
removing `agent-plan` or `plan-run`; bypassing receipts; unbounded loops;
runtime self-modifying graphs; unconditional parallelism; a web frontend;
tree-sitter in core (the parser registry is the seam for an optional adapter
later); semantic enrichment as a correctness requirement (Phase 9 is an
optional `summary`/`crux`/`concept` pass through existing provider routing).
