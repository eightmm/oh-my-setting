# Efficient Waiting

Use this when a command, job, subagent, or peer operation will outlast one
turn, or when a status check keeps returning "still running". It guides
judgment only; it creates no mode, hook, daemon, state, or authority.

## What actually costs

A shell loop that polls and sleeps is cheap. A model turn that re-reads the
conversation to learn nothing changed is not. Remove status-only model turns,
not polling: keep the polling inside one bounded non-model call and hand the
model a result it can act on.

## Pick the wait by what you are waiting for

| Waiting on | Do this |
|---|---|
| Build, test, script | One blocking call with output sent to a log file; then read a bounded tail or digest, not the stream. |
| Slurm job | `oms job-digest <id> [log] --wait --wait-timeout 540 --max-bytes 16384`. Exit 124 means observation expired; queue state may be unknown. |
| Native subagent | One long wait. Do not alternate short waits with list or status calls when no new information is expected. |
| Peer operation | Reuse `oms_peer_result` with the operation ID, `wait_seconds` (0-50, below the host timeout), and the last `cursor` as `after`; unchanged bodies are omitted. Expiry does not restart the peer. |

Keep the inner wait budget below the host's own tool-call timeout, or run the
call as a host background task that notifies on exit. A call the host kills
mid-wait is one more read timeout, not a job failure.

For a known long-running terminal command, request a useful wait explicitly
(for example 30-60 seconds when the host and communication limits permit),
not repeated 1-second reads. These are examples, not a universal minimum:
interactive input or a diagnostic may need an immediate read. Tool waits can
return early on output, so send noisy progress to a log and return a digest.
Use completion notifications only where the host actually supports them;
OMS's detached MCP operations do not automatically resume the model.
After an unchanged result, increase the next wait within the supported host
and user-update limits; do not reset to a short poll. Check the clock only for
a deadline decision, not before every status read. A waiting ceiling is not a
target or proof that the running host accepted that duration.

Do not spawn a model worker whose only job is to poll. While something is
pending, do independent work that is already authorized; if there is none,
make one long wait instead of many short ones.

Workers notify the parent for completion, failure, a blocker, or evidence that
changes another task's next action; avoid unchanged progress/heartbeat messages.
Keep required user-facing updates, but do not duplicate them as peer messages.

## Timeouts are not outcomes

- A read, wait, or observer timeout is not job failure, cancellation, or
  permission to resubmit. The original job or operation is still running.
- Pending is not completion, and leaving the queue is not success. Confirm
  exit status and required outputs before downstream work, landing, or a
  success claim. Empty or failed accounting is unknown, not passed.
- Do not promise a notification or automatic continuation unless the runtime
  capability was verified. Otherwise record what is pending and how to check.

## Bound what returns to context

Keep full logs on disk. Return status, the first causal error, the last
traceback, and a bounded tail; say what was omitted and where to read more.
Do not replay a log or dashboard that has not changed since the last read.
A line cap alone is not a byte cap: one multi-megabyte line defeats it.
Operation cursors identify the visible result, not elapsed time or unread file
contents. `unchanged=true` retains status/exit and artifact paths but omits the
answer/log body; omit `after` to retrieve it again. Cursors do not acknowledge
messages or suppress later completion, failure, or changed visible evidence.

## Managed Codex settings

`oms` manages only keys whose effect it can evidence, and never overrides a
value the user set (`oms doctor` reports the state):

Normal install/update links the shared waiting rule and this reference for
Codex, Claude Code and Antigravity. Codex config owns the numeric ceiling;
the global rule owns the behavior. Do not duplicate it into user-owned
`developer_instructions`, enable a backend, or add a polling hook.

- `background_terminal_max_timeout` raises the ceiling for an empty
  background-terminal poll. A ceiling is not a duration: the wait that is
  actually requested still has to be long.
- `features.multi_agent_v2` wait floors are written only where the user
  already enabled that backend in table form. OMS never enables a feature to
  make it efficient.

Model, reasoning effort, service tier, compaction, sandbox, and approval
settings are quality or authority decisions and stay untouched.

`doctor` checks the config file, not the live app's effective settings. The app
may use a different binary, backend, profile or override than PATH's `codex`.
Confirm the active host/tool contract before claiming wait floors apply; do not
enable V2 merely to make this check pass. Host limits and higher-priority
communication requirements still apply. Measure actual status-only returns and
usage separately; a successful config parse proves neither savings nor behavior.
Use existing `oms artifact-index telemetry` for recorded primary `prompt_bytes`,
provider-reported tokens and elapsed time. Bytes are not tokens; missing reports
are unknown, not zero usage. Compare like tasks and verification outcomes.
The byte metric excludes provider-added context, retries and repair prompts.

## Floors that always win

Saving usage never justifies skipping required verification, stopping before
the task is done, weakening an approval, or reporting an unobserved result.
Count success as completed, verified work per unit of usage, not fewer tokens.

Motivated by a Codex user report that short sleep/poll turns dominated an
orchestrator's cost (author's own measurement, not reproduced here). Codex key
names, the 300000 ms default, and the `min <= default <= max` wait validation
were checked against codex-cli 0.150.1 and 0.155.1 on 2026-09-21. These are
guidance and configuration choices, not measured quota savings.
