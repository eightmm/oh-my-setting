# oms-command-panel

Native terminal panel command adapter. Panel reentry and job supervision resolve
`oms` in this order: the current executable when named `oms`, an executable
sibling `oms`, an existing `scripts/oms` under the supplied source root, then
an executable `oms` on `PATH`. Script directories and search directories are
canonicalized before changing working directory; the `oms` leaf is preserved,
including when it is a symlink to a versioned binary. Execute access is checked
with the OS, rather than inferred from mode bits. Interactive reentry uses
`bash` for the legacy script and accepts non-executable scripts. Job supervision
requires executable scripts and skips other scripts in favor of `PATH`.
Missing candidates produce an unavailable executable error before room creation,
state updates or tmux mutations. Each lifecycle entry resolves once and reuses
that prefix for its launch and watch commands.

`OMS_PANEL_JOB_EXECUTABLE` overrides supervisor resolution when nonempty and
must identify an executable file (bare command names are checked on `PATH`).
The override is passed through verbatim after validation and does not override
interactive panel reentry.
Native dispatcher, hooks, lifecycle, landing and Pi harness integration paths
receive the same high-risk recommendation as their legacy script counterparts.

Wave 3 hosts must supply the real source root when a legacy script is intended,
install sibling binaries together or expose `oms` on `PATH`, and retain the
native producer integrations described in the panel-core README. Resolution
performs reads only and creates no record, ref or file. Concurrent calls resolve
independently; interruption publishes nothing. Executable replacement between
resolution and process creation remains subject to ordinary OS spawn behavior.

Resolution regressions live in `tests/support/executable_resolution.rs` and run
as library tests with isolated subprocess environments and temporary Git repos.
`tests/panel_parity.rs` retains the end-to-end recommendation and CLI contracts.

Interactive Main registers a physical intent and pending usage through peer's
typed admission producer immediately before its inline spawn. Campaign soft
usage policy applies to that physical call. The tmux launcher only relays context;
it creates no duplicate call. State and Room roots are explicitly the selected
canonical repository, and absent caller context is removed from inherited tmux
server environment. Explicit empty or non-UTF-8 bindings cannot silently fall
back to an unbudgeted launch. Dispatch locks are released before execution; the
existing Main session lock remains held through spawn/wait and settlement.
TTY streams are inherited. Exit, spawn failure and cancellation retain unknown
usage and cleanup; a false cancellation shield permits the final lifecycle write.
The native root adapter qualifies the newly created Main/Room against the active
observed host policy before registration and again before launch. This owner
check works before Plan initialization, preserves child/depth restrictions and
does not turn TTY exit into sealed cleanup evidence.

Native resume enumerates complete Room summaries and reads current bindings with
`room show`. Ambiguous, foreign or unverified ownership refuses before provider
entry. A terminal Main's exact binding transfers to a fresh Main attempt through
Room's atomic `join --replaces` contract. Joined descendants must be reconciled
first. Physical usage liabilities survive this explicit resume. The canonical
library fixture uses real lifecycle/Room producers and fake provider processes;
it covers terminal resume, live/foreign/unverified/duplicate bindings, retained
descendants, budget refusal, spawn failure, exit 125 and cancellation. The staged
entry fixture executes the generated shell against stale server environment.

`oms panel --teams [--json]` is an optional read-only Team inspector. It reads the
canonical Plan `team-list`, host-local resource snapshot and retained physical
intents; it creates no enrollment or model call. Root owner/generation, Team
ID/generation and logical lead/member IDs remain distinct even with duplicate
labels. Contract phases and intent phases do not establish running, served model,
cleanup, usage or acceptance. Source errors preserve unknown/null; absence is not
zero native children. Resource occupancy is accounting-domain claims, with native
global hard caps explicitly unsupported. Reads are independent snapshots.
Team contracts and resource reads use the same explicitly selected state root;
the execution checkout remains separate for managed scratch worktrees. Ambient
state locators cannot redirect only the Team source. Member generations wider
than a JSON unsigned 64-bit integer are preserved as decimal strings in the view.

