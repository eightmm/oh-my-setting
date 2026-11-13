# Rust context inspection handoff

This change prepares compatibility evidence for the first native context slice.
It does not implement a Rust command, switch the installed dispatcher, or prove
that an OMS execution path is Python-free. The governing completion boundary is
[Complete Rust migration](RUST-MIGRATION.md).

## Ownership and comparison baseline

The implementation agent owns the Rust packages, native command routing, and
production lifecycle changes. This verification work owns the context contract
data, the candidate adapter in the existing context test, and this handoff.
Coordinate changes to shared files before editing them; do not independently
replace the same production function in two branches.

The frozen reference is commit
`74f87f654a314961420be2aa8ab6dfc1999151e1`. Each contract file records its source
path, SHA-256, entry point, and default arguments. Check those fields before
replaying it. The five original inspection seeds remain part of the expanded
corpus. Do not regenerate expected results from the candidate under test.

Record a newer reference explicitly when an accepted behavior change requires
it. A migration should preserve that change in both the specification and the
fixtures, rather than silently accepting either implementation's output.

## Public coverage and implementation slices

The public spelling is `oms runtime context-inspect`. Its full contract includes
more than the pure `inspect_manifest` function. The actual option and dispatch
sources are [cli_parser.py](../scripts/lib/oms_runtime/cli_parser.py) and
[cli.py](../scripts/lib/oms_runtime/cli.py).

| Slice | Public surface | Reference and acceptance evidence |
|---|---|---|
| M1a: manifest projection | `MANIFEST`, `--limit`, JSON projection | `inspect_manifest`; inspection vectors; metadata-only assertions |
| M1b: comparison and file access | `MANIFEST --compare OTHER`, `--verify-current` | `compare_manifests`, `read_manifest`, `inspect_files`; comparison/read vectors and canonical filesystem tests |
| M1c: session and report modes | `--injections`, `--session-hash`, `--session`, `--timeline`, `--format text`, `--pretty` | `context_session`, `context_report`; existing session/report tests and public CLI assertions |
| Installed entry point | PATH `oms` in checkout, symlink, and copy installations | Native selection before interpreter lookup; installation and Python-free evidence |

These are review boundaries, not new user commands or a claim of implementation
status. Track each mode as reference-only, native-comparable, or native-retired
in the implementation PR. A successful M1a comparison cannot retire M1c or the
whole `context-inspect` command.

## Contracts to preserve

### Metadata and comparison

- Budget, selected payload, framing, and omissions are bytes. They do not prove
  token savings, billing savings, provider context completeness, or quality.
- Metadata-only inspection does not open selected sources or the bundle. Keep
  `validation`, `sources_current: null`, `bundle_verified: false`, and the
  qualification text; absent source files do not invalidate saved metadata.
- Preserve required-source debt and truncation accounting, including marker
  bytes, zero-length sources, and repeated full-source digest qualifications.
- Integers reject Booleans and values outside `0..2^53-1`; the schema must be
  integer `1`, the budget is positive, and `limit` is an integer in `1..100`.
  Preserve each field's own validation and error precedence.
- Preserve list order, bounded `{total, items, omitted}` groups, optional-field
  presence, and distinctions between absent, null, empty, and unknown.
- Comparison is right minus left. Equal sources or bundle hashes do not repair
  missing contract or phase-rules identity. Keep scope mismatch reasons and the
  legacy projection shape when neither manifest records phase rules.
- Raw JSON decoding must reject duplicate keys, non-finite constants, malformed
  UTF-8, and malformed JSON. A decoded value still needs the entry point's own
  semantic validation; successful `read_manifest` alone is not a valid pack.

### Files, errors, and authority

- Manifest reads are bounded, descriptor-relative, and reject traversal and
  symlink components. Preserve owned regular-file, single-link, byte-limit,
  and change-during-read checks from
  [context_inspect.py](../scripts/lib/oms_runtime/context_inspect.py).
- Current-source verification preserves `unchanged`, `changed`, `missing`, and
  `unreadable` separately. Do not substitute an empty success for an unsafe or
  failed read. Its checks differ from manifest ownership/link checks; port the
  actual function contract rather than imposing one generic file policy.
- Keep inspection read-only, including comparison, text rendering, and error
  paths. Do not compile a missing bundle, repair state, rerun a provider, or
  write an inspection cache as an implicit convenience.
- Keep `InspectionError` messages from the vectors and public error framing,
  stdout/stderr separation, and exit status from the canonical CLI. Do not
  include raw input, transcript contents, or private paths in diagnostic text.
- Validate incompatible options before reading the manifest, as the existing
  CLI does. `--pretty` applies only to JSON; text is for manifest inspection or
  comparison; compare and current-source verification cannot be combined.
- Exactly one of a manifest, `--injections`, or `--session` selects a mode.
  `--session-hash` requires injections; `--timeline` requires a session. The
  timeline has its own step bound; `--limit` does not replace that bound.
- Preserve [child authority](../scripts/lib/oms_runtime/child_policy.py): a
  marked harness child may inspect manifests and injection metadata, but may
  not inspect a transcript or timeline, or invoke context compilation.
