# Collaboration dashboard

`oms dashboard` is a read-only terminal view of what OMS knows about a
repository's collaboration: goal and scope, task/plan and attempt states, which
provider and model ran each recorded operation, acceptance evidence, and the
next decision from the inbox.

```bash
oms dashboard --repo PATH                       # one snapshot
oms dashboard --repo PATH --json                # schema-1 projection
oms dashboard --repo PATH --watch               # refresh every 5 s until Ctrl-C
oms dashboard --repo PATH --watch --interval 10 --count 3
```

`--interval` takes whole seconds (minimum 2, default 5). `--count` stops after
N refreshes. On a terminal each refresh clears the screen after the new view is
rendered. When stdout is a pipe or file, snapshots are appended and separated
by a `----` line. Nothing is erased. `--json` emits one snapshot and cannot be
combined with `--watch`. Width follows `COLUMNS`, then `tput cols` on a
terminal, otherwise 120 columns.
Terminal watch mode also follows `LINES` or `tput lines` (default 24), keeping
summary headings, model routes, failures and the next decision visible while
omitting older details. The footer says how many lines are hidden; a single
snapshot or JSON retains the bounded detailed view.

Exit status: `0` when every source was collected, `1` when the view is degraded,
`2` for usage errors, and `130` on Ctrl-C.

## What it reads

Each refresh runs three existing read-only queries:

| Source | Query | Used for |
| --- | --- | --- |
| state | `oms state --json` | goal, scope, task, plan, attempt counts, delegations, acceptance criteria, inbox attention |
| artifacts | `oms artifact-index --json list 8` | recent operations, provider/model attribution, review rows |
| attempts | `oms agent-events list --limit 8 --json` | recent attempts with reported usage |

The attention section comes from the same `project_inbox` projection as
`oms inbox`, applied to the state snapshot. The dashboard does not write `.oms`
or call a model. It does not refresh CI, touch the network, or keep a store of
its own. The lifecycle stream is read twice per refresh: once inside
`oms state` and once for per-attempt usage, which the compact state omits.

## Reading the view

- **Coverage.** Only OMS-managed records are shown. A native provider subagent
  that never wrote OMS state does not appear. The dashboard does not infer a
  supervising agent or a plan/execution/integration stage.
- **Unknown is not zero.** A missing owner, model, token count, or cost shows
  as `unknown` in text and `null` in JSON. Attempt usage counts only when the
  provider reported it.
- **Model attribution.** `model=` is the selected route model recorded at
  dispatch. `served=` is the model the provider reported running, or
  `unreported`. `route=` is the recorded route class (`explicit`,
  `role-default`, `provider-default`, `fast`, `balanced`, `deep`). A fallback
  shows its recorded reason, and any reason outside the known set shows as
  `other`.
- **Completion is not acceptance.** A `done` attempt only means that the
  worker stopped. Acceptance is the evidence status of each criterion:
  `verified`, `failed`, `missing`, `stale`, `inconclusive`, or
  `skipped_with_reason`. Failed and stale criteria are listed first.
- **Reviews.** `review gate:` counts recorded `review-outcome` rows
  (exit 0 = pass; missing exit = unknown). These are historical counts over
  the listed window, not proof of a review for the current tree. If none were recorded, it says
  `none recorded in window (not a pass)`. Seat answers (`kind=review`) are
  counted separately, because a seat answering is not a verdict.
- **Degraded collection.** A source that fails or returns an unsupported
  contract adds a `COLLECTION FAILED <source>: ...` line, and its section
  reads `UNAVAILABLE`. Only the collector exit code is displayed: stderr may
  quote private record contents. A source larger than 4 MiB is unavailable
  rather than silently truncated into a healthy-looking snapshot.

Every label is treated as untrusted. Control characters, including terminal
escapes, BEL, and C1 codes, are replaced with `?`, and so are format characters
such as bidi overrides and zero-width characters. Labels are truncated, each
list shows at most 8 rows plus an omitted count, and each line fits the
terminal width. The view never includes artifact task goals, approval
summaries, patch or artifact paths, prompts, or transcripts.

