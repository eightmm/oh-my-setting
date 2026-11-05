# Migration notes

## 0.7

Version 0.7 opens the front of the autonomy chain — a vague sentence
becomes a reviewed intent spec — and closes two gaps behind it: a run can
now be re-entered from a fresh session, and a consented landing may
refreeze the definition of done it actually touched. Alongside that, every
verb keeps exactly one spelling. Stored state keeps its shape; one command
surface changes.

### Retired second spellings (the one breaking change)

The dispatcher used to accept two names for five tools. The alias is gone
rather than redirected, so the retired spelling now fails like any unknown
tool. Use the surviving name — the one nearly every caller already used:

| Retired | Use |
| --- | --- |
| `oms repo-state` | `oms state` |
| `oms agent-thread` | `oms thread` |
| `oms runtime-core` | `oms runtime` |
| `oms oms-init` | `oms init` |
| `oms oms-run` | `oms run` |

Scripts under `scripts/` moved with their verbs (`repo-state.sh` →
`state.sh`, and so on); anything calling those paths directly needs the new
filename. Typed data keeps the old labels on purpose: rows already written
with `"tool":"oms-run"` stay readable, and `state-verify`'s `runtime-core`
finding family and `inbox`'s `runtime-core-unavailable` key are consumer
contracts, not command names.

### Project skills carry the oms marker

`oms skill-forge add` now refuses a new project skill whose name lacks the
`oms-` prefix and names the corrected spelling. Skills already stored under
an unprefixed name keep validating, linking, and loading — only the add
gate is strict. The ML template's two project skills ship as
`oms-ml-experiment` and `oms-dataset-safety`; a project that installed them
under the old names keeps those, and re-applying the template will not
install a prefixed duplicate beside them.

The rule is about shared namespaces: forged skills land in `.claude/skills`
and `.agents/skills` next to skills you own, so the marker says which are
the harness's. The `oms` command's own subcommands are already inside that
marker and are never prefixed again.

### New doors (nothing migrates)

- `oms intent draft` turns a goal sentence into a reviewed intent spec and
  `oms intent adopt` is the only writer of `PROJECT.md`. Adopt refuses an
  acceptance command that already passes: a check that passes before the
  work cannot prove the work.
- `oms autopilot reenter` resumes a live run from a fresh session with no
  re-explanation, refusing while any claimant is alive and requeueing the
  leases a provably dead session still held. `oms autopilot abandon` is the
  append-only exit for a contract that cannot finish.
- `oms autopilot shadow` records what re-entry would have decided without
  deciding it. The session-start hook calls it when a live receipt exists;
  it writes only to `.oms/plan/autopilot-shadow.jsonl`.
- `oms autopilot --allow-verifier-change` binds verifier-change consent
  into the run contract, and a consented landing refreezes exactly the
  acceptance-manifest entries its own patch touched.
- `oms autopilot --allow-floor-change REASON` binds immutable owner consent
  at propose time and forwards the exact reason through goal-drive and plan-run
  to patch-land/patch-admit. Candidate checks still apply; changing, adding or
  dropping the reason requires abandoning the run and proposing a fresh contract.

### Korean output, if you want it

An installed `oms-korean` output style and `OMS_ANSWER_LANGUAGE=ko` apply
clear-Korean rules to final human-facing answers only — never to thinking,
commits, code, or identifiers. Both are opt-in: unset, nothing changes.

## 0.6

Version 0.6 makes the peer transports carry why the model stopped, narrows
what a judging seat is given, and stops append-only state from growing
unbounded. Nothing migrates automatically; no stored state changes shape.

### One workflow-visible default flip

- **`oms consult` no longer attaches shared memory by default.** Shared
  memory holds prior conclusions, and a second opinion anchored on the
  first is not a second opinion. Pass `--memory` to restore the old
  behavior for one call; the active task packet still rides by default.
  This matches peer-ask, peer-review, and agent-call, which were already
  opt-in.

### Transports (transparent to callers)

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

### State growth

- `oms gc` now also compacts the lifecycle event stream: attempts that are
  terminal and quiet past `--days` drop as whole streams (dry-run reports,
  `--apply` acts; survivors must still project or nothing is touched).
- Projections are bounded at the MCP mouth: `oms_agent_operations` returns
  the most recent 40 attempts, `oms_fail_ledger` the 30 most recently
  active fingerprints, each stating its omission. The full history stays
  on disk; `fail-ledger list` without `--limit` reads all of it.
- `tsp-queue logs` prints a bounded tail by default; `logs --full <id>`
  is the old whole-log read, and `oms job-digest` the structured one.

### Detection

- Project style detection no longer has an rg fast path: `find -maxdepth 3`
  is the single contract on every machine. A repository whose only ML or
  Slurm signal sits deeper than three levels detected differently per
  machine before; it now detects `general` everywhere — pass the style
  explicitly to `apply-project-template` if that is wrong for a repo.

