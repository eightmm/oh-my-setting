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
The default panel layout, refresh, keys and actions remain unchanged. TM-06B
controls and TM-06C live-refresh/I/O calibration are separate work.
