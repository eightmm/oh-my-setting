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

## Selection-policy provenance

A manifest may record `phase_rules_sha256`, the SHA-256 of the rules used to
select its context. Inspection preserves that digest without opening the rules
file. An explicitly recorded value must be a lower-case 64-hex digest; null,
empty or malformed values are rejected instead of silently ignored.

When either comparison operand records this field, `phase_rules` shows the
left/right digests and `same-recorded-digest`, `different-recorded-digest`, or
`unknown`. Different digests add `different-phase-rules` to the scope mismatch
reasons. One missing digest adds `phase-rules-identity-unavailable`. Both make
`recorded_scope_matches` false even when the selected files and bytes match.

Two legacy operands with no policy field keep their existing output shape and
scope comparison. This is compatibility, not proof that their policies were
the same. No current rule file, source or provider is opened by this comparison;
matching recorded digests are not current freshness or authority evidence.

## Bounds, authority and compatibility

### Physical-call context attribution

Native provider calls prepare a metadata-only manifest at
`.oms/runtime/context/calls/CALL_ID.json`, after the canonical physical New
registration. Inspect that exact path with the existing `context-inspect`
command; JSON and `--format text` support `oms-context-request-inspection`.
The record connects the canonical `operation_id` and `call_id` to the prepared
primary prompt's UTF-8 byte count, request digest and renderer version. These
identifiers are correlation data, never launch or controller credentials.

Known ranges are assigned to `contract`, `task`, `retrieved`, `history`,
`tool_schema`, `tool_result` or `framing`, with a selected-byte digest, an exact
serialized-prefix digest and source size/range when observed. No range is counted
twice. Up to 4096 spans are retained; remaining bytes stay `unattributed_bytes`.
The shared compiler also adds `request_attribution` to new schema-1 manifests,
including head/tail/middle source ranges and phase-policy identity. In this new
attribution, truncation markers are framing; the existing `bytes_selected` and
budget fields retain their prior definition, which includes markers in payload.
The inspector bounds span lists with the existing `--limit`.

`dispatch: prepared` proves preparation only; `executor-returned` records the
executor boundary, and `executor-error` its error return. None proves that the
provider or model consumed the prompt. An interrupted or failed metadata update
can leave the prepared record even after execution. `provider_delivery`, layer
tokens, tokenizer, cache hit and provider-added context bytes remain null.
Compiled-manifest and phase-policy links are null when the actual producer has
no checked link; inspection does not infer them by matching text or searching
for a recent manifest. With `peer-delegate --context-manifest`, the native producer
compiles once from the pristine detached HEAD with the existing implementation
phase and 65,536-byte budget. Checked pack targets and their ancestor contracts
use the native graph dependency adapter; stale or linked targets are skipped.
The exact UTF-8 bundle is retained in initial and repair prompts, with a partial
bundle warning when required context is missing or truncated. The saved manifest
and phase-policy digests are bound to those actual prompt bytes and recorded on
each physical call; the delegation index retains the existing four context fields.
Snapshot-local state does not import the parent's uncommitted task or memory.

Compilation, manifest/bundle mismatch, unsupported parser discovery and outbound
validation fail before worker registration. Compilation failure cleans up only a
still-pristine owned snapshot; changed snapshots are preserved. Dry-run and
export-only keep their existing pre-worktree behavior. The installed adapter uses
the existing qualified graph helper and native private-group proof; standalone
adapters without them refuse sources that require those services. This wiring
does not implement automatic graph selection, plan-task claims or patch admission.

Fallback attempts retain separate call manifests under the same operation.
Usage, cumulative scope and late corrections remain exclusively in the canonical
`.oms/usage/receipts.jsonl` projection; the inspector reports that reference but
does not open or price it. No layer bytes are converted into tokens or added to
provider usage. Source text, prompts and raw tool output are absent from the new
metadata. Prefix hashes use incremental state, not repeated full-prefix scans.

Context recording uses one nonblocking canonical lock attempt and finite
metadata, with the launch fence checked around preparation. Optional evidence
failure does not change an execution's exit, completeness, fallback decision or
usage/owned-cleanup settlement. The returned update follows that settlement;
on failure the caller receives a static warning and missing evidence stays
unknown. Filesystem I/O itself is subject to the host's kernel. Per-call manifests
cannot be compared or source-verified as compiled packs; `--compare` and
`--verify-current` refuse those combinations. Legacy manifests retain their
existing inspection and comparison output.

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

## What filled a session

The same verb answers two questions a saved manifest cannot, each instead of a
`MANIFEST` operand. Exactly one of the three is given; `--compare` and
`--verify-current` belong to a manifest.

```bash
oms runtime context-inspect --injections --pretty
oms runtime context-inspect --injections --session-hash HASH --pretty
oms runtime context-inspect --session TRANSCRIPT.jsonl --pretty
```

