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
task handle as plan, executor, approval, or patch authority.

## Codex app-server read transport

Set `OMS_CODEX_TRANSPORT=app-server` only for an explicitly selected read seat.
The adapter starts one ephemeral thread and one turn with:

- repository cwd;
- `sandbox=read-only` / `sandboxPolicy.type=readOnly`;
- network disabled;
- `approvalPolicy=never`.

It accepts bounded agent-message deltas and successful completion only. Any
server request for approval, permissions, or user input fails closed. A failed
app-server request is never resent through `codex exec`. Write delegation must
use the ordinary CLI transport.

## A2A v1 read bridge

`oms agent-card --url http://127.0.0.1:PORT` prints the public card. Running
`oms a2a-bridge --repo . --host 127.0.0.1 --port PORT` is the only way to start
the bridge; installation and update never do so. The bind host must be a
loopback IP literal.

The HTTP+JSON interface provides:

- `GET /.well-known/agent-card.json`;
- `POST /message:send` with exactly one text part: `status`, `inbox`, or
  `capabilities`.

Responses are synchronous A2A Messages containing JSON text. Streaming, push,
authentication, remote bind, mutation prompts, provider calls, continuation,
and A2A Tasks are unsupported. Keep it behind the host's local trust boundary;
localhost-only is not an account-isolation mechanism.