The view projects at most 64 Teams, 32 members per Team, 512 physical intents and
8 intents per member, and reports omitted counts. Plan responses use the existing
16 MiB bound; native resource/intent reads use their existing bounded registry.
Plain output and the existing Reader sanitize terminal text and redact content.
The Team inspector does not add Team controls or Team live refresh to the board.
TM-06B controls and TM-06C live-refresh/I/O calibration are separate work.

`oms panel --routes` preserves its existing JSON output, including with `--json`.
With `OMS_PANEL_READER_BOARD` and a foreground TTY, the non-JSON request opens the
existing Reader as a proposed-route inspector. `--json` always bypasses the
Reader. It displays the existing chosen snapshot: configured/requested/resolved
and unreported served identity, requested/selected/applied effort, account pin,
and catalog/capability source and reported ages. Missing evidence remains unknown;
reported age alone proves neither freshness nor account entitlement. Billing,
entitlement and quota remain unknown without a source. The view performs no
launch, approval, fallback, catalog refresh or model call. Default calls and
non-TTY output retain their existing behavior.

Optional producer-reported capability scope includes its alias/endpoint,
observation and expiry. The inspector separates freshness at read time from
selected-account matching, and leaves selected endpoint binding unknown.
Missing scope/time evidence stays unknown; a reported fresh source grants no
route eligibility. The native catalog currently supplies no scoped observation,
so known fresh/stale examples cover injected catalog ingestion and rendering only.

Tree and graph folding is currently library-only state; the interactive board
does not yet provide fold or unfold keys.

The Auto overview puts Overall Main above the work list. An active managed call
appears first as direct work when its joined Room membership and lifecycle
parent attempt both identify the current live owner. Wide boards label this
area `Overall Main · Direct work`. Provider Leads, native observations and calls
with incomplete or historical parent evidence stay in provider activity groups.
Provider sections show recorded work and retain confirmed session activity.
Click `Lead chat [1/2/3]` to dock a connected provider's existing native session.

The title uses the project name and the Board shows weekly quota left when space
allows. Plan progress appears only for work linked to the current Room.
Click the weekly line or press `w` for each provider's reading, source and
reset time (scrollable with the existing overlay scroll).
Current Codex quota stays unlabelled unless positively verified for the live
Main or Codex lead and its per-home quota receipt; registered Codex accounts
appear separately with their session-log reading, log age and reset, withholding
percentages for stale, expired or missing entries. The details show refresh
progress until a fresh snapshot arrives. Press `U` to refresh quota from idle
Claude or AGY sessions. `Esc` returns to the Board. Missing readings show `—`.
Aged or reset readings carry their own status.

Terminal calls collapse into history. `Tab` opens the all-calls views, and explicit
search or Attention includes matching history. Narrow boards use the flat view
and preserve the same selection identity.

Active managed workers bound to the current Room and Main enter Attention as
`no recent lifecycle events` when their latest recorded event reaches the
existing heartbeat window (300 seconds by default). This includes starting,
working and verifying calls, and uses the snapshot's observation time. Fresh
events clear the advisory; missing, malformed, future or stale snapshot evidence
cannot create it. Native children, unmanaged native assignments and Main are
excluded. The attention-only filter keeps these rows visible. The worker row
shows `[no events]`; Call Detail gives the reason and last recorded lifecycle
state. This is an event-liveness advisory, not proof of missing output or a stalled model;
it neither reads transcripts nor cancels, retries or changes a lifecycle state.

The [panel contract](../../docs/TEAM-ORCHESTRATION-PANEL.md) describes the
task-first view, native assignment binding and shared result inspector planned
on top of the existing task, Room, attempt and result records.

On a foreground terminal with tmux available, bare `oms panel` opens a split
board and offers installed Codex, Claude, AGY, and Pi Main harnesses for the
current repository. Pi requires the Codex CLI and uses the Codex gateway.
If its managed Main already exists, it reopens that pane
and restores its board if needed without asking again. After a fresh Main starts,
available provider orchestrators open or reconnect in background after the Main
and room are verified. Missing or failing providers do not prevent use of Main.
Codex and AGY orchestrators start idle; role guidance accompanies assignments.
Use `oms panel --watch` for a standalone board. Non-terminal invocations keep the
watcher output path.

