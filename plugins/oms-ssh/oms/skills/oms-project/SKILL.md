---
name: oms-project
description: Use OMS from chat to give a project goal to a chosen Overall Main, check work by its leads, or decide a pending OMS approval.
---

Use the OMS plugin tools in the current conversation. Call `oms_list_projects`.
When it returns aliases, use the one the user selected or named. If their
project is ambiguous, show the choices. Pass that alias as `project` on each
call in this conversation, including when opening the panel. With a `pinned`
or `cwd` binding, omit the alias.
Check the selected project with `oms_panel_snapshot` before submitting work.
An ambiguous project, connection error or rejected binding needs attention
before any action.

- When the snapshot identifies this conversation’s Main, carry out the user’s
  goal in its existing Room and selected checkout. Inspect enrolled leads and
  their live assignments. A Codex Main can assign a Codex Lead. Reuse the
  applicable lead or dispatch through the existing Room’s authorized lead
  workflow. Verify the assignment and its receipt. Choosing a lead or sending
  an issue/PR reference into chat does not by itself start a managed Main or
  establish execution. Report the handoff as pending until acknowledged.
- For a request to handle **all open issues and PRs**, page both
  `inventory: "issues"` and `inventory: "pullRequests"` on
  `oms_panel_git_status` for the selected project. Omit `cursor` for the
  first page, then pass each `github.inventory.nextCursor` until exhausted.
  Follow up with the host's authorized repository-scoped tooling as needed. Track both kinds by repository, type and number. Consult
  accessible Project fields, linked work, assignees, checks and existing
  branches. The panel shows bounded samples and visible Projects metadata.
  Reconcile those samples against the complete inventory; mark missing scopes
  and inaccessible Projects explicitly. Group related work under the current
  Main, preserve user changes and active assignments, and continue through
  implementation and verification. Recheck remaining open items and report
  per-item results, links and concrete external blockers. Merge or close only
  with the required authority.
- For an explicitly requested new managed Main, use the user's chosen Main (`codex`, `claude`, `agy`, or `pi`).
  If they left the choice open, offer those choices. Pass the goal and its
  constraints as a single line to `oms_start_main` with a fresh lowercase UUID
  as `requestId`. Report the receipt's Room and attempt. If a start response is
  missing or inconclusive, call `oms_start_status` with the original project
  and `requestId`. For `recorded`, report the matching Room, attempt and state.
  For `reserved`, continue tracking that Room. Only `not_found` permits a
  retry of the same submission, using its original UUID.
- For progress, use `oms_panel_snapshot` and describe recorded work, including
  any unavailable status. A launch receipt alone does not establish that a
  lead completed its assignment.
- For branch, GitHub items and checks, call `oms_panel_git_status` on demand.
  For a chosen item, send its paired `kind` (`issue` or `pull`) and positive
  `number` on the same project. Review the returned item before dispatch. For
  provider and attempt usage use `oms_panel_usage`. For a native Board
  section, call `oms_panel_tui_view` with its view and page. For messages
  shared with an enrolled Main, open the panel and select the participant
  in Chat. The widget calls its private `oms_panel_chat` tool.
  Report unavailable, unreported and partial data as returned.
- For approvals, call `oms_list_approvals`. Show the specific operation for
  review. After the user explicitly approves or denies that item, pass its
  exact ID, version and digest to `oms_decide_approval`. Refresh stale items
  and ask for a new decision if the operation changed.
- When a user asks to see the panel, pass the selected alias to
  `oms_open_panel` so the tab opens on the project from this conversation.
  Use its tools directly in chat when a tab is unavailable.

Only when the **direct** plugin returns a successful `oms_list_projects`
with `binding: "unbound"` may a shell-capable project chat run the same
typed requests in its own trusted active checkout.
Run `oms mcp-server` there with `OMS_MCP_TOOL_PROFILE=chatgpt-panel`.
Send JSON-encoded, newline-delimited MCP initialization and
`tools/call` requests over stdin, and read each response by ID. Retain the
same project checks, explicit user decisions, tool permissions and retry ID.
An SSH connection failure, denied permission, or different or unavailable
project in a configured map must be resolved on that connection.
