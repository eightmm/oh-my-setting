# Components

oh-my-setting is a local control plane for installed coding-agent CLIs. Codex,
Claude Code, and Antigravity remain its managed core. Cursor, Grok Build,
Gemini CLI, Qwen Code, OpenCode, DeepSeek Harness, Mistral Vibe, Pi, GitHub
Copilot CLI, Factory Droid, Aider, and marker-prefixed custom adapters are
detected as optional transports. It gives them shared rules, skills, state,
peer calls, isolated write delegation, and one verification boundary for
OMS-managed delegated patches.
The default subsystem catalog is `oms list` (also `--frontdoor`); core
primitives and intent variants remain in `oms list --all`. Use
`oms <tool> --help` for flags and the
[command-routing reference](../custom-skills/oms-agent-harness/references/command-routing.md)
when two commands appear to overlap.

### Consolidated paths

Agent guidance groups discovery into state, collaboration, implementation,
verification/landing, continuity, and operations. Public commands remain
compatible; these groups do not grant combined authority.

- Inbox projects shared state; detail-only lifecycle/approval reads remain
  separate because summaries truncate those lists. Reports are non-atomic
  across underlying stores. State and runtime reuse validated task/plan results
  within one query, without a persistent status cache or another CLI input.
- Recent log windows read backwards; MCP display reads at most 64 KiB before
  its existing output cap. Full artifact/thread references stream once so an
  early result is not lost. Peer and MCP share final Output/Exit interpretation.
- Runtime context reuses fresh Project Graph evidence selection, preserving
  explicit inputs and budgets. Missing/stale/unsupported graphs retain local
  discovery; read-only context neither builds graphs nor grants authority.
  Private delegation packs use the same freshness/path checks after their
  bounded build; they never refresh the parent's graph from a worker tree.
- Consult and advice share exact call-receipt validation; absent or ambiguous
  receipts cannot borrow the newest artifact. Advice is a prompt variant using
  the shared model router, not a separate fixed-high-effort routing policy.
  Consult prints the current result without replaying recent thread history;
  the thread retains that history for follow-up calls. Advice injects its
  bundled decision contract once, retaining the contract for custom roles.
- Tick and install auto-update share scheduler detection/selection while
  preserving distinct schedules, ownership markers, and repair transactions.
- Diagnostic, council, autonomy, profile, experiment, and continuity variants
  remain behind the existing front doors. Already shared engines are reused,
  not replaced with another orchestration layer or a combined memory store.

### Surface boundaries

Use `oms list --all` for the current catalog. Keep distinct authority and
provenance boundaries; remove duplicated routing and generated boilerplate.

| Surface | Decision |
|---|---|
| State, threads, memory, journal, graph/context | Keep shared evidence and incremental collaboration; reuse injected resume/digest context before extra journal reads. |
| Plans, delegation, admission, recovery, release | Keep leases, reviewed bytes and explicit authority; model capability cannot replace concurrency or recovery guarantees. |
| Install, update, repair, scheduler | Keep reversible ownership. Tick's backlog and current-day journal sync have different scopes. |
| Councils, runtime experiments, HPC, external frontends | Keep explicit workflows; no new default calls. One peer precedes a council when sufficient. |
| Daily and task-close skill-creation recommendations | Removed: resolved failures and frequent tool use alone do not justify another skill. Explicit forge and existing skill verification reminders remain. |
| Tool-family telemetry | Opt-in with `OMS_USAGE_TRACK=1`; historical usage remains readable and GC-managed. No daily scan of usage/skill bodies to recommend new skills. |
| Skill selection and onboarding | ASCII trigger boundaries prevent `oom` matching `room`; generic continuation/model questions no longer force the harness. Keep specific collaboration/graph/autopilot triggers. Store ordinary build/test facts in the existing contract. |
| Generic keyword skill hints | Off by default; native skill discovery remains. `OMS_SKILL_HINTS=1` restores keyword hints without changing live collaboration or journal delivery. |
| Stop session budgets | Answer-format blocking is removed, including legacy opt-ins. Positive `OMS_SESSION_BUDGET_TURNS` or `OMS_SESSION_BUDGET_HOURS` enables a session cap. Journal finalization remains. |
| External adapters | Removed Herdr pane control and the standalone A2A HTTP bridge/card. Native peer calls, live threads, and CLI/MCP state reads remain; external clients of the removed adapters must migrate. No extras dispatcher remains. |
| Standalone semantic evaluation | Retired executable engine; use current-HEAD `patch-admit` checks and `peer-review` with the minimal-change rubric. Historical reports remain readable. |

The seven skills retain distinct installation, coordination, specification,
diagnosis, security, Slurm and local-GPU responsibilities. Combining them would
load unrelated procedures or blur safety boundaries. Trace requires competing
hypotheses only when real alternatives remain, not a fixed worksheet.
Retired daily hint knobs (`OMS_USAGE_HINT_MIN`, `OMS_USAGE_HINT_DAYS`,
`OMS_USAGE_SKILL_ROOTS`) no longer affect prompts; no stored skills or history
are deleted. `OMS_SKILL_FORGE_HINT` still controls existing skill-contract
verification reminders at task close.

## How it fits together

```text
user request
  -> provider skill/router
  -> oms command
       -> read: consult / peer-ask / peer-review / advise
       -> write: peer-delegate -> isolated worktree -> patch
                                      -> patch-admit -> patch-land
       -> orchestrate: agent-supervisor -> agent-events -> approval-inbox
       -> operate: runtime backend / open-in
       -> observe: inbox / targeted state queries / otel-export
  -> local state in .oms/
       -> inbox / state / handoff / MCP / Work Journal
```

The owning agent remains responsible for scope, admission, verification,
commit, push, and release decisions. Peer agreement is evidence, not approval.

## Install and ownership

