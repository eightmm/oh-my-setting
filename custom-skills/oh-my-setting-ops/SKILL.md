---
name: oh-my-setting-ops
description: >
  Maintain an oh-my-setting install from chat. Use when the user asks to check
  install status, update oh-my-setting ("업데이트 해줘"), run doctor ("닥터
  돌려줘", "설치 상태 확인"), clean legacy skill links, fix duplicate skill
  picker entries, regenerate local snapshots, or explain what the installed
  agent rules/scripts are doing. For the shared repo dashboard ("what's the
  current state", "oms state") use agent-harness instead.
---

Goal: the user asks in chat; the agent runs local scripts. Do not tell the user
to run scripts manually unless a command is impossible from the current agent.

## Rules

- Use local files and shell commands only. Do not use MCP servers, app
  connectors, or plugin connector tools.
- Start with `~/.oh-my-setting/scripts/status.sh` or repo-local
  `scripts/status.sh` when orienting; it reports install links, tools, active
  task state, and auto-update state.
- For install health, run `doctor.sh`; for skill picker duplicates, run
  `skill-doctor.sh`.
- For legacy oh-my-setting symlink cleanup, run `cleanup.sh --dry-run` first,
  summarize exactly what would be removed, then run `cleanup.sh --apply` when
  the user asked for cleanup/fix or already approved the current task.
- `cleanup.sh --apply` only removes known oh-my-setting legacy symlinks and
  backup skill symlinks. Do not delete unrelated third-party plugins, caches,
  regular files, or directories unless the user explicitly approves that
  separate scope.
- After cleanup or update, rerun `doctor.sh` and report remaining warnings.
- Update/uninstall/plugin removal must run from the canonical receipt owner;
  foreign checkouts refuse before pulling or mutating global state. Use the
  canonical root printed by `status.sh`.
- Treat `Auto Update: stale` as historical state only; update/check again from
  the canonical checkout before reporting current update health.
- If the current session still shows stale skills, tell the user that the UI may
  cache skill lists until a new agent session starts.

## Common Requests

Status:

```bash
~/.oh-my-setting/scripts/status.sh
```

Update and verify:

```bash
~/.oh-my-setting/scripts/update.sh
```

Install health:

```bash
~/.oh-my-setting/scripts/doctor.sh
```

Duplicate `$skill` entries or stale skills:

```bash
~/.oh-my-setting/scripts/cleanup.sh --dry-run
~/.oh-my-setting/scripts/cleanup.sh --apply
~/.oh-my-setting/scripts/skill-doctor.sh
```

Regenerate local snapshots:

```bash
~/.oh-my-setting/scripts/write-machine-snapshot.sh
~/.oh-my-setting/scripts/generate-slurm-skill.sh
```

## Output

Use compact prose. Include commands run and whether doctor/skill-doctor passed.
Do not add a fixed changed/verified/not-verified block for questions or design
explanations.
