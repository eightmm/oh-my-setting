# Work inspection and return briefs

These commands read the existing canonical TaskEnvelope. They do not introduce
another plan, scheduler, approval mechanism, or task database.

```bash
oms runtime evidence matrix --format text
oms runtime evidence matrix --task TASK --limit 20
oms runtime next --explain --format text
oms runtime next --explain --task TASK
oms runtime brief --format text
oms runtime brief --before .oms/runtime/effective-contract.json --format text
```

The default is JSON. `--pretty` formats JSON and cannot be combined with
`--format text`. Each list reports total, displayed items and omitted count.
`--limit` is 1–100 (default 20), including nested evidence and dependency lists.
Unmodified `runtime next` and `evidence show` retain their previous outputs.

## Evidence matrix

The matrix joins a criterion to a plan task only when both its recorded task ID
and plan ID match. An active-task criterion uses the active task's own ID instead.
Project criteria without a recorded relation remain `unrecorded`: matching
names, paths, or recent timestamps are not a relation. The matrix preserves the
canonical criterion status and bounded evidence references, kind, status,
signal, source, support and time. It does not manufacture a verifier command or
landed commit. Tasks lacking recorded criterion links are reported separately.

This implements a read view of current links, not proposal-time `covers`
assignment, automatic coverage inference, or a full requirement-to-commit chain.

## Next-action explanations

`next --explain` adds task-local reasons to the canonical suggested actions.
It distinguishes unresolved/missing dependencies, plan-contract blocking,
active attempts, review, landing, and cancelled-not-verified state. Cancelled
prerequisites are not treated as done. A contradictory actionable flag is
reported rather than silently resolved. Unknown reasons stay unreported.

Action authority and command text come from the canonical projection. They are
not executed. `next_actor` identifies the parent controller; whether a human's
approval is needed is deliberately null because this snapshot cannot prove it.
Every actual mutation must still use the existing authority front door. Cycles
remain bounded direct-dependency explanations, not a new scheduler or inferred
root-cause engine. `--task` filters the task view; canonical actions remain
plan-wide. Task, format or non-default limit options require `--explain`.

## Return brief and handoff freshness

Without `--before`, a brief displays the current matrix and inbox and explicitly
states that no comparison baseline exists. With a baseline, it compares recorded
head, source digests, task states, verifier digests, criterion states and evidence
references. It shows additions/removals separately; removal never means success.

Use an explicit file written by `runtime envelope write`, or a saved bare output
of `runtime envelope show`. The write acknowledgement wrapper is accepted but
its `written` field is never followed. Reading uses the existing bounded,
descriptor-relative JSON reader: the file must be inside the selected repository,
owned, regular, single-link, no symlink path components, at most 4 MiB. No native
transcript, provider configuration or external URL is read.

Plan generation or active-task changes, unavailable identity and a baseline
newer than the current snapshot suppress state transitions. Source changes are
still reported. Equal IDs/digests describe recorded scope; they do not authenticate
cross-machine ownership or approve reuse. A saved envelope is not a receipt.

These are two-snapshot differences, not replay of all intermediate events.
`history_complete` is always false. Repeated states and failures occurring between
snapshots cannot be reconstructed. No automatic bookmark, cursor store, notifier,
background process or extra model call is created.

## Boundaries and exit status

Schema-2 envelopes are validated, including duplicate IDs, supported criterion
statuses, bounds, identity fields and timestamp timezone. Control characters are
neutralized in displayed strings; ordinary Unicode is retained. This is not a
secret scrubber: the same private metadata already shown by the runtime remains
private. Do not publish reports without reviewing them.

Exit 0 means a successful read query, not completion, acceptance or a clean
project. Invalid input, missing selected tasks, invalid limits and unsupported
schemas return 2. Child runtimes may use these read queries; write operations
remain denied. Canonical readers may maintain their existing regenerable caches;
these features add no store and never write plan, task, receipt or approval state.

## Tests and remaining scope

`tests/test_work_inspect.py` tests the actual pure projections and renderers.
The existing `context-core` suite invokes it with `--cli`, which additionally
uses `scripts/runtime.sh`, a private Git repository and actual envelope output.
It checks old `next` compatibility, child read access, text/JSON, invalid flags,
unsafe baseline paths, source changes and repository-file invariance.

The first implementation provides agent-facing JSON/text views. Automatic tmux
panel integration, navigation focus tests, human-only decision classification,
proposal-time coverage assignment, contract amendment and native UI measurements
remain separate work. This does not mark those larger roadmap items complete.
No provider, network, GPU, Slurm or user installation is required by the tests.
