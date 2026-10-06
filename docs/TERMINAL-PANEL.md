# OMS terminal control panel

Run `oms` in a terminal to open the control panel. Native Claude Code or Codex
keeps the conversation pane; the OMS sidebar shows recorded main, advisor and
worker activity. `oms --help` and bare `oms` with redirected input/output retain
command help. The panel uses existing OMS state, authentication and front doors.

```bash
oms panel --repo . --layout inline         # portable menu, no tmux needed
oms panel --repo . --layout split          # uses an already-installed tmux
oms panel --repo . --json                  # read-only status and role policy
oms panel --repo . --routes                # read-only role policy
oms panel --repo . --providers --json      # registered CLI peers; no provider execution
oms panel --repo . --watch --owner claude  # read-only activity sidebar
oms panel --repo . --watch --view compact --attention-only
oms panel --repo . --room team --watch --view tree
oms panel --repo . --launch codex --task 'Explain the parser and fix its bounded bug'
oms panel --repo . --launch claude --resume SESSION_ID
oms panel --repo . --launch codex --model MODEL --dry-run
oms panel --repo . --results               # summaries first, ordinary scrollable text
oms panel --repo . --results --task-id ID --json
oms panel --repo . --room team --chats --json
oms panel --repo . --room team --open-chat sol-main --chat-surface app --dry-run --json
oms panel --repo . --council --owner codex --task-id ID --label 'Decision' --prompt 'Compare options' --dry-run
```

## Shared work rooms and the graph

A native main launched from the menu joins the selected work room. Opening a
second native main from that menu keeps the same room; independent panel
launches create separate rooms unless an existing `--room ID` is selected.
The `o Rooms` menu selects or creates a task-scoped room. It does not enroll
other sessions merely because they share a repository. Auto uses a connected
vertical tree for a selected room at 28 columns and 12 rows or larger. All
joined mains have rows; empty role cards are omitted. Explicit parent IDs
connect advisors, reviewers and workers under their own main. Calls whose
parent is outside the shown mains stay in `UNLINKED`. Scrolling exposes rows
outside the viewport. `--view tree` selects this layout directly. `g Graph`
retains the card graph at 76 columns and 36 rows; smaller graph panes explain
the fallback. `--view compact|detail` keeps the list at any size.

On a POSIX terminal, the right watcher accepts clicks, arrow keys/Tab and
Enter. Clicking a main focuses its proven existing tmux conversation pane;
if no terminal exists, a proven exact Codex app link can open its original chat.
An unresolved or departed participant shows a reason. These clicks do not
start or resume CLI sessions, send a prompt or call a model. Workers,
advisors and reviewers open recorded results; repository tasks show the
displayed snapshot's title, dependencies and declared paths. Escape returns
from details. Click a main's left chevron or press Space to fold its calls;
the wheel and PageUp/PageDown scroll. `t`, `g`, and `b` select tree, graph and
attention views in the watcher; `q` closes only that watcher. Resize and
collection discard pending coordinate clicks before drawing a new map.

Mouse support is enabled only in the OMS tmux session, never globally. Hold
Shift for the terminal's own text selection. The watcher restores mouse
reporting, echo, canonical input and the cursor on exit, interrupt and
HUP/TERM. Redirected output stays plain and passive. Unsupported terminals
retain the existing control-menu chat/result selection.

The graph shows joined mains, advisors/reviewers, workers, a mailbox, the
repository's canonical task board, declared scopes and the latest room log.
A running council launched by a shown main appears in the advisor card as
`DEBATE` with its answering seat count; its seats never stand in for the main.
The default tree keeps the same evidence in a `COUNCIL` branch; clicking it
reads that task's results scoped to its main participant.
Only explicit parent IDs draw call connections. Lifecycle state drives moving
particles/spinners; joining without lifecycle evidence says `presence unknown`.
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

```bash
oms room join --repo . --id team --participant sol-main --provider codex \
  --role main --model gpt-6-sol --native-session EXACT_SESSION_ID
oms room join --repo . --id team --participant opus-main --provider claude \
  --role main --model claude-opus-5-5 --native-session EXACT_SESSION_ID
oms room send --repo . --id team --participant sol-main --to opus-main \
  --kind question --message-id api-question --text 'Inspect the caller contract.'
oms room updates --repo . --id team --participant opus-main --json
oms room send --repo . --id team --participant opus-main --to sol-main \
  --kind answer --reply-to api-question --text 'Caller contract confirmed.'
oms room ack --repo . --id team --participant opus-main --message api-question
```

