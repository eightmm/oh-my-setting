# Native MCP component

This crate implements the native persistent stdio server and 33 OMS tools.
At source revision `f9a695ea` (2026-11-18), the root executable already connects
it through `serve_with_services`; lifecycle integration registers the installed
absolute `oms` executable with the `mcp-server` argument. The private
`oms-mcp-peer-owner` is part of the validated native payload. These are source
wiring facts, not a claim that a particular user's installation or provider was
exercised by the documentation audit.

## Current native consumers

| Boundary | Current owner |
|---|---|
| Service entry and configuration | [root dispatch](../oms/src/main.rs), `mcp-server` branch; pins `oms_program` and the sibling peer owner |
| Command execution | [CollabServices](../oms/src/services_collab.rs), `CommandExecutor::execute`; staged native command under supervisor timeout, cancellation and output bounds |
| Provider registration | [lifecycle integrations](../oms-command-lifecycle/src/integrations.rs), `mcp`; preserves foreign configuration ownership |
| Definitions and handlers | [tools.json](src/tools.json), [lib.rs](src/lib.rs), [command.rs](src/command.rs), [peer.rs](src/peer.rs), [extra.rs](src/extra.rs), [output.rs](src/output.rs) |
| Public boundary fixtures | Root `mcp_executor_runs_staged_native_and_honors_timeout_and_cancel`, `mcp_executor_reports_output_limit_distinctly`; this crate's `tests/native.rs` |

