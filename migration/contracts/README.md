# Migration contract vectors

These frozen inputs and results are compatibility evidence for the next native
slice. They do not establish that Rust implements the behavior. The
[implementation handoff](../../docs/RUST-CONTEXT-HANDOFF.md) separates projection,
file access, session/report modes, and installed entry point acceptance.

## Corpus and provenance

| File | Reference entry point | Case input |
|---|---|---|
| [context-inspect-v1.json](context-inspect-v1.json) | `inspect_manifest` | `input`: decoded manifest value |
| [context-compare-v1.json](context-compare-v1.json) | `compare_manifests` | `left`, `right`: decoded manifest values |
| [context-read-v1.json](context-read-v1.json) | `read_manifest` | `input_hex`: exact file bytes, hex encoded |

Each file has `fixture_schema: 1` and a `reference` object containing the commit,
source path, source SHA-256, entry point, and default `arguments`. The reference
is commit `74f87f654a314961420be2aa8ab6dfc1999151e1`; validate the recorded source
digest before regenerating or replaying expected behavior. Do not generate an
oracle from the implementation being tested.

The inspection corpus retains the original five seeds: complete source and
framing bytes, empty selection, partial source with required debt, Boolean schema
rejection, and duplicate selected-path rejection. Expanded cases cover additional
boundaries; they supplement the canonical tests rather than replacing them.

## Native consumer contract

For every named case, merge its optional `arguments` over the reference defaults
and invoke the specified operation once. A case has exactly one outcome:

- `expected`: the exact JSON-compatible value returned by that entry point;
- `expected_error`: the exact `InspectionError` message, without a CLI prefix.

Compare object keys without depending on their serialization order, but preserve
array order, numeric and Boolean types, null versus missing fields, and complete
strings. Do not ignore unknown fields or normalize errors and qualifications.
Hash encodings, rendered command bytes, stderr framing, and exit statuses are
separate contracts; structural JSON equality does not prove them.

For read cases, decode `input_hex` without UTF-8 replacement or JSON parsing,
write those bytes to a fresh owned regular file inside a disposable repository,
then call the safe reader. Parsing and reserializing the fixture first would
erase duplicate keys, malformed encoding, and other relevant evidence. The
expected success value is the reader's decoded JSON, not an inspected manifest.
Apply semantic manifest validation separately when the public operation needs it.

The byte corpus exercises decoder behavior. It does not create symlinks,
hardlinks, foreign ownership, special files, oversized files, or concurrent
mutation. Preserve the canonical filesystem cases and run their native
equivalents before retiring the corresponding implementation.

## Existing suite and candidate command

[`tests/test_context_inspect.py`](../../tests/test_context_inspect.py) remains the
canonical coverage for projection, comparison, safe reads, current-source
verification, session metadata, reports, and child authority. Its `--cli` mode
also exercises public option combinations and unchanged fixture state.

```bash
python3 tests/test_context_inspect.py --cli
python3 tests/test_context_inspect.py --cli --candidate-oms /absolute/path/to/oms
```

The candidate must name an existing executable file and requires `--cli`. Its
path becomes absolute without resolving symlinks, preserving a supplied install
link as the entry point. For each existing public `context-inspect` CLI scenario,
the adapter runs the public `scripts/oms runtime` reference first, including its
dispatcher error hints, and the candidate second, using
`runtime --repo DIR context-inspect ...` for the candidate. It checks unchanged
fixture file contents after each invocation and compares exit code and the
complete decoded stdout/stderr strings exactly. Text decoding includes newline
handling, so this is not raw-byte stream or process-trace verification.

Run the suite in its separate reference checkout and pass the candidate from the
implementation checkout or installation. Candidate mode refuses changed tracked
production paths (`scripts`, `config`, `rules`, `roles`, `templates`) or an
unavailable reference commit, so a migrated dispatcher cannot compare against
itself. The recorded Git object must be present in the reference checkout.

Fixture compilation and other test classes, including all 64 corpus cases,
still use the Python reference. Passing the adapter demonstrates parity for the
existing CLI scenarios; it does not prove every mode/input combination, native
execution of all corpus or filesystem unit cases, or interpreter absence.

Native tests must consume these vectors directly and preserve the remaining
canonical contracts. A Python-free disposable host must separately test the
installed checkout, symlink, and copy entry paths. Review absolute spawn paths,
dependency build scripts, and installed helpers; hiding Python on PATH alone is
insufficient. See the handoff for mode-specific acceptance and evidence limits.

## Updating and retiring the reference

When intended behavior changes, update the specification, provenance, and vectors
in the same reviewed change. Keep identified bug fixes explicit; do not silently
accept a candidate difference or drop an inconvenient case.

Once native coverage is authoritative, execute the fixtures without starting an
interpreter and remove the obsolete Python implementation and temporary Python
comparison driver together. Keep inert source-text fixtures needed for Python
project analysis. The inventory tool's Rust tests are not native context tests,
and this preparation change makes no claim of a completed production cutover.
