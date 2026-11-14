# oms-command-hooks

Native hook adapters for a trusted installation root and an explicit caller
directory. This crate is isolated from the root Cargo workspace. It does not
install hooks or change the dispatcher. Full legacy parity is not established;
the remaining integration gaps below must be resolved before replacing all
installed hook surfaces.

`owns_hook(name, args)` is a pure command-name classifier. `run_hook` accepts
argv without the command name and already-read stdin bytes. `run_with_source`
reads stdin once, except for argument-only notification/configuration/audit
commands. All three return or classify malformed owned invocations. Execution
returns `(exit, stdout, stderr)`; unknown command names return 75. The library
does not change process-global cwd, environment, signal handlers or toolchains.

| Surface | Input and output contract |
| --- | --- |
| `command-guard-hook` | JSON command event; refusal on stderr with exit 2; malformed input allows with a diagnostic. `--audit [N]` accepts decimal counts, clamps 0 to 1 and large counts to 1000, and prints the audit view. |
| `git-move-guard-hook` | Panel-main command event; moving canonical Git state while calls are live exits 2 on stderr; other events allow. Malformed registry entries remain live. |
| `syntax-guard-hook` | Edit/Write/MultiEdit/apply_patch event; bounded findings in PostToolUse additionalContext JSON, always exit 0. At most 12 paths and 2 MiB per file. TOML uses oms-toml, JSON uses serde_json; Bash and Python edits use their syntax oracle. |
| `fail-ledger-hook` | Bash result/failure event; native fail-ledger records or resolves; only prior-context/advisor speech reaches stderr with exit 2. Background and interrupted successes do not resolve. |
| `resume-hook` | SessionStart event; plain resume text bounded to 4096 bytes, exit 0. Reads task/plan, handoff, failure, land, update, relay and optional memory state. |
| `telemetry-hook` | JSON lifecycle event; no stdout, exit 0; retained events contain bounded identifiers and reported numeric usage. |
| `precompact-handoff` | JSON event and optional agent argv; adopted repositories only; quiet exit 0 on every path. Includes Antigravity prompt-history capture. |
| `turn-guard` | Stop event; explicit session caps can emit a JSON block decision while exiting 0. An unavailable budget state emits a systemMessage notice. |
| `skill-router` | Prompt event; journal and bounded state/collaboration/context/skill hints on stdout, exit 0. Automatic task creation and skill suggestions remain opt-in. |
| `pre-push-check` | Git's four-column ref-update stdin; preserves tag/deletion skips, quick/full selection and check-runner exit status. Dirty tracked quick pushes exit 1. |
| `harness-hook` | Restricted Codex plugin subcommand; forces provider identity in the call context and wraps resume text as SessionStart additionalContext JSON; exit 0. It also serves the Pi extension: `panel-hook` (silent outside a panel main; otherwise presence on every event and panel and collaboration hints on a prompt), `relay` (presence plus turn relay, silent) and `relay-hint` (relay card; in a panel main, SessionStart startup/compact/clear/resume re-arms the control-tower hint and compact/clear add a resume line from room state and the claimed plan task, without the tmux number; the next prompt, whose hint also leads with that line for compact/clear/resume, consumes the marker once). The Pi subcommands read one JSON payload on stdin, run as Codex, stay silent on invalid input and record content-free injection rows for non-empty output. |
| `claude-statusline` | At most 256 KiB of JSON; bounded status text, optional ANSI color and cached Git/context readings; invalid input stays silent. |
| `claude-subagent-statusline` | At most 1 MiB of JSON; up to 128 sanitized id/content JSON rows; invalid input stays silent. |
| `codex-hook-bridge` | Strict bounded JSON/config paths and catalog stdin; only exact owned commands retire after native plugin verification and positively trusted/enabled hooks/list entries. Probe absence exits 1; errors preserve hooks and exit 2. |
| `codex-hud-config` | `install/remove/check PATH [--dry-run]`; owned HUD blocks only; preserves user/customized values, line endings and modes; check absence exits 1, errors exit 2. |
| `codex-usage-config` | Same actions plus `--notify-script PATH`; native TOML inspection preserves user values and multi_agent_v2 constraints. Removal retracts exact owned blocks without parsing TOML. |
| `codex-app-notify` | `send REPO TEXT`; quiet best effort, usage errors exit 2. `notify::deliver_codex` separately returns correlated submission/completion/persistence receipts. |
| `claude-app-notify` | `send [--wakeup] [--consumer HEX32 or --consumer=HEX32] [--] REPO TEXT` (unknown `-…` options exit 2; text after `--` stays literal) prints the delivery receipt. Without `--wakeup` nothing is posted (a post can start a paid turn); with it, a matching enrolled consumer (default: the caller's exported session) and a safe owned native socket are required. |
| `codex-turn-notify` | Last argv is the turn-complete JSON; quiet best effort, exit 0; ignores notification-chat/headless/subagent turns and writes sanitized OSC notifications to one selected terminal. |

`assignment` reuses the task crate's complete assignment validation and
collaboration checks. `surfaces` provides native Claude hook ownership,
convergence, removal, fail-closed policy and drift-audit helpers. Other modules
are `bridge`, `common`, `config`, `guards`, `handoff`, `lifecycle`, `notify`,
`push`, `routing`, `shell`, `statusline` and `syntax`.

The shared task, operations, room, memory, journal, state, peer, landing and
lifecycle producers retain ownership of their record formats. JSON encoding
uses oms-json; TOML parsing uses oms-toml. No external TOML dependency or
serde_json arbitrary-precision feature is requested. The pinned rustix crate
also enables `net` for safe Unix sockets and Linux peer-credential checks.

Runtime write behavior is deliberately explicit:

| Files/records | Concurrent call and interruption behavior |
| --- | --- |
| `.oms/hooks/events.jsonl`, `.oms/usage.jsonl`, `.oms/handoffs/consumed.jsonl` | Canonical native file locks serialize appends. Event appends also use the legacy `.events-lockdir`. Appends read only the final byte, use no-follow descriptor-relative opens and fsync. A killed append may leave a torn row; the next append begins on a fresh line. Size limits refuse further appends. |
| `.oms/hooks/sessions/*.json`, panel-activity rows, HUD `ctx-*`/`git-*` cache files, daily hint/skill-dedupe state, notify session state | oms-lock plus oms-safe-fs publishes complete replacement bytes. Budget counters and per-turn skill dedupe transform under the lock; room delivery holds a separate event-directory lock across its cursor read/write. Derived hints may repeat after a crash. Independent last-writer cache updates may supersede one another. An interrupted atomic publication can leave its exclusive temporary file; destination bytes remain old or complete new bytes. |
| Config, `.oms-bak`, bridge digest backup | One outer transaction protects backup plus replacement; exact original bytes are checked again before replacement. Existing first-touch backups, including empty files, are preserved. Bridge backups must match exactly. A kill between backup and replacement leaves the backup and original config. A kill after replacement leaves the backup and the complete new config. |
| Handoff digests and `.hook-capture-*.tmp` | Claude/Codex capture stages into a hidden non-digest file, then publishes a complete digest with task/verification/dissent context. Antigravity publishes the complete digest atomically. A kill before publication can leave a hidden staging file; it does not become the newest visible digest. Concurrent same-session captures within a second may replace the same complete digest. Journal observation follows publication, so interruption can leave a digest without its advisory journal event. |
| Native task packets, checkpoints, failures, operation references and journal projections | The owning native producer supplies its locks, validation, CAS and recovery behavior. A later task-step failure can leave an earlier valid packet update; the hook does not fabricate a completion receipt. |
| Notification chat state and `codex-notify.*.msg` | A machine-level native lock serializes senders. A kill after chat creation but before state publication can leave an unrecorded chat. A kill after submission can leave uncertain delivery and a message file; no receipt claims UI observation. Claude inbox submission remains delivery-unknown. |

Trust boundaries reviewed in source:

| Asset → boundary → abuse path | Control/evidence |
| --- | --- |
| State/config → untrusted paths → link/special-file replacement | `common` uses no-follow regular reads and descriptor-relative mutations; backups and config compare original bytes. |
| Agent execution → command payload → destructive shell operation | `shell` only tokenizes literals; `guards` preserves narrow refusals and child-specific Git behavior. It is an accident guard, not a shell sandbox. |
| User hook configuration → plugin inventory/trust → removal of user commands | `bridge` rejects duplicate JSON keys and unknown shapes, recognizes exact legacy commands, and requires a complete enabled/trusted native inventory. |
| Agent context → room data → implied instructions/acknowledgment | `routing` checks consumer/provider binding, marks peer data untrusted and never acknowledges delivered mail. Mismatched consumers withhold mail. |
| App wakeup → native registry/socket → another user's session | `notify` checks path ownership, private registry/socket modes, process liveness, socket identity and Linux peer credentials; Claude wakeup requires an explicit boolean. |
| Stored/transported text → retained output → sensitive content | oms-scrub redaction or refusal precedes retained summaries and notification transport. Receipts distinguish submission, uncertainty, completion and stored matching output. |

Remaining integration/parity gaps:

- Automatic `/clear`/resume/background-lineage room rebinding, secondary
  coordination-room delivery and the complete legacy panel reminder/idle/status
  hint set are not implemented. Changed consumers remain withheld.
- Detached context capture, Stop-triggered cross-app notifications and deferred
  journal/CI publishing need a host-owned asynchronous service. Current capture
  runs synchronously with cooperative cancellation; direct notification APIs
  are implemented. Resume's previous-room and validated portable-capsule hints
  are not yet connected.
- Push verification still invokes the canonical Bash check runner. Replacing
  that heavy subprocess requires a native gate service outside this crate.
  Bash syntax checks and explicit Python AST checks remain language oracles.
- Legacy output wording, transcript extraction details, optional memory
  rendering and all concurrent/crash paths have not received behavioral parity
  testing. Compilation does not establish those contracts.

Implementation-phase verification is limited to the requested offline
`cargo +1.99.1 check --all-targets` and `cargo +1.99.1 fmt -- --check` commands
with this manifest. Existing shell/Python suites do not route through the new
crate; they remain unchanged. No tests were added, deleted or weakened, and no
runtime, live-app, full-gate or fault-injection tests were run in this phase.
