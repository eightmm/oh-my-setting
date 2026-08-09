# Components

oh-my-setting is a local control plane for Codex, Claude Code, and Antigravity.
It gives them shared rules, skills, state, peer calls, isolated write
delegation, and one verification boundary for OMS-managed delegated patches.
The supported command catalog is always `oms list`; use `oms <tool> --help`
for flags.

## How it fits together

```text
user request
  -> provider skill/router
  -> oms command
       -> read: agent-call / consult / peer-review
       -> write: peer-delegate -> isolated worktree -> patch
                                      -> patch-admit -> patch-land
       -> orchestrate: agent-supervisor -> agent-events -> approval-inbox
       -> operate: execution-profile / herdr-adapter / open-in
       -> observe: ops-cockpit / otel-export / semantic-eval
  -> local state in .oms/
       -> inbox / state / handoff / MCP / Work Journal
```

The owning agent remains responsible for scope, admission, verification,
commit, push, and release decisions. Peer agreement is evidence, not approval.

## Install and ownership

The initial installer always installs or verifies the required core tools it
coordinates: Node, uv, Codex, Claude Code, Antigravity, GitHub CLI, and Notion
CLI. Setup fails rather than silently recording a profile with missing CLIs.
Required versions, URLs, and integrity values live in `tools.lock.json`; direct
platform payloads and npm wrapper/native packages are verified before use and
npm installation runs offline with a fresh cache. Install,
update, repair, and uninstall share one user-wide lifecycle lock. Lock
ownership survives installer `exec` handoff but cannot be inherited or released
by a child shell.
An already-installed external CLI at the exact version is reused as
version-only evidence; doctor distinguishes it from a digest-owned install.
Managed direct binaries and npm package/shim swaps carry recovery state, and
doctor reports digest drift or an interrupted transaction separately from lock
schema validity. PATH persistence follows Bash's existing login-file priority
instead of creating a higher-priority profile.
Optional provider registrations report a warning and remain visible to `oms
doctor --contract`. Windows Git Bash requires the exact locked native Node
release before setup; Linux and macOS can install it during setup.

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
oms update
oms uninstall
```

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

## Asking other agents

| Need | Front door | Result |
|---|---|---|
| One independent read | `oms agent-call` | Answer artifact |
| Continue a peer thread | `oms consult` | Threaded answer |
| Council or debate | `oms peer-ask` | Per-seat artifacts and family count |
| Diff review | `oms peer-review --gate` | Verdicts plus mechanical backstop |
| High-risk decision | `oms advise` | Adversarial recommendation |

Read calls scan outbound context for credentials and machine-sensitive data.
`--export-only` writes a prompt without invoking another CLI; import the answer
through `oms artifact-index import`.

Provider count is not independence. Two routes backed by the same model family
remain one opinion for diversity reporting.

## Models and reasoning effort

Routing is deliberately small:

- No `--model`: use the provider default.
- `--model NAME`: use that exact model with no implicit switch.
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

Artifacts record requested and selected models, reasoning effort, fallback use,
and reason. An executor stores `provider-default` as provenance, but loading it
does not turn that sentinel into an explicit model request.

## Delegating writes

`oms agent-run` sends read work to `agent-call` and write work to
`peer-delegate`. Auto mode uses conservative wording classification; callers
can choose `--mode read` or `--mode write`.

A write delegation:

1. Creates a detached worktree from the current `HEAD` under the private
   `${XDG_CACHE_HOME:-$HOME/.cache}/oh-my-setting/worktrees` root (override
   with `OMS_DELEGATE_WORKTREE_ROOT`).
2. Injects the brief, selected role or frozen executor, and bounded context.
3. Runs the provider and the declared verification command.
4. Returns an artifact and binary-safe patch; it does not commit or push.
5. Optionally uses bounded repair rounds with the prior failure attached.

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
attempt/lease capability, or executor capability. The default guard preserves
parallel agent state, rejects destructive rewrites, and binds the selected
task/lease plus executor/soul objects exactly while other tasks may move. A
caller that guarantees no sibling writer may set
`OMS_WORKER_AUTHORITY_EXCLUSIVE=1` for complete authority-state comparison and
rollback. Plan landing and completion require the reviewed patch bytes,
verifier, lease, and any executor ID/soul receipt to match exactly inside the
plan lock. The delegated checkout's physical identity and both Git
backpointers are rechecked
at every mutation boundary, including cleanup. Landing terminal rows close a
transaction only when their canonical receipt and durable plan, approval, and
lineage outcome also converge.

```bash
oms peer-delegate --to codex --prompt "Implement the bounded change."
oms patch-admit --patch path/to/change.patch
oms patch-land --patch path/to/change.patch
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

`.oms/` and project agent files are clone-local by design. A fresh clone needs
the template/private setup again; continuity is not silently copied between
machines.

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
preserved and parks the run instead of being overwritten.

Frozen executors combine a reusable role with task-specific scope, base SHA,
lease, model route, and verify command. They are write-only and cannot widen
their own authority or recursively delegate. A failed landing may re-arm the
same reviewed task and executor once, without changing that frozen contract;
provider failure or signal exit blocks that repair before it can become
claimable again, including after a restart.

