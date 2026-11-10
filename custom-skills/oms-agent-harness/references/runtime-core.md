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

The TaskEnvelope reconciles `PROJECT.md`, the active task, plan,
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

To see what context cost or whether a saved manifest is still current, use the
read-only inspector: `oms runtime context-inspect --injections` (bytes OMS hooks
injected, `--session-hash H` for one session), `--session TRANSCRIPT` (per-tool
result bytes and repeated reads in one Claude Code JSONL transcript), and
`MANIFEST --verify-current` (compares current source hashes; not with
`--compare`). `--format text` prints a readable report. Sizes and hashes only,
not a judgment of quality.

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

## Advisory verbs: which to run

Each verb below only reads recorded state or the working tree: no model call,
and nothing it prints is approval, verification, or gate evidence. Run it when
you hold the question; none starts, retries, kills, or merges anything.

| Question | Command | Prints |
|---|---|---|
| Should I delegate this? | None: a judgment call. | Use [model-routing.md](model-routing.md) and [roles.md](roles.md). |
| Is the brief good enough? | `oms peer-delegate --to NAME --brief-file F` (add `--strict-brief` to refuse) | `warning: brief:` lines for a missing goal, scope, output form, stop condition or file pointers; briefs under 240 bytes are not linted. |
| How deep should the review be? | `oms peer-review advise [--base REF]` | `none`, `single`, `gate+security` or `council`, with reasons. |
| Is something looping? | `oms fail-ledger list --stalls` | Repeated or alternating failures and no-evidence goal-drive runs, or `stalls: none`. |
| Where did tokens go? | `oms artifact-index telemetry --by role` | Per role: calls, reported tokens, cache share, admitted patches, tokens on unadmitted ones. |
| How big are injected prompts? | `oms project-doctor --prompts` | Words and bytes per shipped prompt file, whether it loads every session, repeated sentences. |
| Is the prompt prefix cache-stable? | `bash scripts/check.sh invariants` (source checkout) | Fails with `file:line` on a timestamp, temp path or unordered listing in injected text. |
| What do always-loaded rules cost here? | `oms project-doctor --budget` | Bytes, words, lines of loaded instruction files per provider. |
| Which role has which authority? | `config/role-contracts.json` | Per role: `read_only`, `writes` scope, required `returns` lines. Linted, not enforced at runtime. |
| Is the contract's evidence there? | `oms project-doctor --contract .` | Missing referenced files and each criterion's evidence status. |
| How exposed are the provider CLIs? | `oms doctor --security` | ok/warn/info rows for bypass modes, blanket allows, sandbox switches. |
| Could this host sandbox? | `oms doctor --sandbox` | Per-mechanism availability (bwrap, namespaces, AppArmor, Landlock). |
| What does each provider support? | `oms doctor --providers` | Provider by function matrix, marked fixture or live. |
| Who is editing these paths? | `oms lease list`, `oms lease check PATH...` | Active and expired advisory leases; `check` exits 1 on a conflict. |
| How do candidates compare? | `oms candidate table --metric KEY` | Candidates by tool:metric, each number linked to a run or receipt. |
| What would CI records suggest next? | `oms ci-status suggest` | Suggested commands from recorded CI, review and finding facts; unknown stays `unknown`. |
| What is actionable in the plan? | `oms agent-plan ready` | Task ids whose dependencies are done, in claim order. |
| What do I tell the next session? | `oms session-handoff prompt` | An editable Markdown prompt drafted from OMS records. |
| Who wrote this commit? | `oms land blame-session COMMIT_OR_RANGE` | Recorded attribution per commit (ai, human, mixed, unknown, `unrecorded`). |
| Are checkpoints piling up? | `oms run checkpoints` | Per directory: file count, size, `ok:` or `warn:` against latest/best. |
| What needs attention? | `oms inbox --repo .` | Ranked items with one exact next command each. |

Read the output as a prompt to look, then verify in the source of record.
