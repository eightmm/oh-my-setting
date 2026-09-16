# Interoperability Surfaces

These adapters expose existing OMS reads and peer operations. They do not add a
new task, approval, landing, provider-write, or publication authority.

Live collaboration reuses the existing peer tools: `oms_peer_start` with
`kind=message|ack` records only a thread turn; `oms_peer_result` with `thread`
and optional `after` returns a bounded incremental page without starting a
provider. Ordinary operation-id polling is unchanged. See state-memory.md for
delivery versus acknowledgment and hook boundaries. Core discovery stays at
12 tools; no subscription or push capability is advertised.

## MCP protocol revisions

The stdio server is dual-era. A legacy client opens with `initialize` and is
served on the revision it negotiated. A modern client (revision `2026-07-28`)
sends no handshake: it names its revision in every request's
`_meta["io.modelcontextprotocol/protocolVersion"]`, may probe
`server/discover` first, and receives `resultType` plus `ttlMs`/`cacheScope`
on list results. The per-request field wins wherever it is present; a named
revision the server does not implement is refused with error `-32022` and the
supported list, never served on the fallback. Requests without the field
follow the session, so existing clients see the same bytes as before.

## MCP Tasks extension

The default MCP wire stays unchanged. Enable the server with
`OMS_MCP_TASKS_EXTENSION=1`; the client must negotiate protocol `2026-07-28`
and declare `io.modelcontextprotocol/tasks` in that individual request's
client-capability metadata. Only a same-repository `oms_peer_start` may become
a Task. Its `taskId` is the existing durable peer operation ID.

- `tasks/get` polls that operation.
- `tasks/cancel` records cooperative cancellation intent.
- `tasks/update` refuses because OMS peer operations never enter
  `input_required`.
- There is no task list and no second task store. Use `oms_peer_operations` for
  the existing bounded operation inventory.

Clients without all opt-ins receive the legacy CallToolResult. Do not treat a
task handle as plan, approval, or patch authority.

## Provider transport

Use the existing peer tools over the CLI transport. The optional OMS app-server
read adapter was retired. A stale `OMS_CODEX_TRANSPORT=app-server` setting fails
explicitly; remove it only when CLI execution is intended. Native model
discovery, MCP graph views, and thread messaging are unaffected.
