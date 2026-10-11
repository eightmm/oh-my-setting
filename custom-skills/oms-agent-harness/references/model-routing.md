# Model routing and capability

Treat model selection as a runtime contract.

Provider transport and model family are different identities. Load
[provider-routing.md](provider-routing.md) when selecting an installed optional
agent, a Grok/GLM route, or a custom adapter.

## Separate judgment from execution

The parent owns the plan, unresolved tradeoffs, patch admission and final
acceptance. A role describes work; it does not select a model or authorize a
call. Keep the current registry and explicit user model choices.

Before an authorized delegation, choose from repository evidence:

| Work | Handling |
|---|---|
| File existence, known lookup, syntax or a known small fix | Run the local tool directly. |
| Exploration or documentation research | Bound a read-only brief to the missing fact and require source references; reuse existing findings. |
| Implementation with known paths, constraints and verifier | Use the routine worker route when substantial enough to justify a worker. |
| Bounded implementation needing further investigation | Keep the standard worker route; return unresolved decisions to the parent. |
| Architecture, scientific assumptions or security tradeoffs | Resolve the uncertainty in the parent before assigning implementation. |

Do not call a classifier to choose a tool, create three workers because three
roles exist, or replay the whole conversation. See [roles.md](roles.md) for
brief and return requirements.

Review at decision points: before committing to a material unresolved plan,
after the same unresolved failure recurs, and before accepting substantial work.
First inspect source and existing evidence. A repeat needs a changed hypothesis
and a discriminating probe; completion needs the requested scope matched to
fresh verification. Consult a peer only when authorized and a specific missing
judgment could change the next action. These checkpoints do not enable a
background advisor or require another model call for routine completion.

## Existing router contract

1. Run `oms models` for cached catalogs and per-model effort scales. Use
   `--refresh` only when a live probe is needed.
2. No `--model` means provider default — except for a write worker
   (`peer-delegate`, `plan-run`, autopilot), which takes its seeded role rank
   (the Codex Sol family, the seeded Claude worker, the medium Gemini line).
   Codex Astra, Sol, and Luna each use the newest numeric version in the local
   known catalog; their cost order remains Astra, Sol, Luna across versions.
   Standard Codex workers use Sol when available, routine work uses Luna when
   available, and recognized capacity recovery tries cheaper ranks first.
   Missing families are not invented. This is a stable worker preset; it is
   not computed relative to an
   account-specific provider default. Judging calls keep that provider
   default. Recovery tries cheaper candidates before higher ones. Other
   providers use the registry's generation seed (`oms_provider_price_order`);
   an unseeded generation routes as the provider default and says so.
   `OMS_ROLE_ROUTING=0` switches this off.
   An effective `--model NAME` is exact and never switches to a catalog entry
   or provider default. For assigned plan tasks, the reviewed assignment owns
   that effective route; run-wide model options are defaults for unassigned tasks.
   For a substantial routine task with known paths, constraints and a verifier,
   pass `peer-delegate --workload routine`: select the lowest seeded routable
   rank, not the standard worker route. The parent decides from repository
   evidence; no extra classifier call or prompt-keyword heuristic runs.
   `--model` still wins, `OMS_ROLE_ROUTING=0` still disables presets, and an
   unknown generation still uses provider default. This is not a spending cap:
   existing bounded recovery may use a higher rank. Unclassified
   plan-run/autopilot workers keep the standard route. A reviewed task
   `assignment` can select routine work or an exact model per transport;
   [autonomy-loop.md](autonomy-loop.md) owns its schema and precedence.
3. `--fallback-model NAME` is an opt-in, one-shot fallback used only for a
   recognized capacity error. A write attempt that changed its worktree is
   never retried.
4. Work locally unless the user or applicable instructions explicitly request
   delegation. When authorized, use an OMS delegate for bounded writes. Keep
   tiny known edits local; retain ambiguous, scientific, security-sensitive
   and architectural judgment with the parent. A routine brief needs relevant
   context and success criteria, not the whole conversation. Native subagents
   are outside this shell router: choose their model explicitly when supported.
   Provider choice is separate: use an already-authorized installed transport,
   never install, log in, fan out or switch companies merely to find a cheap model.
5. An unpinned provider-default route may use a bounded distinct catalog model
   when a model safeguard or unavailable-name error explicitly permits
   recovery. Policy, auth, permission, context, and verification failures do
   not route around the result.
   Only the routable set is a candidate: Codex's known Astra, Sol, and Luna
   families use their newest numeric version; other providers keep the
   provider-family newest-generation rule (`oms models` lists the rest apart).
   Unknown or malformed Codex families are not automatic routes. A previous
   generation or a model another vendor re-hosts through the CLI runs only
   when named with `--model`, with a warning; it is never chosen, and
   `model-doctor` warns when a configured default has fallen outside the set.
6. Pass `--reasoning-effort` only after checking the selected model's cached
   scale. Supported values are `auto`, `low`, `medium`, `high`, `xhigh`, `max`,
   and `ultra`; each provider accepts only its reported subset.
   `auto` inherits the provider's configured default, except that a Codex
   read-only seat (council, review, consult) runs at `medium`: on a seeded
   five-defect audit (gpt-6-sol, three runs each) it matched `high` recall
   with 60% less input, 42% fewer tool calls and 47% less time. Pass
   `--reasoning-effort high` for high-risk judgment; write workers keep the
   configured default. Claude seats keep their default: on the same audit
   Fable at `medium` found 9/15 against 11/15 for only 17% less cost, and
   Opus 5.5 showed no saving.
7. For high-risk review, run `oms model-doctor --strict-diversity`. Provider
   identity is not model-family independence: Antigravity using Claude and
   Claude Code using Anthropic remain one family.
8. The owner still admits patches and runs mechanical verification. Agreement
   is evidence, not a pass condition.

`autopilot --collaboration auto` narrows this generic router to GPT-6 and Claude:
Codex defaults are pinned before catalog recovery, older explicit models and
fallbacks are rejected, and reviewed Codex assignments require an exact Astra,
Sol or Luna model. Claude follows the existing registry. This restriction is
scoped to that campaign; it does not configure native Codex app subagents.
