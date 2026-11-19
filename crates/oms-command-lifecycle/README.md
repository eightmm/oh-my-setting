# Native lifecycle implementation

This crate owns native lifecycle planning, validation, transactions, and provider
configuration. It preserves `run_with_source` and `run_with_services`;
the former uses `UnavailableServices`. It never dispatches a Bash or Python
lifecycle implementation as a fallback. `#![forbid(unsafe_code)]` remains enabled.

## Implemented surfaces

- `update` check/apply, `--fallback-to-edge`, `--rollback`, auto-update status,
  and `--probe-agy-surfaces`: validate refs, clean checkout, live worker/panel
  fences, and canonical ownership before mutation. Apply uses a separate update
  journal, backs up managed payloads and the receipt, invokes the incoming native
  gate, verifies its receipt/doctor, and publishes the final receipt last. Gate
  failure restores payloads and Git state if their recorded identities still
  match; concurrent changes retain recovery evidence. Preview precedes probing.
- `install`, `link`, `unlink`, and `uninstall --purge`: converge recorded
  components, manage native launchers/skills, remove only recorded provider
  account effects, retire owned interpreter trees, and optionally remove the
  checkout. Dirty purge needs `--purge-dirty`; unsafe root/ancestor paths and
  ambiguous ownership fail closed. Permission cleanup preserves unrelated grants.
- `install-codex-plugin`: inventory through the bounded Codex CLI, initial
  marketplace/plugin registration, moved-marketplace/version refresh, and full
  removal. Deleted previous checkouts do not prevent removal. Unmanaged TOML
  tables, foreign caches, modified managed caches, and unrelated config changes
  remain refusals. External snapshots cover the OMS marketplace/cache namespaces
  and actual Codex config, never the entire provider plugins directory.
  Installed hook payloads invoke `oms harness-hook` directly; legacy shell
  templates are retired in favor of a crate-owned native inventory without
  reading or installing their scripts.
  Cache provenance records the generated payload digest, so repeated installs
  converge and modified caches remain refusals.
- `install-claude-hooks`: isolated, bounded capability negotiation before global
  receipt/transaction locking; conditional `onFailure` and compact-window
  convergence; adoption/removal of legacy OMS wrapper commands even when their
  checkout was deleted. Native hook schema 12 is defined in the crate, so hook
  installation and `doctor --surfaces` need no shell installer or script files.
  Statuslines invoke `oms claude-statusline` and `oms claude-subagent-statusline`.
  Account mode requires a matching effects reservation
  (`--ownership-receipt`) and removes only exact recorded leaves/hooks.
- `install-agy-plugin --probe-surfaces`: disposable home/workspace certification
  checks hook delivery, payload, blocking, and timeout behavior. Cache identity
  uses Bash-compatible binary spelling and stat identity, including macOS path
  aliases. A binary removed during the probe produces `binary_changed`.
  `agy-surfaces.env` is evidence only for that binary; changed identity retracts
  hooks. Certified lifecycle plugins register `PreInvocation` and `Stop` via
  `oms agy-hook`, while `PreToolUse` and `PostToolUse` require
  `tool_hooks_verified=yes` or explicit `OH_MY_SETTING_AGY_HOOKS=1` (uncertified
  plugins remain MCP-only unless `OH_MY_SETTING_AGY_HOOKS=1` is set). The adapter translates provider payloads,
  bounds stdin, honors kill switches, and keeps advisory Stop calls nonblocking.
- `install-tools` / `install-profile`: catalog/profile inheritance, `--only`,
  `--full`, `--upgrade`, installed-provider-only `--providers`, capability
  checks, and Slurm snapshots. Managed Node, uv/uvx, gh, Antigravity, Claude,
  Codex, and Notion payloads use current upstream metadata, HTTPS-only downloads,
  digest/SRI verification, constrained archive extraction, and managed provenance.
  Release observations never pin the next update; an explicit user tool lock
  stays pinned. Optional npm refresh selects installed global packages and their
  current latest release without an age delay; native Pi uses its updater.
  Ad-hoc tool operations preserve capability choices, profile reapply honors the
  selected receipt, and Node precedes npm-based providers. A valid Antigravity
  ownership sidecar is retired at vendor handoff outside the core rollback.
  Existing foreign tools are preserved or refused, not silently replaced.
