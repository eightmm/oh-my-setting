# State, Memory, and Live Edits

Use project memory for stable preferences and recurring pitfalls; keep required
rules in checked-in policy. Prefer compact `context` or targeted `search` over
loading the full source log.

```bash
oms agent-memory --repo . context
oms agent-memory --repo . search --text pgvector
oms agent-memory --repo . append --agent codex --text "Run the focused check first."
oms agent-memory --repo . pin --agent codex --text "Current migration boundary: v2."
```

Use the active task packet for one short-lived handoff. Automatic prompt
recording is opt-in with `OMS_AUTO_TASK=1`.

```bash
oms agent-task --repo . init --goal "Ship the focused fix" --verify "bash scripts/check.sh"
oms agent-task --repo . update --state "Patch ready" --next "Run checks"
oms agent-task --repo . verify --verification "focused check passed"
oms agent-task --repo . close
```

Before `close`, set `OMS_AGENT_TASK_CLOSE_MEMORY=0` when the outcome should not
be promoted into durable memory.

Use a change guard only when user edits or scope drift are plausible:

```bash
oms change-guard --repo . --allow scripts/ begin
oms change-guard --repo . check
oms change-guard --repo . end
```

The skill router emits hints and records guarded routes. Disable it with
`OMS_SKILL_ROUTER_OFF=1`; disable the final verification guard with
`OMS_TURN_GUARD_OFF=1`.
