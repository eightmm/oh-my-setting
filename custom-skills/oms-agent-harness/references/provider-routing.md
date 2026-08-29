# Provider routing and adapters

Treat an **agent transport** and a **model family** as separate facts. `grok`
is an xAI Grok Build transport; `glm` is a Z.AI model family carried by a CLI
such as OpenCode, Cursor, Claude Code, or Qwen Code. Never invent a `glm`
provider from a model name.

## Discover and select

OMS recognizes these built-in transports when their executables are on PATH:

| OMS name | Executable | Read/write contract |
|---|---|---|
| `codex` | `codex` | native read-only/workspace-write sandbox |
| `claude` | `claude` | plan/acceptEdits permission modes |
| `antigravity` | `agy` | disposable read worktree; delegated write worktree |
| `cursor` | `cursor-agent` | ask/force with sandbox enabled |
| `grok` | `grok` | explicit allow/deny policy and strict/workspace sandbox |
| `gemini` | `gemini` | plan/yolo approval mode with sandbox |
| `qwen` | `qwen` | safe plan/yolo mode with sandbox |
| `opencode` | `opencode` | plan/build agent in an OMS-owned worktree |
| `deepseek` | `dsh` | headless + read-only/workspace-write permission preset |
| `vibe` | `vibe` | invocation-only worktree trust; plan/accept-edits with an exact tool allowlist |
| `pi` | `pi` | ephemeral print mode; project resources off; read/edit tool allowlists |
| `copilot` | `copilot` | programmatic mode with MCP, shell, URL, and memory writes denied |
| `droid` | `droid` | default read-only or bounded `--auto low` edits |
| `aider` | `aider` | ask/dry-run or code mode; commits and shell suggestions off |

Physical discovery only resolves an executable. Automatic routing additionally
requires a bounded `--version`/`--help` usability probe; a hanging or stale shim
stays visible as broken but never enters a provider pool. `oms models` does not
execute a CLI and reports `usable: null`; `--refresh` and `model-doctor` perform
the bounded probe. Documented local model catalogs run only on explicit refresh.
None of these probes logs in, installs, upgrades, changes configuration, or
starts an inference request.

```bash
oms models --providers auto
oms model-doctor --providers auto --json
oms consult --to grok --prompt 'Check this design assumption.'
oms consult --to opencode:model=zai/glm-4.7 --prompt 'Review this plan.'
oms consult --to deepseek --prompt 'Review the ownership boundary.'
oms consult --to aider:model=deepseek/deepseek-chat --prompt 'Review this plan.'
oms agent-run --to cursor --mode write --prompt 'Implement the reviewed change.'
```

Use `--providers all` only for diagnostics that should also show absent
built-ins. The default `peer-ask` and `peer-review` council remains the stable
Codex/Claude/Antigravity set, so installing another CLI never causes surprise
fan-out or spend. Name optional seats explicitly; `consult --all` is also an
explicit fan-out request.

Exact model names stay attached to their carrier (`opencode` in the GLM
example and `aider` in the DeepSeek-model example). `deepseek` by itself is the
official DeepSeek Harness transport. Family inference supports known names such
as GLM, Grok, Gemini, Claude, GPT, Qwen, DeepSeek, and Mistral. An unknown or
provider-default model on a multi-model carrier remains `unknown`; do not count
it as independent evidence. DeepSeek Harness and Vibe have no documented exact
model flag on these headless surfaces, so OMS refuses `--model` for them.
DeepSeek runs with its process telemetry hard-disabled; its permission preset
still does not claim network or universal host-process isolation.

Treat provider-native user/admin configuration and previously trusted project
configuration as host code. OMS narrows the documented agent, tool, approval,
and worktree surface, but cannot erase native hooks, plugins, MCP servers,
credentials, or organization policy that the CLI loads before the turn. Audit
those layers separately when the repository or provider configuration is not
already trusted.

The built-in set is deliberately narrower than “every agent executable.” Kimi
CLI prompt mode currently conflicts with plan mode and auto-approves tools;
Trae, OpenHands, Goose, and similar surfaces stay custom-adapter candidates
until a stable noninteractive read/write permission contract can be pinned.

## Upstream contracts

- [DeepSeek Harness CLI behavior](https://github.com/deepseek-ai/deepseek-harness/blob/master/apps/cli/reference/README.md)
- [Mistral Vibe programmatic mode](https://docs.mistral.ai/vibe/code/cli/work-with-cli)
- [Pi CLI usage](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/usage.md)
- [GitHub Copilot CLI programmatic reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference)
- [Factory Droid Exec](https://docs.factory.ai/droid-exec/overview)
- [Aider scripting](https://aider.chat/docs/scripting.html)

## Custom adapters

An extra transport must use the marker-prefixed executable
`oms-agent-adapter-ID`, where `ID` is a lowercase safe identifier. Put it on
PATH or in:

```text
${OMS_PROVIDER_ADAPTER_DIR:-$XDG_CONFIG_HOME/oh-my-setting/provider-adapters}
```

The executable should support bounded `--version` and `--help` probes. A call
uses this argv contract:

```text
oms-agent-adapter-ID run
  --access read|write
  --workdir PATH
  --prompt-file PATH
  [--model NAME]
  [--effort LEVEL]
```

Read the prompt from the named file, write the final answer to stdout, write
diagnostics to stderr, and return the provider exit status. Do not commit,
push, edit `.oms`, or retain a background process. Read calls run from a
disposable detached worktree, but the adapter is still a trusted local
executable with the host user's network and credentials unless it adds its own
OS sandbox.

Custom adapters are read-only by default. Enable write delegation per exact
adapter ID only when its implementation is trusted:

```bash
export OMS_PROVIDER_WRITE_ADAPTERS=internal-agent,lab-agent
```

Write calls still run in the ordinary OMS isolated worktree and return a patch
through `peer-delegate`; the allowlist does not grant commit, push, landing, or
publication authority. Unknown executables without the `oms-agent-adapter-`
marker are never discovered.

If a provider is absent or an adapter violates this contract, preserve the
typed failure. Do not silently route to another provider; choosing a different
agent can change cost, policy, data exposure, and model-family independence.