- `doctor --contract`, `--live-models`, `--strict-diversity`, and `--repair`:
  isolated native template/hook contract fixtures, bounded provider/model
  discovery, declared/detected effort controls, configured-default routability,
  capability `.env`/model/effort caches, model-family diversity, recorded-component convergence,
  project/security-policy baselines, managed copy/digest inventories, and MCP
  probe memoization. MCP cache keys include command, execution context, timeout,
  and binary identity; a memo contains validated tool hashes, not arbitrary
  provider output. Unknown model families cannot certify diversity.
- Existing status, sandbox, residue, pins, scheduler, and tick behavior remains
  native. Optional external adapters must actually satisfy profile requirements;
  their absence is accepted only by explicit allow-missing policy.

Downloads, metadata, archive members, provider output, and stdin are bounded.
Invalid paths, links escaping staged payloads, malformed manifests, checksum
mismatches, downgrade attempts, and ambiguous ownership fail before publication.
Provider commands are cancellable, time-bounded process groups with closed stdin
unless a protocol explicitly supplies input. Probe output is not authority.

## Host dispatcher contracts

The following are native domain boundaries, rather than shell fallbacks. The
embedding binary must use `run_with_services` and route the crate's owned command
names, including the private `agy-hook` adapter. Default methods return nonzero
when their producer is unavailable.

- `prepare_incoming_source`: returns an exact parent-owned managed scratch lease
  with marker, live owner generation and Git registration checks. A cold update
  builds there with that checkout's toolchain and locked dependencies; offline
  mode is an explicit caller choice. The incoming binary's
  `link --plan` verifies its own embedded source inventory before the frozen
  payload is journaled into the original cache. Existing caches remain subject
  to strict validation. Preparation preserves the live checkout, local refs and
  installed files; explicit cleanup failure retains the ownership record and
  refuses the update. Disposable paths never enter the outer recovery journal.
- `incoming_lifecycle`: `Preflight` proves prepared payload/dispatch availability
  before live checkout mutation; `Reconcile` invokes that checkout's
  native `--oms-dispatch` frontdoor to converge recorded components and returns a
  schema-2 receipt; `Verify` invokes its native doctor; `RefreshTools` runs after
  core commit. Child processes must finish before returning. The child receives
  `OMS_NATIVE_UPDATE_PARENT` only in its own environment. Borrowing the receipt
  mutex requires the durable `native-update/lease.json`, matching receipt,
  live parent generation, process ancestry, and held mutex. It uses its own
  component journal. Recursive updates are refused.
  Reconciliation preserves the capability receipt and leaves all tool work to
  `RefreshTools`, which `--no-tools` and rollback skip. Older rollback targets
  still run their own installer implementation. Custom installation, tool and
  provider paths are resolved before switching child cwd. Generated
  schedulers retain the installation and Rust paths without freezing a temporary
  toolchain override or sourcing interactive shell startup files.
- `apply_project_template` and `provider_hook`: call the existing native
  template/hook domains for doctor fixtures and certified Antigravity routing.
  Fixture destinations are disposable. Advisory hooks cannot block a turn.
- `install_extras`: preflight/reconcile explicit service, permission, or
  publication options through their existing native authority checks. Reconcile
  occurs after the core install commits and retains independent recovery state.
- `hook_protection`: supply read-only CI protection proof for
  `install-hooks --quick --check-protection` before mutation.
- Tick maintenance, approved schedules, catalog refresh, optional GC, and wake
  remain the existing host callbacks specified below.

