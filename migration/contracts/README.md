# Migration contract vectors

These frozen examples are replayed by the native `oms-context` tests. They do
not establish installed CLI, lifecycle or complete migration parity.

## Context manifest projection

[`context-inspect-v1.json`](context-inspect-v1.json) records five cases for
`inspect_manifest(..., limit=10)` from the source commit and SHA-256 in its
`reference` object:

- a complete source and its framing-byte budget;
- an empty selection;
- a partial selection with missing-required debt;
- a Boolean schema value, which must not be accepted as integer schema 1;
- a duplicate selected path, which must be rejected.

Each case contains either the exact JSON-compatible projection in `expected`
or the `InspectionError` message in `expected_error`. Object key order is not
part of this projection contract. Byte-sensitive hashes, command output and
error exit codes have their own contracts and must not be inferred from these
JSON values.

The preparation change generated and replayed these cases against the pinned
reference module. The native `oms-context` suite now executes all five vectors
alongside strict JSON, projection, comparison, encoding and bounds regressions.
The separate `oms-safe-fs` suite exercises descriptor-relative reads, symlinks,
ownership, hard links and change detection. M1 still requires public CLI and
installed-selection coverage equivalent to
[`test_context_inspect.py`](../../tests/test_context_inspect.py).
Five examples do not replace that coverage.

When intended behavior changes, update the specification and vectors in the
same reviewed change. During a port, compare both implementations against the
same accepted contract and preserve identified bug fixes explicitly. Once the
port is authoritative, Rust runs these fixtures without starting an interpreter;
the reference Python code and temporary comparison runner are removed.
