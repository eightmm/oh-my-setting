# Native lifecycle implementation draft

This standalone crate is a partial migration. It is not ready for lifecycle
cutover. Compilation establishes neither behavioral parity nor host deployment
readiness. The embedding dispatcher and admission remain the owner's work.

Public entrypoints are `owns`, `run_with_source`, and `run_with_services`.
The latter takes the same command, argument, cwd, source, and cancellation
inputs plus `&dyn Services`. `owns` is an IO-free family selector, including
malformed invocations. Execution returns `(exit, stdout, stderr)` without
changing process-global cwd, environment, or signal handlers.

Path-valued options retain `OsString` bytes, including attached option values.
Option names, actions, configuration command strings, and textual registries
still require UTF-8. JSON serialization can also refuse a non-UTF8 path; this
crate does not silently replace those bytes.

## Implemented paths

- `link`, `unlink`, and skill-source validation retain schema-1/2 receipt
  reading, schema-2 receipt production, physical ownership comparisons,
  enabled skills, copy markers, and displaced-user backups. The native payload
  proof now stays alive and is rechecked during target publication. `unlink`
  uses the receipt's recorded native frontdoor for ownership comparisons.
- `install` can consume a validated staged payload or build a missing payload
  with locked, offline Cargo 1.99.1. It stages the complete cache directory
  atomically, installs native artifacts and a hash-checking POSIX frontdoor,
  converges supported recorded components, and publishes the receipt last.
  The frontdoor invokes native private dispatch with explicit source/revision
  arguments. It requires `sha256sum` or `shasum`, with no Python activation,
  interpreter-tree installation, or legacy-launcher fallback. Invalid existing
  caches are refused. Component disable transitions withdraw owned effects.
- `uninstall --yes` withdraws supported integration effects and managed targets
  in one target transaction. Account-effect registries and unsupported provider
  removal transactions refuse before global cleanup. Checkout purge and managed
  interpreter-directory retirement remain unavailable.
- `install-claude-hooks` converges declared hooks, HUDs, attribution, and the
  evidenced model effort default. Exact managed entries from another genuine
  checkout can be adopted; user siblings survive. Unset guard configuration
  preserves existing owned `onFailure` blocks, and explicit disabling removes
  them. Enabling fail-closed guards refuses without capability negotiation.
  Account reservation, auto-compact negotiation, and native HUD execution are
  still incomplete; existing hook/HUD declarations retain their shell forms.
- `install-mcp` registers the managed native frontdoor for Claude and Codex.
  TOML is parsed with `oms-toml`; a candidate must preserve all unrelated data,
  including when marker-like text occurs inside strings. Unmanaged registration
  tables are refused. Both provider configurations validate before publication.
- `install-agy-plugin` materializes a complete, owned MCP-only plugin directory
  atomically. Foreign or modified plugin directories are refused. No lifecycle
  hook certification is inferred from a cache assertion.
- `install-codex-plugin` can converge an already registered plugin's proven
  cache and managed HUD/usage blocks. Customized values survive. The settings
  editor validates syntax and the entire semantic delta, handles displaced HUD
  markers, preserves the file mode, and creates a first-touch backup. Initial
  marketplace/plugin registration and provider-owned removal remain unavailable.
- `install-profile` / `install-tools` retain capability inheritance, availability
  checks, planning, checking, reapply/add, explicit degraded receipts, and native
  runtime requirements. Tool download/selection/upgrade remains incomplete.
- `install-autoupdate`, `uninstall-autoupdate`, and tick trigger installation
  retain journaled native scheduler files and external activation recovery.
  Removal inspects existing units as well as cron. Exact legacy tick cron
  entries can migrate under the same journal; ambiguous prefixes/markers refuse.
- `update --check` resolves pinned refs, edge, and explicit fallback-to-edge.
  Fetch and target resolution serialize through an `oms-lock` derived from the
  common Git directory. `auto-update check|status|attention|install|remove`
  preserves the legacy status-file format and can read the earlier JSON draft.
  Attention distinguishes disabled, unwired, no-run, failed, blocked,
  session-only, overdue, unknown, and ok. Failed checks record a complete failure
  outcome. Apply and rollback still refuse before checkout mutation.