Recovery tools include:

```bash
oms checkpoint create --label "before risky edit"
oms fail-ledger list
oms artifact-index unresolved
oms approval-inbox expire [--apply]
oms approval-inbox reconcile --older-than-seconds 300 [--apply]
oms patch-land --recover
oms gc
```

## Durable operations and optional frontends

| Front door | Actual boundary |
|---|---|
| `agent-events`, `agent-supervisor` | Append-only attempt lifecycle and bounded `trusted-local` execution. Resume creates a child attempt; reconcile closes stale supervisor-owned queues that lost their runtime record. The supervisor never lands, commits, or pushes. |
| `approval-inbox` | Private, version-CAS approval outside `.oms`; a patch grant binds the exact base, bytes, attempt when present, lease, profile, verifier hash/mode, ML mode, executor+soul, and admission exceptions, then is consumed once. `expire --apply` closes unused grants; stale reservations reconcile dry-run first to terminal `interrupted` with an unknown outcome. Patch landing defers to `patch-land --recover`. |
| `execution-profile`, `herdr-adapter` | Environment/CLI contract preflight and optional pane/agent control. They are not a sandbox or landing authority. |
| `open-in`, `ops-cockpit` | Probed VS Code/Stably Orca/Codex launch plans and a read-only, non-atomic operational summary. |
| `otel-export`, `semantic-eval` | Local content-free OTLP JSONL linking lifecycle, approval, landing, artifact, and hook metadata with opaque IDs and usage-trust labels; advisory patch evaluation from trusted host checks plus a self-reported judge result. |

`trusted-local` inherits host files, credentials, processes, and network.
`isolated` only checks an existing Docker daemon and local image; `remote` only
checks an operator-owned executable adapter. The current supervisor does not
execute those two backends. Authenticated provider-native token/cost limiting is
not available, so a trusted-local job requesting either hard budget is refused
before launch. The private approval store is outside worker-writable `.oms/`
and uses `0700`/`0600`, but another process running as the same OS user can read
it; this is a write-integrity boundary, not account isolation.

Herdr, VS Code, and Orca are optional interfaces. Their session states are
observations, not OMS verification results: in particular, Herdr `done` means
background work returned to idle, not that tests passed. Direct commit, merge,
push, or PR actions in an external frontend bypass OMS admission and landing.
OMS is the authority only for flows started and completed through its managed
commands; keep one flow under one authority.

`semantic-eval` calls no model. A spec command needs `--allow-host-checks`; its
temporary worktree is not a host sandbox. A local judge file has self-reported
provenance, so an evaluation requiring an independent judge remains
`incomplete`. `otel-export` writes only to stdout or a local file and never
sends network traffic.

## ML and HPC

The ML/Slurm template adds local-first experiment controls:

- `oms run new/current/show/timeline` for the run spine.
- `oms run capsule` for reproducibility capture and replay metadata.
- `oms research-runner` for hypothesis-led runs.
- `oms data-manifest` for fingerprints, split checks, and leakage evidence.
- `oms experiment-board` for claims and collision avoidance.
- `oms run-reconcile`, `oms job-digest`, and `oms tsp-queue` for Slurm or local
  GPU work.

Machine, cluster, dataset, and run details stay local unless the user explicitly
chooses a connector or tracked summary.

## Provider integrations

- Claude Code: skill hints, turn guard, session capture, handoff, and compact
  main/subagent HUDs.
- Codex: local plugin hooks plus a managed native status line when the user has
  not set one. On Python 3.9/3.10, arbitrary existing TOML requires `tomli` so
  the helper never rewrites an unvalidated config.
- Antigravity: shared rules, skills, MCP, and provider calls. Hooks are enabled
  only after the installed binary passes a live surface probe; otherwise it is
  intentionally MCP-only. `--peer-permissions` grants `read_file(*)`,
  and sandboxed `command(*)` globally; all-MCP access remains approval-gated.
  Delegate exceptions accept only an exact command token and normalized
  absolute worktree parent. A sidecar lets uninstall remove only rules this
  install added.
  HUD/session-capture parity is not claimed.
- MCP: shared state reads plus background peer actions that can incur provider
  cost and write `.oms` artifacts. Stdio records and prompts are byte-bounded;
  oversized input is rejected before provider argv or prompt files are built.

Integration removal failures propagate to `oms uninstall`; successful-looking
messages are emitted only after the corresponding CLI confirms removal.

## Verification and release

`bash scripts/check.sh` is the repository gate: shell lint, Bash 3.2 parsing,
Python 3.9 grammar, skill validation, focused suites, and sharded smoke tests.
CI additionally runs install/update/uninstall on Linux, macOS, and Windows Git
Bash, BSD fixtures on macOS, and a real Python 3.9 compatibility job.

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
- Locked upstream artifacts and the `edge` source ref remain supply-chain
  choices; review the lock and pin the source ref for higher assurance.
- Policy handling classifies provider text. Unfamiliar or localized refusal
  wording can be reported as an ordinary provider failure.
- Version publication, pushes, and pull requests remain explicit owner actions.
  `goal-drive` may create a local commit only inside its bounded acceptance
  loop; it never pushes or releases that commit.
