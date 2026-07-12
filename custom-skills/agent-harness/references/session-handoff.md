# Prior Session Handoff

Use session handoff only to continue a prior Codex, Claude Code, or Antigravity
session. Shared memory and active tasks are better for curated forward-looking
state.

```bash
oms session-handoff capture --agent codex --cwd .
oms session-handoff list
oms session-handoff show <digest>
```

The capture is mechanical and stores a compact local digest under
`.oms/handoffs/`. It scans transcript-derived content and refuses sensitive
material by default. Use `--allow-sensitive` only after explicit inspection and
only when loading the digest remains within the approved trust boundary.

Claude captures user/assistant turns, Codex captures task messages and final
answers, and Antigravity history may contain prompts only. State that limitation
when it affects continuity. Loading a digest into another provider is always an
explicit parent action.
