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
| Slurm job | `oms job-digest <id> [log] --wait --wait-timeout 540 --max-bytes 16384`. Exit 124 means still queued. |
| Native subagent | One long wait. Do not alternate short waits with list or status calls when no new information is expected. |
| Peer operation | Read the existing operation ID with `oms_peer_result`; never start a second operation to check the first. |

Keep the inner wait budget below the host's own tool-call timeout, or run the
call as a host background task that notifies on exit. A call the host kills
mid-wait is one more read timeout, not a job failure.

Do not spawn a model worker whose only job is to poll. While something is
pending, do independent work that is already authorized; if there is none,
make one long wait instead of many short ones.

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

## Managed Codex settings

`oms` manages only keys whose effect it can evidence, and never overrides a
value the user set (`oms doctor` reports the state):

- `background_terminal_max_timeout` raises the ceiling for an empty
  background-terminal poll. A ceiling is not a duration: the wait that is
  actually requested still has to be long.
- `features.multi_agent_v2` wait floors are written only where the user
  already enabled that backend in table form. OMS never enables a feature to
  make it efficient.

Model, reasoning effort, service tier, compaction, sandbox, and approval
settings are quality or authority decisions and stay untouched.

## Floors that always win

Saving usage never justifies skipping required verification, stopping before
the task is done, weakening an approval, or reporting an unobserved result.
Count success as completed, verified work per unit of usage, not fewer tokens.

Motivated by a Codex user report that short sleep/poll turns dominated an
orchestrator's cost (author's own measurement, not reproduced here). Codex key
names, the 300000 ms default, and the `min <= default <= max` wait validation
were checked against codex-cli 0.150.1 and 0.155.1 on 2026-09-21. These are
guidance and configuration choices, not measured quota savings.