This crate does not implement those other native domains or change their public
contracts. Host wiring is required for their dependent modes; it is not needed
for self-contained configuration, cleanup, supply, probe, and diagnostic paths.

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

## State, concurrency, and interruption

The install lifecycle uses the canonical `install-lifecycle.lock.d` protocol,
including PID generation, stale-owner claims, and validated owner adoption, then
the receipt mutex. Account changes additionally lock the provider-account
registry before the receipt mutex. Claude capability probes run before these
locks. Provider/CLI mutations are serialized with lifecycle receipt publication.

The durable transaction journal records intended writes and prior payloads
before publication. Managed files use exclusive staging and atomic rename;
CLI-owned OMS trees preserve symlink targets and regular hardlinked content.
Snapshots copy the old payload without changing its live inode. Account writes
and removals bind the parent directory identity and expected prior contents or
link identity. A second call observes either a completed state or recovery work;
ambiguous concurrent changes keep the journal and backups instead of deleting
unrelated files. Update Git restoration uses expected refs/HEAD and clean-tree
checks, and its journal is separate from the incoming component journal.

A kill before sealing external CLI changes can leave an unsealed transaction;
automatic recovery refuses to overwrite an unproven result. A kill during purge
or interpreter retirement leaves the renamed tree in the transaction undo area
until commit/recovery. Receipt publication is last. Optional vendor updater/npm
internal state retains the vendor's own recovery semantics; tool refresh after
an update does not roll back an already committed core update. Failures state
that distinction explicitly.

Private probe/download directories use random names, exclusive creation, and
mode 0700. Ordinary unwind removes them; SIGKILL may leave a private directory.
Probe caches, model catalogs, security baselines, snapshots, and status records
use their own validated locks/atomic publication. They cannot certify an
unavailable native host producer. Durable native journals are intentionally
retained when recovery cannot prove ownership.

## Verification status

The crate-local `native_lifecycle` regression uses a disposable home/source and
an isolated PATH containing Git and a deterministic Codex inventory fixture,
with no Python executable. It covers native Claude hook/statusline generation,
account ownership round trips (or explicit ancestor-safety refusal in sandboxes
with unmapped ownership), schema-12 doctor surface validation, native Codex
cache generation without legacy templates, Git-hook convergence, cron
registration, legacy MCP/interpreter-link removal, install planning and missing
payload refusal, tick dispatch and all ten `Services` methods. All default
service methods fail closed. Public service signatures remain unchanged.

Focused verification:

```sh
cargo +1.99.1 test --manifest-path Cargo.toml -p oms-command-lifecycle --test native_lifecycle --locked --offline
cargo +1.99.1 clippy --manifest-path Cargo.toml -p oms-command-lifecycle --test native_lifecycle --locked --offline -- -D warnings
cargo +1.99.1 fmt --manifest-path Cargo.toml -p oms-command-lifecycle -- --check
```

Successful payload install/link/repair and certified Antigravity installation
still need the embedding host's authenticated native payload. Bootstrap's source
proof, outside this crate, retains its `scripts/agent-task.sh` authority marker
and reviewed source inventory. This crate does not weaken that proof. The
staged payload carries `oms` plus the Cargo-built companions
`oms-mcp-peer-owner`, `oms-run-bounded-helper`, `oms-state-queue-worker` and
`oms-graph-python-helper` (a native Rust parser, not an interpreter or a required
runtime). Verification accepts only `oms` and bootstrap's companion set
(adding `oms-supervisor` when present) and installs each present one under
`libexec/<revision>`. Legacy paths in cleanup, cron
migration, and provider evidence references remain readable. Disposable live
Antigravity certification probes use shell test fixtures; installed plugins do
not ship those fixtures. The integrity frontdoor and Git's executable hook entry
retain minimal shell launchers that execute native commands directly.

Provider CLI compatibility, real Linux/macOS installation, live surface/model
probes, signal/crash recovery, and cross-process contention require host
verification. No dependency, lockfile, or external dispatcher changes are made.
