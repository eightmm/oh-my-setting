# Delegation, Artifacts, and Landing

Use one explicit provider entrypoint:

```bash
oms agent-run --to claude --repo . --mode read --prompt "Assess this plan."
oms agent-run --to codex --repo . --mode write --prompt "Implement the bounded fix."
```

Read mode cannot edit the repo. Write mode uses an isolated worktree and
returns an artifact log plus patch; workers cannot commit or push. Outbound
context is scanned and sensitive-looking content blocks the call.

Use `--no-memory`, `--no-task`, or `--no-ml-context` to omit prompt layers.
Use `--export-only` for read calls/reviews when another provider must not be
called directly, then import the answer with `oms import-agent-result`.

Artifacts are indexed under `.oms/artifacts/index.jsonl`. Inspect with:

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
