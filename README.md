# oh-my-setting

One setup that gives Codex, Claude Code, and Antigravity the same rules,
skills, and workflows on every machine. After install, everything is used by
talking to your coding agent — there is nothing to run in a terminal.

[한국어](README.ko.md)

## Install

The only shell step:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

Open a new shell once if freshly installed CLIs are not found. From here on,
your agent runs everything.

## Start

Open your coding agent in any directory — empty, mid-project, or ongoing —
and say:

```text
Start this project.
```

The agent detects the state and routes:

- empty dir → spec interview → `PROJECT.md` → template → safe skeleton → doctor
- existing repo → inspect the code, apply the template, fill `PROJECT.md` from
  the code, interview only for gaps
- ongoing project → read `PROJECT.md`, run the doctor, report status and the
  next step

No code is written before the spec is confirmed.

## What You Can Say

Project:

```text
Start this project.
Apply the oh-my-setting ml template.        # or: general, slurm
Run the oh-my-setting project doctor.
```

Review and advice (three local models in parallel):

```text
Run a multi-agent review of the current diff.
Run the ML pre-training review gate on this diff.
Ask all three models with one debate round: vector DB or pgvector?
Delegate this to codex: add input validation to scripts/train.py.
```

Experiments (ML):

```text
Launch this training run through the run ledger, note "lr sweep".
Show the last 10 ledger entries.
Wait for Slurm job 12345 to finish, then digest it and report.
Check this molecular dataset's split for leakage before I train.
```

Memory and handoff:

```text
Remember for this repo: run scripts/check.sh fast before claiming done.
Pin for this repo: current task is the dataloader refactor.
Show the active task packet.
```

Maintenance:

```text
Check the oh-my-setting install status.
Update oh-my-setting and re-run its doctor.
Unlink oh-my-setting.                        # or: uninstall it completely
```

## What's Inside

- **Start router + spec interview** — one entry phrase; staged interview,
  confirmed `PROJECT.md`, then template, skeleton, and doctor in one go.
- **Multi-agent workflows** — review (diff gate, ML checklist, debate,
  synthesis), ask (independent opinions), delegate (isolated git worktree,
  returns a reviewable patch). Artifacts land under `.oms/artifacts/`.
- **Safety rails** — outbound prompts are scrubbed before any external CLI
  call (credentials, private keys, machine paths, cluster details block the
  call); injected context is fenced as reference data; diffs are sanitized.
- **Shared memory + task handoff** — compact cross-agent memory
  (`.oms/memory/`) and an active task packet (`.oms/task/current.md`) so any
  of the three agents can continue the same work; closing a task promotes its
  outcome into memory.
- **ML guardrails** — experiment run ledger with a pre-flight `check.sh` gate
  and duplicate-run warning, scaffolded `ml_smoke.py` one-batch contract,
  ML-aware review gate, chem-bio domain checklist (splitting/leakage, labels,
  metrics), long-log digester (`--wait` blocks until a Slurm job finishes),
  machine/Slurm snapshots.
- **Project templates + doctor** — managed rule blocks for general/ml/slurm
  projects and a doctor that verifies every agent sees the same rules.

## Notes

- Local-first: no MCP servers, app connectors, or plugin connector tools.
- Never commit tokens, API keys, private data, or cluster/machine details.
- The scripts the agent runs live in `~/.oh-my-setting/scripts/` — documented
  for transparency and recovery, not for manual use.

## Star

If this helped: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
