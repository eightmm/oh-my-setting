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
The assignments give the Main and provider orchestrators responsibility for
review and verification, with substantive implementation delegated to scoped
workers. Research, review and verification use the provider's read-only
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
consumption and task completion remain `unconfirmed`; the canonical task is
still claimed and the lifecycle transport attempt stays nonterminal until its
owner settles it. There is no synthetic Room receipt, verifier result or task
acceptance or automatic claim heartbeat. Result capture/review uses its existing
owners and is not automated by this bridge. Native session IDs and native child cancellation/resume are not
supported here. Cwd and base identity are evidence, not write isolation:
`write_isolation` is `unmanaged` and `child_cleanup` is `unsupported`. This first
bound path supports the ordinary Codex Lead; Claude, AGY and alternate Codex
Leads continue to use the existing unbound conversation path.

Click a Codex, Claude or AGY native child row, or select it and press Enter.
The Board opens available recent replies after verifying the Room, Main,
provider pane, assignment turn and child identity. A recorded parent summary
may be available without the child chat. `Esc` closes the reader.
