# oms-command-telemetry

The native CLI owns artifact-index views and mutations. The reusable `store`
module provides repository-confined transactions without a Python subprocess.
All code forbids unsafe Rust.

`store::register_panel_result(repo, owner, task_id, artifact_relative,
interrupted)` publishes a successful `panel-result` and returns its event id.
`store::append_artifact_index(repo, request, interrupted)` accepts an
`AppendArtifactRequest`: kind, provider, exit_code, artifact, patch, prompt,
verify_exit, source_artifact, task_id, optional index and task_goal,
env_overrides, inherit_env, and append options. Defaults inherit environment;
an explicit empty override suppresses a variable without changing the process.
Overrides use canonical OMS variable names, including run, operation, attempt,
parent, plan, model, reasoning, trace, review and criterion-coverage context.
Panel registration clears inherited plan, active-task, parent, attempt and
operation context, while retaining clean run and trace context. A valid
`.oms/runs/CURRENT` supplies a missing run id within `OMS_RUN_CURRENT_TTL`
(default 86400 seconds).

Internal artifact, patch and source paths are repo-relative or absolute under
the logical/physical repository root. Each must be a readable, owned,
single-link regular file without symlink traversal. The writer hashes their
bytes under the index lock, records relative paths and SHA-256 digests, and
uses the artifact or patch digest for artifact_id. Without either payload it
hashes event_id. External references record only basename, ownership false,
and a digest when safely readable. Prompt metadata contains only hash and byte
count. Existing metrics helpers best-effort cache duration_s, tokens,
provider_usage, served_model, configured_model, model_attribution and cost_usd.
Invalid typed review or coverage payloads refuse publication.

`store::append_rows` accepts exact JSONL bytes plus `AppendOptions` (keep,
high_water and an optional expected snapshot). Every new row requires a unique
nonempty string event_id. `parse_index_line` and `read_index` reject partial
rows, blank/non-object JSON, invalid UTF-8, NUL, bare CR, and non-finite JSON
constants; CRLF is preserved. Each row is at most 1 MiB; input and stored index
bounds are 256 MiB. Empty append batches only read. `replace_bytes` performs
bounded replacement with the same snapshot comparison and locking authority.

Retention reads `OMS_ARTIFACT_INDEX_KEEP` (default 1000) and
`OMS_ARTIFACT_INDEX_HIGH_WATER` (default keep + keep / 5); malformed environment
integers use defaults, while nonpositive keep or high below keep refuse.
`retained_lines` takes `RetentionOptions`. It preserves complete earlier,
unique resolution-target closures, required events, completion pins, and
unresolved failures. A resolution only settles a failure with matching
operation_id/artifact_id, matching parent_event_id/resolves_event_id, a complete
schema-1 outcome, resolution resolved, and exit 0. Recent panel decisions
(within 30 days and at most five minutes in the future, newest 3000) are
preferred within the byte budget. Protected rows do not consume the ordinary
keep floor. Required lineage exceeding keep or any hard protection exceeding
the byte ceiling refuses the transaction.

`completion_references` / `completion_pins` scan at most 4096 content-addressed
receipts, each at most 4 MiB and at most 64 MiB total. Invalid names, hashes,
duplicate keys, non-finite constants, shapes, unsafe files or observed changes
return None. None protects every index row, subject to the byte ceiling.

`canonical_index` rebases root aliases and validates confined paths;
`ensure_oms_ignore` initializes `.oms/.gitignore` to `*` plus LF only when
missing. Existing contents are preserved. Writers use `oms_lock::Lock::for_file`
with environment settings and Bash-compatible lock mode selection. The lock spans index reads,
payload hashes, snapshot CAS, retention and atomic replacement. Creation uses
0700 parents and 0600 files subject to umask; existing index permissions follow
`oms-safe-fs` replacement semantics. The temporary body is fsynced before
rename and its directory is fsynced where supported.

`salvage_index` plans or applies repair, returning `SalvageReport`. Apply first
publishes and verifies an exact, content-addressed, mode-0600 quarantine, then
replaces the corrupt snapshot with recovered rows and a required salvage
receipt. A second serialized call sees the repaired healthy index. A kill
between writes leaves the quarantine and old index; after index publication,
the quarantine already exists. A mismatch at an existing quarantine refuses.