The [desktop plugin](../../docs/DASHBOARD.md#oms-in-codex-and-chatgpt-work)
uses the dedicated `chatgpt-panel` profile. It provides the conversation UI,
project selection, Main launch, runtime status and pending approval decisions.
The `mcp-app` profile reuses this UI for Claude and Antigravity and exposes
six project-read tools. The operator binds an alias catalog or pins a project.
Both panel profiles enforce their tool allowlists on direct requests.

Core discovery lists 14 tools and full discovery lists 33. Both profiles accept
the same 33 tool names through `tools/call`, subject to their request and child
restrictions; discovery is not authorization. There is one graph UI resource and
no prompt API. `oms_output_read` is registered in both profiles and accepts the
`read_arguments` returned by output-digest. Its in-process adapter uses the
canonical capture reader, with no command execution or arbitrary path input.

Unqualified reference line numbers and retired Python/Bash paths below describe
frozen migration evidence. The Rust files above own current production behavior.

## Consumer ABI

```rust,ignore
pub fn serve<R: BufRead, W: Write>(
    source_root: &Path,
    caller_cwd: &Path,
    interrupted: &AtomicBool,
    reader: &mut R,
    writer: &mut W,
    config: Config,
) -> io::Result<()>;
```

`Server::new` and `Server::handle` support an embedding caller that already owns
framing. `handle(&impl value::ToValue) -> Option<value::Value>` accepts both
serde values and the native lossless value. Its return type is deliberately the
MCP-local value rather than serde: serde cannot represent lone surrogates or
non-finite ids. `Value` display produces ASCII JSON, including escaped surrogate
code points and the canonical non-finite constants; `value::encode` supplies the
internal pretty/sorted string-domain encodings. Native consumers are `serve`
(for framed requests), `Server::handle` (for embedding and the compiled client),
and the private `peer_owner` entry (for stored launch/receipt values). The root
native front door preserves this return choice when embedding the server.
`serve` retains the ABI above and owns the strict UTF-8 reader, byte bound,
parsing and output framing. `Config` contains a typed `ToolProfile::{Core,Full}`,
a task extension
boolean and a trusted absolute `peer_owner` executable path. `Config::from_env`
reads the existing profile and task feature variables. It introduces no public
OMS verb, flag or MCP schema. The consuming executable must supply its own
trusted physical checkout and captured caller directory; MCP arguments never
select an executable. It must arrange for blocked input reads to wake when its
interruption flag is set. Child-operation waits observe that flag directly.

The bundled `oms-mcp-peer-owner` is **private libexec**, never a PATH command.
Its entire positional ABI is `SOURCE_ROOT REPO OPERATION [OMS_PROGRAM]`. A parent that embeds
this entry instead must call `peer_owner(&[OsString])` from a private executable
entry with that exact argument layout. The owner validates stored typed launch
fields, rechecks delegated-worker restrictions and provider registration, locks
the operation, starts the fixed OMS verb and durably records its exit. It does
not accept a command string, executable selector, shell program or launch mode.

Tool metadata, profiles, version and the resource template are captured once.
Regexes are compiled once per fixed pattern. Each request captures its report;
completed task snapshots reuse the captured peer status and metadata. Action
boundaries reacquire receipts and directory state rather than caching permission
or cancellation grants. No latency, physical-disk or HPC performance claim is made.

## Method matrix

| Method | Implementation and reference |
| --- | --- |
| `server/discover` | Stateless versions/capabilities/server metadata and private 300000 ms TTL; reference lines 2000–2017 |
| `initialize` | Supported session revision or 2025-06-18 fallback; lines 2018–2038 |
| `ping` | Revision-dependent complete discriminator; lines 2039–2040 |
| `tools/list` | Stable full/core definitions, cache hints; lines 2041–2045 |
| `tools/call` | All 32 tools remain callable in both discovery profiles; legacy reference lines 2112–2180, native reader in `src/output.rs` |
| `resources/list`, `resources/read` | Exact versioned graph template, MIME and CSP metadata; lines 2046–2063 and 914–941 |
| `tasks/get` | Opt-in modern capability check, live/terminal snapshot and embedded final result; lines 2064–2111 and 1786–1826 |
| `tasks/update` | Canonical refusal: no outstanding input requests; same handle branch |
| `tasks/cancel` | Capability, task ancestry, terminal/legacy checks, idempotent durable receipt; same handle branch and 1829–1854 |

`tasks/result` and `resources/templates/list` return method-not-found: neither
exists in the reference handler. Task results are in `tasks/get.result`.
2026-07-28 needs no handshake and per-request metadata wins over the session;
2025-06-18 and 2025-03-26 retain their result shapes. Unknown named revisions
return -32022. Discovery and tool annotations grant no model or messaging rights.
Notifications, including explicit null ids, have no effects or responses after
request-type validation. Batches are invalid requests. A record includes its
newline in the 256 KiB bound; oversized records are drained before recovery.

## Tool and fixed subprocess matrix

Every command below runs through the `CommandExecutor` host boundary with a
fixed argv array. `serve`/`Server::new` default to `SubprocessExecutor`, which
spawns only a native `oms`: an absolute, regular, executable file, never
`scripts/oms`, and with no `PYTHON*` variable set by this crate. Resolution
order: `Config::oms_program` when set (authoritative; an invalid pin is an
error, never skipped); the running executable when named `oms`; an `oms` beside
an absolute private peer owner; absolute `PATH` entries. Relative locations and
non-executable files are ignored; nothing found is an error, not a bare `oms`.
Embedding callers can implement `CommandExecutor` with
`serve_with_services` / `Server::with_executor`. The current root host uses a
supervised staged native process; the trait does not imply in-process dispatch.
There are 23 literal `argv` descriptors and nine specialized tool handlers.
Legacy `bash scripts/*.sh` descriptor spellings are converted to native verbs by
`command.rs`; they do not launch Bash scripts. `oms_peer_start` launches the private
`peer_owner` process; it receives `Config::oms_program` as an optional fourth
argument (`SOURCE_ROOT REPO OPERATION [OMS_PROGRAM]`) and resolves the same
way, so the parent must point `Config::peer_owner` at a native bundled helper.
The
canonical definitions/descriptions/annotations and input/output schemas are
embedded in `src/tools.json`; `tests/definitions.json` independently records the
reference discovery output. Both were generated from the canonical Python
module during development, never by a production subprocess or build script.

| Literal tool | Fixed verb/argv or native handler | Reference lines |
| --- | --- | --- |
| `oms_inbox` | `inbox --json` | 238–247 |
| `oms_repo_state` | `state --json` | 248–257 |
| `oms_agent_operations` | `agent-events list --json --limit 40` | 258–272 |
| `oms_approvals` | `approval-inbox list --pending --json` | 273–282 |
| `oms_task_state` | `agent-task status --json` | 283–292 |
| `oms_fail_ledger` | `fail-ledger list --json --limit 30` | 293–304 |
| `oms_handoffs` | `session-handoff list --json --limit 40` | 305–314 |
| `oms_handoff_show` | `session-handoff show FILE`, bounded bare name | 315–329 |
| `oms_journal` | `journal show --json`, optional `--blockers` | 330–346 |
| `oms_runtime_release` | `runtime release status` | 350–361 |
| `oms_runtime_profile` | `runtime profile current` | 362–373 |
| `oms_runtime_failures` | `runtime failure catalog` | 374–385 |
| `oms_peer_start` | Native validation, storage and owner; fixed `consult`, `advise`, `peer-ask`, `thread` or `room` | 387–471 |
| `oms_peer_result` | Native state, liveness, extraction, cursors, answer ranges/search; fixed `thread`/`room` updates | 472–514 |
| `oms_peer_operations` | Native recent-20 read-only projection | 515–526 |
| `oms_project_graph_map` | `graph project map --json` | 532–541 |
| `oms_project_graph_render` | `graph project map --limit 72 --depth 2 --ui-model` or `graph project context --task TEXT --max-files 12 --ui-model` | 542–568 |
| `oms_project_graph_query` | `graph project find --json --limit 40 QUERY` | 569–590 |
| `oms_project_graph_api` | `graph project api --json --limit 120 PATH` | 591–613 |
| `oms_project_graph_search` | `graph project search --json --limit 80 QUERY` | 614–636 |
| `oms_project_graph_symbol` | `graph project symbol --json --limit 40 NAME` | 637–663 |
| `oms_project_graph_trace` | `graph project trace --json --depth 2 --direction out NODE` | 664–689 |
| `oms_project_graph_blast` | `graph project blast --json --limit 120` | 690–696 |
| `oms_project_graph_affected` | `graph project affected --head HEAD --json --base BASE` | 697–719 |
| `oms_execution_graph_status` | `graph exec status --json` | 720–726 |
| `oms_execution_graph_route` | `graph exec route --json --run RUN` | 727–745 |
| `oms_execution_graph_events` | `graph exec events --json --limit 40 --run RUN` | 746–767 |
| `oms_memory_recall` | `agent-memory recall --json --limit N QUERY` (native addition) | n/a |
| `oms_symbol_slice` | In-process bounded numbered slice; no spawn, no scrubber crate (local fail-closed secret screen) | n/a |
| `oms_failure_diagnose` | One of `runtime failure classify`, `fail-ledger check --cmd CMD`, `fail-ledger list --json --limit 10` (native addition) | n/a |
| `oms_output_read` | In-process `oms-output-capture::read_capture_checked` with shared outbound scan; retained bytes only | n/a |
| `oms_state_compact` | `state compact --dry-run` by default; `keep_recent` retains a per-family count (default 25), and `older_than_days` retains newer files (default 7 days) | n/a |

The table anchors identify canonical definitions. Execution handlers are at
852–1000, 1105–1290, 1292–1753 and 1857–1929; `tool_definitions` starts at 771.
The golden arrays are the executable argv/schema evidence.

State commands run in the requested physical repository with inherited child
restrictions. They have a 30-second bound. Thread/room and ask
thread preparation have a five-second bound. Output is streamed into a bounded
prefix while complete decoded character counts continue: oversized JSON is
refused; raw text retains the first 60000 characters and an omission count.
Interruption/timeout kills the owned process group and reaps the direct child.
No pid recovered from disk is signalled; stored pids are liveness probes only.

Peer launches preserve context flags, provider/model validation, consult thread
choices and ask preparation. MCP alias validation is in `src/peer.rs`; the
provider executor is in `oms-command-peer`. Their identity/discovery duplication
is tracked as CC-01. Discovery of a custom executable does not invoke it or
prove authentication or runtime support. Provider roles, effort and read
isolation remain owned by the fixed downstream verbs; MCP exposes no new knobs
for them. The native owner supplies the existing cancellation directory. Current
per-seat execution uses the Rust peer executor and `oms-supervisor`, injected by
`CollabServices`; retained `peer-common.sh`/`run-bounded.py` references are historical
compatibility evidence. An entire council still does not acquire an invented
single-call timeout from this adapter.

## Filesystem authority, concurrency and interrupted writes

Reads use descriptor-relative `oms-safe-fs` acquisition with no-follow ancestry,
regular/single-link checks and observed-change detection for operation files,
receipts and owned answers. Owned answers are bounded at 8 MiB and references
bind the operation, filename and normalized answer digest. Search is literal;
range boundaries never split UTF-8. Historical artifact paths remain explicit
legacy reads with completed-section parsing, bounded at 8 MiB here. Logs are
streamed through bounded line buffers; references use one captured file length
and tails read at most 64 KiB. This is observed-change protection, not a sandbox
against another process with the same filesystem rights.

| Written path or record | Concurrent call / kill between writes |
| --- | --- |
| `.oms/.gitignore` | Create-only publication of a synced temporary inode; another start retains the existing entry. A kill can leave a private `.part`, without publishing partial contents. |
| `.oms/artifacts/mcp/OPERATION/` | Random operation ids and exclusive directory creation separate starts. A kill before metadata publication leaves an incomplete run, never a fabricated successful result. |
| `prompt.txt`, `meta.json`, private `launch.json` | Each uses exclusive mode-0600 temporary creation, full write, file sync, rename and directory sync. No child starts before all three are published. A kill can leave `.part` files or a partially prepared run. No automatic relaunch. |
| `owner.lock` | No-follow, single-link, owned mode-0600 file with nonblocking exclusive `flock`. A second owner refuses; process death releases the lock. The inert file can remain. |
| `pid`, `status` | Synced atomic publication. An owner death after pid publication without status reads stalled on positive evidence of death. Missing/malformed pid remains unknown/running; malformed status is a failed completion. |
| `cancel-request.lock` | No-follow, single-link, owned mode-0600 file with blocking exclusive `flock` serializing `cancel-request.json` validation and publication across both fast-path creation and invalid-receipt replacement. Process death releases the lock; the inert per-operation file can remain. |
| `cancel-request.json` | Idempotent first-wins publication under `cancel-request.lock`: uses `RENAME_NOREPLACE` where supported and falls back to `link` + `unlink` on filesystems that reject rename flags, so the first valid receipt for the task wins and retries preserve its exact bytes. Legacy receipts without `requested_at` remain valid (`requested_at` is `null`); malformed `requested_at`, invalid or foreign files are replaced on cancel and ignored on read. A kill before publication leaves no receipt plus a possible `.part`. Receiving no RPC response does not prove no receipt was recorded. |
| `cancel-observed.json` | Native owner writes it only for cancellation before launch. Once the CLI starts, its existing supervisor owns observation. A requested receipt alone never proves provider termination. |
| `run.log` | Separate exclusive log per operation, streamed by the owner/verb. Kill can leave partial logs; logs are not completion receipts and are not claimed durable. |
| `answers/*`, thread/room records, downstream caches/locks | Fixed OMS verbs own these writes and their existing safety protocols. A second call has their documented effects; this crate neither coalesces writes nor bypasses their locks. |

Task snapshots (`tasks/get`) and peer status/result payloads (`oms_peer_operations`,
`oms_peer_result`) report a `cancellation` object with `requested_at` (from the
first valid `cancel-request.json`, or `null` when absent or omitted by a legacy
receipt) and `state`: `none` (no valid request), `requested` (request recorded
while running, or in the metadata-only `oms_peer_operations` listing for a
completed run without observation), `confirmed` (`cancel-observed.json` present),
`completed_first` (single-task read only: request recorded, and the operation
finished its own work without observation) or `unknown` (request recorded, and
the operation stalled or ended without a durable own exit or observation).
Existing status and compatibility fields remain unchanged.

Mutation opens require owned, real ancestry. Existing directory modes are
preserved; newly created directories use 0700. Receipt files cannot follow links.
A completed result does not promote its data into approval or verification.

## Retained output reads

`oms_output_read` closes the CC-04 digest-to-reader path. Pass a digest's
`raw.read_arguments` unchanged for the first page, then reuse
`next_read_arguments`, `next_search_arguments` or a hit's `read_arguments`.
`query` performs literal search; `queries` batches one to eight literals with
`compact=true`. Compact responses return `read_base` plus relative continuation
fields to merge into that base. This store is separate from the normalized
operation answers read with `oms_peer_result.answer_ref`.

The main caller retains the existing explicit `repo` choice. Delegated workers
can read only their captured caller repository. A physical UTF-8 root without
control characters is required; a reference binds repository, filename and
SHA-256 but does not authorize a different root or filesystem path. The reader
opens only `.oms/artifacts/output-digest/<capture-name>`, preserving no-follow,
single-link, root/path identity, change detection, the 64 MiB stored-byte bound,
and SHA checks from `oms-output-capture` and `oms-safe-fs`.

The adapter scans the same complete verified snapshot with `oms-scrub` before
returning any page or hit, so a key block outside a selected page still withholds
the result. It uses the producer's bounded OS environment scan semantics and
also checks search text that could be echoed. A scan finding or failure returns
`capture_withheld` without capture content. The producer's `withhold` behavior
still removes retrieval metadata; this reader adds no completeness or
verification claim to a stored capture.

Pages contain up to 16384 decoded UTF-8 bytes and use character boundaries.
Search has an eight-hit budget. The complete JSON text payload is limited to
65536 serialized UTF-8 bytes, inclusive; oversize returns `response_too_large`
instead of truncating JSON. This bound excludes the MCP envelope and its JSON
string escaping. Results use the existing text content form without a second
`structuredContent` copy. Execution failures set `isError=true` and carry
`{"error":{"code":"...","message":"..."}}`, retaining canonical capture
error codes. Missing, changed, unsafe or oversized captures never rerun a
command. The existing native fixture `output_capture_reads` exercises producer
metadata through discovery and the real stdio reader, including these boundaries.

## Historical component admission evidence

This section records the original migration review and its then-unresolved
integration work. Its past check results are not new test runs for the current
source revision; current native wiring and boundary fixtures are listed above.

The reference server was read in full (1–2243), including the complete handler
(1945–2183). Direct helper coverage: `peer_artifacts.py:14–73,260–271`, all of
`peer_cancellation.py`, graph resource renderer `oms_graph/render.py:589–1143`,
provider normalization `provider-registry.sh:1–145`, and the cancellation/deadline
owner in `run-bounded.py:1–174`. Scoped test inspection covered MCP cases in
`state-surfaces-smoke.sh`, provider registry MCP routing around 1011, and graph
MCP search/symbol cases around 315/331. This does not claim those shell suites ran.

Native checks use a compiled stdio client and a compiled fake OMS executable
linked only into wholly synthetic source roots. No provider or room service is
called. Goldens cover 126 canonical handler exchanges, all tool definitions,
23 state argv arrays, nine peer argv variants, all 13 provider aliases and cursor
byte contracts, including all 23 built-in provider names. Functional cases cover framing recovery, profiles, graph bounds,
peer completion across server exit, receipts, cancellation concurrency, negative
pids, malformed environment restrictions, owner re-entry, corruption, foreign paths, symlinks/hardlinks, hidden-middle changes,
UTF-8 ranges/search, output bounds and direct-child interruption/reaping.

## Frozen parser and path contract

MCP uses [`oms-context`](../oms-context/src/lib.rs)'s additive
`parse_experiment_json_with_depth(input: &[u8], limit: usize, max_depth: usize)`
producer and the local [`value` adapter](src/value.rs). The producer keeps
WTF-8 strings, raw number spellings and duplicate-last object values. Existing
`parse_json`, `parse_experiment_json` and session entrypoints retain the default
1024-container budget and their original number/string/byte dialects. Existing
production consumers are `oms-context::manifest`, the root `oms` crate,
`oms-command-input`, `oms-work-inspect` and `oms-experiment` for `parse_json`;
`oms-command-experiment` for `parse_experiment_json`; and
`oms-context-session`, `oms-command-attempt-bundle` and `oms-command-telemetry`
for `parse_session_json`. The new entry permits budgets from zero (scalars only) through 16384. It validates depth
before decoding and serializes deep parsing on a bounded worker stack: 16 MiB
minimum, 16 KiB per observed container, at most 256 MiB reserved. MCP requests
use 9997, not the shared default. Clone, equality, encoding, representation and
drop in the local adapter use explicit work stacks rather than Rust recursion.
No new external dependency family or unchecked serde representation is used;
`ryu` is the existing cached float-formatting family.

The frozen production reference is **CPython 3.12.15**, source revision
`06fa2cbe015681600633944bcf71e94f60285c83`, with server SHA-256
`1316d7197dc9d9a758e60f3afd8ee2f2891ff7b5bb346762f2419b4382c1cefd`.
Actual stdio probes accept 9997 total containers and reject 9998 for array,
object and mixed nesting, including inert ignored fields. CPython 3.11.15
rejects the recorded 1025-array probe. Native MCP intentionally preserves the
chosen 3.12.15 boundary independently of host Python; it does not claim both
incompatible extreme-stack behaviors.

[`parser-reference.json`](tests/parser-reference.json) contains the original
43 adversarial exchanges plus 27 boundary/domain/deep-resource exchanges.
Parser expectations were captured from the reference, never the candidate. Long
requests/responses use lossless repeated UTF-8 chunks with lengths and SHA-256
of the expanded bytes. This removes duplicate fixture bulk without changing the
frozen bytes. The metadata records runtime, source digest and the 3.11 probe.
The CC-04 catalog addition updates only the `nonfinite-list-params` response's
tool list from the independent `definitions.json` fixture: `oms_output_read`
precedes `oms_state_compact`. Its non-finite request, response envelope, other
69 exchanges and all frozen source/chunk hashes remain unchanged. This is a
catalog payload update, not a new parser reference or a relaxed comparison.
[`value-reference.json`](tests/value-reference.json) independently freezes
canonical metadata, graph Unicode fields and the raw-path result error.

| Surface | Native behavior and reference evidence |
| --- | --- |
| Wire bytes / BOM | Strict UTF-8 decoding, Python whitespace stripping and 256 KiB newline-inclusive framing; no UTF-16/32/BOM byte-encoding detection. Invalid UTF-8 is a parse error. |
| Internal strings / filesystem JSON | `value::parse` retains valid WTF-8 Python string data; `from_slice` first applies strict UTF-8 filesystem decoding. Escaped surrogates survive JSON parsing; invalid literal file bytes do not masquerade as strings. |
| Depth | Frozen 9997 total containers, checked before the decoder; boundary, over-bound and >1024 accepted records cover all three nesting shapes. Shared default 1024 remains unchanged. |
| Id / method | Lone surrogates produce the source's invalid-id/invalid-method errors before notification handling; valid absent/null ids cause no method effects or response. Numeric NaN/Infinity ids remain numbers. |
| Ignored fields / keys | Surrogates and non-finite values remain data and do not cause unrelated parse failures. Duplicate keys retain first position and last value; no private serde marker can impersonate a scalar. |
| Consumed text / protocol / resources | UTF-8 validation occurs at each source field boundary. Resource Python representations and unsupported-protocol diagnostics retain surrogate identity; protocol prefixes count code points. |
| Numbers | Python integer normalization, binary64 float conversion/formatting, NaN/Infinity and the 4300-digit integer bound; 4301 digits refuse at parse time. The shared raw representation remains unchanged. |
| Raw Unix cwd / peer argv / owner | OS bytes reach fixed state, graph, thread, consult, advise and ask argv unchanged. The private helper reads OS arguments, not Unicode-only strings. Compiled fixtures verify raw source, repo, artifact and prompt paths. |
| Returned paths / metadata | Surrogateescape converts raw filesystem bytes to lossless Python strings for response JSON; `Text::os_string` reverses that path domain. Metadata and graph strings also preserve ordinary lone surrogates. Explicit non-UTF-8 `repo` arguments still receive the canonical text-field refusal. |
| Raw-path peer result / task snapshot | The reference strictly UTF-8 hashes its path-bearing cursor snapshot and raises a field error. Native returns the same -32602 response; it neither invents a cursor nor falls back to an ordinary result if task promotion fails. Launch paths and durable artifacts remain intact. |

Native syntax and filesystem diagnostics can use different platform/library
wording outside the frozen exchanges. Error classes, refusal boundaries and
lossless data are tested; no blanket byte-identical diagnostic claim is made.
Child stdout/stderr still use the reference's explicit UTF-8 replacement policy.
This differs from filesystem path encoding, which never uses lossy display in
argv or returned string values.

Native safety bounds on legacy branches are explicit compatibility changes:

| Branch | Native bound / failure or edge behavior | Reference contract |
| --- | --- | --- |
| Legacy answer file | 8 MiB inclusive; oversized, linked or unreadable files add the existing `answer_errors` and set `isError`; other readable answers survive | `artifact_answer:1326–1330` has no byte cap; canonical legacy symlinks are followed |
| Successful thread/room output | 60000 decoded characters inclusive; excess returns `error: thread JSON output exceeds 60000-character limit` | `thread_exchange:1925–1926` returns all stdout; failure output already has the canonical 60000-character clip |
| Log reference record | 16384 raw bytes including newline; an overlong record is drained and omitted, and later records remain readable | `log_references:1308–1323` has no line cap; total log scan is not tail-capped here |
| Run metadata | 256 KiB inclusive; malformed, unsafe or oversized metadata becomes `{}` | `run_meta:1438–1442` falls back on parse/read errors but has no byte cap; missing answer-storage metadata retains legacy interpretation |
| Stored status / pid | 4096 raw bytes inclusive; malformed/oversized existing status means done/exit 1, malformed/oversized pid means unknown/running | `run_status:1489–1495` and `run_alive:1473–1477` read without a byte cap |
| Private launch record | 128 KiB inclusive; unreadable/oversized/invalid typed launch fields fail ownership before downstream execution and record failure when ownership was acquired | New private native ABI; no canonical launch-record format exists |
| State/receipt/answer ancestry | Real no-follow directories, regular single-link files and observed-change checks; mutation requires current ownership | Strengthens legacy reads while preserving canonical owned-answer and cancellation restrictions; independent trust review remains required |

The existing compiled `peer_reading` scenario now covers empty, malformed,
normal, exact-limit and over-limit legacy answers, metadata, status, pid and log
records, plus successful thread output and private launch-record bounds. Its regression demonstrates that
an 8 MiB refusal no longer becomes successful answer text. A malformed artifact
without a completed section remains an empty answer with the canonical note;
its bytes are never fabricated. These bounds are not silently claimed as parity.

### Historical reconstruction checks

The component-era Linux checks did not prove native macOS, WSL2, Ubuntu 20.04
binaries, installed registration, provider cancellation escalation, real room
membership or live model availability. Integration and host checks were then
parent work. These historical results must not be substituted for the current
revision's CI or native entry fixtures.

That focused verification used isolated scratch workspaces, Rust 1.99.1, two build
jobs and the offline dependency cache. Package check, Clippy with warnings
denied, all MCP tests, the shared context tests and pinned formatting are the
component checks. The complete admission verifier, installed entry and host
matrix remain parent work. A second scratch integration used the current
06fa2cbe context sources and retained its `inspection_limit_value` export; its
context contracts and the MCP reference scenario passed. The recovered candidate
was based on `60f8e677`; this reconstruction applies
to `06fa2cbe015681600633944bcf71e94f60285c83`. That base already has
`parse_experiment_json` and its regression, so reconstruction retains them and
adds only the explicit-depth API and regression. The existing
`inspection_limit_value` export, context-inspect reference pin, root consumers
and manifest stay unchanged. This base carries the 64 frozen context cases in
`oms/tests/context_manifest_contract.rs`, not a separate `oms-command-context`
crate; all three frozen-corpus tests pass with the reconstructed producer.

The three new compiled scenarios cover the 70 frozen exchanges, raw peer paths
and lossless value/metadata/graph behavior. Existing scenarios retain the prior
31-tool, profile, protocol, receipt, output, cancellation and argv coverage.
Only expectations whose canonical behavior changed from the former native
parser refusal were corrected, using frozen reference evidence first.
Reference metadata and local API checks supplement the existing suites; no
legacy suite is removed while its installed entry remains.

Reversal evidence is reported with the patch artifacts. Removing a newly added
producer/type API can prevent tests from compiling, so that absence alone is
not claimed as behavioral proof. Buildable reversals additionally restore the
old parser/depth behavior and raw-path refusal/encoding, then run the affected
functional scenarios. A deterministic completed-owner fixture also reproduced
and guards the task-promotion fallback. Production bytes are restored before
returning the patch. Patch exports and reports remain outside the product;
there is no embedded `component.patch`.
