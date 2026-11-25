# Collaboration dashboard

This guide is for operators who need a quick read of recorded OMS collaboration.
It covers the terminal dashboard, its JSON contract and the status available in project chats.

`oms dashboard` is a read-only terminal view of what OMS knows about a
repository's collaboration: goal and scope, task/plan and attempt states, which
provider and model ran each recorded operation, acceptance evidence, and the
next decision from the inbox.

```bash
oms dashboard --repo PATH                       # one snapshot
oms dashboard --repo PATH --json                # schema-1 projection
oms dashboard --repo PATH --watch               # refresh every 5 s until Ctrl-C
oms dashboard --repo PATH --watch --interval 10 --count 3
```

`--interval` takes whole seconds (minimum 2, default 5). `--count` stops after
N refreshes. On a terminal each refresh clears the screen after the new view is
rendered. When stdout is a pipe or file, snapshots are appended and separated
by a `----` line. Nothing is erased. `--json` emits one snapshot and cannot be
combined with `--watch`. Width follows `COLUMNS`, then `tput cols` on a
terminal, otherwise 120 columns.
Terminal watch mode also follows `LINES` or `tput lines` (default 24), keeping
summary headings, model routes, failures and the next decision visible while
omitting older details. The footer says how many lines are hidden; a single
snapshot or JSON retains the bounded detailed view.

Exit status: `0` when every source was collected, `1` when the view is degraded,
`2` for usage errors, and `130` on Ctrl-C.

## What it reads

Each refresh runs three existing read-only queries:

| Source | Query | Used for |
| --- | --- | --- |
| state | `oms state --json` | goal, scope, task, plan, attempt counts, delegations, acceptance criteria, inbox attention |
| artifacts | `oms artifact-index --json list 8` | recent operations, provider/model attribution, review rows |
| attempts | `oms agent-events list --limit 8 --json` | recent attempts with reported usage |

The attention section comes from the same `project_inbox` projection as
`oms inbox`, applied to the state snapshot. The dashboard does not write `.oms`
or call a model. It does not refresh CI, touch the network, or keep a store of
its own. The lifecycle stream is read twice per refresh: once inside
`oms state` and once for per-attempt usage, which the compact state omits.

## Reading the view

- **Coverage.** Only OMS-managed records are shown. A native provider subagent
  that never wrote OMS state does not appear. The dashboard does not infer a
  supervising agent or a plan/execution/integration stage.
- **Unknown is not zero.** A missing owner, model, token count, or cost shows
  as `unknown` in text and `null` in JSON. Attempt usage counts only when the
  provider reported it.
- **Model attribution.** `model=` is the selected route model recorded at
  dispatch. `served=` is the model the provider reported running, or
  `unreported`. `route=` is the recorded route class (`explicit`,
  `role-default`, `provider-default`, `fast`, `balanced`, `deep`). A fallback
  shows its recorded reason, and any reason outside the known set shows as
  `other`.
- **Completion is not acceptance.** A `done` attempt only means that the
  worker stopped. Acceptance is the evidence status of each criterion:
  `verified`, `failed`, `missing`, `stale`, `inconclusive`, or
  `skipped_with_reason`. Failed and stale criteria are listed first.
- **Reviews.** `review gate:` counts recorded `review-outcome` rows
  (exit 0 = pass; missing exit = unknown). These are historical counts over
  the listed window, not proof of a review for the current tree. If none were recorded, it says
  `none recorded in window (not a pass)`. Seat answers (`kind=review`) are
  counted separately, because a seat answering is not a verdict.
- **Degraded collection.** A source that fails or returns an unsupported
  contract adds a `COLLECTION FAILED <source>: ...` line, and its section
  reads `UNAVAILABLE`. Only the collector exit code is displayed: stderr may
  quote private record contents. A source larger than 4 MiB is unavailable
  rather than silently truncated into a healthy-looking snapshot.

Every label is treated as untrusted. Control characters, including terminal
escapes, BEL, and C1 codes, are replaced with `?`, and so are format characters
such as bidi overrides and zero-width characters. Labels are truncated, each
list shows at most 8 rows plus an omitted count, and each line fits the
terminal width. The view never includes artifact task goals, approval
summaries, patch or artifact paths, prompts, or transcripts. Panel-managed
attempts can include a bounded descriptive task title and relative execution
location; [the terminal panel](TERMINAL-PANEL.md) records explicit role and main
relationships. Generic attempt parent IDs are not assumed to mean supervision.

