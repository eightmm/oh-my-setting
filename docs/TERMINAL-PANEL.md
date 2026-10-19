# OMS terminal control panel

This guide is the full reference for using the OMS terminal panel and its recorded work rooms.
It covers startup, navigation, role routing, results, recovery and the contracts behind them.

## In this guide

- [Quick start](#quick-start) and [starting the sole main](#starting-the-sole-main)
- [Layout and activity](#layout-and-activity), including [keys](#keys)
- [Roles and automatic allocation](#roles-and-automatic-allocation)
- [Discussion and results](#discussion-and-results)
- [Reference](#shared-work-rooms-and-the-graph), [contract and verification](#contract-and-verification), and [recovery](#salvaging-a-stopped-worker)

Run `oms` in a terminal to open the control panel. Native Claude Code, Pi or Codex
keeps the conversation pane; the OMS sidebar shows recorded main, advisor and
worker activity. `oms --help` and bare `oms` with redirected input/output retain
command help. The panel uses existing OMS state, authentication and front doors.
Without `--repo`, the panel resolves the current Git top-level and refuses a non-repository with exit 2; `--providers`, `--routes` and `--help` remain available anywhere, and explicit `--repo PATH` also accepts a non-repository directory.
The sidebar reads OMS state; it does not replace the providers' native conversation or authentication.

## Quick start

1. Run `oms` in a terminal. The control window is the inbox; the panel starts at most one live main, using Claude by default. Pi is the explicit alternative (`OMS_PANEL_MAIN=pi` or the Pi start key).
2. The main window keeps the board on the left whenever the window is wide enough (above the CLI only when it is too narrow); the right side is the reader for workers, advisors, reviews and results, alongside the native conversation. At wide widths it shows the graph; narrower layouts fall back to the tree. At fewer than 20 columns the board omits its tab box.
3. The compact goal banner and Plan tab show a bounded goal preview (up to 200 characters and, in Plan, two wrapped lines); `oms agent-plan list` prints the full plan. The bottom box contains Log, Detail, Plan, Messages, Past and Attention tabs (Past and Attention stay even when empty), plus Debate while a debate exists. The pinned **Needs you** attention strip stays above activity when the inbox has items.
4. The control window's **Needs you** inbox lists failed or timed-out calls, calls or mains waiting for approval or input, unanswered broadcasts and patches awaiting admission. Select with arrows or a click; Enter opens the selected chat or result.
5. `Tab` cycles visible tabs; arrows move within the current surface; Enter opens or expands. `Esc` closes a reader or backs out. The detailed keys and their surfaces are listed under [Keys](#keys).
6. At a main's exit prompt, `r` resumes or restarts in that pane and Enter closes the window. `F9` switches between chat and board; `F5` opens the control window, and from that window returns to the previous one; `F12` shows panel-wide keys.

```bash
oms panel --repo . --layout inline         # portable menu, no tmux needed
oms panel --repo . --layout split          # uses an already-installed tmux
oms panel --repo . --host tmux --launch codex   # team graph left of native chat
oms panel --repo . --host tmux --launch claude
oms panel --repo . --reopen                # restore only a missing tmux board
oms panel --repo . --json                  # read-only status and role policy
oms panel --repo . --routes                # read-only role policy
oms panel --repo . --providers --json      # registered CLI peers; no provider execution
oms panel --repo . --watch --owner claude  # read-only activity sidebar
oms panel --repo . --watch --view compact --attention-only
oms panel --repo . --room team --watch --view tree
oms panel --repo . --launch codex --task 'Explain the parser and fix its bounded bug'
oms panel --repo . --launch claude --resume SESSION_ID
oms panel --repo . --launch codex --model MODEL --dry-run
oms panel --repo . --swap-main codex [--model MODEL]
oms panel --repo . --results               # summaries first, ordinary scrollable text
oms panel --repo . --results --task-id ID --json
oms panel --repo . --room team --chats --json
oms panel --repo . --room team --open-chat sol-main --chat-surface app --dry-run --json
oms panel --repo . --council --owner codex --task-id ID --label 'Decision' --prompt 'Compare options' --dry-run
```

`oms panel --salvage TASK_ID` verifies a kept worktree against the execution repository recorded for that room call and saves its patch and provenance in the room repository.

Each dispatched call has a short room alias such as `B7`, shown before its role
on the board, selected card, reader, session log and Results. `--alias NAME`
chooses one (1–8 letters, digits or hyphens); otherwise the panel assigns the
next alias using the role letter and number. Aliases are unique across a room lineage.
`--continue TASK_OR_ALIAS` retains the continued call's alias, and Results
accepts an alias in `--task-id` when it resolves to one task. The default alias
prefix is selected by role (for example, `B` for builder and `W` for worker).


## Starting the sole main

Panel mains carry the assigned goal through worker recovery, result inspection,
integration, required checks and owner finalization. A worker's exit or review
handoff is an intermediate step. The main continues actionable work within the
user's scope, or records a concrete blocker and the next action. Publication
still requires the user's authority; a local result can be completed first.

The `s` board key lists installed main targets and their weekly quota readings, then
requests a bounded handoff before swapping. `oms panel --swap-main PROVIDER
[--model MODEL]` is the command form. It requires an existing tmux panel, one
verified live main and an inactive landing operation. The current main is asked
for a handoff of at most 3000 bytes (90 seconds by default via `OMS_PANEL_SWAP_HANDOFF_SECONDS`, capped at 300); if
none arrives, OMS uses a recorded-state digest. OMS then exits the old main and
relaunches the replacement in the same pane and room without stopping workers.
If the old main does not exit, the panel preserves it and the room claims.
A current Pi main can be swapped without a resume binding: the replacement
starts fresh. If replacement startup fails or ownership cannot be verified,
OMS does not restore Pi; it reports no verified replacement main and retains
the handoff file and room claims for recovery. `--model`
selects an exact installed catalog model; swap has no dry-run and requires tmux.

Use the shared `OMS_ROOM_REPO` for plan and result records when editing elsewhere.
For `--dispatch`, it selects the state repository for the room, main attempt,
and call results; `--repo` still selects the source and execution repository
for workers, researchers, advisors and reviewers. Explicit reviews inspect and verify
that source repository. Automatic patch reviews instead construct a private checkout
of the worker's recorded base plus its digest-verified patch; both the complete diff
and mechanical gate use that candidate. Missing base objects, digest mismatches or
patch-application failures require manual review; they never fall back to testing
the unpatched source. The gate rejects candidate changes by the verifier.
The automatic review dispatches the reviewer at the deep tier, or at frontier when the patch's changed files are unknown or match a trust-boundary pattern (`_AUTO_REVIEW_FRONTIER_PATHS` in scripts/lib/panel_review.py: plugins, hooks, install/uninstall, land and patch scripts, workflows, rules, permission/settings/policy files).
A write call that exits 0 with a digest-verified patch, no failed verify and no guard stop starts this review automatically (`auto_review` in config/panel-routing.json, default true). It reuses the call's `--verify`, else the plan task's, and mails the main for manual review when neither exists. A FAIL verdict drafts a `--continue` fix brief and prints its command without launching it.
`--room` selects a room without
changing `OMS_ROOM_REPO`. With no `OMS_ROOM_REPO`, that room resolves in `--repo`.
An owner result saved with `--finalize --outcome completed --verify COMMAND`
binds to the current plan and task. Plan displays **completed locally** for a
matching passing result, including one recorded outside a panel session, while
retaining the plan's review/acceptance state. Changed task generations, replaced
plans, modified evidence and a later unverified result invalidate that display.
Use `--verify-timeout SECONDS` to set the finalize verifier wall time (1–86400 seconds; default 1800).
The verifier runs outside the shared result lock; finalization rechecks its
snapshot before writing.
Historical results without the binding remain available in Results.
`--outcome failed` asks for rework, so the task's review and blocked calls stay on
the board; `--outcome rejected` withdraws the work and moves them to Past. A review
whose task continues in a later `--continue` round also moves to Past. After an
authorized landing in another worktree, `agent-plan finish --landed-commit SHA`
can complete the shared task using its exact landing evidence. If the landing
uses a separate clone, opt into `oms land --handoff-receipt-to PATH` with the
shared checkout as PATH. This transfers the successful gate/push/CI receipt and
its range predecessor into that checkout's canonical namespace after fetching
and checking the same destination. For an already-passed receipt, retry with
`--no-update --handoff-receipt-to PATH` to transfer only its existing bytes; the
SHA may be an ancestor of a destination another main advanced. Omitting
`--no-update` retains normal CI/install steps and exact-tip safety checks.
Ordinary receipt lookup stays clone-local;
see [receipt handoff](COMPONENTS.md#plans-and-bounded-autonomy).
To refresh the shared checkout's scripts after a passed land and handoff, add
`--sync-shared PATH` with an explicit checkout path. The separate refresh respects
live-call locks and refuses dirty or diverged trees; deferred/refused sync is
reported in land status without failing the passed land. Its companion sync
receipt preserves the original handed-off landing bytes. Without this flag,
handoff leaves the checkout untouched.

The board offers `[+ Claude main]`, `[+ Pi main]` and `[+ Codex main]` for installed harnesses
when no main is active. A click, or `n` then `1` (Claude), `2` (Pi) or `0` (Codex), starts the
sole main without leaving the current window. While a main is active or its lifecycle is
unverified, the row explains the limit and offers no start targets. Startup is serialized;
an invocation-scoped lease also prevents concurrent inline/Herdr/native main processes.
Parallel tasks use workers, advisors and reviewers under this main. The panel
does not start a second live main.

To start a main in an empty existing panel without a terminal:

```bash
oms panel --repo . --spawn-main claude --task 'short task' --json
oms panel --repo . --launch pi --task 'short task'
```

`--spawn-main` never creates a panel session, attaches a client or moves focus, and returns
`{"window": N, "provider": ..., "owner": ..., "room": ...}`. It refuses inside workers or
when any panel room has a live main. It permits a replacement after terminal evidence
confirms the previous main ended. Unknown window/lifecycle state refuses replacement.

Fresh Claude mains receive a panel-generated UUID via `--session-id`, retained as
the attempt's `native_session` reference. Resumed Claude mains keep `--resume`
and that reference, so restarting keeps the same room participant and unread mail.
After `/clear`, restart and explicit resume follow the participant's current
bound session; the original join boundary and mailbox remain intact.
Codex starts with its existing flags; a recorded native session can be resumed.
After a native main exits in its panel window, the chat pane shows
`r resume/restart here · Enter close window`. Press `r` to relaunch in that pane,
resuming its recorded session or starting fresh when no session is recorded.
Restart does not resend the original `--task`, even when starting a fresh session.
Fresh Codex restarts start a new session even after the previous session's first
hook bound its room consumer; that binding is checked only when resuming a stored session.
The previous attempt must be terminal, and startup rechecks the sole-main limit
and resume ownership under the main-session lock. Another live main or unverified
ownership refuses restart and returns to the prompt. Enter closes the whole
window, including its board watcher, only while the attempt tag and repository
still match and the native pane marker is unset; otherwise the window is preserved.
Close pins the window id and rechecks the repository, attempt and native marker on the tmux
server at deletion. Repository paths containing `#`, commas, colons, braces,
backslashes or control characters preserve the window with a notice because they
cannot be safely embedded in that guard.
Failed ownership queries preserve the window. SIGTERM/SIGHUP
at the prompt restore terminal modes before exiting with 143/129.
Without a terminal, native launch retains its existing exit behavior.

Pi uses the existing `oms harness pi` bridge and requires both Pi 0.80.4 or newer (its `agent_settled` event marks the main idle) and Codex CLI on PATH.
Its default model and effort come from the Codex main preset; `--model` selects an exact
Codex catalog model. Pi owns its tools and UI, while OMS records the inference owner as
`codex` and the harness as `pi`. Pi history is separate: this initial integration refuses
`--resume` until Pi session ownership can be verified. It has no Codex native hook or
context/quota reading, but [`scripts/lib/pi-oms-extension.mjs`](../scripts/lib/pi-oms-extension.mjs)
forwards Pi activity to `hook_state.py` and delivers room mail through its hook context.
At Pi's exit prompt, `r` starts a fresh Pi main; it does not resume Pi history. The launch uses Pi's documented
[`--append-system-prompt` argument](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/cli/args.ts)
to preserve its base instructions and pass the OMS room and sole-main contract.

Only the person ends a main, from its native chat; the native CLI keeps its history where it
already stores it.


## Layout and activity

When the window has room, the board stays on the left, with the native
conversation beside it and a right-side reader for selected calls and results.
The board's bottom dock has Log, Detail, Plan, Messages, Past and Attention tabs,
plus Debate while a debate exists. Needs you is an attention strip above activity. The
dock opens on Log (the recorded session events) and takes every row the activity tree leaves.

### Keys

Keys apply to the focused surface. The board and reader bindings are contextual; `?` shows live board hints for the current surface and width.

| Key | Graph/tree board | Result reader |
| --- | --- | --- |
| Arrow keys | Move through calls or the selected tab's items | Move through result lines |
| `Home` / `End` | First / last item | Start / end of result |
| `PageUp` / `PageDown`, wheel | Page through calls or selected Detail/Plan content; wheel over a graph band (including Goal) scrolls that band | Scroll result |
| `Tab` | Next visible tab | Next reader view |
| `Enter` | Preview a graph call; open a tree call; expand a message/group | Finish search input |
| `Space` | Expand/collapse a selected group or fold a main in the tree | Page down |
| `f` | Open the selected full result; open the recorded Debate result | — |
| `r` / `e` / `d` | — | Raw / Events / Diff view |
| `i` | Toggle pane context headers in a managed panel | Run info view |
| `g` / `t` | Graph / tree view | — |
| `b` / `v` | Toggle attention-only / expand or collapse the board | — |
| `n`, then `1` / `2` / `0` | Start Claude / Pi / Codex when no main is active in a managed panel | `n` next search match |
| `N` | — | Previous search match |
| `s`, then `1` / `2` / `0` | Swap main to Claude / Pi / Codex in a managed panel | — |
| `a` / `w` / `c` | Ask advisor / open selected worker worktree / request selected call cancellation | — |
| `k` / `u` | Capacity / refresh | — |
| `m` | Cycle Work / Overview / Focus in a managed panel | — |
| `[` / `]` / `o` | `[` / `]` scroll the Log tab older / newer while it is active; the wheel scrolls over its body | Previous / next section / outline |
| `/` | — | Search result text |
| `z` | — | Zoom the tmux reader pane when width is under 120 columns or it is already zoomed |
| `Esc` / `q` | `Esc` closes detail or returns; `q` quits an inline watcher, does nothing on a managed board, and detaches from the control menu | Close reader |
| `?` | Show/hide board key help | — |
| `Ctrl+L` | Repaint the board | Repaint the reader |
| `F5` / `F8` / `F9` / `F12` | Control window (when present) / cycle layout / swap chat-board focus / panel-wide key list in tmux | — |
| Shift-drag | Copy terminal selection in a managed panel | — |
| Mouse click | Select a row/tab; graph calls and the main preview their cards | — |

At the native main's exit prompt, `r` resumes/restarts there and Enter closes the window. The terminal/job workspace has its own controls; this table covers only the board and result reader.

A dispatched call settles only after its supervisor proves the whole process tree exited. On macOS and Windows, where that supervisor is unsupported, the call stays pending and the panel prints a manual-clear hint; independently stop every provider descendant before clearing its retained scope.


`--host auto|inline|tmux|herdr` selects the terminal host independently of
the activity view. Auto reuses an existing local POSIX Herdr pane; otherwise
the existing layout selection applies. `--host inline` disables host automation;
`--host tmux` explicitly selects split. Herdr requires the running pane's
`HERDR_ENV`, `HERDR_PANE_ID` and owned `HERDR_SOCKET_PATH`. It is optional,
never installed or started by OMS, and its Windows named-pipe transport is not
implemented here. Ordinary Windows/inline OMS remains available.

In a Herdr repository pane, the menu opens a fresh tab containing the native
OMS launch wrapper and a task panel. It does not type into an existing
agent. The API uses bounded local JSON requests and never retries a mutation
whose outcome is uncertain. Original-chat navigation reconciles active OMS
main ownership and exact pane/terminal/socket identity; reused panes and
unproven sessions are rejected. Numeric footer readings retain their reported
scope and unknown values. A socket error does not stop the native sibling.

The board stays on the left in a landscape window; it moves above the CLI when the
window is too narrow or physically tall (terminal cells are about twice as tall as
wide, so rows × 2 ≥ 1.1 × columns, with at least 41 rows), and returns left when it
is wide enough again (4 columns past the narrow limit and rows × 2 ≤ 0.9 × columns;
between the two a window keeps its current edge) (28
percent of the width or 45 percent of the height the first time, sized from the
terminal that is attaching; optional integer `OMS_PANEL_BOARD_PERCENT` overrides the
landscape default, clamped to 25–35 percent, with invalid values ignored). In Work
mode, a landscape default or scaled record is capped at the smaller of 38 percent
of the window and the width that leaves 120 columns for the CLI; the existing minimum
split rules apply on narrower windows. A new landscape board stays unsplit when
20 board columns, the divider and 40 CLI columns cannot fit; a portrait split needs
20 rows on each side of the divider. A deliberate border drag remains in effect
for that Work window and is recorded there. A call opened from the board shows in a
reader pane above the CLI.
Herdr uses the same left board, starting at the same 28 percent share and honoring
`OMS_PANEL_BOARD_PERCENT`; the tall-window rule is tmux-only and Herdr's placement is unchanged. Later Herdr resizing is not recorded or shared;
`SplitSync` synchronizes the board width or portrait height only in tmux.
In tmux, each window records its board edge and size in its window option
`@oms_panel_split` (`left:<cells>c` or `top:<rows>r`; the older percent form is
converted on first sight). Resizing recomputes the portrait/landscape edge with a
small hysteresis band and scales the recorded size with the window, clamped to keep
at least 20 cells for the board and chat. A flip moves the
existing board pane without moving focus, preserving pane ids; dragging the border sets the size followed
by later resizes. Press `m` on the board or `F8` in tmux to cycle Work, Overview and
Focus. Overview gives the graph about 62 percent of the landscape width without
changing the Work record. Focus zooms the CLI pane; leaving it unzooms and returning
to Work restores its recorded width. SplitSync stays idle while Focus is zoomed and
does not apply the Work record in Overview. Graph rendering falls back to the tree
when the board is too narrow for the graph. The control window and other zoomed
windows are left alone.
A watcher whose OMS sources change re-executes itself once they compile, so
updated boards need no manual restart; an open reader stays. Select `v` in the
task panel to expand its graph; `Esc` or watcher shutdown collapses it. Native input and permission
prompts stay in the original CLI. The room's detailed tree and graph remain
available. A successfully exited native main is labelled `exited`, independently
of a worker result, verifier or owner acceptance.

The graph fits from 76 columns by 16 rows. Advisors/reviewers sit above their
exact main, and its workers sit below, with activity moving on verified parent
connections. The live main, or a selected one, controls this view; arrow/Tab navigation reaches
its calls. Unlinked calls stay unlinked and are available in
the tree (`t`). Model-call usage stays at the top, while taller views show
provider readings, recorded messages or repository tasks. Missing router,
progress, locks or billing data is never invented. Smaller interactive panes
retain tree navigation; passive output shows the list.
Clicking a main requests navigation to its original chat; workers show their
content in the right-side reader (or the board detail area). The remaining chat pane retains the provider's native interactive terminal,
including editing, pasted input and permission prompts. `--launch` opens this
layout in the checkout's panel when tmux is available, while `--host inline
--launch` runs the CLI alone.

The room's default tree uses closed usage, activity and action cards. Wide
usage cards list the retained model-call totals (the quota/context readings are
the pinned usage line); call totals appear once and are not account quotas. A narrow
usage line uses `W` for weekly quota left and `C` for main context used. Activity keeps
main/role branches, wraps task titles, separates repository tasks, and closes
the viewport border even while scrolling. Empty declared scopes are omitted.
The control menu is an inbox. **Needs you** lists up to 12 items, newest first,
each as glyph, who (`Sol 6.1 worker for Opus 5.5`), what and age: failed or
timed-out calls with their title, calls or mains waiting for approval or input,
unanswered room messages that a main sent to `all`, and finished write workers
whose patch has no admission or landing record. Arrow keys or a click select a
row; the wheel moves by lines and page keys by the visible item count, stopping
at either end. Enter (or a click) opens that main's chat or shows the call's result,
and Esc returns. An empty list says `Nothing needs you right now`; a room with no
main shows `Press 1 (Claude) or 2 (Pi) to start the sole main`. The menu keeps
start (`1` Claude, `2` Pi, `0` Codex, `3` Resume, `t` Task), `o` rooms, `9` refresh and
quit/detach; board keys are not repeated. Enter `?` for all other actions in
ordinary scrollable help (chats, results, views, advisor, review, council,
finalize); existing shortcuts still work.
Menu keys act at once, without Enter, on a POSIX terminal with raw input; the
board's key handling is reused. Text prompts (a task title, a provider) stay
line-based inside that prompt only. Elsewhere, such as Windows Git Bash or
redirected input, the menu reads `oms> ` and needs key + Enter. The inbox
refreshes every five seconds. The watcher retains its arrow, mouse and page-key
navigation.
In a managed tmux control pane the quit key is labelled `Detach`; it preserves
the native sessions. In a coupled tmux board, `F9` swaps focus between the
chat and the board and keeps both alive; `q` does nothing there, and Esc or `v`
un-zoom the board only while it is zoomed.
Standalone watchers retain `q` to exit. If a board was explicitly removed,
`oms panel --reopen` restores it without restarting the native CLI. Repeating
the operation preserves an existing board. Inside an OMS window, bare
`oms panel` does the same; the control menu also provides `z` for this action.
Restore accepts only this checkout's front door with a matching repository,
refuses ambiguous/replaced input panes, and does not restart a terminated CLI.

Client detach retains the host's live processes. A Herdr server restart is
different: bare vendor auto-restore is not an OMS resume and cannot prove
fresh OMS ownership. Continue through the OMS wrapper's existing exact-resume
checks after its previous execution is confirmed terminal. An unknown shutdown
remains blocked; pane IDs or a Herdr `done` badge cannot authorize takeover.
This adapter does not change Herdr's global configuration or restore policy.

Outside Herdr, `auto` chooses split on POSIX when tmux is installed, including
`--launch` and runs started inside another tmux session (the client switches to
the panel instead of nesting); otherwise it uses inline. `--host inline` or
`--layout inline` keeps a run in the current terminal. Each checkout has one OMS panel
session, `oms-<project>`: the checkout's directory name lowercased, runs of other
characters turned into `-`, at most 32 characters (`project` when nothing is left). When that
name is held by another checkout or by a session OMS did not create, the panel uses
`oms-<project>-` plus the first 6 hex digits of the SHA-256 of the resolved path, and neither
session is taken over. A live legacy `oms-panel-<12 hex>` session proven for the checkout stays in use
under its name, because its processes carry that name; new panels use the new name. The first `split` creates it with a control window and records the
checkout on the session. A new panel opened without `--launch` also opens one
installed main (Claude, then Pi, then Codex) in its own window with a new room;
`OMS_PANEL_MAIN=pi` selects Pi; an empty value opens no main, and an unknown value opens none with a stderr warning.
Inline and Herdr without `--launch` instead open the control menu, preselecting nonempty
`OMS_PANEL_HARNESS`, otherwise the new-panel main (the selected `OMS_PANEL_MAIN` name
or first installed Claude/Pi/Codex), falling back to Claude. Press the selected harness's
key (`1` Claude, `2` Pi, `0` Codex) to start it.
Reattaching never opens mains again; later launches attach to it (or switch the current
tmux client), and `--launch` adds a native window there instead of opening a
second session. A session with that name recorded for another checkout is
refused. A launch that loses a creation race, or arrives before a just-created
session has recorded its checkout, waits up to two seconds and joins it; the
owner marker is written after the session's room, so a joining launch follows
that room. New mains default to the panel's recorded room while it is open and
has participant/message headroom; otherwise, or when `--room` is given, the
existing room rules apply. Rooms stay bounded and task-scoped; the panel
outlives them. Separate tmux servers are separate scopes. Each native conversation gets
its own sidebar with state refreshed every five seconds. Existing windows and tmux settings
are preserved, with one addition: `F9` selects the other pane
of the window, `F8` cycles layout modes, `F5` opens the control window and `F12` shows
the key list, all without the prefix (F10/F11 belong to terminal menus). tmux key
bindings are server-wide, so the binding acts only in sessions that carry the panel's `@oms_panel_repo` marker,
passes the key through unchanged in every other session, and is not installed when the key is
already bound by the user (a binding OMS installed earlier under the old session-name test is replaced). With default bindings, `Ctrl-b w` picks a window and `Ctrl-b d`
detaches while work continues. Quit in the control pane also detaches. Failed
construction rolls back only the new OMS session or added window. Inline returns to the menu
when the native CLI exits and refreshes then; it has no permanent sidebar.

The board has one MAIN card for its live main, with its team grouped into
ADVISORS, REVIEWERS and WORKERS. Team counts and elapsed time appear with the
main summary; call rows show state, model and task title. Calls remain attached
to their recorded parent, and each row opens its reader. The repository control view shows `NEXT MAIN` as a one-line preset;
that preset never fills missing model/effort fields in a recorded main.
The native main card shows provider, requested model/effort and recorded task
when supplied at launch. Missing fields remain unrecorded.
Tasks entered later inside the native conversation are not intercepted:
its main title stays unrecorded unless supplied at launch, while dispatched child
titles are recorded. The palette uses warm Claude, blue Codex, teal workers and
lavender advisors and reviewers. Role cards show lifecycle state,
model, task title/ID, read/write authority, effort and location. Worktree locations
use a relative unique suffix; repository calls show `repository`. Detached
worker worktrees can be cleaned after completion; their locations remain history.
Task titles have their own bright lines, wrapping to two lines in wide views
and up to three in narrow sidebars. Main task titles wrap too. Internal task IDs
stay in wide metadata rows or Details. Details gives bounded titles separate
wrapped lines, so long titles need not compete with location and purpose.
No absolute home paths or transcripts are added to panel metadata.

Activity is organized under the window's sole live MAIN: ADVISORS / REVIEWERS /
WORKERS, followed by each call and its task title. Main rows can show `? N open` questions and `cN%` context used. When room history contains earlier mains, they fold to their status rows automatically; the selected main and the main owning the selected call stay expanded. The control window renders the Needs you attention inbox; the main board carries the MAIN card and its team counts. Legacy calls without explicit panel ownership
remain unlinked; a generic attempt parent is never promoted into a native-main
relationship. A linked owner whose metadata is unavailable is labelled so.

Press `v` to toggle board expansion between the current overview and an expanded graph; `Esc` or closing the watcher collapses any expansion. In compact layouts, calls retain their model, state and task title. At 80 columns or wider, these share one row with metadata on the
right; narrow views place metadata below the title. Detail expands wrapped
titles and location/effort metadata. The view prioritizes attention calls and reports overflow when calls do not fit. Small panes use compact summaries. Press `b` to
toggle attention-only: waiting input, waiting approval, blocked, review and
orphaned calls, plus retained failed terminal outcomes. Filtered calls are
counted rather than changing their states. Settled calls stay in the Past tab
and are hidden from the board; attention-only empties the Past tab as well. Both controls affect presentation, not the main identity or
dispatch authority. A native window inherits these
settings when opened, and the control sidebar follows subsequent menu changes.
CLI `--view` overrides a watch's window density; `--attention-only` keeps its
filter enabled. These flags apply only to the menu or watch, not model operations.

The usage line has one `Claude` reading and one `Codex` reading. For each provider, the selected window's exact main attempt wins; otherwise the live, then newest, matching main is used. Claude reads its session HUD cache first, then the proven pane footer when the cache is absent/stale and to fill missing fields; Codex uses its native reading or proven pane footer. Missing readings show `--`. Claude's existing session HUD cache supplies reported context and optional
`seven_day` usage, with a ten-minute TTL and expired reset values suppressed.
The consumer hash selects that session's cache; no newest-session guess is
made. A proven existing tmux pane can supply numeric fields from its last
eight visible native footer lines. `TUI` or `HUD/TUI` labels those mirrored
readings; native output may lag while idle and changed footer formats become
unknown. The watcher performs no native transcript/registry scan or account
request for these readings. Header collection runs once per five-second
snapshot, never per animation frame. Ended mains have unknown current context.

The separate `USAGE` area groups reported tokens by model using only the existing
recent eight repository artifact records. It stays repository-wide when the
activity view is scoped or filtered. A short wide pane uses a single usage
strip; taller panes use a small card, with reported USD cost when height is at
least 40 rows. `x` counts retained call records, not retries or native turns.
`(sel)` identifies a selected model whose served identity was not reported;
served identities and selected identities are separate groups. Mixed/fallback
operations stay in `Mixed models`, and unknown models stay unknown. `?` means
unreported; `+?` means a known subtotal plus unreported records, while a reported
zero stays zero. Overflow reports omitted model groups. Owner results, review
gate summaries and council syntheses are excluded, and lifecycle usage is not
added again. These are provider-reported partial observations, not account
totals or remaining quota; they do not supply the provider header. Provider token definitions can
differ, particularly for cache input; no cross-model total is shown. The window
can change or shrink as new artifact records arrive.

On a capable terminal the sidebar animates live-state indicators
at four frames per second using the cached snapshot. Borders and role dividers
stay still. Only changed
rows are redrawn; resizing triggers a complete repaint. Starting, working,
verifying and live-marker records can move; blocked/waiting/review and terminal
states stay still. The motion means recorded activity, not token streaming,
progress percentage or a new dispatch event. The native main's explicit child
links determine the call tree. Rendering frames makes no state writes or model
calls; state collection remains every five seconds, plus collection time.
Interactive watch collects state and quota readings in one background read
at a time. It keeps the last frame available for input while a read is pending,
and applies completed reads within a 50 ms polling interval. Redirected or
noninteractive watch retains synchronous snapshots and `--count` semantics.
Snapshots that change the hit map discard stale coordinate clicks; navigation still validates
the current room, participant and terminal before focusing it. A proven terminal
destination does not require scanning unrelated native chat histories; app
navigation and the full chat catalog keep their bounded identity lookup.
`--count` counts collected snapshots rather than animation frames.

Use `oms panel --watch --no-animation` for a motion-free view, or set
`OMS_PANEL_NO_ANIMATION=1` in the launching environment to cover automatically
opened sidebars. Redirected output and `TERM=dumb` are static plain snapshots.
`NO_COLOR` controls color independently. `OMS_PANEL_COLOR=always` enables color
only in the OMS panel even when its environment inherits `NO_COLOR`;
`OMS_PANEL_COLOR=never` disables it, and `auto` preserves the default.
`OMS_PANEL_THEME=light` (or a light `COLORFGBG` background) uses darker shades for light
terminal themes; `dark` forces the default shades. Redirected
output and `TERM=dumb` stay plain in every mode. The watch restores the terminal cursor
on exit or interruption. The control menu redraws in place on keys and every five
seconds rather than animating.

A native window binds children by its recorded main attempt ID. Earlier mains'
calls stay in the room's tree. The control view shows
repository-wide records. Control option `4 Main` chooses the main harness for
subsequent manual dispatch; Pi uses the Codex dispatch owner. Main/child links come from explicit
lifecycle metadata, never matching a provider name. Legacy unlinked records stay
repository-wide. A marker and its attempt are coalesced only by their exact
execution location. Selected/served attribution uses exact attempt IDs; a model
name alone is not proof the provider served it.

Active attempts have a separate bounded window so newer outcomes cannot hide
them. Overflow is reported; Details retains the bounded active and recent
records. `REPO ACCEPT` and repo attention belong to shared repository state,
including earlier work. Recent outcomes and successful native process exit do
not establish acceptance of a task. Native subagents that do not write OMS
records remain unobserved.

| Menu action | Behavior |
| --- | --- |
| 1 Claude / 2 Pi / 0 Codex | Start the sole main: native Opus / Pi with Codex inference / native Sol |
| 3 Resume | Select Claude or Codex and an exact verified native session ID |
| 4 Main | Choose the main harness; Pi uses the Codex dispatch owner |
| t Auto task | Enter a task and open the owning native main |
| 5 Explain | Read-only worker; optional tier, else routine |
| 6 Implement | Optional tier; scoped brief and verifier required |
| a Advisor | Optional tier, else frontier; read-only |
| 7 Review | Optional tier, else frontier; mandatory review verifier |
| c Council | Opus, Sol, Sonnet and Luna by default; `--workload frontier` swaps Sonnet and Luna for Fable and Astra |
| r Results | Numbered result list; choose a number to read summaries, evidence and model answers |
| f Finalize | Record owner outcome, summary and evidence; request app delivery |
| n Retry delivery | Explicitly retry a saved result; an uncertain earlier send may duplicate |
| 8 Dashboard / 9 Refresh | Read shared dashboard; no model call |
| v Detail / b Attention | Cycle auto / compact / detail; toggle action-needed calls |
| h Chats / o Rooms | Browse the room's chats; list rooms and switch or create one |
| p CLIs / z Panel | Show provider CLIs and pick a target; reopen the panel session |
| q Quit | Exit inline or detach the owned split session |

`NO_COLOR` disables color unless the panel explicitly selects
`OMS_PANEL_COLOR=always`; `TERM=dumb` uses ASCII without screen clearing. The
view fits the current pane width/height. Redirected watch output appends plain
snapshots; `--count N` bounds it. Native launch needs an installed, authenticated
provider. `installed` means executable found on PATH, not verified login or model
availability. Shared OMS references carry evidence across providers; native
conversations remain separate. Use isolated workers for implementation.


## Roles and automatic allocation

The native main owns the task, architecture, workload judgment, final synthesis
and acceptance. It receives instructions to inspect source and dispatch bounded
subtasks through `oms panel --dispatch`. There is no additional classifier call
or prompt keyword router. Selecting Auto task (`t`) supplies the task as a
native user turn; when entering work in the native conversation, the same
instructions apply. Model selection for each bounded call is part of that task.
The launcher does not autonomously fan out before the main has a task.

| Main harness | Exact model | Effort |
| --- | --- | --- |
| Claude main (default) | `claude-opus-5-5` | high |
| Codex main / Pi | `gpt-6.1-sol` (shown as Sol 6.1) | high |

Routing is provider-neutral within the configured light, routine, deep and frontier capability tiers. It filters to installed providers and removes candidates reporting at least 90% weekly usage when multiple providers are available; a sole installed provider remains eligible regardless of usage. With readings for all remaining candidates, more weekly quota left goes first; when any reading is missing, routing spreads work away from the main's provider. A tie follows configured tier order. Weekly quota comes from the provider's available local reading; Codex uses `rate_limits` records in its local CLI session logs. Missing, stale or malformed readings stay unknown. Reviewer, verifier, security-auditor and critic prefer a provider other than the reviewed patch's writer (an automatic review whose patch links to one write attempt) or else the main's, when one remains. Researchers are pinned to the light tier at low effort. Role defaults are shown below. A write session can continue across write-capable roles with `--continue`; judges use fresh context.

| Tier | Configured model candidates (effort) |
| --- | --- |
| light | `claude-haiku-5-5` (low), `gpt-6-luna` (low) |
| routine | `claude-sonnet-5-5` (medium), `gpt-6-luna` (medium) |
| deep | `claude-opus-5-5` (high), `gpt-6.1-sol` (high) |
| frontier | `claude-fable-5-1` (high), `gpt-6-astra` (high) |

| Role | Default tier | Access | Front door | Runs |
| --- | --- | --- | --- | --- |
| `researcher` | light | read | `oms panel --dispatch researcher` | `peer-delegate --role researcher` |
| `worker` | routine | read or scoped write | `oms panel --dispatch worker` | `peer-delegate --role` |
| `builder` | routine | read or scoped write | `oms panel --dispatch builder` | `peer-delegate --role` |
| `refactorer` | routine | read or scoped write | `oms panel --dispatch refactorer` | `peer-delegate --role` |
| `documenter` | routine | read or scoped write | `oms panel --dispatch documenter` | `peer-delegate --role` |
| `release-engineer` | routine | read | `oms panel --dispatch release-engineer` | `peer-delegate --role release-engineer` |
| `integrator` | deep | read or scoped write | `oms panel --dispatch integrator` | `peer-delegate --role` |
| `performance-engineer` | deep | read or scoped write | `oms panel --dispatch performance-engineer` | `peer-delegate --role` |
| `reviewer` | frontier | read | `oms panel --dispatch reviewer` | `peer-review` |
| `verifier` | frontier | read | `oms panel --dispatch verifier` | `peer-delegate --role --read-only` |
| `security-auditor` | frontier | read | `oms panel --dispatch security-auditor` | `peer-delegate --role --read-only` |
| `advisor` | frontier | read | `oms panel --dispatch advisor` | `advise` |
| `architect` | frontier | read | `oms panel --dispatch architect` | `peer-delegate --role --read-only` |
| `critic` | frontier | read | `oms panel --dispatch critic` | `peer-delegate --role --read-only` |

The plain `worker` role uses the `analysis-worker` strategy for reads and
`implementation-worker` for writes. Other roles use their role name as the
strategy. Write access requires `--access write`, a scoped brief and a verifier;
read access is the default. The same role routes are available without opening
the panel; see [Without the panel](#without-the-panel).

`--workload light|routine|deep|frontier` overrides the tier for one dispatch and
`--reasoning-effort` overrides one dispatch's effort. Any panel dispatch with `--verify` gets `OMS_PEER_VERIFY_TIMEOUT=20m` when unset; peer commands elsewhere default to 10m. Set the variable in the environment to override it. A write attempt keeps its heartbeat while verification runs, then stops it within about two seconds; if reconciliation left that attempt blocked as `heartbeat_expired`, a passing verify resumes only that exact block (same attempt, sequence and reason, checked under the ledger lock) to review. Operator blocks, cancellation, timeout and failed verification keep their outcome. The verify command runs without the worker's `OMS_PANEL_*`, `OMS_ROOM_*`, `OMS_MODEL_*`, `OMS_REASONING_*`, `OMS_INDEX_*` and state/attempt/lease variables, while keeping `OMS_HARNESS_*` as the recursion guard; nested suites therefore reach the top-level verdict without recording into the live room. Use `--to` and `--model` only when a specific provider is explicitly required; `--model` requires `--to`, and an exact route does not silently fall back. Tier routes use the configured model as a floor and pin the newest catalog version in that same provider family by numeric version (an omitted minor version counts as zero). Availability comes from installed/cached provider data; no paid model probe runs, and a chosen model failure does not substitute an older or more expensive family. `--workload` cannot be combined with explicit `--to`. Independence ordering applies to reviewer, verifier, security-auditor and critic when both providers remain available. Their records keep the board kinds `worker`, `researcher`,
`advisor` (advisor, architect, critic) and `reviewer` (reviewer, verifier,
security-auditor) in `panel_role`; the strategy itself is the recorded purpose
(plain workers record explain/investigate/implement). `oms panel --routes`
prints each role's tier and chosen candidate, plus
these kinds and the write-capable roles. Dispatch and council owner defaults use
`OMS_AGENT`, then Claude's `CLAUDECODE`/`CLAUDE_CODE_ENTRYPOINT` markers, then
Codex's `CODEX_SANDBOX` marker, otherwise Codex; `--owner` overrides detection.

[The panel policy](../config/panel-routing.json) is separate from generic OMS
provider defaults. An explicit native `--model` override preserves native effort
settings. Advisor (which direction?), architect (how to structure?) and critic
(when does it fail?) are separate judgments; the bootstrap does not ask the main
to call them together. A missing provider or unsupported model fails that route
without silently selecting another model.

A panel main acts as the control tower: its bootstrap delegates implementation,
investigation and test repair to bounded workers by default and keeps scope,
briefs, integration, review, patch admission and coordination.
Wait on each dispatched call separately, with one notification per call; review,
admit or re-dispatch it as soon as it finishes, never batch-wait for a wave.
Keep configured parallelism full: start the next ready plan task or follow-up
round in the same turn. Send remaining review findings to a worker with
`--continue`.
It edits directly only for admitted-patch integration, conflict resolution, or
a fix of at most two lines; everything else goes to a worker even when the brief
feels longer. Outside the panel, the global rule still requires an
explicit request before spawning subagents.

The sole panel main also lands its own work. `oms scratch-worktree add` starts a detached
worktree; create a local, nontracking task branch there with
`git checkout --no-track -b oms/<task>`, then admit worker patches and rebase on
the remote target. No upstream is needed. After verification, commit and run
`oms land` from that worktree only when commit and publication are authorized;
`oms land` pushes the verified SHA to its target. Nobody commits in the shared
checkout. Mains in sessions outside this panel may land independently:
a concurrent `oms land` exits 75 if the lock stays held past `OMS_LOCK_TIMEOUT` (default 300s); retry after the active land finishes.
Worker guard stop diagnostics identify the snapshot, path and kind that caused
the stop. The worker guard treats `oms/*` branches and harness worktrees belonging to
mains in other sessions as soft, and `oms room scope` warns about overlapping files before the work
starts.

Task complexity and write authority are independent. A write role needs a
scoped brief file and mechanical verifier. Read workers
use isolated worktrees too. Write workers return a patch for parent inspection
and admission; the panel does not apply it. Reviewers require a verifier and
record a review gate. Other read roles run an explicit `--verify` in their read-only delegate; an advisor refuses one (dispatch a reviewer or verifier to run checks) and a researcher takes no gate. Workers cannot recursively dispatch or open owner sessions.
An empty write patch from a panel dispatch or plan task exits 6 (`no changes`).
Normal native permissions, harness policy and account consent remain in effect.

```bash
oms panel --repo . --dispatch worker --owner claude \
  --purpose explain --task-id parser-notes --label 'Explain parser' \
  --prompt 'Explain the parser using source evidence.' --dry-run
oms panel --repo . --dispatch worker --owner claude --workload light \
  --purpose investigate --prompt 'Identify the relevant configuration entries.'
oms panel --repo . --dispatch builder --owner claude --access write \
  --brief-file scoped-task.md --verify 'bash scripts/check.sh --affected'
oms panel --repo . --dispatch architect --owner claude \
  --label 'Structure the parser' --prompt 'Compare these bounded design options.'
oms panel --repo . --dispatch security-auditor --owner claude \
  --prompt 'Audit the install script change for privilege and secret handling.'
oms panel --repo . --dispatch reviewer --owner claude --workload deep \
  --prompt 'Review the changed parser.' --verify 'bash scripts/check.sh --affected'
oms panel --repo . --room team --dispatch advisor --owner claude --main opus-main \
  --prompt 'Advise on the parser plan.'
```

From the control window, a dispatch belongs to the room's active main of that
provider. `--main PARTICIPANT` asserts that caller's identity (the board's `a` uses it); a room with multiple live mains refuses every dispatch until the others are released with `oms panel --release-main` or the swap is finished. A native main always dispatches as itself.

A `--task-id` that names a shared-plan task moves it for the calling main: a
write dispatch claims and starts a ready task (a read-only one leaves it ready), a claimed one is started, and later workers
of the same main reuse the running lease. A task another provider or main holds,
or one already in review or done, is left unchanged with a warning; `--dry-run`
never moves it. Stderr names the next verbs, `oms agent-plan review` after
acceptance and `finish --landed-commit SHA` after `oms land`. The panel never
passes `--plan-task`, whose per-worker transitions assume one worker per task.
After a call's exit is verified, dispatch prints the next dependency-ready plan
task ID with `oms agent-plan claim --id ID --provider PROVIDER` and
`oms panel --dispatch ROLE --owner PROVIDER --task-id ID` hints when one exists.
These hints leave claiming and dispatch to the main.

Continue reviewed work with `--continue TASK_ID`: `oms panel --repo . --dispatch refactorer --owner claude
--continue TASK_ID --access write --brief-file next.md --verify COMMAND` takes the same role,
workload and access checks; any write-capable role may continue (a builder session can
become a refactorer), while reviewer, verifier, security-auditor, critic and the other
read-only roles never resume. It finds the latest worker of that task owned by this
main, and starts a new run in a new worktree at the current HEAD that resumes
the worker's native session (recorded as `refs.native_session` in the attempt,
never in room text). A continuation transfers native conversation history only when
that provider session is still available. Files and the old worktree never transfer.
Only an exact exit-zero result contributes its verified patch inline, plus a bounded
recorded summary for a fresh session. Stderr names what actually transferred.
An oversized patch refuses continuation instead of pointing at artifacts absent
from the new worktree. Inspect and admit that patch in the owner checkout first,
or include a bounded recovery in a fresh dispatch brief.
Stopped writes with a recorded worktree must first go through
`oms panel --salvage TASK_ID --attempt ATTEMPT_ID`; inspect the recovery and include
the recovered work in a fresh dispatch brief. A timeout result card may still offer
the general continuation hint; the CLI enforces this salvage prerequisite for writes.
A continued implementation needs `--access write` (the default is read-only); later rounds of one task show "round N". Unless the
environment sets them, write calls get a 40m wall (60m at high effort) and any
call with `--verify` gets a 20m verify wall, enough for the repository smoke suites.

Give each dispatched subtask a descriptive task ID and short `--label`; reuse
the exact reviewed plan task ID where one exists. Labels are optional for direct
calls: an omitted label displays the typed purpose. Prompts and briefs are at
most 64 KiB. Labels are bounded and checked for sensitive-looking content before
recording; use a descriptive title. A native task that cannot provide a safe
display title still reaches the native main, with an unrecorded title in status.
The `t Auto task` menu follows the same rule. Participant labels are bounded to
120 characters while the original native task text is preserved.
`--dry-run` prints the selected route and
arguments, starts no process and writes no state. Its arguments can contain the
supplied task, so keep the launch plan private when that task is private.

### Without the panel

The same role and tier routing works from any Claude Code, Codex or Pi session
without a panel: `oms panel --repo . --dispatch ROLE --prompt TEXT` (or
`--access write --brief-file PATH --verify COMMAND` for a write role). The caller
is detected from `OMS_AGENT`, else Claude Code's or Codex's own environment, so
reviewer independence still prefers the other provider. Without a room the call
is recorded as a standalone result: read it with `oms panel --results --task-id
ID`. `--dry-run` prints the chosen route and argv without running a model.


## Discussion and results

The council uses the existing `peer-ask` debate, thread, artifact and lifecycle
front doors. Its four seats cover both families: the deep pair (Opus, Sol) plus the
routine pair (Sonnet, Luna) by default, or plus the frontier pair (Fable, Astra) with
`--workload frontier`. One rebuttal round means eight primary seat calls; two
means twelve. Format repair can add one call per
primary call. `--dry-run` exposes both counts without executing a model. Every
selected seat must complete to report council success; failed seats remain
visible and the owner still decides the outcome. All seats are read-only. There
is no model fallback or additional synthesis-model call.

The board's bottom is one box whose top edge is a tab strip: Log, Detail, Plan,
Messages, Past and Attention, which stay even when empty, plus Debate while a debate exists. The selected tab is bracketed; a `•` after Debate marks a
debate running or finished since the tab was last opened, a number after
Messages counts the unread mail for live participants and one after Past counts its calls. A click on a tab name or
Tab (through the visible tabs in that order, then around) switches; a tab never changes
by itself, and the box stays after Esc. The box is reserved before the upper
layout is drawn, and it takes every row the tree and warnings leave. It persists when empty or dismissed.
On supported ordinary layouts (80x40, 157x60 and 126x90), its bounds stay fixed
as calls come and go. Physically short terminals adapt the readable body, strip,
or tree fallback to available space. The tabs stay clickable when only the strip fits.

Messages shows up to 200 retained messages, with sender, recipient, unread state and message kind. Left/Right cycles the filters in this order: needs you, by call, all, then this call when a call is selected. To/from me is offered only when the room has multiple recorded mains. Up/Down selects; Enter expands a message, and a second Enter on an expanded row or `f` opens its associated call in the right reader. Reading does not acknowledge messages. The unread badge and message tags count pending messages for live readers; complete projections are exact and truncated projections show a lower bound with `+`.

Detail is the selected call's persistent, reserved reader; it remains present
when dismissed or when there is no selection. Past lists every settled call of the room's mains
and of calls with no known main, newest first, one row each (role and model, task, how it ended,
end time). Up/Down pick a row; Enter or a click shows it in Detail exactly as a board card would,
and `f` opens its full result on the right. Plan shows the shared repository plan:
the goal, then one row per task (state glyph, id, title, claimant by the board's
main name or provider, verify command) ordered verified, review, running or
claimed, ready, blocked. Debate shows the shown main's debates, newest first
(Left/Right cycles when there are several): title with the opener's board name
and local time, seats answered and round, the question (at most three lines),
one line per seat with its VERDICT or first Answer sentence (sentences end only
at `.`, `!` or `?` followed by a space, never inside backticks or quotes; the
line is cut at about 140 columns), `(changed in round 2: (c) -> (a))` only when
the seat's choice token such as (a), A, yes/no or proceed/revise differs between
rounds, Agreement/Disagreement only when the synthesis has such headings, and
the owner's recorded decision. Up/Down moves the seat cursor, Enter shows that
seat's retained answer in the tab. If only part of that round's source answer
was retained, the tab shows a truncation notice and the recorded source artifact
path. Esc returns, and `f` opens the full recorded result reader. Evidence is
read in the background.

Messages lists the room's last 200 messages, oldest first, one row each with
local time (date when from another day), sender and recipient by the board's
main names, an `unread` mark while a recipient has not consumed it, and the
question/answer/handoff/status kind. Left/Right cycles the filter (all,
to/from the selected main, to/from this window's main), Up/Down selects, Enter
or a click expands the message in place to its full wrapped text and again
collapses it. Reading never acknowledges a message; the full log is read in the
background and re-read only when the room's message counts change.

The owning main records a result through `--finalize`. Native process exit and
worker completion never create accepted results by themselves. `completed`
records an owner summary; `accepted` additionally requires a passing explicit
`--verify`, at least one `--evidence` reference, and matching landed patch
evidence for every recorded write worker of that task and every write worker
without a task ID under the finalizing native main; workers assigned to other
tasks are checked when their own task is accepted. A plan-bound patch retains its
exact plan lineage; a standalone patch matches its successful landing by digest.
Verifier output stays local. Evidence references bind repository-relative files
of at most 1 MiB each to SHA-256. The summary is
bounded to 8 KiB, rejects sensitive-looking text and private absolute paths,
and is cleaned of terminal controls. New result revisions are immutable artifacts
registered in the existing artifact index, rather than a second task database.
Finalization checks the full lifecycle projection, independently of the display
window. Missing retained patch or landing evidence refuses acceptance. A failed,
abandoned or rejected write worker under the same main also refuses `accepted`;
the owner can record `completed` or `failed` with its actual summary instead.

After saving the immutable result, finalize closes stopped `review` workers of
exactly that task and the owner's lineage: `completed` → `done`
(`owner_completed`), `accepted` → `done` (`owner_accepted`), and `rejected` →
`cancelled` (`owner_rejected`). `failed` keeps `review` for rework. Running
workers and unknown exits remain pending; settlement errors are reported without
undoing the result. Lifecycle `done` grants no acceptance: `completed` retains
plan review/acceptance, and acceptance derives only from the `accepted` outcome.
The result must still match the current plan task generation. Workers created
after the result revision remain pending as `newer_than_decision`.

`oms panel --settle-review` lists eligible finalized-task leftovers without
writing, including task, age in review, selection reason and target state.
Add `--apply` to close them. Repeat `--attempt ID` with `--reason TEXT` to
explicitly cancel stopped review workers as `owner_superseded`, recording the
reason. The default scope is the calling main's lineage. `--owner-attempt ID`
selects a named dead main's lineage. Unknown ownership and other live mains are
refused. Compare-and-set transitions and per-attempt revision keys make retries
safe after partial settlement. No worktree, patch or artifact is deleted.

For legacy terminal mains with no host/native ownership refs, explicitly add
`--legacy` to `--owner-attempt ID`. Every legacy main in that participant's chain
must have its last lifecycle event older than 24 hours; override this threshold
with `--legacy-older-than HOURS` (finite and positive). Selected workers report
`selection_reason: legacy_terminal_main` and `legacy_older_than_hours`. Mains
with any host/native ownership refs still require the normal absence proofs;
age never substitutes for those proofs. Finalized-result and explicit `--attempt`
selection otherwise remain unchanged.

`oms panel --settle-review --standalone --older-than DAYS` selects parentless
workers in review, without requiring a main. The finite positive threshold
measures time since entering review. Only matching indexed legacy artifacts
whose Exit section proves termination allow cancellation as `owner_superseded`,
with reason `aged standalone review`. Recent, unknown-age or unproven-exit
attempts remain pending. This mode cannot be combined with owner or explicit
attempt selection. Both recovery modes are dry runs until `--apply` is added
and use the same result lock and expected-review-state transitions.

For an undecided backlog, explicitly use `--aged-days N` in the calling main's
lineage, a named dead main's lineage, or with `--standalone`. There is no
default; N must be finite and at least 7. This selects review workers, and in a
main's lineage also blocked workers, whose last transition into their current
state was more than N days ago (`--standalone` selects review workers only), even without a finalized result or indexed exit artifact. Blocked
workers are cancelled with `reason_code: owner_superseded` and reason
`aged blocked without owner decision (N days)`; review workers use
`aged review without owner decision (N days)`. Unknown ages and recent workers
remain pending. The calling-main scope never selects newer workers. Named-main
recovery still requires its existing dead-main and host/native absence proofs.
Parentless attempts with room ownership refs remain pending. Waiting-input and
waiting-approval workers are not selected. This option cannot be combined with
`--older-than`, `--attempt` or `--reason`. Preview first; `--apply` uses the same
result lock and lifecycle-sequence compare-and-set transitions, without deleting artifacts.
Settlement also compares the selected attempt's lifecycle sequence under the
lifecycle lock. Leaving review and re-entering it before apply invalidates the
selection, even when the state name is again `review`; retry rechecks its age.

```bash
oms panel --finalize --owner codex --task-id ID \
  --summary-file SUMMARY.txt --outcome accepted \
  --verify 'test -f RESULT.txt' --evidence RESULT.txt --notify
oms panel --results --task-id ID
oms panel --retry-delivery --task-id ID
```

Results lists the latest twenty retained tasks from at most 300 lifecycle
attempts and 1000 artifact events. Selecting a task shows up to eight recent
call records. Participant browsing selects that participant's tasks before
applying the recent-20-task limit. An explicit query outside retained evidence
reports an incomplete window. Bounded normalized answer sections retain up to
32 KiB per answer; raw prompts, provider
logs and native conversation histories are not exported. Missing, changed or
linked evidence is reported as unavailable. Oversized answers show a preview
notice and their source artifact reference. The reader retains answer lines
and reflows them when resized; Up/Down moves one line and Home/End goes to the
first/last page. Page keys and the wheel expose the complete retained preview.
The registered result and source artifacts retain their full bounded content.
The menu's numbered list shows titles, outcome, verifier and app status before
loading any normalized answers. Select a number to open that task's detail and
return to the list afterward. Browsing performs read-only queries; it does not
dispatch, accept a patch or retry app delivery. CLI `--results --task-id ID`
continues to expose the same detail directly.

`--notify` sends a cleaned summary to the existing fixed Codex app receiver.
The local app must be available and that receiver must exist, be unarchived and
idle. Delivery never creates or replaces it. A persisted receipt requires the
matching thread, turn, command item, successful command completion and stored
output. It proves stored app output; OS popup visibility remains unobserved.
Missing, busy or archived receivers, timeouts and protocol failures are recorded
separately from the work outcome. `OMS_CODEX_NOTIFY=0` disables delivery.

Delivery intent and final receipt are separately indexed immutable artifacts.
Repeated finalization of the same revision does not resend. An interrupted
send remains pending or uncertain until the owner inspects it and explicitly
retries. Retry uses the saved result even if the original summary file is gone;
a persisted revision is never resent. Native sessions launched by the panel
receive `OMS_PANEL_RESULTS=1`, so the older Claude Stop notification does not
duplicate a finalized product notification. A resumed older session retains
its prior bootstrap; use the current front doors explicitly for new features.


## Shared work rooms and the graph

Boards for one repository share atomic dashboard and result-query caches in
`.oms/hooks/panel-cache`, invalidated by input file stats or five-second expiry;
room selection and terminal reads stay per window. A read that races a state
change collects directly instead of failing. When refresh still fails, the
board or control window keeps its last good state and shows
`State refresh failed; showing data from Ns ago`.

If the first read fails, or a window switches to a room it has not successfully
read, the panel reports that room state is unavailable and its work and messages
are unknown. It does not show empty-work counts. The control menu remains
available for retry and navigation; a later successful read restores the board.

A bound live Codex main shows its current model and effort from its exact native
session's read-only state, matched to this repository. A missing or unreadable
native state (including WAL state without its reader files) shows the current
model as unavailable; the launch setting stays
separate in its detail. Historical Sol 6 calls retain their original identity,
while current Sol defaults and Sol 6.1 calls are labelled accordingly. GPT
labels include their model version (Sol 6, Luna 6, Astra 6 and Sol 6.1). Native
chat rows are never updated; SQLite may coordinate its existing reader locks.

Expanded main details list calls as Needs attention, Active, No record and Finished,
then recent messages and the work location; every team row opens that call's
recorded result, and status-unavailable calls keep separate identities and result
links. Result details put the main's decision
and verification before the worker's answer. Long excerpts remain scrollable and
point to the full retained result rather than filling the board with raw output.
Press `f` in a call preview (or click or Enter on the call) to open that complete
scrollable reader. The reader revalidates the exact recorded target.

A native main launched from the menu joins the selected work room. Starting a replacement
main from the menu or with `--launch` keeps the panel's recorded room while it is open and has headroom; a full or closed room makes the next
main start a new room, and an explicit `--room ID` or `o Rooms` choice wins.
A bare control window shows the panel's recorded room.
Role dispatch refuses a full room before enrollment, checking headroom for assignment and result
messages. A main has no cap on concurrent calls: split separable work into as many parallel calls as it
Interruptions give the child up to 20 seconds to clean up, settle unfinished attempts and send a numeric
result handoff. Sensitive summaries are withheld with an `oms panel --results --task-id` pointer;
room races can still prevent delivery, while the indexed evidence remains available.
Each reviewer result sends one addressed handoff containing the judgment and a
findings pointer. A passing automatic review also prepares an editable completion
summary and an owner finalization command; admission and acceptance remain explicit.
Room hook delivery still requires explicit `oms room ack` after consumption.
Acknowledgment reminders are at most once per five minutes per session and are
omitted when that hook delivers new mail.
The `o Rooms` menu selects or creates a task-scoped room. It does not enroll
other sessions merely because they share a repository. Auto uses the connected tree when the board is at least 20 columns wide and 12 rows high. Every
joined main of the room, the live one first, has rows; empty role cards are omitted. Explicit parent IDs
connect advisors, reviewers and workers under their own main. Calls whose
parent is outside the room's mains stay in `UNLINKED`. Scrolling exposes rows
outside the viewport. `--view tree` selects this layout directly. `g Graph`
retains the card graph at 76 columns and 16 rows. When the graph cannot fit or collection warnings are present, a board at least 20 columns wide and 8 rows high uses the tree for chat/result navigation and warnings. Passive
output retains its list fallback. `--view compact|detail` keeps the list at any size.

On a POSIX terminal, the board watcher accepts clicks, arrow keys/Tab and
Enter. Clicking a main focuses its proven existing tmux conversation pane;
if no terminal exists, a proven exact Codex app link can open its original chat.
An unresolved or departed participant shows a reason. These clicks do not
start or resume CLI sessions, send a prompt or call a model. In the graph, the
click (or Enter) on a worker, advisor, reviewer, researcher or council seat
keeps a short summary in the board's detail area and opens the call's full
retained result (answer, review state, changed files) in a reader pane on the
right: the native chat pane is split, the reader on top (about 62 percent of
its height) and the chat below, and the board keeps its width. Another call
reuses that reader. `q` or Esc in the reader, or Esc on the board, closes only
the reader, and focus returns to the board; selecting a main closes it and
shows the chat. The reader is started as `oms panel --results --participant ID
[--task-id ID]` with identifiers revalidated from recorded state, closes itself
when its board pane goes away, and is never discovered as a board or an input
pane by `--reopen`. When the reader would get fewer than 8 rows, and outside a
managed tmux main window (inline layout, Herdr, a standalone `--watch`, the
control window), the call's content stays in the board's detail area as before.
A main's detail lists its whole team, finished calls included, and clicking a
team row opens that call the same way, as does Enter on a call in the tree. Repository tasks show the
displayed snapshot's title, dependencies and declared paths. Escape closes the
detail or a results screen. Click a main's left chevron or press Space to fold its calls;
the wheel and PageUp/PageDown scroll. `t`, `g`, and `b` select tree, graph and
attention views in the watcher; `Home`/`End` jump to the first/last item and keys
the board does not bind are ignored. `q` quits a standalone watcher and does
nothing on a managed tmux board (use `F9`). Resize discards pending coordinate
clicks; a refresh discards them only when it changes the drawn hit map.
The selection is shown before a chat or result opens.

Mouse support is enabled only in the OMS tmux session, never globally. To copy
text, hold Shift while dragging for the terminal's own selection (emulators
differ), or use tmux copy-mode (`prefix [`). Clicking plain Detail text, or a
full result's text, neither navigates nor repaints; nothing is copied to the
OS clipboard automatically. The watcher restores mouse
reporting, echo, canonical input and the cursor on exit, interrupt and
HUP/TERM; the OMS session's tmux `mouse` option is set best effort (the watcher keeps
keyboard input if tmux refuses) and stays on afterwards, so it also applies to the
session's native chat windows. Redirected
output stays plain and passive. Unsupported terminals
retain the existing control-menu chat/result selection.

The board shows one live main per panel. In graph view its order is the header
and legend, the wrapping Goal box, attention strip, judges (reviewer, verifier,
security-auditor, critic, advisor and architect), a dashed MAIN card, writer
calls below the main, the dock (opening on the session log) and footer. The tree groups calls under
their recorded role and parent. Calls appear as one-line rows; selecting one
expands it into a dashed card. Status tags are bracketed and role colours
distinguish judges, the main and writers. A finished write call awaiting patch
admission is tagged `patch awaits admission`.

Click or Enter on a graph call selects its preview; Enter in the tree opens the
call. Group drill-down is available in the tree. The right reader shows the
selected call in `Summary`, `Raw`, `Events`, `Diff` and `Run info` views
(Tab changes views; `r`, `e`, `d` and `i` select Raw, Events, Diff and Run
info). A selected call's reader is revalidated against its recorded result.
Clicking the main previews its card. Child connections use explicit parent
IDs. Lifecycle state drives activity marks; joining without lifecycle evidence
shows `presence unknown`. Missing or old call records do not establish
acceptance. Unknown calls retain their unknown status, and unhandled failures,
blocked and waiting calls remain visible. Actions only change the display;
they do not change room records, acknowledgments or lifecycle states.

The graph pins its header, legend, Goal box and attention strip above the call
layout. The full goal wraps instead of being clipped, and its counts summarize
the plan. When it exceeds the visible space, scroll over the Goal box to move through
its lines; the Plan tab also shows the full goal with plan details and supports
scrolling. The tree pins the header, usage line and goal preview. Small
boards retain selection for the main and at least one call.

The usage line always covers both providers (`Claude week 66% · ctx 73% │ Codex --`); a provider without a
reading reads `Codex --`, whichever main the window belongs to. The goal box is a card titled `GOAL` with the
full wrapped plan goal, with progress right-aligned on the title row
(`╭─ GOAL ──── ▕██████░░░░▏ 6/9 verified · 1 in review · 2 claimed ─╮`; ASCII `+- GOAL` and `[######....]`).
Cancelled tasks are closed, never verified, so they leave the total (the `[oms plan]` sentence uses the
same total). A running task whose recorded calls all ended counts as `stale` instead of `claimed`; one whose
calls wait for its main's decision stays `claimed` (the main card and Plan tab mark it `awaiting decision`);
expired claims and abandoned review leases are stale too.
Only the current holder's calls count: the claimant call itself, or calls of the claiming main that joined at or
after the claim, so calls an earlier holder left never mark a fresh claim stale (an `oms agent-plan touch`
heartbeat restarts that clock, so a touched hold reads claimed until a new call joins).
Zero counts are omitted and `stale` is dropped first on narrow boards; the full box adds
`N stale, not counted: no live worker · release with oms agent-plan release --id ID`. On a
board under 20 rows (or when the pinned rows would starve the body) the box shrinks to one row,
`[ GOAL  <goal>  <bar> <counts> ]`, before any pinned row is dropped. Clicking the box selects the
`("tab", "plan")` target on every row it draws. Without an active plan (none, no goal, or idle for 7 days)
it is a one-row dim box, `[ GOAL · none · oms agent-plan init --goal TEXT ]`. While `oms land status --json` reports `active: true` (a probe of the land lock, never the receipt), one more row reads `LANDING <sha7> · <step> · <N>m`. `oms agent-plan claim` run by a panel main
also records its room participant (`claimed_by_participant`); the main's card then adds
`Task: <id> · <state>` beside its title (`+N more running` counts only held tasks with a live or unknown
call, `N awaiting decision` those whose calls wait for the main's decision; tasks whose calls all ended are left out), or, without a recorded title, `Now: <declared status>`
(with its age past 30 minutes and `(stale)` past an hour) or a dim `no status yet`. Every prompt of a panel main
carries a `[oms plan]` sentence (goal, progress, the main's task, the next ready task and its claim
command) and, when its declared status is missing or older than 20 minutes, a reminder to set it.

What the numbers mean. Each word names one stage, and the same set carries the same word on the
board group, the goal box, the main card's `Team:` line, Detail, the Attention queue and the Plan tab:

- `running`/`active`: a call's process is live.
- `finished`: a call's process exited; that alone admits or verifies nothing.
- `awaiting decision`: a finished call whose patch awaits admission, or a call in lifecycle `review`, until
  its main records a decision on the task (`Awaiting decision (N)` on the board, `N awaiting decision` in `Team:`;
  the Attention queue names the patch case `patch awaits admission` and shows how long it has waited);
  `completed`, `accepted` and `rejected` decide the same task across the board, Attention, window flag,
  Plan fold and carried calls. `failed` keeps it in rework. A decision recorded under a different plan binding than the
  current plan's task of the same id does not apply to it; decisions without a binding, and decisions for
  tasks the current plan no longer holds, keep applying.
- `unrecorded`: no live process, heartbeat or exit was seen (`status unknown`); it is not a failure and stays
  until its main decides. Detail says so in one line.
- `in review`: plan tasks in the plan's `review` state only (`N in review` in the goal box, `in review` in the
  Plan tab, the tree and the main card's `Task:`).
- `verified`: plan tasks `done` (`N/M verified`); finished or admitted calls never count.
- `landed`: `oms land` pushed the change and `agent-plan finish --landed-commit SHA` recorded it; while a land
  runs the goal box adds `LANDING`.

The Attention queue ranks what needs the person by urgency, not recency, and every board surface reads the
same ranked list, computed once per frame. Its classes, in rank order, are `answer needed` (a call or main
waiting for approval or input, a question escalated to the person or asked of `all` by a main), `failed` (a
failed, timed-out, abandoned or lost call whose result handoff is unread, that no later round superseded and
whose task is still open), `awaiting decision` (a finished call awaiting its main's decision, oldest first,
with its age; a patch awaiting admission is labeled `pending admission`), `held, workers ended` (a running
plan task whose holder's calls all ended; its row opens the Plan tab), `unread mail` (other unanswered room
mail from or to mains), and `delivery failed` (a failed main wake-up delivery). Provider errors are labeled
`failed`. Failed items open a reader with the ending, errors, kept worktree and a salvage hint when applicable.
Within a class the newest comes first. Above the board a header such as
`Attention 7 · 1 needs you · 2 failed · 3 decide · 1 stale` (zero classes dropped; narrower widths shorten
the words to `1 you · 2 fail · 3 dec`, then `Attention 7`) opens the Attention tab and is followed by the top
three items. The `Attention N` tab groups every item under a class heading; arrows pick one, Enter or a click
shows it in Detail, and `f` opens a call's full result on the right. The queue is display-only: it changes no
lifecycle state or authority.

Results classify call endings as `cancelled`, `guard stop`, `timed out`,
`verify failed`, `provider error` or `failed` from the recorded lifecycle,
verification and provider evidence.

When the graph leaves rows blank between the cards and the bottom box, a dim summary fills them:
`Attention N` with the first two queue items, `In progress N calls · oldest Nm`, and `Recent` with the
last three finished calls and their end words. It is display-only, never takes rows from the cards or the
box, and is hidden when fewer than four rows are spare.

A main is named by its model; an earlier main of the room on the same model adds its
participant id tail (`Opus 5.5 #9f3c`). A question to a main with no linked answer
(`oms room send --kind answer --reply-to ID`) adds `? N open` to its card and a `? ` window-name
prefix (`! ` wins). The recipient's hooks list open questions by id and name, from five
minutes on every hook and `overdue` from fifteen; the asker's own hook records one
`remind-<id>-1` note at ten minutes and `-2` at thirty, and the control window's
`Needs you` list shows the question from twenty minutes. A `--kind answer` without
`--reply-to` links the one open question from that recipient, or is refused with the ids.

The bottom box has Log, Detail, Plan, Messages, Past and Attention tabs, which stay even when empty,
plus Debate while a debate exists; the Messages tab is a
filtered room log. A main without an advisor or reviewer shows one dim
`◇ Advisors: none active · a asks one` row above its card. The pinned usage line
(`Claude week 66% · ctx 73% │ Codex week 2% · ctx 84%`) holds the readings; the model-call totals
stay in the tree USAGE card. Truncation reads `…` (`...` in ASCII mode).
`⚑` (ASCII `A`) marks a call waiting for approval, `?` one waiting for input.
When a call of a main fails, times out, is blocked or waits for a person, that
main's tmux window name gets a `! ` prefix, set and cleared only by the watcher in
that window. Numeric usage readings from a native pane count only when they are
whole fields of its last two status lines. A notice shows on a short board in place
of the key hints. Board text is written for
people: states read `running`, `finished` or `needs approval`, call exits and
provider status lines are dropped from messages, and the detail says whether its
main reviewed a result instead of showing internal fields. A main reads `working`
during a hook-recorded turn, `idle · Nm` after Stop, `quiet · Nm` after a stale
busy signal plus 90 seconds of unchanged native-pane output, and `live` until its
first activity signal. Quiet output does not prove that the model stopped. Clicking the main's
card opens its proven original conversation. Calls with no known main are counted on one
warning row (`t` lists them in the tree). Within advisors and within workers, attention states sort
first. A panel permits one live main across all its rooms; `--launch` and `--spawn-main`
refuse another while it is active. A start first abandons a main that a crash or reboot left open when
the `--release-main` proof holds (expired heartbeat, host and native process gone) and says so on stderr;
an unverifiable main still refuses the start. The control window's board shows the room's live main. PageUp/PageDown scroll the detail
when one is shown, otherwise every call band by one page. Graphs of different
rooms are never merged, and `o Rooms` in the control window switches rooms.
The bottom box state (tab, previewed target and detail scroll) is shared by the panel session in `@oms_panel_bottom`, capped at 512 bytes; malformed or oversized state is ignored, and a target that does not name a participant in the current room is not applied. Changes reach every board on refresh, and entering a window does not reset the selection. The dock opens on the Log tab unless a tab was chosen or stored. At heights of 28 rows or more, it reserves at least its height-band body (12, 8 or 5 rows) and takes every row left under the activity tree and warnings, with no blank padding. The Log tab lists the recorded events (one line says so when there are none); the strip shows how many events it is scrolled past, and `[` / `]` or the wheel scroll it. A finished goal banner names `superseded` (instant) and `completed-external` (which reruns acceptance, about 9 minutes), with the `oms agent-plan --repo . retire --check` and `retire --apply` command forms; the check prints the SHA-256 CAS token required by apply. Narrow banners retain the completion ratio.
Terminals attached to the same panel session share its current window, so
navigating from one client also moves the other. The wheel scrolls only the band or detail under the
pointer.

The footer shows at most three hints chosen by the selection (a main: `Enter Chat`,
`a Ask advisor`; a call: `Enter Show`, `w Worktree` when it applies;
`Esc Close` while a preview or detail is open; on the Messages and Debate tabs the arrows, `Enter` and the filter or target keys) and always ends with the fixed panel keys
`F9 Chat⇄Board  F5 Control  F12 Keys` (`q Quit` and `? Keys` on a standalone watcher). `?` swaps in the full list of keys
that work in that view, with `v Expand` or `v Collapse` following the board's state.
Notices disappear on the next input or after ten seconds.

`a` in the watcher asks an advisor for the selected main (or the shown one).
In tmux it opens a popup that takes one question and runs
`oms panel --dispatch advisor --main PARTICIPANT` without the
watcher's inherited caller identity; the dispatch re-proves that main is live,
and the answer returns to that main's room mail. Outside tmux the board shows
the command instead. Popups need tmux 3.2 or newer; when the popup cannot open,
the board says so and the control menu's `a Advisor` remains. A failed call keeps
the popup open with its exit status. Selecting or clicking never calls a model. A prompt-less
`--dispatch advisor` on a terminal asks its question interactively.

`w` on a selected, finished worker opens a plain shell window at its kept
worktree. The recorded `worktree:PARENT/NAME` label must match exactly one entry
of `git worktree list`; a running worker, a cleaned-up or ambiguous worktree
and repository-local calls are refused, and recorded results stay available.
The shell has no worker authority; edits there are not part of the recorded
patch. Outside tmux the board shows the path.

The board is an overview above and a detail area below. Cards keep only the
rows their content needs (an unrecorded location stays in the detail), and a
provider without a reading reads `Codex --` on the pinned usage line (the tree keeps its USAGE box where model-use data exists). On a
board of 24 rows or more, the bottom dock is reserved before the upper
layout is sized and then grows to the rows left under the tree. When the main preview is
automatic, it is marked `auto` and does not replace a chosen preview. Esc
dismisses the preview while preserving the box. Choosing a block replaces the
Detail content without resizing the reserved box; it shows the selected content,
starting with outcome and verification: for a worker or advisor the latest retained answer, the changed files
of a recorded patch (summarized only while its SHA-256 still matches, so it
survives worktree cleanup), outcome, verification and artifact references (read
in the background, refreshed with each snapshot); for a main its addressed mail
and team. Reading mail there never
acknowledges it. The native CLI pane itself is never used for detail; the reader is
a separate pane split from it.
A CLI exit never changes a task to accepted. The board is repository-wide, not
a claim that every displayed task belongs to this room. Declared scopes are
labels, not file locks or authority; write workers still use isolated patches,
verifiers and owner admission. Usage retains the recent-eight-artifact limit,
selected/served attribution and unknown native usage.

```bash
oms room new --repo . --id team --topic 'Parser change'
oms panel --repo . --room team --launch codex
oms panel --repo . --room team --launch claude
oms panel --repo . --room team --watch --view graph
oms room show --repo . --id team --json
```

These are agent-side interfaces; agents operate them internally. For already
open Codex, Claude or another CLI, the agent joins with its exact native session
ID and a distinct participant ID. Only the session hash is stored. Without a
native main role, participants use their assigned room identity for navigation.
Explicit worker session registration and messaging remain supported; that routing
record does not grant an owner app link. Custom native CLI mains retain explicit
session/resume support. Without a
room environment, automatic lookup is bounded to 64 work rooms, 1024 directory
entries and 8 MiB, including bounded header peeks that exclude ordinary threads.
Incomplete or ambiguous discovery never establishes a unique receiver; the room
inventory reports `incomplete`. Exact explicit selection bypasses inventory
lookup. Closed/left rooms do not
deliver. Membership is a routing record, not authentication or an OS sandbox.

For an existing tmux main, the session's selected room can also receive coordination
mail when that room explicitly enrolls the same main, native consumer, provider,
model and role. The session must belong to the same repository. Hooks retain the
original room's cursor and use a separate cursor for coordination mail; neither
delivery acknowledges messages. Existing process environment, attempts, leases,
dispatch and acceptance authority keep their original binding. Selecting a room
alone does not enroll a receiver or transfer its work. This requires the updated
hook code; older installed hooks continue to read the process's original room.

```bash
oms room join --repo . --id team --participant sol-main --provider codex \
  --role main --model gpt-6.1-sol --native-session EXACT_SESSION_ID
oms room join --repo . --id team --participant opus-main --provider claude \
  --role main --model claude-opus-5-5 --native-session EXACT_SESSION_ID
oms room send --repo . --id team --participant sol-main --to opus-main \
  --kind question --message-id api-question --text 'Inspect the caller contract.'
oms room updates --repo . --id team --participant opus-main --json
oms room send --repo . --id team --participant opus-main --to sol-main \
  --kind answer --reply-to api-question --text 'Caller contract confirmed.'
oms room ack --repo . --id team --participant opus-main --message api-question
oms room describe --repo . --id team --participant sol-main --label 'Improve result navigation'
```

`oms room close --id team --participant sol-main` records a typed `closed`
event. A joined member or the recorded creator may close a room; workers cannot.
New rooms record the creator when `new --participant` or the inherited participant
is supplied. Existing rooms without that field require joined membership.
Closed rooms reject join/send, appear with `closed: true` in show/list, and retain
read/ack access for enrolled recipients. `oms gc --apply --days N` removes aged
closed rooms and empty rooms without joined participants under the thread lock;
unread mail, unknown joined presence, recent or malformed logs are preserved.

`describe` updates an enrolled main's work title without leaving the room or
moving its native session, join time, mailbox anchor or unread messages. An
explicit title takes precedence over its launch title; existing `status` messages
continue to supply the `Now` line. Identical title updates append nothing. Only
the title changes: declared paths use the separate scope workflow, and workers
cannot use `describe`. A session bound to a room can describe only its own main.
Legacy room logs remain readable; the new event kind requires a current reader.

The canonical thread log records participant roles, addressed message IDs,
reply links and explicit consumption. Stable message IDs deduplicate identical
sends; conflicting reuse fails. Broadcast recipients are fixed at send time.
A new join does not replay earlier messages. Reads exclude sender echoes and
other participants' direct mail, advance a checked cursor and do not acknowledge.
Delivery/consumption/answer/acceptance are separate states. Rooms are bounded
to 32 members (mains and other unparented participants), 480 dispatched calls
(participants with a parent, which keep their slot after finishing) and 1024
messages, so each tick's room read stays bounded.

Bounds apply per room log. When a room passes 896 messages or 416 calls, the
next dispatch rolls it over to a linked successor (`oms room rollover --id ROOM`
does the same by hand); ids that name the old room keep working, the old log is
sealed and retained (`oms room show --id OLD --exact`), and nothing is
discarded. The successor carries joined mains, running calls, calls whose
results are unread, mail still owed to calls, open questions (a call's oldest
one; its later ones count as other mail) and each main's latest status, plus
the newest other unread mail up to 128 messages; older unread mail stays in the
sealed log, a note in the successor says so, and acknowledging it through
either id records the receipt. Carried mail keeps its send time and a
broadcast its original audience; a call whose parent left stays behind. Until
its carry completes the successor refuses other writes; the next write by a
main or the panel finishes a carry a crash cut short, and a worker's write is
refused until then, since workers cannot carry. The successor keeps the sealed
room's creator, and a rebound main keeps its first native session. The Past
tab also lists the previous room's settled calls.
A running main keeps its environment and native session: its writes, reads and
dispatches follow the old id to the successor.

New event kinds need a current reader: install the same SHA and restart panel processes.

Installed prompt/edit hooks deliver at the next safe point. Idle sessions must
poll or reach a safe point; there is no vendor-independent push/wakeup promise.
Provider inventory reports native hook wiring as unverified. MCP reuses the
existing twelve core tools: `oms_peer_start` message/ack with `thread` and
`participant`, plus `to_participant`, `reply_to`, `message_id`, `message_kind`
or `message_ids`; `oms_peer_result` with `thread`, `participant` and `after`
returns only addressed deltas. Room messages never invoke a new model.
Workers can send/ack only as their inherited admitted room participant, and
cannot create/join rooms, publish to the app or recursively start peers.

A panel role dispatch joins its worker/advisor/reviewer, records its room and
participant in lifecycle refs, supplies the room interface in the scoped
context, and returns a bounded artifact-backed handoff to its caller. A handoff
reports the call exit and pending parent acceptance. It does not merge native
histories or write source files in the main's worktree.

If an exit cannot be verified, the attempt and write scope remain held with no
final handoff. The next dispatch reconciles pending calls whose recorded process
identities and, after a forced stop, provider groups are proven gone. Pool-limit
refusals name unresolved calls; stop any remaining providers and verify cleanup
before cancelling their attempts. TERM/HUP ends the panel after dispatch cleanup.

Native instructions include explicit room and participant arguments for tools
that do not inherit the CLI environment. Role dispatch uses `--room ID` to
resolve the verified active main; missing or ambiguous ownership is refused.
A standalone dispatch without room/main binding remains available and reports
that its relationship is unrecorded.

Mains declare what they are editing so their dispatches and other sessions can
see it: `oms room scope --id team --participant opus-main --owns scripts/lib
--owns docs/TERMINAL-PANEL.md` replaces that participant's declared scopes and
`--clear` empties them. Only the participant itself may declare (the inherited
`OMS_ROOM_PARTICIPANT` must match; workers cannot); at most sixteen normalized
repo-relative scopes; advisors and reviewers own none. A write dispatch takes
repeatable `--scope PATH`; the worker joins with those scopes, and before launch
the panel compares them with every other joined main's scopes (including mains
from sessions outside this panel) and every live write call's (no result handoff,
no terminal attempt). Two scopes overlap when
one equals the other or is a path-component prefix of it (`scripts/lib`
overlaps `scripts/lib/room.py`, not `scripts/li`; `.` overlaps everything;
globs compare literally). Overlaps print one stderr warning and appear as
`overlaps` in `--dry-run` JSON. The launch is never refused: declarations
coordinate work, they are not locks. The main detail view shows `Scope: …`.
Finalizing as `accepted` refuses write-worker patches outside their declared
scopes (including rename sources and destinations), using glob-aware path matching;
`completed` remains allowed, and workers without declared scopes keep the existing behavior.

### Original chats and recovery

`h Chats` lists the selected room's participants. Selecting a native main focuses
its existing tmux conversation pane when that pane still exists and its canonical
repository, provider, room and active main attempt match the enrolled participant.
First launches and resumed stable participants both qualify; blocked but
nonterminal attempts remain navigable. Old or unprovable window markers do not
establish a destination. When that destination is in another tmux session, navigation selects its window
and pane and switches the single client attached to the caller's verified
session. Missing or multiple current clients require selecting the original
terminal; another attached client is never chosen by recency. Herdr pane lookup
failures affect their participant while other proven targets remain available.
An unqueried duplicate candidate cannot establish unique ownership, and socket
failures continue to make host evidence unavailable. Without a proven terminal
destination, Codex uses the exact local session's `codex://threads/<thread-id>` app link.
Claude Desktop navigation uses `claude --desktop --resume <session-id>` on
macOS/native Windows when the installed CLI advertises `--desktop`; native
platform, authentication and live-session restrictions still apply. It never
force-closes a running CLI. Linux/WSL users use the original Claude app sidebar
or the existing terminal; OMS does not invent a Claude Code session URI.
Headless workers expose their recorded results and artifacts instead of a
native app chat. Unbound or undiscovered sessions say `unresolved`.
The list shows ambiguity reasons and marks participants that left. Their proven
original chats remain available as history; this does not rejoin them or resume work.
Room views and chat navigation require an explicit selection from `o Rooms`,
`--room` or the room environment; another session's `CURRENT` pointer does not
select a work room. Narrow panes retain joined main names and mailbox totals;
an explicitly requested graph uses the 76x16 requirement, retaining tree
navigation when the interactive card layout cannot fit.

`--chats` is read-only. `--open-chat PARTICIPANT --dry-run --json` previews the
destination without navigation; an explicit open submits no new prompt. Local
lookup examines bounded filenames and Codex's first metadata row, without
copying transcript bodies or native session IDs into the shared room log.
Navigation success is a request, not evidence that an app window appeared.

A native main launched by the panel binds its first real prompt-hook session
to its explicitly enrolled participant. This preserves the original join
boundary, pending messages and cursor. An existing unbound Claude participant
can also be bound by its own agent with
`oms room bind --repo . --id team --participant opus-main --native-session current`.
An already bound identity cannot silently change; a different native session
requires explicit leave/rejoin. Closed or left membership does not auto-resume.
A main's own SessionStart moves its binding when the provider starts another native session in that process (Claude `clear`; Codex `clear`, `resume` or `fork`), replacing only the identity bound at that moment; `startup` and `compact` never do.
Later hooks compare both enrolled provider and native identity before reading or
advancing a cursor. A mismatched user prompt receives a short reconnection notice;
tool hooks withhold the room mail without repeating that notice.

A Claude main moved to Claude Code's background service continues under a new
session id without the panel environment. Its SessionStart `fork`/`resume` hook,
or else its next prompt, re-attaches it by transcript lineage: when exactly one
joined Claude main's transcript shares the new transcript's first user/assistant
record uuid and the new records carry Claude Code's background `sessionKind`, the
binding moves with the same compare-and-swap, keeping the join boundary and mail.
A conversation forked by hand into another terminal shares the root but is not a
background session, so it never takes the main's binding. This is cooperative, same-UID evidence, not authentication;
zero or several matches change nothing, and a miss is retried at most every ten
minutes. `oms panel --dispatch --owner claude` from that session then finds its
main through `CLAUDE_CODE_SESSION_ID`; workers never take this fallback.

An exact resume of a still-joined, proven-terminal panel owner reuses its logical
participant while starting a distinct execution attempt. Join boundary, queued
mail and cursor anchors remain intact; selected execution model metadata does
not rewrite the enrolled participant. Existing lifecycle refs and tmux markers
link the new attempt to that participant. Unknown, conflicting or nonterminal
ownership blocks reuse. Cooperating OMS resume requests in the same repository
serialize by provider/native identity across rooms, and check joined/left room
history before launching. Incomplete ownership discovery also refuses launch.
This is not authentication or a global lock against other applications/OS users.

Native CLI Ctrl-C remains a turn interrupt handled by the child. HUP/TERM use
bounded shutdown, restore previous handlers and exit the panel with 129/143.
An unconfirmed launch/shutdown remains nonterminal; heartbeat age never proves
that its child has stopped. Ordinary CLI exit keeps the logical subscription,
and control-window dispatch requires one recorded active main of the requested
provider. An inferred caller carries that main's execution ancestry. SIGKILL,
power loss, missing/compacted ownership evidence and surviving descendants may
require explicit operator inspection; they never authorize automatic takeover.

The watcher keeps refreshing after a collection timeout or malformed response,
shows degraded evidence and can recover on the next successful refresh. Room
history, mailbox cursors and delivery intents are durable across process
restarts; OMS is not an always-running supervisor. Dead Claude registry records
are excluded from live receiver discovery. Unconfirmed sends never retry
implicitly, and safe-point delivery still requires a live participating session.
Incomplete native registry discovery cannot authorize an otherwise apparently
unique inbox. Collection and room errors stay visible in graph and narrow views.
The canonical operator suite has an optional `OMS_PANEL_SOAK_SECONDS=30..3600`
Unix fixture run for sustained watch, addressed delivery, explicit ack, invalid
cursor and temporary room-loss recovery. It is off in ordinary gates and uses
fake providers rather than live accounts or GUI delivery.

Sources: [Codex thread links](https://learn.chatgpt.com/docs/reference/commands#deep-links),
[Claude CLI to Desktop](https://code.claude.com/docs/en/desktop#coming-from-the-cli).

`oms room publish --repo . --id team` sends a bounded snapshot to the existing
fixed Codex app receiver. It saves a delivery intent before sending and its
receipt afterward; repeated persisted/uncertain snapshots do not resend.
`--retry` is an explicit retry that can duplicate uncertain output. Busy,
absent and failed delivery leave the task state unchanged. No new receiver chat
is created, and a persisted command output is not proof of a desktop popup.
The detailed moving panel is available in the app's integrated terminal; a
native custom graph widget or automatic app subscription is not implemented.

### Claude Desktop Code and Claude Code terminal delivery

Claude Desktop's **Code tab** shares the CLI's hooks and MCP configuration.
An explicitly joined local Code session can consume room updates at the same
safe points and use the existing `oms_peer_start` / `oms_peer_result` tools.
The animated panel also runs in its integrated Terminal pane. The separate
Claude **Chat tab**, cloud sessions and cross-machine routing are not targets
of this local inbox transport.

Inside the receiving Claude Code session, the agent can bind the current
session without copying a socket address or exposing authentication material.

```bash
oms room join --repo . --id team --participant opus-app --provider claude \
  --native-session current --label 'Opus app'
```

`current` resolves only the session's exported
`CLAUDE_CODE_MESSAGING_SOCKET` in the bounded native registry. It never selects
the most recent session. A room participant can then send the room snapshot:

```bash
oms room publish --repo . --id team --app claude --to opus-app --allow-wakeup
```

This uses the native Claude Code inbox, **not a zero-token notification**.
An idle receiver can start a model turn, so `--allow-wakeup` is mandatory for
sending; omitting it returns `wakeup_required` without a socket post. The
receiver's own inbound accept/hold/refuse policy remains in charge. OMS does
not change that policy, save messaging tokens, impersonate a native sender,
or create a Claude session. The registry must resolve exactly one enrolled
session, and the local Unix socket must be private, owned by this OS user,
free of symlinks and unchanged during connection. On Linux, the connected
process must also match the registry PID. Native Windows named pipes are
currently reported as `unsupported_platform`; Linux/WSL and macOS Unix
sockets use the same protocol.

`submitted` means the bounded JSON frame was written; it does not prove native
acceptance, transcript persistence, a visible card or an OS popup. Such a send
is marked unconfirmed and is not resent implicitly, even when the snapshot
changes. `--retry` can duplicate it. Codex and each Claude participant have
independent durable intents/receipts; existing logs without a target remain
Codex receipts. Graph and compact views show the latest app delivery states.
`OMS_CLAUDE_NOTIFY=0` disables native Claude posts. Panel `--finalize --notify`
continues to use its existing fixed Codex receiver; Claude delivery is selected
explicitly through room publishing.

Sources: [Desktop shared configuration](https://code.claude.com/docs/en/desktop#shared-configuration),
[native inbox and inbound controls](https://code.claude.com/docs/en/cross-session-messaging#the-sessions-inbox-socket).


## Additional CLIs and adapters

The `p CLI peers` menu reads the same provider registry as `oms models`. Grok,
Antigravity (`agy`), Gemini, Cursor and other registered transports need no
panel-specific registration. An executable `oms-agent-adapter-ID` on PATH or
in the configured adapter directory also appears automatically. Discovery only
lists capabilities; it neither calls a model nor adds that adapter to preset
or automatic routing. Presence is executable discovery, not an authentication
or runtime health check. Models come from the existing cache without refresh.

A main can explicitly select a registered CLI for a bounded worker, advisor
or reviewer. `--to codex|claude` without `--model` pins that provider's newest
candidate family and effort from the role's tier; an explicit `--model` overrides
it. Other transports supporting per-call model selection require an exact
`--model`; omit the `--workload` tier. An optional `--reasoning-effort` is forwarded through the existing
provider capability checks. With an explicit model or custom route, omitting
effort retains the front door's native settings. For profile-controlled transports such as Vibe/DeepSeek Harness,
omit `--model`; the native profile selects it and an exact override is refused.
Preset roles use a capability tier ordered by weekly quota left, then configured tier order (see Roles below).
Codex delegated routes accept the exact Sol, Luna and Astra model IDs in the
panel policy, including Sol 6.1. Model-family versions are distinct from roles. Preset dispatch chooses the newest
configured/catalog numeric version within its existing family and pins the exact ID;
a newer Astra never replaces a Luna or Sol route. Explicit CLI selections retain
the caller's exact model request. A compact model preview does not limit routing.

```bash
oms panel --repo . --dispatch advisor --owner codex --to grok --model MODEL \
  --task-id design-options --label 'Compare design options' --prompt 'Compare these bounded options.' --dry-run
oms panel --repo . --dispatch worker --owner claude --to agy --model MODEL \
  --purpose investigate --prompt 'Inspect the relevant source without edits.' --dry-run
```

These examples are plans; the caller must select a model actually supported by
the installed CLI. Other targets use the same role metadata, exact parent
attempt, artifact results and isolated worker path as the preset routes. An
adapter's write route additionally requires explicit `OMS_PROVIDER_WRITE_ADAPTERS`
opt-in, a scoped brief and verifier; it still returns a patch for owner review.
Discovery never widens write or publication authority.

For a new CLI such as Muse, whose actual interface has not been verified here,
the integration unit is a small adapter rather than another panel branch. Its
existing [adapter contract](COMPONENTS.md#asking-other-agents) accepts
`run --access read|write --workdir PATH --prompt-file PATH`, plus optional
`--model` and `--effort`. It must translate those arguments into the CLI's
documented noninteractive invocation, enforce the requested access, forward
exit status, print the answer on stdout and diagnostics on stderr. Unsupported
access or arguments must fail explicitly. An adapter must not commit, push,
modify OMS owner state or retain background processes.

The inventory distinguishes headless calls, room polling, native launch and
unverified native hook wiring. A call adapter alone does not attach to an
already-open app or wake an idle session. For additional native launch, explicitly
list canonical provider IDs in `OMS_PROVIDER_NATIVE_ADAPTERS` and provide the
marker executable `oms-agent-adapter-ID` on PATH or in the same adapter directory.
Its additional contract is `open --workdir PATH --context-file PATH`, with
optional `--model` and `--resume`. The file contains bounded OMS bootstrap,
room participation and the supplied user task; it is removed after process exit.
The adapter must preserve interactive stdio, forward exit status, consume that
context, and translate only supported documented vendor flags. It inherits
`OMS_ROOM_ID`, `OMS_ROOM_PARTICIPANT` and canonical `OMS_ROOM_REPO` for polling.
The `p CLIs` menu's `n Native` action or `--launch ID` uses this explicit capability;
unsupported native adapters fail rather than falling back to a headless call.
This opt-in grants no write/publication authority. The actual Grok/Muse interactive
interfaces have not been verified; the contract is covered by protocol fixtures.
Owner finalization/council retains Codex/Claude's existing policy. No additional daemon, dependency or terminal injection is added.


## Context limits for mains

A main's context usage comes from the statusline cache (Claude) or the
transcript (Codex). At 30% left a handoff digest is saved silently
(`OMS_CTX_CAPTURE_PCT`), at 15% a warning is shown once (`OMS_CTX_WARN_PCT`)
and at 8% once more (`OMS_CTX_URGENT_PCT`); the latch re-arms above 30%
(`OMS_CTX_REARM_PCT`). A panel main is bound to its window, room participant
and plan claim, so it is advised to compact in place (`/compact`) instead of
migrating to a fresh session; nothing is compacted for it. After a compact or
clear it gets one `[oms panel] resumed after ...` line with its window, model,
room, plan task, open questions, running workers and declared status (ids,
counts and names only). The board shows `ctx N%` on the main's card
when known, with `compact soon` at 15% or less.


## Contract and verification

`--json` returns schema 1, kind `oms-panel`, a nested `oms-dashboard`, provider
`installed` booleans, `split_available` and additive `routing` policy. Lifecycle
metadata is written through typed `agent-events --ref NAME=VALUE` and projected
as bounded panel metadata; see [the dashboard contract](DASHBOARD.md).

Exit 0 is successful collection, 1 is degraded collection or launch failure, 2
is invalid usage or a worker attempting owner actions, and 130 is interruption.
Native launches propagate the process exit. Existing tools retain patch
admission, verification and publication authority. No permission bypass,
automatic patch application, commit, push, installation or new service is added.

The existing operator-tools suite exercises exact role routes through real OMS
front doors and lifecycle writers using fixture provider executables. It checks
model/effort arguments, parent links, titles, read/write separation, worktree
isolation, returned patches, review gates, worker recursion rejection and
read-only views. Native PTY and real tmux checks cover inline restoration,
exact resume, main selection, independent sidebars and detach/preservation.
Width/height, Korean title wrapping, cached frame cadence, changed-row repaint,
resize, reduced motion, attribution and unsafe-label tests cover the renderer.
A PTY watch verifies actual incremental animation output without lifecycle writes. The
lifecycle-events suite covers bounded refs and immutable idempotency. Fixture
execution does not establish account access or a live main's judgment quality.
When a sandbox denies Unix sockets, the suite reports socket-dependent sections
as skipped and exits 77; parent verification outside that sandbox remains authoritative.
The same operator suite covers council routes, durable results, verifier and
patch lineage, privacy refusal and delivery deduplication/retry after interruption.
The notifier suite covers unloaded and busy receivers, shell wrappers, early
events, lost acknowledgements and correlated persisted output.
The operator suite prints per-section and per-shard timings. `ci-status history`
shows recent completed CI runs and ranked failing jobs/tests. The operator suite
also verifies the main hierarchy with earlier mains on record, legacy ownership,
separate review groups, compact/detail budgets, attention filtering, numbered
result selection, menu cycling and view inheritance in an existing tmux server.
Use `tests/operator-tools-smoke.sh --list-sections` to see focused verification
areas: `auto-review` covers the review gate, `native-pty` covers terminal
lifecycle behavior, and `room-rollover` covers room succession; combine
independent top-level areas with commas. Core panel behavior (board, routing,
rooms, native windows) has no section of its own: verify it with `--shard 1/4`,
about 8 minutes at low load.


## Researcher and supporting strategies

Use `oms panel --dispatch researcher --owner claude --prompt 'Find and cite the relevant contract.'`
for one bounded document/source question. The control menu's
expanded `e Researcher` entry uses the same route. Researcher is a separate room
participant beneath its main; its bounded answer is retained in Results and its
status appears in tree/graph views. It never becomes a main or a patch approver.

The researcher takes the light tier's Haiku or Luna candidate, whichever provider
is installed first (`--to codex|claude` narrows it). Current configured floors are
`claude-haiku-5-5` and `gpt-6-luna`. The dispatcher compares
numeric versions within that family against the cached provider catalog and pins
the newest exact ID. An omitted minor version means zero; `6.10` is newer than
`6.2`. Cached discovery does not prove account access or live availability. No
paid availability probe runs. If the pinned model cannot run, the task fails
without an implicit older-model or more expensive-family substitution. A provider
without a model catalog retains the configured exact ID until policy is refreshed.

Researcher requires read access, the light tier and low effort. Write scopes,
implementation, review gates, worker continuation and high-model/effort overrides
are refused in routing; lifecycle recording independently rejects research
metadata on a write call. Source content is data, never task authority. The main
owns interpretation, architecture and acceptance. Official Haiku model/effort
spelling is documented in [Claude model configuration](https://code.claude.com/docs/en/model-config).

The other strategies dispatch as roles of their own (see the role table above);
`test-designer`, `patch-reviewer` and `repo-auditor` stay `peer-delegate --role`
profiles. A deterministic check should run directly rather than paying another
model to repeat a command.


## Owned terminals, jobs and history

Expanded `s Terminals and jobs` opens a separate owned tmux workspace. F4 shows
controls and F5 returns to its origin. These are ordinary shells and explicit argv
jobs, not AI chats. Listing does not capture screens or logs; reads are explicit
and bounded. Jobs require Linux pidfds/subreaper ownership; unsupported hosts
retain the normal OMS lifecycle. Shell/workspace ownership remains POSIX/tmux.

`f` shows completed jobs with their actual completed count. A selected provably
completed job or gone shell offers `x forget`, with a permanent history/log deletion
warning and explicit `yes` confirmation. Live, uncertain, foreign or unsafe records
are refused. Retained history is never automatically deleted.

`c` archives completed/gone items and frees active slots while preserving records,
job recipes and the latest generation's retained log. `h` opens or leaves the
history view. Individual items offer `a archive` or `b restore`; restoring a record
does not launch its command or reopen a shell. Archive and restore revalidate the
exact identity and job generation under the existing ownership locks. Busy,
unsafe, live and unknown records stay in place.
If a transfer is interrupted between its marker update and directory move,
the same archive/restore operation revalidates ownership and completion before
finishing it; inconsistent markers never authorize restart or permanent deletion.

When a new shell, job or workspace reaches the per-kind 128 active-record bound,
the allocator archives a positively completed/gone record automatically before
allocating the new slot. Each kind retains at most 512 archived records. A full
archive or an active set with no safe candidate refuses the new allocation with
an explicit capacity reason; existing processes and history remain intact. The
reclaim scan has a three-second budget and skips busy record locks rather than
waiting on each one. Native ownership probes retain their existing timeouts.

CLI equivalents are `oms panel job forget ID --repo . --generation N` and
`oms panel terminal forget ID --repo .`. Use `oms panel terminal workspaces --repo .`
to discover workspace records and `oms panel terminal workspace-forget ID --repo .`
for one provably gone workspace. These explicit commands permanently remove private
history and any retained job log. Revalidate the ID/generation before requesting them.
Restart keeps the previous completed generation and log when prelaunch validation
or an unreleased supervisor constructor fails. Once approval release may have
occurred, uncertainty stays unknown rather than allowing a duplicate generation.

Use `oms panel job archive --repo .` for completed job history,
`oms panel job list --repo . --archived` to list it, and
`oms panel job restore ID --repo . --generation N` to restore one exact generation.
Archived job logs use the usual `logs ID --generation N` command. Terminal history
uses `archive`, `list --archived`, and `restore ID`; workspace history uses
`workspace-archive`, `workspaces --archived`, and `workspace-restore ID`.


## Capacity and display recovery

On the focused board, `k` opens the capacity/recovery screen and `k` or `Esc`
returns to the board. `u` or `Ctrl+L` requests a fresh read and repaints the panel
without discarding its selected task, logs, sessions or model context. The control
menu also offers `k Capacity`; `Ctrl+L` there follows the existing Refresh action.

`oms panel --repo . --health` reports the same limits; add `--json` for the complete
bounded projection or `--room ID` to inspect a specific room. It reports selected
main context, reported weekly usage, room mains/calls/messages, and private
active/history slots separately. Missing, ambiguous or stale context/quota stays
unknown. Context bands reuse the existing `OMS_CTX_WARN_PCT` (15% left) and
`OMS_CTX_URGENT_PCT` (8% left) settings and existing hook handoff/compaction flow.
The health screen makes no model calls and changes no native permissions.

Clearing the panel display does not free model context or subscription quota.
Use native compaction or an explicit handoff for context, and the native quota
reset for exhausted usage. Room acknowledgement keeps message evidence; new
mains already use a fresh bounded room near capacity. Existing sessions and
their room evidence stay intact.

## Salvaging a stopped worker

Run `oms panel --repo . --salvage TASK_ID [--attempt ATTEMPT_ID] [--json]` to capture the latest stopped
delegate's recorded, kept execution worktree, including a separate clone or
scratch worktree, or the explicitly selected stopped attempt of that task.
Selection keeps the same room-owner, stopped-state and worktree identity checks;
an attempt from another task is refused. It preserves that checkout and index,
and reports changed paths, before/after byte sizes, the patch path and its
`.patch.json` metadata under `.oms/artifacts/delegate/`. Metadata and the patch
header carry `salvaged_from` (task, attempt, kept path and stop reason), base and
payload hashes, file sizes and `encoding_warnings` for display. Admission uses
controller records in `.oms/salvage-provenance/`, keyed by the diff payload hash;
editing headers, sidecars or warning lists cannot change those records.
The flags guard the salvaged artifact as captured; an edited or reformatted
patch is a different patch that ordinary admission checks. `patch-admit`
examines every textual patch hunk for encoding loss, independent of salvage
identity; `--accept-encoding-loss` records owner acceptance for detected hunks.
Warnings flag hunks whose
replacement has the same ASCII skeleton after Unicode decomposition but fewer
non-ASCII characters, and files losing more than half their non-ASCII bytes.
These are review hints; they can include intentional edits or deletions.
Review the patch before using `oms patch-land --patch PATH --accept-salvaged`.
Flagged encoding loss also requires `--accept-encoding-loss`; normal admission
checks still run. Salvage captures evidence and never admits or applies it.