The canonical thread log records participant roles, addressed message IDs,
reply links and explicit consumption. Stable message IDs deduplicate identical
sends; conflicting reuse fails. Broadcast recipients are fixed at send time.
A new join does not replay earlier messages. Reads exclude sender echoes and
other participants' direct mail, advance a checked cursor and do not acknowledge.
Delivery/consumption/answer/acceptance are separate states. Rooms are bounded
to 32 participants and 1024 messages; start a new room when those limits are
reached. History is retained, not silently discarded.

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
histories or write source files in another main's worktree.

### Original chats and recovery

`h Chats` lists the selected room's participants. Selecting a native main focuses
its existing tmux conversation pane when that pane still exists and its canonical
repository, provider, room and active main attempt match the enrolled participant.
First launches and resumed stable participants both qualify; blocked but
nonterminal attempts remain navigable. Old or unprovable window markers do not
establish a destination. Otherwise,
Codex uses the exact local session's `codex://threads/<thread-id>` app link.
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
an explicitly requested graph explains the 76x36 requirement when using the list.

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
Later hooks compare both enrolled provider and native identity before reading or
advancing a cursor. A mismatched user prompt receives a short reconnection notice;
tool hooks withhold the room mail without repeating that notice.

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
absent and failed delivery leave work status unchanged. No new receiver chat
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
the most recent session. Another main can then send the room snapshot:

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
or reviewer. `--to` requires an exact `--model` for transports supporting per-call
model selection; omit preset `--workload` and
`--seat`. An optional `--reasoning-effort` is forwarded through the existing
provider capability checks. Omitting effort retains the front door's native
settings. For profile-controlled transports such as Vibe/DeepSeek Harness,
omit `--model`; the native profile selects it and an exact override is refused.
Default role allocation remains Sol/Opus, Luna/Sonnet and Astra/Fable.
Codex delegated routes accept only the three GPT-6 models in the panel policy.

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
Additional mains can use routine/light/advisor routes; main-level worker routing
requires an explicit target. Owner finalization/council retains Codex/Claude's
existing policy. No additional daemon, dependency or terminal injection is added.

## Roles and automatic allocation

The native main owns the task, architecture, workload judgment, final synthesis
and acceptance. It receives instructions to inspect source and dispatch bounded
subtasks through `oms panel --dispatch`. There is no additional classifier call
or prompt keyword router. Selecting Auto task (`t`) supplies the task as a
native user turn; when entering work in the native conversation, the same
instructions apply. Model selection for each bounded call is part of that task.
The launcher does not autonomously fan out before the main has a task.

| Role / workload | Exact selected model | Effort | Authority |
| --- | --- | --- | --- |
| Codex main | `gpt-6-sol` | high | Native main permissions |
| Claude main | `claude-opus-5-5` | high | Native main permissions |
| Light, clear worker | `gpt-6-luna` | low | Bounded read or isolated write |
| Routine implementation / long straightforward explanation | `claude-sonnet-5-5` | medium | Bounded read or isolated write |
| Main-level worker | Owning main's Sol / Opus preset | high | Bounded worker; no promotion |
| Astra advisor / reviewer | `gpt-6-astra` | high | Read-only |
| Fable advisor / reviewer | `claude-fable-5-1` | high | Read-only |

[The panel policy](../config/panel-routing.json) is separate from generic OMS
provider defaults. An explicit native `--model` override preserves native effort
settings; main-level workers still use the policy preset. Advisor `--seat auto`
selects Astra for a Claude main and Fable for a Codex main. Explicit seats allow
either advisor. Advisors are useful at material decisions; the bootstrap does
not ask the main to call both on every turn. A missing provider or unsupported
model fails that route without silently selecting another model.

Task complexity and write authority are independent. A write worker needs an
implementation purpose, scoped brief file and mechanical verifier. Read workers
use isolated worktrees too. Write workers return a patch for parent inspection
and admission; the panel does not apply it. Reviewers require a verifier and
record a review gate. Workers cannot recursively dispatch or open owner sessions.
Normal native permissions, harness policy and account consent remain in effect.