## Sample

Illustrative output from the operator test fixture:

```text
OMS dashboard  repo=demo  main@b6c3a2aa8c39  snapshot=2026-10-04T00:51:30Z
coverage: OMS-managed records only; native provider subagents are not observed
goal: operator fixture (plan)
scope: unbounded (none recorded)
task: none active
plan: 2 task(s)  ready=1 review=1  actionable=ready_task
attempts (OMS lifecycle): 1 total, 0 active  cancelled=1
  att_one cancelled codex/agent-supervisor task=task_one tokens=12 cost_microusd=7 2026-08-01T00:00:03Z
delegation: live codex started=2026-08-01T00:00:00Z live
operations (artifact index, newest first):
  codex delegate model=unknown route=unrecorded exit=0 success served=unreported tokens=unknown at=2026-08-01T00:00:10Z
  claude review model=unknown route=unrecorded exit=1 unresolved served=unreported tokens=unknown at=2026-08-01T00:00:07Z
  codex call model=gpt-test route=explicit fallback=other exit=0 success served=gpt-served tokens=120 at=2026-08-01T00:00:05Z
review gate: none recorded in window (not a pass)  seat answers=1
acceptance: 0/2 verified  missing=2  (worker completion is not acceptance)
  missing plan-task-ready_task [plan-task] Plan task ready_task is admitted.
  missing plan-task-review_task [plan-task] Plan task review_task is admitted.
attention: 2 item(s)
  P1 unresolved-artifacts: 1 unresolved artifact outcome(s)
     next: oms artifact-index --repo . unresolved; oms artifact-index --repo . resolve-recovered --dry-run
  P3 runtime-evidence-missing: 2 acceptance criterion/criteria lack current evidence
     next: oms runtime evidence show
next decision: initialize_project (repo_write): oms init
```

## JSON schema 1

`--json` prints one object. Its fields are additive, and removing or retyping
one requires a schema bump.

| Field | Type | Meaning |
| --- | --- | --- |
| `schema`, `kind` | `1`, `"oms-dashboard"` | contract identity |
| `generated_at` | UTC timestamp | snapshot time |
| `repo` | `{name, branch, head}` | directory name, branch, 12-character HEAD, or `null` |
| `coverage` | string | the OMS-managed-records-only statement |
| `collection` | `{ok, sources: {state, artifacts, attempts}}` | each source is `"ok"` or a sanitized error |
| `goal` | `{text, source}` or `null` | recorded objective (`task`, `plan`, or `project`) |
| `scope` | `{allowed[], forbidden[], source}` or `null` | `source` is `unbounded` when nothing is recorded |
| `task` | object or `null` | `present, healthy, task_id, status, verification, stale, next` |
| `plan` | object or `null` | `present, healthy, task_count, by_state, actionable[], stale_claims, stale_reviews, contract_blocker` |
| `attempts` | object | `available, total, active, by_state, recent[], active_recent[], note`, plus `error` when unavailable |
| `attempts.recent[]` | object | `attempt_id, state, provider, tool, task_id, reason_code, updated_at, tokens, cost_microusd` |
| `attempts.*[]` panel metadata | object, when explicitly recorded | Additive `parent_attempt_id` and `panel {role, owner, purpose, workload, model, effort, location, access, label}`. Locations are `repository` or a relative `worktree:` token; models are requested selections. |
| `attempts.active_recent[]` | object | Up to eight current attempts, newest update first: `attempt_id, state, provider, tool, task_id, reason_code, updated_at`. Independent of the recent outcome window. |
| `delegations` | array or `null` | Up to five markers, live first: `id, provider, role, task_id, requested_model, reasoning_effort, live, started_at, location`. Location is a relative worker worktree token. Requested model is marker configuration, not proof of the served model. |
| `operations` | object | `available, recent[], omitted`, plus `error` when unavailable |
| `operations.recent[]` | object | `event_id, ts, kind, provider, task_id, attempt_id, exit, status, verify_exit, route_class, requested_model, selected_model, served_model, model_attribution, fallback_used, fallback_reason, tokens, provider_usage, cost_usd`; `provider_usage` is `null` or `{input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cache_in_input}`, counts only |
| `reviews` | object or `null` | `outcomes, passed, failed, unknown, seat_answers` over the listed window |
| `acceptance` | object or `null` | `available, total, counts, complete, criteria[] {id, status, source, text}, omitted, note` |
| `attention` | object or `null` | `actionable, items[] {priority, code, summary, command}, omitted, next[] {id, authority, command}` |
| `observation` | object | `{schema, kind, atomic, revision, observedAt, completeness, sources, evidence}`: bounded multi-source, non-atomic (`atomic: false`) observation revision (`obs-v1:...` or `null` when unknown), observation time, completeness (`complete`, `partial`, `unknown`), per-source status, and repository-wide evidence references. The revision compares the bounded public view in the same physical checkout and conversation binding. It excludes read time. `observedAt` marks this read, not an atomic cut across stores. A partial source can retain valid rows, and a stale Plan denotes recorded stale claims or reviews. Omitted and trimmed counts describe rows in bounded windows that may overlap. The latest repository artifact does not establish a link to a particular Room or run. |