`delete_orphans` returns `OrphanReport` and owns only `.oms/artifacts` regular,
owned, single-link bodies. It preserves indexed/completion paths, output-digest,
lock names and standard index/ignore names. It validates the kept snapshot
under the lock and refuses uncertain receipts. Scans exceeding 65536 entries
or depth 64 refuse before unlinking. Nonnegative grace is measured in seconds;
zero allows immediate deletion. Dry runs report candidates without unlinking.
Each unlink rechecks file identity and descriptor-relative ancestry. Deletion
is incremental: a kill or later error may leave a deleted prefix, which a
second call safely skips. This is a conservative bounded sweep, not a cleanup
transaction spanning every body.

StoreError preserves storage MutationPhase and exposes exit_code. Validation
returns 2, lock errors retain oms-lock status, observed snapshot changes and
uncertain/published replacement failures return 75, and cancellation returns
130. Concurrent writers serialize; readers see the old or new complete index.
A kill before rename may leave an exclusive temporary file or empty parents;
subsequent calls do not remove another operation's temporary file. Ignore
initialization can remain after an append failure. A publication error requires
state reconciliation before retrying; an identical subsequent write is not
proof of durability after the original failure.

Verification uses Rust 1.99.1 offline checks and focused CLI parity tests.
The owning agent retains dispatcher integration and full admission checks.

Native CLI ownership now covers every `artifact-index` argument vector,
including help and parse errors. `run` retains its existing signature. The
caller supplies `telemetry` as the action for both standard reports and
`telemetry --waste [--top N]`; top accepts 1 through 100 and cannot combine
with `--by`. Waste adds the schema-1 `waste` block with five categories,
reported shares and deterministic rankings. The selected index window only
limits delegate categories; model switches, injected bytes and stored outputs
use the retained hook ledger and capture directory. No report writes files.

The CLI implements list (default 20), latest, latest-run, failures, unresolved,
resolve, resolve-superseded, resolve-recovered, validate, migrate, salvage,
prune and import. Unresolved annotations and automatic resolution share one
join, with provider and complete-lineage checks before mechanical writes.
Explicit resolution validates every target before publishing one batch; a
second call reports already resolved. Migration and prune replace a checked
snapshot atomically. Prune supports --files, --stale and --dry-run; stale
cleanup preserves completion and recent decision pins plus resolution closure.
File deletion rechecks the current index after acquiring its lock, including
when a dry run reports a hypothetical retained body. Interruption can leave
an already deleted prefix; repeated sweeps skip bodies already removed.

For legacy parity, positional N is rejected for mechanical resolvers, and
latest-run emits the compact text summary even with --json. Empty read views
return schema/action/rows in JSON and fail in text; empty mechanical resolvers
report zero. Validate reports structural corruption without repairing it.
Normal views and mutations remain strict. Git worktree roots are resolved
using `git -C ... rev-parse --show-toplevel`; explicit logical aliases are
rebased before descriptor-relative index validation.

Import accepts --kind, --provider, --file, --prompt-file, --repo,
--artifact-dir and --exit, with the legacy provider aliases and custom adapter
discovery. It publishes an exclusive artifact before recording its index row.
A kill between the two writes leaves an unindexed artifact that can be
inspected or swept after the grace window; a second same-second import refuses
a filename collision rather than overwriting an existing answer. Ignore
initialization can remain after failure. Artifact destinations remain confined
to the repository, and external input files are read with the existing safe
producer helper. Sensitive content and unusable pasted answers produce
warnings. Index-accounting failure preserves the imported artifact and warns.

CLI writers and store producers use `oms-lock` LegacyAuto mode, matching
Bash's flock selection when flock is available and mkdir otherwise, with
OMS_LOCK_FORCE_MKDIR honored. Unsafe writable-by-others lock paths refuse;
interoperability fixtures create legacy locks under umask 077. Atomic index
publication leaves readers seeing either the old or new complete body. A
kill before publication can leave temporary bodies or empty directories;
after publication, retrying resolution is idempotent. Salvage always
publishes quarantine before the repaired index.

The crate tests compare native views, waste, argument failures, migration,
retention and mechanical resolution against the public legacy oracle, and
exercise batch atomicity, concurrent resolution, salvage and native import.
JSON comparison ignores object ordering; telemetry's existing golden-byte
fixtures continue to check its exact sorted output.