## Sample

Illustrative output from the operator test fixture:

```text
OMS dashboard  repo=demo  main@b6c3a2aa8c39  snapshot=2026-10-04T00:51:30Z
coverage: OMS-managed records only; native provider subagents are not observed
goal: operator fixture (plan)
scope: unbounded (none recorded)
task: none active
plan: 2 task(s)  ready=1 review=1  actionable=ready_task
attempts (OMS lifecycle): 1 total, 0 active  cancelled=1
  att_one cancelled codex/agent-supervisor task=task_one tokens=12 cost_microusd=7 2026-08-01T00:00:03Z
delegation: live codex started=2026-08-01T00:00:00Z live
operations (artifact index, newest first):
  codex delegate model=unknown route=unrecorded exit=0 success served=unreported tokens=unknown at=2026-08-01T00:00:10Z
  claude review model=unknown route=unrecorded exit=1 unresolved served=unreported tokens=unknown at=2026-08-01T00:00:07Z
  codex call model=gpt-test route=explicit fallback=other exit=0 success served=gpt-served tokens=120 at=2026-08-01T00:00:05Z
review gate: none recorded in window (not a pass)  seat answers=1
acceptance: 0/2 verified  missing=2  (worker completion is not acceptance)
  missing plan-task-ready_task [plan-task] Plan task ready_task is admitted.
  missing plan-task-review_task [plan-task] Plan task review_task is admitted.
attention: 2 item(s)
  P1 unresolved-artifacts: 1 unresolved artifact outcome(s)
     next: oms artifact-index --repo . unresolved; oms artifact-index --repo . resolve-recovered --dry-run
  P3 runtime-evidence-missing: 2 acceptance criterion/criteria lack current evidence
     next: oms runtime evidence show
next decision: initialize_project (repo_write): oms init
```

## JSON schema 1

`--json` prints one object. Its fields are additive, and removing or retyping
one requires a schema bump.

| Field | Type | Meaning |
| --- | --- | --- |
| `schema`, `kind` | `1`, `"oms-dashboard"` | contract identity |
| `generated_at` | UTC timestamp | snapshot time |
| `repo` | `{name, branch, head}` | directory name, branch, 12-character HEAD, or `null` |
| `coverage` | string | the OMS-managed-records-only statement |
| `collection` | `{ok, sources: {state, artifacts, attempts}}` | each source is `"ok"` or a sanitized error |
| `goal` | `{text, source}` or `null` | recorded objective (`task`, `plan`, or `project`) |
| `scope` | `{allowed[], forbidden[], source}` or `null` | `source` is `unbounded` when nothing is recorded |
| `task` | object or `null` | `present, healthy, task_id, status, verification, stale, next` |
| `plan` | object or `null` | `present, healthy, task_count, by_state, actionable[], stale_claims, stale_reviews, contract_blocker` |
| `attempts` | object | `available, total, active, by_state, recent[], note`, plus `error` when unavailable |
| `attempts.recent[]` | object | `attempt_id, state, provider, tool, task_id, reason_code, updated_at, tokens, cost_microusd` |
| `delegations` | array or `null` | `id, provider, role, live, started_at` |
| `operations` | object | `available, recent[], omitted`, plus `error` when unavailable |
| `operations.recent[]` | object | `event_id, ts, kind, provider, task_id, attempt_id, exit, status, verify_exit, route_class, requested_model, selected_model, served_model, model_attribution, fallback_used, fallback_reason, tokens, cost_usd` |
| `reviews` | object or `null` | `outcomes, passed, failed, unknown, seat_answers` over the listed window |
| `acceptance` | object or `null` | `available, total, counts, complete, criteria[] {id, status, source, text}, omitted, note` |
| `attention` | object or `null` | `actionable, items[] {priority, code, summary, command}, omitted, next[] {id, authority, command}` |

When the state source is unavailable, every section derived from state is
`null`. A `null` means "not known", never "none".
