# Interoperability Surfaces

These adapters expose existing OMS reads and peer operations. They do not add a
new task, approval, landing, provider-write, or publication authority.

Live collaboration reuses the existing peer tools: `oms_peer_start` with
`kind=message|ack` records only a thread turn; `oms_peer_result` with `thread`
and optional `after` returns a bounded incremental page without starting a
provider. Operation reads default to immediate; optional `wait_seconds` (0-50)
waits inside the server for done/stalled, not log activity. Keep it below the
host timeout; this serial stdio server serves its next request after the wait.
Expiry returns running without cancelling, resubmitting, or starting a model.
See state-memory.md for
delivery versus acknowledgment and hook boundaries. Core discovery stays at
12 tools; no subscription or push capability is advertised.

`oms_peer_start` accepts optional booleans `include_memory` and `include_task`
for `consult`, `advise`, and `ask`. Omitted values preserve CLI defaults:
memory off for all three, task on for consult and off for advise/ask. Explicit
false omits that context. Memory is selected by the existing CLI and query,
bounded and scrubbed as reference data; this option never writes memory or
enables per-model-turn extraction. Use memory when prior lessons are relevant;
leave it off for an independent opinion. The start response, operation metadata,
and operation listing expose supplied values as `context_options`, recording
the request rather than proving that context was available. These options are
rejected for message/ack. Worker delegation restrictions still apply.
When memory is requested, MCP calls default to `relevant` mode (pinned facts
and query-ranked recall, without unrelated recent summaries). An explicit
`OMS_AGENT_MEMORY_MODE` override wins. For a resumed consultation, include the
current question and constraints; no separate automatic memory extraction runs.

Large operation answers retain UTF-8 head/tail previews within a 60,000-byte
answer-body budget, including seat labels and separators (not the JSON envelope).
`answer_truncated` and `answer_details` disclose missing evidence. Each detail
identifies the artifact and exit, normalized `answer_bytes` and `answer_sha256`,
and `preview_bytes` including the marker but excluding the seat label.
If a label cannot fit its share, `inline_omitted` is explicit and metadata keeps
the seat identity and exit. Hidden answer changes invalidate the result cursor;
the digest establishes neither artifact-file integrity nor authority. Read the
original artifacts for omitted evidence, or use `read_arguments` when present.

New MCP starts write peer artifacts under the returned operation's `answers/`
directory; direct CLI defaults and legacy operation reads are unchanged.
`answer_details[].read_arguments` selects one completed, normalized answer by
operation, filename and content digest. Pass it to `oms_peer_result`, optionally
with `answer_offset` (UTF-8 bytes, default 0) and `answer_limit` (4–16384 bytes,
default 16384). Follow `next_read_arguments` until `has_more=false` to reconstruct
the answer, or choose a byte boundary to inspect an omitted region. JSON framing
is outside this byte budget. This reads the answer section, not the quoted prompt.
`after` is a change cursor, not a page position, and cannot accompany `answer_ref`.
Neither can thread reads or positive completion waits. Failed seat exits remain
visible and mark the page as an error even if the overall operation exited zero.

Ranges refuse changed references, unfinished/legacy operations, invalid offsets,
missing artifacts, symlinks/reparse points, hard links, and files over 8 MiB.
The file cap includes the recorded prompt; oversized owned artifacts also lose
their preview and report an error. Each page rereads and hashes the bounded
artifact to check its version, so exhaustive paging is costlier than a direct
local file read. Prefer ranges for selected evidence.
CRLF/LF-only changes preserve the normalized answer reference. The reader checks
the opened file and directory identities, parses a bounded in-memory snapshot,
and never invokes a model or writes a cache. Local owner-writable state remains
a trust boundary, not a sandbox against a hostile same-user filesystem writer.
Old operations retain their preview/file-access route; no migration or automatic
model rerun is needed.

Add `answer_query` to the returned `read_arguments` for case-sensitive literal
search (1–256 UTF-8 bytes, no regex), optionally starting at `answer_offset`.
Do not combine it with `answer_limit`. Up to eight non-overlapping matches carry
byte offsets, bounded surrounding context and their own `read_arguments`.
Follow `next_search_arguments` when `has_more` is true. No matches is a successful
empty result; missing/changed evidence remains an error. Use search plus a range
for selected evidence rather than paging through the entire answer.

`oms_peer_start(kind=ask, debate_rounds=0..3)` returns a ready thread and uses
parallel calls (default: opening answers only). `thread_arguments` reads turns
as they complete. `kind=message` notes enter the next configured round's shared
snapshot; they never start a round or interrupt a model. Councils suggest zero
operation wait to keep the connection available. Reuse cursors at useful task
boundaries; no automatic client wakeup is implied.

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
- `tasks/cancel` requests cancellation for new supervised POSIX operations.
  A live `run-bounded.py` owner stops its own provider process group, escalating
  TERM to KILL after the configured grace period. Subsequent calls see the request
  before spawning. The reader never signals a persisted PID. Until provider cleanup
  and CLI completion, `tasks/get` remains `working` with `cancellation_requested`.
  Only an observed cancellation plus operation exit yields `cancelled`; a late
  request alone never relabels a normal completion. Partial artifacts are retained.
  Legacy/native Windows operations refuse cancellation explicitly without changing
  their execution state. Normal start/read behavior remains available there.
- `tasks/update` refuses because OMS peer operations never enter
  `input_required`.
- There is no task list and no second task store. Use `oms_peer_operations` for
  the existing bounded operation inventory.

Clients without all opt-ins receive the legacy CallToolResult. Do not treat a
task handle as plan, approval, or patch authority.
Operation result/list reads also expose `cancellation_requested` and, once exited,
`termination_reason=cancelled`. This is local process cancellation, not proof that
a remote provider stopped billing or that an intentionally detached external job
was terminated. There is no automatic restart of a cancelled operation.

## Provider transport

Use the existing peer tools over the CLI transport. The optional OMS app-server
read adapter was retired. A stale `OMS_CODEX_TRANSPORT=app-server` setting fails
explicitly; remove it only when CLI execution is intended. Native model
discovery, MCP graph views, and thread messaging are unaffected.
