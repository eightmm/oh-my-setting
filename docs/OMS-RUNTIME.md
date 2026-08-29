# OMS Typed Runtime Core

This change adds a typed, standard-library-only semantic core **above** the
existing hardened OMS execution plane. It deliberately does not rewrite
`peer-delegate`, `patch-admit`, `patch-land`, plan leases, executor souls,
approval consumption, commit intents, or Draft PR intents.

The core solves a different problem: OMS already has strong primitives, but the
meaning of a task, its evidence, its context, its optional capabilities, and its
recovery state is distributed across several files and commands. The new core
projects those sources into typed read models and small front doors while the
existing transaction writers remain authoritative.

## Boundary

```text
agent skill / MCP / intent router
                |
                v
+----------------------------------------------+
| OMS runtime core (new, read/projection first) |
|                                               |
| TaskEnvelope | EvidenceCoverage | ContextPlan |
| profiles | failure taxonomy | portable state  |
| execution receipts | experiments | telemetry  |
+-----------------------+-----------------------+
                        |
                        v
+----------------------------------------------+
| Existing hardened execution plane             |
|                                               |
| peer-delegate -> patch-admit -> patch-land     |
| plan leases | executor souls | approvals       |
| goal-drive | commit intent | Draft PR intent   |
+-----------------------+-----------------------+
                        |
                        v
           existing .oms append-only state
```

The runtime core has no daemon and no independent authority database. Project
state on disk remains the source of truth. The Python package uses Python 3.9+
and only the standard library.

A delegated process marked with `OMS_HARNESS_CHILD=1` can use the runtime's
read/probe actions, including install planning, but mutation and execution fail
closed with exit 2. In particular, backend execution, state/output writers,
capability installation, release application, and the direct update lifecycle
remain parent-only. The same boundary covers public auto-update wiring/apply,
doctor repair, uninstall, provider-permission apply/remove, and cleanup apply;
observation-only counterparts remain available: auto-update status/attention,
explicit non-refresh doctor reports (`--tool-lock`, `--surfaces`, `--contract`,
or `--no-model-doctor`), cached `models` output, provider-permission
check/print, and uninstall/cleanup dry-runs. Model-doctor and `models --refresh`
remain parent-owned because they rewrite routing capability state.
This is a cooperative authority boundary carried by the harness environment,
not an operating-system sandbox against a process that deliberately clears
that marker.

## Stable entrypoint

The additive entrypoint is:

```bash
oms runtime ...
```

The underlying script is `scripts/runtime.sh`. Additional intent aliases
can compile to the same entrypoint without creating more state writers.

## Effective TaskEnvelope

```bash
oms runtime envelope show
oms runtime envelope write
oms runtime next
```

The envelope projects, without copying authority:

- `PROJECT.md`
- `.oms/task/current.md`
- `.oms/plan/tasks.json`
- the newest executor metadata
- current Git state
- unresolved failure receipts
- criterion-level evidence coverage

It returns the effective objective, merged scope, acceptance criteria, budget,
source digests, warnings, and a small set of valid next actions. `write` stores
only a derived snapshot under `.oms/runtime/`; the original files remain
authoritative. When current evidence completes the work, the projection keeps
the two cleanup authorities distinct: it recommends `agent-task close` only
for an active task packet, and the read-only `agent-plan retire --check` when
an all-done plan is the remaining active record.

Plan execution recommendations come from `agent-plan status --json.actionable`,
not from the stored `ready` label alone. For a reviewed plan, the same snapshot
also exposes `plan.contract`. When its PROJECT digest/state is unsatisfied,
runtime and inbox replace `execute_ready_task` with the read-only
`inspect_plan_contract`; already-claimed/review/landing work keeps its normal
continuation authority.

Acceptance criteria can carry explicit stable IDs:

```markdown
- [id:api-compatible] Existing public APIs remain compatible.
- [criterion:equivariance] Rotation error remains below 1e-5.
```

Criteria without explicit IDs receive deterministic content-derived IDs.

## EvidenceCoverage

