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

Detection runs only bounded `--version`, `--help`, or documented local model
catalog commands. It never logs in, installs, upgrades, changes provider
configuration, or starts an inference request.

```bash
oms models --providers auto
oms model-doctor --providers auto --json
oms consult --to grok --prompt 'Check this design assumption.'
oms consult --to opencode:model=zai/glm-4.7 --prompt 'Review this plan.'
oms agent-run --to cursor --mode write --prompt 'Implement the reviewed change.'
```

Use `--providers all` only for diagnostics that should also show absent
built-ins. The default `peer-ask` and `peer-review` council remains the stable
Codex/Claude/Antigravity set, so installing another CLI never causes surprise
fan-out or spend. Name optional seats explicitly; `consult --all` is also an
explicit fan-out request.

Exact model names stay attached to their carrier (`opencode` in the GLM
example). Family inference supports known names such as GLM, Grok, Gemini,
Claude, GPT, Qwen, DeepSeek, and Mistral. An unknown or provider-default model
on a multi-model carrier remains `unknown`; do not count it as independent
evidence.

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
