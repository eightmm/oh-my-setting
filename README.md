# oh-my-setting

A local control plane that lets different coding-agent CLIs continue the same
project from the same durable rules, state, plans, evidence, leases, and
handoffs. Codex, Claude Code, and Antigravity remain the managed core; optional
providers join through the same capability-safe transport contract.

**You never run any of it.** After the host prerequisites below, install is the
only command you type; from then on the harness is the agent's, not yours. Its
tools (`oms ...`) are invoked by an agent mid-task, its state (`.oms/`) is
written by agents, and its setup steps — updates, health checks, even the
permissions another agent CLI needs — are things you ask your agent to do. If
a document here shows a command, it is showing you what the agent will run.

[한국어](README.ko.md)

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

On Windows, a fresh default Codex setup — or any profile selecting a
Node-backed tool — first needs native **Node.js with npm**. Install the exact
release named by the installer from the [official Node.js
archive](https://nodejs.org/dist/), keep its PATH option enabled, and reopen Git
Bash. The installer stops instead of accepting a different Node version. An
Antigravity-only `core` profile does not need Node.

The default installer selects the `core` capability: the harness, Bash, Git,
Python, and one coding-agent provider. It does not make GitHub CLI, Notion CLI,
all three providers, research tooling, or cluster tools mandatory. Use the
`full` compatibility profile only on machines that should carry the historical
all-provider/GitHub/Notion/research footprint. Nothing needs root; managed tool
versions, platform URLs, and integrity values are pinned in `tools.lock.json`.
Provider dependencies are selected with the provider: Codex and Claude include
locked Node, while Antigravity does not. New downloads are verified before use.
Existing external CLIs at the exact version are reused and labeled as
version-only by doctor. Install, update, repair, and uninstall share one
user-wide lifecycle lock.

Installation and scheduled updates use a private uv-managed Python pinned in
`tools.lock.json`; system/project Python is only a bootstrap, not the timer's
interpreter. Automatic updates preserve local checkout edits and report them as
`blocked`. Keep private policy in user skills outside the tracked install tree.
Systemd installs report whether logout persistence is available; explicitly set
`OH_MY_SETTING_AUTO_UPDATE_LINGER=1` when installing the trigger to enable it
(host permission may be required), or use cron. Linger applies to all this
account's user services, so OMS does not silently enable it by default.

Capability profiles are `core`, `council`, `github`, `notion`, `research`,
`hpc`, `container`, `remote`, and `full`. The selective installer reuses the
existing locked download transactions, records the exact requested profile in a
private receipt, and reapplies only that tool set during updates. Existing
installs without a capability receipt retain the legacy full-tool update path
until an agent explicitly migrates them. See
[docs/OMS-RUNTIME.md](docs/OMS-RUNTIME.md).

When GitHub or Notion capabilities are selected, an interactive installer can
delegate browser login to `gh auth login` and `ntn login` and discover the Work
Journal's Notion target; a non-interactive install records the missing
capability instead of weakening the core runtime.
Claude Code gets compact main and subagent HUDs (model/effort, context,
rate-limit countdowns, cost, Git state); Codex gets the equivalent native footer
when it has no user footer. The daily updater applies clean fast-forwards by
default and skips dirty or diverged checkouts. Uninstall restores managed
configuration but leaves the external CLIs and the user-local PATH entry in
place.

The install manages the three global agent rule files — `~/.claude/CLAUDE.md`,
`~/.codex/AGENTS.md`, and `~/.gemini/AGENTS.md`. A pre-existing file is moved
to `<file>.backup.<timestamp>` (announced at install time and reported by
`oms doctor`), stops applying while the install is active, and is restored by
`oms uninstall`.

| Host | Needs | Managed files |
|---|---|---|
| Linux, WSL | Bash 3.2+, Git, Python 3.9+ (or uv) | symlinks |
| macOS | stock Bash 3.2, Git, Python 3.9+ (or uv) | symlinks |
| Windows Git Bash | Git, Python 3.9+; exact locked native Node when selected tools need it | verified copies |

The awkward cases — Windows copy mode, Antigravity's headless permissions,
Notion data-source selection — live in
[docs/COMPONENTS.md](docs/COMPONENTS.md) and
[docs/WORK-JOURNAL.md](docs/WORK-JOURNAL.md). Upgrading an existing install
is one `oms update`; what each release changes underfoot is stated in its
migration note, currently [docs/MIGRATION-0.7.md](docs/MIGRATION-0.7.md).

## Start

Open your coding agent in any directory — empty, mid-project, or ongoing — and
say:

```text
Start this project.
```

The agent detects the state and routes: an empty directory goes through a spec
interview to `PROJECT.md`, a template, and a doctor; an existing repo gets
inspected first and interviewed only for gaps; an ongoing project gets a status
report and the next step.

Once `PROJECT.md` is confirmed, you can ask the agent to carry it through a
reviewed task proposal, bounded implementation, acceptance checks, and a Draft
PR. Generated work never approves itself, and merge/release stay separate.

## Typed Runtime Core

`oms runtime` is a standard-library-only semantic layer over the existing
hardened execution plane. It reconciles the distributed task state into an
effective TaskEnvelope, projects criterion-level evidence coverage, compiles
bounded context manifests, selects optional capability profiles, exports
sanitized cross-machine capsules, runs honest local/container/remote backends,
and evaluates comparable research experiments.

It deliberately does **not** replace the established mutation path:

```text
peer-delegate -> patch-admit -> patch-land
```

Plan leases, executor souls, one-use approvals, commit intents, and Draft PR
intents remain authoritative. Runtime snapshots, capsules, context bundles, and
backend receipts are evidence or advisory state, never write authority.

The compact catalog is `oms list --frontdoor`; every compatibility primitive is
still available through `oms list --all`.

## What You Can Say

```text
Start this project.
Apply the oh-my-setting ml template.              # or: general, slurm
Run a peer review of the current diff.
Ask all three models with one debate round: vector DB or pgvector?
Delegate this to codex: add input validation to scripts/train.py.
Take this confirmed PROJECT.md through implementation to a Draft PR.
Show the effective task contract and which completion criteria lack evidence.
Compile the smallest review context for this patch.
Prepare only the core and research capability profiles on this machine.
Export a sanitized continuity capsule for the other workstation.
Run this experiment with fixed seeds and reject it if invariants regress.
Ask another agent about this split policy, and keep the thread.
Check this dataset's group split for leakage before I train.
Frame this as a hypothesis-driven experiment before I launch the run.
Wait for Slurm job 12345, then digest its log and report.
Update oh-my-setting and re-run its doctor.
```

## What's Inside

Your agent picks these up on its own when a task calls for them. The compact
agent surface is `oms list --frontdoor`; the complete compatibility catalog is
`oms list --all`, documented in [docs/COMPONENTS.md](docs/COMPONENTS.md).

- **Typed semantic runtime** — effective TaskEnvelope projection,
  criterion-linked EvidenceCoverage, context manifests, canonical failure
  recovery, optional capability profiles, portable capsules, execution
  receipts, comparable ExperimentContract v2 studies, and content-free harness
  effectiveness telemetry
- **Project setup** — start router, spec interview, `general`/`ml`/`slurm`
  templates, a doctor that validates the three managed rule/config surfaces,
  agent files hidden from git locally
- **Asking other agents** — durable peer threads, three-model councils and
  review gates backstopped by the project's own checks, decision-point
  advisors, outbound scrubbing. Judging seats are given less on purpose: a
  four-tool read belt, no MCP surface, evidence without the author's
  rationale — and a seat that died or was cut off is named in the
  synthesis, never quoted as one more opinion
- **Delegating writes** — isolated worktree delegation (`--read-only` for
  audits that must not produce a patch), an admission ladder, one mutation
  boundary, hash-frozen executor souls and worker-authority fingerprints
  with snapshot-backed authority repair on violation, primary authority
  receipts withheld from write children, and no recursive delegation — a
  worker asking for another peer is refused server-side and told to report
  the need instead
- **Agent state and handoff** — Work Journal with daily summaries and an
  optional Notion mirror, a prioritized attention inbox, pre-compaction
  handoffs, source-validated shared memory, reversible tracked-state
  checkpoints, task packets with executed verification, and a failure ledger
  that names an advisor after repeats, plus commit intents that resume after a
  landing interruption without calling the provider again
- **ML and HPC** — run spine and reproducibility capsules, pre-registered
  hypothesis runs, dataset leakage manifests, Slurm reconcile and GPU queue
- **Providers and models** — cached capability probing, provider defaults or
  exact model/effort selection, opt-in one-shot capacity fallback,
  family-diversity diagnostics, content-free native activity/token
  telemetry when providers expose it, and transports that carry the
  provider's own stop reason — an answer cut off at a token limit fails
  closed instead of reading as finished
- **Operations and execution boundaries** — durable attempt events with
  child-attempt resume, a bounded supervisor, one-use approvals,
  `trusted-local`/`isolated`/`remote` preflight and executable backends,
  optional Herdr control and VS Code/Stably Orca/Codex launchers, a read-only
  cockpit, local OTLP JSONL, advisory semantic evaluation, and an agent-managed
  bounded coding loop whose only built-in remote-write path creates a branch
  plus Draft PR
- **Maintenance** — transactional update with rollback, explicit stable/edge
  channel projection, doctor, one full verification gate plus a
  protected-branch quick pre-push mode; append-only state compacts under gc
  and every agent-facing projection is bounded, with the omission stated
  rather than silent

Skills load in three layers: general-purpose skills everywhere,
machine-conditional skills only where their command exists (`oms-slurm`,
`oms-gpu-workstation`), and per-repo skills forged into `.oms/skills/`. Global
skills are named `oms-*` so they cannot collide with your own skills in the
shared skill roots. Newly forged project skills are also `oms-*`; stored legacy
names remain readable and are not renamed automatically.

## Notes

- Local-first: use local files and CLIs by default. Connectors are allowed when explicitly requested or local sources cannot answer reliably.
- Never commit tokens, private data, or cluster/machine details — per-project
  agent files stay out of git by design. Portable runtime capsules are sanitized
  and advisory; raw `.oms` state is intentionally not synchronized.
- Scripts live in `~/.oh-my-setting/scripts/`, reachable as `oms <tool>`.
  Documented for transparency and recovery, not for manual use.

## Star

If this helped: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