A fresh managed Codex or Claude Main can select a configured account with
`oms panel --launch codex --account ID` or
`oms panel --launch claude --account ID`. When a Main is already open, the
requested account must match its verified launch binding.

The managed Main can open provider orchestrators with
`oms panel --orchestrator PROVIDER --room ROOM`. Use that command with
`--task TEXT` to submit a one-line assignment to an idle, connected provider.
The Main contract is management only: Main plans, assigns, reviews evidence
and decides acceptance, and sends every repository edit to a worker or Lead
unless the user tells it to make a change itself. Provider orchestrators lead
one assignment each, delegate substantive implementation to scoped workers and
keep their own edits to integration and unblocking. Research, review and verification use the provider's read-only
controls where available. Native child records report effective access as
unverified. Set `OMS_PANEL_MAIN_PERMISSIONS=native` for new Main and
orchestrator launches to use provider permission settings without OMS adding
bypass flags. This includes automatic leads and Codex's alternate lead.
The board can dock that provider's existing native input pane without restarting
the conversation. Only live panes from the selected repository and Room can
receive board usage refreshes. Native provider scheduling is outside OMS campaign
accounting and requires an Overall Main recorded outside a managed campaign.

For a canonical Plan task, an existing ordinary Codex Lead also accepts:

```sh
oms panel --orchestrator codex --room ROOM --task-id TASK --task 'Inspect the assigned source'
```

This opt-in path claims a ready task through Plan once, or reuses the current
Main's unexpired claim. Before any input, it creates an idempotent lifecycle
attempt, writes a private immutable `native-assignment.json` under that attempt,
and sends an addressed Room notice containing the evidence reference. The
binding captures task criteria and scope, plan/lease identity, Main and Lead pane
process generations, persisted account custody and its verification status,
actual cwd/worktree and base SHA, and the exact payload digest. An ambient or
untagged account remains explicitly unverified. A linked worktree must belong to the same Git
repository. The task, lease, operation, checkout and native target are checked
again before buffering, pasting and Enter. It uses the existing Plan, Room and
lifecycle writers; it does not create another assignment database.

An exact repeat with the same unexpired claim reuses the attempt and Room
message without claiming or sending again. Once the durable send boundary is
recorded, interrupted or unconfirmed delivery returns `status: "unknown"` and
exit 75. An exact retry under that claim reports the saved attempt. A prefix
that stopped before that boundary can resume its remaining steps.

If the claim expires, is released, or changes, inspect the saved attempt with
`oms agent-events show --attempt ATTEMPT --json`, the private binding and Room
message, and the existing provider pane. Reconcile the old attempt before
dispatching under a new claim or Main. Confirm the native job's state in its
provider session even when its OMS lifecycle record is terminal.

`submitted` means the exact Turn appeared in a submitted user message. Provider
consumption and completion remain `unconfirmed` by transport or return claims.
New bound assignments freeze an explicit task scope, parent verifier and declared
role, plus canonical requirement IDs when present. The default declared role is
`worker`; it grants no capabilities. Task ID/title is the fallback deliverable only
when the task has no registered requirement IDs. This checks coverage of the
registered contract, not whether all user prose or task semantics were satisfied.

`oms panel --results --task-id TASK --json` exposes a typed source template, the
existing capture manifest template, deterministic input paths and a structured
next action. Fill the source and manifest, then import with
`--orchestrator codex --task-id TASK --native-result MANIFEST`. The existing capture
owner retains immutable source, candidate and verification bytes. Reports claim
`reported`, `blocked` or `needs-decision`; reported checks never become verifier or
acceptance receipts. Legacy prose reports remain readable as advisory evidence.

Main reviews the exact returned `result.sha256` with
`--native-reconcile ATTEMPT --result-sha256 SHA --decision request-revision|blocked|reviewed`.
After requesting revision, `--orchestrator codex --task-id TASK --task TEXT
--after-native-result SHA` sends another immutable round under the same claim.
Replacing a typed report within a round instead requires both the manifest's
`previous_sha256` and `--expected-previous-result-sha256 SHA`; identical replay and
the first report need no replacement flag. These public digests identify the
retained result artifact, not its source file.

