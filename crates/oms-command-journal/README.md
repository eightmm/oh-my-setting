# oms-command-journal JSON compatibility

Journal events, adapter records, project identity, config, quarantine/index and
sync state, cold manifests/segments, recorded search, and Notion responses parse
with `oms_json::parse_python_ordered` (`python_compat`). This matches Python
`json.loads`, including duplicate keys updating their original insertion slot.
No input surface uses strict mode: the Python implementations accept duplicate
keys, nonfinite numbers, and escaped surrogate code points.

Integers retain their decimal value through normalization, identity hashing,
SQLite payloads, cold replay, Notion request construction, rendering, and JSON
output. The parser accepts up to 4300 decimal digits and 1024 nested containers;
overflow or malformed input produces a parser error. Integer `1` and float `1.0`
remain distinct; integer `-0` normalizes to `0`, and float `-0.0` retains its sign.
Schema counters still require a nonnegative machine-sized integer where their
Rust fields have that type; overflow is an explicit manifest error.

Sanitization visits the first 64 original object entries in insertion order,
then normalizes keys. Trimmed key collisions retain the last visited value.
Canonical serialization sorts keys only after this step. Keys named
`$serde_json::private::Number` are ordinary keys. Sanitization removes nonfinite
values. Canonical writers reject remaining nonfinite numbers and lone
surrogates, preserving the Python strict UTF-8 writer's failure instead of
substituting text. Adapter ingestion and event normalization reject selected unencodable
strings before coercing identifiers to Rust strings; unused fields stay ignored. The low-level parser and
sanitizer preserve surrogate code points; valid surrogate pairs become Unicode
scalars. Display-only rendering escapes lone surrogates explicitly.

The CLI (`run`), `MemoryAppendFn`, canonical v1 schema, and cold manifest layout
retain their compatibility entry points. The native root uses the additive
`run_with_batch_services` adapter: `distill.json` fixes a bounded prepared batch,
then the memory writer publishes candidate keys with the source records before
the journal completes its receipt. Prompt hooks use the same keyed batch writer
and one summary/index refresh, with the shared count-only distill report.
Legacy callbacks keep content-based retry
reconciliation. Source/payload collisions and changed sources cannot authorize
new appends; automatic notes still wait for memory review. Existing locks,
append/recovery, atomic replacement, cold pending
seal protocol, and bounded streaming Notion transport still govern writes.

`tests/lossless_json.rs` covers exact integer round-trips, numeric identities,
ordered sanitization, duplicate/private-marker keys, nonfinite filtering,
surrogate preservation/errors, parser boundaries, adapter ingestion, dedupe,
SQLite reads, cold replay, and exact Notion retry counters.
Only the retry scheduling calculation saturates after reaching its delay cap;
the persisted retry count retains its full decimal value. CI attempt ordering
compares decimal magnitudes without a machine integer conversion. During the implementation-first phase these tests
are compiled with `cargo check --all-targets --offline`; execution is deferred.