- Compare only immutable captured inputs. Never fall back automatically to a
  legacy operation after a native operation might have performed a side effect.
  Reconcile durable state before any further execution of an uncertain action.

## Using the candidate adapter

Keep one canonical suite in
[`tests/test_context_inspect.py`](../tests/test_context_inspect.py). Run the
reference before attributing a difference to the native implementation:

```bash
python3 tests/test_context_inspect.py --cli
python3 tests/test_context_inspect.py --cli --candidate-oms /absolute/path/to/oms
```

The candidate argument names an existing executable file, not a shell command.
The adapter makes the path absolute without resolving symlinks, so a supplied
installation link remains the actual entry point. It invokes that file as
`runtime --repo DIR context-inspect ...`. Use `--cli` with it; do not substitute
a Rust leaf binary with a different argument contract.

Run this suite from the separate reference checkout and pass the executable from
the implementation checkout or installation. Candidate mode verifies that tracked
`scripts/`, `config/`, `rules/`, `roles/`, and `templates/` still match the frozen
reference commit; a missing Git object, failed check, or changed production path
is an error. This prevents a migrated dispatcher becoming its own comparison
oracle. Fetch the recorded commit into the reference checkout when needed; do
not relax the check to run from a modified implementation tree.

Fixture compilation continues through the existing reference `context` command.
For each existing public `context-inspect` scenario, the adapter executes the
repository's public `scripts/oms runtime` reference first, including dispatcher
error hints, and checks that fixture file contents are unchanged. It
then executes the candidate with the same arguments and environment. It checks
fixture file contents again and compares exit code, complete decoded stdout,
and complete decoded stderr exactly. Subprocess text decoding includes newline
handling; this is not raw-byte stream comparison or a process trace.

Other test classes, including the 64 frozen corpus cases, still exercise the
Python reference. This keeps compiler work outside the first inspection slice
and reuses the existing CLI scenarios without creating another independent suite.
A passing candidate run establishes parity for those scenarios; it does not
prove every mode/input combination, native execution of the corpus or filesystem
unit cases, or Python absence. Consume the frozen vectors directly in native
tests as described in the [contract guide](../migration/contracts/README.md).
Extend the nearest existing case when a real missing contract is identified.

For a partial implementation, report the exact modes that fail or remain on the
reference. Do not add automatic fallback or suppress unsupported candidate
modes to turn the report green. Complete-output parity, public process costs,
and Python-free operation are distinct evidence requirements.

## Installed entry point acceptance

The current [dispatcher](../scripts/oms) has two relevant interpreter boundaries.
It checks Python before ordinary tool execution, and a copied dispatcher with
no adjacent scripts reads its install receipt through Python to find the source
checkout. [runtime.sh](../scripts/runtime.sh) also executes the Python core.
Skipping only the late preflight does not eliminate the earlier copy bootstrap.

For each native slice, perform the following on the same candidate artifact:

1. Record the source commit, compiler/profile, target, artifact hash, and selected
   native modes. Install into disposable ownership-isolated locations.
2. Exercise checkout execution, symlink installation, and copy installation
   separately. Invoke the installed PATH entry as the user would, rather than
   only calling a binary under the build directory.
3. On a disposable host without Python, run the migrated manifest, compare,
   current-source, and supported session/report cases through those entries.
   Use inert fixtures prepared in advance; do not run the Python adapter there.
4. Confirm the expected stdout/stderr/exit status and unchanged fixture state.
   Include invalid input and option combinations, not only a successful pack.
5. Inspect dependency build scripts, installed helpers, and interpreter spawn
   paths. PATH sentinels and process traces are useful additional signals, but
   cannot alone exclude absolute-path or embedded-interpreter execution.
6. Retain existing copy/symlink ownership, update, repair, rollback, and uninstall
   guarantees for changed installation paths. Do not remove a user's separately
   owned interpreter or claim unsupported platforms from a Linux-only run.

This proves selected native entry points only. The final migration additionally
requires Python-free source builds, tests, evaluation, packaging, hooks, MCP,
providers, state recovery, and lifecycle operations under the migration plan.
An independently installed provider or a user's requested project command may
have its own Python requirement; record that separately from OMS requirements.

## Admission, retirement, and handoff evidence

Before marking a mode native-retired, record its reference revision, supported
options, byte/error contracts, native test results, remaining bridges, and the
legacy files that can now be deleted. Keep timing evidence tied to equivalent
inputs, accepted outputs, build profile, repetitions, and cache conditions.

Use affected checks and the existing CI lanes. Do not infer current success from
another commit's run. A compiler failure, unavailable toolchain, unsupported mode,
or test that was not executed remains explicit in the PR's verification section.
No Rust speedup or clean-host result is established by this preparation work.

Retirement removes the obsolete Python implementation and temporary candidate
comparison driver once equivalent native coverage is authoritative. Keep inert
Python source fixtures needed to test user projects. Lower the committed Python
inventory with the actual removal; preserving an obsolete upper bound is not a
ratchet. The native implementation agent owns that production cutover, and the
verification work supplies reviewable evidence for it.
