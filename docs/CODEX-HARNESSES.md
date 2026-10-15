# Codex subscription inference in other harnesses

This guide is for operators running Codex inference through Pi or Claude Code.
It covers routing, credential boundaries, compatibility and verified evidence.

## In this guide

- [Data and authority](#data-and-authority)
- [Claude auto permission checks](#claude-auto-permission-checks)
- [Compatibility and evidence](#compatibility-and-evidence)

`oms harness` selects the agent UI and tool loop separately from the inference
model. It is shipped with OMS; invoking it starts the selected, already
installed CLI. It does not install a CLI, alter its saved settings, or start a
persistent service.

`oms` on PATH runs the installed checkout, which can differ from the repository
you are editing. Before this feature is released and installed, use the
repository entrypoint from its root:

```bash
./scripts/oms harness claude --model gpt-6.1-sol
```

If the installed `oms` reports `unknown tool: harness`, it does not contain
this feature. Updating the development files alone does not update that
installation; `./scripts/oms harness ... --check` checks the development
entrypoint without making an inference request.

```bash
oms harness codex --model gpt-6.1-sol
oms harness pi --model gpt-6.1-sol
oms harness claude --model gpt-6.1-sol
oms harness claude --model gpt-6.1-sol --check
oms harness claude --model gpt-6.1-sol -- --resume
```

Use an exact model from the installed Codex catalog. `--check` verifies the
executable, current ChatGPT subscription authentication, model catalog entry,
and credential RPC without making an inference request. A catalog entry does
not establish account entitlement; only a completed inference request does.
`--dry-run` prints argv without accessing credentials. Arguments after `--`
belong to the selected CLI; model, provider, API-key, and settings overrides
that could change routing are rejected. `--effort` defaults to `low` and must
be supported by the selected model. All inference requests, including Claude
background tasks and subagents, use that same exact model.

## Data and authority

```text
Codex CLI -> native Codex inference
Pi -> explicit oms-codex extension -> local Responses gateway
Claude Code -> local Messages gateway -> Responses conversion
                                           |
                                  Codex subscription backend
                                           |
                            native Codex app-server auth broker
```

The Codex CLI owns credential storage, keychain access, and token refresh.
OMS obtains an access token over private app-server stdio and keeps it in
memory. It never copies `auth.json`, saves refresh tokens, puts subscription
tokens in CLI arguments, or sends them to Pi/Claude Code. The gateway uses a
fresh local capability, binds only to `127.0.0.1` on an ephemeral port, requires
the exact Host header, and rejects browser Origin headers. Upstream credentials
go only to the fixed HTTPS Codex backend; redirects are not followed. Native
auth errors and upstream error bodies are not relayed or logged.
Frontend proxy environment variables are removed so local inference requests
cannot be routed through an ambient HTTP proxy.

Requests have a 16 MiB body/event limit and at most eight local handlers. The
gateway relays streaming text and tool arguments incrementally, emits keepalive
pings, and requires upstream completion before reporting a successful response.
Authentication gets one native refresh attempt after HTTP 401; model errors,
quota limits, and refusals never trigger model/provider/API-key fallback.
Gateway HTTP errors set `x-should-retry: false` to suppress SDK retries.
The gateway and auth broker stop with the launched CLI. Ctrl-C remains the
native CLI's turn interruption; termination of the launcher cleans up its child.

Tools execute in the selected harness with its own permissions, hooks, MCP,
session storage, and repository rules. The bridge executes no model tools.
Pi uses an invocation-only provider extension and retains its usual settings
and sessions. Claude Code's model aliases are pinned for this invocation; its
experimental betas, deferred tool search, and nonessential traffic are disabled
to keep requests within the supported function-tool contract. Normal CLI
configuration still applies, including administrator restrictions.

### Claude auto permission checks

Claude Code 2.1.296 sends a separate, nonstreaming classifier request before
actions such as Bash or MCP calls. That request can include XML stop sequences
such as `</block>`. The bridge supports these text-only requests: it waits for
completed upstream inference, removes the first matching sequence and subsequent
text, and returns the native `stop_sequence` fields. Stop sequences are local
response processing; they do not stop upstream generation or reduce reported
subscription usage. Streaming requests or requests declaring tools with custom
stops are rejected; limits are 16 sequences of 4,096 characters each.

The selected native permission mode remains in effect. Classifier requests use
the same pinned Codex model and subscription as the conversation. OMS supplies
no safety verdict and enables no permission bypass. Incomplete replies and
transport failures return errors; Claude's own parser and permission rules
continue to reject missing, malformed, or denying verdicts. GPT classifier
behavior through this experimental bridge is outside Anthropic's
[official auto-mode model support](https://code.claude.com/docs/en/permission-modes#eliminate-permission-prompts-with-auto-mode).

## Compatibility and evidence

The Pi/Claude bridge is experimental. It supports text, image inputs, ordinary
function tools and results, parallel historical tool calls, streaming and
nonstreaming replies, and opaque reasoning continuity. Anthropic server tools,
document blocks, custom stop sequences in streaming/tool requests, and Responses server-side conversation
IDs/background mode are rejected. Anthropic cache controls are not mapped;
reported usage comes from OpenAI. Codex's legacy subscription endpoint does not
accept output-token caps, so native `max_tokens`/`max_output_tokens` values do
not constrain generation through this bridge. Pi uses a conservative 200,000
context estimate; supply `--context-window` when a verified model limit differs.
Pi's zero API price metadata represents subscription billing and does not mean
requests consume no ChatGPT quota.

Authentication export depends on Codex's **deprecated** `getAuthStatus` RPC
with `includeToken`, alongside `account/read` and `model/list`. If native Codex
removes that RPC, the bridge fails explicitly. Reusing an existing Codex token
with the legacy subscription endpoint follows the current native/upstream
source contract; it is separate from OpenAI's documented Sign in with ChatGPT
flow for registering an independent third-party OAuth client. This feature
does not promise continued endpoint support or access to every model.

The offline contract suite is [test_codex_harness.py](../tests/test_codex_harness.py),
run by the canonical provider-registry suite. It uses fake authentication RPC
and HTTP/SSE peers and checks model pinning, tool/result/opaque reasoning
round trips, completion, error redaction, local authorization, and cleanup.
Offline checks do not establish live account/model access, actual Pi UI
operation, or native macOS/Windows behavior.

On 2026-10-10, the existing subscription login completed a real
`gpt-6.1-sol` request through Claude Code 2.1.296 and the local bridge, returning
the requested `OMS_BRIDGE_OK` text with exit 0. A separate native Claude fixture
executed `Read`, returned the file content to the gateway, and completed the
second turn. That tool-loop proof uses a fake model; it is separate from the
live text-only subscription proof. Pi was not installed for native validation.
The legacy backend omitted Content-Type on its valid SSE reply, so the bridge
validates event framing and completion rather than requiring that header.

The subsequent auto-mode regression reproduced the reported “cannot determine
the safety of Bash” error without any inference call: the bridge rejected the
classifier's `stop_sequences`. After the correction, native Claude fixtures
execute approved Bash and MCP actions and block denying or malformed verdicts;
an unavailable classifier also leaves the MCP action unexecuted. A
separate fixture routed only the classifier request to the real existing
`gpt-6.1-sol` subscription; Claude approved and executed the disposable Bash
marker with exit 0. Its conversation replies used a fake model. This establishes
classifier transport and native permission behavior, not safety calibration.

Primary contracts:

- [OpenAI authentication](https://learn.chatgpt.com/docs/auth)
- [Codex app-server](https://learn.chatgpt.com/docs/app-server)
- [Legacy Codex auth RPC source](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/v1.rs)
- [OpenAI third-party ChatGPT plan usage](https://developers.openai.com/siwc/token-sharing-open-source)
- [Claude Code gateway compatibility](https://code.claude.com/docs/en/llm-gateway-protocol)
- [Pi provider extension contract](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/custom-provider.md)
- [Pi legacy Codex subscription transport](https://github.com/earendil-works/pi/blob/main/packages/ai/src/api/openai-codex-responses.ts)
