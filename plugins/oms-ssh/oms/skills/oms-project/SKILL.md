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

- For a goal, use the user's chosen Main (`codex`, `claude`, `agy`, or `pi`).
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