- `doctor` retains core receipt/target/payload/skill/operations/memory checks,
  link repair, surfaces, tool-lock, security, residue, sandbox, and provider
  reports. TOML security/MCP inspection now uses `oms-toml`. Extension inventory
  includes Claude plugin declarations and Antigravity plugin/extension names.
  Provider `--check --table --root` validates profiles, references, implementation
  definitions, and test words without executing configuration content.
- `doctor --mcp` hashes launch metadata and sorted environment names, reports
  unpinned package launchers and non-loopback HTTP, and merges the latest pin
  baseline under its canonical lock. Unknown providers keep their pins.
  `--probe-tools NAME` selects exactly one enabled local stdio server and speaks
  initialize/initialized/tools-list without a shell. Bounds are 4 MiB, 20 pages,
  500 tools, and one 1–120 second deadline. The process group is always stopped.
  Only tool hashes/counts and cleaned names are reported; configuration values,
  schemas, descriptions, headers, and server stderr are not printed.
- Security finding fingerprints use a locked, atomic `doctor-security.json`;
  harness children observe without publishing it. Snapshot home storage uses
  native `statvfs`, with an explicit unknown result on failure.
- `tick` retains physical-path registration, native attempt reconciliation,
  memory health, and telemetry. It deduplicates repositories, defers for live or
  unproven verification markers, and preserves validated wake ledgers. Complete
  maintenance, schedules, catalog refresh, optional GC, and idle-main wake use
  injected native `Services`. Missing/invalid phase receipts produce failure,
  never a successful partial sweep. Failure classification uses the declared
  `oms-command-failure` adapter.

## Native tick service contract

`tick_maintenance` runs while the receipt mutex is held. It must use native
journal, CI, thread/task/plan, artifact, failure, and plugin-cache producers with
all their own authority checks and retirement thresholds. It must not reacquire
that receipt mutex. On success, stdout is a JSON object with exactly these fields:

- `journal_rc`, `ci_rc`, `plugin_rc`, `plan_retire_rc`, `artifact_resolve_rc`, and
  `artifact_supersede_rc`: zero or the explicit string `skipped`.
- `threads_closed`: an array of safe identifiers.
- `tasks_closed`, `plans_retired`, `artifacts_resolved`, `artifacts_superseded`,
  and `failures_retired`: unsigned counts.

A failing or malformed result cannot certify completion. Catalog refresh runs
once per invocation; its service owns the daily stamp, freshness check, and
shared lock. GC is opt-in. The schedule callback receives the original tick
argument vector, including the `schedule` action, and must validate the approved
contract and append through its canonical journal. A path or callback result is
not new approval. Schedule listing is read-only.

The wake callback owns idle-main, empty-input, schedule, dedupe, daily-budget,
and notification checks, and returns its canonical wake receipt. Current-day
ledger entries cannot disappear during sweep publication. Malformed, empty,
linked, multiply linked, or oversized existing receipts are retained. Receipt
publication is capped at 1 MiB. `run_with_source` supplies unavailable services;
the embedding host must use `run_with_services` for a complete native sweep.

## Remaining migration work and integration boundaries

The earlier `ProducerUnavailable` explanation was incorrect. On this base,
`oms-bootstrap::ReferenceProducer` composes the checked Reference service from
an authenticated `ValidatedPayload`. It does not implement staging, receipt
publication, or frontdoor lifecycle operations; those are native transactions
in this crate. It also cannot authenticate an incoming revision using the
running binary's old embedded source inventory.

The remaining blockers are explicit:

- Incoming-revision native dispatch/build handoff, update apply/rollback,
  worker/panel admission fences, doctor-gated recovery, and provider refresh.
  The parent must also wire lifecycle routing and bootstrap frontdoor support
  in the native main; this crate does not change that excluded consumer.
