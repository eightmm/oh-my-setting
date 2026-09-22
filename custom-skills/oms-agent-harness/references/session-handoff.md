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
A transcript with fewer than two user turns is skipped (exit 0, nothing
written) — there is nothing to hand off; pass `--min-user-turns 0` to capture
a trivial session anyway. A sensitive transcript still refuses loudly
regardless of the floor.

New digests put the active task snapshot before historical conversation, naming
the task ID, HEAD, packet digest and verification observation time. The assistant
summary remains an unverified claim. Resume prefers the current task over a
newer unrelated digest, labels changed/unbound snapshots, and sends a pointer
instead of the transcript. Source and verification must still be rechecked.
The entire resume block defaults to 4 KiB (`OMS_RESUME_MAX_BYTES`, 512–16384),
including the truncation notice and state pointer; no model call is added.
Starting inside a Git subdirectory still resumes the repository root's state.
Capture retains only the first request, bounded recent turns and latest answer,
not the entire conversation in memory; multiline summaries keep their evidence
and uncertainty lines. `OMS_HANDOFF_TURNS` selects 0–50 recent turns (default 6);
zero omits that section's turns without changing the real session turn count.

Claude captures user/assistant turns, Codex captures task messages and final
answers, and Antigravity history may contain prompts only. State that limitation
when it affects continuity. Loading a digest into another provider is always an
explicit parent action.