```bash
oms panel --repo . --dispatch worker --owner codex --workload routine \
  --purpose explain --access read --task-id parser-notes --label 'Explain parser' \
  --prompt 'Explain the parser using source evidence.' --dry-run
oms panel --repo . --dispatch worker --owner claude --workload light \
  --purpose investigate --prompt 'Identify the relevant configuration entries.'
oms panel --repo . --dispatch worker --owner codex --workload main \
  --purpose implement --access write --brief-file scoped-task.md --verify 'bash tests/affected.sh'
oms panel --repo . --dispatch advisor --owner claude --seat fable \
  --label 'Review architecture options' --prompt 'Compare these bounded design options.'
oms panel --repo . --dispatch reviewer --owner codex --seat astra \
  --prompt 'Review the changed parser.' --verify 'bash tests/affected.sh'
```

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

## Layout and activity

`auto` chooses split on POSIX when tmux is installed and the terminal is not
already inside tmux; otherwise it uses inline. `split` creates an independent
`oms-panel-*` tmux session with a control window. Each native conversation gets
its own sidebar with state refreshed every five seconds. Existing windows and tmux settings
are preserved. With default bindings, `Ctrl-b w` picks a window and `Ctrl-b d`
detaches while work continues. Quit in the control pane also detaches. Failed
construction rolls back only the new OMS session. Inline returns to the menu
when the native CLI exits and refreshes then; it has no permanent sidebar.

Each recorded MAIN has a closed orange card. ADVISORS, REVIEWERS and WORKERS
share that card's borders, with separate labelled role dividers. Individual
calls keep their title and metadata inside the role section without nested
boxes. The repository control view shows `NEXT MAIN` as a one-line preset;
that preset never fills missing model/effort fields in a recorded main.
The native main card shows provider, requested model/effort and recorded task
when supplied at launch. Missing fields remain unrecorded.
Tasks entered later inside the native conversation are not intercepted:
its main title stays unrecorded unless supplied at launch, while dispatched child
titles are recorded. Purple ADVISORS and cyan WORKERS show lifecycle state,
model, task title/ID, read/write authority, effort and location. Worktree locations
use a relative unique suffix; repository calls show `repository`. Detached
worker worktrees can be cleaned after completion; their locations remain history.
Task titles have their own bright lines, wrapping to two lines in wide views
and up to three in narrow sidebars. Main task titles wrap too. Internal task IDs
stay in wide metadata rows or Details. Details gives bounded titles separate
wrapped lines, so long titles need not compete with location and purpose.
No absolute home paths or transcripts are added to panel metadata.

Activity has three levels: a recorded MAIN, its ADVISORS / REVIEWERS / WORKERS,
then each call and its task title. The repository view keeps separate native
mains in separate cards, in stable attempt-ID order with UNLINKED last. The
available space expands cards with attention calls first; a card that cannot
fit is a one-line summary with its leading attention/task title and role counts.
A native window uses its main card as the root.
Legacy calls without explicit panel ownership appear under UNLINKED; a generic
attempt parent is never promoted into a native-main relationship. A linked
owner whose metadata has left the display window is labelled unavailable.

Press `v` to cycle auto / compact / detail. Auto selects compact when the
recorded calls would crowd the pane. Compact retains model, state and one task
title line. At 80 columns or wider, these share one row with metadata on the
right; narrow views place metadata below the title. Detail expands wrapped
titles and location/effort metadata. Up to eight calls fit the bounded view;
role dividers report hidden calls and the overflow line shows nonzero counts
with `8 Details`. Small panes use borderless summaries. Press `b` to
toggle attention-only: waiting input, waiting approval, blocked, review and
orphaned calls, plus retained failed terminal outcomes. Filtered calls are
counted rather than changing their states. Both controls affect presentation,
not the main identity or dispatch authority. A native window inherits these
settings when opened, and the control sidebar follows subsequent menu changes.
CLI `--view` overrides a watch's window density; `--attention-only` keeps its
filter enabled. These flags apply only to the menu or watch, not model operations.