When the state source is unavailable, every section derived from state is
`null`. A `null` means "not known", never "none".

## OMS in Codex and ChatGPT Work

In a Codex project with OMS installed, ask **“@oh-my-setting 이 프로젝트 패널 상태 보여줘. Main과 Lead가 어떤 작업을 하고 있어?”**.
The `oh-my-setting` plugin supplies Codex hooks, including optional skill
hints. The project host’s separately configured OMS MCP server (Core or Full)
supplies the read-only `oms_panel_snapshot` tool. The assistant passes the
current project checkout in `repo` and reports the snapshot in the conversation.
For a bundled MCP connection, select the standalone **OMS** marketplace entry
below.

Both connections use the repository projection from
`oms dashboard --json --panel-snapshot`. It identifies the repository, branch
and collection time, the recorded Main and bound Room when available, active
and recent attempts, current task state, recent operation outcomes, review and
acceptance counts, and inbox attention. Only attribute a row to a Main or Lead
when records link them. Provider-native workers without a record have unknown
status. Ask again in the same chat to refresh.

For **this conversation’s Main**, Codex supplies the current thread ID on each
MCP tool call. OMS checks whether that thread is enrolled as the Codex owner
of an open Room in the selected repository. A matching Room identifies the
Main for this chat. Opening or refreshing the panel only reads existing
membership. To continue an enrolled Main, give it work in the current chat.
The overview groups Main, Lead and worker attempts for the enrolled Room.
A worker row is nested when the recorded parent attempt or Room participant
connects it to that Lead. Partial collection is shown as partial. **Main chat**
opens the original Codex conversation when the host advertises link support
and the Room confirms the current user conversation as the owner. Use that
conversation's normal composer to send messages and see replies. Otherwise
the panel can show the permitted Room history.

The Main can use a Lead from the same provider. An explicitly requested new managed
Main gets a separate Room and launch receipt. Project-wide attempt and acceptance counts retain
their repository scope. If OMS cannot establish a single match, the panel
leaves this conversation’s Main unconfirmed.

### How the panel reaches the runtime

| Surface | Data and control path |
| --- | --- |
| Codex project chat | The assistant calls the OMS MCP tool. Rust collects project state, attempts and artifact outcomes and returns a bounded snapshot for the reply. |
| ChatGPT Work content tab | The host renders the plugin’s HTML and forwards its tool requests over MCP. Refresh reads the same Rust dashboard. Start and approval use separate project-scoped tools. |
| Terminal panel | The Rust TUI calls native OMS panel services directly and can attach to managed tmux sessions. It draws the Board from project and provider records. |

When a user submits a goal through **@OMS**, the selected Main coordinates the
work on the project host. An authorized provider Lead can delegate to its
native workers. Their recorded attempts, task updates, results and evidence
supply later status queries. An approval in the Work tab answers the selected
Room question. [Terminal panel](TERMINAL-PANEL.md) ·
[Main and Lead responsibilities](TEAM-ORCHESTRATION-MAIN-POLICY.md).

### Select a Main and give it a goal

In the desktop app, mention **@OMS** in a project chat. For example, ask
“Use Claude as Main for the docs project and have its leads review the API
examples.” In the same conversation, ask “What are the leads working on?” or
“What needs my approval?”. When OMS has multiple connected projects, select
the intended project by its alias. Main and Lead execution, state and provider
authentication live on that project's OMS host.

