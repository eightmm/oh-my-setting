# Runtime Core and Minimal Execution

Use the runtime core as a typed read/projection layer over existing OMS state.
It does not replace the hardened delegation, admission, landing, approval, or
publication boundaries.

## Orient with one projection

```bash
oms runtime envelope show
oms runtime next
oms runtime evidence show
```

The TaskEnvelope reconciles `PROJECT.md`, the active task, plan, executor,
failures, and evidence. Treat it as a derived view. Never edit its snapshot to
change authority.

## Compile context only at a boundary that benefits from it

```bash
oms runtime context --target PATH --require TEST --max-bytes 65536
```

Use this before delegated implementation or review when reproducibility and
bounded prompt size matter. A nonzero context-debt result means retrieve the
missing required source before patching. Do not force it onto every interactive
parent turn.

For a write worker, prefer `oms peer-delegate --context-manifest`: it compiles
against the detached `HEAD` the worker receives, embeds that exact bundle in
initial and repair prompts, and records manifest/bundle digests on the artifact
row. A requested compilation or outbound-content check that fails blocks the
call instead of falling back to a prompt without the bundle. A valid partial
bundle remains usable: nonzero debt is attached to the prompt and artifact row,
and the worker must inspect the missing or truncated paths before editing them.

## Complete by evidence coverage

Use stable criterion IDs in project/task acceptance lists. Bind only an existing
artifact or fresh task-verification receipt:

```bash
oms runtime evidence bind \
  --criterion CRITERION_ID \
  --ref EVIDENCE_ID \
  --status verified \
  --depends path/to/source \
  --depends path/to/test
```

A changed dependency makes the binding stale. Provider confidence is not a
completion signal.

Task-packet criteria are scoped to their `active_task_id`. A successor packet
that reuses a criterion ID does not inherit the predecessor's binding or
`covers` receipt; project criteria remain reusable, and a fresh successor task
gate still counts.

## Choose the minimum capability profile

```bash
oms runtime profile check core
oms runtime profile install-plan core research --primary-provider codex
oms runtime profile install core research --primary-provider codex
```

Notion, GitHub, research tools, HPC commands, container isolation, and
remote execution are optional profiles. `council` inherits `core` and adds a
second distinct provider seat. The selective installer reuses the existing
locked download and transaction functions; it does not create another
supply-chain path. Do not widen the installation because a capability exists;
select it only when the current project needs it.

## Choose an honest execution boundary

- `trusted-local`: supervised host process; not a sandbox.
- `isolated`: read-only repo plus one writable worktree, no network by default,
  no host credentials, dropped capabilities, resource limits.
- `remote`: external adapter; record its claims as attestations, not local facts.

Use the existing `peer-delegate -> patch-admit -> patch-land` flow for code
mutation. A runtime backend receipt is execution evidence, not landing
authority.

## Move between machines with a capsule, never raw `.oms`

```bash
oms runtime capsule export
oms runtime capsule import capsule.json
```

The capsule is advisory and sanitized. It never carries a lease, approval,
command, raw transcript/log, machine path, credential, or publication right.
After import, use the bounded `continuity.latest_import` view in `oms state`,
`oms inbox`, or the resume hint. `current`, `head-diverged`, and
`state-diverged` compare source provenance with local canonical state; none of
those states restores task, plan, evidence, approval, or landing authority. A
damaged local import is isolated as `invalid` instead of disabling canonical
runtime state.

## Research work

Use ExperimentContract v2 when a run tests a claim. Pre-register seeds, primary
and no-regression metrics, controlled variables, and invariant commands. A
pre-existing metrics file is rejected by default to prevent stale-result reuse.
Incomplete seeds or invariant evidence remain inconclusive.

## Failure routing

Classify once, then follow the canonical recovery. In particular:

- capacity may use one explicit fallback;
- policy/auth failures do not route around the provider;
- context-insufficient may expand context;
- verifier mutation and scope violation reject the patch;
- unknown remote side effects are never replayed;
- exhausted budgets preserve partial evidence and stop.
