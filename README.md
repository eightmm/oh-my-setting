# oh-my-setting

One setup that gives Codex, Claude Code, and Antigravity the same rules,
skills, and agent harness on every machine.

**You never run any of it.** Install is the only command you type; from then on
the harness is the agent's, not yours. Its tools (`oms ...`) are invoked by an
agent mid-task, its state (`.oms/`) is written by agents, and its setup steps —
updates, health checks, even the permissions another agent CLI needs — are
things you ask your agent to do. If a document here shows a command, it is
showing you what the agent will run.

[한국어](README.ko.md)

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

This installs the harness and the CLIs it coordinates — Claude Code, Codex,
Antigravity, Node (via nvm), uv, GitHub CLI, and Notion CLI. A council with one
installed peer is not a council, so the peers are part of the install rather
than a flag to remember. Nothing needs root. Add `--no-tools` on a machine that
cannot have them. In an interactive terminal the installer delegates browser
login to `gh auth login` and `ntn login`, then discovers the one compatible
Work Journal data source. The CLIs keep their own credentials; the harness
does not copy them. A non-interactive install prints the follow-up commands and
continues. Use `--connect-services` to require a completed connection or
`--no-connect-services` to skip both logins.

Claude Code also gets a compact status-line HUD by default: model, live context
bar and tokens, five-hour/seven-day plan usage when Claude supplies it,
estimated session cost, and reasoning effort. It is a local renderer and makes
no API call. An existing user `statusLine` is left untouched; update refreshes
only an oh-my-setting-owned command and uninstall removes only that command.

Antigravity answers headless council calls only with standing permissions;
without them a peer call is silently denied. A default install reports what
would be denied, and `--peer-permissions` grants the consult profile
(`read_file(*)`, `command(*)`) at install time, keeping a `.bak` of the
settings it widened. Codex and Claude Code need nothing here — they carry
authority per invocation.

For the Work Journal Notion mirror, automatic discovery succeeds when exactly
one accessible data source has the expected schema. If none or several match,
give the installer the intended one with `--notion-data-source ID`. Only the
nonsecret ID, property mapping, and CLI authentication mode are persisted. See
[docs/WORK-JOURNAL.md](docs/WORK-JOURNAL.md).

| Host | Needs | Managed files |
|---|---|---|
| Linux, WSL | Bash 3.2+, Git, Python 3.9+ (or uv) | symlinks |
| macOS | stock Bash 3.2, Git, Python 3.9+ (or uv) | symlinks |
| Windows Git Bash | Git, Python 3.9+; Node 22+ and provider CLIs from their native installers | verified copies |

Windows gets copies because Git for Windows cannot assume symlink privileges;
the mode is recorded in the install receipt so update, doctor, and uninstall
share one ownership contract (`OH_MY_SETTING_LINK_MODE=auto|symlink|copy`
overrides it). Native PowerShell is not an execution surface — use Git Bash or
WSL. Details for the awkward cases live in
[docs/COMPONENTS.md](docs/COMPONENTS.md).

## Start

Open your coding agent in any directory — empty, mid-project, or ongoing — and
say:

```text
Start this project.
```

The agent detects the state and routes: an empty directory goes through a spec
interview to `PROJECT.md`, a template, and a doctor; an existing repo gets
inspected first and interviewed only for gaps; an ongoing project gets a status
report and the next step. Architecture-shaping work waits for the decisions it
depends on, while clear bounded changes proceed from inspected local contracts.

## What You Can Say

```text
Start this project.
Apply the oh-my-setting ml template.              # or: general, slurm
Run a peer review of the current diff.
Ask all three models with one debate round: vector DB or pgvector?
Delegate this to codex: add input validation to scripts/train.py.
Ask another agent about this split policy, and keep the thread.
Check this dataset's group split for leakage before I train.
Frame this as a hypothesis-driven experiment before I launch the run.
Wait for Slurm job 12345, then digest its log and report.
Update oh-my-setting and re-run its doctor.
```

## What's Inside

Your agent picks these up on its own when a task calls for them. The full
per-script catalog is [docs/COMPONENTS.md](docs/COMPONENTS.md).

