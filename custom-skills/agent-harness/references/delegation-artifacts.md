# Delegation, Artifacts, and Landing

Use one explicit provider entrypoint:

```bash
oms agent-run --to claude --repo . --mode read --prompt "Assess this plan."
oms agent-run --to codex --repo . --mode write --prompt "Implement the bounded fix."
```

The harness resolves `auto` to `fast`, `balanced`, or `deep` from the operation
and role, then maps that class to the provider CLI. Use `--model-class` to pin a
class, `--model` for an exact model, `--fallback-model` for an explicit backup,
or `--no-model-fallback` to disable fallback. Only a recognized capacity error
may retry, at most once; a write attempt that changed its worktree is never
retried, including changes to ignored files. Antigravity read fallback receives
a freshly recreated isolation worktree. Provider/class mappings can be overridden with variables such as
`OMS_MODEL_CODEX_FAST`, `OMS_MODEL_CLAUDE_BALANCED`, and
`OMS_MODEL_ANTIGRAVITY_DEEP`.

Reasoning effort follows the selected class by default: `fast=low`,
`balanced=medium`, and `deep=high`. Use `--reasoning-effort` to override it for
Codex or Claude. Antigravity exposes effort through its model variants rather
than a separate flag, so select an explicit Low/Medium/High model there. If a
custom variant does not identify its effort, provenance leaves effort unset.

Read mode cannot edit the repo. Write mode uses an isolated worktree and
returns an artifact log plus patch; workers cannot commit or push. Outbound
context is scanned and sensitive-looking content blocks the call.

Use `--no-memory`, `--no-task`, or `--no-ml-context` to omit prompt layers.
Use `--export-only` for read calls/reviews when another provider must not be
called directly; the export records the validated model route. Then import the
answer with `oms import-agent-result`.

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
