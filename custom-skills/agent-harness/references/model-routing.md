# Model routing and capability

Use model names as a runtime contract, not as marketing labels.

1. Run `oms model-doctor` before changing provider/model mappings or claiming a
   multi-model quorum. The default pass is local-only and checks installed CLI
   versions, required flags, resolved `fast`/`balanced`/`deep` routes, and model
   family diversity.
2. Use `oms model-doctor --strict-diversity` for a high-risk review gate. Missing
   or CLI-incompatible providers do not count. Unknown or fewer than two usable
   model families fail in strict mode.
3. Use `--live-models` only when account-visible availability must be checked.
   A provider without a stable model-list command remains explicitly unverified;
   never invent availability from documentation alone.
4. Distinguish provider identity from model-family identity. An Antigravity route
   using Claude is not independent from a Claude Code route using Anthropic.
5. Prefer exact model IDs for reproducible work. Override routes with
   `OMS_MODEL_<PROVIDER>_<CLASS>` or an explicit `--model`; record the selected
   model and reasoning effort in the artifact index.
6. A capacity fallback may lower one model class once. Write-mode fallback is
   forbidden after the isolated worktree changes. Authentication, permission,
   context-length, or verification failures are not capacity failures.
7. The owning agent remains responsible for admission and mechanical
   verification. Model agreement is evidence, not a pass condition.
