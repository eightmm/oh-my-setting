# Read-only workflow views

These are additive views over the canonical TaskEnvelope, not a new task store,
review engine, verifier, or permission system. Existing default JSON output is
unchanged unless the new flag is requested.

```bash
oms runtime evidence show --matrix --limit 20 --pretty
oms runtime next --explain --task TASK_ID --limit 20 --pretty
```

The evidence matrix shows explicit criterion/plan-task or active-task links and
retained evidence references. It never assigns a project criterion by matching
its title to a task. Missing lineage is `not-recorded`; contradictory lineage is
`invalid-...`. Evidence status is copied from the canonical projection, not
recomputed from task state. No receipt, `covers` assignment, proof or approval is
written. Project requirement assignment and end-to-end landing provenance remain
separate work; this read view does not claim to implement those writers.

The next-action explanation uses the canonical actionable set. It reports direct
unresolved/missing/cancelled dependencies, cycles, contract blockers, active
attempts and parent review/landing states. A stored `done` dependency is not new
proof; the underlying canonical status/acceptance checks remain authoritative.
`parent` means the existing controller, not necessarily the human. Action hints
are for the whole plan even when `--task` filters the task rows. Commands are
shown as data and are never executed. A missing reason stays unknown.

`--task` requires `--explain`. Limits are 1–100; each list carries total, items and
omitted counts. Matrix evidence inherits the canonical retained-history window
(up to 20 items per criterion in the current collector). Empty criteria are not
successful completion. Output text is bounded and terminal controls neutralized;
metadata is still private, not automatically safe to publish.

Tests are discovered by the existing `tests/runtime-core-smoke.sh` lane through
`test_oms_runtime_*.py`; no new CI lane is added. The new module fixtures cover
lineage, stale/failed data, limits, cycles, input immutability and lack of I/O.
The CLI integration test must also run in the full checkout. A green module-only
run is not a full runtime/provider/TUI validation. Revert the scoped commit to
remove the views; no stored data needs migration.

## Return Brief and handoff comparison

```bash
oms runtime return-brief saved-envelope.json --pretty
oms runtime return-brief before.json --against after.json --limit 20 --pretty
```

The baseline is an explicit schema-2 snapshot produced by the existing
`envelope write` command. With no `--against`, only the second endpoint is the
canonical live envelope. There is no implicit latest-file discovery, persistent
bookmark, approval replay or task mutation. Canonical live collection may refresh
its existing disposable evidence cache; no new authority store is added here.

The report separates changed task states, changed criterion definitions/statuses,
contract-field changes and failure appearances/disappearances. A missing failure
is `no_longer_present`, never automatically `resolved`: retention and TTL can
remove records. Endpoint snapshots cannot establish complete intermediate
history. A plan rollover does not reuse task IDs as if they were the same tasks.
Matching recorded plan IDs are not repository identity/authenticity proofs.
Reverse, equal and invalid timestamps are labelled, not treated as a chronological
replay. No old `verified` status grants current acceptance.

Reads are explicit, repo-local, own-UID, single-link regular files, using bounded
no-follow descriptor traversal. Oversize JSON, duplicate keys, non-finite values,
excessive structure, symlinks/hardlinks/FIFOs and detected concurrent changes are
refused. Inputs are untrusted metadata. The report never follows paths or runs
commands found inside them. This implements snapshot comparison, not the full
TUI Return Brief, automatic last-seen cursor or native session resume feature.
