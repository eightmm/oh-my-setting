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
child boundary and invalid-limit exit.

Run the nearest registered suite, not a new CI lane:

```bash
bash tests/context-core-smoke.sh
```

The standalone Python invocation tests the module only; it is not the full
runtime integration test. No real model, provider login, GPU or Slurm job is
needed by either path. Roll back the scoped commit to remove the command; no
stored records need migration.
