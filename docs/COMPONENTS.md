# Components

oh-my-setting is a local control plane for Codex, Claude Code, and Antigravity.
It gives them shared rules, skills, state, peer calls, isolated write
delegation, and one verification boundary for OMS-managed delegated patches.
The primary subsystem catalog is `oms list --frontdoor`; compatibility
primitives and intent variants remain in `oms list --all`. Use
`oms <tool> --help` for flags and the
[command-routing reference](../custom-skills/oms-agent-harness/references/command-routing.md)
when two commands appear to overlap.

## How it fits together

```text
user request
  -> provider skill/router
  -> oms command
       -> read: consult / peer-ask / peer-review / advise
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

The default installer selects the `core` capability: Bash, Git, Python, the
harness, and one coding-agent provider. It installs or verifies only the locked
tools required by the selected profile; optional capabilities add the council,
GitHub, Notion, research, HPC, container, or remote surfaces. Setup fails rather
than silently recording a selected profile with missing required CLIs. Required
provider dependencies are part of that plan: Codex and Claude include Node,
while Antigravity does not. Versions, URLs, and integrity values live in
`tools.lock.json`; direct platform payloads and npm wrapper/native packages are
verified before use and npm installation runs offline with a fresh cache.
Install, update, repair, and uninstall share one user-wide lifecycle lock. Lock
ownership survives installer `exec` handoff but cannot be inherited or
released by a child shell.
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
| One independent read or continued peer thread | `oms consult [--to PROVIDER]` | Threaded answer |
| Council or debate | `oms peer-ask` | Per-seat artifacts and family count |
| Diff review | `oms peer-review --gate` | Verdicts plus mechanical backstop |
| High-risk decision | `oms advise` | Adversarial recommendation |

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

For compatibility, `oms agent-run` sends read work to `agent-call` and write
work to `peer-delegate`. New agent guidance uses the intent-specific front
doors above. A caller that intentionally needs the wrapper should pass
`--mode read` or `--mode write`; auto mode remains conservative wording
classification.

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
task/lease plus executor/soul objects exactly while other tasks may move. On a
violation the failing run also repairs the operation's own authority from its
hash-verified pre-launch snapshot: scope, verifier, executor receipt, soul
bytes, and review evidence always restore, while claim-cycle fields restore
only under the operation's own lease and are otherwise kept and named. A
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
preserved and parks the run instead of being overwritten. Repository hooks are
disabled for every child Git operation in the drive, not only the final ref
update. Frozen patch creation enters the validated physical directory, uses
relative writes, and atomically replaces the final leaf without following a
directory symlink; a changed absolute lookup parks before consumption.

An active plan that became stale after equivalent work landed elsewhere is
removed through `oms agent-plan retire`, never by inventing task transitions.
The default command is a no-write check that returns the exact plan SHA-256.
Apply is parent-only and requires that SHA, a disposition, and a reason. For
`completed-external`, the command itself runs a fresh acceptance on one clean
committed HEAD and binds full plan/acceptance/output hashes, ref, file
generation, and a unique proof token. `superseded` is limited to all-done old
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

Frozen executors combine a reusable role with task-specific scope, base SHA,
lease, model route, and verify command. They are write-only and cannot widen
their own authority or recursively delegate. A failed landing may re-arm the
same reviewed task and executor once, without changing that frozen contract;
provider failure or signal exit blocks that repair before it can become
claimable again, including after a restart.

Each new autopilot receipt also binds an opaque run owner. Claims and worker
markers inherit it. Re-entry recovers only that owner's exact current
`claimed`/`running` leases under the plan lock: live markers, markerless
running work, another owner, and `review`/`landing` evidence are preserved.
Its owner and dead claimant come from one receipt-lock judgment token, never a
later ledger reread. Routine GC CASes the observed state and lease and vetoes
any exact live worker marker under the plan lock, so a retry or task that
advances during cleanup cannot be requeued by stale evidence. Executor cleanup
uses the same check/apply predicate under its metadata lock. Windows liveness
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

## Durable operations and optional frontends

| Front door | Actual boundary |
|---|---|
| `agent-events`, `agent-supervisor` | Append-only attempt lifecycle and bounded `trusted-local` execution. Resume creates a child attempt; reconcile closes stale supervisor-owned queues that lost their runtime record. The supervisor never lands, commits, or pushes. |
| `approval-inbox` | Private, version-CAS approval outside `.oms`; a patch grant binds the exact base, bytes, attempt when present, lease, profile, verifier hash/mode, ML mode, executor+soul, and admission exceptions, then is consumed once. `expire --apply` closes unused grants; stale reservations reconcile dry-run first to terminal `interrupted` with an unknown outcome. Patch landing defers to `patch-land --recover`. |
| `execution-profile`, `herdr-adapter` | Environment/CLI contract preflight and optional pane/agent control. They are not a sandbox or landing authority. |
| `open-in`, `ops-cockpit` | Probed VS Code/Stably Orca/Codex launch plans and a read-only, non-atomic operational summary. Its `observations` block projects the pending observation decisions — turn-guard intervention pairing, fail-ledger hook-row retirement, usage-family exposure — with no thresholds or tuning. |
| `otel-export`, `semantic-eval` | Local content-free OTLP JSONL linking lifecycle, approval, landing, artifact, and hook metadata with opaque IDs and usage-trust labels; advisory patch evaluation from trusted host checks plus a self-reported judge result. |
| `autopilot`, `draft-pr` | Confirmed spec to reviewed plan, bounded landing, acceptance and semantic review; optional exact create-only GitHub branch plus Draft PR. No merge, release, ready, tag, or branch-update authority. |

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
- `oms runtime experiment` for comparable seed-, metric-, and invariant-bound
  studies; `oms research-runner` remains the compact single-run/run-ledger
  compatibility path.
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
  Runs are addressable from disk rather than from the conversation that started
  them: `oms_peer_operations` lists them newest first, a run whose process died
  without an exit reads as `stalled` instead of polling as running forever, and
  a finished result names the thread to continue from.

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
