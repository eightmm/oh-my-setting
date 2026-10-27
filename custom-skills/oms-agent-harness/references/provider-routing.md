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
| `muse` | `muse` | write calls only; no documented headless read-only mode |
| `kimi` | `kimi` | write calls only; print mode auto-approves tools |
| `kiro` | `kiro-cli` | write calls only; headless docs do not establish refusal of untrusted tools |
| `continue` | `cn` | headless prompt with read-only or auto mode |
| `goose` | `goose` | headless run with automatic tool mode; write calls only |
| `cline` | `cline` | headless task with explicit tool auto-approval; write calls only |
| `kilo` | `kilo` | autonomous run with configured tool permissions; write calls only |
| `openhands` | `openhands` | headless mode auto-approves actions; write calls only |
| `auggie` | `auggie` | print mode with explicit write-tool permissions; write calls only |

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
oms peer-delegate --to cursor --prompt 'Implement the reviewed change.'
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

Read seats are admitted only when the OMS invocation uses a documented read-only
or plan mode. Kimi's print mode auto-approves tools and cannot combine with
`--plan`; Muse has no documented headless read-only mode. Kiro's headless docs
describe explicit trust flags but do not state that untrusted tools are refused
without one, so OMS treats it as write-only too. These providers are refused
before launch for read calls and are excluded from automatic read/council pools.
Write calls remain available through the worker-owned clone.

Goose chat mode disables all tools rather than providing a useful repository
read mode. Cline's plan option is documented, but its CLI reference does not
promise that shell access is blocked, so OMS does not admit it as a read seat.
Kilo has no documented read-only command mode in its CLI reference. Auggie's
ask mode permits retrieval and non-editing tools, but its installed MCP/plugin
configuration can add tools outside the documented core fence; OMS keeps it
write-only. OpenHands headless mode always approves every action. These
transports are excluded from automatic read/council candidate lists and from
the core/council read-capability profiles.

The one-shot forms are `goose run -t`, positional `cline PROMPT`, `kilo run
--auto PROMPT`, `openhands --headless -t`, and `auggie --print PROMPT`. Each
exits after its submitted task. Goose's `auto` mode is selected for write calls;
Cline's writes explicitly set `--auto-approve true`; Kilo writes use `--auto`;
OpenHands headless mode itself always approves actions; Auggie writes explicitly
allow its edit, write, and terminal tools. Goose, Cline, and Kilo accept a model
override. Kilo exposes model-specific reasoning variants through `--variant`;
valid levels depend on the selected model. OpenHands and Auggie's available docs
do not confirm an effort flag, so OMS leaves effort at the configured default.
OpenHands also has no confirmed per-invocation model flag.
Goose, Cline, Kilo, OpenHands, and Auggie can carry models across vendors, so
their family is inferred from the selected model ID.

For a prompt beginning with `-`, every transport receives a temporary copy with
one leading newline. This keeps CLI transports from parsing the prompt as an
option while preserving the same prompt bytes after that newline; stdin
transports receive the copy through stdin. Provider usability probes use only
top-level version and help commands; provider-specific operational help is
queried only after the usability probe passes.

Provider credentials and initial setup must be completed before headless use:
Goose `configure`, Cline `auth`, Kilo `/connect`, and Auggie `login` have
interactive setup surfaces. The OMS invocation disconnects stdin so these
surfaces cannot block a worker; if setup is missing, the CLI call fails and the
operator must configure it first. Goose updates (`goose update`), Cline updates
(`--update`), and Kilo upgrades (`kilo upgrade`) are explicit commands. Auggie
documents automatic updates and OMS disables them for the invocation with
`AUGMENT_DISABLE_AUTO_UPDATE=1`. No extra update flag is passed to OpenHands.

Junie and Crush remain custom-adapter candidates for now. Junie's documented
headless entry can prompt for approval of sensitive actions, while the documented
plan invocation is interactive; no headless auto-approval flag is established.
Crush's `run` surface is one-shot, but its documentation does not confirm a
headless write-approval flag. A custom adapter can define a reviewed local
policy without OMS guessing either CLI's behavior.

The argv shapes below follow the cited vendor documentation and have not yet
been exercised against real provider CLIs. Kiro carries models from multiple
vendors, so OMS infers its family from the model ID. Continue likewise carries
configured models from multiple vendors.

## Upstream contracts

- [DeepSeek Harness CLI behavior](https://github.com/deepseek-ai/deepseek-harness/blob/master/apps/cli/reference/README.md)
- [Mistral Vibe programmatic mode](https://docs.mistral.ai/vibe/code/cli/work-with-cli)
- [Pi CLI usage](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/usage.md)
- [GitHub Copilot CLI programmatic reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference)
- [Factory Droid Exec](https://docs.factory.ai/droid-exec/overview)
- [Aider scripting](https://aider.chat/docs/scripting.html)
- [Meta Muse Code headless and approval flags](https://dev.meta.ai/docs/muse-code/extending)
- [Kimi Code CLI command reference](https://moonshotai.github.io/kimi-code/en/reference/kimi-command)
- [Kiro CLI headless mode](https://kiro.dev/docs/cli/headless/)
- [Kiro CLI v3 permissions migration](https://kiro.dev/docs/cli/v3/migration-guide/)
- [Kiro model families](https://kiro.dev/docs/cli/models/)
- [Continue CLI quickstart](https://docs.continue.dev/cli/quickstart)
- [Goose CLI commands and run mode](https://goose-docs.ai/docs/guides/goose-cli-commands/)
- [Cline CLI reference](https://github.com/cline/cline/blob/main/docs/cli/cli-reference.mdx)
- [Kilo CLI and autonomous run mode](https://kilo.ai/docs/code-with-ai/platforms/cli)
- [Kilo CLI command reference](https://kilo.ai/docs/code-with-ai/platforms/cli-reference)
- [OpenHands headless mode](https://docs.openhands.dev/openhands/usage/cli/headless)
- [OpenHands CLI command reference](https://docs.openhands.dev/openhands/usage/cli/command-reference)
- [Auggie CLI reference](https://docs.augmentcode.com/cli/reference)
- [Auggie tool permissions](https://docs.augmentcode.com/cli/permissions)
- [Junie CLI quickstart and approvals](https://junie.jetbrains.com/docs/junie-cli.html)
- [Crush source and command README](https://github.com/charmbracelet/crush/blob/main/README.md)

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