## 0.5

Version 0.5 integrates the Typed Runtime Core directly into the product and
makes installation capability-scoped. Nothing migrates automatically; every
change below is opt-in or backward compatible.

### Capability profiles

A fresh 0.5 install defaults to the `core` capability profile: Bash, Git,
Python, and exactly one coding-agent provider, with no service logins. A
private capability receipt records the selection.

- **Existing installs change nothing.** An install with no capability receipt
  keeps the legacy full-tool update path byte for byte.
- **Opting in** is one explicit apply — `oms install-profile --apply
  --profile core --profile github` (choose your own set). From then on
  `oms update` refreshes exactly that selection.
- **Rolling back** is deleting the receipt
  (`~/.config/oh-my-setting/capabilities.json`); the next update restores
  legacy full-tool behavior. Uninstall removes the receipt with the install.
- `--full` remains the explicit compatibility install with the historical
  all-tools footprint.
- With a receipt present, doctor names each absence precisely: a selected
  capability's missing tool is a defect; an unselected capability reads as
  "not installed" with the exact add command. An installed CLI is always
  usable regardless of selection.

### Typed runtime projection

`oms runtime` (envelope, evidence, context, profile, release, doctor,
capsule, experiment, failure) works directly on the checkout. `oms state`,
`oms inbox`, and state-verify read the runtime projection; a broken
projection is reported fail-closed, never guessed around. Runtime state is
projection or evidence only — it grants no mutation, landing, commit, push,
or publication authority.

### Update channels

`config/update-channels.json` is an advisory projection surface read from the
checkout's own copy (`oms runtime release status`); `scripts/update.sh` never
consults it and `auto_apply` stays false. The `stable` channel names the
release commit by exact SHA; the promotion that records a new stable lands in
a follow-up commit, so a checkout sitting exactly on a release commit reads
the previous stable in its own manifest — one commit of skew by design, and
advisory either way.

### Failure taxonomy

Fail-ledger rows now carry a canonical `failure_code` and `recovery` beside
the free-form reason, and `fail-ledger check` surfaces them in every refusal
built on it. Rows written before 0.5 lack the fields and stay readable.

## 0.4

Version 0.4 makes install intent explicit and supports source refs as
reproducible entrypoints.

### Choose a channel

- Current: use the raw `main/install.sh` installer. Its default `edge` channel
  follows the repository's remote default branch.
- Reproducible pin: pass an exact tag, branch, or commit with `--ref`.

`--ref` overrides `OH_MY_SETTING_REF`; the environment overrides the embedded
installer default. A pin is checked out detached at an exact commit. `edge`
checks out and fast-forwards the remote default branch. An unknown or unsafe
ref fails before provider links or hooks are changed.

### Installation profiles

The default profile is `minimal`. `--full` records `full`; selecting individual
components such as `--tools` or `--auto-update` records `custom`. The concrete
component flags remain authoritative. Automation can set
`OH_MY_SETTING_PROFILE=minimal|full|custom` explicitly.

These receipt profiles change components, not skills. Every installation links
the same general-purpose skill catalog.

The schema-2 install receipt persists the selected ref and concrete component
choices. A plain later update therefore does not silently re-enable a Claude
hook, Codex plugin, or update timer that was disabled at install time. Existing
schema-1 receipts remain readable and are atomically rewritten on the first
successful 0.4 relink/update.

### Transactional updates

`oms update` now requires a clean canonical checkout. It records the previous
successful commit and reconciles links, hooks, plugin cache, and doctor before
committing the update. A link or doctor failure restores the previous HEAD,
links, and receipt. Use `oms update --check` for a read-only fetch comparison
and `oms update --rollback` to return to the recorded prior success.

### Removed compatibility surfaces

| Removed in 0.4 | Replacement |
|---|---|
| `multi-agent-ask.sh` | `oms peer-ask` |
| `multi-agent-review.sh` | `oms peer-review` |
| `multi-agent-delegate.sh` | `oms peer-delegate` |
| `OMS_MULTI_AGENT_*` | the matching `OMS_PEER_*` variable (legacy names are now ignored, not errors) |
| `workflows/spec-first.md` | `spec-interview` skill |
| `workflows/slurm-hpc.md` | `project-slurm-AGENTS.md` plus `oms snapshot --cluster` |
| `workflows/new-server.md` | chat-driven installer and project bootstrap |

Legacy environment variables fail with an explicit replacement instead of
silently changing timeout behavior. During relink/uninstall, an OMS-owned
`~/.oh-my-setting-workflows` link is removed and its newest user backup is
restored; foreign links and regular files are preserved.

### Source install

The public installation path follows the repository source. Pin an audited
commit when reproducibility matters:

```bash
curl -fsSLO https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh
bash install.sh --ref <commit>
```

No `.oms` project data migration is required. Existing managed installs can
switch channel by rerunning an installer with the desired `--ref`.
