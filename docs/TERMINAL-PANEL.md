# OMS terminal control panel

This guide is the full reference for using the OMS terminal panel and its recorded work rooms.
It covers startup, navigation, role routing, results, recovery and the contracts behind them.

## In this guide

| I want to… | Jump to |
|---|---|
| Open a main conversation | [Quick start](#quick-start) |
| Find a key or change focus | [Keys](#keys) · [Control menu](#control-menu-and-focus) |
| Choose an account or dispatch work | [Provider accounts](#provider-accounts) · [Roles and tiers](#dispatch-roles-and-tiers) |
| Review and accept a result | [Finalize an outcome](#finalize-an-outcome) · [Accept a patch](#accept-and-publish-a-patch) |
| Recover interrupted work | [Settle workers](#settle-stopped-workers) · [Salvage](#salvaging-a-stopped-worker) |

**Section index**

- [Quick start](#quick-start) and [starting the sole main](#starting-the-sole-main)
- [Layout and activity](#layout-and-activity), including [keys](#keys)
- [Roles and automatic allocation](#roles-and-automatic-allocation)
- [Discussion and results](#discussion-and-results)
- [Reference](#shared-work-rooms-and-the-graph), [contract and verification](#contract-and-verification), and [recovery](#salvaging-a-stopped-worker)

Run `oms` in a terminal to open the control panel. Native Claude Code, Codex, Agy or Pi keeps
the conversation pane; the OMS sidebar shows recorded main, advisor and worker activity.
`oms --help` and bare `oms` with redirected input/output retain command help. The panel
uses existing OMS state, authentication and front doors. Without `--repo`, the panel
resolves the current Git top-level and refuses a non-repository with exit 3; explicit
`--repo PATH` also requires a Git repository and returns 3 otherwise. `--providers`,
`--routes` and `--help` remain available anywhere. The sidebar reads OMS state; it does
not replace the providers' native conversation or authentication.

<details>
<summary>Transcripts: navigation, masking and retention</summary>

Finished dispatch calls keep an owner-only OMS transcript beside their activity summary
in `.oms/panel-state/dispatch-logs/`. In a call's Detail press `x` for Transcript, or
select Transcript (`x` or Tab) in its Results reader; arrows, Page Up/Down and search in
the reader navigate the captured assistant messages, commands and tool results, edit
content and final report. Esc returns. Secret-shaped lines are masked using the outbound
scanner patterns. Each capture keeps at most the last 8 MiB with a truncation marker;
captures older than 30 days or beyond the newest 128 are pruned when another capture
finishes. Output excerpts are capped at 8 KiB. Calls without an OMS capture show
“transcript not captured”; provider homes are never read.

</details>

## Quick start

1. Run `oms` in a terminal and choose Claude, Codex or Agy. Enter or EOF opens control only; Ctrl-C exits with 130. Missing CLIs show their reasons. The panel starts at most one live main. Pi remains an explicit alternative (`OMS_PANEL_MAIN=pi` or the Pi start key).
2. The main window keeps the board on the left whenever the window is wide enough (above the CLI only when it is too narrow); the right side is the reader for workers, advisors, reviews and results, alongside the native conversation. At wide widths it shows the graph; narrower layouts fall back to the tree. At fewer than 20 columns the board omits its tab box.
3. The compact goal banner and Plan tab show a bounded goal preview (up to 200 characters and, in Plan, two wrapped lines); `oms agent-plan list` prints the full plan. The bottom box contains Log, Detail, Plan, Messages, Past and Attention tabs (Past and Attention stay even when empty), plus Debate while a debate exists. The pinned **Needs you** attention strip stays above activity when the inbox has items.
4. The control window's **Needs you** inbox lists failed or timed-out calls, calls or mains waiting for approval or input, unanswered broadcasts and patches awaiting admission. Select with arrows or a click; Enter opens the selected chat or result.
5. `Tab` cycles visible tabs; arrows move within the current surface; Enter opens or expands. `Esc` closes a reader or backs out. The detailed keys and their surfaces are listed under [Keys](#keys).
6. At a main's exit prompt, `r` resumes or restarts in that pane and Enter closes the window. `F9` switches between chat and board; `F5` opens the control window, and from that window returns to the previous one; `F12` shows panel-wide keys.

```bash
oms                                      # open the control panel
oms panel --repo . --launch codex         # start a Codex main
oms panel --repo . --launch agy           # start a fresh Agy main
oms panel --repo . --results              # read retained results
```

<details>
<summary>All launch, layout, status and council command examples</summary>

```bash
oms panel --repo . --layout inline         # portable menu, no tmux needed
oms panel --repo . --layout split          # uses an already-installed tmux
oms panel --repo . --host tmux --launch codex   # team graph left of native chat
oms panel --repo . --host tmux --launch claude
oms panel --repo . --reopen                # restore only a missing tmux board
oms panel --repo . --json                  # read-only status and role policy
oms panel --repo . --routes                # read-only role policy
oms panel --repo . --recommend debug --diff main..HEAD --failed 2   # read-only tier/effort advice
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
oms panel --repo . --results --task-id ID --export result.md   # the reader's text, to a new local file
oms panel --repo . --results --page 2      # the next 20 retained tasks (1-based; no --task-id)
oms panel --repo . --room team --chats --json
oms panel --repo . --room team --open-chat sol-main --chat-surface app --dry-run --json
oms panel --repo . --council --owner codex --task-id ID --label 'Decision' --prompt 'Compare options' --dry-run
```

</details>

`oms panel --salvage TASK_ID` verifies a kept worktree against the execution repository recorded for that room call and saves its patch and provenance in the room repository.

Each dispatched call has a short room alias such as `B7`, shown before its role
on the board, selected card, reader, session log and Results. `--alias NAME`
chooses one (1–8 letters, digits or hyphens); otherwise the panel assigns the
next alias using the role letter and number. Aliases are unique across a room lineage.
Concurrent dispatches reserve aliases before launch, so a four-call worker
wave receives `W1` through `W4` rather than duplicate names.
`--continue TASK_OR_ALIAS` retains the continued call's alias, and Results
accepts an alias in `--task-id` when it resolves to one task. The default alias
prefix is selected by role (for example, `B` for builder and `W` for worker).


## Starting the sole main

Panel mains carry the assigned goal through worker recovery, result inspection,
integration, required checks and owner finalization. A worker's exit or review
handoff is an intermediate step. The main continues actionable work within the
user's scope, or records a concrete blocker and the next action. Publication
still requires the user's authority; a local result can be completed first.

### Swap the main

The `s` board key lists installed main targets and their weekly quota readings, then
requests a bounded handoff before swapping. `oms panel --swap-main PROVIDER [--model
MODEL]` is the command form. It requires an existing tmux panel, one verified live main.
A land in flight does not block it; the handoff names the land and its
`oms land status --repo PATH` command. The current main is asked for a handoff of at most
3000 bytes (90 seconds by default via `OMS_PANEL_SWAP_HANDOFF_SECONDS`, capped at 300);
if none arrives, OMS uses a recorded-state digest. Before asking the main to exit, OMS
validates the target's durable owner/harness pair, prepares the replacement command and
rechecks the current room, attempt and window ownership.
Both the handoff request and the exit request recheck the original pane's process id,
live state, main attempt and native-pane binding immediately before typing. A changed
or unreadable binding refuses that input without transferring the room or respawning;
cleanup only removes options still owned by the verified attempt. The tmux comparison
and send remain separate operations, including the delay before Enter, so this is not
an atomic input guarantee.

OMS then exits the old main and relaunches the replacement in the same pane and room
without stopping workers. If preparation or a recheck fails before the exit request, OMS
removes that swap's handoff file so the preserved main can be swapped again. If the old
main does not exit, the panel preserves it and the room claims. A current Pi main can be
swapped without a resume binding: the replacement starts fresh. If replacement startup
fails or ownership cannot be verified, OMS does not restore Pi; it reports no verified
replacement main and retains the handoff file and room claims for recovery.

`--model` selects an exact installed catalog model; swap has no dry-run and requires
tmux.

#### Automatic quota swaps

The main board also requests this swap automatically when its fresh weekly quota
reading reaches 97%. Thresholds compare whole percentages as displayed: 97% for
the current main and 90% for targets. It chooses an installed alternative below
90% weekly usage, or an alternative with unknown usage, and waits 30 minutes between automatic
attempts. Set `OMS_PANEL_AUTO_SWAP=0` to disable this behavior. Five-hour quota
does not trigger a swap. Codex target ranking reads up to 64 recently modified
session logs and chooses their newest unexpired weekly event. Before handoff,
the automatic swap checks the target's weekly quota again. Below 100% usage it
postpones the swap if the main does not provide a handoff; at 100% it may use
the recorded-state digest. An active land does not hold the swap back; the
handoff names it. A postponed handoff does not start the cooldown.
Other refused automatic attempts, including a changed pane, retain the attempt
cooldown; that receipt does not claim an exit was delivered or a replacement started.

### Installation deferral while workers are live

Delegated workers and other panel calls also defer installation: `oms update`
exits 76 while a worker marker proves its owner is live, or while a call is
registered in the install checkout, current checkout, or a checkout named by
`oms land` (its repository, state root, receipt handoff and sync target).
A symlinked or unreadable call registry also defers; a malformed entry names how
to clear it. `oms land` keeps successful push and CI evidence, records
`update.reason: deferred-live-workers`, and prints a retry command. Under `--wait`, `oms
land` rechecks the deferred installation every 15 seconds until the workers finish or
`--sibling-wait` (default 1800 seconds) expires, so no manual retry is needed. Without
`--wait`, the run does not recheck; rerun the printed retry command (which includes
`--wait`) to resume installation. Auto-update records the deferral as skipped. This
check does not detect a worker that starts after installation begins.

### Amend an unclaimed task

The owner can correct an unleased ready or blocked task with `oms agent-plan amend --id
ID --expected-plan-sha256 SHA --expected-task-sha256 SHA [--allowed "p1,p2"] [--verify
CMD] --reason TEXT`. It records the old and new fields in task history and prints the
new task SHA. Live workers and proposal-bound tasks prevent amendment; dependents keep
their existing links.

### Run a planned goal

For a goal bounded by `PROJECT.md`, the main reads the user's request scope,
`PROJECT.md` and its References before planning. It proposes bounded tasks with
`oms plan-from-spec`, reviews the proposal, and applies the reviewed bytes with
`--apply` and `--expected-proposal-sha256`. Applied proposal tasks are ordinary
shared-plan tasks and use the same lifecycle as every other plan task: claim a
ready task in the confirmed `PROJECT.md` plan's order (dependencies first),
dispatch it with `--task-id`, verify and inspect the work, integrate
it with the required checks, run `oms agent-plan review` after acceptance, and
record the owner result with `oms panel --finalize`.

#### Acceptance and plan completion

For one-step owner patch acceptance, `oms panel --accept TASK --repo PATH --message TEXT
--verify COMMAND` admits and commits the reviewed patch and records the plan review;
after a published landing, `oms panel --finalize --landed-commit SHA` binds the owner
result to that accepted change. Acceptance treats a `--continue` chain as one candidate:
its latest round supersedes earlier unadmitted rounds, and its patch still needs a
matching passing review gate. Choose the role from the task's scope and source, rely on
the automatic reviewer gate, and add a critic or security-auditor when the change
warrants one.

Use `oms agent-plan add` only for bounded gaps outside the reviewed proposal.
Dependencies become ready automatically once every prerequisite is `done`: after an
authorized land, use `oms agent-plan finish --id ID --landed-commit SHA`; after
patch-land, use `finish` with its expected landing receipt. Eligible verified local
research completion uses `oms agent-plan accept-main-research`. Owner `--finalize`
records the result but does not itself mark the plan task done; the plan makes
dependents ready automatically, so do not manually release them.

`oms autopilot run` remains the headless workflow.

#### Existing autopilot campaigns

When the user asks the panel main to complete a project, or this repository already has
an autopilot contract, the main supervises that campaign instead of starting a competing
panel plan driver: it checks autopilot status and the receipt before acting, reviews the
exact proposal digest before running, and uses only `oms autopilot propose`, `run
--review-mode gate`, `status`, or validated receipt re-entry. It does not dispatch panel
write workers for campaign tasks, release or reclaim them, reset cycle exhaustion, or
override a parked receipt. Read-only reviewer, critic, or security-auditor judgment can
inform proposals, parks, and final changes.

Accept only after gated whole-change review and fresh acceptance. Without a campaign,
ordinary panel plan execution above is unchanged. To start without a panel, run `oms
autopilot propose`, review its exact proposal and digest, then run with `--review-mode
gate` and `--expected-proposal-sha256`. An autopilot campaign runs from the main
checkout on its own `oms/autopilot-*` work branch, the sole exception to the
no-commit-in-shared-checkout rule; the main does not commit there itself, and
publication still needs authority (Draft PR or `oms land` from that branch).


#### Parked campaigns

For a parked campaign, `oms autopilot status` puts the recorded phase, reason,
semantic verdict and evidence path first, so a failed or incomplete review is
not presented as a completed drive.



### State and execution repositories

Use the shared `OMS_ROOM_REPO` for plan and result records when editing elsewhere.
For `--dispatch`, it selects the state repository for the room, main attempt,
and call results; `--repo` still selects the source and execution repository
for workers, researchers, advisors and reviewers. Explicit reviews inspect and verify
that source repository. Automatic patch reviews instead construct a private checkout
of the worker's recorded base plus its digest-verified patch; both the complete diff
and mechanical gate use that candidate. Missing base objects, digest mismatches or
patch-application failures require manual review; they never fall back to testing
the unpatched source. The gate rejects candidate changes by the verifier.


#### Automatic patch review

Automatic review uses routine for patches containing only `.md` or `.rst` files under
`docs/` or top-level `README.md` and `CHANGELOG.md`, excluding `AGENTS.md` and
`CLAUDE.md`; otherwise it uses deep. Unknown or unsafe paths and trust-boundary patterns
also use deep (`_AUTO_REVIEW_FRONTIER_PATHS` in scripts/lib/panel_review.py: plugins,
hooks, install/uninstall, land and patch scripts, workflows, rules,
permission/settings/policy files). Patch admission reports `security: ADVISE` with risk
paths and auditor guidance, or `SKIP` when none are found; this advisory does not change
the `ADMIT`/`REJECT` verdict.


#### Review triggers and findings

A write call with a digest-verified patch, no failed verify and no guard stop starts
this review automatically when it exits 0. If the worker exits nonzero, the recorded
call verifier must actually have run and passed; the worker failure remains visible and
review does not imply acceptance. Timeout and signal exits do not start automatic
review. The reviewer is told that a failed worker may have left a partial patch, and its
completion hint requires owner judgment. The candidate patch must also apply for the
review gate to run (`auto_review` in config/panel-routing.json, default true).

It reuses the call's `--verify`, else the plan task's, and mails the main for manual
review when neither exists. A completed FAIL verdict with findings drafts a `--continue`
fix brief and retains its manual command. After releasing reviewer settlement, the
controller reserves and launches one automatic continuation through `oms panel`, and
posts the task, review artifact and fix brief to the owner. The reservation is durable
on the writer attempt (`panel_auto_rework` names the reviewer run) across the task's
main lineage; replay cannot launch it again. A controller exit after reservation may
lose the launch; the fix-brief notice remains for owner recovery. A second blocking
review needs design judgment: the main runs `oms panel --council` over the findings,
records consensus and dissent, then decides whether to dispatch a narrowed brief, fix
directly, or finalize rejected, and implements the decision. Worker failures and guard
stops require authority judgment: the main acts within its delegated authority and asks
the user only when the next step exceeds it or is ambiguous.
Continuation retains the task's WIP slot. A refused or failed continuation is reported
and leaves the manual notice available. It runs in the writer's recorded execution
repository; room state stays in the state repository, and the fix brief is passed by
absolute path from that state repository. Gate crashes, failed verification and reviews
without findings never launch rework. Gate reviewers make a full pass over the scoped diff
and report every evidenced blocking finding. A follow-up gate receives screened findings
from the last completed failed gate's final provider attempt and checks whether each
remains blocking. The carried findings are bounded to 3000 bytes; an omission notice
links the previous artifact when findings exceed that limit.


#### Room selection

`--room` selects a room without
changing `OMS_ROOM_REPO`. With no `OMS_ROOM_REPO`, that room resolves in `--repo`.


#### Owner result binding

An owner result saved with `--finalize --outcome completed --verify COMMAND`
binds to the current plan and task. Plan displays **completed locally** for a
matching passing result, including one recorded outside a panel session, while
retaining the plan's review/acceptance state. Changed task generations, replaced
plans, modified evidence and a later unverified result invalidate that display.
Use `--verify-timeout SECONDS` to set the finalize verifier wall time
(1–86400 seconds; default 1800).
Worker, review-gate and patch-admission verify records include elapsed seconds.
The verifier runs outside the shared result lock; finalization rechecks its
snapshot before writing.
Historical results without the binding remain available in Results.
`--outcome failed` asks for rework, so the task's review and blocked calls stay on
the board; `--outcome rejected` withdraws the work and moves them to Past. A review
whose task continues in a later `--continue` round also moves to Past.

#### Landing receipt handoff

After an
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


#### Refresh the shared checkout

To refresh the shared checkout's scripts after a passed land and handoff, add
`--sync-shared PATH` with an explicit checkout path. The separate refresh respects
live-call locks and refuses dirty or diverged trees; deferred/refused sync is
reported in land status without failing the passed land. Its companion sync
receipt preserves the original handed-off landing bytes. Without this flag,
handoff leaves the checkout untouched.

### Start a main in an existing panel

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

### Resume and restart

Fresh Claude mains receive a panel-generated UUID via `--session-id`, retained as the
attempt's `native_session` reference. Resumed Claude mains keep `--resume` and that
reference, so restarting keeps the same room participant and unread mail. Resumed Claude
mains pass `--system-prompt-snapshot off` so they rebuild their appended prompt instead
of reusing the stored one; this needs Claude Code 2.1.257 or later. When supported by
the installed Claude CLI, panel Claude mains also use `--autocompact 300000`. When
supported, they pass `--disallowedTools=Agent` to route delegation through the panel.

After `/clear`, restart and explicit resume follow the participant's current bound
session; the original join boundary and mailbox remain intact. Codex starts with its
existing flags; a recorded native session can be resumed. After a native main exits in
its panel window, the chat pane shows `r resume/restart here · Enter close window`.
Press `r` to relaunch in that pane, resuming its recorded session or starting fresh when
no session is recorded. Restart does not resend the original `--task`, even when
starting a fresh session. Fresh Codex restarts start a new session even after the
previous session's first hook bound its room consumer; that binding is checked only when
resuming a stored session.

The previous attempt must be terminal, and startup rechecks the sole-main limit and
resume ownership under the main-session lock. Another live main or unverified ownership
refuses restart and returns to the prompt. Enter closes the whole window, including its
board watcher, only while the attempt tag and repository still match and the native pane
marker is unset; otherwise the window is preserved. Close pins the window id and
rechecks the repository, attempt and native marker on the tmux server at deletion.
Repository paths containing `#`, commas, colons, braces, backslashes or control
characters preserve the window with a notice because they cannot be safely embedded in
that guard.

Failed ownership queries preserve the window. SIGTERM/SIGHUP at the prompt restore
terminal modes before exiting with 143/129. Without a terminal, native launch retains
its existing exit behavior.

Panel owner fields and `--owner` for dispatch, finalize and review settlement accept
`claude`, `codex`, `pi` and `antigravity`. The resolver records Claude and Codex under
their own identities. Pi's default model and effort come from the Codex main preset; an
explicit model in the full Codex catalog resolves to the gateway with owner `codex`. Any
other exact Pi model resolves to direct transport with owner `pi`. The `agy` harness
resolves to registry owner `antigravity`. Deep and frontier tiers also offer read-only
Antigravity routes after native providers with known weekly quota. Routine independent
judges can also use the deep-tier Antigravity route.

Reviewers, verifiers, security auditors and critics of Codex or Claude work take
Antigravity first while it is installed, not in daily cooldown and below 90% of any
recorded weekly quota; the judged family is excluded unless the owner pins it. Independent
review uses the linked writer's recorded model family, or the main model family when no
writer is linked. Matching retries from one provider can share a resolved family; links
across providers or families are ambiguous. An unknown linked family cannot establish independence. Existing Pi rows with owner `codex` remain readable. Swap events
and restart ownership validate the recorded owner/harness pair without consulting the
current model cache; Pi permits recorded owners `codex` and `pi`, while `agy` uses
`antigravity`.

A default Pi swap retains the default gateway selection even when its cached catalog is
empty. Registered native-adapter mains retain their registry identities for watchers,
dispatch and council. Their registration fallback remains separate from worker tiers.

Direct Pi launch is deferred; its launch dry run reports the resolved owner with
`argv: null`. Fresh Agy mains use `agy --prompt-interactive` with the OMS bootstrap,
room contract, unsettled lineage and task as initial user-message context. The task
appears once as JSON data. This is user context, not system injection. Agy keeps its
native model and permission settings; only an explicit `--model` adds a model flag.
The owner is `antigravity`, while the harness and CLI are `agy`. Discovery advertises
native launch without executing the CLI, and dry runs show the actual fresh-launch argv.
Agy `--resume` is refused before launch or enrollment: its native session ownership
cannot yet be verified. Restarting an exited Agy main starts fresh; no conversation ID
or history binding is invented. Native histories remain separate.
The available Pi launch uses the existing `oms harness pi`
bridge and requires Pi 0.80.4 or newer (the bridge waits for `agent_settled`)
and Codex CLI on PATH. Pi owns its tools and UI. Pi history is separate: this integration refuses
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
Call rows keep status, token totals and age aligned; fresh calls show their
recorded role and queued state before an attempt exists. Detail parses live
provider-stream activity for running calls and shows a What it did summary for
finished calls; tree rows include the latest event.

| Board area | What it shows |
| --- | --- |
| Goal | Plan goal and verified task count |
| Needs you | Calls and messages that need an answer or decision, including failures, holds and delivery errors |
| Activity tree or graph | Main and delegated calls, their status and latest events |
| Session log | Scrollable recorded activity |
| Bottom tabs | Log, Detail, Plan, Messages, Past and Attention; Debate while a debate exists |

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
| `r` / `e` / `d` / `x` | — | Raw / Events / Diff / Transcript view |
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
| `/` | Filter the call rows: type words to narrow by title, role, provider, model or state (every word must match, case-insensitive); `Enter` keeps the filter, `Esc` clears it, arrows still move. The heading shows `Filter /text · shown N of M`. The Attention header and counts are never filtered, and a hidden call that waits on you is reported as `N waiting hidden`. Resets when the room changes | Search result text |
| `z` | — | Zoom the tmux reader pane when width is under 120 columns or it is already zoomed |
| `Esc` / `q` | `Esc` closes detail or returns; `q` quits an inline watcher, does nothing on a managed board, and detaches from the control menu | Close reader |
| `?` | Show/hide board key help | — |
| `Ctrl+L` | Repaint the board | Repaint the reader |
| `F5` / `F8` / `F9` / `F12` | Control window (when present) / cycle layout / swap chat-board focus / panel-wide key list in tmux | — |
| Shift-drag | Copy terminal selection in a managed panel | — |
| Mouse click | Select a row/tab; graph calls and the main preview their cards | — |

At the native main's exit prompt, `r` resumes/restarts there and Enter closes the window. The terminal/job workspace has its own controls; this table covers only the board and result reader.

A dispatched call settles only after its supervisor proves the whole process tree
exited. On macOS and Windows, where that supervisor is unsupported, the call stays
pending and the panel prints a manual-clear hint; independently stop every provider
descendant before clearing its retained scope.


### Terminal hosts

`--host` selects the terminal host independently of the activity view.

| Host | Behavior | Needs |
|---|---|---|
| `auto` | Reuse an existing local POSIX Herdr pane; otherwise use layout selection. | Existing host environment |
| `inline` | Disable host automation. | No terminal multiplexer |
| `tmux` | Explicitly select split layout. | An already-installed tmux |
| `herdr` | Use the current owned Herdr pane. | `HERDR_ENV`, `HERDR_PANE_ID`, owned `HERDR_SOCKET_PATH` |

Herdr is optional: OMS never installs or starts it. Its Windows named-pipe
transport is not implemented. Native Windows is unsupported; WSL2 uses the
Linux host path.


In a Herdr repository pane, the menu opens a fresh tab containing the native
OMS launch wrapper and a task panel. It does not type into an existing
agent. The API uses bounded local JSON requests and never retries a mutation
whose outcome is uncertain. Original-chat navigation reconciles active OMS
main ownership and exact pane/terminal/socket identity; reused panes and
unproven sessions are rejected. Numeric footer readings retain their reported
scope and unknown values. A socket error does not stop the native sibling.

### Layout modes and resizing

The board stays on the left in a landscape window; it moves above the CLI when the
window is too narrow or physically tall (terminal cells are about twice as tall as wide,
so rows × 2 ≥ 1.1 × columns, with at least 41 rows), and returns left when it is wide
enough again (4 columns past the narrow limit and rows × 2 ≤ 0.9 × columns; between the
two a window keeps its current edge) (28 percent of the width or 45 percent of the
height the first time, sized from the terminal that is attaching; optional integer
`OMS_PANEL_BOARD_PERCENT` overrides the landscape default, clamped to 25–35 percent,
with invalid values ignored).

In Work mode, a landscape default or scaled record is capped at the smaller of 38
percent of the window and the width that leaves 120 columns for the CLI; the existing
minimum split rules apply on narrower windows. A new landscape board stays unsplit when
20 board columns, the divider and 40 CLI columns cannot fit; a portrait split needs 20
rows on each side of the divider. A deliberate border drag remains in effect for that
Work window and is recorded there. A call opened from the board shows in a reader pane
above the CLI. Herdr uses the same left board, starting at the same 28 percent share and
honoring `OMS_PANEL_BOARD_PERCENT`; the tall-window rule is tmux-only and Herdr's
placement is unchanged.

Later Herdr resizing is not recorded or shared; `SplitSync` synchronizes the board width
or portrait height only in tmux. In tmux, each window records its board edge and size in
its window option `@oms_panel_split` (`left:<cells>c` or `top:<rows>r`; the older
percent form is converted on first sight). Resizing recomputes the portrait/landscape
edge with a small hysteresis band and scales the recorded size with the window, clamped
to keep at least 20 cells for the board and chat. A flip moves the existing board pane
without moving focus, preserving pane ids; dragging the border sets the size followed by
later resizes.

Press `m` on the board or `F8` in tmux to cycle Work, Overview and Focus. Overview gives
the graph about 62 percent of the landscape width without changing the Work record.
Focus zooms the CLI pane; leaving it unzooms and returning to Work restores its recorded
width. SplitSync stays idle while Focus is zoomed and does not apply the Work record in
Overview. Graph rendering falls back to the tree when the board is too narrow for the
graph. The control window and other zoomed windows are left alone. A watcher whose OMS
sources change re-executes itself once they compile, so updated boards need no manual
restart; an open reader stays.

Select `v` in the task panel to expand its graph; `Esc` or watcher shutdown collapses
it. Native input and permission prompts stay in the original CLI. The room's detailed
tree and graph remain available. A successfully exited native main is labelled `exited`,
independently of a worker result, verifier or owner acceptance.

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
usage line uses `W` for weekly quota left and `C` for main context used, and keeps a
reported 5-hour window as `5h N%` without its reset countdown. Activity keeps
main/role branches, wraps task titles, separates repository tasks, and closes
the viewport border even while scrolling. Empty declared scopes are omitted.

### Control menu and focus

The control menu is an inbox. **Needs you** lists up to 12 items, newest first, each as
glyph, who (`Sol 6.1 worker for Opus 5.5`), what and age: failed or timed-out calls with
their title, calls or mains waiting for approval or input, unanswered room messages that
a main sent to `all`, and finished write workers whose patch has no admission or landing
record. Arrow keys or a click select a row; the wheel moves by lines and page keys by
the visible item count, stopping at either end. Enter (or a click) opens that main's
chat or shows the call's result, and Esc returns. If a refresh removes the selected
row, the cursor stays near its place and the next Enter only says the selection
changed; Enter again, or any navigation key first, acts on the row now selected.

An empty list says `Nothing needs you right now`; a room with no main shows `Press 1
(Claude) or 2 (Pi) to start the sole main`. The menu keeps start (`1` Claude, `2` Pi,
`0` Codex, `3` Resume, `t` Task), `o` rooms, `9` refresh and quit/detach; board keys are
not repeated. Enter `?` for all other actions in ordinary scrollable help (chats,
results, views, advisor, review, council, finalize); existing shortcuts still work. Menu
keys act at once, without Enter, on a POSIX terminal with raw input; the board's key
handling is reused. Text prompts (a task title, a provider) stay line-based inside that
prompt only.

Elsewhere, such as Windows Git Bash or redirected input, the menu reads `oms> ` and
needs key + Enter. The inbox refreshes every five seconds. The watcher retains its
arrow, mouse and page-key navigation. In a managed tmux control pane the quit key is
labelled `Detach`; it preserves the native sessions. In a coupled tmux board, `F9` swaps
focus between the chat and the board and keeps both alive; `q` does nothing there, and
Esc or `v` un-zoom the board only while it is zoomed. Standalone watchers retain `q` to
exit. If a board was explicitly removed, `oms panel --reopen` restores it without
restarting the native CLI.

Repeating the operation preserves an existing board. Inside an OMS window, bare `oms
panel` does the same; the control menu also provides `z` for this action. Restore
accepts only this checkout's front door with a matching repository, refuses
ambiguous/replaced input panes, and does not restart a terminated CLI.

Client detach retains the host's live processes. A Herdr server restart is
different: bare vendor auto-restore is not an OMS resume and cannot prove
fresh OMS ownership. Continue through the OMS wrapper's existing exact-resume
checks after its previous execution is confirmed terminal. An unknown shutdown
remains blocked; pane IDs or a Herdr `done` badge cannot authorize takeover.
This adapter does not change Herdr's global configuration or restore policy.



### tmux sessions

Outside Herdr, `auto` chooses split on POSIX when tmux is installed, including
`--launch` and runs started inside another tmux session (the client switches to
the panel instead of nesting); otherwise it uses inline. `--host inline` or
`--layout inline` keeps a run in the current terminal.

#### Session identity

Each checkout has one OMS panel
session, `oms-<project>`: the checkout's directory name lowercased, runs of other
characters turned into `-`, at most 32 characters (`project` when nothing is left). When that
name is held by another checkout or by a session OMS did not create, the panel uses
`oms-<project>-` plus the first 6 hex digits of the SHA-256 of the resolved path, and neither
session is taken over. A live legacy `oms-panel-<12 hex>` session proven for the checkout stays in use
under its name, because its processes carry that name; new panels use the new name. The first `split` creates it with a control window and records the
checkout on the session.

#### Default main selection

A new or empty panel offers Claude, Codex and Agy, with missing-CLI reasons. The
startup prompt accepts `1`/`claude`, `2`/`codex` and `3`/`agy`; Enter or EOF selects
no main and leaves usable control. Ctrl-C exits with 130. These prompt numbers are
separate from the existing control shortcuts (`1` Claude, `2` Pi, `0` Codex).
Choosing control only does not allocate or bind a room to a roomless panel.
In a managed roomless control, numeric start/resume keys and `t` remain blocked.
Use `o Rooms` to select or create a room first, or `p` Providers then Native.
Waiting for another poll does not select a room; a failed room query is reported
separately from an intentionally unset room.
An explicit `--launch` wins, followed by a set `OMS_PANEL_MAIN` (`claude`, `codex`,
`agy` or `pi`), then the picker. Empty or whitespace-only `OMS_PANEL_MAIN` opens no
main; an invalid value opens none with a warning. `OMS_PANEL_HARNESS` selects only
the control menu's harness and grants no startup launch authority. Inline and Herdr
use the same choice; selecting none opens their control menu. No preference is saved.
Generated tmux control panes enter that persistent menu directly, including after a
reload; they do not repeat the outer startup picker. Herdr keeps the control menu
running after opening a chosen main in a sibling tab and retains the menu on reload,
including when no main was selected. Explicit `--launch` and standalone
inline native launches keep the native session's lifetime.

Read-only JSON, watch, dry-run, dispatch, spawn, swap and reopen paths never prompt.
Non-terminal startup keeps the existing terminal-required error. A proven live or
unproven main is reattached without a picker or new launch. Provider choice and any
supported crash-resume offer happen outside construction/session locks; launch
rechecks sole-main and legacy ownership under the existing guards after input.
If an ownership query or start lock fails, startup reports that refusal rather than
reporting an empty selection or a missing CLI.


#### Reattach and concurrent launches

Reattaching opens a main the same way only when the panel has no live main, under the
same start guards; when none can open, the control view shows why (for example an empty
`OMS_PANEL_MAIN` or a missing CLI). A main window whose startup fails waits for Enter
before closing, so its error stays readable. Later launches attach to it (or switch the
current tmux client), and `--launch` adds a native window there instead of opening a
second session. A session with that name recorded for another checkout is refused. A
launch that loses a creation race, or arrives before a just-created session has recorded
its checkout, waits up to two seconds and joins it; the owner marker is written after
the session's room, so a joining launch follows that room.

New mains default to the panel's recorded room while it is open and has
participant/message headroom; otherwise, or when `--room` is given, the existing room
rules apply. Rooms stay bounded and task-scoped; the panel outlives them. Separate tmux
servers are separate scopes.
Crash resume validates the native session's room before creating a window. A
roomless panel adopts that binding; a conflicting selected or recorded room is
refused. A bound resume keeps its room even when a fresh main would rotate rooms.
Automatic adoption is deferred until construction rechecks the recorded room.
If another caller records a different room while startup waits, that room is
preserved and the resume is refused; an original explicit room selection keeps
its precedence. Native ownership uses the recorded harness and participant even
after a window is renamed; malformed or unproven window inventory cannot prove
that a panel is empty.

#### Refresh and key bindings

Each native conversation gets
its own sidebar with state refreshed every five seconds. Existing windows and tmux settings
are preserved, with one addition: `F9` selects the other pane
of the window, `F8` cycles layout modes, `F5` opens the control window and `F12` shows
the key list, all without the prefix (F10/F11 belong to terminal menus). tmux key
bindings are server-wide, so the binding acts only in sessions that carry the panel's `@oms_panel_repo` marker,
passes the key through unchanged in every other session, and is not installed when the key is
already bound by the user (a binding OMS installed earlier under the old session-name test is replaced). With default bindings, `Ctrl-b w` picks a window and `Ctrl-b d`
detaches while work continues. Quit in the control pane also detaches.

#### Failed construction and inline exit

Failed
construction rolls back only the new OMS session or added window. Inline returns to the menu
when the native CLI exits and refreshes then; it has no permanent sidebar.

### Activity and usage

The board has one MAIN card for its live main, with its team grouped into ADVISORS,
REVIEWERS and WORKERS. Team counts and elapsed time appear with the main summary; call
rows show state, model and task title. Calls remain attached to their recorded parent,
and each row opens its reader. The repository control view shows `NEXT MAIN` as a
one-line preset; that preset never fills missing model/effort fields in a recorded main.
The native main card shows provider, requested model/effort and recorded task when
supplied at launch. Missing fields remain unrecorded.

Tasks entered later inside the native conversation are not intercepted: its main title
stays unrecorded unless supplied at launch, while dispatched child titles are recorded.
The palette uses warm Claude, blue Codex, teal workers and lavender advisors and
reviewers. Role cards show lifecycle state, model, task title/ID, read/write authority,
effort and location. Worktree locations use a relative unique suffix; repository calls
show `repository`. Detached worker worktrees can be cleaned after completion; their
locations remain history. Task titles have their own bright lines, wrapping to two lines
in wide views and up to three in narrow sidebars.

Main task titles wrap too. Internal task IDs stay in wide metadata rows or Details.
Details gives bounded titles separate wrapped lines, so long titles need not compete
with location and purpose. No absolute home paths or transcripts are added to panel
metadata. Up to four background rows sit below the board: `Autopilot:` with the
receipt's recorded stage and age, `Graph:` runs started within 24 hours (marked
unconfirmed), `Queue:` tsp and fallback jobs whose enqueue recorded this project's
physical checkout, and `Install pending:` with the short SHA and recorded reason while
the newest land pushed but did not install.

They are recorded evidence, not process liveness; a failing source drops only its row.

A work health line sits under the goal box on boards 16 or more rows high: `▶ running 2 · ⏸ waiting on
you 1 · oldest 25m · ✗ failed 1 · ✓ done 3 · plan 6/9 tasks · ! 2 unresolved · CI ✓ passed 3h ago · ctx 73%
used`. It is assembled from records the board already holds: current (not yet settled) calls by state, the
Attention queue's `needs you` count with its oldest wait, plan progress, the actionable count of `oms
fail-ledger list --unresolved`, the newest `.oms/ci.jsonl` row for the branch and the live main's context
reading. A segment appears only when its source is recorded and has something to say (zero counts drop;
`waiting on you` takes the attention colour), and every segment carries a glyph or word besides colour
(ASCII: `> running`, `! waiting`, `x failed`, `+ done`). From 100 columns and 24 rows a line that does not fit
splits in two; below 80 columns the words shorten, below 50 they shorten again, and whole segments drop
lowest priority first (done, context, plan, unresolved, CI, running, failed; waiting on you last). A room with
nothing recorded shows no line. The two ledgers are read on the 5-second collection only, and only when
their file changed. The CI part is an observation of the last recorded run, not a verification of this
checkout, and the line shows no quota or tokens.

A `↻ stalls N` segment (ASCII `! stalls N`, attention colour) counts the findings of `oms fail-ledger list
--stalls`. The panel calls the detector library on the failure ledger and goal-drive progress log in-process
(no subprocess), parses them again only when a file changed, and lets a finding age out by
`OMS_STALL_MAX_AGE` on its own. It ranks just below `waiting on you`, above failed. It is an observation of
recorded repeats, not evidence that work failed. The same line, with the same widths and drop order, also sits
under the goal box of the graph view (28 rows or more, where the call tree keeps its rows) and in the control
window's menu header (16 rows or more, dropped first when the inbox would get short). `oms panel --health`
adds a `Work:` row to its text and a `work` object (`segments`, `stalls`) to its JSON.

Activity is organized under the window's sole live MAIN: ADVISORS / REVIEWERS / WORKERS,
followed by each call and its task title. Main rows can show `? N open` questions and
`cN%` context used. When room history contains earlier mains, they fold to their status
rows automatically; the selected main and the main owning the selected call stay
expanded. The control window renders the Needs you attention inbox; the main board
carries the MAIN card and its team counts. Legacy calls without explicit panel ownership
remain unlinked; a generic attempt parent is never promoted into a native-main
relationship. A linked owner whose metadata is unavailable is labelled so.

Press `v` to toggle board expansion between the current overview and an expanded graph;
`Esc` or closing the watcher collapses any expansion. In compact layouts, calls retain
their model, state and task title. At 80 columns or wider, these share one row with
metadata on the right; narrow views place metadata below the title. Detail expands
wrapped titles and location/effort metadata. The view prioritizes attention calls and
reports overflow when calls do not fit. Small panes use compact summaries. Press `b` to
toggle attention-only: waiting input, waiting approval, blocked, review and orphaned
calls, plus retained failed terminal outcomes.

Filtered calls are counted rather than changing their states. Settled calls stay in the
Past tab and are hidden from the board; attention-only empties the Past tab as well.
Both controls affect presentation, not the main identity or dispatch authority. A native
window inherits these settings when opened, and the control sidebar follows subsequent
menu changes. CLI `--view` overrides a watch's window density; `--attention-only` keeps
its filter enabled. These flags apply only to the menu or watch, not model operations.

The usage line has one `Claude` reading and one `Codex` reading. For each provider, the
selected window's exact main attempt wins; otherwise the live, then newest, matching
main is used. Claude reads its session HUD cache first, then the proven pane footer when
the cache is absent/stale and to fill missing fields; Codex uses its native reading or
proven pane footer. Missing readings show `--`. Claude's existing session HUD cache
supplies reported context and optional `seven_day` and `five_hour` usage, with a
ten-minute TTL and expired reset values suppressed. The consumer hash selects that
session's cache; no newest-session guess is made.

Where the row has room, each reported quota percentage carries a five-cell bar of what
is left: `5h ▮▮▮▮▯ 77% left (2h10m) · week ▮▮▮▯▯ 58% left` (`[####-]` without Unicode).
The bar's width never changes with the value; any quota left keeps one filled cell and
only an untouched quota fills all five. An unreported percentage draws no bar, and
context, which counts up, has none. The bars are the first thing a tight row drops,
before the long words and the countdown; `oms panel --health` draws the same bars on
its weekly and 5-hour rows from 48 columns up, and `--health --json` carries none.

When Claude reported a 5-hour window, the reading leads with it:
`Claude 5h 77% left (2h10m) · week 58% left · ctx 62% used`. The percentage is the
provider's own, never one derived from token counts, and the countdown is the time to
the reported reset. Without a reported window nothing is added to the line; a window
whose reset has passed is dropped rather than shown as current, including on a held
frame; a reset further out than the window is long shows the percentage without a
countdown. Only the session HUD cache supplies it: the pane footer is not parsed for the
5-hour field because a footer cannot show whether its window already rolled over, and
Codex has no 5-hour reading here. The window is information only; it starts no main
swap and raises no board alert.

`OMS_QUOTA_WARN_PCT=N` (1 to 99, percent used; unset or `0` is off) draws your own line
on the reported windows. While a current 5-hour or weekly reading is at or past it the
board carries `! Quota past your 80% line: Claude 5h 82% used, resets in 40m`, and the
crossing counts as one attention event for `OMS_PANEL_ATTENTION_BELL` and
`OMS_PANEL_ATTENTION_CMD` (class word `quota` in the payload, no percentage or provider
name). A window that stays past the line warns once; it re-arms when the reading drops
back under the line, which is what a reset does. Only a percentage the provider reported
counts: an unreported, stale or rolled-over window says nothing, and nothing is derived
from token counts. It is an advisory: it changes neither the health level nor any
session, and starts no swap.

The same line is read once when `peer-delegate.sh` starts a (non-dry-run) dispatch: if the
target provider's last reported window (Claude 5-hour or weekly from the HUD cache, Codex
weekly from its session log) is at or past `OMS_QUOTA_WARN_PCT` and was reported within the
last 10 minutes, one stderr line names the window, the percentage, the age and the first
other provider configured for the role's tier in `config/panel-routing.json`, for example
`oms: quota hint: claude 5h window 91% used (last reported 2m ago, resets in 49m); configured
alternative: codex gpt-6.1-sol (deep tier); dispatch continues unchanged`. Nothing is
recorded, no provider is swapped and the dispatch is unchanged; a missing, stale or
rolled-over reading prints nothing.

When the Claude status line payload carries the last request's `current_usage` token
counts, the HUD cache also records `cache_read_share`: the percentage of that request's
input tokens (fresh, cache-creation and cache-read) the provider read from its prompt
cache. `oms panel --health` lists it as `80% of last input [reported] Claude prompt
cache read`, and `--health --json` as an advisory item of kind `cache`. It is a ratio of
reported counts for one request, not a quota, a price or a cache-lifetime timer; with no
counts reported nothing is listed, and it has no warning band.

The task board replaces that line with a provider strip for Claude, Codex and Antigravity (the
control menu and the reader keep the line above). At 80 columns and 24 rows or more it is one
row per provider: `Claude      ▮▮▯▯▯ weekly 41% used · resets in 3d4h · 5h 23% · 1 live`. The bar
and percentage are the tightest fresh window (the one with the most used), then the time to its
reset, the other window and the count of live seats (the main plus workers running or waiting,
not advisors or reviewers). A shorter or narrower pane folds it into one row, at 60 columns
`C ▮▮▯▯▯ 41% 3d4h ·1 │ X ▮▮▮▯▯ 62% 3d4h ·1 │ A — ·0`, which drops its bars, then its full names,
as the width shrinks; under 56 columns it is `C 41% · X 62% · A —`, so the MAIN card keeps its
place on a 12-row pane. A trailing `~` (or `stale 11m`) marks a reading older than the ten-minute
HUD freshness limit; the percentage is of quota used, and `!` marks a window past your
`OMS_QUOTA_WARN_PCT` line and `!!` one with 10% or less left (the health screen's own band),
neither on a stale reading. Provider names take the palette's warm main colour (Claude), counsel
blue (Codex) and lavender judge colour (Antigravity); the marks carry the meaning without colour.

`oms panel --usage [--json]` prints the snapshot behind the strip and exits (schema 1: per
provider the account label, every reported window with `used`, `resets_at`, `resets_in`,
`age_seconds`, `source` and `stale`, the context, the cache-read share and the live seats). It
reads only what a provider already wrote into a file OMS reads today and calls nothing. Every
missing value is `{"unreported": reason}`, never a zero: Claude's windows and cache share come
from the status-line HUD cache of the selected main (a record over ten minutes old shows its age,
not its numbers), Codex's windows from the `rate_limits` of the newest session log (the
10080-minute window is `weekly`, the 300-minute one is `5h`, and any other length is listed as
`<minutes>m` rather than guessed at; each needs a 0 to 100 percentage and a reset still ahead,
and the tightest fresh one leads the strip, as for Claude; a Codex 5-hour window raises no `!`
cue and is not in `--health`), and Antigravity reports no quota window locally, so it shows only OMS-recorded token
counts of the calls OMS delegated to it (7 days), not a quota. No provider records a per-day
token total here, and quota is never estimated from tokens.

A proven existing tmux pane can supply numeric fields from its last eight visible native
footer lines. `TUI` or `HUD/TUI` labels those mirrored readings; native output may lag
while idle and changed footer formats become unknown. The watcher performs no native
transcript/registry scan or account request for these readings. Header collection runs
once per five-second snapshot, never per animation frame. Ended mains have unknown
current context.

The separate `USAGE` area groups reported tokens by model using only the existing recent
eight repository artifact records. It stays repository-wide when the activity view is
scoped or filtered. A short wide pane uses a single usage strip; taller panes use a
small card, with reported USD cost when height is at least 40 rows. `x` counts retained
call records, not retries or native turns. `(sel)` identifies a selected model whose
served identity was not reported; served identities and selected identities are separate
groups. Mixed/fallback operations stay in `Mixed models`, and unknown models stay
unknown.

`?` means unreported; `+?` means a known subtotal plus unreported records, while a
reported zero stays zero. Overflow reports omitted model groups. Owner results, review
gate summaries and council syntheses are excluded, and lifecycle usage is not added
again. These are provider-reported partial observations, not account totals or remaining
quota; they do not supply the provider header. Provider token definitions can differ,
particularly for cache input; no cross-model total is shown. The window can change or
shrink as new artifact records arrive.

#### Provider coverage

The machine-checked source for provider support is `config/provider-capabilities.json`,
shown by `oms doctor --providers`; its evidence names are verified by
`tests/provider-registry-smoke.sh`. Where this table and that file disagree, the file
wins; this table keeps the panel-specific detail below.

Static reading of the code and fixture tests; no live-native verification. Every
"fixture" test uses fake native files or a fake tmux, so each cell is fixture-only or untested.
`impl` = implemented, `part` = partial, `unsup` = unsupported. Anchors are `panel_native.py`
(PN), `panel_chats.py` (PC), `panel_state.py` (PS), `room_view.py` (RV). Fixtures are in
`core/native-integration` (NI) or `core/room-and-board-rendering` (RB).

| | launch | observe | subagents | usage | attach | resume | stop |
|---|---|---|---|---|---|---|---|
| Claude | impl: PN `native_command`; NI | part: PS `main_activity` (hook file, not liveness); NI | impl: PC `native_subagents`, `_native_agents`; NI, RB | part: PC `native_advisors`, `_transcript_usage`; NI | part: PC `plan` (existing tmux pane; `--desktop --resume` on macOS only); NI | impl: PN `_native_resume_binding`; NI `startup-crash-resume` | part: PN `_run_native` `stop_native` on panel exit, no stop control; not tested |
| Codex | impl: PN `native_command`; NI | part: PS `main_activity`; NI | part: PC `_codex_native_agents` collects, RV `native_subagents` renders Claude mains only; NI | part: PC `_codex_usage`, `current_models`; NI | part: PC `plan` (pane or `codex://` link); NI | impl: PN `_native_resume_binding`; NI | part: as Claude; not tested |
| Pi | part: PN `native_command` (gateway transport only; direct refused); NI | part: activity forwarded by the Pi extension to `hook_state.py`, no native reading; not tested | unsup: no reader in PC | unsup: no reader in PC | part: PC `plan` pane only if registered; not tested | unsup: PN `native_command` refuses until ownership is verifiable; not tested | part: as Claude; not tested |
| Agy | part: PN `native_command` (fresh only); NI `agy-fresh-main` | unknown: no Agy hook reader found; not tested | unsup: no reader in PC | unsup: no reader in PC | part: PC `plan` pane only if registered; not tested | unsup: PN `run_single_native` refuses; NI `agy-fresh-main` | part: as Claude; not tested |

`oms dashboard` is managed-only; the panel also reads native session files.
By default the panel shows the registered main, its linked subagents and (for Claude and
Codex mains) their native usage. External sessions, other repos and other hosts are not
read: `native_index` matches only enrolled consumer hashes inside one repo's room.
Native rows (subagents, advisor) are read-only and are never participants, task owners,
leases or acceptance evidence; a recent file write is shown as `active`, never as liveness.

`oms panel --metrics` reports retained owner decisions for write tasks by provider and
model, counting a task once per model it touched and grouping attempts without a
recorded model as `unknown`.

`oms panel --metrics` also carries `route_history` (in `--json` and as `route` lines): write
attempts grouped by role (`panel_purpose`) x provider x model x effort (`panel_effort`,
`unrecorded` when absent) with attempts, admitted, rejected, reworked, admit rate over
decided tasks and median reported tokens. `--min-samples N` (default 3) hides groups with
fewer than N decided tasks and says how many. Per role, a `suggest` line names the best
admit rate among shown groups; it is reported history for a person to weigh, never a
routing decision, and nothing is applied or stored.

From the same rows, `--metrics` adds `role_fold_audit` and `model_preference` (JSON keys next
to `route_history`; `fold ...` and `model preference ...` text lines after the `route` lines).
The fold audit lists per role attempts, decided tasks and admit rate (withheld under
`--min-samples`), the roles from `roles/` never used in the window, and role pairs whose
rarer member shares a task at least 80% of the time (`OMS_FOLD_CO_OCCURRENCE`, with at least
`--min-samples` tasks). It is headed "fold candidates (reported history, not a decision)":
fold one role at a time and keep it only if the folded version measures worse. The model
preference puts a role's provider/model groups side by side (each with at least
`--min-samples` decided tasks; two groups needed) with admit rate, median reported tokens
and the best-to-worst admit-rate gap, or "no clear difference" under 15 points
(`OMS_MODEL_GAP_NOISE`). Both thresholds are estimates.

On a capable terminal the sidebar animates live-state indicators at four frames per
second using the cached snapshot. Borders and role dividers stay still. Only changed
rows are redrawn; resizing triggers a complete repaint. Starting, working, verifying and
live-marker records can move; blocked/waiting/review and terminal states stay still. The
motion means recorded activity, not token streaming, progress percentage or a new
dispatch event. The native main's explicit child links determine the call tree.
Rendering frames makes no state writes or model calls; state collection remains every
five seconds, plus collection time.

A call that is new, or whose state moved, since the previous snapshot is marked `✦`
(`*` in ASCII) for about three seconds in the tree, list and graph views. In the graph
it marks the call rows of the bands (gate, counsel, workers); the main box and the
expanded card of a selected call do not draw it. The mark sits in
the two columns the selection marker uses, so no row moves or changes width when it
appears or ends; a selected row keeps its own marker, and color only tints the glyph.
The first snapshot of a room, a room switch and the snapshot after a failed or held
read mark nothing. It ends without waiting for another snapshot, costs one repaint
when it does, and never changes ordering, attention counts, the bell or the notifier.
Reduced motion shows no mark.

Interactive watch collects state and quota readings in one background read at a time. It
keeps the last frame available for input while a read is pending, and applies completed
reads within a 50 ms polling interval. Redirected or noninteractive watch retains
synchronous snapshots and `--count` semantics. Snapshots that change the hit map discard
stale coordinate clicks; navigation still validates the current room, participant and
terminal before focusing it. A proven terminal destination does not require scanning
unrelated native chat histories; app navigation and the full chat catalog keep their
bounded identity lookup.

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
subsequent manual dispatch; gateway-backed Pi uses the Codex dispatch owner. Main/child links come from explicit
lifecycle metadata, never matching a provider name. Legacy unlinked records stay
repository-wide. A marker and its attempt are coalesced only by their exact
execution location. Selected/served attribution uses exact attempt IDs; a model
name alone is not proof the provider served it.

Active attempts have a separate bounded window so newer outcomes cannot hide
them. Overflow is reported; Details retains the bounded active and recent
records. `REPO ACCEPT` and repo attention belong to shared repository state,
including earlier work. Recent outcomes and successful native process exit do
not establish acceptance of a task.

Each worker and reviewer row shows input and output separately while a call runs
and when it finishes (for example, `~12.3k in / 2.3k out` and
`12.3k in / 2.3k out`). The cached share is appended when available, such as
`13.9m in / 50.3k out · 96% cached`; selected details add cached and new input
(for example, `cached 13.4m, new 495.4k`). When output usage is unavailable, the
row falls back to `N tok`. Input, cache and output are summed when the provider
reports a breakdown, so Claude and Codex compare. Unknown usage leaves the row
cell blank; missing data is never shown as 0. Narrow panes keep the short form
or drop it before clipping the call's name.

A Claude main's own subagents (Agent tool and workflow runs) appear as read-only rows
under it while their files were written in the last 10 minutes; they are not panel
workers. The panel reads Claude's transcripts under
`.claude/projects/<project>/<session>/subagents/`, matching the session by the room's
stored hash, so no session id is kept. An Agent-tool row shows its transcript's input
and output tokens (`2.0k in / 450 out`); a workflow row shows the summed tokens of the
agent transcripts under its run directory and how many of them are live (`3/5 agents
live · 100.0k+ in / 20.0k out`).

Only the main this window shows has rows. Reads are bounded: only assistant records
holding a usage object are parsed, one entry per message id; a refresh reads appended
bytes (at most 8 MiB per transcript and 16 MiB per refresh, marked `+` until a large
file is read through, and also while any agent or record of the total is unread or
unreadable; a record over 256 KiB is read from its first and last bytes), a replaced
transcript is read again, and a missing, torn or malformed file is skipped. A Codex or
Pi main has no such rows.

| Menu action | Behavior |
| --- | --- |
| 1 Claude / 2 Pi / 0 Codex | Start the sole main: native Opus / Pi with Codex inference / native Sol |
| 3 Resume | Select Claude or Codex and an exact verified native session ID |
| 4 Main | Choose the main harness; gateway-backed Pi uses the Codex dispatch owner |
| t Auto task | Enter a task and open the owning native main |
| 5 Explain | Read-only worker; optional tier, else routine |
| 6 Implement | Optional tier; scoped brief and verifier required |
| a Advisor | Optional tier, else deep; read-only |
| 7 Review | Optional tier, else deep; mandatory review verifier |
| c Council | Choose routine (Sonnet, Terra), deep (Opus, Sol, Sonnet, Terra; default), or frontier (Opus, Sol, Fable, Astra) by difficulty |
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
In the CLIs menu, Native for `agy` or `antigravity` uses the built-in Agy harness
unless an explicit Antigravity native adapter is configured. Capability, model
syntax and CLI availability are checked before allocating its room.


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
| Claude main | `claude-opus-5-5` | high |
| Codex main / gateway-backed Pi | `gpt-6.1-sol` (shown as Sol 6.1) | high |
| Agy main | Native settings unless `--model` is supplied | Native settings |

Set `OMS_PANEL_PUBLIC_ROUTE_OPT_IN=1` for a dispatch to allow public-only routes in a
private or unverified repository; otherwise these routes require a GitHub origin
verified public. Visibility for a normalized GitHub origin is checked with `gh` on every
routing decision; only a not-public answer is cached, for 10 minutes, in the state
repository at `.oms/panel-state/public-route-visibility.json`, so a repository made
private never keeps free routes; rejected origins are not cached and filtering prints
nothing. An explicit `--to pi --model ...` bypasses the `public_only` check entirely.

Routing is provider-neutral within the configured light, routine, deep and frontier
capability tiers. It filters to installed providers and removes candidates reporting at
least 90% weekly usage when multiple providers are available; a sole installed provider
remains eligible regardless of usage. With readings for all remaining candidates, more
weekly quota left goes first; when any reading is missing, routing spreads work away
from the main's provider. Readings less than 15 points apart (`tie_band.gap`) count as
tied. A tie goes to the provider with fewer dispatches in this room, then to the one
that is not the main's; configured tier order settles what remains.

Weekly quota comes from the provider's available local reading; Codex uses `rate_limits`
records in its local CLI session logs. Missing, stale or malformed readings stay
unknown. Reviewer, verifier, security-auditor and critic exclude the reviewed patch's
writer family, or the main's when no writer is known. If no independent family is
usable, the result records `review unavailable` and remains pending. An owner-pinned
same-family judge is labelled on the result, including standalone results. Researchers are pinned to the light tier at low
effort. Only security-auditor defaults to frontier; other roles use deep or below. An
explicit `--workload frontier` remains available.

Role defaults are shown below. A write session can continue across write-capable roles
with `--continue`; judges use fresh context.

On a Linux host where `kernel.apparmor_restrict_unprivileged_userns` is 1 (the probe
`oms doctor` warns about), Codex's `workspace-write` sandbox (bwrap) cannot start.
Write-access routing then skips Codex, takes the next tier entry and records a
`route_reason` naming the restriction; an explicit `--to codex` write call fails before
any provider call with a one-line message. Read-only Codex seats are not gated: the
source shows no evidence that they need the same namespace, so that stays unverified.

### Provider accounts

#### Register and remove accounts

Codex accounts support routing. Claude accounts support registration, explicit
verification and owned integration lifecycle; they do not participate in routing or
quota selection. Antigravity's independent OAuth namespace remains unproved, so
`antigravity:ID` is rejected before file changes. Changing HOME or supplying an API
key does not establish an independent entitled Antigravity account.

For Claude, use `oms connect-services --account claude:work`, optionally with
`--home PATH`, `--first|--last`, `--repair`, `--remove` or `--verify`. The default
config root is `${CLAUDE_CONFIG_DIR:-~/.claude}/accounts/work`; it is not an OS HOME.
Registration reports `registered, login pending` and never runs login or auth status.
Manually log into **claude.ai** using the printed `CLAUDE_CONFIG_DIR` command.
Manual config-scoped claude.ai use is documented, but explicit OMS isolation
verification remains unsupported/uncertain. Matching `configDirectory` and
`authMethod: claude.ai` do not attest credential origin or establish that shared
global Anthropic OAuth profiles were excluded. No real independent authenticated
accounts have been proved here. API-key, helper, external OAuth, keyless Console,
gateway and unknown modes are unsupported too: an `api_key` status enum cannot
prove storage origin. Global auth environment overrides refuse verification.
`--verify` requests only native `claude auth status` in the selected config root,
checks the nonsecret selection fields, and returns exit 2 until trustworthy origin
evidence exists. Raw diagnostics and credential contents are never stored or logged. Native status may initialize vendor metadata inside that
selected config root. OMS registry/effect receipts, main settings and other profiles
remain unchanged. Registration, doctor, relink and update never run native status.

Claude links only OMS `CLAUDE.md`, `skills` and `output-styles/oms-korean.md` to
the current checkout, and merges hooks, HUDs and defaults into its own
`settings.json`. Existing files and user links are preserved rather than adopted.
Settings ownership lives beside that file in `.oms-claude-hooks.json`; preexisting
values remain user-owned even when equal to OMS defaults. Removal deletes only
unchanged recorded effects and preserves user edits and hook siblings. No operation
removes account directories, credentials, history or sessions, or logs out a provider.

Claude's private `provider-accounts.claude.json` (schema 1, provider `claude`,
`accounts` containing only `id` and physical `home`) and
`provider-account-effects.json` sit beside the legacy Codex registry. Claude actions
never create or rewrite `provider-accounts.json` or `codex-account-links.json`.
The last Claude removal publishes an empty accounts array. IDs are provider-local:
`codex:work` and `claude:work` can coexist. Modern lifecycle operations reject
physical-home collisions across both providers and pending effect reservations.
Malformed or unknown Claude state blocks modern mutations while passive Codex
routing keeps its unchanged legacy reader. Doctor reports provider-qualified
nonsecret integration warnings without executing providers.

Effect intent is durable before home/link/settings changes. The registry publishes
after effects, and ownership finalizes afterward. These separate files are not one
atomic transaction. Concurrent OMS calls share the existing registry lock; hook
writes additionally hold the install lock. Relink refresh runs after the global
install lock is released. A settings identity or concurrent snapshot mismatch,
retargeted link, or ambiguous interrupted write preserves data and reservation
authority with recovery guidance. Equal bytes after a crash cannot prove ownership;
OMS does not restore a settings backup or adopt a replacement home. Uninstall stops
before global cleanup or unlink when account cleanup is incomplete. These checks do
not claim protection against arbitrary mutation by the same UID or root.

Rollback preserves legacy Codex reads and selections. Old versions cannot manage
Claude accounts or their effects, and cannot detect arbitrary `--home` collisions
with active Claude homes. Perform account mutations with the modern version. An old
uninstall is not Claude cleanup; returning to a modern version with duplicate homes
preserves both states and refuses mutation until the conflict is resolved.

The global install receipt's optional physical `claude_settings_path` binds
`hooks_schema` to its main settings target and source owner. Alternate account
settings never stamp or unstamp the main receipt. An old relink may drop that field;
without it, only the actual default main target can stamp, and a normal modern
relink restores the binding. This does not fix historical alternate-target writers.

Register another entitled Codex account with:

```bash
oms connect-services --account codex:credits --first
```

The command creates `~/.codex/accounts/credits` with mode 0700 (under `CODEX_HOME`
when set). Previously registered homes remain in place. Use `--home PATH` for another
location; relative paths are resolved physically from the current directory. The home
must differ from the main Codex home and be outside Git repositories. If your home
directory is versioned, use `--home PATH` outside that repository. Existing account
homes must belong to the current user and must not be writable by the group or other
users. Every ancestor through the filesystem root must be owned by the current user or
root. An ancestor writable by the group or other users must have the sticky bit.

Refusals name the offending directory and `--home PATH` elsewhere as a fix; the command
never changes existing permissions automatically. Registration requires verifiable
ownership, so native Windows Python, which cannot report file owners, refuses it. The
main home is the parent environment's `CODEX_HOME`, or `~/.codex`. The new home links
`AGENTS.md`, `config.toml`, `hooks.json`, `.hook-trust`, `plugins`, `rules`, and
`skills` to the matching main-home entries. Re-running keeps matching links and existing
files. Conflicting links are reported; `--repair` replaces only those links.

Missing main-home entries leave dangling links, reported by `oms doctor`.

When `auth.json` is absent, registration succeeds with `registered, login pending`
and prints the exact interactive command `CODEX_HOME=<home> codex login`, quoting
the home for the shell. Run that command yourself; registration never runs login
or reads, copies, links, or prints login data. Each home keeps its own login,
model cache, sessions, and other CLI state. After login, the registered account
is available to routing without editing JSON.

`oms connect-services --account codex:credits --verify` checks `codex login
status` for every registered Codex home, including the main home. It fails if
any home or login cannot be verified, and does not make a model call.

`--first` places the account ahead of existing accounts; the default `--last`
appends it. The ID `default` is reserved for the main home. A fresh registry
retains the main home as `default` after a
`--first` account. Re-running an ID reuses its registered home unless `--home`
changes it. To unregister and remove only links created by this command:

```bash
oms connect-services --account codex:credits --remove
```

The home, login, model cache, sessions, private files, and pre-existing links remain. A
private `codex-account-links.json` ownership receipt beside the registry records each
home's `st_dev` and `st_ino`, plus link targets before creation, including interrupted
work. Removal and `--repair` open and verify the directory without following a symlink,
then check each link through that directory handle. Changes to the directory identity,
entry type, or target keep the entry and produce a warning. Moved or recreated homes and
older receipts without an identity cannot authorize link removal.

On platforms lacking directory-relative unlink or `O_NOFOLLOW`, entries stay with
`skipped: platform lacks safe unlink`. An explicit `--remove` retires stale receipts for
missing or replaced homes, keeps their links, and prints the exact registration command
with `--home`. Re-registering that ID at the same path then succeeds when the home is
safe; existing matching links remain unowned by the command. Cleanup updates receipts
per entry, retaining only remaining recorded links for a later `--remove`. Removed,
absent, or changed entries lose their recorded authority; matching links subsequently
created at those names remain untouched by later cleanup.

Authority is retired before unlink. A kill between those steps can leave an unrecorded
shared link that stays in place on subsequent cleanup. Uninstall also removes recorded
links still pointing to main-home or OMS-managed targets. It preserves account homes and
logins. Uninstall ends its account cleanup with counts of removed links, kept links by
reason, and kept receipt rows; the account registry is always kept. Kept-link counts
include changed or unmanaged entries and recorded links whose directory identity,
safety, target or unlink checks prevented removal.

`--dry-run` prints the projected summary without changing account state. When links or
receipt rows stay, recovery guidance names `oms connect-services --account codex:ID
--remove` or `oms connect-services --account codex:ID --repair`; replace `ID` with the
account to unregister or repair. Registry and receipt writes use atomic replacement
under one shared writer lock; an interrupted operation can leave partial links or an
older registry, and is safe to rerun. `oms doctor` reports each registered home's
presence, safety of its complete ancestor chain, shared-link resolution, and existence
of `auth.json` and `models_cache.json`; these account checks only warn.

Missing registered homes and receipt homes absent from the registry (even when the
registry itself is absent) warn with the same remove/repair guidance. Doctor leaves
homes, links, login state, registry entries and receipt rows unchanged.

Provider accounts are stored machine-wide in
`${XDG_CONFIG_HOME:-~/.config}/oh-my-setting/provider-accounts.json`.
A relative `XDG_CONFIG_HOME` is ignored in favor of `~/.config`. This user-local
file applies to every project. The command writes mode 0600, schema 1, and
`policy: ordered` when introducing the Codex entry; each account has only `id`
and its resolved absolute `home`. It preserves other providers and unknown
fields, refusing malformed state without rewriting it. Routing's existing
validation still rejects unsupported providers and unknown fields.

Provider names must occur in the routing policy. Only Codex accounts are enabled
**in this legacy routing file**; Claude
and Pi entries are rejected with `accounts for PROVIDER are not supported yet`. The
home-variable table in `scripts/lib/provider_accounts.py` maps Codex to `CODEX_HOME`;
Claude's `CLAUDE_CONFIG_DIR` and Pi's `PI_CODING_AGENT_DIR` remain disabled for routing.
Claude registration uses its separate private registry;
Pi registration remains unavailable. Account entries have only `id` and `home` fields.
Unknown keys, duplicate IDs or physical homes (including across providers), malformed
configuration and non-absolute homes are errors.

IDs use 1–64 lowercase letters, digits, underscores or hyphens, beginning with a letter
or digit. Invalid configuration excludes Codex from tier selection while other providers
remain available. A missing or unavailable directory is reported per account and that
account is skipped. Panel collection retains both provider rows and shows
configuration/home errors on the Codex usage line through `accounts_error`; account
readings and individual errors are under `accounts`. Where `os.getuid` is available, the
resolved home must belong to the current user and must not be group/other-writable.

Use mode `0700`, or remove group/other write bits with `chmod go-w`; ownership errors
require restoring ownership or choosing a user-owned home. Directory existence is still
checked on other platforms. Routing, launch and pinned continuation all recheck this
validator, so a permission change after selection is refused before the launch anchor.

#### Account selection

Both policies rank fresh measured weekly quota usage ascending, with unknown
readings after measured ones. Accounts at 90% weekly usage are ineligible during
tier selection; missing, stale or malformed readings remain eligible.

| Policy | Equal-quota or unknown-quota tie break |
|---|---|
| `ordered` (default) | Registry configuration order |
| `spread` | Fewer live OMS workers for that provider/account in this repository, then registry order |

`spread` is best-effort: simultaneous decisions can choose the same account,
and workers in other repositories are outside its count.

<details>
<summary>Quota freshness, model availability, headless routes and account bindings</summary>

Per-provider policy defaults to `ordered`. Both `ordered` and `spread` rank eligible
accounts by measured weekly quota usage ascending (most quota left first), placing
unknown readings after measured readings. A reading counts as measured only when its own
recorded event timestamp (`observed_at` from `account_readings`, not session-log file
`mtime`) is within `ACCOUNT_READING_FRESH_SECONDS` and its reset time has not passed;
stale, missing or malformed readings are treated as unknown and remain eligible.
`ordered` breaks equal or unknown quota ties by registry configuration order (and uses
registry order when all readings are unknown).

Each account is ineligible at 90% weekly usage during tier selection, including when
Codex is the sole provider; missing, stale or malformed readings remain eligible. Codex
remains a candidate while any account can serve the requested model. Only model slugs
are read from each home's `models_cache.json` (at most 4 MiB): a readable list omitting
the requested model excludes that account and appears in `account_reason`. Missing,
unreadable or malformed caches leave support unknown and eligible. Routing inspects no
other home files beyond the existing bounded session-log reader.

`spread` uses the same measured-quota-first, unknown-after-known order, but
breaks equal or unknown quota ties by fewer live OMS workers for that
provider/account in the current repository's panel attempt records (using the
launch decision when the attempt has no `panel_account` ref), then list order.
Two unknown readings use the same worker-count tie break. This is best-effort:
there is no reservation store, so same-instant decisions can select the same
account before either worker appears in the records. Other projects share the
configuration but their workers are outside this repository's count.


##### Route metadata

Runtime routes carry `account` and `account_reason`. The route line includes
`codex account=credits (runs under its own home) / MODEL / EFFORT / ACCESS`.
The private dispatch launch anchor retains that decision under `route` without
storing the home. New account launches also record `account_applied: true` there
when applying the home to the child's environment. With no configuration file
(or no Codex entry), existing routing
and display behavior is unchanged; the implicit default home is `CODEX_HOME` or
`~/.codex`. Account fields remain invalid in `config/panel-routing.json`.


##### Headless calls

Headless Codex calls through peer review, peer ask, consult/advise, agent-call,
delegation and autonomy workers share this account policy at the common runner. Explicit
Codex targets (`--to codex` on direct calls/delegation, `--providers` on peer
ask/review, or `--model`) follow the panel's rule: select from eligible accounts in the
same measured-quota-first, registry-tie-break order, falling back to the caller's home
if none qualifies. Implicit/tier routes keep the configured ordered/spread policy and
require an eligible account. Automatic consult and advise selections use that implicit
policy and may try the next peer after a failed call; explicit provider choices retain
caller-home fallback.

A direct `--resume-session ID` without a panel account decision keeps the caller's home
and selects nothing. A malformed account registry emits one warning with the
configuration error and runs under the caller's home; panel dispatch still validates its
own registry and refuses invalid configuration.

##### Applied account identity

An explicit `CODEX_HOME` differing from the main `~/.codex` home takes precedence.
Panel dispatch marks its route with internal `OMS_CODEX_ACCOUNT_ROUTE` so the
runner preserves that decision, including unbound legacy continuations, without
selecting again. The marker is removed from the provider child's environment.
Without registered accounts, the runner leaves the environment and attempt
record unchanged. Other providers and native panel mains are unaffected.
Applied accounts appear as `account` and `account_applied: true` in the existing
`.attempts.json` record; session lookup can resolve that recorded id rather than
the caller's current home. Provider-default calls have no model pin, so model
cache filtering applies only when a model is specified.

</details>

#### Continuation and fallback

At launch, the selected id is resolved again and its directory is rechecked. Only the
Codex child's `CODEX_HOME` changes; the caller and independently routed calls keep their
own environment. OMS never reads or transfers account credentials. `--continue TASK`
reuses the previous write attempt's account when resuming its native Codex session. A
removed account or missing home refuses native resume with the id and recovery
instructions: re-register the account, or continue on a non-Codex provider using `--to
PROVIDER --model MODEL` or a `--workload TIER` routed to another provider for a fresh
worker.

Switching to another provider uses the existing summary handoff without the old account.
Attempts without a recorded account, including implicit default-home selections, retain
the current home. Selection-only anchors without `account_applied: true` and missing or
invalid anchors also continue without an account binding under the current home.
Implicit selections are not recorded as an account in new launch anchors. A recorded
Codex capacity/usage-limit failure can overflow once to the next eligible account in
quota order across all remaining configured accounts serving the same model (not only
accounts listed after the failed one), provided the failed worker left the worktree
unchanged.

Both launch anchors retain their respective account ids. Account overflow is
capacity-only; the tier provider fallback remains available for
provider-down/model-unavailable failures and after the overflow account fails. Each
fallback is bounded to one use. Policy refusals and verification failures never trigger
account overflow or provider fallback. The home is rechecked before publishing the
launch anchor. When a primary applies an account home, provider fallback runs as a
separate call with the parent-derived environment: `CODEX_HOME` returns to the parent's
value or is absent if the parent left it unset.

This also applies after account overflow. Once child exit is confirmed, the controller
records the withheld in-delegate decision as `panel_fallback_decision: deferred` through
the existing lifecycle metadata writer. The workspace balance is the upstream hard
spending limit; v1 has no OMS money cap or strict-cap ledger. Registering an account
shares the main home's existing OMS setup; it does not install a separate OMS copy.

### Dispatch roles and tiers

Routine work tries Pi with `openrouter/thinkingmachines/inkling:free` at medium effort
first, then the eligible Claude Sonnet and Codex Terra candidates; Pi has no weekly
reading, so the configured preference stays first, and its exact model is checked
against a populated local catalog. A recorded Pi provider failure can fall back once to
an eligible tier candidate, provided a write leaves the worktree unchanged; policy
refusals and verification failures do not trigger fallback. An OpenRouter free-model
daily-cap failure cools the exact Pi route until 00:00 UTC; ordinary rate limits only
trigger the one-time fallback. Judge tier allocation also permits Antigravity on deep
and frontier; an explicit `--to` may select another provider.

| Tier | Configured model candidates (effort) |
| --- | --- |
| light | Pi `openrouter/thinkingmachines/inkling:free` (low), `claude-haiku-5-5` (low), `gpt-6-luna` (low) |
| routine | Pi `openrouter/thinkingmachines/inkling:free` (medium), `claude-sonnet-5-5` (medium), `gpt-6.1-terra` (medium) |
| deep | `claude-opus-5-5` (high), `gpt-6.1-sol` (high), Antigravity `gemini-4-argon-high` (high, read-only) |
| frontier | `claude-fable-5-1` (high), `gpt-6-astra` (high), Antigravity `gemini-4-argon-high` (high, read-only) |

Tier entries may declare `"access": "read"` to exclude them from every write
selection, including provider fallback. Antigravity tier entries require this
marker; invalid access values and missing Antigravity markers are refused with
one line. Its unknown weekly quota does not change native quota or tie-band
ordering: it follows every eligible native with a known weekly reading, while
unknown-quota candidates retain their existing order. Independent judges count
Antigravity as a separate provider from the implementer. Its configured model is
a catalog selector: routing requires the registry's routable list, preserves the
configured model level and pins the newest model in that family. A missing or
unroutable catalog skips the candidate. Existing board and result views display
the selected provider and model. A read fallback to Antigravity also requires
the registry's `exact_model_override`; otherwise the fallback is recorded as
`alternate-not-routable`.

| Role | Default tier | Access | Front door | Runs |
| --- | --- | --- | --- | --- |
| `researcher` | light | read | `oms panel --dispatch researcher` | `peer-delegate --role researcher` |
| `scholar` | deep | read | `oms panel --dispatch scholar` | `peer-delegate --role scholar` |
| `worker` | routine | read or scoped write | `oms panel --dispatch worker` | `peer-delegate --role` |
| `builder` | routine | read or scoped write | `oms panel --dispatch builder` | `peer-delegate --role` |
| `refactorer` | routine | read or scoped write | `oms panel --dispatch refactorer` | `peer-delegate --role` |
| `documenter` | light | read or scoped write | `oms panel --dispatch documenter` | `peer-delegate --role` |
| `release-engineer` | routine | read | `oms panel --dispatch release-engineer` | `peer-delegate --role release-engineer` |
| `integrator` | deep | read or scoped write | `oms panel --dispatch integrator` | `peer-delegate --role` |
| `performance-engineer` | deep | read or scoped write | `oms panel --dispatch performance-engineer` | `peer-delegate --role` |
| `reviewer` | deep | read | `oms panel --dispatch reviewer` | `peer-review` |
| `verifier` | deep | read | `oms panel --dispatch verifier` | `peer-delegate --role --read-only` |
| `security-auditor` | frontier | read | `oms panel --dispatch security-auditor` | `peer-delegate --role --read-only` |
| `advisor` | deep | read | `oms panel --dispatch advisor` | `advise` |
| `architect` | deep | read | `oms panel --dispatch architect` | `peer-delegate --role --read-only` |
| `critic` | deep | read | `oms panel --dispatch critic` | `peer-delegate --role --read-only` |

Scholar performs multi-source literature research with verified citations at high effort; its default wall is 60 minutes, and `--workload frontier` selects the hardest surveys. Researcher remains the pinned cheap lookup role.

The plain `worker` role uses the `analysis-worker` strategy for reads and
investigations, and `implementation-worker` for implementation. Other roles use
their role name as the strategy. Write access requires `--access write`, a scoped brief and a verifier;
read access is the default. For measurements needing scratch writes, use
`worker --purpose investigate --access write --brief-file B --verify CMD`:
it keeps the analysis strategy, permits scratch outside the repository, rejects
repository patches and returns findings for owner `--finalize --outcome completed`
judgment (never `accepted`). The same role routes are available without opening
the panel; see [Without the panel](#without-the-panel).

Write dispatches with `--task-id` warn when the shared-plan task has no verifier
or its verifier differs from `--verify` (ignoring only outer whitespace). The
warning changes neither command nor plan claims; write dispatch still requires
a scoped brief and a verifier.

`--workload light|routine|deep|frontier` overrides the tier for one dispatch and
`--reasoning-effort` overrides one dispatch's effort.

#### Verification budgets

| Verification path | Default timeout |
|---|---|
| Panel `--verify`, patch-admit and patch-land admission | 20m |
| `oms peer-review --gate` | 20m |
| Other standalone peer commands | 10m |

Set `OMS_PEER_VERIFY_TIMEOUT` in the environment to override it. Verifier budgets must
be positive finite durations up to 24h; zero, malformed and larger values are refused
before the command runs on both native and Python timeout backends. An empty value keeps
the caller's default, and fractional durations remain valid.

#### Heartbeats during verification

A write attempt keeps its heartbeat while verification runs, then stops it within about
two seconds; if reconciliation left that attempt blocked as `heartbeat_expired`, a
passing verify resumes only that exact block (same attempt, sequence and reason, checked
under the ledger lock) to review. Operator blocks, cancellation, timeout and failed
verification keep their outcome.

#### Verifier environment

The verify command runs without the worker's `OMS_PANEL_*`, `OMS_ROOM_*`, `OMS_MODEL_*`,
`OMS_REASONING_*`, `OMS_INDEX_*` and state/attempt/lease variables, while keeping
`OMS_HARNESS_*` as the recursion guard; nested suites therefore reach the top-level
verdict without recording into the live room.

#### Explicit routes and model selection

Use `--to` and `--model` only when a specific provider is explicitly required; `--model`
requires `--to`, and an exact route does not silently fall back. Codex and Claude tier
routes use the configured model as a floor and pin the newest catalog version in that
same provider family by numeric version (an omitted minor version counts as zero). Pi
and OpenCode tier routes keep exact `provider/model` IDs; OpenCode omits the unsupported
effort flag. Availability comes from installed/cached provider data; no paid model probe
runs, and a chosen model failure does not substitute an older or more expensive family.
`--workload` cannot be combined with explicit `--to`.

#### Role records and owner detection

Independence ordering applies to reviewer, verifier, security-auditor and critic when multiple providers remain available. Their records keep the board kinds `worker`, `researcher` (researcher, scholar),
`advisor` (advisor, architect, critic) and `reviewer` (reviewer, verifier,
security-auditor) in `panel_role`; the strategy is generally the recorded purpose
(plain workers record explain/investigate/implement, and scholar records research).
Dispatches also pass the selected strategy in `OMS_PANEL_STRATEGY` alongside the board kind in `OMS_PANEL_ROLE`; lifecycle refs retain it as `panel_strategy`. `oms panel --routes`
prints each role's tier and chosen candidate, plus
these kinds and the write-capable roles. Dispatch and council owner defaults use
`OMS_AGENT`, then Claude's `CLAUDECODE`/`CLAUDE_CODE_ENTRYPOINT` markers, then
Codex's `CODEX_SANDBOX` marker, otherwise Codex; `--owner` overrides detection.

`oms panel --recommend KIND` (explore, mechanical-edit, implement, debug, design,
review, summarize) prints the first row of
[`config/task-routing-rules.json`](../config/task-routing-rules.json) that matches
the kind, a risk (`low`, `medium`, `high`) and a size (`small`, `medium`, `large`),
with its tier, effort and reason; `--json` prints the same. Give `--diff RANGE` to
measure risk (the review gate's risk-path selectors) and size (lines and files,
bucket limits in the file), or give both `--risk` and `--size`. It launches nothing and
keeps an explicit `--model`, `--reasoning-effort` or `--workload` you pass. With
`--failed N` (N of 2 or more) it adds a suggestion: raise effort one step, and once
effort is already `high`, raise the tier; `deep` is the ceiling because frontier is
reserved for security audits. The table is checked by `python3 scripts/lib/route_rules.py`
(unknown keys, unreachable rows, missing default row); the thresholds are starting
points, not measured. Run work you can verify at low effort and re-run only the failures higher.


#### Main workflow and briefs

[The panel policy](../config/panel-routing.json) is separate from generic OMS
provider defaults. An explicit native `--model` override preserves native effort
settings. Advisor (which direction?), architect (how to structure?) and critic
(when does it fail?) are separate judgments; the bootstrap does not ask the main
to call them together. A missing provider or unsupported model fails that route
without silently selecting another model.

A panel main acts as the control tower: its bootstrap delegates implementation,
investigation and test repair to bounded workers by default and keeps scope, briefs,
integration, review, patch admission and coordination. Claude, Codex and Pi share the
compact main contract with their durable `--owner` value; provider launch details and
the appended room identity remain separate. The contract retains authority, delegation,
admission, review, finalization and recovery invariants. Command walkthroughs live in
the `oms-agent-harness` [panel main command
reference](../custom-skills/oms-agent-harness/references/delegation-artifacts.md#panel-main-command-reference),
[roles](../custom-skills/oms-agent-harness/references/roles.md) and [model
routing](../custom-skills/oms-agent-harness/references/model-routing.md).

Reference pointers use the absolute bundled directory resolved from the `oms`
entrypoint, so they also work without the skill installed. The native integration
regression caps the UTF-8 bootstrap at 6,250 bytes with fixed
repository/entrypoint/reference labels (6,077 bytes, formerly 12,549); room instructions
and provider launch labels are measured separately.

#### Process results as they arrive

Wait on each dispatched call separately, with one notification per call; review, admit
or re-dispatch it as soon as it finishes, never batch-wait for a wave. Start the next
ready plan task or follow-up round in the same turn; a new write dispatch is refused
at the WIP limit 6 per main (live write workers plus results awaiting decision; an advisory
snapshot check, so concurrent dispatches can briefly exceed it). Send remaining review
findings to a worker with `--continue`. Edit directly for admitted-patch integration or
conflict resolution, when the cause is known, the change is local (one or two files) and
a brief costs more than the edit, or after a worker failed the same bounded task twice.

Delegate investigation, larger multi-file or parallelisable work, long verification, and
trust boundary changes (authentication, permissions, secrets, install, deletion, hooks).
Direct edits use the same review gate (oms peer-review --gate --verify) and the same
landing path as worker patches. Serialize shared release bookkeeping (changelog,
version, manifest, inventory files) outside parallel worker briefs: the main writes it
once per landing batch during integration, or assigns one documenter for the batch. For
experiments the main authorizes launches, since paid compute needs the user's authority;
builders write run scripts and configs, a worker with `--purpose investigate` reports
finished outputs (`oms job-digest` condenses a training or Slurm log), and the main
waits on jobs with one background wait or notification, not repeated polls.

Outside the panel, the global rule still requires an explicit request before spawning
subagents.


#### Land from an isolated worktree

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
mains in other sessions as soft, and `oms panel` refuses overlapping write dispatches against declared scopes before the work
starts.

A delegated run also fingerprints the user-level files that decide what the next
session runs: the Claude `settings.json`, the Codex `config.toml` and `rules/`,
the Antigravity `settings.json`, shell start files and the git config. When one
differs after the run, `oms delegate` prints
`warning: user-level configuration changed during this run: <names>` and adds a
section to the artifact. It names files only, restores nothing and never fails
the run, strict mode included: a provider CLI may update its own settings and a
person may be editing them. `~/.claude.json` is left out because the CLI
rewrites it on every run.


#### Write authority

Task complexity and write authority are independent. A write role needs a
scoped brief file and mechanical verifier. Read workers
use isolated worktrees too. Write workers return a patch for parent inspection
and admission; the panel does not apply it. Reviewers require a verifier and
record a review gate. Among read roles only reviewers run `--verify`; the others refuse one (dispatch a reviewer to run checks). Workers cannot recursively dispatch or open owner sessions.
An empty write patch from a panel dispatch or plan task exits 6 (`no changes`).
Normal native permissions, harness policy and account consent remain in effect.
Pi write workers have an unsandboxed shell. Worktree isolation and patch admission govern returned changes; worker guards provide retrospective checks.

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


#### Carry findings into a dispatch

`oms panel --dispatch ROLE --task-id T --brief-file F` or `--continue` appends
up to five newest settled read-only findings for T from each artifact's
Output section, with repo-relative references; repeat `--from-task ID` to carry
findings from other tasks into the same brief; `--dry-run` appends none. A carried
block that the outbound scan flags, or that holds a machine path, is replaced by
an omission line naming its artifact, so reviewer text never blocks the dispatch.


#### Bind the dispatch to its main

From the control window, a dispatch belongs to the room's active main of that provider.
`--main PARTICIPANT` asserts that caller's identity (the board's `a` uses it); a room
with multiple live mains refuses every dispatch until the others are released with `oms
panel --release-main` or the swap is finished. A native main always dispatches as
itself.


#### Shared-plan task transitions

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

#### Continue a write task

Continue reviewed work with `--continue TASK_ID`: `oms panel --repo . --dispatch
refactorer --owner claude --continue TASK_ID --access write --brief-file next.md
--verify COMMAND` takes the same role, workload and access checks; any write-capable
role may continue (a builder session can become a refactorer), while reviewer, verifier,
security-auditor, critic and the other read-only roles never resume. The short form `oms
panel --repo . --owner claude --continue TASK_ID --brief-file next.md` takes the role,
`--access write`, workload, every `--scope` and `--verify` from the task's newest
recorded write dispatch in this main's lineage; any flag you pass overrides its recorded
value, except that `--scope` adds to the inherited scope (a plan-bound dispatch's
declared scope also covers the plan task's allowed paths).

A recorded explicit model also inherits its provider and model; a recorded explicit
reasoning effort is inherited independently. Historical records without these markers
keep tier selection. `--workload` forces tier selection instead of inheriting the model.
Stderr identifies an inherited route and its attempt; an inherited model is an explicit
target and does not fall back to another provider. A task with no recoverable write
dispatch is refused with a message to pass `--dispatch ROLE`. It finds the latest worker
of that task owned by this main, and starts a new run in a new worktree at the current
HEAD that resumes the worker's native session (recorded as `refs.native_session` in the
attempt, never in room text).

A continuation transfers native conversation history only when that provider session is
still available. A write continuation materializes its verified patch in a new worktree
at the recorded base; a fresh session can also receive a bounded recorded summary.
Stderr names what actually transferred. Stopped writes with a recorded worktree must
first go through `oms panel --salvage TASK_ID --attempt ATTEMPT_ID`. Inspect the recovery,
then use `--continue TASK_ID --access write` to carry the newest verified salvage patch
at its recorded base. The kept worktree must still match that capture, or have been
removed. If it changed, including being reset to its recorded base, salvage it again
before continuing; older complete captures remain recorded and do not prevent using
the new capture. Salvaged patches are never embedded inline in a prompt; continuation
without write materialization refuses.

Cache advisory (no behavior change): the continuation note also shows the native session's age
(`session last active 4m12s ago`, from the session file mtime or the attempt's `updated_at`; `unknown`
when neither exists) so the parent can choose resume or a fresh dispatch. When a resumed session is
inside the warm window (`OMS_CACHE_WARM_SECONDS`, default 300, `0` disables) and the route's model or
effort differs from the recorded one, and when `--swap-main` replaces a main active inside the window, one
`note:` line says the provider prompt-cache prefix will be rebuilt. The action always proceeds.

A timed-out write result card advises salvage first; a timed-out read keeps the
`--continue` hint. Salvage of an attempt with nothing to recover reports
`nothing_to_recover` and exits 0. A continued implementation needs `--access write` (the
default is read-only); later rounds of one task show "round N". Unless the environment
sets them, write calls get a 40m wall (60m at high effort) and any call with `--verify`
gets a 20m verify wall, enough for the repository smoke suites.

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
reviewer independence excludes the judged family. Without a room the call
is recorded as a standalone result: read it with `oms panel --results --task-id
ID`. `--dry-run` prints the chosen route and argv without running a model.


## Discussion and results

### Council workloads

The council uses read-only seats. The owner decides the outcome.

| Workload | Routes | Seats | Primary calls with one rebuttal |
|---|---|---|---|
| `routine` | Routine pair + Antigravity | 3 | 6 |
| `deep` (default) | Routine and deep pairs + Antigravity | 5 | 10 |
| `frontier` | Deep and frontier pairs + Antigravity | 5 | 10 |

- Two rebuttal rounds use 9 primary calls for `routine`; `deep` and `frontier`
  would need 15, so the council drops the latest additional same-family seat
  (one stderr line each) until it fits: four seats, 12 primary calls, all three
  families kept.
- **The budget counts primary calls.** Format repair can add one call per primary
  call. `--dry-run` reports both counts without running a model.
- Missing required families or unavailable routes refuse the council before
  dispatch. `OMS_COUNCIL_ALLOW_PARTIAL=1` is the explicit partial-seat opt-out.

<details>
<summary>Exact routes, model catalog selection, family requirements and output contracts</summary>

The council uses the existing `peer-ask` debate, thread, artifact and lifecycle front
doors. Omitted `--task-id` and `--label` are derived from the prompt and reported on
stderr; dry-run JSON includes both values. `--workload routine` selects the routine pair
(Sonnet, Terra); the default `deep` adds the deep pair (Opus, Sol), while `frontier`
combines the deep pair with the frontier pair (Fable, Astra). Seats come from the
`tiers` routes and `council.extra` in `config/panel-routing.json`: routine uses the
routine tier plus the extra Antigravity seat, deep uses the deep and routine tiers plus
Antigravity, and frontier uses the deep and frontier tiers plus Antigravity, so every
workload seats all three provider families (`claude`, `codex`, and `antigravity`). One
rebuttal round means six primary calls for routine (three seats) and ten for deep or
frontier (five seats); two rounds mean nine for routine and twelve for deep or frontier
after one same-family seat is dropped to fit the 12-primary-call budget.

Its configured Gemini ID selects a name and
level (for example, `gemini-4-argon-high` selects `argon-high`); council routing
pins the newest numeric generation of that family in the provider's routable
catalog, excluding models shown as not routed by `oms models`. Codex and Claude
seats also pin the newest catalog model in their configured family. If that Gemini
name is absent, routing may select the newest generation of another Gemini name
at the same level only when exactly one name is routable there. An installed
Codex or Claude CLI with an empty catalog retains the configured model: Claude
Code cannot enumerate models, and Codex may not have refreshed its catalog yet.


Installation and catalog lookup checks still apply to every seat. When `codex`,
`claude`, or `antigravity` cannot seat (not installed, catalog lookup failed, model
absent), the council refuses before any
provider call or started-council record, exiting nonzero with one line naming the
missing family and the reason (unless `OMS_COUNCIL_ALLOW_PARTIAL=1` is set as an
explicit opt-out for hosts that cannot run a provider, which keeps the skip-and-continue
behavior). A council over the 12-call budget first drops seats as above; if one
seat per family still does not fit, it refuses naming the budget and the maximum
rounds, and prints no "dropped" lines. The council summary shows the resolved model.

Format repair can add one call per
primary call. `--dry-run` exposes both counts without executing a model. Every
required seat must complete to report council success; failed seats remain
visible and the owner still decides the outcome. All seats are read-only. There
is no model fallback after a seat launches or additional synthesis-model call.

</details>

### Blind lens reviews

For blind lens reviews, add `--lenses correctness,security,compatibility` to `oms panel
--council`. Each unique comma-separated name of 1..32 characters matching `[a-z0-9-]+`
gets one seat; 2..12 seats are required. Assignments cycle over the exact routes
selected by `--workload`, with the first pass covering all three provider families
before repeating a family, and are frozen before dispatch. With at least three lenses,
each family gets at least one lens; two-lens reviews still require two independent
families. Lens mode defaults to `--rounds 0` and rejects every other round count: there
are no rebuttals, but every opening still requires the deliberation evidence,
alternatives, counterargument and verification sections.

Each run uses a fresh thread; `--thread ID` supplies its prefix. Seat IDs, lenses and
targets identify prompts, artifacts, repairs and thread turns even when targets repeat.
Failure history stays keyed by the provider target; summaries retain the seat and lens
labels so later calls can warn about failures and clear them on recovery. Synthesis
remains local and labels every seat, including failures and answers omitted by the
existing byte budget. Without `--lenses`, the council keeps its ordinary routes, thread
behavior and one-round default. `--rounds` requires `--council`, including an explicit
`--rounds 1`.

### Dock and message navigation

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

#### Messages

Messages shows up to 200 retained matching messages, oldest first. Each row
shows local time (and date on another day), sender and recipient by board main
name, unread state, and question/answer/handoff/status kind.

| Key | Action |
|---|---|
| Left / Right | Cycle `needs you` → `all` → `this call` → `to/from me`. |
| Up / Down | Select a message. |
| Enter or click | Expand or collapse its full wrapped text; code and tables keep their indentation, list items hang under their text, and text cut at 8000 characters ends with a `[... N more characters not shown]` line. |
| `f` | Open the associated call in the right reader; show a notice when none exists. |

- `this call` appears only when a non-main participant is selected.
- `to/from me` appears only with multiple recorded mains and an identified main
  for this window.
- **Reading never acknowledges a message.** Unread badges and tags count pending
  messages for live readers. Complete projections are exact; truncated counts
  are lower bounds marked with `+`.

The full log is read in the background and reread only when the room's message
counts change. When the selection has moved off the newest message and more
arrive, the header adds `↓ N new` (`v N new` in ASCII) next to the `a-b of N` position.




#### Detail and Past

Detail is the selected call's persistent, reserved reader; it remains present
when dismissed or when there is no selection. Past lists every settled call of the room's mains
and of calls with no known main, newest first, one row each (role and model, task, how it ended,
end time). Up/Down pick a row; Enter or a click shows it in Detail exactly as a board card would,
and `f` opens its full result on the right.

#### Plan

Plan shows the shared repository plan:
the goal, then one row per task (state glyph, id, title, claimant by the board's
main name or provider, verify command) ordered verified, review, running or
claimed, ready, blocked.
A ready task whose dependencies are not all done reads `ready (waiting on ID, ID +N)`,
and a blocked task adds its recorded reason (up to 60 characters). A task's Detail
snapshot lists each dependency with its listed state, the unfinished ones under
`Waiting on:`, and a blocked task's reason (up to 500 characters) with its `oms agent-plan reopen`
command. Only a done dependency satisfies a claim; a cancelled or unlisted one
never does. These notes are display-only.

#### Debate

Debate shows the shown main's debates, newest first
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



### Finalize an outcome

The owning main records a result through `--finalize`. Native process exit and worker
completion never create accepted results by themselves. `completed` records an owner
summary; `accepted` additionally requires a passing explicit `--verify` and matching
landed patch evidence for every recorded write worker of that task and every write
worker without a task ID under the finalizing native main; workers assigned to other
tasks are checked when their own task is accepted. A plan-bound patch retains its exact
plan lineage; a standalone patch matches its successful landing by digest.

Without `--evidence`, accepted records the one digest-verified admission report of each
landed patch, and refuses when that report is missing or ambiguous. If a digest-matching
landing patch still applies cleanly to the verifier checkout, `accepted` refuses before
verification; finalize with `--repo WORKTREE` containing the landing, or after landing
and syncing. Verifier output stays local. Evidence references bind repository-relative
files of at most 1 MiB each to SHA-256. The summary is bounded to 8 KiB, rejects
sensitive-looking text and private absolute paths, and is cleaned of terminal controls.

New result revisions are immutable artifacts registered in the existing artifact index,
rather than a second task database. For each landed write patch, `accepted` also
requires a passing automatic review gate. Before any side effect, `--finalize` waits up
to 120 seconds for live review read passes on the same execution checkout; if they
remain, it exits 75, so retry later. If review is missing or ends without a verdict,
finalize `completed` (not `accepted`) and explain why in the summary. Finalization
checks the full lifecycle projection, independently of the display window.

Missing retained patch or landing evidence refuses acceptance. A failed, abandoned or
rejected write worker under the same main also refuses `accepted`; the owner can record
`completed` or `failed` with its actual summary instead.

After saving the immutable result, finalize closes stopped `review` workers of
exactly that task and the owner's lineage: `completed` → `done`
(`owner_completed`), `accepted` → `done` (`owner_accepted`), and `rejected` →
`cancelled` (`owner_rejected`). `failed` keeps `review` for rework. Running
workers and unknown exits remain pending; settlement errors are reported without
undoing the result. Lifecycle `done` grants no acceptance: `completed` retains
plan review/acceptance, and acceptance derives only from the `accepted` outcome.
The result must still match the current plan task generation. Workers created
after the result revision remain pending as `newer_than_decision`.

### Settle stopped workers

`oms panel --settle-review` lists eligible finalized-task leftovers without
writing, including task, age in review, selection reason and target state.
Add `--apply` to close them. Repeat `--attempt ID` with `--reason TEXT` to
explicitly cancel stopped review workers as `owner_superseded`, recording the
reason. The default scope is the calling main's lineage. `--owner-attempt ID`
selects a named dead main's lineage. Unknown ownership and other live mains are
refused. Compare-and-set transitions and per-attempt revision keys make retries
safe after partial settlement. No worktree, patch or artifact is deleted.

#### Legacy mains

For legacy terminal mains with no host/native ownership refs, explicitly add
`--legacy` to `--owner-attempt ID`. Every legacy main in that participant's chain
must have its last lifecycle event older than 24 hours; override this threshold
with `--legacy-older-than HOURS` (finite and positive). Selected workers report
`selection_reason: legacy_terminal_main` and `legacy_older_than_hours`. Mains
with any host/native ownership refs still require the normal absence proofs;
age never substitutes for those proofs. Finalized-result and explicit `--attempt`
selection otherwise remain unchanged.

#### Standalone workers

`oms panel --settle-review --standalone --older-than DAYS` selects parentless or absent-parent
workers in review, without requiring a main. The finite positive threshold
measures time since entering review. Only matching indexed legacy artifacts
whose Exit section proves termination allow cancellation as `owner_superseded`,
with reason `aged standalone review`. Room-bound standalone attempts are eligible
only with indexed exit proof and a finished call when its launch record exists.
When the launch record is absent, the exit proof and review age suffice; an
unreadable or invalid record leaves the attempt pending. Recent, unknown-age or unproven-exit
attempts remain pending. This mode cannot be combined with owner or explicit
attempt selection. Both recovery modes are dry runs until `--apply` is added
and use the same result lock and expected-review-state transitions.

#### Aged undecided work

For an undecided backlog, explicitly use `--aged-days N` in the calling main's lineage,
a named dead main's lineage, or with `--standalone`. There is no default; N must be
finite and at least 7. This selects review workers, and in a main's lineage also blocked
workers, whose last transition into their current state was more than N days ago
(`--standalone` selects review workers only), even without a finalized result or indexed
exit artifact. Room-bound standalone attempts remain pending under this policy. Blocked
workers are cancelled with `reason_code: owner_superseded` and reason `aged blocked
without owner decision (N days)`; review workers use `aged review without owner decision
(N days)`.

Unknown ages and recent workers remain pending. The calling-main scope never selects
newer workers. Named-main recovery still requires its existing dead-main and host/native
absence proofs. Parentless or absent-parent attempts with room ownership refs remain
pending. Waiting-input and waiting-approval workers are not selected. This option cannot
be combined with `--older-than`, `--attempt` or `--reason`. Preview first; `--apply`
uses the same result lock and lifecycle-sequence compare-and-set transitions, without
deleting artifacts. Settlement also compares the selected attempt's lifecycle sequence
under the lifecycle lock.

Leaving review and re-entering it before apply invalidates the selection, even when the
state name is again `review`; retry rechecks its age.

```bash
oms panel --finalize --owner codex --task-id ID \
  --summary-file SUMMARY.txt --outcome accepted \
  --verify 'test -f RESULT.txt' --evidence RESULT.txt --notify
oms panel --results --task-id ID
oms panel --retry-delivery --task-id ID
```

Accepted results also require a passing automatic reviewer gate for each landed write patch.

### Accept and publish a patch

For one reviewed worker result, the owning room main runs `oms panel --accept TASK
--repo INTEGRATION_CLONE --message 'fix: description' --verify CMD` (optionally
`--expected-patch-sha256 DIGEST`). It admits the unique candidate, commits only its
patch, and records plan review: **committed locally; publication pending**. Successive
accepts replay earlier canonical acceptances' verifiers in the same clone before the
candidate verifier, refuse candidates naming the earlier task they break, and reset the
replay set once those commits are landed. Run `oms land --repo INTEGRATION_CLONE`
explicitly, then add `--repo INTEGRATION_CLONE --landed-commit SHA` to `--finalize`:
publication proof finishes the plan before recording its result.

For reviewed patches admitted and committed together in a batch, the same finalize
command works without a `panel_accept` record. The latest write candidate must belong
to the current task lease and have a passing automatic review gate bound to its
`patch_sha256`. The receipt's push-base-to-landed range must introduce an exact replay
of those patch bytes on every touched path, and the landed tree must retain those
bytes; other tasks' paths may share that commit. A patch already present in the base
or changed by a later commit does not qualify. The existing land receipt and
remote-target reachability checks still apply. Finalize checks admission evidence,
the indexed candidate artifact, landing proof, allowed paths and the verifier before
moving a running task through review and the same landed finish path. These checks
also apply to `completed --landed-commit`; a refused precondition leaves the task's
state unchanged. A task already in review or done must record the candidate's exact
artifact and patch paths. A missing review pass or edited
integration patch refuses with the missing proof; the owner can still record
`completed` without `--landed-commit`, which leaves plan acceptance pending.
The outcome or summary alone never finishes a plan task.

SHA is the full head that `oms land` pushed, not the accepted commit when commits such
as the batch changelog sit on top. Verification runs in that execution clone; evidence
remains in the canonical state repository. Only clones recognized by the existing
repository resolver are supported. Accepted outcomes still require matching landed-patch
evidence and a passing review gate. `--landed-commit` is allowed only with `--outcome
accepted` or `completed`. The post-apply fence compares the candidate identity and
digest, plan task hash and lease, and passing review gate; heartbeat, usage and
settlement updates alone do not invalidate it. Admission allows the plan task's paths
plus scope grants in authenticated dispatch/continue launch records for that same task
and lease; room declarations and brief text do not widen admission. The effective scope
also binds admission reuse and is re-read by the acceptance fence.

Changes to an existing test selected by the verify command, including assertion-only
insertions, require the owning main's explicit `--allow-verifier-change`. Both candidate
verification and the restored base verification floor must still pass.

Acceptance records its exact commit and tree with the task, lease, candidate digest and
review gate. Publication must contain that commit or a rebased candidate whose tree
exactly matches replay of the reviewed patch bytes; patch-ID only locates candidates.
The locked plan finish enforces this binding for direct `agent-plan finish
--landed-commit` calls too. The commit is built from the locked, verified index tree
with a ref compare-and-swap, without commit hooks. If authority changes after
application, recovery prints a literal-pathspec add command and a helper that repeats
the current-authority and exact-tree checks before committing; inspect the reviewed
bytes before running it and never reapply.

The per-clone lock serializes callers sharing an integration clone; the plan lease
compare-and-set remains the authority across clones. Use `patch-land --recover` for
incomplete application receipts. If finish succeeds but result recording fails, retry
only `--finalize` after confirming the exact task completion.

### Results reader

Results lists the latest twenty retained tasks from at most 300 lifecycle attempts and
1000 artifact events. Selecting a task shows up to eight recent call records.
Participant browsing selects that participant's tasks before applying the recent-20-task
limit. An explicit query outside retained evidence reports an incomplete window. Bounded
normalized answer sections retain up to 32 KiB per answer; raw prompts, provider logs
and native conversation histories are not exported. Missing, changed or linked evidence
is reported as unavailable. Oversized answers show a preview notice and their source
artifact reference.

The reader retains answer lines and reflows them when resized; Up/Down moves one line
and Home/End goes to the first/last page. Page keys and the wheel expose the complete
retained preview. The footer reads `rows a-b/N` followed by `all`, the percentage
read so far, or `end`; paging stops at the last full page. Wrapped list items hang
under their text and keep their nesting. After a resize the reader keeps its place by
the text above the first shown row, also when the view has no section headings. Page
keys move by the visible body rows, so a notice row never makes one line vanish. A
Summary excerpt cut at its line limit ends with `[... N more lines · r opens Raw]`.
The Messages kind column shows a capital (`Q`, `N`, `H`...) when the board is too
narrow for the word, and the footer offers `Enter Chat` only with a selection. The registered result and source artifacts retain their full bounded
content. The menu's numbered list shows titles, outcome, verifier and app status before
loading any normalized answers. Select a number to open that task's detail and return to
the list afterward. Browsing performs read-only queries; it does not dispatch, accept a
patch or retry app delivery.

CLI `--results --task-id ID` continues to expose the same detail directly.

`--results [--task-id ID] [--participant P] --export FILE` writes what that reader prints
(at 100 columns) to `FILE` instead of the terminal. The file must be new: an existing
path, a symlink included, is refused with exit 2 rather than replaced, and it is created
with mode 600. The export holds only the reader's bounded, already-masked text. No
transcript, patch body or raw log is added, nothing is uploaded, and `--json` is not
combined with it.

### Result delivery

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

Plain `attempt.heartbeat` appends do not invalidate that cache, so a cached
attempt's `last_heartbeat_at`, `updated_at` and `sequence` can lag up to 30
seconds; any other lifecycle row, or a heartbeat carrying refs or usage,
invalidates it.

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
labels are the family and version read from the model ID (Sol 6, Luna 6, Astra 6,
Sol 6.1 and Terra 6.1). Native
chat rows are never updated; SQLite may coordinate its existing reader locks.

Expanded main details list calls as Needs attention, Active, No record and Finished,
then recent messages and the work location; every team row opens that call's
recorded result, and status-unavailable calls keep separate identities and result
links. Result details put the main's decision
and verification before the worker's answer, with an attributed verdict when available. Long excerpts remain scrollable and
point to the full retained result rather than filling the board with raw output.
Press `f` in a call preview (or click or Enter on the call) to open that complete
scrollable reader. The reader revalidates the exact recorded target.

A native main launched from the menu joins the selected work room. Starting a replacement
main from the menu or with `--launch` keeps the panel's recorded room while it is open and has headroom; a full or closed room makes the next
main start a new room, and an explicit `--room ID` or `o Rooms` choice wins.
A bare control window shows the panel's recorded room.
Role dispatch refuses a full room before enrollment, checking headroom for assignment and result
messages. A main has no cap on concurrent calls: split separable work into parallel calls.

Interruptions give the child up to 20 seconds to clean up, settle unfinished attempts
and send a numeric result handoff. Sensitive summaries are withheld with an `oms panel
--results --task-id` pointer; room races can still prevent delivery, while the indexed
evidence remains available. Each finished call sends one result handoff containing its
outcome, verification or review judgment, retained artifact path and next command.
Failures combine the judgment and result in that handoff. Automatic-review launch
retries defer the handoff until a reviewer launch or manual-review decision is settled.

Successful write results stay quiet while their automatic review runs. A passing
automatic review also prepares an editable completion summary and an owner finalization
command, or names the paths outside the worker's declared scope instead of a command
acceptance would refuse; admission and acceptance remain explicit. Room hooks render one
line per message: exact message ID, sender role and ID, the first 300 characters of text
and the artifact path when present, including paths beyond the excerpt. The untrusted
peer-data framing remains. One `oms room ack` command lists the delivered IDs; delivery
requires explicit acknowledgment after consumption.

Full text remains in `oms room updates` and results. Acknowledgment reminders are at
most once per five minutes per session and are omitted when that hook delivers new mail.
The `o Rooms` menu selects or creates a task-scoped room. It does not enroll other
sessions merely because they share a repository. Auto uses the connected tree when the
board is at least 20 columns wide and 12 rows high. Every joined main of the room, the
live one first, has rows; empty role cards are omitted. Explicit parent IDs connect
advisors, reviewers and workers under their own main.

Calls whose parent is outside the room's mains stay in `UNLINKED`. Scrolling exposes
rows outside the viewport. `--view tree` selects this layout directly. `g Graph` retains
the card graph at 76 columns and 16 rows. When the graph cannot fit or collection
warnings are present, a board at least 20 columns wide and 8 rows high uses the tree for
chat/result navigation and warnings. Passive output retains its list fallback. `--view
compact|detail` keeps the list at any size.

On a POSIX terminal, the board watcher accepts clicks, arrow keys/Tab and Enter.
Clicking a main focuses its proven existing tmux conversation pane; if no terminal
exists, a proven exact Codex app link can open its original chat. An unresolved or
departed participant shows a reason. These clicks do not start or resume CLI sessions,
send a prompt or call a model. In the graph, the click (or Enter) on a worker, advisor,
reviewer, researcher or council seat keeps a short summary in the board's detail area
and opens the call's full retained result (answer, review state, changed files) in a
reader pane on the right: the native chat pane is split, the reader on top (about 62
percent of its height) and the chat below, and the board keeps its width.

Another call reuses that reader. `q` or Esc in the reader, or Esc on the board, closes
only the reader, and focus returns to the board; selecting a main closes it and shows
the chat. The reader is started as `oms panel --results --participant ID [--task-id ID]`
with identifiers revalidated from recorded state, closes itself when its board pane goes
away, and is never discovered as a board or an input pane by `--reopen`. When the reader
would get fewer than 8 rows, and outside a managed tmux main window (inline layout,
Herdr, a standalone `--watch`, the control window), the call's content stays in the
board's detail area as before.

A main's detail lists its whole team, finished calls included, and clicking a team row
opens that call the same way, as does Enter on a call in the tree. Repository tasks show
the displayed snapshot's title, dependencies and declared paths. Escape closes the
detail or a results screen. Click a main's left chevron or press Space to fold its
calls; the wheel and PageUp/PageDown scroll. `t`, `g`, and `b` select tree, graph and
attention views in the watcher; `Home`/`End` jump to the first/last item and keys the
board does not bind are ignored. `q` quits a standalone watcher and does nothing on a
managed tmux board (use `F9`).

Resize discards pending coordinate clicks; a refresh discards them only when it changes
the drawn hit map. The selection is shown before a chat or result opens.

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

### Goal and task states

The usage line always covers both providers (`Claude week 66% · ctx 73% │ Codex --`); a
provider without a reading reads `Codex --`, whichever main the window belongs to. The
goal box is a card titled `GOAL` with the full wrapped plan goal, with progress
right-aligned on the title row (`╭─ GOAL ──── ▕██████░░░░▏ 6/9 verified · 1 in review ·
2 claimed ─╮`; ASCII `+- GOAL` and `[######....]`). Cancelled tasks are closed, never
verified, so they leave the total (the `[oms plan]` sentence uses the same total). A
running task whose recorded calls all ended counts as `stale` instead of `claimed`; one
whose calls wait for its main's decision stays `claimed` (the main card and Plan tab
mark it `awaiting decision`); expired claims and abandoned review leases are stale too.

Only the current holder's calls count: the claimant call itself, or calls of the
claiming main that joined at or after the claim, so calls an earlier holder left never
mark a fresh claim stale (an `oms agent-plan touch` heartbeat restarts that clock, so a
touched hold reads claimed until a new call joins). Zero counts are omitted and `stale`
is dropped first on narrow boards; the full box adds `N stale, not counted: no live
worker · release with oms agent-plan release --id ID`. On a board under 20 rows (or when
the pinned rows would starve the body) the box shrinks to one row, `[ GOAL  <goal>
<bar> <counts> ]`, before any pinned row is dropped.

Clicking the box selects the `("tab", "plan")` target on every row it draws. Without an
active plan (none, no goal, or idle for 7 days) it is a one-row dim box, `[ GOAL · none
· oms agent-plan init --goal TEXT ]`. While `oms land status --json` reports `active:
true` (a probe of the land lock, never the receipt), one more row reads `LANDING <sha7>
· <step> · <N>m`. `oms agent-plan claim` run by a panel main also records its room
participant (`claimed_by_participant`); the main's card then adds `Task: <id> · <state>`
beside its title (`+N more running` counts only held tasks with a live or unknown call,
`N awaiting decision` those whose calls wait for the main's decision; tasks whose calls
all ended are left out), or, without a recorded title, `Now: <declared status>` (with
its age past 30 minutes and `(stale)` past an hour) or a dim `no status yet`.

Every prompt of a panel main carries a `[oms plan]` sentence (goal, progress, the main's
task, the next ready task and its claim command) and, when its declared status is
missing or older than 20 minutes, a reminder to set it. When no goal is active (none, no
goal, all-closed, or idle 7 days with no held work), the hint directs the main to
register the stated objective with `oms agent-plan init --goal TEXT` (`init` archives
and replaces the current canonical plan) or ask one question using prior-work
candidates, then manually re-add relevant unfinished tasks; bounded previous-plan
details are reference data.

With no ready task, the hint instead names a waiting `proposal-review` proposal
(`oms autopilot status` shows it; the hint carries its digest for the reviewed
`--expected-proposal-sha256` apply). With no proposal, no work in flight and no passing
acceptance record, it suggests `oms plan-from-spec --discover` at most once per plan
generation per UTC day (cap in `.oms/hooks/discover-hint.json`; unreadable state means no
suggestion). A met acceptance or no goal never reopens the goal. Nothing applies itself.

A registered goal with zero tasks awaits them: use `oms agent-plan add`, never `init`.

### State vocabulary

Each word names one stage. Board groups, the goal box, the main card's `Team:`
line, Detail, Attention and Plan use the same vocabulary.

Results retain failed stages from earlier attempts while a newer matching write
attempt is starting, working or verifying without an indexed terminal call. With
the same declared purpose and a valid covering scope, the next action is
`inspect current attempt`. Lifecycle state alone does not prove that its process
is live; this guidance grants no retry, admission or acceptance authority.

| State | Meaning |
|---|---|
| `running` / `active` | The call's process is live. |
| `finished` | The process exited; admission and verification are separate. |
| `awaiting decision` | A finished patch awaits admission, or a call is in lifecycle `review`, until its main decides. |
| `unrecorded` | No live process, heartbeat or exit was seen. Detail says `status unknown`; this is not failure and remains until the main decides. |
| `in review` | A plan task is in the plan's `review` state. |
| `verified` | A plan task is `done`. Finished or admitted calls do not count. |
| `landed` | `oms land` pushed the change and `agent-plan finish --landed-commit SHA` recorded it. The goal box shows `LANDING` while land runs. |

<details>
<summary>Decision labels and plan-generation matching</summary>

`awaiting decision` appears as `Awaiting decision (N)` on the board and
`N awaiting decision` in `Team:`. Attention names the patch case `patch awaits
admission` and shows its wait time. Plan-task review appears as `N in review`
in the goal box, and `in review` in Plan, the tree and the main card's `Task:`.

`completed`, `accepted` and `rejected` decide the same task across the board,
Attention, window flag, Plan fold and carried calls. `failed` keeps it in rework.
A decision under a different plan binding does not apply to the current plan's
task of the same id. Decisions without a binding, and decisions for tasks no
longer held by the current plan, keep applying.

</details>

### Attention priority

The Attention queue ranks what needs the person by urgency, not recency, and every board
surface reads the same ranked list, computed once per frame. Its classes, in rank order,
are `answer needed` (a call or main waiting for approval or input, a question escalated
to the person or asked of `all` by a main), `failed` (a failed, timed-out, abandoned or
lost call whose result handoff is unread, that no later round superseded and whose task
is still open), `awaiting decision` (a finished call awaiting its main's decision,
oldest first, with its age; a patch awaiting admission is labeled `pending admission`),
`held, workers ended` (a running plan task whose holder's calls all ended; its row opens
the Plan tab), `unread mail` (other unanswered room mail from or to mains), and
`delivery failed` (a failed main wake-up delivery).

Provider errors are labeled `failed`. Failed items open a reader with the ending,
errors, kept worktree and a salvage hint when applicable. Within a class the newest
comes first. Above the board a header such as `Attention 7 · 1 needs you · 2 failed · 3
decide · 1 stale` (zero classes dropped; narrower widths shorten the words to `1 you · 2
fail · 3 dec`, then `Attention 7`) opens the Attention tab and is followed by the top
three items. The `Attention N` tab groups every item under a class heading; arrows pick
one, Enter or a click shows it in Detail, and `f` opens a call's full result on the
right.

The queue is display-only: it changes no lifecycle state or authority.

In a managed tmux panel each board also publishes the queue's counts as its window's
`@oms_panel_attention_counts` option, in rank order with the short class words (`1 you
/ 2 fail / 1 dec`). The option is unset while nothing waits and reads `?` while the board
shows a failed or held read or an unreadable room, so an unread queue never looks empty. tmux formats read
it without starting a command:

```text
set -g status-right '#{?#{@oms_panel_attention_counts},OMS #{@oms_panel_attention_counts} | ,}%H:%M'
```

`tmux list-windows -a -F '#{session_name}:#{window_name} #{@oms_panel_attention_counts}'`
lists every open board on the tmux server. The value holds counts and class words
only, never a label, prompt, command or path. It is written when the summary changes
and removed when the board stops; it is a display cache, not lifecycle evidence.

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
of the key hints.

Set `OMS_PANEL_ATTENTION_BELL=1` in the launching environment to also ring the
terminal bell once when a call or message newly waits on the person: `answer needed`
or any failed class. The first read of a room only seeds what already waits, so
opening, restarting or switching a board never rings for it; a failed or held read
rings nothing, and an item that left the queue and returns rings again. In a managed
panel only the main window's board rings, so a panel without a main stays silent; a
standalone `--watch` rings itself on a terminal. The bell carries no text: tmux marks
the window and applies your own `bell-action`, `monitor-bell` and `visual-bell`
settings. It is off by default.

Set `OMS_PANEL_ATTENTION_CMD` to a command line to run your own notifier on the same
events, with the same seeding and the same one-board rule. The command is split into
arguments and run without a shell, detached from the board's terminal and stopped
after 10 seconds; an event that arrives while the previous notifier still runs is
skipped, and a failing notifier never affects the board. It receives one JSON line
on standard input:

```json
{"schema": 1, "kind": "oms-panel-attention", "repo": "my-project", "new": {"needs": 1},
 "waiting": {"needs": 1, "failed": 2, "decide": 1}, "ids": ["3f2a9c0d5b7e1a46"], "ts": "2026-11-10T03:04:05Z"}
```

`new` and `waiting` count items by attention class, `ids` are hashes of the new items
and `repo` is the repository directory name. The payload never carries a label,
prompt, command, path or message text; whatever the notifier sends onward, such as a
desktop or phone notification, is its own choice. OMS itself opens no connection.

Set `OMS_PANEL_ATTENTION_OSC` to `9`, `777` or `99` to have the board write a terminal
notification escape on the same events, with the same seeding and the same one-board
rule: OSC 9 (iTerm2, WezTerm, Ghostty, Windows Terminal), OSC 777 `notify`
(urxvt-style, foot) or OSC 99 (kitty). Inside tmux it is written to the most recently
active client attached to the board's session, the way a finished Codex turn is
announced, so tmux passthrough is not needed and the escape crosses SSH to the
terminal application, which decides whether to show it. The text is the repository
directory name, the number of new and waiting items and their attention class words,
such as `oms my-project: 1 new (1 failed), 3 waiting`; it never carries a label,
prompt, command, path or message text, and control characters are removed. It is off
by default and any other value is ignored.

Each Attention item is unread until you look at it. Opening a row of the Attention tab
(Enter, a click or `f`) or a row of the control window's Needs-you list marks it read;
in the Attention tab `r` marks every row read and `l` moves the selection to the
newest unread row. A read row is dimmed, and the Attention header and the Needs-you
title read `2 new / 5 waiting` once any row is read; the tmux counts option keeps the
totals. The bell, notifier and OSC notice fire only for rows that are new to the board
and unread, so a read row that leaves the queue and returns stays quiet, while a row
that moves to another attention class is unread again. The read marks live per room in
`.oms/artifacts/panel-attention-read.json` (under a lock; hashed row ids, the class
word and a time, at most 100 ids per room and 8 rooms, rows no longer in the report
dropped), so they survive a restart or room switch. Reading is presentation only: it
never approves, answers, dismisses or changes a call, and the attention-only filter
(`b`) still shows read rows. No color means no dimming; the counts line still shows.

### Reader-facing status labels

Board text is written for
people: states read `running`, `finished` or `needs approval`, call exits and
provider status lines are dropped from messages, and the detail says whether its
main reviewed a result instead of showing internal fields.

#### Native main activity

| Main label | Meaning |
|---|---|
| `working` | A hook-recorded turn is active. |
| `idle · Nm` | Time since Stop. |
| `quiet · Nm` | A stale busy signal plus 90 seconds of unchanged native-pane output. |
| `live` | No first activity signal has arrived yet. |

 Quiet output does not prove that the model stopped. Clicking the main's
card opens its proven original conversation. Calls with no known main are counted on one
warning row (`t` lists them in the tree). Within advisors and within workers, calls waiting on the
person (needs approval, needs input, awaiting decision) sort first, then running, then failed. The
Activity heading counts the calls the board draws and adds `N past` for the settled ones in the Past
tab; with none drawn, a dim row says `No active calls` or `No calls yet`. Past rows dim the call
name and keep the state tag's color.

### Single-main and recovery guards

A panel permits one live main across all its rooms; `--launch` and `--spawn-main`
refuse another while it is active. A start first abandons a main that a crash or reboot left open when
the `--release-main` proof holds (expired heartbeat, host and native process gone) and says so on stderr;
an unverifiable main still refuses the start. The control window's board shows the room's live main. PageUp/PageDown scroll the detail
when one is shown, otherwise every call band by one page. Graphs of different
rooms are never merged, and `o Rooms` in the control window switches rooms.


### Shared dock state

The bottom box state (tab, previewed target and detail scroll) is shared by the panel
session in `@oms_panel_bottom`, capped at 512 bytes; malformed or oversized state is
ignored, and a target that does not name a participant in the current room is not
applied. Changes reach every board on refresh, and entering a window does not reset the
selection. The dock opens on the Log tab unless a tab was chosen or stored.

#### Dock size and scrolling

At heights of 28 rows or more, it reserves at least its height-band body (12, 8 or 5
rows) and takes every row left under the activity tree and warnings, with no blank
padding. The Log tab lists the recorded events (one line says so when there are none);
the strip shows how many events it is scrolled past, and `[` / `]` or the wheel scroll
it.

#### Completed-plan banner

A finished goal banner names `superseded` (instant) and `completed-external` (which
reruns acceptance, about 9 minutes), with the `oms agent-plan --repo . retire --check`
and `retire --apply` command forms; the check prints the SHA-256 CAS token required by
apply. Narrow banners retain the completion ratio. Terminals attached to the same panel
session share its current window, so navigating from one client also moves the other.
The wheel scrolls only the band or detail under the pointer.

### Footer and quick actions

The footer shows at most three hints chosen by the selection (a main: `Enter Chat`,
`a Ask advisor`; a call: `Enter Show`, `w Worktree` when it applies;
`Esc Close` while a preview or detail is open; on the Messages and Debate tabs the arrows, `Enter` and the filter or target keys) and always ends with the fixed panel keys
`F9 Chat⇄Board  m Modes  F12 Keys  ? Keys` when space allows (`q Quit` and `? Keys` on a standalone watcher); narrow managed footers drop `F12 Keys` before `? Keys`. `?` swaps in the full list of keys
that work in that view, with `v Expand` or `v Collapse` following the board's state, `x Transcript`
on a call, `[ ]` on the Log tab, paging keys and `Ctrl+L`; a hint that fits no row is dropped whole and
the list ends in `…` when any were dropped. A key with no binding in the board shows `Key not bound here;
? lists the keys`. The control menu's `[k]` groups and the result reader's key row come from tables in
`panel_keymap.py` (the reader drops whole hints, lowest priority first, at narrow widths). Notices
disappear on the next input or after ten seconds.

`a` in the watcher asks an advisor for the selected main (or the shown one). In tmux it
opens a popup that takes one question and runs `oms panel --dispatch advisor --main
PARTICIPANT` without the watcher's inherited caller identity; the dispatch re-proves
that main is live, and the answer returns to that main's room mail. Outside tmux the
board shows the command instead. The board split sizes the pane in cells; only a
detached or unmeasured window uses `-l N%`, and only on tmux 3.1 or newer (probed once
with `tmux -V`; older or unparseable versions get cells of the best known size). `-p` is
never used because tmux 3.3/3.4 mis-parse it. Popups need tmux 3.2 or newer; when the
popup cannot open, the board says so and the control menu's `a Advisor` remains. A
failed call keeps the popup open with its exit status. Selecting or clicking never calls
a model. A prompt-less `--dispatch advisor` on a terminal asks its question
interactively.

`w` on a selected, finished worker opens a plain shell window at its kept
worktree. The recorded `worktree:PARENT/NAME` label must match exactly one entry
of `git worktree list`; a running worker, a cleaned-up or ambiguous worktree
and repository-local calls are refused, and recorded results stay available.
The shell has no worker authority; edits there are not part of the recorded
patch. Outside tmux the board shows the path.

The board is an overview above and a detail area below. Cards keep only the rows their
content needs (an unrecorded location stays in the detail), and a provider without a
reading reads `Codex --` on the pinned usage line (the tree keeps its USAGE box where
model-use data exists). Antigravity shows rolling seven-day input, cached and output
tokens and calls (`agy 7d in … · cached … · out … / N calls`) from the most recent 1000
artifact index rows when available; these are recorded usage, not a quota percentage. On
a board of 24 rows or more, the bottom dock is reserved before the upper layout is sized
and then grows to the rows left under the tree.

When the main preview is automatic, it is marked `auto` and does not replace a chosen
preview. Esc dismisses the preview while preserving the box. Choosing a block replaces
the Detail content without resizing the reserved box; it shows the selected content,
starting with outcome and verification: for a worker or advisor the latest retained
answer, the changed files of a recorded patch (summarized only while its SHA-256 still
matches, so it survives worktree cleanup), outcome, verification and artifact references
(read in the background, refreshed with each snapshot); for a main its addressed mail
and team.

Reading mail there never acknowledges it. The native CLI pane itself is never used for
detail; the reader is a separate pane split from it. A CLI exit never changes a task to
accepted. The board is repository-wide, not a claim that every displayed task belongs to
this room. Declared scopes are labels, not file locks or authority; write workers still
use isolated patches, verifiers and owner admission. Usage retains the
recent-eight-artifact limit, selected/served attribution and unknown native usage.

### Room commands

```bash
oms room new --repo . --id team --topic 'Parser change'
oms panel --repo . --room team --launch codex
oms panel --repo . --room team --launch claude
oms panel --repo . --room team --watch --view graph
oms room show --repo . --id team --json
```

With an inherited room binding, `room list --repo OTHER` and `room show --repo OTHER --id ROOM` can read another repository without using the inherited room or participant.

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

The canonical thread log records participant roles, addressed message IDs, reply links
and explicit consumption. Stable message IDs deduplicate identical sends; conflicting
reuse fails. Broadcast recipients are fixed at send time. A new join does not replay
earlier messages. Reads exclude sender echoes and other participants' direct mail,
advance a checked cursor and do not acknowledge. Routine worker notes stay in the
durable log and `oms room updates` but are omitted from main hook prompts and
acknowledgment reminders. Typed questions, answers and handoffs, actionable notes
(questions, blockers, verification failures, review verdicts and owner decisions),
unclassified notes, and notes from other roles remain immediate.

Use `--kind question` for questions and `--kind handoff` for completion.
Delivery/consumption/answer/acceptance are separate states. Rooms are bounded to 32
members (mains and other unparented participants), 480 dispatched calls (participants
with a parent, which keep their slot after finishing) and 1024 messages, so each tick's
room read stays bounded.

Bounds apply per room log. When a room passes 896 messages or 416 calls, the next
dispatch rolls it over to a linked successor (`oms room rollover --id ROOM` does the
same by hand); ids that name the old room keep working, the old log is sealed and
retained (`oms room show --id OLD --exact`), and nothing is discarded. The successor
carries joined mains, running calls, calls whose results are unread, mail still owed to
calls, open questions (a call's oldest one; its later ones count as other mail) and each
main's latest status, plus the newest other unread mail up to 128 messages; older unread
mail stays in the sealed log, a note in the successor says so, and acknowledging it
through either id records the receipt.

Carried mail keeps its send time and a broadcast its original audience; a call whose
parent left stays behind. Until its carry completes the successor refuses other writes;
the next write by a main or the panel finishes a carry a crash cut short, and a worker's
write is refused until then, since workers cannot carry. The successor keeps the sealed
room's creator, and a rebound main keeps its first native session. The Past tab also
lists the previous room's settled calls. A running main keeps its environment and native
session: its writes, reads and dispatches follow the old id to the successor.

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

Mains declare what they are editing so their dispatches and other sessions can see it:
`oms room scope --id team --participant opus-main --owns scripts/lib --owns
docs/TERMINAL-PANEL.md` replaces that participant's declared scopes and `--clear`
empties them. Only the participant itself may declare (the inherited
`OMS_ROOM_PARTICIPANT` must match; workers cannot); at most sixteen normalized
repo-relative scopes; advisors and reviewers own none. A write dispatch takes repeatable
`--scope PATH`; omitted scope uses the plan task’s allowed paths or the whole repository (`.`), and the worker records its task id with its scopes.

Two scopes overlap when one equals the other or is a path-component prefix of it
(`scripts/lib` overlaps `scripts/lib/room.py`, not `scripts/li`; `.` overlaps
everything; globs compare literally).
Write dispatches, including `--dry-run` (also from a rebound main whose environment lost its room id), refuse overlaps with a joined main's declaration or with a call's joined write declaration until the controller records that call's structured call result or its parent main reads the call's result handoff; a same-task `--continue` waits the same way.
An unread handoff is not delivery. A rollover carries exactly the calls still holding a declaration, so the successor room's participants are the whole lock. A call whose parent main left holds nothing, because nobody remains to integrate it. Liveness is read from joined state, never from join order: a superseded main that never left, and its calls, keep their declarations (an over-lock, never an under-lock). Once its process has stopped, a stale declaration clears with `OMS_ROOM_PARTICIPANT= oms room scope --repo PATH --id ROOM --participant ID --clear`; the refusal prints that exact command. A finished write call's id is single-use: rejoining it with a declaration is refused, so old delivery evidence never releases a new declaration.
Delivery releases the declaration even while the result awaits the owner's decision: the WIP limit, not the lock, paces undecided results.
A write dispatch first delivers its own main's finished calls, then checks declarations, and rolls the room over only after they pass. A call cannot leave the room as itself: its parent main records the departure, including the controller's rollback of an unstarted call.
A dispatch that loses the atomic check at enrollment keeps its plan claim and prints the lease and the exact `oms agent-plan release` command: releasing automatically could race another dispatch of the same task on the shared lease.
The refusal identifies each blocking task, participant and scope; there is no override. The main detail view shows `Scope: …`. Finalizing as
`accepted` refuses write-worker patches outside their declared scopes (including rename
sources and destinations), using glob-aware path matching; `completed` remains allowed,
and workers without declared scopes keep the existing behavior.

### Original chats and recovery

`h Chats` lists the selected room's participants. Selecting a native main focuses its
existing tmux conversation pane when that pane still exists and its canonical
repository, provider, room and active main attempt match the enrolled participant. First
launches and resumed stable participants both qualify; blocked but nonterminal attempts
remain navigable. Old or unprovable window markers do not establish a destination. When
that destination is in another tmux session, navigation selects its window and pane and
switches the single client attached to the caller's verified session.

Missing or multiple current clients require selecting the original terminal; another
attached client is never chosen by recency. Herdr pane lookup failures affect their
participant while other proven targets remain available. An unqueried duplicate
candidate cannot establish unique ownership, and socket failures continue to make host
evidence unavailable. Without a proven terminal destination, Codex uses the exact local
session's `codex://threads/<thread-id>` app link. Claude Desktop navigation uses `claude
--desktop --resume <session-id>` on macOS/native Windows when the installed CLI
advertises `--desktop`; native platform, authentication and live-session restrictions
still apply.

It never force-closes a running CLI. Linux/WSL users use the original Claude app sidebar
or the existing terminal; OMS does not invent a Claude Code session URI. Headless
workers expose their recorded results and artifacts instead of a native app chat.
Unbound or undiscovered sessions say `unresolved`. The list shows ambiguity reasons and
marks participants that left. Their proven original chats remain available as history;
this does not rejoin them or resume work. Room views and chat navigation require an
explicit selection from `o Rooms`, `--room` or the room environment; another session's
`CURRENT` pointer does not select a work room.

Narrow panes retain joined main names and mailbox totals; an explicitly requested graph
uses the 76x16 requirement, retaining tree navigation when the interactive card layout
cannot fit.

`--chats` is read-only. `--open-chat PARTICIPANT --dry-run --json` previews the
destination without navigation; an explicit open submits no new prompt. Local
lookup examines bounded filenames and Codex's first metadata row, without
copying transcript bodies or native session IDs into the shared room log.
Navigation success is a request, not evidence that an app window appeared.

A native main launched by the panel binds its first real prompt-hook session to its
explicitly enrolled participant. This preserves the original join boundary, pending
messages and cursor. An existing unbound Claude participant can also be bound by its own
agent with `oms room bind --repo . --id team --participant opus-main --native-session
current`. An already bound identity cannot silently change; a different native session
requires explicit leave/rejoin. Closed or left membership does not auto-resume. A main's
own SessionStart moves its binding when the provider starts another native session in
that process (Claude `clear`; Codex `clear`, `resume` or `fork`), replacing only the
identity bound at that moment; `startup` and `compact` never do. Later hooks compare
both enrolled provider and native identity before reading or advancing a cursor. A
mismatched user prompt receives a short reconnection notice; tool hooks withhold the
room mail without repeating that notice.

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

A main can explicitly select a registered CLI for a bounded worker, advisor or reviewer.
`--to codex|claude` without `--model` pins that provider's newest candidate family and
effort from the role's tier; an explicit `--model` overrides it. Other transports
supporting per-call model selection require an exact `--model`; omit the `--workload`
tier. An optional `--reasoning-effort` is forwarded through the existing provider
capability checks. With an explicit model or custom route, omitting effort retains the
front door's native settings. For profile-controlled transports such as Vibe/DeepSeek
Harness, omit `--model`; the native profile selects it and an exact override is refused.

Preset roles use a capability tier ordered by weekly quota left, with readings within
the 15-point tie band going to fewer room dispatches, then away from the main (see Roles
below). The light tier starts with Pi’s Inkling route when available; researchers remain
on Haiku or Luna. Codex delegated routes accept a versioned model of the Astra, Sol,
Terra and Luna families (cost order, highest first). Model-family versions are distinct
from roles. Preset dispatch chooses the newest configured/catalog numeric version within
its existing family and pins the exact ID; a newer Astra never replaces a Terra, Luna or
Sol route.

`oms models` routes a family only at its newest version, and not at all while that
version is a major generation behind the others (any 5.6 model once 6.x is listed).
Preset dispatch applies the same rule: a tier candidate whose family is not routed is
ineligible, the usual availability and quota rules choose among the rest, and with none
eligible the dispatch is refused. Explicit CLI selections retain the caller's exact
model request. A compact model preview does not limit routing.

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

The inventory distinguishes headless calls, room polling, native launch and unverified
native hook wiring. A call adapter alone does not attach to an already-open app or wake
an idle session. For additional native launch, explicitly list canonical provider IDs in
`OMS_PROVIDER_NATIVE_ADAPTERS` and provide the marker executable `oms-agent-adapter-ID`
on PATH or in the same adapter directory. Its additional contract is `open --workdir
PATH --context-file PATH`, with optional `--model` and `--resume`. The file contains
bounded OMS bootstrap, room participation and the supplied user task; it is removed
after process exit.

The adapter must preserve interactive stdio, forward exit status, consume that context,
and translate only supported documented vendor flags. It inherits `OMS_ROOM_ID`,
`OMS_ROOM_PARTICIPANT` and canonical `OMS_ROOM_REPO` for polling. The `p CLIs` menu's `n
Native` action or `--launch ID` uses this explicit capability; unsupported native
adapters fail rather than falling back to a headless call. This opt-in grants no
write/publication authority. The actual Grok/Muse interactive interfaces have not been
verified; the contract is covered by protocol fixtures.

Owner finalization/council retains Codex/Claude's existing policy. No additional daemon,
dependency or terminal injection is added.


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

A panel main's prompt hook also appends the cached project orientation
(`oms graph project map --brief`) once per graph revision and context
generation; compact, clear and resume re-emit it, and a missing cache stays
silent. It reads only the existing cache and never starts a graph refresh;
refresh remains on the panel-start warm path and dispatch preparation. The
hook stays silent when `OMS_GRAPH_AUTOBUILD=0` or
`OMS_PROJECT_GRAPH_STATE` is relative. Other sessions are unaffected.


## Contract and verification

`--json` returns schema 1, kind `oms-panel`, a nested `oms-dashboard`, provider
`installed` booleans, `split_available` and additive `routing` policy. Lifecycle
metadata is written through typed `agent-events --ref NAME=VALUE` and projected
as bounded panel metadata; see [the dashboard contract](DASHBOARD.md).

Exit 0 is successful collection, 1 is degraded collection or launch failure, 2
is invalid usage or a worker attempting owner actions, 3 is an unusable or
non-repository `--repo` (or current directory), 75 is deferred finalization
while review read passes remain live, and 130 is interruption.
Native launches propagate the process exit. Existing tools retain patch
admission, verification and publication authority. No permission bypass,
automatic patch application, commit, push, installation or new service is added.

The existing operator-tools suite exercises exact role routes through real OMS front
doors and lifecycle writers using fixture provider executables. It checks model/effort
arguments, parent links, titles, read/write separation, worktree isolation, returned
patches, review gates, worker recursion rejection and read-only views. Native PTY and
real tmux checks cover inline restoration, exact resume, main selection, independent
sidebars and detach/preservation. Width/height, Korean title wrapping, cached frame
cadence, changed-row repaint, resize, reduced motion, attribution and unsafe-label tests
cover the renderer.

A PTY watch verifies actual incremental animation output without lifecycle writes. The
lifecycle-events suite covers bounded refs and immutable idempotency. Fixture execution
does not establish account access or a live main's judgment quality. When a sandbox
denies Unix sockets, the suite reports socket-dependent sections as skipped and exits
77; parent verification outside that sandbox remains authoritative. The same operator
suite covers council routes, durable results, verifier and patch lineage, privacy
refusal and delivery deduplication/retry after interruption.

The notifier suite covers unloaded and busy receivers, shell wrappers, early events,
lost acknowledgements and correlated persisted output. The operator suite prints
per-section and per-shard timings. `ci-status history` shows recent completed CI runs
and ranked failing jobs/tests. The operator suite also verifies the main hierarchy with
earlier mains on record, legacy ownership, separate review groups, compact/detail
budgets, attention filtering, numbered result selection, menu cycling and view
inheritance in an existing tmux server. Use `tests/operator-tools-smoke.sh
--list-sections` to see focused verification areas: `auto-review` covers the review
gate, `native-pty` covers terminal lifecycle behavior, and `room-rollover` covers room
succession; combine independent areas with commas.

`core/room-and-board-rendering` covers board rendering and shared-plan verifier
warnings; `dispatch-results` covers dispatch and result handling. Size a composed
`--verify` from its measured section timings: sequential sections add together, and slot
waiting precedes the verifier clock. Use a finite caller override when that composition
outlasts the default. Local measurements on 2026-10-31 were 16s for
`core/room-and-board-rendering`, 422s for `dispatch-results` and 4s for the two selected
timeout regressions (`test_python_timeout_fallback_bounds_verifier` and
`test_run_with_timeout_require_refuses_unbounded`).

These scoped runs did not reproduce a 20m budget exhaustion; they do not bound the full
serial suite or other host loads.


## Researcher, scholar and supporting strategies

Use `oms panel --dispatch researcher --owner claude --prompt 'Find and cite the relevant contract.'`
for one bounded document/source question. The control menu's
expanded `e Researcher` entry uses the same route. Researcher is a separate room
participant beneath its main; its bounded answer is retained in Results and its
status appears in tree/graph views. It never becomes a main or a patch approver.

The researcher takes the light tier's Haiku or Luna candidate, whichever provider is
installed first (`--to codex|claude` narrows it). Current configured floors are
`claude-haiku-5-5` and `gpt-6-luna`. Read-only researcher seats that the panel
dispatched may use provider web search/fetch while judging roles remain web-free. A
researcher reads the repository and the web in one context, so repository text can leave
the machine in a search query or fetched URL; do not dispatch researchers in a
repository whose content must stay local. For a pure "where is X in this repo"
question add `--no-web`: the researcher launches with the four-tool read belt
(`Read,Grep,Glob,Bash`; Codex `web_search="disabled"`) and no web tools. The
default is unchanged, and `--no-web` is refused for every other role, including
scholar. The dispatcher compares numeric versions
within that family against the cached provider catalog and pins the newest exact ID.

An omitted minor version means zero; `6.10` is newer than `6.2`. Cached discovery does
not prove account access or live availability. No paid availability probe runs. If the
pinned model cannot run, the task fails without an implicit older-model or more
expensive-family substitution. A provider without a model catalog retains the configured
exact ID until policy is refreshed.

Use `oms panel --dispatch scholar --owner claude --prompt 'Compare the primary sources with verified citations.'`
for a multi-source literature survey. Scholar uses the deep tier at high effort by
default, accepts `--workload frontier`, and keeps read access and the research
purpose. It shares the researcher board kind and web tool policy, with the
`scholar` strategy recorded separately. Invalid research strategies, board roles,
purposes and write access are refused before the provider runs, including calls with an
existing attempt. Scholar has a 60-minute default wall; `OMS_PEER_TIMEOUT`
overrides it. Review gates and worker continuation remain unavailable.

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
unknown. A current reported 5-hour window adds a `Claude · 5h N% left · resets in …`
row and, in `--json`, a `Claude 5-hour left` item carrying `resets_at` and
`advisory: true`; an advisory item changes neither `level` nor the board's capacity
alert, and no item is listed when the window was not reported, has rolled over, or the
readings are stale. Context bands reuse the existing `OMS_CTX_WARN_PCT` (15% left) and
`OMS_CTX_URGENT_PCT` (8% left) settings and existing hook handoff/compaction flow.
The health screen makes no model calls and changes no native permissions.

`oms panel --repo . --sessions [--room ID] [--json]` prints one normalized status per
recorded session or call, for notifiers and outside scripts. Text mode is one line each:
`ID PROVIDER/ROLE STATE waiting=WHO raw=RAW since=TS source=SRC [stale]`. `--json` is
schema 1, kind `oms-sessions`: top-level `stale`, `stale_reason` and `sessions`, each row
carrying `id` (the attempt id, else the room participant), `attempt`, `participant`,
`provider`, `role`, `state` (`working | waiting | blocked | done | failed | unknown`),
`waiting_for` (`input | approval | review | owner | none | unknown`), `raw_state` (the OMS
state it came from), `since`, `since_basis`, `source` and `stale`. The view is a projection
of the board's own report with one mapping table (`STATE_MAP` in
`scripts/lib/session_status.py`); a raw state without a row reads `unknown`. `quiet`,
`presence unknown` and `orphaned` stay `unknown` (the evidence does not say), `idle` is
`waiting` for `none`, and `cancelled` is `done`; use `raw_state` to tell them apart.
`since` is set only where a record gives it (a main's native activity, or the end of a
finished call, whose `since_basis` says `ended` or `last event`); a running call has no
state-change time recorded, so it reads null. `stale` is true when the read was held, the
refresh failed or the attempts source was unreadable; an empty `sessions` with a true
top-level `stale` means unknown, not none. Exit status follows the board's snapshot. It
calls no provider CLI and no model; provider-native inputs such as `claude agents --json`
are a named but unimplemented extension point in the mapping table's docstring.
Observation is not approval or verification evidence.

Clearing the panel display does not free model context or subscription quota.
Use native compaction or an explicit handoff for context, and the native quota
reset for exhausted usage. Room acknowledgement keeps message evidence; new
mains already use a fresh bounded room near capacity. Existing sessions and
their room evidence stay intact.

## Salvaging a stopped worker

Run `oms panel --repo . --salvage TASK_ID [--attempt ATTEMPT_ID] [--json]` to capture
the latest stopped delegate's recorded, kept execution worktree, including a separate
clone or scratch worktree, or the explicitly selected stopped attempt of that task.
Selection keeps the same room-owner, stopped-state and worktree identity checks; an
attempt from another task is refused. It preserves that checkout and index, and reports
changed paths, before/after byte sizes, the patch path and its `.patch.json` metadata
under `.oms/artifacts/delegate/`. Metadata and the patch header carry `salvaged_from`
(task, attempt, kept path and stop reason), base and payload hashes, the UTC
`salvaged_at` timestamp, file sizes and `encoding_warnings` for display. Continuation
selects the newest salvage for the same task and attempt by `salvaged_at`, then filename,
independently of filesystem timestamps. Its timestamp must match the verified patch
header. Legacy captures without that field in either file sort before dated captures.
If every capture is legacy, materialization requires a single distinct payload across
all verified captures. Different legacy payloads refuse continuation because filenames
cannot establish recency; inspect the retained captures and include the intended recovery
in a fresh dispatch brief. A single legacy capture or several identical-payload legacy
captures remain selectable.

Continuation verifies every capture for the attempt and requires each controller
provenance entry to have an intact patch and sidecar. Interrupted publication, damaged
files or invalid timestamps refuse continuation instead of falling back to an older
capture. Unreadable artifacts that cannot be attributed to another attempt also refuse.

Admission uses controller records in `.oms/salvage-provenance/`, keyed by the diff
payload hash; editing headers, sidecars or warning lists cannot change those records.
The flags guard the salvaged artifact as captured; an edited or reformatted patch is a
different patch that ordinary admission checks. `patch-admit` examines every textual
patch hunk for encoding loss, independent of salvage identity; `--accept-encoding-loss`
records owner acceptance for detected hunks. Warnings flag hunks whose replacement has
the same ASCII skeleton after Unicode decomposition but fewer non-ASCII characters, and
files losing more than half their non-ASCII bytes.

These are review hints; they can include intentional edits or deletions. Review the
patch before using `oms patch-land --patch PATH --accept-salvaged`. Flagged encoding
loss also requires `--accept-encoding-loss`; normal admission checks still run. Salvage
captures evidence and never admits or applies it.
