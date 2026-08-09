# Security

oh-my-setting installs shell tooling, symlinks agent config into your home
directory, and lets local agent CLIs (Codex, Claude Code, Antigravity) call
each other. This file explains the threat model and how to report issues.

## Reporting a vulnerability

Email the maintainer (see the repo owner's GitHub profile) with:

- a description and impact,
- steps to reproduce,
- affected script(s) and commit.

Do not open a public issue for a secret leak or a supply-chain concern; report
privately first. Please allow time for a fix before public disclosure.

## What the installer touches

- Manages `~/.codex/AGENTS.md`, `~/.claude/CLAUDE.md`, the Antigravity config,
  and skills with symlinks or verified copies. Pre-existing files move to
  `*.backup.<timestamp>` and are restored by uninstall.
- `--peer-permissions` explicitly adds only `read_file(*)` and sandboxed
  `command(*)` to Antigravity's user-global allow-list. All-MCP access remains
  approval-gated. A sidecar records only new entries; uninstall removes those
  entries without restoring a stale whole-file backup over later user edits.
  Delegate exceptions accept one exact command token and one normalized
  absolute worktree parent; rule metacharacters are rejected.
- Installs or reuses the required toolchain: Node/nvm, uv, `gh`, Antigravity,
  and the global npm packages for Codex, Claude Code, and Notion CLI. Exact
  versions come from `tools.lock.json`; direct platform payloads plus npm
  wrapper/native packages are verified against repository-pinned digests or
  integrity before use. npm installs the verified wrapper offline with a fresh
  cache before adding the verified native payload. A pre-existing external CLI
  at the exact version is reused as version-only evidence; it is not described
  as byte-authenticated. Managed direct binaries have digest ownership sidecars,
  while npm wrapper/native packages and shims use recoverable swaps.
  Uninstall leaves them and the user-local PATH entry in place. Windows Git
  Bash requires the exact locked native Node release beforehand.
- Optionally writes a local machine/Slurm snapshot under `local/` and generated
  Slurm references (gitignored). Skip with `OH_MY_SETTING_GENERATE_MACHINE=0`
  and `OH_MY_SETTING_GENERATE_SLURM=0`.
- Auto-update defaults to `apply`: it fast-forwards and relinks a clean
  checkout, but skips dirty or diverged state. Set
  `OH_MY_SETTING_AUTO_UPDATE_MODE=check` for notification-only behavior.
- Install, update, repair, and uninstall share one user-wide lifecycle lock.
  Ownership follows an `exec`, but not a child shell; a process start token is
  checked where the host exposes one. Existing managed checkouts are not
  overwritten or purged while dirty unless the user explicitly chooses the
  destructive uninstall override.
- The MCP server reads harness state, and `oms_peer_start` can launch an
  external provider call and write local `.oms` artifacts. Provider cost and
  data-handling rules therefore still apply to MCP-triggered calls. Stdio
  requests and peer prompts have byte limits and are rejected before prompt
  files or provider argv are created.

## Secret handling

- Never commit tokens, API keys, private data, or cluster/machine details.
  `.gitignore` blocks `.env`, `*.key`, `*.pem`, `local/`, and generated state.
- Project memory and its derived SQLite index are local plaintext under
  `.oms/memory/`; the index is not an encrypted secret store. Memory writes are
  scrubbed before either representation is updated, and `.oms/` is gitignored.
  Memory provenance stores bounded hashes and IDs, never branch names, paths,
  commands, or raw diffs, and is omitted from compact provider prompts.
- Outbound prompts/diffs to external agent CLIs are scanned for secrets and
  machine/cluster details; a match blocks the call (`scripts/lib/agent-memory-common.sh`).
- The Claude Code HUD reads the vendor-provided status JSON locally but ignores
  transcript and path fields. It shell-evaluates nothing, strips terminal
  control characters, bounds input/output, and performs no network request.
- Delegated patches pass a sensitive-content scan before they can be admitted
  (`scripts/patch-admit.sh`).
- Git-tracked records (the run ledger, gate skip reasons, data manifests) are
  scanned before writing; a sensitive gate skip reason is refused outright.
- Lifecycle events contain bounded identifiers and state, not prompts,
  commands, output, secrets, or absolute paths. Approval grants live outside
  worker-writable `.oms/` state, use version compare-and-set, and are consumed
  once by an exact action. A request bound to a lifecycle attempt can be
  consumed only while that same attempt is explicit in the caller environment.
  An explicit expiry pass can terminalize unused grants; stale reservations
  require a dry-run-first reconcile and retain an unknown outcome instead of
  being guessed successful or failed. Patch-land reservations defer to its
  intent-and-tree recovery before terminalization.
  The approval directory/file modes are `0700`/`0600`, but a process running as
  the same OS user can still read them; this protects worker-writable state, not
  against a fully hostile same-account process.

## Runtime boundary

Read calls use each provider's practical read-only mode; write delegation uses
an isolated worktree plus patch admission and verification. Verification runs
candidate project code with the user's host permissions. Repository and
shared-state changes are checked after a worker exits, but this is detection,
not process containment: it cannot prove what was read, see a reverted write,
or contain writes outside the repository. Provider and verifier calls have a
wall-clock bound, but that is not isolation. Use an OS sandbox or container
when that stronger boundary is required.

Write-provider children do not receive the primary `OMS_STATE_REPO`, attempt,
plan lease, or executor capability variables. The default guard preserves
parallel owner work while rejecting rewrites, truncation, and deletion of
existing shared-state evidence. For a plan-bound delegation it also binds the
complete selected task/lease and executor/soul objects while allowing sibling
tasks to move. A mismatch fails before a review or landing is published; it is
not auto-restored because a new lease owner may now be legitimate. If the caller
guarantees no sibling state writer, `OMS_WORKER_AUTHORITY_EXCLUSIVE=1`
additionally compares and restores the full owner-authority surface. Enabling
that mode during parallel work can discard legitimate sibling writes. Neither
mode can detect a write restored before exit or contain a process that escapes
the monitored lifetime. Live-sibling worktree exclusion is cooperative: an
adversarial same-UID process can imitate its marker and PID while forging Git
metadata. Use an OS sandbox for that threat model.

The delegated checkout is also bound to its physical directory identity, Git
directory/common-directory, and both regular Git backpointers. Those receipts
are rechecked before provider, capture, verification, repair, publication, and
cleanup; a replaced path or symbolic worktree registry is preserved for
inspection instead of followed.

Plan landing and completion bind the exact reviewed patch bytes, task verifier,
lease, and, when present, executor ID plus soul hash. Omitting or replacing any
receipt fails closed through a compare-and-set inside the plan lock. A rejected
landing can re-enter the same review lease for one bounded repair; it cannot
mint a wider lease or repeatedly re-arm an executor. That repair is
terminalized as blocked on provider failure or signal exit, including after a
restart, so a later run cannot call another worker for it.

Landing and goal journals are appendable evidence, not standalone authority.
Terminal rows repeat a canonical receipt and recovery also checks the durable
patch, task, approval, lineage, and Git state that corresponds to the outcome;
minimal or mismatched terminal rows are ignored.

`goal-drive` publishes only the frozen expected tree: it builds that tree with
a private index, creates the commit without running repository commit hooks,
and updates `HEAD` only if the expected parent still matches. It holds Git's
standard index lock across ref publication and replaces the real index only
when its base digest still matches; concurrent staged bytes are preserved and
park the run. Durable outer and inner landing receipts let a restart recover an
applied-but-unfinished patch before converging the exact commit. An open inner
landing is recoverable only while `HEAD` still equals its recorded base.

Admission runs candidate verification and, when the patch changes a recognized
verification surface, a second base-owned floor with that surface restored from
HEAD. The automatic surface covers the named verifier, test/spec paths, common
verification configuration, and conventional repo-owned check/test/verify/lint
helper names; it is not a proof of every dynamically loaded dependency. Keep
unconventionally named trusted helpers under those paths or name them directly
in the verification command. If any earlier policy or integrity gate fails,
admission does not execute either verifier projection. An explicit override is
therefore also the caller's opt-in to execute the permitted candidate change.

`agent-supervisor` currently executes only `trusted-local` jobs. It bounds wall
time and cancellation, but runs as the current user and never lands, commits,
or pushes. POSIX cleanup owns the launched process group; a descendant that
deliberately creates a new session can escape. Windows launches suspended,
assigns the process tree to a kill-on-close Job Object, then resumes it; setup
failure aborts before an unowned child can continue.
Hard token/cost budgets are refused before launch because trusted-local has no
authenticated provider-native limiter. `execution-profile` is a readiness
check, not an executor: `isolated` verifies an existing Docker daemon and local
image, while `remote` verifies an operator-owned adapter. It installs, pulls,
and connects to nothing.

The Herdr adapter controls panes and recognized agents but has no approval,
lease, admission, success, or landing authority. Its `done` state is an idle
observation after unseen work, not proof of verification. External frontend
commit/merge/push actions bypass OMS; OMS authority applies only to managed
flows. `semantic-eval` invokes no model; its optional judge JSON is
self-reported and cannot prove independent provenance.
Spec commands run only after `--allow-host-checks`, in a temporary worktree
that is not a process sandbox. Use trusted specs only.

`ops-cockpit` is read-only but exposes operational and approval metadata and is
not an atomic snapshot. `otel-export` writes a whitelisted, content-free OTLP
JSONL stream to stdout or a local file and performs no network transmission. It
links lifecycle, approval, landing, artifact, and hook metadata with opaque
correlation IDs and labels usage as measured or advisory. Treat both outputs as
local operational metadata.

## Hardening recommendations for users

- Review `install.sh` before piping it to a shell, or clone and run it locally.
- Review changes to `tools.lock.json`; the lock authenticates the selected
  upstream bytes, not the upstream publisher's intent.
- Pin a trusted ref instead of `edge` when reproducibility matters.
- Use check-only updates unless you trust unattended clean fast-forwards.
- Run `oms doctor` after install to verify managed state.
