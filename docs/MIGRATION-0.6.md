# Migrating to 0.6

Version 0.6 makes the peer transports carry why the model stopped, narrows
what a judging seat is given, and stops append-only state from growing
unbounded. Nothing migrates automatically; no stored state changes shape.

## One workflow-visible default flip

- **`oms consult` no longer attaches shared memory by default.** Shared
  memory holds prior conclusions, and a second opinion anchored on the
  first is not a second opinion. Pass `--memory` to restore the old
  behavior for one call; the active task packet still rides by default.
  This matches peer-ask, peer-review, and agent-call, which were already
  opt-in.

## Transports (transparent to callers)

- claude print calls ride `--output-format json`, codex exec calls ride
  `--json`. Both are parsed back to plain text immediately, so artifacts,
  threads, and every downstream reader keep their shape; the one addition
  is a `stop-reason:` line at the top of the Output section. A CLI or stub
  that answers in plain text passes through untouched.
- A truncation now fails closed instead of reading as a finished answer:
  `max_tokens` (claude) and a JSONL stream whose turn never closed (codex)
  classify as `truncated`, and a review GATE line in such a transcript is
  voided. If a seat that used to "pass" starts reading no-verdict, the
  transcript was being cut — raise the effort/timeout rather than
  restoring the old blindness.
- Read-access claude seats run with `--strict-mcp-config` and a four-tool
  belt (`Read,Grep,Glob,Bash`). Write workers keep MCP and the default
  tool set. `oms_peer_start` refuses when called from inside a delegated
  worker, for every provider — recursive delegation is the owner's
  decision.

## State growth

- `oms gc` now also compacts the lifecycle event stream: attempts that are
  terminal and quiet past `--days` drop as whole streams (dry-run reports,
  `--apply` acts; survivors must still project or nothing is touched).
- Projections are bounded at the MCP mouth: `oms_agent_operations` returns
  the most recent 40 attempts, `oms_fail_ledger` the 30 most recently
  active fingerprints, each stating its omission. The full history stays
  on disk; `fail-ledger list` without `--limit` reads all of it.
- `tsp-queue logs` prints a bounded tail by default; `logs --full <id>`
  is the old whole-log read, and `oms job-digest` the structured one.

## Detection

- Project style detection no longer has an rg fast path: `find -maxdepth 3`
  is the single contract on every machine. A repository whose only ML or
  Slurm signal sits deeper than three levels detected differently per
  machine before; it now detects `general` everywhere — pass the style
  explicitly to `apply-project-template` if that is wrong for a repo.