`--injections` reads the injection ledger: every OMS hook that prints text for
the model appends one row to `.oms/hooks/events.jsonl` with `action: injection`,
the hook kind (`resume`, `prompt`, `session-start`, `panel`, `turn-guard`), the
UTF-8 `bytes` and `lines` it printed and, for a SessionStart, the reported
`source`. The row never carries the text. The `oms-context-injections` object
sums calls, bytes and the largest single injection by hook, by SessionStart
source and by session hash, and lists under `not_counted` what the ledger does
not see: shell-side state hints from the prompt router, fail-ledger advice,
syntax-guard findings, move-guard refusals and everything the provider loads
itself (rules, skills, tool output). Rows older than the hook ledger's
retention (`oms gc`) are gone.

`--session` reads one Claude Code JSONL transcript the person names and reports,
in the `oms-context-session` object, tool-result bytes by tool, the largest
single result, and repository files read more than once. Paths outside the
repository are counted, not named; prompts, commands, reasoning and result text
are never returned. Any other file reports `format: unknown`; Codex,
Antigravity and Pi transcripts are not read. The transcript is capped at
256 MiB and opened without following a final symlink. A marked harness child
may read the ledger but not a transcript.

The same report includes `async_context`: bytes observed in typed Claude
`attachment.async_hook_response` records whose `hookSpecificOutput` contains
nonempty `additionalContext`. Ordinary messages that mention those names are
not observations. `recorded_bytes` includes every matching row;
`identified_bytes` counts each unambiguous recorded session/UUID once (both
identities must be present), with
separate duplicate, conflicting, missing-identity and identity-limit counts.
At most 4096 identities are retained during this read. Neither attachment text,
hook names nor session/UUID values are returned or stored.

This distinguishes provider-recorded asynchronous context from hook stdout in
`--injections`. It does not join the two sources, attribute a response to OMS,
or establish that the model used it: `producer_attribution` and `model_use`
remain `unknown`. `not_observed` means the selected transcript contained no
supported nonempty attachment; synchronous hooks, unrecorded delivery and
other provider formats remain outside this measurement. The projection is
read-only and is recomputed from the bounded input, so repeated inspection
does not append cumulative counts to the hook ledger.

`--session TRANSCRIPT.jsonl --timeline` reads the same transcript as an
ordered list instead: one `oms-context-timeline` step per tool call with the
tool name, the recorded gap between the call row and its result row
(`duration_ms`, null when a timestamp is missing), an error flag from the
provider's own `is_error`, the byte size of the arguments and of the result, and
`repeat_of` when an earlier step made the identical call. `first_error` is the
first flagged result and `steps_after_first_error` counts what followed; it marks
where a failure first showed, not what caused it. Arguments, commands and
outputs are never returned; up to 200 steps are listed (`--limit` does not
apply), and a first error beyond that is still included.

`oms agent-events show --attempt ID --bundle [--json] [--transcript PATH]` gathers
this timeline with the other records OMS keeps for one attempt: its lifecycle
events and longest gap, the artifact-index rows that carry the attempt id (or
answer one of them through `parent_event_id`: route, brief digest, exit and
verify exit, result lint, review seat verdicts), and the fail-ledger rows stamped
inside the attempt's time window (the ledger has no attempt id, so the match is
only by time). The transcript is the path a panel participant recorded under
`.oms/hooks/panel-activity/`, or `--transcript`. Each fact names its source file;
nothing is copied wholesale or written. Every string passes the shared secret
scanner first, flagged strings are withheld and a scan that cannot run prints
nothing. It is observation, never verification or approval.

Both are bytes as recorded. They are not tokens, not a bill and not the
provider's current window: a provider may already have cleared or summarized
what a transcript still holds.

## What survives a compaction

A compaction replaces the conversation with the provider's summary. What OMS
puts back does not depend on that summary:

- **Re-read from `.oms` state on every SessionStart** (`startup`, `resume`,
  `clear`, `compact` alike): the `[oms resume]` block — active task goal, next
  step and verify command, the live plan and its acceptance, the newest handoff
  pointer, unresolved failures, and the opt-in memory section. The block is
  capped at 4096 bytes, below the size at which Claude Code replaces hook output
  with a file pointer.
- **Only when the SessionStart reports `source: compact`**, and only when there
  is a block to attach it to: one line saying the lines above came from state,
  not from the summary, and that OMS skills are invoked by name. Claude Code
  does not list skill descriptions again after `/compact`.
- **Panel mains only**: the `[oms panel] resumed after ...` line and the cached
  orientation, once per context generation.
- **Captured before the compaction**: the PreCompact handoff digest, which the
  resume block points to.

Not put back: scope beyond what the task packet states, orientation for
sessions that are not a panel main, and anything a hook printed earlier in the
conversation. Whether Codex, Antigravity or Pi fire SessionStart after their own
compaction was not established; where they do not, nothing is re-injected
there.

A project can also tell the summarizer what to keep. Claude Code documents a
"Compact instructions" section in `CLAUDE.md`; the project templates do not
carry one, because they sit at their word limits. A project that wants it adds,
outside the managed block:

```markdown
## Compact instructions
Keep the active OMS task id, its allowed and forbidden paths, the verify
command, and any gate still waiting on the owner.
```

Whether that section is honored when it arrives through an `@AGENTS.md` import
was not tested. Codex's `compact_prompt` setting was seen only in a sample
configuration, not in its reference; OMS does not write it.

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