In ChatGPT Work, open **OMS** from the conversation's content-tab picker for
the Main selector, goal editor, project progress and pending decisions. In
Codex, use the same plugin tools directly in the conversation. Acknowledging a
goal means the Main received a launch request. Work in progress and outcomes
come from runtime records. Approve or deny a specific pending question after
reviewing it. A stale review requires a fresh decision. The approval is an
answer to that question for its Main to consume.

If the start response is lost, ask OMS to check that request in the same
chat. `oms_start_status` looks up the original project and request UUID.
`not_found` allows a retry with that UUID. `reserved` identifies the Room
while launch remains unconfirmed. `recorded` returns the matching attempt
and its state.

This 570-pixel Chromium capture uses a local example project with three recorded plan tasks, two claimed owners and one prerequisite. The panel read the project through the generic MCP Apps profile.

![OMS Now view showing the project goal, claimed tasks, result availability and project map](assets/oms-chat-panel.png)

In a Codex conversation with a verified Room owner, expand **읽기 전용 검토 요청**
to ask one Claude or AGY researcher a question. Submitting the form invokes the
selected provider once and can use its allowance. **요청 상태 확인** checks the
same request ID. If the operation was found but its Room receipt is missing,
**작업방에 실행 기록 연결** links the existing operation. It is also available
in the researcher’s conversation. Linking rechecks Room ownership and never
starts another provider. **검토 대화 보기** opens the Room handoff and the provider's
recorded answer. A pending request or missing answer remains visible as such.
The research request grants no write scope, Lead role or approval.
To share an answer, choose **Attach review result** (검토 결과 첨부), review the
displayed text, then send it to the current conversation or copy it. Canceling
the preview leaves the answer private. When a host lacks message support or
declines the request, the preview still offers copying.

Use the header’s **한국어 / English** selector to change panel labels immediately.
The panel preserves drafts and the selected project. It prefers a saved choice,
then a supported host or browser language, and defaults to Korean. Recorded
answers and evidence retain their original text.

### Tasks and results

**Now** shows the recorded goal and verification. **Work** shows tasks and their recorded owners. **Results** shows outcomes and verification evidence. **More** opens Relationships, the project map, chat, activity, metrics and Git. From Work, Results or Relationships, select a task to open its detail by ID. Prerequisites appear separately from ownership, including cycles or references outside the retrieved page.

The read-only `oms_panel_work_items` tool projects the selected project’s plan and bound results. The detail version identifies a work record and its evidence. If that version changes during refresh, reopen the current version to review it. The panel distinguishes completed work, accepted results, failed or unrun verification, and missing evidence. The history link opens project-wide activity.

### Project map and command directory

The project map loads from the selected checkout as soon as the host confirms the project. Open **Explore** to follow nodes, inspect the source and bounds, or search the repository. Git containment describes folders and files. Indexed links distinguish extracted, inferred and ambiguous relationships. Reading the map leaves the index unchanged.

**Find tools** searches the host's public OMS commands by name or purpose. Its shortcuts open existing panel views. Choosing a command prepares a help question. Hosts that advertise the OpenAI message extension can insert that question or a Git goal into the active composer for editing. Other hosts can copy the draft. **Attach to chat** sends only the explicitly selected node ID, kind and selected project alias to a host that supports text model context. Switching projects clears the preceding selection. [OpenAI plugin extensions](https://developers.openai.com/ko-KR/plugins/build/extensions) · [Message and model-context protocol](https://github.com/openai/mcp-extensions/blob/main/docs/spec.md#prompt-target-and-send-behavior)

### Connection details

Expand **연결 정보** to inspect the current host handshake, display mode, OMS tool profile, and last project check. Opening it reads the runtime host's PATH and Main registry. Selecting Codex, Claude, AGY, or Pi previews that harness. **이 하네스로 새 목표 작성** selects it in the new Main form; submitting the form is a separate action. Pi uses the CodexGateway. AGY and Pi resume are unsupported by the current registry. See the [Main launch registry](../crates/oms-command-panel/src/native_panel.rs).