Systemd auto-update setup stages unit files and restores prior files, timer
enablement/activity and any newly enabled linger if activation fails. Failed
restoration retains the backup and reports its location; this cannot undo an
update process that already started. CI prepares ShellCheck lazily, so a full
selection does not download it again in the planning job, and parallel narrow
checks share one verified download.
Selected smoke cases also use the existing parallel runner: selection is
validated before dispatch, partitioned without duplicates, and capped to the
selected case count. No test coverage is removed. CI cancellation groups
include the event as well as the ref, so scheduled/manual checks cannot cancel
a push check awaited by landing; newer pushes still replace obsolete pushes
([GitHub concurrency](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency)).
Native platform checks share the existing lifecycle matrix's macOS/Windows
runners instead of provisioning two extra jobs. The same checks remain gated;
`!cancelled()` lets later checks report failures after an earlier failure
without continuing a cancelled run
([GitHub status functions](https://docs.github.com/en/actions/reference/workflows-and-actions/expressions#status-check-functions)).

The default installer selects the `core` capability: Bash, Git, Python, the
harness, and one coding-agent provider. It installs or verifies only the locked
tools required by the selected profile; optional capabilities add the council,
GitHub, Notion, research, HPC, container, or remote surfaces. Setup fails rather
than silently recording a selected profile with missing required CLIs. Required
provider dependencies are part of that plan: Codex and Claude include Node,
while Antigravity does not. Bootstrap versions, URLs, and integrity values live in
`tools.lock.json`. Providers resolve official stable releases during installation
and updates; a private `provider-tools.lock.json` records the resolved checksum
snapshot used by the installer and doctor, not a permanent version pin.
Explicit `OH_MY_SETTING_TOOL_LOCK` keeps reproducible pinned operation.
Recognized standalone Codex uses its native updater and keeps its launcher;
OMS checks the resulting version, not a provider-owned payload digest.
Direct platform payloads and npm wrapper/native packages are
verified before use and npm installation runs offline with a fresh cache.
Install, update, repair, and uninstall share one user-wide lifecycle lock. Lock
ownership survives installer `exec` handoff. A checkout-selected foreground
child may borrow the live parent's critical section only after matching its
owner, PID, start token, and canonical path; ownership and release authority
remain parent-only, and the borrow cannot pass to another child.
An already-installed external CLI at the exact version is reused as
version-only evidence; doctor distinguishes it from a digest-owned install.
Managed direct binaries and npm package/shim swaps carry recovery state, and
doctor reports digest drift or an interrupted transaction separately from lock
schema validity. PATH persistence follows Bash's existing login-file priority
instead of creating a higher-priority profile.
Optional provider registrations report a warning and remain visible to `oms
doctor --contract`. Windows Git Bash requires the exact locked native Node
release before setup when the selected profile needs a Node-backed tool; Linux
and macOS can install it during setup.

Managed rules and skills use symlinks on Linux/macOS and verified copies on
Windows Git Bash. Existing user files move to timestamped backups and return
on `oms uninstall`. Uninstall stops before unlink or purge if any integration
cannot be removed. It intentionally leaves external CLIs and the user-local
PATH entry installed.

The source installer follows `edge` by default. The daily updater applies only
clean fast-forwards and skips dirty or diverged checkouts. Set
`OH_MY_SETTING_AUTO_UPDATE_MODE=check` for notification-only updates, or pin a
ref when reproducibility matters. A missing pinned ref fails closed unless the
operator explicitly chooses `--fallback-to-edge`.

Useful maintenance commands:

```bash
oms status
oms doctor
oms doctor --repo . --json
oms doctor --repo . --remediation-plan
oms update
oms uninstall
```

Structured doctor reports adapt the existing checks; they do not create a
second health engine. Findings carry stable IDs, severity, and content-free
remediation metadata. `--remediation-plan` prints commands and their authority
class but never executes them, and it cannot be combined with `--repair`.
Doctor checks installed Git HEAD/ref validity without fetching or rewriting
refs. Damaged references require targeted recovery with preserved originals;
link repair is not Git recovery.

`oms status` separates the active OMS workspace revision/changed-entry count
from the canonical installation's actual revision/changed-entry count. Alignment
is `in-sync`, `pending-source-changes`, `different-revision`,
`installed-uncommitted`, or `unknown`; receipt SHA alone does not prove live
checkout identity. This display neither updates an installation nor proves
plugin-cache freshness or verification.

## Project onboarding

Say “Start this project.” or “이 프로젝트 시작해줘.” The
`oms-spec-interview` skill routes by state:

- Empty directory: clarify the goal, write `PROJECT.md`, apply a template, run
  the project doctor.
- Existing repository: inspect first, then ask only about gaps that change the
  implementation.
- Ongoing harness project: report current state and the next actionable step.

`general`, `ml`, and `slurm` templates add a managed policy block and a small
verification contract without overwriting user content. `oms project-private`
keeps `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, and `PROJECT.md` out of commits via
`.git/info/exclude`; tracked files require an explicit untrack action.

```bash
oms init
oms apply-project-template general .
oms project-doctor .
oms project-private --check
```

## Local skills and optional bundle operations

Use `oms skill-forge add|validate|link` for ordinary local guidance. Neither
external bundle management nor evaluation is required for a local edit.
`oms skill-forge` remains the single project-skill authority. Local authored
skills live under `.oms/skills`; explicitly imported bundles first pass a bounded,
no-follow preview and then live as content-addressed immutable revisions under
`.oms/skill-store`. Import, update, and rollback require explicit apply plus
digest CAS. They reject links, hardlinks, nonregular entries, sensitive file
names/content, embedded URL credentials, oversized trees, and an unprefixed
skill name. Import never executes a bundle script.

```bash
oms skill-forge preview --source PATH_OR_GIT --json
oms skill-forge import --source PATH_OR_GIT --expected-bundle-sha256 SHA --apply
oms skill-forge update --source PATH_OR_GIT --expected-current-sha256 OLD --expected-bundle-sha256 NEW --apply
oms skill-forge rollback oms-NAME --to SHA --expected-current-sha256 CURRENT --apply
```

Skill evaluation is an explicit black-box experiment. A suite declares near-
miss trigger cases and baseline/treatment task commands; host commands run only
with `--allow-host-commands`. The result contains aggregate counts and command
digests, never prompts or outputs. `--record` appends content-free metrics that
`oms runtime benchmark` projects. Write useful guidance directly from reviewed
source evidence, then use `skill-forge add|link` for local skills or the
preview/import flow for bundles. Automatic draft generation is not required.

## Asking other agents

| Need | Front door | Result |
|---|---|---|
| One independent read or continued peer thread | `oms consult [--to PROVIDER]` | Threaded answer |
| Council or debate | `oms peer-ask` | Per-seat artifacts and family count |
| Diff review | `oms peer-review --gate` | Verdicts plus mechanical backstop |
| High-risk decision | `oms advise` | Adversarial recommendation |

Debate rounds after the initial independent answers share a 64 KiB prompt
budget across all seats (`OMS_DEBATE_ROUND_BYTES`). Each fresh call receives
section-balanced excerpts of current claims/findings, evidence, changes and
remaining disagreements, including its own position. Structured excerpts use
at most 4 KiB each; freeform and undersized section budgets use a bounded
head-and-tail excerpt, retaining the leading conclusion and final objections.
Original artifacts remain linked, and omitted content is marked rather than
presented as a complete summary. All prompts are prepared before any seat is
started; a frame too large to retain evidence fails without partial calls.
Within a round, identical artifact/quota pairs reuse the sanitized excerpt;
different quotas and later rounds are processed separately. This reduces local
processing, not the number of model calls or the prompt's token count.
Round-byte reporting excludes provider-native context, reads, retries and
synthesis, and must not be reported as measured token savings.
An agy print-timeout diagnostic is incomplete output even with exit status zero;
it cannot count as an answered seat, enter a later round, or pass a review gate.
Excerpts recognize plain and Markdown section headings without treating fenced
or indented code and `>`-quoted examples as new answers; short sections donate unused space
to longer ones. Early stopping requires every called seat to answer and its
entire change section to explicitly declare no change. A dropped seat's last
good answer remains available, labeled as stale in the synthesis.

Councils with `--thread ID` publish every completed turn without waiting for
slower seats. Opening answers and rebuttal rounds remain parallel, with one
prior-round evidence/notes snapshot and all-seat budget preflight per round.
Coordinator notes/decisions use `thread context --notes-only` (2 KiB, eight
turns); answers and provider failure notes are not duplicated there. Notes
arriving during a round apply only if another configured round runs.
MCP `kind=ask` returns a ready thread and accepts `debate_rounds=0..3` (default
zero); its suggested operation wait is zero while exchanging messages.
Threads stay open for authorized focused follow-up through `consult`; neither
notes nor reads start new calls. CLI export-only does not create/publish turns.
Closing a council rejects new CLI/MCP council starts and its next configured
round before provider dispatch. Ordinary history reads remain available;
already-running calls are not interrupted. `thread context --require-open`
is the shared preflight, not a lock across an entire provider call.
There is one parallel council, not a separate sequential scheduler, persistent
provider session, token stream, or mid-generation interruption.

Claude read calls retain public assistant text from `stream-json --verbose`
alongside the terminal result, so a closing remark cannot erase earlier
evidence. User/tool/thinking/subagent events are not answers; absent terminal
results are marked truncated. Existing JSON envelopes and write/interactive
transports remain supported, with unchanged permissions. The transport follows
the [official streaming contract](https://code.claude.com/docs/en/headless#stream-responses);
fixture validation is not a live-provider guarantee or measured cost saving.
Initial and subsequent debate prompts share evidence/verification framing,
while honoring explicitly requested answer formats. Claims use stable IDs and
provider-reported confirmed/refuted/unverified labels; agreement never promotes
them to owner-verified evidence. Retractions must accompany subsequent claims.
Each run keeps a bounded, sanitized initial request and answer copies, mirrored
into isolated read worktrees. Tracked base/state identity is shared and a tracked
source change stops later rounds. Base plus supplied diff is the shared view;
untracked or omitted bytes remain explicit uncertainty, not an identical full
checkout promise. The copies are removed after the run; original artifacts stay.
Usage details retain input/output and cache-read/write counts with unknowns as
null. Codex cache reads are included in its input count; Claude reports caches
separately. Historical `tokens used` semantics remain unchanged. Details are
cached in the existing artifact index, not a new store or a billable-cost claim.

Read calls scan outbound context for credentials and machine-sensitive data.
`--export-only` writes a prompt without invoking another CLI; import the answer
through `oms artifact-index import`.

The artifact index is a repo-bound, no-follow, copy-on-write JSONL store.
Normal views and mutations reject structural corruption. Agent recovery is
two-step: `oms artifact-index salvage` plans without writes, while
`salvage --apply` preserves the exact raw bytes in a content-addressed 0600
quarantine before one CAS repair and receipt. Legacy schema repair remains the
separate `migrate` operation. A row over 1 MiB or recovery snapshot over 256
MiB is refused without mutation. A write that would exceed the 16 MiB healthy
ceiling first applies lineage-aware retention and refuses only when its required
new rows still cannot fit; salvage uses the same output ceiling. Its receipt
references the raw quarantine, so ordinary artifact retention preserves it
until that receipt is pruned.

Provider count is not independence. Two routes backed by the same model family
remain one opinion for diversity reporting.

Provider discovery separates an executable transport from its model family.
`glm` is therefore never guessed as a provider: a GLM route names its carrier
and exact model, for example `opencode:model=zai/glm-4.7`. Likewise,
`deepseek` names the official DeepSeek Harness transport, while a DeepSeek model
behind Aider is expressed as `aider:model=deepseek/deepseek-chat` (or the exact
model name configured by that carrier).

`oms models --providers auto` is a no-exec physical inventory; its `usable`
field is unknown until `--refresh`. Refresh and `model-doctor --providers auto`
run bounded local version/help probes without inference, login, install, update,
or configuration changes. A visible but broken shim remains in diagnostics as
`broken`, while automatic routing admits only a probe-proven usable transport.
Optional agents do not become default council seats merely because they appear
on PATH: a custom adapter is excluded from every automatic pool (advisor
auto-pick, consult auto-pick and failover) and runs only when named with `--to`
or under an explicit `consult --all` fan-out.

Provider-native user, organization, and previously trusted project
configuration remains part of the trusted host boundary. OMS pins the
documented noninteractive mode, tool/permission surface, and outer worktree
fences, but those flags are not a substitute for reviewing native hooks,
plugins, MCP servers, credentials, or managed policy loaded by the provider.

Custom transports must be executable as `oms-agent-adapter-ID` on PATH or in
the configured adapter directory. Their protocol is a prompt-file based
`run --access read|write --workdir PATH` call. Read calls use a disposable
worktree. Write is refused unless `ID` appears in
`OMS_PROVIDER_WRITE_ADAPTERS`, then still returns through the normal isolated
delegation and patch gate. The adapter remains a trusted host executable, not
an OS sandbox, and carries no landing, commit, push, or publication authority.

A provider subprocess is a harness child, not a new owner. Child-marked
processes may inspect saved review verdicts, but every peer-call and delegation
front door refuses a recursive start. The child reports that another opinion or
worker is needed and the parent decides whether to spend or fan out.
Global or explicit-file memory source/derived-state changes, global role
creation, journal configuration/sync, model-capability refresh, and host queue
enqueue/cancel are parent-owned as well. Pure inspection and project-local
counterparts remain available where applicable.

## Models and reasoning effort

Routing is deliberately small:

- No `--model`: use the provider default.
- `--model NAME`: use that exact model with no implicit switch.
- DeepSeek Harness and Vibe currently expose no documented per-invocation model
  selector on their OMS headless surfaces. An explicit `--model` for either is
  refused instead of being silently ignored; configure the native profile or
  omit the flag.
- `--fallback-model NAME`: on a recognized capacity error, retry once with
  that model. A write attempt that changed its worktree is never retried.
- An unpinned provider-default route may use bounded catalog recovery for a
  model safeguard or unavailable-name error.
- Recognized policy declines are terminal. Authentication, permission, context,
  and verification failures are not model-capacity fallbacks.

`--reasoning-effort` accepts `auto`, `low`, `medium`, `high`, `xhigh`, `max`,
or `ultra`, then validates the value against the selected provider/model
capability snapshot. `oms models` reads the cache; `oms models --refresh` and
`oms model-doctor` perform explicit probes.
Cached catalog reads separate tab-delimited model IDs from display labels and
discard listing banners, so labels cannot leak into automatic recovery routes.

Artifacts record requested and selected models, reasoning effort, fallback use,
and reason.

## Delegating writes

Choose `oms consult` (or the lower-level `agent-call`) for a read and
`oms peer-delegate` for a patch. The `agent-run` wrapper and keyword mode
classifier were removed. Task-aware calls/delegates record outcomes directly;
existing artifact and task history is preserved.

A write delegation:

1. Creates a detached worktree from the current `HEAD` under the private
   `${XDG_CACHE_HOME:-$HOME/.cache}/oh-my-setting/worktrees` root (override
   with `OMS_DELEGATE_WORKTREE_ROOT`).
2. Injects the brief, optional role, and bounded context.
3. Runs the provider and the declared verification command.
4. Returns an artifact and binary-safe patch; it does not commit or push.
5. Optionally uses bounded repair rounds with the prior failure attached.

Explicit `--verify` and plan-task checks win. Otherwise writes default only to
an implemented `scripts/check.sh fast`; ML project detection does not select a
heavier suite. A check script without `fast` requires a chosen verification
command or an explicit `--no-verify`, before graph work or provider startup.
Review and patch admission use the same selector. Admission has no implicit
full-suite fallback or no-verify override; choose an explicit command when
`fast` is absent. An explicit ML preset still selects an available `ml-smoke`.
Read-only reports do not auto-run tests. Missing or blocked providers skip
verification with a non-success result, rather than test unchanged code.

With `--repair 2|3`, compact candidate/diagnostic hashes and exit codes go into
the existing artifact-index `repair_replay` field. Replay compares the requested
budget with stopping consecutive identical failures; single-repair runs skip
this overhead. Selection requires 12 distinct prompts with the same base SHA,
selected model/effort/provider, verifier command and cap. Latest complete baseline
runs form chronological two-thirds development and one-third held-out sets;
both must save calls. Any recorded success lost by early stopping, including
duplicate-prompt reruns, vetoes selection. After four challenger runs, the next
run retains the original cap to replenish uncensored evidence. These thresholds
are OMS heuristics, not paper results or statistical guarantees.

Reads are bounded to 1000 rows/2MiB. Censored failures, sparse/invalid history,
unknown models and fallback routes cannot justify early stopping. A stop remains
a failure. `OMS_REPAIR_REPLAY=0` retains the cap and records outcomes without
reading history; `oms artifact-index telemetry` reports recorded/delegation
counts, selections and stops, not a measured token or wall-clock saving.
No extra run, timer, model, test omission or automatic delegation is introduced.
Explicit repeated-failure advice bypasses replay: a different agent contributes
to that trajectory and its cost, so single-worker evidence is not comparable.

This is a narrow adaptation of [Dream-RSI §3](https://arxiv.org/html/2609.14858v1#S3),
not a reproduction of its full exploration system: two fixed stopping policies,
no generated policy code, no concurrency optimization. The [upstream repository](https://github.com/zhengkid/Dream-RSI)
listed its full implementation as pending on 2026-09-15. Historical non-regression
does not guarantee future task success or equivalent environments; a changed
base/verifier/model cohort starts without transferable evidence.

The worker does not see the caller's uncommitted changes. `patch-admit` checks
applicability, path scope, sensitive content, syntax, and the stored test
contract in another temporary worktree. If a patch changes its verifier,
tests, or common verification config, admission also runs the same command in
a second projection with that verification surface restored from HEAD.
Conventional repo-owned check/test/verify/lint helpers are included even when
the top-level command does not name them directly. A failed pre-verification
policy/integrity gate skips project-code execution; an explicit override opts
into the corresponding candidate run.
`patch-land` is the single mutation boundary and requires a clean main tree
(including untracked files) plus lease checks. A policy can require a one-use,
exact-action approval; `--request-approval` creates the request without applying
the patch.

Write providers receive task context but not the primary `.oms` pointer,
or attempt/lease capability. The default guard preserves
parallel agent state, rejects destructive rewrites, and binds the selected
task/lease objects exactly while other tasks may move. On a
violation the failing run also repairs the operation's own authority from its
hash-verified pre-launch snapshot: scope, verifier, and review evidence always restore, while claim-cycle fields restore
only under the operation's own lease and are otherwise kept and named. A
caller that guarantees no sibling writer may set
`OMS_WORKER_AUTHORITY_EXCLUSIVE=1` for complete authority-state comparison and
rollback. Plan landing and completion require the reviewed patch bytes,
verifier and lease to match exactly inside the
plan lock. The delegated checkout's physical identity and both Git
backpointers are rechecked
at every mutation boundary, including cleanup. Landing terminal rows close a
transaction only when their canonical receipt and durable plan, approval, and
lineage outcome also converge.

```bash
oms peer-delegate --to codex --prompt "Implement the bounded change."
oms patch-admit --patch path/to/change.patch  # optional read-only preview
oms patch-land --patch path/to/change.patch   # runs admission, then mutates
```

## Security boundary

The harness provides isolation and post-run detection, not a complete process
sandbox. It detects repository, git metadata, ignored-file, hook, and shared
state changes around a worker. It cannot prove what a process read, see a write
that was undone before exit, or contain writes outside the repository. An
ordinary provider/verifier shell can leave a background process. On POSIX,
`agent-supervisor` owns and closes one process group; a process that deliberately
starts a new session can escape it. On Windows, a kill-on-close Job Object owns
the launched process tree. Neither boundary is an OS sandbox.

Treat a clean worker result as “no monitored repository surface moved,” not
“the process was harmless.” Stronger guarantees require OS/container isolation.
The default provider permission profile remains practical rather than strict;
outbound scrubbing, isolated worktrees, admission, and verification are the
main controls. Provider and verifier wall clocks use `timeout`/`gtimeout` or a
POSIX Python fallback; without either mechanism the call is refused.

## State, memory, and handoff

Project state lives under ignored `.oms/` paths:

| State | Purpose |
|---|---|
| task and plan | Current goal, acceptance, DAG state, leases |
| memory | Scrubbed shared notes with provenance |
| artifacts | Calls, reviews, patches, verification, lineage |
| failures | Open/resolved failure fingerprints |
| threads | Cross-provider conversation history |
| handoffs | Bounded pre-compaction and session summaries |
| work-journal | Daily local activity and optional Notion mirror |

`oms state` summarizes the repository; `oms inbox` ranks items needing
attention. `oms fail-ledger list`, `oms artifact-index unresolved`, and
`oms thread list` expose the underlying records. Writers are append-oriented;
use migration and GC commands instead of editing JSONL by hand.

Stop materializes the journal locally. Its detached publisher starts only if
a Notion target is configured or GitHub CI polling is available for this repo.
Existing content hashes, sync locks and CI polling intervals deduplicate work
against scheduled maintenance; hosts without a timer keep the Stop fallback.
Native telemetry drops metricless routine activity even from old per-tool
registrations. Session boundaries and explicit failures remain; set
`OMS_TELEMETRY_DEBUG=1` only when empty activity is useful for diagnosis.

`.oms/` and project agent files are clone-local by design. A fresh clone needs
the template/private setup again; continuity is not silently copied between
machines.

Concurrent collaboration uses the same thread log: `thread new --live` opts a
scoped conversation into existing prompt/edit-hook delivery, `append` posts a
question or change, `updates --after CURSOR` reads complete new turns, and `ack`
records explicit consumption (not approval). Each hook session retains a small
delivery cursor, never a second transcript. Worktrees can address the canonical
log explicitly with `--repo`; no cross-machine sync or forced live-model
interruption is implied. MCP reuses `oms_peer_start` message/ack modes and
`oms_peer_result(thread, after)` without increasing its 12-tool core catalog.
Operation reads optionally wait up to 50 seconds (`wait_seconds`) for completion
inside the existing server; log activity does not wake that wait. Default reads
remain immediate and expiry never cancels or restarts the peer. The host timeout
must exceed the wait; this serial stdio server cannot handle another request
until it returns.
Start responses include `result_arguments` for that bounded read of the same
operation. Install/update distributes the shared waiting guidance through the
existing global-rule and skill links; it does not add polling hooks or overwrite
user-owned Codex `developer_instructions`. Configured ceilings permit longer
waits but do not enforce agent behavior or prove usage savings.
Operation results return a content `cursor`; passing it back as `after` omits
unchanged answer/log bodies while retaining status, exit and artifact references.
Omit it to reread the full bounded result. Elapsed time alone does not invalidate
the cursor; changed visible evidence, completion and failure do. This is stateless
conditional delivery, not automatic completion notification or acknowledgement.
Worker prompts omit the model catalog: model choice stays with the authorized
parent through `oms models`, not with non-delegating workers.
Read-call thread views omit an exactly repeated final question already supplied
separately; stored turns and earlier decisions stay intact. Repair briefs retain
the task and required context, but patches over 4 KiB are read from the preserved
staged worktree instead of embedding another copy. Artifact telemetry records
primary `prompt_bytes` separately from reported tokens and wall time; provider
context and retry/repair inputs are not included in this byte metric.
The detailed operating contract is in the harness state-memory reference.

## Plans and bounded autonomy

`oms plan-from-spec` proposes tasks from an active `PROJECT.md`; it refuses a
draft spec and does not alter the plan until the proposal is explicitly
applied. `oms plan-run` claims and executes at most one ready task. `oms
goal-drive` repeats within a cycle cap and acceptance command; it is not an
unbounded autonomous loop. Before landing, `goal-drive` freezes the reviewed
patch and appends a commit intent. A restart reconciles an unapplied, applied,
or already committed intent without calling the provider again. Publication
builds the frozen tree in a private index, creates the commit without repository
commit hooks, and compare-and-sets `HEAD` against the recorded parent while
holding Git's index lock. A staged index that differs from the frozen base is
preserved and parks the run instead of being overwritten. Repository hooks are
disabled for every child Git operation in the drive, not only the final ref
update. Frozen patch creation enters the validated physical directory, uses
relative writes, and atomically replaces the final leaf without following a
directory symlink; a changed absolute lookup parks before consumption.

`agent-plan status --json` is the canonical read-time plan decision. Its
additive `contract` object compares the reviewed `project_contract.spec_sha256`
with one bounded, no-follow snapshot of `PROJECT.md`, and its `actionable`
list already includes dependency, claim-expiry, and contract gates. A bound
plan whose project file is missing, draft, invalid, unreadable, or byte-drifted
cannot issue a new `ready`, `next`, or `claim` authority. Exact transitions for
an existing lease remain available so drift cannot strand running work or
prevent cleanup. Legacy plans without a reviewed contract keep their existing
claim behavior. Runtime, state, and inbox consume this decision rather than
reconstructing readiness from stored task states.

The change guard sends its sorted changed-path stream through the canonical
scope engine once. The engine loads and validates the stored allow/deny rules,
compiles each bounded Bash-compatible glob once, and emits one deny-first
verdict per path. The public single-path scope operations remain compatible;
the batch operation is an internal performance boundary for large diffs.

An active plan that became stale after equivalent work landed elsewhere is
removed through `oms agent-plan retire`, never by inventing task transitions.
The default command is a no-write check that returns the exact plan SHA-256.
Apply is parent-only and requires that SHA, a disposition, and a reason. For
`completed-external`, the command itself runs a fresh acceptance on one clean
committed HEAD and binds full plan/acceptance/output hashes, ref, file
generation, and a unique proof token. `superseded` is limited to unclaimed old
plans and explicitly claims no verification. Both preserve the exact bytes in
one disposition-independent content-addressed archive and append a typed
receipt before unlinking the exact live generation. Claimed, running, review,
landing, malformed/unproven markers, or a live task marker veto retirement.
Authority JSONL is strict LF/CRLF-delimited finite JSON with bounded rows, and
every receipt is cross-linked to its archive and, when claimed, its unique
acceptance row. Replaying the same operation completes an interrupted unlink;
a different nonempty plan lineage is untouched, while same/ambiguous lineage
fails closed. Canonical plan mutations also veto that same-lineage residual.
After unlink already completed, `init`/reviewed proposal apply may remove the
validated intent before creating a new plan. `state` and runtime expose the
latest receipt as display context; `state-verify` is the trust surface that
rejects malformed/duplicate receipts, proof or task-state contradictions,
residual intents, and active/retired lineage conflicts.

`oms autopilot` is an agent-side control plane: the end user states the goal and
authority while the top-level parent performs every transition below. It
proposes an initial plan for parent review, atomically applies only that exact
proposal, drives the approved tasks, and may propose one `r1-` remainder tranche
of at most two tasks. The
mechanical acceptance command remains the hard gate. A non-pass acceptance persists its bounded, normalized output body under `.oms/plan/acceptance/` keyed by the receipt row's output digest. Custom acceptance commands
bind their declared `Required check files` and reject verifier mutation; Draft
PR publication defaults cross-family semantic review to a blocking gate. Its
base is frozen to a commit before the drive, so a moved branch name cannot make
the whole-change review empty. Each drive ends with one unique canonical result
that binds its internal receipt, status, and reason; the durable terminal row
must match it before the remainder tranche is authorized. `propose` requires
the base; its printed continuation is shell-safe, retains every effective
option, and accepts only a regular non-symlink proposal snapshot of at most
1 MiB.
`oms intent adopt` likewise validates one frozen candidate, then publishes only
that snapshot after a locked SHA-256 and byte-for-byte comparison with the live
candidate; an editor save during acceptance leaves the draft intact and creates
no `PROJECT.md`.
With `--draft-pr`, `oms draft-pr` rechecks the clean HEAD, tree, remote base,
GitHub identity, write permission, and verifier before a create-only push and
Draft PR. Every introduced Git object, including trees, is scanned for
credential-shaped and machine-private content; defaults cap this at 20,000
objects, 32 MiB per object, 256 MiB total, and a 180-second scan budget with
fixed termination escalation; Git history traversal also runs under a 512 MiB
process-memory ceiling. Hosts without both controls park before traversal.
Temporary payloads are removed before parking.
Hidden/sparse index entries and Git grafts are refused. Its local intent records
a pre-push uncertainty phase, and verifier mutation terminally spends that
intent, so a deleted branch is not recreated
after an interrupted or tampered push; it cannot update an existing remote
branch, merge, mark ready, tag, or release. Starting on the base branch
creates a deterministic local `oms/autopilot-<spec-digest>` branch before the
first drive cycle, with or without Draft PR publication; any other checked-out
branch parks rather than being silently driven. Recovery branches use strict
`-rN` names, and the complete committed diff from the reviewed base must stay
inside the plan envelope. That selected branch is frozen through review, and a
base-branch restart parks when a matching recovery branch already exists
instead of creating a competing lineage. A failed or ambiguous
push spends that intent; retry from a new branch name rather than risk replaying
an effect whose absence cannot be proven permanently. The two publication
recovery cases: an interrupted but unspent intent replays with
`oms draft-pr --repo REPO --intent INTENT publish`; a spent or terminal intent
never replays. Repeated preparation prints that exact, shell-safe replay command
only for an unblocked regular intent; otherwise it parks without advertising
publication. For a spent intent, rename the work branch once
(`git branch -m oms/autopilot-<digest> oms/autopilot-<digest>-r2`, which the
checkout guard accepts); the parent then resumes review and preparation on the
new name. This recovery procedure is agent-facing, not an end-user handoff.

Task-specific behavior belongs in the brief, not a separately generated Soul.
Soul executors and their command are removed. Existing records and bound reviews
remain readable evidence but cannot authorize new execution or landing; create
a fresh reviewed task without editing away the old binding. GC preserves legacy
executor markers instead of attempting to resume or retire their contracts.
Default state/envelope queries no longer scan or advertise these retired files.
Normal task leases, scoped admission, one-use approvals and one-shot repair remain.

Each new autopilot receipt also binds an opaque run owner. Claims and worker
markers inherit it. Re-entry recovers only that owner's exact current
`claimed`/`running` leases under the plan lock: live markers, markerless
running work, another owner, and `review`/`landing` evidence are preserved.
Its owner and dead claimant come from one receipt-lock judgment token, never a
later ledger reread. Routine GC CASes the observed state and lease and vetoes
any exact live worker marker under the plan lock, so a retry or task that
advances during cleanup cannot be requeued by stale evidence. Windows liveness
binds the Git Bash PID to its native WINPID and uses a wait-only process handle;
it never probes by sending signal zero through Python. A missing legacy native
identity is preserved as unknown. GC treats markers as bounded, no-follow
evidence and deletes only an unchanged generation under its marker lock. More
than 4,096 marker entries makes the entire scan unproven and preserves every
entry; it never turns a partial enumeration into recovery authority.
Legacy receipts and tasks without an owner stay readable; owner-based re-entry
never guesses them, while exact state+lease GC remains backward compatible.

Recovery tools include:

```bash
oms checkpoint create --label "before risky edit"
oms fail-ledger list
oms artifact-index unresolved
oms artifact-index salvage
oms approval-inbox expire [--apply]
oms approval-inbox reconcile --older-than-seconds 300 [--apply]
oms patch-land --recover
oms gc
```

## Graphs

`oms graph` (`scripts/lib/oms_graph/`, standard library only) adds two
inspectable graphs above the plane described in the previous section; the
contract is `docs/GRAPH-ENGINEERING.md`.

The **Project Graph** (`oms graph project build|check|map|find|api|search|neighbors|trace|blast|affected|analyze|context`)
is a deterministic structural graph of the repository — file, module,
class/function/method, test, config, and document nodes with `contains`,
`imports`, `calls`, `references`, and `tests` edges — extracted by
stdlib parsers (Python `ast`, bounded shell regexes, Markdown path
references) without a model, key, or network. Every edge carries
`EXTRACTED`, `INFERRED`, or `AMBIGUOUS` confidence, so inference is never
presented as fact. Deterministic source summaries improve task ranking;
`api` projects signatures without bodies, `search` groups exhaustive literal
hits by enclosing symbol, and overviews report unsupported-extension coverage
instead of presenting an empty graph as complete. Extraction is content-addressed under
`.oms/project-graph/cache/`, `graph.json` carries no timestamps and is
byte-identical for the same working tree, and `check` judges freshness by
working-tree bytes, not by commit. `blast --base` uses the merge base and
handles an unborn `HEAD`; `blast` walks reverse dependencies from
the changed files; `context` produces a bounded task-specific pack (files,
tests, blast radius, hubs, a byte estimate that is never called tokens) and
can compile it through `oms runtime context`. Discovery honors `.gitignore`,
skips symlinks, binaries, oversized and secret-shaped files, and treats
every source byte as data.

The **Execution Graph** (`oms graph exec validate|render|route|run|resume|decide|status|events|shadow|test|commit`)
is an advanced option for explicit branching, concurrent graph tasks or resuming
an existing graph run. Ordinary execution stays with plans/goal-drive/autopilot;
the bundled goal-drive graph is an example, not another default driver.
A GraphSpec (`config/graphs/*.json`)
declares `agent`, `tool`, `gate`, `router`, `subgraph`, and `terminal` nodes,
typed semantic outcomes (`completed failed unverified partial blocked
changes_requested approved skipped`, deliberately separate from the plan
task lifecycle), edges keyed by outcome with `repeat` budgets, `join`
semantics, and proof predicates over facts. The validator rejects unknown
endpoints, unreachable nodes, missing terminals, cycles without a stop
policy, ambiguous routes, and recursive subgraphs. `route` is a pure
evaluation over facts (plan state, admission/landing/acceptance receipts,
git) and recorded outcomes: a claimed `completed` whose proof facts are
absent is `unverified`, and no model participates in routing. Runs live in
`.oms/graph/runs/<run-id>/` as a frozen spec, an append-only `events.jsonl`
with idempotency keys, and a derived projection; `resume` rebuilds state
from events and current facts, never from conversation memory. Task identity
is frozen before work starts: `plan_task: "next"` is a selector resolved by
one read-only peek, the concrete id is recorded on the `node_started` row
and, under `bind_task`, as a run-scoped binding that `plan_task_from` nodes
execute exactly (`implement` choosing `t1` means `land` lands `t1` after the
plan's `next` has moved on). Agent nodes execute through
`plan-run --id TASK [--land] [--context-pack FILE]` — the project-graph
context pack an agent node's `context` field builds is orientation for the
worker brief, never authority; the adapter has no path to `agent-plan
land/finish/claim`, landing stays serialized by `patch-land`, and `--jobs N`
runs a scheduler wave concurrently only for disjoint explicit tasks (same
task, unknown or overlapping scope, landing, write tools, and a second
selector serialize). The read-tool cache is keyed by a workspace fingerprint
as well as HEAD and fails closed. `resume` reconciles a crashed node against
the plan — a live lease waits, an expired one is `unverified`, a dead write
tool is `blocked`. `exec commit --binding NAME` commits exactly the bound
task's landed patch so the next landing finds a clean tree. The retired
`exec shadow` comparison is replaced by
`runtime next` for control-plane actions and `exec route/status` for actual
graph runs; existing comparison history is preserved.

## Durable operations and optional frontends

Lifecycle and approval JSONL access rejects linked leaves/direct parents and
checks named/opened file identity. Trace-tail reads, append and POSIX private
permissions use the same verified handle; torn-tail recovery remains intact.
Host ancestry aliases remain supported. This is not a same-user filesystem sandbox.

| Front door | Actual boundary |
|---|---|
| `agent-events`, `agent-supervisor` | Append-only attempt lifecycle and bounded `trusted-local` execution. Resume creates a child attempt; reconcile closes stale supervisor-owned queues that lost their runtime record. The supervisor never lands, commits, or pushes. |
| `approval-inbox` | Private, version-CAS approval outside `.oms`; a patch grant binds the exact base, bytes, attempt when present, lease, profile, verifier hash/mode, ML mode and admission exceptions, then is consumed once. `expire --apply` closes unused grants; stale reservations reconcile dry-run first to terminal `interrupted` with an unknown outcome. Patch landing defers to `patch-land --recover`. |
| `land` | One detached job per landing: gate (in a detached worktree of the verified SHA, so untracked files and the live session's `.oms` writes stay out), `git push --no-verify` of the verified SHA, CI success, then `oms update` guarded to that SHA when the repo is the harness checkout, receipt beside its gate log under `$XDG_STATE_HOME/oh-my-setting/land/<repo-slug>/` read by `oms land status`. Refuses dirty or diverged trees and never pushes a HEAD that moved during the gate. Sibling worktrees of the same repository whose autopilot receipt is live (`proposing`, `proposal-review`, `driving`) are reported at intake and waited for before the push, up to `--sibling-wait` seconds (default 1800); at the deadline the landing ends `blocked` without moving shared refs, and `--ignore-siblings` records the override. |
| `init` | Creates repo-local `.oms` state and its ignore guard, then registers the canonical repo root with `oms tick`. The non-fatal summary says `registered`, `already registered`, or `not registered`; an unavailable tick helper or unwritable registry never prevents local initialization. |
| `tick` | Hourly unattended sweep of registered repos: `oms init` registers a newly initialized repo automatically, while `oms tick register` remains available for an adopted repo. The sweep performs journal sync, attempt reconcile, threads idle over 7d closed, active goal-less task packets closed after 7d, idle all-done plans retired after 14d, mechanically recovered or exactly superseded artifact failures resolved, and single stale failure-ledger rows retired after `OMS_TICK_FAILURE_STALE_DAYS` (14d). `OMS_TICK_RETIRE=0` opts out of task/plan/failure retirement but not artifact resolution; gc remains opt-in with `OMS_TICK_GC=1`. Each receipt and `swept` line reports `tasks_closed`, `plans_retired`, `artifacts_resolved`, `artifacts_superseded`, and `failures_retired`; a stale Codex plugin cache is refreshed. `install` wires a systemd user timer or a cron line this checkout owns. |
| `open-in` | Probed VS Code/Stably Orca/Codex launch plans. The redundant `ops-cockpit` aggregate was retired; use `inbox`, then the relevant state, approval, or artifact telemetry query. Historical records are unchanged. |
| `otel-export` | Local content-free OTLP JSONL linking lifecycle, approval, landing, artifact, and hook metadata with opaque IDs and usage-trust labels; opt-in `--gen-ai` standard semantic attributes. |
| `autopilot`, `draft-pr` | Confirmed spec to reviewed plan, bounded landing, acceptance and semantic review; optional exact create-only GitHub branch plus Draft PR. No merge, release, ready, tag, or branch-update authority. |

`check.sh` and `tick` coordinate through the existing per-file lock namespace.
Checks may run concurrently; a sweep defers while a check or another sweep is
active on that repo. Dead check markers are recoverable, and the timer stays
enabled. This does not exclude maintenance receipts or task state from the
gate's state-leak detection; both entrypoints must have the updated code.

Commands that retain different authority may still share one decision engine.
Scope consumers use one Bash-compatible literal/glob matcher with deny
precedence; different glob declarations are never assumed to be subset-related.
Consultation entrypoints use one provider registry for aliases, availability,
and explicit/configured/automatic preference. State, inbox, runtime, plan, and
resume surfaces consume the same failure-attention, approval effective-state,
task-verification, plan-actionability, and PROJECT-state projections. These
shared reads do not grant mutation authority: durable approval `state` remains
distinct from its read-time `effective_state`, and `goal-drive` remains the
bounded executor for an approved plan while `autopilot` also owns spec review,
plan proposal, and final semantic review.

`trusted-local` inherits host files, credentials, processes, and network.
`isolated` only checks an existing Docker- or Podman-compatible daemon and local
image; `remote` only checks an operator-owned executable adapter. The compatibility
preflight and `oms runtime backend` resolve the same executable, so readiness and
execution cannot silently select different engines. The current supervisor does
not execute those two backends. Authenticated provider-native token/cost limiting is
not available, so a trusted-local job requesting either hard budget is refused
before launch. The private approval store is outside worker-writable `.oms/`
and uses `0700`/`0600`, but another process running as the same OS user can read
it; this is a write-integrity boundary, not account isolation.

VS Code and Orca are optional interfaces. Their session states are
observations, not OMS verification results. Direct commit, merge,
push, or PR actions in an external frontend bypass OMS admission and landing.
OMS is the authority only for flows started and completed through its managed
commands; keep one flow under one authority.

Scheduled `auto-update` is the unattended policy and status wrapper, not a
second updater. Receipt-owned schema-1 and schema-2 installs both preflight and
apply through `update.sh`, which remains the canonical rollback-capable install
transaction. Only receipt-less legacy checkouts retain the configured-upstream
compatibility path until a successful update creates an install receipt.
Before applying a new commit, both paths require successful `test.yml` push CI
for that exact SHA. Missing, pending, failed, or unreadable CI defers to the next
scheduled run; `gh` must be available and authenticated. Legacy updates merge
the checked SHA, without a second pull that could fetch an unchecked tip.
New cron/systemd triggers set `OMS_AUTO_UPDATE_MANAGED=1`: the wrapper selects
the checkout's exact private uv Python before reading receipts or running the
transaction. Runtime setup failures write a failed updater state instead of
leaving an old success visible. New installs provision the runtime before
provider configuration. Internal `scripts/python-runtime.sh` shares
versioned environments and launchers under the user's private OMS data folder;
system Python is used only to bootstrap the pinned uv download/lock parser.
Provisioning is locked, existing broken/unowned environments are preserved,
and rollback selects the old checkout's retained Python version. These runtime
files are outside the checkout and retained on uninstall for safe recovery.
`attention` reports skipped updates as `blocked` and a logout-dependent user
timer as `session-only`; systemd's `Persistent=true` alone does not keep the
user manager alive after logout. Linger is opt-in because it affects every user
service. Custom systemd drop-ins and private user skills remain user-owned.
Cron ownership is one exact begin/end marker pair. Install, removal, status,
and legacy receipt inference share the same `absent | valid | malformed`
classification; a malformed block is reported but never rewritten, so an
orphan marker cannot consume unrelated user cron entries. That malformed
verdict takes precedence over another installed trigger and is identical in
dry-run and apply mode.

`semantic-eval` is a migration-only shim: help explains the replacement, and
old execution requests fail without running checks or rewriting stored reports.
Use `patch-admit` at current HEAD and `peer-review`; this is not a replacement
for historical-base evaluation. `otel-export` writes only to stdout or a local file and never
sends network traffic. Writers accept a valid incoming `OMS_TRACEPARENT`, store
only opaque trace/span IDs and flags, and derive child context without storing
raw `traceparent`, `tracestate`, baggage, prompts, or tool arguments. Existing
`oms.*` attributes remain the default; `gen_ai.*` is emitted only with
`--gen-ai`.

## ML and HPC

The ML template records the scientific contract in PROJECT.md; optional
reference docs require `--full-docs` and never overwrite existing files.
ML/Slurm workflows use local-first experiment controls:

- `oms run new/current/show/timeline` for the run spine.
- `oms run capsule` for reproducibility capture and replay metadata.
- `oms runtime experiment` for comparable seed-, metric-, and invariant-bound
  studies; `oms runtime experiment launch` pre-registers a single command
  through the same run ledger (replacing `research-runner`).
- `oms data-manifest` for fingerprints, split checks, and leakage evidence.
- `oms experiment-board` for claims and collision avoidance.
- `oms run-reconcile`, `oms job-digest`, and `oms tsp-queue` for Slurm or local
  GPU work. `oms job-digest <id> --wait --wait-timeout S --max-bytes N` keeps
  the scheduler polling inside one shell call instead of repeated model turns:
  a spent budget exits 124 with observation incomplete and the job untouched, the
  digest is capped to whole lines with the omission stated, and a repeating
  controller error is reported once plus a total. Leaving the queue or empty
  accounting is reported as unknown, never as success. Where GNU `timeout` is
  present, both `squeue` and `sacct` use the remaining observation budget, with
  a one-second kill grace. Expired budgets skip accounting. Without GNU `timeout`
  (BSD/macOS), scheduler calls cannot be preempted; only sleeps are bounded.

Machine, cluster, dataset, and run details stay local unless the user explicitly
chooses a connector or tracked summary.

## Provider integrations

- Claude Code: skill hints, turn guard, failure memory, edit-time syntax guard
  (a file that does not parse after Edit/Write is reported in the same turn as
  feedback, never a block), session capture, handoff, and compact main/subagent
  HUDs. Model routing stays explicit in delegate/run commands; direct edits do
  not pay for a separate policy hook.
- Codex: local plugin hooks, including the edit-time syntax guard, plus a
  managed native status line when the user has not set one. On Python 3.9/3.10,
  arbitrary existing TOML requires `tomli` so
  the helper never rewrites an unvalidated config.
  The same installer manages usage-efficiency keys it can evidence:
  `background_terminal_max_timeout = 900000` (a longer ceiling for an empty
  background-terminal poll, not a wait duration) and, only where the user
  already enabled `[features.multi_agent_v2]` as a table, `min_wait_timeout_ms`
  and `default_wait_timeout_ms`. A user value, a boolean-form feature flag, or
  a lower user `max_wait_timeout_ms` is preserved; OMS never enables a feature,
  and model, reasoning, service tier, compaction, sandbox and approval keys are
  untouched. Codex rejects a config whose wait keys break `min <= default <=
  max`, so only when it writes those floors the installer lets the real `codex
  features list` load the result and removes its keys if that fails; an
  ordinary install pays no extra provider probe, since Codex ignores a root
  key it does not know. `oms doctor` reports the state; a project
  config, profile or `-c` flag can still outrank the written value.
  Install/update retires exact legacy OMS user-hook bridge entries only after
  verifying an enabled native plugin, current cache and stable hook support.
  It backs up the original bytes and preserves custom hooks; inconclusive
  replacement evidence or malformed/linked config leaves the file untouched.
- Antigravity: shared rules, skills, MCP, and provider calls. Hooks are enabled
  only after the installed binary passes a live surface probe; otherwise it is
  intentionally MCP-only. `--peer-permissions` grants `read_file(*)`,
  and sandboxed `command(*)` globally; all-MCP access remains approval-gated.
  Delegate exceptions accept only an exact command token and normalized
  absolute worktree parent. A sidecar lets uninstall remove only rules this
  install added.
  HUD/session-capture parity is not claimed.
- Optional CLI transports: Cursor, Grok Build, Gemini CLI, Qwen Code, OpenCode,
  DeepSeek Harness (`dsh`), Mistral Vibe, Pi, GitHub Copilot CLI, Factory Droid,
  and Aider join the same artifact, model-route, and provider-child contracts
  when detected. OMS does not install, authenticate, or reconfigure them.
  Provider-native read/edit modes and tool allowlists are pinned per invocation;
  Vibe trust is invocation-only, Pi project resources are ignored, and DeepSeek
  invocation telemetry is disabled. DeepSeek's filesystem preset does
  not confine network reads or every host process; Aider, OpenCode, and custom
  adapters additionally rely on the OMS worktree and post-run authority guard
  rather than an OS sandbox.
- MCP: shared state reads plus background peer actions that can incur provider
  cost and write `.oms` artifacts. Stdio records and prompts are byte-bounded;
  oversized input is rejected before provider argv or prompt files are built.
  By default `tools/list` exposes 12 core tools: inbox, repo state, journal,
  handoff list/read, peer start/result/operations, and graph render/query/trace/
  affected. Set `OMS_MCP_TOOL_PROFILE=full` in the MCP server's environment
  and restart its connection to discover all 26 tools; `core` is the default.
  The profile is fixed at process startup, so cached discovery is stable.
  Unlisted registered tools remain callable for existing clients: this setting
  reduces discovery overhead, not permissions. Schemas, annotations, UI resources,
  and protocol/Tasks negotiation are identical in both profiles.
  Runs are addressable from disk rather than from the conversation that started
  them: `oms_peer_operations` lists them newest first, a run whose process died
  without an exit reads as `stalled` instead of polling as running forever, and
  a finished result names the thread to continue from.
- MCP Apps: `oms_project_graph_render` is the presentation-only graph tool. It
  links the versioned `ui://oms/project-graph/v1.html` resource, while map,
  query, trace, blast, API, search, and affected remain reusable data tools.
  Node selection creates an editable instruction draft and never sends it
  without an explicit user action.
- MCP protocol revisions: the server is dual-era. `initialize` still
  negotiates for legacy clients; a `2026-07-28` client needs no handshake —
  `server/discover` answers statelessly and the revision named in each
  request's `_meta` selects the result shape, with `-32022` for a revision the
  server does not implement. Requests naming no revision keep their bytes.
- MCP Tasks: disabled unless `OMS_MCP_TASKS_EXTENSION=1`. With protocol
  `2026-07-28` and per-request `io.modelcontextprotocol/tasks` capability, a
  same-repository `oms_peer_start` reuses its durable operation ID as a Task;
  `tasks/get|update|cancel` add no list or second store. Older clients keep the
  prior CallToolResult shape.
- Provider work uses the existing CLI transport. The optional OMS app-server
  read adapter is retired; stale `OMS_CODEX_TRANSPORT=app-server` configuration
  fails explicitly instead of silently changing execution policy. Codex model
  discovery may still use its native app-server; that is not a work transport.

Integration removal failures propagate to `oms uninstall`; successful-looking
messages are emitted only after the corresponding CLI confirms removal.

## Verification and release

Installed global guidance and general/ML project templates favor one primary
test layer, reused fixtures, affected tests and small routine CI. New project
contracts include `CI scope/runtime`; expensive GPU/data/platform checks need
relevant-change, release, scheduled or explicit triggers under that contract.
The ML check scaffold uses `uv run --no-sync` after project Setup, so routine
checks do not implicitly resolve/install dependencies. Existing project check
scripts and workflows are not overwritten; OMS does not generate a CI matrix
just because it is installed. These defaults guide agents, not an enforcement
service or proof of measured CI savings.

`bash scripts/check.sh` is the repository gate: shell lint, Bash 3.2 parsing,
Python 3.9 grammar, skill validation, focused suites, and sharded smoke tests.
Development uses affected tests and static checks, not this full gate per edit.
For committed ranges, `--affected --changed-from BASE --changed-to HEAD` checks
ordinary README/docs changes directly, otherwise selects existing tests from
positive graph evidence. Skill/template Markdown uses the direct skill validator
(including project skill templates and their local references), not runtime
prompt/GC/lifecycle suites;
`config/models.json` uses routing/registry suites. Install/permission/state,
executable templates, selector changes, dirty trees, or uncertain graphs retain
full coverage. `source-distribution-smoke.sh --docs-only` checks documentation
references without its CI orchestration, installation or CLI-help probes.
`--quick` is partial feedback, not a substitute for affected or release checks.

CI selects once and runs affected tests plus changed-file lint in that same
job for PRs and main pushes. Full selections and weekly/manual runs fan out to
the existing lanes and additionally exercise
install/update/uninstall on Linux, macOS, and Windows Git Bash, BSD fixtures on
macOS, and real Python 3.9 compatibility. The stable `gate` accepts only planned
skips. Require it in branch protection before enabling a local quick push hook;
having a workflow alone does not protect the branch. Deployment via `oms land`
still verifies the exact committed tree with one full gate, pushes its SHA,
waits for CI success, then updates only to that SHA. Failed or skipped CI cannot
refresh the installation. A moving update target is rejected by the updater.
Retrying the same commit, destination and gate resumes CI/install from its
successful push receipt; successful stages are reused. An installation receipt
is reused only while the installed checkout is clean and still at the same SHA.
A repository-wide lock prevents concurrent landing gates; background launches
acknowledge lock acquisition before reporting success. CI query errors are recorded immediately,
and each query is bounded by the remaining polling deadline (at most 30 seconds).

The existing check output and GitHub job summary show selection reasons and the
five slowest measured stages/tests per lane. Test timings reuse the smoke
runner's opt-in measurements; there is no extra run, job or timing database.
Mode-only/list output remains machine-readable and nested fixtures cannot
append to the real job summary. Summaries do not alter required coverage.

The gate fingerprints `.oms` file contents, entry modes, symlinks, and
directories so a test cannot quietly mutate the live checkout. Live `hooks/` and
`work-journal/` activity is excluded because it belongs to the active session.

Use an outside advisor for irreversible architecture choices, repeated
failures, or a release go/no-go. Routine bounded completion relies on the
declared mechanical gate.

## Known limits

- Native PowerShell is not a supported shell; Windows support means Git Bash.
- Full non-Linux behavior is strongest on the install lifecycle; most public
  tools run their comprehensive suites on Linux plus portability fixtures.
- On the exercised Windows surface (install, update, doctor, journal status,
  uninstall), paths cross to python as argv — which Git Bash converts — or
  base64-encoded where a value must survive the environment
  (provider-permissions). A new script on that surface must pick one of
  those two crossings; plain env path values reach native python rewritten.
- Locked upstream artifacts and the `edge` source ref remain supply-chain
  choices; review the lock and pin the source ref for higher assurance.
- Policy handling classifies provider text. Unfamiliar or localized refusal
  wording can be reported as an ordinary provider failure.
- Version publication, merge, and release remain explicit owner actions.
  `goal-drive` only creates local commits. `draft-pr publish` is the narrow
  exception for a verified create-only branch and Draft PR; it never updates
  an existing branch or advances the PR.
- Recovery-branch admission is a continuity guard, not an ownership proof:
  autopilot accepts only the deterministic branch or a strict
  `oms/autopilot-<spec-digest>-rN` branch descended from the reviewed base,
  with a clean in-envelope diff and the configured final gates, but no durable
  receipt attests which process created that local branch. Inspect any
  unexpected matching branch before checkout or resume; use
  `--review-mode gate` when provenance is uncertain.
- Frozen-patch path validation rejects static symlinks and detects the
  exercised directory-swap windows, but it is not a portable `dirfd`/`openat`
  transaction. A hostile same-UID process may still win a check-to-write race
  around `.oms/plan/commit-patches`; do not run `goal-drive` with untrusted
  same-account writers, and use OS isolation when that threat is in scope.
- Draft publication pushes with `--no-verify --no-signed`, so repository-local
  pre-push hooks and worker-writable signing configuration never execute under
  the publisher's credentials. It refuses repository-local executable
  transport, filter/diff/fsmonitor, proxy, credential, include, and URL-rewrite
  keys plus unsafe command-scope Git configuration before any remote operation, scans all new
  history objects, disables implicit tag/submodule
  pushes, and binds the `gh` viewer. Residue: a trusted global Git credential
  helper can still identify a different account; GitHub identity checks cover
  `gh`, not every possible transport credential source.
- GitHub creates pull requests from branch names, not caller-supplied expected
  object IDs. The publisher checks both refs immediately before and after the
  request and parks on drift, but another authorized GitHub writer can still
  move a ref in that request window; restrict writers or protect those branches
  when that residual race is unacceptable.
