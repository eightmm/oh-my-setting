# Saved context inspection

`oms runtime context-inspect` reads an existing OMS context manifest. It does
not compile a new pack, open the bundle, contact a provider, or write state.
By default it does not open listed source files; `--verify-current` reads them
to compare their full-source digests. It is an agent-facing read surface for
[NF-03](CONTROL-PLANE-UX-AND-FEATURES.md#nf-03--context-budget-inspector), not a new
panel or a replacement context compiler.

```bash
oms runtime context-inspect .oms/runtime/context/MANIFEST.json --pretty
oms runtime context-inspect before.json --compare after.json --limit 10 --pretty
oms runtime context-inspect before.json --verify-current --pretty
```

Use the exact `manifest_path` returned by `oms runtime context`, or explicitly
choose another saved manifest in the selected `--repo`. There is no implicit
latest-file discovery. Both operands of `--compare` must be in that repository.

## Reading the result

The schema-1 `oms-context-inspection` object reports recorded budget, used and
remaining bytes, selected source payload and framing bytes, source rows sorted
by size, omission reasons and required-context debt. `--limit` is 1–100
(default 10); each bounded list has `total`, `items`, and `omitted`.

`bytes_selected` includes any truncation marker the original compiler inserted;
framing bytes are the recorded bundle total minus these payload sizes. This is
not a tokenizer, API bill, account quota, or measurement of the provider's full
context. Repeated full-source hashes identify candidates for inspection only:
two truncated selections of the same source need not repeat the same bytes.
Nothing is removed from a pack automatically.

`validation: recorded-metadata-only`, `sources_current: null` and
`bundle_verified: false` describe the default. With `--verify-current`,
`sources_current` counts unchanged, changed, missing and unreadable sources,
and bounds the changed and missing path lists using `--limit`. This is a
read-time comparison, not authenticity, approval or acceptance. The flag
cannot be combined with `--compare`; comparisons do not verify source files.
The manifest's `reported_sufficient` is preserved under `debt`, not promoted.

The `oms-context-comparison` object reports added/removed selected paths,
changed source metadata, byte/debt deltas and differences in recorded phase,
targets and contract-state digest. A missing contract digest makes recorded
scope unknown. Matching recorded scope is not proof of identical repositories,
current tasks or valid experiment controls. No winning run, quality improvement,
token saving or billing saving is inferred. Existing `experiment compare` and
`benchmark compare` retain their separate contracts.

## Human-readable report

Use `--format text` for a plain, noninteractive report. JSON remains the default
on both terminals and pipes; `--format json --pretty` retains the existing JSON
contract. `--format text --pretty` is rejected before reading a manifest.

```bash
oms runtime context-inspect before.json --format text
oms runtime context-inspect before.json --verify-current --format text
oms runtime context-inspect before.json --compare after.json --format text
```

The report puts recorded required-context debt and current-source observations
before byte totals. Truncated required paths are explicitly a subset of the
missing/incomplete list, not additional debt. A query without `--verify-current`
says **not checked**, never unchanged. When requested, the read-time hash counts
and bounded changed/missing paths are shown; this is not an atomic snapshot or
bundle verification. Unreadable files remain unreadable rather than becoming 0
changes or a success verdict. The renderer itself opens no files.

Every list, including paths inside repeated-hash groups, shows total, shown and
not-shown counts from the existing projection. Omission size `null` is printed
as `unreported`, not zero. Comparisons label their direction **right minus left**
and display recorded-scope differences or missing identity before signed byte
and debt deltas. Zero deltas do not prove identical packs or a better result.

Text output uses headings and indented rows, not padded columns, terminal modes,
colors or control sequences. Paths/reasons remain Unicode; display controls and
line separators are neutralized. Labels longer than 160 code points have an
explicit `[label shortened]` marker; JSON keeps the full metadata. This is a
read-only report, not a clickable task selector or automatic panel integration.
Terminal wrapping is left to the terminal; no fixed cell-width guarantee is made.
Metadata can still be private. No summary is sent to a model or remote service.

## Bounds, authority and compatibility

Manifest reads require an own-UID, single-link regular file inside the selected
repository. Descriptor-relative opens refuse symlinks in path
components, parent traversal, special files, files over 4 MiB and detected
in-place changes while reading. A manifest list has at most 4,096 rows and path
traversal at most 64 components. JSON duplicate keys, non-finite numbers,
unsupported schema and inconsistent byte/debt data fail closed. Verified source
reads also refuse symlinks and special files, cap each file at 2 MiB, and count
unsafe, oversized or changing files as unreadable.

This first version supports repo-relative source labels. Manifests with external
or invalid required-path labels are refused rather than normalized into a
misleading local source. Source paths are opened only with `--verify-current`.
Unknown additional manifest fields are ignored, not echoed. Display reasons are
bounded and terminal controls are neutralized; this is not a secret-redaction
service. Treat metadata names/reasons as private and do not publish them blindly.

A marked harness child may inspect or compare; its existing `context` compilation
write prohibition is unchanged. No install setting, model route, permission,
network listener, persistent cache or existing schema is modified. The new
runtime subcommand and its output kinds are additive public interfaces.

Exit 0 means a valid metadata query, even when it reports context debt or scope
mismatch. Exit 2 means usage/unsupported/invalid input; Ctrl-C is 130 through the
runtime entrypoint. Exit 0 is never a verification pass. The original compiler's
exit 3 for insufficient context remains unchanged.

## Verification

`tests/test_context_inspect.py` exercises the actual stdlib implementation using
synthetic manifests and real temporary files (including symlink/hardlink/FIFO,
malformed JSON, concurrent change, list caps and metadata-only comparison).
The existing `tests/context-core-smoke.sh` invokes it with `--cli`, adding an
end-to-end compiler → saved manifest → inspector/compare test through
`scripts/runtime.sh`. That test removes the listed source and bundle before
inspection and checks repository bytes remain unchanged. It also checks the
child boundary and invalid-limit exit. Text/report assertions extend this same
suite: ordering, subset counts, bounded lists, Unicode/control handling, unknown
identity and delta direction. The CLI case also compares default/explicit JSON,
text inspection/source verification/comparison, child access and invalid format
combinations without changing repository bytes.

Run the nearest registered suite, not a new CI lane:

```bash
bash tests/context-core-smoke.sh
```

The standalone Python invocation tests the module only; it is not the full
runtime integration test. No real model, provider login, GPU or Slurm job is
needed by either path. Roll back the scoped commit to remove the command; no
stored records need migration.