Every installation exposes the same five general-purpose skills, so unrelated
sessions never carry domain context they cannot use. Two further layers add
themselves only where they apply: machine-conditional skills, linked only
where their required command exists (`slurm` on a cluster, `gpu-workstation`
where `nvidia-smi` is), and project skills forged into a repository's
`.oms/skills/` — the ML template installs experiment and dataset-safety
discipline, and `oms skill-forge` stores what inspection verified about that
specific repo. All three CLIs load every layer natively.

**Project setup**

- Start router for empty, existing, and ongoing repositories
- Staged spec interview that gates architecture-shaping work
- `general` / `ml` / `slurm` templates with a `PROJECT.md` contract
- Project doctor that verifies all three agents read the same rules
- Per-project agent files hidden from git locally, so public commits and your
  own `.gitignore` stay free of harness residue

**Asking other agents**

- Consult a peer mid-task on one durable shared thread every provider can read
  and extend, instead of one-shot questions
- Councils that send the same question to three models, report how many
  independent model families actually answered, and can run debate rounds
- Review gate with three independent reviewers, backstopped by a mechanical run
  of the project's own checks — a self-reported pass cannot pass a failing diff
- Advisor pass at decision points, routed to a different CLI family
- Outbound scrubbing before any prompt leaves, and one command to grant a
  provider the standing permissions it needs to work headlessly

**Delegating writes**

- Delegate a write task to an isolated git worktree and get a reviewable patch
- Admission ladder before anything lands: clean tree, verification, and refusal
  of patches that pass by deleting assertions or test files
- One mutation boundary (`patch-land`), one landing at a time per repository
- Executor souls — hash-frozen behaviour, path allowlist, frozen verify command,
  bound to a task lease — so a worker's authority is provable afterwards
- Reusable role profiles injected into any provider's worker
- Worker-authority fingerprints across config, remotes, refs, hooks, and shared
  state; delegation depth capped so workers cannot spawn workers

**Agent state and handoff**

- Automatic Work Journal project memory: sanitized durable outcomes become
  incrementally indexed, rebuildable daily/weekly local summaries that agents
  read back with `oms journal show` and a once-per-day prompt digest, with an
  optional finalized-period Notion mirror configured during install
- Session handoff digests captured automatically just before Claude Code or
  Codex compacts a session away, landing in the project's `.oms/handoffs/`
  where the daily digest points at the newest one
- Shared memory with reversible Markdown sources and a searchable local index
- Read-only retained-window routing telemetry and memory source/index health
  diagnostics, both available as structured JSON
- Task packets whose verification is actually executed, not asserted
- Subtask DAG with leases, and bounded execution that claims one scoped task,
  delegates in isolation, and stops at review unless landing is explicit
- Failure ledger keyed by command fingerprint, which names an advisor once the
  same thing has failed twice unresolved — fed automatically by a Claude Code
  hook whenever a Bash tool command fails in a harness-adopted repo
- Append-only JSONL state contracts with a validator and repair tools

**ML and HPC**

- Run ids and a run spine that ties commands, artifacts, and outputs together
- Reproducibility capsules, and provenance from an output back to its run
- Pre-registered hypothesis runs with prediction, baseline, and metric fixed up
  front, plus an experiment board so parallel agents do not duplicate work
- Dataset split manifests that flag train/eval leakage on IDs and declared group
  keys, and detect split drift — raw rows are never stored
- Slurm job reconcile, a single-machine GPU queue, log digests sized for agent
  context, and local hardware/cluster snapshots

**Providers and models**

- Capability probing per CLI, cached against the binary's identity
- Tier routing (fast / balanced / deep) with call-time fallback when a model is
  unavailable, and a diagnostic for provider compatibility and family diversity

**Maintenance**

- Transactional update with rollback, install doctor, and status reporting
- Optional auto-update trigger
- One verification gate, wired as a pre-push hook
- Cleanup and uninstall that restore what they replaced

## Notes

- Local-first: use local files and CLIs by default. Connectors are allowed when explicitly requested or local sources cannot answer reliably.
- Shared harness writes use per-file locks; `OMS_LOCK_TIMEOUT` sets wait/stale
  recovery seconds (default `300`).
- Never commit tokens, API keys, private data, or cluster/machine details.
- Per-project agent files are local-only, so a fresh clone or `git clean -x`
  will not have them — re-apply the template there, in one sentence.
- The scripts live in `~/.oh-my-setting/scripts/`, also reachable as
  `oms <tool>` (`oms list` prints the catalog). Documented for transparency and
  recovery, not for manual use.

## Star

If this helped: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