- Legacy lifecycle-lock adoption; checkout purge; managed interpreter-tree
  retirement; account reservation, refresh, and cleanup; Slurm snapshot/service
  connection/permission/publication components; full install profiles.
- Verified tool downloads, full/only/upgrade selection, vendor refresh, Codex
  marketplace registration/removal, Antigravity isolated surface certification,
  guard capability negotiation, and complete provider settings convergence.
- Doctor MCP probe memoization, complete security/project-policy coverage,
  model capability/live probes and strict diversity, complete copy/digest
  inventories, contract fixtures, full component repair, and exact legacy
  help/text/JSON compatibility. Existing partial schemas are not cutover proof.
- Host implementations of native tick services. No Python or shell producer is
  selected when those services are absent. `oms-context-report` is not invoked
  with unrelated lifecycle findings; its context-report schema remains distinct.

## State, concurrent calls, and interruption

| Written object | Concurrent call / interruption behavior |
| --- | --- |
| Native cache directory and its executable/checksum/manifest | Complete directory staging and one journaled rename under the receipt and journal locks. Validation precedes host publication. A second call validates the existing revision; an incomplete journal rolls back the first staging. A kill during unjournaled preparation can retain an unclaimed stage. |
| Cargo build output under `rust/target/lifecycle-build` | Cargo owns its build locks. Failed/stopped output is not an install proof and is not published as a payload. |
| Native libexec artifacts, frontdoor, managed targets/copies/markers, user backups, capability/snapshot files, install receipt | One receipt lock plus the shared journal lock. Target changes have durable undo entries; the receipt publishes last. Conflicting identities retain recovery material. A second call recovers before mutation. |
| `native-lifecycle/transaction.json`, `.oms-stage-*`, `.oms-undo-*` | Journal locking also serializes distinct receipt overrides sharing one config root. Uncommitted state rolls back; committed state finishes cleanup. Unclaimed preparations are preserved. |
| Claude hook/HUD/default settings and one-time backup; Claude MCP JSON; Codex MCP/HUD/usage configuration and first-touch backup | Same target journal and locks. User siblings and semantic values are retained. Independent provider writers do not honor these locks; complete cross-process compare-and-swap coverage remains unverified. |
| Codex cache/provenance markers; Antigravity plugin directory/ownership marker | Same journal. Complete plugin directories replace atomically; provenance writes roll back with their cache. A stopped operation restores recorded targets or retains a conflict. Codex provider registration/removal is not attempted. |
| Service/timer files and crontab (including legacy tick migration) | File changes use the target journal. External before/after state is durable before mutation. Changed crontabs refuse rollback; unavailable host tools can retain the recovery journal. |
| MCP pins and optional tool hashes | Latest baseline is read after canonical lock acquisition. Merge and publication are atomic; unknown-provider pins and unrelated tool hashes survive. Failure before publication retains the old store. |
| Security baseline | Canonical file lock and atomic replacement; only section hashes/timestamps persist. Concurrent callers compare against the latest completed baseline. |
| Tick registry | Canonical file lock, physical-path deduplication, atomic publication. A stopped writer leaves the previous complete registry. |
| Tick sweep receipt and wake ledger | Canonical receipt mutex, verification-marker admission, ledger validation, bounded atomic publication. A stopped sweep retains the previous receipt; native domain changes and notifications retain their producers' own journals/locks. |
| Git fetch refs / auto-update outcome | Common-Git-directory fetch lock spans fetch, resolution, and state publication. Git owns ref atomicity; state replacement has a separate canonical lock. A kill can leave fetched refs with the prior complete outcome, never a torn outcome. |
| Probe pipes/captures | Captures are exclusive unlinked TMPDIR files; MCP uses nonblocking pipes. Deadline, cancellation, and errors stop the owned process group. Nothing is memoized persistently. |

All source changes are confined to this crate. No existing tests were changed.
Behavior, boundary, concurrency, kill-injection, provider/network, macOS/WSL,
and Ubuntu-floor execution were not run in the implementation-first phase.
Only offline all-target compilation and formatting were checked. Admission,
behavioral verification, and the remaining implementation are still required.
