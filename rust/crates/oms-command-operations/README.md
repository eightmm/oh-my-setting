# Native lifecycle and approval operations

This crate implements the canonical `scripts/lib/agent-events.py` operations.
It invokes no legacy command or interpreter. The existing native
`oms-command-attempt-bundle` handles bundle selectors, including its existing
abbreviations and transcript path handling.

## Parent integration contract

`App::{Events, Approvals}` selects the namespace. `App::from_namespace` accepts
`events`/`agent-events` and `approvals`/`approval-inbox`. Both `owns_events` and
`owns_approvals` own their complete verb, including invalid arguments and help;
there is no fallback status for these complete routers.

`run(app, argv, default_repo, interrupted)` returns `(status, stdout, stderr)`.
Arguments exclude the public verb and keep global `--repo REPO` before the
subcommand. Use `run_streaming` with the actual output writer for `follow`:
it flushes every row before waiting, preserves partial output on error, and
checks interruption while waiting. A buffered `run` necessarily returns only
when following ends. Command errors return status 2. Help returns status 0.
All parsed command options are listed below and in command-specific `--help`.

`V` is the lossless JSON record boundary: WTF-8 string bytes, arbitrary decimal
integers (the Python encoder's 4300-digit limit), Python float serialization,
unknown object fields, and Python scalar equality. `V::parse` accepts the Python
string JSON dialect; it does not reinterpret UTF-8 ledger text as another byte
encoding. Nesting beyond the shared parser's supported 1024 containers fails.
`project_attempts`, `load_projection`, `validate_event_row`,
`approval_projection`, `load_approvals`, `public_approval`,
`approval_effective_state`, and `approval_action_digest` expose the common core.
`load_projection` always validates the canonical stream. The legacy optional
performance cache is ignored and never grants authority or hides a corrupt row.

For internal writers, construct `StartRequest` and call `create_attempt`;
construct an event with `new_event` and call `append_lifecycle`. Both receive
an interruption flag. They allocate sequences and fence idempotence under the
canonical event lock. Raw storage helpers are exposed for existing consumers
that already own the canonical transaction: hold `lock::FileLock` throughout
read/project/append; never acquire the same lock recursively. Direct core APIs
are for the trusted parent and do not confer execution authority. The public
router refuses mutations from harness children. JSON approval projections used
outside this process must pass through `public_approval`, which strips internal
grant material. Grants are returned only by an explicit successful decision.

No memory/journal/provider/scheduler service is called by these canonical
operations, so this producer needs no callbacks to those other domains.
The parent must wire both public verbs and replace internal Python imports and
subprocess adapters. Known consumers include:

- `attempt-runner.py`: `new_event`, `append_lifecycle`, `create_attempt`,
  `read_rows`, event/spec paths, and projection.
- `panel_cache.py`, `panel_reader.py`, `room.py`, `panel_results.py`,
  `panel_state.py`, `panel_release.py`, `panel_host.py`, `panel_chats.py`:
  projections, bounded event reads, state transitions, and validation.
- `landing-reference.py`, `landing-capture.py`, `plan-completion.py`,
  `review_settlement.py`: writer/projection APIs.
- `inbox_projection.py`, runtime collectors, MCP tools, `peer-delegate.sh`,
  peer heartbeat/supervisor paths, and `patch-land.sh`: CLI or projection calls.

Preserve the patch-land wrapper's exact attempt/action/baseline/patch/parameter
checks before consuming a grant. The canonical `finish-consume` checks state
and version plus a valid consumer identifier; it does not add a comparison to
the reservation actor. Attempt ownership is a wrapper fence, as exercised by
`tests/patch-land-approval-smoke.sh`'s attempt-bound approval cases. Room
membership, worker text, and event records grant no authority.

This standalone crate has its own temporary workspace and lockfile. Root
workspace registration, unified lockfile, native selector/main wiring, source
inventory, release/install/CI and public docs remain parent integration work.
No changes to those files are included. The admitted bundle crate still owns
its independent read-only bundle projection; shared-core extraction across
that crate, if desired by the parent, cannot be done within this scope.

## Source and mode checklist

Canonical source anchors refer to `scripts/lib/agent-events.py`.

| Native command | Options after the subcommand | Canonical anchor |
| --- | --- | --- |
| events start | `--provider`, `--tool`, `--task-id`, `--run-id`, `--parent-attempt-id`, repeated `--ref`, `--max-wall-seconds`, `--max-tokens`, `--max-cost-microusd`, `--idempotency-key`, repeated `--then`, `--then-actor`, `--then-actor-kind` | 899–1043, 2134–2152 |
| events transition | `--attempt`, `--state`, `--expected-state`, `--expected-seq`, repeated `--ref`, `--reason-code`, `--actor-kind`, `--actor`, `--idempotency-key`, repeated `--then` | 1046–1074, 2154–2166 |
| events recover-verify-heartbeat | `--attempt`, `--expected-sequence` | 1077–1114, 2168–2174 |
| events heartbeat | `--attempt`, `--actor`, `--idempotency-key` | 1117–1129, 2176–2180 |
| events usage | `--attempt`, `--tokens`, `--duration-ms`, `--cost-microusd`, repeated `--ref`, `--actor`, `--idempotency-key` | 1132–1161, 2182–2191 |
| events show | `--attempt`, `--json`, `--bundle`, `--transcript` | 1341–1372, 2193–2201; admitted native bundle |
| events list | `--active`, `--state`, `--limit`, `--json` | 1375–1395, 2203–2208 |
| events resume | `--attempt` | 1398–1425, 2210–2212 |
| events follow | `--after-event`, `--limit`, `--timeout`, `--interval` | 1428–1456, 2214–2219 |
| events reconcile | `--stale-seconds`, `--apply`, `--json` | 1459–1513, 2221–2225 |
| events validate | none | 1516–1521, 2227–2228 |
| events compact | `--days`, `--apply` | 1524–1614, 2230–2235 |
| approvals request | `--attempt`, `--action`, `--object-id`, `--summary`, `--expires-in`, `--base-sha`, `--patch-sha`, `--task-id`, `--lease-id`, `--profile`, `--parameters-json` | 1787–1839, 2239–2252 |
| approvals decide | `--approval`, `--decision` (approve/deny/reject), `--expected-version`, `--actor`, `--changes-json` | 1848–1894, 2253–2259 |
| approvals begin-consume | `--approval`, `--token`, `--expected-version`, `--consumer` | 1897–1939, 2261–2266 |
| approvals finish-consume | `--approval`, `--expected-version`, `--result` (consumed/failed), `--consumer` | 1942–1969, 2268–2273 |
| approvals expire | `--apply`, `--json` | 1972–2003, 2275–2278 |
| approvals reconcile | `--older-than-seconds`, `--apply`, `--json` | 2006–2068, 2280–2292 |
| approvals show | `--approval`, `--json` | 2075–2091, 2294–2297 |
| approvals list | `--pending`, `--state`, `--effective-state`, `--json` | 2094–2113, 2299–2304 |
| approvals validate | none | 2116–2125, 2306–2307 |
| approvals path | none | 2128–2129, 2309–2311 |

All subcommands accept `-h`/`--help`. Long options retain unique-prefix
abbreviations. References and descriptions retain their canonical bounds and
validation. Lifecycle states, reason codes, persisted trace fields, sequence
allocation, state CAS, request/event idempotence, usage reports and recovery
transitions follow 637–992. Approval digest/projection rules follow 1617–1784.
Expiry is effective on reads before an expiry row is appended. Generic approval
reconciliation never guesses whether an effect happened and defers patch-land
to its stronger intent recovery.

## Persistence, concurrency and interruption

- `events.jsonl`: one `owner.json` transaction lock for writers. A second writer
  sees completed prior rows before allocating its sequence. A kill can leave a
  torn final row; strict projection refuses it. Low-level append separates a
  torn tail with a newline, matching canonical append behavior. No automatic
  removal of arbitrary damaged history is attempted.
- Private approval JSONL: same distinct owner protocol and CAS over the entire
  projection. A second begin-consume loses after a successful reservation. A
  kill after reservation leaves `consuming`; recovery records `interrupted`
  with outcome `unknown` or defers patch-land. No effect is run by this crate.
  The store stays in the per-user XDG state directory with mode 0600; its
  approvals directory is restricted to 0700 on request creation.
- `attempts/ATTEMPT/spec.json`: creation event is fsynced first, then the spec is
  published atomically at 0600. A kill between them leaves an event without a
  spec; the same start request key repairs it. Without that key, canonical
  recovery requires the owner to reconcile the incomplete creation.
- Atomic spec/compaction publication uses `oms-safe-fs`. A kill before rename
  may leave an exclusive temporary file; later calls do not delete unrelated
  temporary files. Failed publication reports uncertainty, never success.
  Published bytes and parent directories are fsynced. Compaction replaces only
  whole old terminal streams, keeps ancestors transitively, revalidates the
  survivors, preserves surviving text rows, and prints the canonical receipt.
- `.oms/.gitignore`: exclusive first creation; a concurrent call preserves the
  winner. A kill during this initial small write can leave an incomplete ignore
  file, as in the canonical initialization; it is never overwritten silently.
- Lock directory `owner.json`: PID, process start, nonce and start time use the
  existing schema/name/hash. A live generation never expires with the wait
  bound. Dead/reused/zombie generations may be reclaimed. A malformed owner is
  aged conservatively. The persistent `.recovery` file carries a nonblocking
  kernel flock; a contender rechecks the exact snapshot generation under it,
  then renames to an exclusive `.stale` name. A kill releases the kernel lock;
  a partial owner ages out. Release requires the same directory and nonce.
  A kill during stale cleanup can leave the retired stale directory.

Raw OS repository paths are retained for filesystem/Git access and store/lock
hashing; direct linked ledger parents and linked/multi-linked leaves are
refused before reads, append or chmod. Benign ancestry aliases above the direct
parent resolve to the same physical namespace. Permission changes operate on
validated descriptors.

## Scoped verification

Run unit fixtures with disposable `XDG_STATE_HOME`, `OMS_LOCK_DIR`, and a
parent-context fixture environment; never point approval fixtures at live
state. The tests are in `src/tests.rs`; no canonical tests were modified.
They cover contention, stale-generation recovery, spec repair, start/event
idempotence, lifecycle CAS, usage deltas, projection validation, link refusal,
torn tails/private modes, follow bounds, compaction ancestry/bytes, approval
CAS/replay/expiry/unknown recovery, scalar/Unicode/number boundaries and parser
failures before effects. The parent owns canonical cross-domain tests and the
required scoped cargo check after admission.

Trust-boundary evidence: private approvals → parent-controlled XDG namespace →
forged shared event/UI state → action digest plus private grant hash, expiry and
locked version CAS → `approvals.rs` and grant/replay/expiry fixtures. Ledger
pathname → filesystem descriptor boundary → linked leaf/parent substitution →
regular/single-link and before/opened/named identity checks → `storage.rs` and
link-canary fixtures. Lock ownership → concurrent writer boundary → dead-owner
ABA/reused process → snapshot generation plus kernel recovery gate and exact
release identity → `lock.rs` and contention/recovery fixtures. The parent's
wrapper remains responsible for the specific effect and exact attempt owner.

Verification caveat: reversing every non-test addition removes the new crate's
manifest and implementation. All eight individual reverted test invocations
therefore fail because the manifest is absent; this is a baseline build failure,
not evidence that each behavioral assertion detects a regression. Restoration
is checked byte-for-byte. The contention regression did fail with the initial
implementation and passed after fixing exclusive ignore-file initialization.
