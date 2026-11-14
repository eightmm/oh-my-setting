# Native lifecycle implementation draft

This standalone crate is a partial migration. It is not ready to replace the
lifecycle dispatcher. Compilation does not establish behavior or parity with
the shell implementations. The parent owns admission and integration.

Public entrypoints:

```rust
pub fn owns(tool: &str, args: &[std::ffi::OsString]) -> bool;
pub fn run_with_source(
    tool: &str,
    args: &[std::ffi::OsString],
    cwd: &std::path::Path,
    source: &std::path::Path,
    interrupted: &std::sync::atomic::AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>);
```

`owns` is an IO-free command-family selector, including malformed invocations.
`run_with_source` returns stdout/stderr and an exit status without changing
process-global cwd, environment, or signal handlers. Unsupported paths return
an explicit nonzero result. Arguments currently require UTF-8.

## Implemented paths

- `link`, `unlink`, and `install-skills`: schema-1/2 receipt reading,
  schema-2 receipt production, physical ownership comparisons, enabled skill
  selection, copy markers, user backups, and native payload/helper checks.
- `install-claude-hooks`: additive hook convergence, `--settings`, `--remove`,
  and `--print-expected`; hook rows are read as data from the source declaration.
- `install-hooks`: full/quick Git hook materialization, with refusal to replace
  an unmanaged hook. The protection-probe adapter remains unavailable.
- `install-mcp`: native stdio registration for Claude JSON and an isolated
  managed Codex TOML block. Existing unmanaged TOML tables are refused.
- `install-profile` / `install-tools`: profile inheritance and capability
  availability checks, `--plan`, `--check`, `--reapply`, `--add`, `--receipt`,
  and explicit degraded receipts with `--allow-missing`.
- `install-autoupdate`, `uninstall-autoupdate`, and `tick install|uninstall`:
  native scheduler file production, strict cron marker parsing, and journaled
  recovery data for external crontab/user-manager changes. Existing systemd
  units without the native ownership marker are refused.
- `status`, `--fingerprint`, and `--compare`: native metadata/digest reports.
- `snapshot` / `write-machine-snapshot`: atomic private machine snapshots,
  `--check`, and `--dry-run`, without requiring an interpreter. Hardware probes
  are bounded; home storage currently reports unknown.
- `update --check` and `auto-update check|status|attention|install|remove`:
  read/fetch/update-availability paths and scheduler routing. `attention`
  currently reports only no-run or unknown.
- `tick register|unregister|status|run`: physical-path registry, native attempt
  reconciliation, memory health, telemetry inspection, and explicitly partial
  sweep receipts.
- `doctor`: core receipt/target/payload/skill/operations/memory checks,
  `--repair` of links, `--surfaces`, `--tool-lock`, `--security`, `--residue`,
  `--sandbox [--probe]`, JSON extension inventory, MCP pins, provider-table
  reading, and structured findings/remediation output. Configuration is data;
  no provider/server process runs in these reports. `--sandbox --probe` alone
  runs the explicitly requested bwrap smoke probe.

## Remaining migration work