```bash
oms runtime evidence show
oms runtime evidence unbound
oms runtime evidence bind \
  --criterion api-compatible \
  --ref evt_012345 \
  --status verified \
  --depends src/api.py \
  --depends tests/test_api.py
```

The projection reads existing artifact, plan-progress, lifecycle, and task
verification receipts. A binding is append-only and can reference only an
existing receipt. Dependency file digests and the verified Git head turn old
passes into `stale` evidence after relevant changes.

Schema-3 plans carry an immutable `plan_id`. Automatic task admissions,
explicit `covers`, and bindings for a plan task count only when that lineage
matches; reusing `t1` in a replacement plan cannot inherit the old `t1` pass.
Legacy plans stay byte-identical on reads and receive an ID once, under the
plan lock, immediately before a parent writes plan-scoped evidence. Project
criteria and plan-level acceptance keep their existing reusable contracts.
Plan-bound delegate and landing rows carry the same frozen ID; landing intents
retain it across recovery, even if a replacement plan reuses the task name.
Missing, malformed, or mismatched lineage yields `missing`, never a vacuous
`verified` result.

Criterion statuses are:

```text
verified
failed
inconclusive
skipped_with_reason
stale
missing
```

Completion is evidence coverage, not model self-confidence.

## ContextManifest

```bash
oms runtime context \
  --target scripts/example.py \
  --require tests/test_example.py \
  --max-bytes 65536
```

The context planner starts from bounded layers and, for Python targets, adds
only direct imports, related tests, and bounded symbol callers. Every selected
source records:

- path and content digest
- inclusion reason and priority
- bytes before and after selection
- truncation policy

Missing required sources become explicit context debt. Sensitive-looking,
non-UTF-8, external, or over-budget sources are omitted with reasons. The
bundle and manifest are stored under `.oms/runtime/context/`.

This is intentionally not a mandatory compiler for every interactive turn.
It is suited to delegated implementation and review boundaries where context
reproducibility matters.

## Optional capability profiles

Notion, GitHub, multiple providers, research tooling, HPC commands, and
container support are independent capabilities rather than hard dependencies.

```bash
oms runtime profile list
oms runtime profile check core
oms runtime profile check core council github research
oms runtime profile apply core research
oms runtime profile install-plan core github --primary-provider codex
```

Profiles:

| Profile | Contract |
|---|---|
| `core` | Bash, Git, Python, and any one registered built-in provider transport |
| `council` | Inherits `core`; at least two registered built-in provider transports |
| `github` | `gh` |
| `notion` | `ntn`, optional Work Journal mirror |
| `research` | `uv`; GPU inspection remains optional |
| `hpc` | Slurm submission/accounting commands |
| `container` | Docker or Podman |
| `remote` | explicit `OMS_REMOTE_ADAPTER` |
| `full` | compatibility profile for the former all-tools install |

The agent-side installer reuses the existing locked tool installer:

```bash
scripts/install-profile.sh --apply \
  --profile core --profile research \
  --primary-provider codex
```

It selects functions from `install-tools.sh`; it does not add a second download
or integrity implementation. A user-local capability receipt is written beside
the normal install receipt. Missing optional adapters disable only their
capability.

## Execution backends

`oms runtime backend` is the canonical readiness and execution engine. The
legacy `oms execution-profile` command keeps its established report schema but
uses this same resolver. `OMS_CONTAINER_ENGINE` is the preferred explicit
container executable; `OMS_DOCKER_BIN` remains a compatibility override. With
neither set, Docker is preferred and Podman is the fallback.

```bash
oms runtime backend describe trusted-local
oms runtime backend check isolated --image oms-runtime:locked
oms runtime backend run trusted-local -- python3 -m unittest
oms runtime backend run isolated \
  --image oms-runtime:locked \
  --worktree /path/to/delegate-worktree \
  --memory-mb 8192 --cpus 4 --pids-limit 512 \
  -- python3 -m unittest
oms runtime backend run remote \
  --adapter /path/to/adapter \
  -- python3 -m unittest
```

### `trusted-local`

This is bounded process supervision, not a sandbox. It enforces a wall clock
and process-group termination where supported, but it cannot prove what the
process read or contain writes outside the repository.

