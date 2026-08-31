# Model routing and capability

Treat model selection as a runtime contract.

Provider transport and model family are different identities. Load
[provider-routing.md](provider-routing.md) when selecting an installed optional
agent, a Grok/GLM route, or a custom adapter.

1. Run `oms models` for cached catalogs and per-model effort scales. Use
   `--refresh` only when a live probe is needed.
2. No `--model` means provider default — except for a write worker
   (`peer-delegate`, `plan-run`, autopilot), which takes the second price
   rank of the routable set (`gpt-5.6-terra`, `opus`, the medium Gemini
   line): implementation runs one rank below the judging seats, which keep
   the provider default. The order within a generation is the registry's
   one seed (`oms_provider_price_order`); an unseeded generation routes as
   the provider default and says so. `OMS_ROLE_ROUTING=0` switches this off.
   `--model NAME` is exact and never switches to a catalog entry or provider
   default.
3. `--fallback-model NAME` is an opt-in, one-shot fallback used only for a
   recognized capacity error. A write attempt that changed its worktree is
   never retried.
4. An unpinned provider-default route may use a bounded distinct catalog model
   when a model safeguard or unavailable-name error explicitly permits
   recovery. Policy, auth, permission, context, and verification failures do
   not route around the result.
   Only the routable set is a candidate: the provider's own family at its
   newest generation (`oms models` lists the rest apart). A previous
   generation or a model another vendor re-hosts through the CLI runs only
   when named with `--model`, with a warning; it is never chosen, and
   `model-doctor` warns when a configured default has fallen outside the set.
5. Pass `--reasoning-effort` only after checking the selected model's cached
   scale. Supported values are `auto`, `low`, `medium`, `high`, `xhigh`, `max`,
   and `ultra`; each provider accepts only its reported subset.
6. For high-risk review, run `oms model-doctor --strict-diversity`. Provider
   identity is not model-family independence: Antigravity using Claude and
   Claude Code using Anthropic remain one family.
7. The owner still admits patches and runs mechanical verification. Agreement
   is evidence, not a pass condition.