`oms-bootstrap::ProducerRequest::execute` currently always returns
`ProducerUnavailable`, including `StageNativePayload`, `WriteInstallReceipt`,
and `MaterializeFrontdoor` (see the sibling crate's `src/producer.rs`). The
verified incoming-revision payload producer is therefore unavailable through
that dependency API. Feature expansion stopped at this brief-defined stop
condition; the crate does not infer a successful producer from cache metadata.

The following paths are incomplete and must remain outside dispatcher cutover:

- Full `install` / `uninstall` orchestration and purge. These fail before edits.
- Applying/rolling back an update, incoming native payload production, worker
  and panel-call fences, fallback-to-edge, component refresh, and its doctor
  rollback gate. Apply/rollback currently fail before checkout mutation.
- Verified tool downloads, provider installation/refresh, full/only/upgrade
  selection, and legacy managed interpreter-tree retirement. A proven legacy
  launcher can be unlinked; interpreter directories are preserved.
- Codex/Antigravity plugin adapters, Antigravity surface certification, Codex
  HUD/usage settings, Claude HUD/default settings, previous-checkout hook
  adoption, registered-account reservation/refresh, and fail-closed guard
  capability negotiation. Plugin commands currently fail before provider edits.
- A full native TOML parser for security and extension inventory; affected
  provider configurations are reported unknown. No unsupported parser result
  is presented as an empty configuration.
- MCP tools/list probing, extension plugin inventory, complete MCP risk
  classification, the security finding-change baseline, model capability/live
  probes, strict diversity, full provider copy/digest inventories, contract
  checks, provider definition/test-reference validation, and the probe memo.
- Full tick maintenance, approved contract schedules, catalog refresh, optional
  garbage collection and idle-main wake. A sweep reports `partial` and names
  every unavailable phase rather than certifying completion.
- Complete legacy CLI/help/JSON/text compatibility and lowest-host behavioral
  verification. The declared context-report/failure adapter dependencies still
  need domain integration; their unrelated report schemas are not repurposed.

## State, concurrent calls, and interruption

| Written object | Concurrent call / interruption behavior |
| --- | --- |
| Install receipt, native libexec payloads, managed links/copies, ownership markers, displaced-user backups | Native host mutations serialize on the physical receipt's `oms-lock`. The receipt is published after target changes. Each target rename has a preceding durable journal entry; undo entries remain until commit. Modified targets cause recovery refusal rather than deletion. Legacy lifecycle-lock adoption is not implemented. |
| `native-lifecycle/transaction.json`, `.oms-stage-*`, `.oms-undo-*` | An incomplete journal is rolled back before a subsequent transaction. A committed journal finishes cleanup. A kill before a stage is journaled may leave an unclaimed staging entry; it is preserved. Recovery conflicts retain the journal/undo material. |
| Claude settings, one-time backup, Claude MCP JSON, Codex MCP managed block, Git pre-push hook | These use the same native receipt lock and target journal. User siblings are preserved by the merge. Independent provider/configuration writers do not honor that lock; complete compare-and-swap coverage against them remains unverified. |
| User service/timer files and scheduler activation | Unit file writes use the target journal. External state is recorded before mutation, and rollback restores the recorded activation state. Recovery may refuse if host tools are unavailable. Native recovery has not been exercised under kill injection. |
| External crontab or `OH_MY_SETTING_AUTO_UPDATE_CRON_FILE` | The former records before/after state durably, refusing restoration if an independent writer changed jobs. The latter is an ordinary journaled file. Marker corruption refuses the entire block edit. Legacy tick single-line migration is incomplete. |
| Capability receipt | Serialized native receipt transaction; publication is atomic. Missing tools are never claimed available. |
| MCP pin file (`mcp-pins.json` / `OMS_MCP_PIN_FILE`) | Atomic replacement under a canonical file lock. Unknown providers retain existing pins. Baseline collection currently precedes lock acquisition; concurrent pin-merge behavior still needs repair/verification. |
| Tick registry (`tick-repos.txt` / `OMS_TICK_REGISTRY`) | Read/modify/write under its canonical file lock and atomic publication. Physical duplicate paths converge to one entry. |
| Per-repository `.oms/tick/last.json` | Repository receipt lock and atomic publication; a stopped sweep leaves the previous completed receipt. Native reconciliation retains its own domain locks. |
| Auto-update state (`local/auto-update.status` or override) | Canonical file lock and atomic publication. A failed fetch leaves the previous state. Concurrent source fetches are not serialized yet. |
| Probe captures | Exclusive files under TMPDIR are immediately unlinked; open descriptors hold them only for the bounded child invocation. Process groups are terminated on timeout/cancellation. |

All source changes are confined to this crate. No existing tests were changed.
Behavior, boundary, concurrency, and kill-injection tests were intentionally not
run in the implementation-first phase. Only the requested offline compile and
format checks were used; review and behavioral verification remain mandatory.