### `isolated`

The container backend applies:

- read-only container root
- read-only repository mount
- one explicit writable delegated worktree mount
- network disabled by default
- no host credential or HOME mount
- dropped Linux capabilities
- `no-new-privileges`
- PID, memory, CPU, and tmpfs limits
- bounded log tail and content-free receipt

The image, container daemon, and host kernel remain operator trust boundaries.

### `remote`

The adapter receives one JSON request on stdin. It owns authentication,
transport, isolation, cancellation, and cleanup. A response must bind the exact
`operation_id` and set boolean `accepted`. An accepted response must also carry
an integer `exit` in `0..255` and boolean attestation fields
`transport_authenticated`, `filesystem_isolated`, and `cleanup_confirmed`.
Missing, false, malformed, or operation-mismatched isolation attestations fail
closed with a runtime error. OMS still records these values as adapter claims—not
locally enforced facts. See `docs/examples/remote-adapter.py`.

Every backend receipt separates:

```text
declared capabilities
enforced capabilities
unknown capabilities
```

A policy that is not actually enforced is never reported as a security
property.

## Migrating an existing install to capability profiles

No migration is forced. An install with no capability receipt keeps the
legacy full-tool update path unchanged. Opting in is one explicit apply —
`oms install-profile --apply --profile core --profile github` (choose your
own set) — which writes the private receipt; from then on `oms update`
refreshes exactly that selection. Rolling back is deleting the receipt
(`~/.config/oh-my-setting/capabilities.json`), which restores legacy
full-tool behavior on the next update. Uninstall removes the receipt with
the install it belongs to.

## Portable continuity capsule

```bash
oms runtime capsule export
oms runtime capsule verify path/to/capsule.json
oms runtime capsule import path/to/capsule.json
oms runtime capsule diff left.json right.json
```

A capsule carries only sanitized continuity:

- objective and scope
- criterion IDs, statuses, and evidence references
- blockers and warnings
- next action IDs
- content digests

It does **not** carry approvals, leases, commands, prompts, transcripts, raw
logs, patches, credentials, environment variables, machine paths, or remote
publication authority. Import is advisory and cannot modify task, plan,
approval, or Git state.

The capsule and `session-handoff` split one responsibility two ways:
`session-handoff` preserves **provider session continuity** — what one CLI's
session was doing, for the next session of that CLI on the same machine. The
capsule preserves **machine-independent project continuity** — sanitized
objective/criteria/evidence state that survives moving to another machine
entirely. An imported capsule is a reference candidate for the next session,
used only after the current Git state and local evidence are re-verified.

## Failure taxonomy

```bash
oms runtime failure catalog
oms runtime failure classify "verification failed"
```

Canonical codes bind failures to deterministic recovery metadata, including
whether retry, provider fallback, or context escalation is valid. Examples:

```text
provider_capacity       -> one explicit fallback
provider_timeout        -> resize task or budget
context_too_large       -> reduce context
context_insufficient    -> escalate context
contract_invalid        -> repair before provider call
verifier_mutated        -> reject patch
verifier_failed         -> analyze failure
scope_violation         -> reject patch
authority_mismatch      -> verify state
state_conflict           -> refresh and reconcile
remote_outcome_unknown  -> never replay the side effect
budget_exhausted        -> preserve partial state
```

## Stable and edge update channels

```bash
oms runtime release status
oms runtime release resolve stable
oms runtime release apply stable
oms runtime release promote \
  --commit <full-commit> \
  --version 0.5.0 \
  --expected-manifest-digest <reviewed-digest>
```

`config/update-channels.json` distinguishes a pinned reviewed `stable` commit
from a clean-fast-forward `edge` channel. The initial stable pin deliberately
remains the last reviewed v0.4.0 base until a maintainer promotes a later full
CI-green commit with an exact manifest digest. Applying a channel delegates to the
existing transactional `scripts/update.sh`. Promotion uses an exact manifest
CAS and never silently auto-applies stable changes.

## ExperimentContract v2