After a canonical Main swap, Results offers
`--native-adopt ATTEMPT --assignment-sha256 SHA [--result-sha256 SHA]`.
The new Main explicitly adopts the exact current assignment and, if present, its
retained result. Omitting the result digest asserts that no current result exists.
The producer proves the ordered Room swap lineage, outgoing wrapper/native process
absence, and fresh successor Main custody. An immutable successor record preserves
the original task claimant, lease, attempt, round and account binding. The stable
Room participant is distinct from the new Main attempt; no claim or budget is reset.
The original Codex Lead/account proof is rechecked through its original private
anchor, without changing accounts or reentering provider input. Supported Main
provider swaps retain this ordinary Codex Lead boundary.

Import, reconciliation, report revision and another round work after adoption.
Unknown input remains unknown and is not replayed. A canonically done/cancelled task
instead permits retirement-only adoption: the original checkout may have advanced,
but this mode cannot reopen execution. The next task obtains its own ordinary
claim and freshly checked checkout after the prior reservation is retired.
Existing tick reminders follow the checked successor, revalidate its current
session and process, and include the new Main attempt in their dedupe identity.
A cue delivered before the swap therefore does not hide unconsumed work from the
successor; it still spends the same daily wake quota. Retirement-only custody
produces an administrative retirement reminder, never an import or execution decision.

A reviewed candidate can enter canonical Plan review with the exact lease and
captured patch. Existing `patch-admit`/`patch-land --plan-task` verification and
genuine Plan finish still own acceptance. After canonical completion or
cancellation, exact-result reconciliation retires the assignment's lifecycle and
Room reservation while preserving the reusable Lead session. There is no automatic
claim heartbeat, native child cancellation/resume, or cleanup proof;
`write_isolation` remains `unmanaged` and `child_cleanup` remains `unsupported`.

This native bound path supports the ordinary Codex Lead. Claude and AGY expose
the same task selection through a managed Plan runner:

```sh
oms panel --orchestrator claude --room ROOM --task-id TASK
oms panel --orchestrator antigravity --room ROOM --task-id TASK --dry-run --json
```

The current Main must own the Room. The task must be ready and its frozen
assignment provider must match the selected provider when one is recorded.
This route pins the canonical task digest, then uses `plan-run` and its issued
claim, scope, verifier and review gates. It does not type into a native Lead or
claim native child custody. It stops at review without automatic landing or
publication. Do not pre-claim; this route claims the ready task itself.
An existing claim is refused, even when expired; reconcile it
separately. `--task`, account, model, effort and native-result overrides are
refused: amend the reviewed Plan to change the work. `--dry-run` only checks
ownership and the frozen selection; it launches no runner or provider. JSON
labels the managed transport and preserves producer output and completion
flags. Incomplete output or cleanup is unknown, with exit 75 when the producer
otherwise returned zero; review success is never inferred from a process exit.
For a failure or unknown return, inspect retained Plan/worker evidence and follow
checked Plan recovery. Do not use `panel --continue` for this route or resubmit
the task blindly; `--continue` belongs to role-dispatch workers.