Text clients can request the same pages with `oms_panel_tui_view`, a registered project, `view`, and optional `page`. Follow `nextPage` for additional rows. The `connections` view checks local executable presence. `modelcatalog` reads the retained [model catalog](../crates/oms-command-providers-skills/src/models.rs), including per-model efforts and unrouted entries. Fixed-effort models show `fixed` while unknown effort scales remain `null`. `routes` previews configured dispatch, model, effort and selected account aliases. `attribution` reads project [call receipts](../crates/oms-command-telemetry/src/store/producer.rs) and identifies a served model when a transport reported it. These views do not authenticate to provider accounts, refresh remote model catalogs, probe versions, or change the current Main. Unknown authentication, transport, account, served effort and host placement remain unconfirmed. The existing [account verifier](../crates/oms-command-providers-skills/src/connect.rs) owns provider login checks. Use the provider’s [Codex authentication](https://developers.openai.com/codex/auth/) or [Claude authentication](https://code.claude.com/docs/en/authentication) flow when it requests a login.

### Claude and Antigravity

The cross-host connection serves the same OMS panel and project-scoped read-only tools.
Choose a registered project to inspect its graph, progress, Git and GitHub work, usage,
a Board view, or the execution host’s command catalog. `oms_panel_capabilities`
lists the names and descriptions from `oms list --all`. When it returns
`nextOffset`, pass that value as `offset` on the same project to continue. A host that supports sending a user message can carry a
selected item reference into the conversation. Verify assignment and execution
in that conversation. Private Codex Room history, managed starts and approval
decisions require the Codex/Work connection.

| Surface | How to connect and open OMS |
| --- | --- |
| Antigravity App 2.34.0 or newer | Install the [OMS panel plugin directory](../plugins/oms-app) globally for workspace-wide availability. Once enabled under **Customizations**, choose **OMS** in the side-panel **+ → Plugins** menu or open the full-page view from **Plugins**. [Antigravity plugins](https://antigravity.google/docs/plugins/) · [MCP Apps](https://antigravity.google/docs/mcp-apps) |
| Claude Desktop | Add the `oms-panel` entry from the [stdio configuration](../plugins/oms-app/mcp_config.json) under `mcpServers` in Claude Desktop’s local developer configuration. Restart Claude Desktop and ask “Open OMS”. Claude selects an inline or expanded view, or a side panel for eligible organizations and conversations. [Local testing](https://github.com/modelcontextprotocol/ext-apps/blob/main/docs/testing-mcp-apps.md) · [Claude display modes](https://claude.com/docs/connectors/building/mcp-apps/display-modes) |
| Antigravity CLI/IDE and Claude Code text sessions | Use the same MCP tools to request the project list and status in chat. Claude Code accepts the stdio file through `--mcp-config`. [Antigravity MCP](https://antigravity.google/docs/ide-mcp) · [Claude Code MCP](https://code.claude.com/docs/en/mcp) |
| Claude on the web | Custom connectors need a reachable HTTPS MCP service. The checked-in connection uses stdio over SSH. The global Artifacts launcher additionally depends on connector and organization eligibility. [Remote connectors](https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp) · [Claude entry points](https://claude.com/docs/connectors/building/mcp-apps/entry-points) |

The plugin uses the desktop’s existing OpenSSH connection to the `oms-runtime`
alias. The OMS version on that host must support `OMS_MCP_TOOL_PROFILE=mcp-app`.
Register the projects once on that host as described below. On a machine that
already runs OMS, the local stdio entry can instead run the absolute installed
`oms` executable with `args: ["mcp-server"]` and
`env: {"OMS_MCP_TOOL_PROFILE":"mcp-app"}`. The generic profile reads the
configured alias catalog or an operator-pinned `OMS_PANEL_REPO`.
The SSH principal that starts the stdio process and its project catalog define
the scope. Each call re-reads the catalog and verifies the canonical checkout.
When a trusted operator maps an alias to a different valid canonical checkout,
the next call uses that checkout. Removing the alias, substituting a symlink,
or loosening the catalog permissions causes the next call to fail, including
after the client reconnects.
Client `_meta`, Room and participant names, and raw paths are ignored or
rejected; mutating tools are refused by name. This does not add public-transport
authentication, and `mcp-app` exposes no Room view.

In **Git → GitHub Projects**, open a repository-linked board and select
**보드 상세 보기**. Each request rechecks the board node against the repository's
linked catalog (up to six boards) and returns up to 20 cards. Use the next
page for more cards. Fields come from the unique single-select field ID and
a current option ID on that board. Missing, ambiguous, deleted or partial
metadata leaves the affected value unknown. The recorded time applies to that
page; cards can move while paging.

The panel identifies same-repository open issues for review and marks observed
open blockers, epics, PRs, drafts, archived or foreign cards, and existing
Plan/task links separately. Status and priority describe the board. A Main
must check current Issue requirements, scope, dependencies and the existing
lease before dispatch. Opening the board or paging only reads it. With missing
Projects access, exact Issue lookup and the repository inventories still work.
The regression fixtures exercise readable boards and pagination. The live
read against OMS Development Project #3 on 2026-11-24 returned
`INSUFFICIENT_SCOPES` for `read:project`. The same account could read Issue #447.

For “handle all open issues and PRs”, a Main pages both inventories in the
selected repository through `oms_panel_git_status`. Each page returns its
continuation cursor. Available Project boards add context; the initial Git
tab only shows samples. The Main reconciles work already in progress, assigns
Leads, verifies outcomes and reports any blocked items. Selecting an issue in
the panel hands its reference to the conversation after user confirmation.

### If the tab fails before the OMS heading appears

A tab listed in the picker can still lack an MCP connection in the open
conversation. With the legacy **OMS Panel** preview, a running Codex executor
listed the installed plugin and could read its HTML globally, while the open
chat returned `unknown MCP server 'oms-panel'` for both the resource and
`oms_open_panel`. Reloading the executor’s MCP configuration left that chat
in the same state.

To recover the **same conversation**, the executor operator re-registered
the already-enabled, identical local package using `plugin/install` on the
running app-server with its registered `marketplacePath` and `pluginName`.
The existing thread could immediately read the HTML, call `oms_open_panel`
and retrieve its project snapshot. The source, cached package, marketplace and
user configuration kept their original contents. Reopen the tab to retry the
view. A new chat in the same project after registration also worked in an
isolated app-server test.

To diagnose a specific thread, compare its named `mcpServerStatus/list` and
`mcpServer/resource/read` results with the host-wide results. Use the server
name and environment advertised by that plugin. Repeat the opener and HTML
read on the same connection while a status query is running. Earlier servers
queued both behind a slow project snapshot. The panel server now serves
those requests independently. Its widget also acknowledges host `ping` and
`ui/resource-teardown` requests, including when the host and widget happen
to use the same request ID.

If clicking **Retry** produces only a desktop inventory request, the executor
has no opener or resource-read result for that click. The desktop renderer’s
diagnostic log is needed to locate that failure. A rendered panel showing
degraded data has reached OMS and describes collection on the project host.

### Register a marketplace once

Choose one transport for the app's **execution host**. Both expose the same
`@OMS` tools. The existing `oh-my-setting` entry supplies separate lifecycle
hooks.

| Where the app can execute plugin tools | Marketplace path in this repository | Entry |
|---|---|---|
| On a supported OMS host, including a Codex project running on that host | Repository root | `oms@oh-my-setting-local` |
| On a PC using OpenSSH to reach the OMS project host, including Windows Work | `plugins/oms-ssh` | `oms@oms-ssh` |

For a managed workspace, an admin can open **Admin → Plugins → Add → Import
marketplace** and use `https://github.com/eightmm/oh-my-setting` as the Source.
Leave **Path** empty for the root catalog or enter `plugins/oms-ssh` for the SSH
catalog. Select a branch or commit when testing a PR. Once the catalog is
available on the execution host, select **OMS** in the app's Plugins directory.
Its setup conversation checks the connection and offers the available projects.
For a direct connection, it can register the active checkout with the owner's
permission. Start a new project chat or continue the installation chat. [Workspace import](https://learn.chatgpt.com/docs/enterprise/plugin-management)
and [personal and repo marketplaces](https://developers.openai.com/plugins/build/plugins).

### Connect the project host

The project host needs an OMS build containing the desktop tools, tmux, and
an available provider for the selected Main and leads. Use the same OS account
that owns the projects and provider sessions.

- **Direct execution.** The plugin starts `oms mcp-server` from PATH. A
  project-scoped executor can supply the project checkout as its working
  directory. Codex 0.172.0 starts bundled servers in their plugin cache. For
  that case, register the alias map below once on the project host. The
  direct server can also receive `OMS_PANEL_PROJECTS` or one pinned checkout
  in `OMS_PANEL_REPO`. A shell-capable project conversation can invoke the
  same typed operations from its active checkout when the direct
  server lacks a project binding. The panel itself uses its MCP connection.
- **Windows or another remote desktop.** The SSH package uses the desktop's
  OpenSSH client. Configure the SSH alias `oms-runtime` once for the existing
  OMS account and host, with its verified host key and a key or agent that
  already authenticates noninteractively. An existing OpenSSH entry with
  an explicit `HostName` can include `oms-runtime` as a second name on its
  `Host` line. The plugin opens a binary-clean stdio connection to that alias
  and runs the server's `~/.local/bin/oms`. The desktop holds no OMS runtime
  or project checkout. [SSH package](../plugins/oms-ssh/oms/mcp.json).

The default project map lives **on the OMS host** under its absolute
`XDG_CONFIG_HOME`, or under `~/.config` when unset or empty, at
`oh-my-setting/desktop-projects.json`. The packaged Codex SSH connection
requires this file. Keep it private to its owner (mode `0600`). Use absolute
physical paths to Git checkouts or adopted OMS projects that this desktop
connection may manage. For example, this map offers two choices in the same
plugin.

```json
{
  "docs": "/srv/projects/docs",
  "engine": "/srv/projects/engine"
}
```

`oms_list_projects` reports the binding and the connection profile’s `readOnly`
flag. A `catalog` offers registered aliases. A `pinned` connection or an
authorized `cwd` already has a project and returns an empty alias list.
`unbound` means the host has no project for this connection. An invalid map
returns an error. A Codex control connection also requires the appropriate
Room binding for actions on a conversation.

Keep access to that map and SSH account restricted to the intended operators.
Refresh OMS after changing the map to see the current choices. Each call on a
mapped connection identifies a project by alias. The plugin accepts the
project paths from the host configuration. SSH uses strict host key
verification and disables forwarding and interactive authentication.
[Plugin thread entrypoints](https://developers.openai.com/plugins/build/extensions).

### Git and GitHub in the desktop panel

The dashboard reads the selected checkout's branch, full HEAD, tracked and
untracked changes, upstream, and local ahead/behind counts in one Git status
call. Counts compare against the locally stored upstream ref. Detached HEAD,
unborn branches and missing upstreams retain the values Git can establish.

The GitHub action invokes `oms_panel_git_status` (CLI equivalent
`oms github panel-status --repo PATH --json`). It reads open pull requests
from the checkout's github.com origin using the runtime host's GitHub CLI.
For the selected branch it shows whether the local HEAD matches the PR.
It also recognizes PRs whose origin branch **and** head match a registered
linked worktree. A fork with a coincident branch name does not qualify.
The title, check counts and up to six named checks belong to the observed PR head. Failed and pending checks appear first. Zero checks means none
were reported. The response marks truncated or partial results, unsupported
origins, and unavailable GitHub access. Opening or refreshing the dashboard
uses local Git alone. This action does not fetch refs or change the checkout.

In the panel, **Usage** shows the provider and attempt counters that OMS
actually recorded. **Chat** reads messages shared with the enrolled Main and
the selected participant. Inspect its linked result or evidence when available.
The TUI views read the native Board, plan, teams, routing policy, metrics,
results, and workspaces on demand. Sessions, Attention, Past, Council and
Activity require this conversation's enrolled Main. Council shows the number
of recorded answers and their model names when a validated linked thread
exists. Activity shows recent Room message metadata addressed to the Main.
Capacity shows the recorded resource policy and its occupancy when configured.

## Claude Code pane

In Claude Code, select **oms-panel** from this repository’s **oh-my-setting** marketplace.
User scope makes the command available across projects. In the current directory,
`/oms-panel` opens the native **OMS** pane. **Now** shows the recorded goal, verification criteria, project task and
open attempts. **Work** shows the plan and its recorded workers, **Results**
shows acceptance criteria and reviews, and **Activity** holds recent operations
and diagnostics. Keys 1–4 switch views, R refreshes and Esc closes the pane.
The mod reads the schema-1 `oms dashboard --json` projection and keeps
suggested actions as text. See the [Claude Code plugin guide](../plugins/claude-code/README.md)
for loading and validation.