```bash
oms runtime experiment template --output experiment.json
oms runtime experiment validate experiment.json
oms runtime experiment register experiment.json --id local-channel-ablation
oms runtime experiment run \
  --id local-channel-ablation --arm baseline --seed 0 \
  --metrics metrics-baseline-0.json \
  -- python3 train.py --seed 0 --metrics metrics-baseline-0.json
oms runtime experiment summarize --id local-channel-ablation
oms runtime experiment invariants experiment.json
```

A contract fixes:

- falsifiable question, hypothesis, and prediction
- baseline and one intended independent change
- controlled variables
- exact seeds
- primary and secondary metric directions
- minimum primary improvement
- no-regression tolerances
- invariant commands

A run refuses a pre-existing metrics file by default so stale output cannot be
silently attributed to a new command. `--allow-existing-metrics` is explicit and
still requires the file fingerprint to change. Summaries are `supported`,
`not_supported`, or `inconclusive`; missing seeds, no-regression metrics, or
required invariant results keep the verdict inconclusive.

## Effectiveness telemetry

```bash
oms runtime benchmark show
oms runtime benchmark snapshot
oms runtime benchmark compare before.json after.json
```

The snapshot uses existing content-free receipts to report evidence coverage,
risk, success rate, durations, token/cost fields when present, context bytes,
skill-evaluation trigger errors and baseline/treatment task-pass deltas, and a
useful-work efficiency denominator. `oms skill-forge eval --record` is the
only writer for those skill metrics; it stores aggregate outcomes and command
digests, not prompts or output. The snapshot explicitly marks metrics that
cannot be inferred mechanically, such as escaped defects, human corrections,
false refusals, reverted lines, and duplicate work.

## Optional interoperability adapters

The stdio MCP server keeps its established tools and operations. MCP Tasks is
a feature-flagged projection over the existing peer-operation directory, not a
runtime task authority. It is advertised only for protocol `2026-07-28`, and
only an individual request declaring `io.modelcontextprotocol/tasks` receives
a task handle. There is intentionally no unscoped task listing.

The Codex app-server adapter is likewise explicit and read-only. It creates one
ephemeral thread with `approvalPolicy=never`, read-only sandboxing, and network
disabled, then accepts only text deltas and a successful completion. Approval
or input requests are a refusal, and an adapter failure does not retry through
the CLI transport. `trusted-local`, isolated, remote, provider write workers,
and patch admission retain their existing engines and authority.

The A2A Agent Card and loopback bridge expose only state projections. They do
not turn runtime envelopes into remote mutation requests, create A2A tasks, or
start a listener during install/update. See the harness interoperability
reference for the exact opt-ins and wire boundaries.

Model routing should remain simple until this evidence exists. The core does
not turn provider names into an unvalidated learned scheduler.

## Migration strategy

1. Add the runtime core and keep every existing writer unchanged.
2. Route `state`, `inbox`, and `state-verify` through shadow projections and
   compare normalized output.
3. Centralize common schema, hashing, atomic write, and failure vocabulary.
4. Add criterion-linked evidence and context manifests to delegated calls.
5. Installation now defaults to `core`; `full` remains the explicit
   compatibility profile, and receipt-less existing installs keep their legacy
   full-tool update path until migrated.
6. The runtime provides actual trusted-local, container, and typed remote
   backends; provider call sites can adopt them incrementally without replacing
   the established patch transaction.
7. Move simple state reducers to the core only after differential and crash
   tests pass.
8. Leave patch admission, landing, commit publication, and Draft PR intent as
   the final migration candidates.

No standing daemon, new authoritative database, micro-DAG for every tool call,
or always-on multi-agent swarm is introduced.

## Verification

The additive implementation includes:

- Python 3.9 grammar checks
- unit tests for projection, evidence, context, capsules, profiles, release,
  failure classification, execution, experiments, and telemetry
- adversarial symlink, secret, stale-metrics, unknown-evidence, CAS, timeout,
  and optional-profile tests
- shell smoke tests for the stable entrypoint and capability installer

Run the focused additive gate with:

```bash
bash tests/runtime-core-smoke.sh
bash tests/install-profile-smoke.sh
```