The Main can attach a reviewed context pack with `--context-pack FILE
--expected-context-pack-sha256 SHA`. The app-only desktop
`oms_prepare_lead_plan_context` tool prepares one explicitly selected Lead
answer excerpt (at most 4096 UTF-8 bytes) for the exact ready task and managed
provider. It reuses the owned answer-page reader and returns separate preview
and execution commands. Use the returned `--dry-run --json` command to inspect
the route, task, assignment and source provenance, then its execution command
under the current Main. Preparation never claims a task or calls a provider.
The pack remains reference data: execution uses the managed Plan worker, with
`native_lead_input: false`. Pack changes, Lead source answer changes, another
physical state repository/Room, or changes to the reviewed task, scope, verifier
or frozen model refuse dispatch. Existing graph packs and managed calls without a
context pack retain their behavior. See the [explicit selection mapping](../oms-mcp-server/README.md#reviewed-lead-findings-for-a-plan-task).

The alternate Codex Lead remains outside the native task binding. Direct
`--dispatch ROLE --task-id TASK` labels managed work but is not an issued Plan
lease; use `plan-run` when the canonical Plan lifecycle is required.
Explicit `--unbound --task TEXT` is conversation without task credit. Main
chooses import, review and the next round; the existing bounded tick wake can send
a reminder into Main's native session, without importing, reconciling or dispatching
the Lead automatically. The
existing approved goal-drive/autopilot contracts remain the owners of bounded
automatic execution. See the [native return workflow](../../docs/TERMINAL-PANEL.md#retain-a-bound-native-report)
for the two input layers and evidence limits.

Click a Codex, Claude or AGY native child row, or select it and press Enter.
The Board opens available recent replies after verifying the Room, Main,
provider pane, assignment turn and child identity. A recorded parent summary
may be available without the child chat. `Esc` closes the reader.

Host-side desktop project map operations manage `oh-my-setting/desktop-projects.json`
under `XDG_CONFIG_HOME` (else `~/.config`), or `OMS_PANEL_PROJECTS_FILE` when set,
with optional per-alias areas stored in a sibling mode `0600`
`desktop-project-areas.json` sidecar so the project map stays a strict object of
alias to path:

```sh
oms panel --register-project [PATH] [--alias NAME] [--area NAME]
oms panel --unregister-project ALIAS
oms panel --projects [--json]
oms panel --install-panel claude|codex --project ALIAS [--dry-run] [--json]
oms panel --uninstall-panel claude|codex --project ALIAS [--dry-run] [--json]
```

`--register-project` resolves `PATH` (defaulting to the current directory) to its
physical absolute path, requires a Git checkout or adopted OMS project, and defaults
`--alias` to the checkout directory name. `--area NAME` sets the alias's area (1 to 24
bytes after trimming surrounding spaces, allowing letters, digits, space, `-`, `_`,
`.` and Hangul); omitting `--area` keeps any existing area for that alias, and
`--area ""` clears it. Re-registering the same alias and path without changing its
area is a no-op; a duplicate alias pointing to another path is refused with a
suggested `--alias` alternative. Writes create missing parent directories, hold the
map lock across both files, stage mode `0600` temporary files in the target directory
and rename them atomically, preserving existing entries (at most 128 aliases and 16
KiB) while refusing symlinks, hard links, foreign or group/world-writable map files,
non-private sidecar files, `OMS_PANEL_PROJECTS` inline maps (which have no sidecar),
`OMS_PANEL_REPO` pinned sessions (which block map edits and `--projects`), and
delegated worker callers. `--unregister-project` removes one alias and its area,
unlinking the default map (and empty sidecar) when the last entry is removed while
refusing to remove the last alias from an explicit `OMS_PANEL_PROJECTS_FILE` map.
`--projects [--json]` refuses delegated workers and missing explicit
`OMS_PANEL_PROJECTS_FILE` paths, shows any registered area next to each alias, and
marks entries whose checkout path no longer exists. `--install-panel claude --project ALIAS`
resolves `ALIAS` from the host project map and runs `claude` (`OMS_CLAUDE_BIN`, else on `PATH`)
in the physical checkout to register a local-scope `oms-panel` MCP entry pinned via
`OMS_PANEL_REPO` with `OMS_MCP_TOOL_PROFILE=chatgpt-panel` and `~/.local/bin/oms mcp-server`,
adopting an exact-match unrecorded local entry (`action: "adopted"`), refusing differing local
`oms-panel` entries, recording ownership in the sibling mode `0600`
`desktop-project-panels.json` sidecar (rolling back once on `add-json` timeout/interrupt or
record-write failure and reporting `uncertain: true` when rollback fails), and treating
identical reinstalls as no-ops; `--uninstall-panel claude --project ALIAS` forgets the ownership
row without running `claude` when the recorded checkout is gone (`action: "forgotten"`), probes `claude mcp get` first to leave user-modified local entries in place while dropping the row, and
removes only matching entries. `--install-panel codex --project ALIAS` and
`--uninstall-panel codex --project ALIAS` are supported only for the default user-config project
map (`oh-my-setting/desktop-projects.json` under `XDG_CONFIG_HOME` or `~/.config`; any other
active map file is refused without modifying anything, while Claude panels remain unaffected).
`--install-panel codex --project ALIAS` requires `ALIAS` to form a valid lowercase Codex plugin
name `oms-ALIAS` (1 to 60 lowercase ASCII letters or digits separated by single `-`; aliases
containing `.` are refused for Codex while Claude panels still accept them), maintains an
OMS-marked mode `0700`/`0600` host marketplace `oms-host-panels` under
`oh-my-setting/codex-panels/` in the user config directory (`plugins/oms-ALIAS/{plugin.json,mcp.json}`
pinned with `OMS_PANEL_REPO`, `OMS_MCP_TOOL_PROFILE=chatgpt-panel`,
`OMS_MCP_PANEL_RESOURCE_VERSION`, and the absolute `~/.local/bin/oms` launcher in `mcp.json`
matching the Claude panel because GUI apps may lack the user `PATH`, without copying skills),
builds `marketplace.json` from the aliases that own a marked bundle on disk plus the alias being
installed, registers the marketplace once and adds `oms-ALIAS@oms-host-panels` via
`OMS_CODEX_BIN`/`codex`, and records a `user`-scope ownership row carrying the `plugin_sha256` and
`mcp_sha256` digests of the written bundle files (strictly required on Codex rows while Claude rows
carry none; rolling back `codex plugin add` whenever Codex did not already list
`oms-ALIAS@oms-host-panels` before the run, and reporting `uncertain: true` while keeping the
bundle and marketplace entry on disk if `plugin remove` fails so a later uninstall can clean up).
OMS identifies installed Codex plugins by `oms-ALIAS`, `oms-host-panels`, and the recorded checkout
(never gating ownership on version or `enabled`), and reports `"disabled": true` as a separate field
in `--json` output when the plugin is disabled in Codex.
Reinstalling an alias with a matching row and unedited bundle refreshes `plugin.json` and
`mcp.json` to the running OMS package and panel resource versions, re-runs `codex plugin add` so
Codex updates its local plugin cache (which re-enables a disabled plugin in `codex` CLI; if a
refresh `codex plugin add` or subsequent sidecar write fails, rollback restores the prior bundle
bytes on disk, re-runs `codex plugin add`, and confirms the restored state via
`codex plugin list --json`, reporting `uncertain` rather than `rolled back` if that fails or cannot
be confirmed), and updates the recorded SHA-256 digests (refusing if the
bundle on disk was modified outside OMS, while treating an already-current bundle as a no-op that
keeps a disabled plugin disabled);
installing with no row when `oms-ALIAS@oms-host-panels` and its marked bundle match OMS's manifest
for that alias and checkout apart from `version` and `OMS_MCP_PANEL_RESOURCE_VERSION` adopts the
existing registration (`action: "adopted"`, keeping any disabled choice) and records its on-disk
digests instead of refusing as unmanaged. `--uninstall-panel codex --project ALIAS` removes a
disabled OMS plugin like an enabled one, checks bundle edits
against the recorded `plugin_sha256`/`mcp_sha256` digests so OMS upgrades uninstall cleanly while
leaving genuinely user-edited `plugin.json` or `mcp.json` bundles and the marketplace in place
(running `codex plugin remove` only when the installed plugin still matches OMS and the recorded
checkout, never deleting the bundle directory or `marketplace.json` entry of any plugin that
remains installed in Codex, dropping the ownership row when leaving user-modified state in place,
and reporting any edited file), keeps `oms-host-panels` and
the root when Codex still lists another plugin from `oms-host-panels` (reporting what remains
installed) or another bundle remains on disk, and otherwise removes the ownership marker last after
all other OMS-written files and directories are gone (restoring the marker and keeping the row if
removing the root fails), while `--unregister-project` refuses aliases that still have a recorded
panel.