The top has one `Claude` row and one `Codex` row, each prioritizing weekly
**used** percentage and the selected main's context **left** percentage.
The exact current main wins; otherwise a unique joined main is required.
Multiple candidates show `main ambiguous`, and missing readings show `--`.
Claude's existing session HUD cache supplies reported context and optional
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
`--count` counts collected snapshots rather than animation frames.

Use `oms panel --watch --no-animation` for a motion-free view, or set
`OMS_PANEL_NO_ANIMATION=1` in the launching environment to cover automatically
opened sidebars. Redirected output and `TERM=dumb` are static plain snapshots.
`NO_COLOR` controls color independently. The watch restores the terminal cursor
on exit or interruption. The control menu redraws on actions rather than
animating over an input prompt.

A native window binds children by its recorded main attempt ID. Other mains'
activity is counted separately and available in Details. The control view shows
repository-wide records. Its selected Main controls subsequent manual dispatch,
and its sidebar follows that selection. Main/child links come from explicit
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
| 1 Codex / 2 Claude | Open a native Sol / Opus main |
| 3 Resume | Select provider and exact native session ID |
| 4 Main | Choose the identity for subsequent manual dispatch |
| t Auto task | Enter a task and open the owning native main |
| 5 Explain | Read-only worker; choose light / routine / main workload |
| 6 Implement | Choose workload; scoped brief and verifier required |
| a Advisor | Choose auto / Astra / Fable, read-only |
| 7 Review | Choose advisor seat; mandatory review verifier |
| c Council | Sol, Opus, Astra and Fable discussion; four seats, two provider families |
| r Results | Numbered result list; choose a number to read summaries, evidence and model answers |
| f Finalize | Record owner outcome, summary and evidence; request app delivery |
| n Retry delivery | Explicitly retry a saved result; an uncertain earlier send may duplicate |
| 8 Details / 9 Refresh | Read shared dashboard; no model call |
| v Density / b Attention | Cycle auto / compact / detail; toggle action-needed calls |
| q Quit | Exit inline or detach the owned split session |

`NO_COLOR` disables color; `TERM=dumb` uses ASCII without screen clearing. The
view fits the current pane width/height. Redirected watch output appends plain
snapshots; `--count N` bounds it. Native launch needs an installed, authenticated
provider. `installed` means executable found on PATH, not verified login or model
availability. Shared OMS references carry evidence across providers; native
conversations remain separate. Multiple mains do not serialize edits: coordinate
main writes and use isolated workers for implementation.

## Discussion and results

The council uses the existing `peer-ask` debate, thread, artifact and lifecycle
front doors. One rebuttal round means eight primary seat calls; two means twelve.
Format repair can add one call per primary call. `--dry-run` exposes both counts
without executing a model. Four seats must complete to report council success;
failed seats remain visible and the owner still decides the outcome. All seats
are read-only. There is no model fallback or additional synthesis-model call.

The owning main records a result through `--finalize`. Native process exit and
worker completion never create accepted results by themselves. `completed`
records an owner summary; `accepted` additionally requires a passing explicit
`--verify`, at least one `--evidence` reference, and matching landed patch
evidence for every recorded write worker of that task or of the finalizing native
main, including subtasks with distinct task IDs. A plan-bound patch retains its
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

```bash
oms panel --finalize --owner codex --task-id ID \
  --summary-file SUMMARY.txt --outcome accepted \
  --verify 'test -f RESULT.txt' --evidence RESULT.txt --notify
oms panel --results --task-id ID
oms panel --retry-delivery --task-id ID
```

Results lists the latest twenty retained tasks from at most 300 lifecycle
attempts and 1000 artifact events. Selecting a task shows up to eight recent
call records, with bounded normalized answer sections; raw prompts, provider
logs and native conversation histories are not exported. Missing, changed or
linked evidence is reported as unavailable. Display bounds can truncate long
text; the registered result and source artifacts retain their full bounded content.
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
The same operator suite covers council routes, durable results, verifier and
patch lineage, privacy refusal and delivery deduplication/retry after interruption.
The notifier suite covers unloaded and busy receivers, shell wrappers, early
events, lost acknowledgements and correlated persisted output.
The operator suite also verifies multiple-main hierarchy, legacy ownership,
separate review groups, compact/detail budgets, attention filtering, numbered
result selection, menu cycling and view inheritance in an existing tmux server.
