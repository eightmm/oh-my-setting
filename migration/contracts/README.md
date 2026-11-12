# Migration contract vectors

These are frozen examples for the next implementation slice, not a claim that
the Rust runtime already implements the corresponding behavior.

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
reference module. The workspace's current Rust tests exercise the inventory
tool; they do not execute these projection cases yet. M1 must wire these
vectors into the Rust implementation's tests and extend the existing
[`test_context_inspect.py`](../../tests/test_context_inspect.py) coverage for
descriptor-relative reads, symlinks, ownership, duplicate JSON keys, byte
limits and changed files. Five examples do not replace that coverage.

When intended behavior changes, update the specification and vectors in the
same reviewed change. During a port, compare both implementations against the
same accepted contract and preserve identified bug fixes explicitly. Once the
port is authoritative, Rust runs these fixtures without starting an interpreter;
the reference Python code and temporary comparison runner are removed.
