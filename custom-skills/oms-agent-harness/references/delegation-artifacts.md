# Delegation, Artifacts, and Landing

Use one explicit provider entrypoint:

```bash
oms agent-run --to claude --repo . --mode read --prompt "Assess this plan."
oms agent-run --to codex --repo . --mode write --prompt "Implement the bounded fix."
```

The harness leaves model choice to the provider unless the caller passes an
explicit `--model`. Use `oms models` to inspect cached provider catalogs before
class, `--model` for an exact model, `--fallback-model` for an explicit backup,
or `--no-model-fallback` to disable fallback. Only a recognized capacity error
may retry, at most once; a write attempt that changed its worktree is never
retried, including changes to ignored files. Antigravity read fallback receives
a freshly recreated isolation worktree. Provider/class mappings can be overridden with variables such as
`OMS_MODEL_CODEX_FAST`, `OMS_MODEL_CLAUDE_BALANCED`, and
`OMS_MODEL_ANTIGRAVITY_DEEP`.

Reasoning effort is passed only when named with `--reasoning-effort` and is
validated against the selected model's cached scale. Use `--reasoning-effort` for
Codex or Claude. Antigravity exposes effort through its model variants rather
than a separate flag, so select an explicit Low/Medium/High model there. If a
custom variant does not identify its effort, provenance leaves effort unset.

Read mode cannot edit the repo. Write mode uses an isolated worktree and
returns an artifact log plus patch; workers cannot commit or push. Outbound
context is scanned and sensitive-looking content blocks the call.

Use `--no-memory`, `--no-task`, or `--no-ml-context` to omit prompt layers.
Use `--export-only` for read calls/reviews when another provider must not be
called directly; the export records the validated model route. Then import the
answer with `oms artifact-index import`.

Artifacts are indexed under `.oms/artifacts/index.jsonl`, including selected
model class/model, reasoning effort, and fallback outcome. Inspect with:

```bash
oms artifact-index --repo . latest
oms artifact-index --repo . unresolved
oms artifact-index --repo . validate
```

Resolve a failed outcome explicitly; never assume a sibling provider success
resolves it. Use `migrate` and `gc` rather than editing JSONL manually.

Before landing a patch:

1. Read the worker log and patch.
2. Run `oms patch-admit --patch <path>` for a review-only verdict, or
   `oms patch-land --patch <path>` to admit and apply to a clean main tree.
3. For a coupled plan task, prefer `oms patch-land --plan-task ID`.
4. Rerun the project check after landing.

Admission verifies applicability, syntax, verifier integrity, path scope, and
the stored verification contract. A rejected patch remains rejected until the
cause changes; consult the fail ledger before retrying.

## What worker-authority detection does not cover

`peer-delegate` compares the primary repo's tracked state, untracked and ignored
files (by stat), local git config, remotes, refs, object-store/worktree/submodule
metadata, and hooks around each worker run, and fails the run when one moves.
Shared `.oms` state is held to its append-only contract rather than compared for
equality: appending is what workers are given `OMS_STATE_REPO` for, but rewriting
existing rows, truncating a ledger, or deleting a state file is a violation.
It is detection, not a sandbox, and these stay outside it:

- A write undone before the worker exits. Before/after comparison cannot see a
  change that was reverted in between.
- Anything the worker reads: inherited tokens, ssh agents, credentials in the
  environment. Reads leave no trace to compare.
- Anything outside the repository — `$HOME`, `/tmp`, a background process that
  outlives the run.
- Content inside an ignored directory beyond `OMS_WORKER_GUARD_MAX_FILES`
  entries; a truncated scan reports itself rather than pretending to be complete.

Closing those needs process isolation, which a bash harness does not have. Treat
a clean result as "no repository surface moved", not as "the worker was safe".
