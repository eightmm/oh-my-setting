# Model routing and capability

Use model names as a runtime contract, not as marketing labels.

1. Run `oms models` to inspect cached provider catalogs and per-model effort
   scales before choosing an explicit model. It does not probe unless passed
   `--refresh`. Run `oms model-doctor` before claiming a multi-model quorum.
2. Use `oms model-doctor --strict-diversity` for a high-risk review gate. Missing
   or CLI-incompatible providers do not count. Unknown or fewer than two usable
   model families fail in strict mode.
3. Use `--live-models` only when account-visible availability must be checked.
   A provider without a stable model-list command remains explicitly unverified;
   never invent availability from documentation alone.
4. Distinguish provider identity from model-family identity. An Antigravity route
   using Claude is not independent from a Claude Code route using Anthropic.
5. Prefer exact model IDs for reproducible work. Pass `--model` and optional
   `--effort`; no model selects the provider default. Legacy model-tier inputs
   warn and are ignored. Record the selected model and reasoning effort in the
   artifact index.
6. An explicit model falls back only to the provider default unless an explicit
   fallback is named. Catalog-backed safeguard recovery uses distinct cached
   entries. Authentication, permission, context-length, or verification
   failures are not capacity failures.
7. The owning agent remains responsible for admission and mechanical
   verification. Model agreement is evidence, not a pass condition.
