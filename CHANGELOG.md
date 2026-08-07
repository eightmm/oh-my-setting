# Changelog

All notable changes to this project are documented here. The format loosely
follows [Keep a Changelog](https://keepachangelog.com/); versions track the
`VERSION` file.

## [Unreleased]

### Fixed
- Self-advice discloses itself. When `oms advise` or `oms consult` finds no
  independent provider CLI and falls back to asking the caller's own family,
  it now says so on stderr (`note: no independent provider available; ...`)
  instead of presenting the answer as an outside read. stdout contracts are
  unchanged; an explicit `--to` stays quiet.
- The fail-ledger hook refuses fingerprints no session can recompute: a
  failed command that names a session-scoped scratch path
  (`/tmp/claude-<uid>/...`) is not recorded — the row would be open forever
  by construction, and such rows were the largest class of permanent ledger
  noise. The deliberate `record` verb is unaffected.
- Smoke-test fixtures stop naming the author's real username; the sensitive
  path literals now use the fictional-user pattern their neighbors already
  used.

### Added
- A split project identity can be merged. `oms journal identity` shows the
  pinned identity against what the current parser detects (a pin taken before
  SSH-remote normalization, or before a remote existed, splits one project
  into two Notion identities), and `--adopt-detected` merges deliberately:
  re-pin, reset the disposable Notion mapping, rebuild the derived views —
  the event log stays append-only with its historical stamps. `status`
  surfaces the drift with its remedy.
- The Notion journal mirrors only the repos you chose. Two fixes with one
  cause: the passive journal boundaries (prompt tick, Stop finish) ran in any
  git repo and seeded `.oms` there, so a repo that was merely cloned got a
  journal and — Notion config being global — a page on the human-facing
  journal. The passive boundaries are now adopted-repos-only and never seed
  (deliberate front doors like `oms journal` and `run-ledger` still do), and
  `oms journal configure --exclude-repo PATH` / `--include-repo PATH` curates
  the mirror per repo: an excluded repo keeps its local journal but never
  syncs, `status` reports the exclusion, and the list survives reconfigure.
- Antigravity surfaces are certified per binary before any hook ships.
  `oms update --probe-agy-surfaces` runs a throwaway-HOME probe that must
  mechanically prove schema acceptance, actual headless `PreInvocation`/`Stop`
  delivery, payload shape, and timeout enforcement; the verdict
  (`verified`/`unsupported`/`unverified`) is cached per agy binary
  fingerprint, only `verified` lets the installer generate `hooks.json`, and
  a new fingerprint actively retracts installed hooks back to MCP-only. The
  certified surface observes turns (state hint, journal/CI tick, advisory-only
  Stop) — it cannot route skills, since `PreInvocation` carries no user
  prompt — and the status line stays out until separately tested. Live probe
  on this machine: `unverified` (agy cannot authenticate under an isolated
  HOME), so nothing changes for any existing install — which is the design:
  the 1.1.10 changelog advertising working Stop hooks is a reason to
  re-probe, never evidence.
- The fail-ledger's automatic writers became symmetric. The Bash failure hook
  now also registers for `PostToolUse`: a command that later exits 0 resolves
  its own unresolved rows mechanically (one python pass, no git-state hash,
  empty-ledger bail first), so ad-hoc failures stop accumulating as permanent
  inbox/resume/advise noise once fixed. Two named semantic changes: flaky
  commands reset their repeat count on each pass, and one-off never-rerun
  rows still need the occasional manual sweep. Goal-drive parks gained
  contract-shaped fingerprints (reason + acceptance hash; run id demoted to
  the summary) so the advise-at-repeat threshold is actually reachable, the
  previously-discarded advise hint now prints at the park, and a passing
  acceptance resolves that contract's open parks.
  `OMS_FAIL_LEDGER_RESOLVE=0` disables only the resolve side.
- Review-uptake telemetry, honestly bounded. `oms artifact-index telemetry
  --review-uptake` partitions delegate rows into review-fed and direct
  cohorts and reports recorded exit-zero, verifier, lineage-coverage, and
  duration/token figures per cohort — labeled observational and
  mechanical-only, with all rates withheld as `insufficient-data` below a
  five-row cohort floor. On today's live index that floor fires (2 delegate
  rows, 0 review-fed), which is the point: the question "do embedded review
  findings change worker output" becomes answerable the moment the data
  exists, without ever being answered before it does.
- Council seats are counted by answer quality, not exit status. A peer-ask or
  peer-review seat whose CLI exits 0 having printed only a permission banner
  (headless Antigravity's standing failure mode) is classified by the same
  answer-quality gate consult already uses: withheld from the ok and family
  counts — so "N independent model families" describes answers that exist —
  dropped from debate replay so a round-two banner cannot become the final
  answer, and rendered as a labeled non-answer in the synthesis instead of
  being quoted as an opinion. A blocked reviewer's `no-verdict` now quotes the
  provider's own stated reason so a gate failure names the operator's fix.
  Dry runs skip classification; `OMS_COUNCIL_QUALITY=0` restores
  exit-status-only accounting.
- A session end captures a handoff digest. The PreCompact snapshot script now
  also registers for `SessionEnd` (nine managed hook registrations; doctor
  checks all nine), because measurement showed the capture stream was dry:
  sessions that never compacted and never hit the pressure band left the
  resume hook nothing inside its 72-hour window. The digest note names its
  trigger, a 15-minute recency dedupe prevents twin digests (recency, not
  existence — an existence check would keep stale mid-session digests
  authoritative), and the harness-child skip keeps peer children's throwaway
  sessions from hijacking the newest-handoff pointer.
  `OMS_PRECOMPACT_HANDOFF=0` still kills the whole script;
  `OMS_SESSIONEND_HANDOFF=0` disables only the new trigger. Existing installs
  pick the registration up on the next `oms update` (or
  `install-claude-hooks.sh`); until then doctor reports the missing hook.
  Capture itself also grew a floor: a transcript with fewer than
  `--min-user-turns` user turns (default 2) is skipped — the floor lives in
  the writer so the hook, the pressure path, and every future caller inherit
  it, and it sits after the sensitivity scan so a short transcript carrying a
  secret still refuses loudly. `--min-user-turns 0` captures anyway.
- The unresolved-artifact queue gained mechanical triage. Each unresolved row
  prints the exact `resolve --event-id ... --reason ...` command that clears
  it, annotated `superseded-by: evt_X` when the failed patch's exact bytes
  were later admitted or landed (joined on `patch_sha256` only — never the
  path, never across sibling providers, and an empty-patch digest never
  matches); `--json` gains an additive `superseded_by` field. `--event-id` is
  now repeatable so a triaged batch clears in one locked call, fixing the
  silent last-wins overwrite when the flag was passed twice. Honest framing:
  this speeds triage to one bounded batch call — on live data only 2 of 17
  rows were mechanically superseded — it does not drain the queue for you.
- Journal lessons distill themselves at the daily tick. The prompt tick now
  runs the journal's distill pass once per local day on its own marker
  (independent of the digest's, which vanishes under
  `OMS_WORK_JOURNAL_DIGEST=0`), deduplicating candidates against what
  `agent-task close` already promoted into `shared.md` so decision-class
  lessons are never re-appended; skips are counted and reported, the
  through-marker still advances past them, and a distill failure cannot break
  the tick. `distill.json` gains an additive `last_run_date` key;
  `OMS_JOURNAL_AUTODISTILL=0` opts out.
- Stale cross-agent threads become advisory triage instead of permanent inbox
  noise. `oms thread list --stale` flags non-current open threads idle past
  `OMS_THREAD_STALE_TTL` (default three days) with the exact close command per
  row, `oms thread stats` reports mechanical utility counts, and the inbox
  now lists only the stale count (`OMS_THREAD_ATTENTION=0` opts out). Nothing
  auto-closes — an open thread is the only record an exchange happened, an
  undatable turn is `unknown` rather than stale, and `gc` still sweeps only
  closed threads.
- The incumbent session learns when a live peer joins its worktree. The
  SessionStart advisory only ever warned the newcomer, so the prompt hook
  now tails the hook ledger for other sessions' recent rows — excluding the
  session's own harness children, a filter also backported to the
  SessionStart detection — and warns once per neighbor, latched in a
  per-session `peers.json` outside `ctx.json`. Advisory only;
  `OMS_PEER_ADVISORY=0` disables.
- Context pressure becomes a measured advisory instead of a surprise. The
  Claude status line now persists its authoritative `context_window` reading
  to a per-session cache, and the prompt-time hook turns that number (or,
  for Codex, the rollout's latest `token_count` event, version-gated and
  fail-open) into at most two one-line advisories per session: a warn band
  (default ≤15% left) that also detaches a mechanical `session-handoff`
  digest capture, and an urgent band (default ≤8%) that says wrap up and
  migrate. Recovery above 30% re-arms both bands. Adopted repos only; the
  advisory never blocks, never migrates, and never injects a per-turn HUD —
  the 2026-08-06 cross-family debate on the codex-context-checkpoint plugin
  rejected forced migration and kept exactly this measured-early-warning
  slice. `OMS_CTX_PRESSURE=0` disables; `OMS_CTX_WARN_PCT`,
  `OMS_CTX_URGENT_PCT`, `OMS_CTX_REARM_PCT`, `OMS_CTX_CACHE_TTL`, and
  `OMS_CTX_CAPTURE` tune it.
- Sessions manage their own continuity. The sensitivity scrubber grew tiers:
  true secrets still block every write, but absolute home paths now
  normalize to `~` for repo-local git-ignored records — previously any Linux
  transcript tripped the flat pattern, so the PreCompact handoff always
  refused, `.oms/handoffs` stayed empty, and fail-ledger rows with paths in
  the failed command were silently dropped. A refused capture now leaves a
  fail-ledger trail (`kind=hook`) instead of exiting quietly. A new
  `SessionStart` resume hook prints one bounded `[oms resume]` block — active
  task packet with its verify command, newest handoff pointer, unresolved
  failures with next actions, and a live-peer advisory sourced from hook
  event heartbeats when another session used the same worktree minutes ago.
  `oms gc` sweeps aged handoff digests, and gate/delegate auto-verify probes
  `check.sh` for a `fast` mode before invoking it instead of assuming the
  template contract.
- Goals get an executable definition of done and a bounded driver. A plan
  now carries a goal-level acceptance command (`agent-plan init --accept`);
  `agent-plan accept` runs it and appends a receipt row (tree SHA, command
  digest, verdict) to `.oms/plan/progress.jsonl`. `oms goal-drive` loops
  acceptance → one `plan-run --next --land` → an exact commit of the admitted
  patch's paths, over an existing human-approved plan, until acceptance
  passes or a hard cycle cap parks it — with stuck detection (same tree, same
  failing acceptance twice), refusal of dirty trees and mid-run acceptance
  edits, and a recorded reason plus fail-ledger trail on every park. Shipped
  deliberately narrow after a cross-family design review: no plan generation,
  no replanning, no automatic lease reclaim — decomposition and recovery stay
  with the parent agent. A parked goal claims the daily state hint so the
  next session resumes it instead of rediscovering it, and peer calls that
  attach shared memory now include a query-ranked "relevant recall" section
  keyed on the operation's own prompt (FTS5/bm25 with a portable fallback) —
  the ranking machinery existed but no automatic path used it.
- `oms plan-from-spec` closes the loop from spec to driver: it reads a
  confirmed PROJECT.md, has a deep-tier peer decompose the remaining work
  into validated plan tasks (scoped paths, mechanical verify, in-proposal
  dependencies), and writes a proposal that becomes plan state only through
  an explicit `--apply` — which also derives the plan's goal and acceptance
  command from the spec when no plan exists. Draft specs are refused: the
  chain is PROJECT.md → propose → human approval → `goal-drive`, with the
  model's judgment always behind the approval gate.
- A durable-writers boundary contract suite locks today's expansion of
  persistent state down: one table-driven fixture drives the same secret and
  home-path sentinels through every writer — fail-ledger, session-handoff,
  shared memory, journal distill, plan receipts — asserting refuse, normalize,
  or digest-only per each sink's contract (a cross-family retrospective named
  this the highest-leverage next step). It immediately caught two gaps, both
  fixed: `agent-plan init` stored secret-bearing goal/acceptance text
  verbatim (now refused, matching the fail-ledger contract), and a
  scrubber-refused lesson crashed `journal distill` mid-run (now skipped and
  reported, the rest still promote).
- `oms journal distill` graduates episodes into lessons: blockers recurring
  across two or more days and explicitly recorded decisions since the last
  distill marker become compact `journal-distill:` entries in shared
  agent-memory (through the memory writer's own scrubbing), capped per run
  with any overflow said out loud, idempotent via an atomically-written
  cursor, `--dry-run` to preview. The episodic→semantic promotion path the
  audit found missing — the journal was write-mostly before this.
- Memory recall learns from its own use. Each recalled entry gains an access
  receipt (`memory_access`, keyed by the content-derived event id so a
  markdown re-derive cannot erase it); ranking nudges recently-recalled
  entries up one quartile and never-recalled 45-day-old entries down one —
  match order stays primary, nothing is ever deleted, reach just fades
  (search-time decay, not destruction).
- The continuity layer closes the autonomy loop. A session-handoff digest now
  carries its resume contract — the active packet's Verify command — and any
  review round that ended split rides along as open dissents the next session
  must acknowledge (agree, override with reasons, or escalate) instead of
  silently re-deriving consensus. `plan-run --land --auto-repair` turns a
  failed landing into exactly one repair delegation with the failed gate's
  own output embedded in the worker brief (the same `--review-artifact`
  channel peer-review findings use), retries the landing once, and then
  parks the task with a recorded next action (`oms advise`) rather than
  looping. Reviewers under `--gate` now state a calibrated CONFIDENCE before
  their GATE line and `verdicts` displays it for tiebreaks — advisory only:
  a confident wrong verdict still cannot outvote the mechanical check. The
  agent-harness autonomy reference gains "Deciding Without the User": take
  reversible forks yourself, weigh split verdicts by confidence, one repair
  round then a cross-family advisor, and face the user with results, not
  menus.
- The opinion-exchange loop closes where measurement said it leaked, based on
  a three-family council (codex, claude, antigravity answering the same
  design question), external research, and an eight-probe code audit that
  killed the plausible-but-wrong items first (the "peers emit diff text"
  hypothesis died against `capture_patch`; the MCP session-model concern died
  against the already-stateless server). What survived, shipped: peer-review
  gains `--writer PROVIDER` and benches the patch author's family from the
  default council (same-family re-judgment is correlated, not independent),
  with review-gates bounding post-gate fixes to one re-run; peer-delegate
  gains `--review-artifact` so prior review findings ride inside the worker
  brief — findings routed as side artifacts get ignored, embedded ones get
  addressed — and the delegation's index row records the review as its
  source, joining review to patch; the delegate epilogue now routes unapplied
  patches through `oms patch-land` instead of a raw `git apply` that
  sidestepped admission; the fail-ledger validates `--kind`, records a
  recommended `--next` action surfaced by `check`/`list`, and `resolve --how`
  finally gives the repeated-then-resolved forge hint content to forge;
  resuming an active packet whose Verify contract has not passed against the
  current tree says so in the injected context; `skill-forge status` flags
  project skills untouched past `OMS_SKILL_STALE_DAYS` (default 90); and the
  MCP server clamps `protocolVersion` to revisions it actually implements
  instead of echoing false conformance. Every new mechanism is advisory or
  additive — no new hard gate ships before its effect is measured.
- Project skills get a cold-start nudge and standing guidance, so forging
  them is a habit every CLI learns rather than a feature only `agent-task
  close` mentions. The skill router's daily state hint gains a third rank:
  when the fail ledger shows a failure that repeated and was then resolved
  but `.oms/skills/` holds no project skill yet, the hint offers
  `oms skill-forge add` — once per local day, outranked by a stale task and
  by unresolved failures, silenced as soon as the repo has any project skill.
  The ledger is read raw for this because `list` zeroes a fingerprint's count
  on resolve, which erases exactly the repeated-then-resolved signal the hint
  keys on. The shared rules gain a one-line forge habit paid for inside their
  500-word budget — by trimming restatements (the hook enumeration the rules
  themselves say not to trust, the no-blanket-override echo, the Gemini
  retirement note) rather than raising the cap — and the agent-harness skill
  carries the full workflow: a repeating fix or procedure becomes a project
  skill; machine and cluster facts (`sinfo`/`sacctmgr`, GPU inventory) stay
  in `oms snapshot [--cluster]` references, documented in the state-memory
  reference.
- Provider HUDs are useful by default after install without taking over user
  settings. Claude's main HUD shows session/repository identity, cached Git
  branch/change counts, fast mode, model/effort, context, cost, and rate-limit
  reset countdowns; it stays width-aware and makes no API call. Git is refreshed
  at most every five seconds per session with a two-second ceiling. A new
  `subagentStatusLine` renders each visible worker's model/effort, context use,
  elapsed time, and state without descriptions or paths. Codex uses its native
  `tui.status_line` for model/reasoning, remaining context, 5-hour/weekly limits,
  and branch. Both installers preserve user-owned settings; uninstall removes
  only unchanged OMS-managed entries. Antigravity exposes no equivalent footer.

### Fixed
- Model selection is now catalog-first: tier policy and hard-coded provider
  model names are removed, while explicit model/effort validation and cached
  catalog visibility remain.
- The check gate is hermetic to the invoking git context. Git exports
  `GIT_DIR` to hooks, and from a linked worktree that path is absolute, so
  it overrode repository discovery for every git call in every test
  fixture: a pre-push run from a worktree rewrote the checkout with fixture
  commits and failed `autonomy-plan-run` with "refusing to land: main tree
  has uncommitted changes" (from the main checkout the exported path is
  relative and resolved inside each fixture by accident, which is why the
  leak never surfaced before). `check.sh` now unsets the `GIT_DIR` family on
  entry, so pushing from a clean linked worktree — the documented recovery
  when the main tree carries another session's edits — gets the same green
  gate as pushing from the main checkout.
- Auto-update trigger state round-trips through the install receipt.
  `oms auto-update install` wrote systemd/cron state but never the receipt,
  so the next `oms update` reconciled components from the receipt and
  silently uninstalled the timer it had just been asked to keep; the same
  reinstall also downgraded an apply-mode trigger to the check default,
  because the chosen mode was recorded nowhere. The trigger scripts now
  record `components.auto_update`, and their check/apply mode in
  `component_modes.auto_update`, through a surgical owner-guarded receipt
  write; update.sh restores the recorded mode when it reinstalls the
  trigger. Schema-1 receipts stay untouched — update.sh probes the
  scheduler directly for those.
- The Stop-hook turn guard parses its verdict instead of substring-matching
  the serialized JSON, so a formatting change in the emitter can no longer
  make the Work Journal mirror blocked answers as finished work. Malformed
  guard output still fails open, exactly like the guard itself.
- A failed install names its recovery path. install.sh runs under `set -e`
  with no trap, so a crash after link.sh died with only the failing tool's
  error and left a partial install for the reader to diagnose; an EXIT trap
  now states the exit code and points at the idempotent rerun or
  `doctor.sh --repair`.
- The installer accepts uv-only machines. A host with `uv` but no `python3`
  command (fresh workstations, cluster accounts) failed hard with "a Python
  3.9+ 'python3' command is required", and no environment variable could
  satisfy the check — found live on a cluster login node. `ensure_python3`
  now falls back to uv's managed CPython: it writes the managed shim as
  `exec "$(uv python find)" "$@"` (resolved at call time, so the shim
  survives uv relocating or upgrading interpreters), installs a CPython
  through uv when none exists yet, and the shim-ownership contract
  recognizes the new shape so uninstall still removes only what the
  installer wrote.
- The Notion mirror now works on machines without a usable OS keychain, and
  the transport choice the docs always promised is actually persisted.
  Connecting on a headless/sandboxed host failed with ntn's "Failed to
  create keychain entry", and even after a manual file-based login the hooks
  could not inherit the choice — `notion_settings()` re-read `OMS_NOTION_CLI`
  from the environment on every call and persisted nothing, so publishing
  depended on PATH order and ambient env (found live: a session whose only
  fix was a hand-patched shim that the next `install-tools --upgrade` would
  silently regenerate). `connect-services` now detects the keychain error,
  switches to ntn's file-based store, names `NOTION_KEYRING=0 ntn login` in
  its hints, and persists `keyring: "file"` plus `cli_command` in
  `work-journal.json`; the transport reads both from config, environment
  still overriding per invocation. Verified by bypassing the shim entirely:
  a sync with no env and the raw CLI first on PATH publishes from persisted
  config alone.

### Changed
- A fresh install now registers the auto-update trigger in **apply** mode:
  the daily schedule fast-forwards a clean checkout instead of only recording
  that an update exists. The safety envelope is unchanged — fast-forward
  only, dirty or diverged checkouts are skipped, the apply lock serializes
  runs, and the doctor validates the result. Opt-outs stay explicit:
  `OH_MY_SETTING_AUTO_UPDATE_MODE=check` (or
  `oms auto-update install --check-only`) records without touching, and
  `--no-auto-update` skips the trigger entirely. Existing installs are not
  flipped: the receipt's recorded mode wins on update, and legacy receipts
  with no recorded mode keep the old check default.
- The shared global rules absorb the guidance that previously lived only in
  Claude-local `~/.claude/rules/*.md` files, so all three CLIs receive it
  identically through the one linked `rules/global-AGENTS.md`: model routing
  by phase with spawned workers one tier below and judgment roles at session
  tier, independent advisors over same-family ones, the peer-CLI wrapper
  contract (peer-ask/review/delegate, agent-consult, advise; Gemini CLI
  retired in favor of agy), harness hook awareness with live-wiring
  inspection over prose lists, and the context-window tail guidance.
  CLI-specific machinery (settings paths, hook registration commands, the
  Claude-only subagent catalog) stayed out — each CLI's own surfaces already
  carry it.
- Auto-update is on by default. A fresh install registers the check-only
  trigger (systemd timer, or cron when no user manager is reachable) so a
  harness cannot silently go stale: it records update availability and
  applies nothing. `--no-auto-update` or `OH_MY_SETTING_AUTO_UPDATE=0` opts
  out, and applying updates remains an explicit choice via
  `OH_MY_SETTING_AUTO_UPDATE_MODE=apply`. Existing installs keep their
  recorded choice — the new default changes only what a fresh install does.
- The `oms` dispatch allowlist is data, and a gate keeps it honest. The
  public/compat catalog was one 500-character case line that nothing
  reconciled against `scripts/` — a rename could leave a ghost entry that
  only failed at dispatch time. Both classes are now sorted
  one-name-per-line lists, and a smoke gate asserts every allowlisted tool
  has a script, nothing is classified twice, the lists stay sorted, and
  every `scripts/*.sh` is deliberately public, compat, or internal.
  `skill-doctor` joins the public catalog: the ops skill documents it, but
  it was reachable only as a side effect of doctor and cleanup.
- The public `oms` catalog consolidated to one front door per capability —
  less is more: an agent choosing between 58 entries picks worse than one
  choosing between 51. `oms snapshot [--cluster]` fronts the machine and
  Slurm context generators; `auto-update install|remove` absorbs the trigger
  installers; `code-source github` absorbs the GitHub source; `artifact-index
  import` absorbs the external-answer importer; `oms run capsule` fronts the
  reproducibility capsule. Every consolidated name stays dispatchable through
  a compat list (old habits, receipts, and already-applied project templates
  keep working) but no longer appears in `oms list`; the dispatcher test pins
  both the hiding and the compat dispatch. The `generate-slurm-skill` shim
  script is gone — its name survives as a dispatcher alias, and its garbled
  catalog line ("(local/slurm.md), not a skill…", the scraper reading a shim
  comment) with it. Templates and skills now name the new doors.
  The local daily files are the evidence layer — every claim carries an
  event-id citation, and one packet update can emit near-identical bullets
  from its update and close events — but Notion is the surface the human
  actually reads, and the citations made the page unreadable in review.
  Publishing strips the trailing `[wj_...]` citations and folds bullets that
  differ only by citation, per section. On review the page still read like a
  git log, so the same pass now also drops `Commit <hash>:` prefixes (the
  message is the human line; hashes stay in the local evidence layer),
  removes sections that say nothing, and floats decisions, blockers, and
  next priorities above the progress listing. Deterministic text transforms
  only: nothing is summarized or rewritten, and local files keep full
  provenance. Pages also carry a kind icon (📔 daily, 📚 weekly, refreshed on
  update so pre-existing pages pick it up), and the progress listing
  publishes as a collapsed toggle — the page opens on judgment, the commit
  detail unfolds on demand, and a nested-limit note replaces overflow past
  the API's per-block child cap. A review of the live page tightened the
  same pass further: the not-yet-verified listing collapses too, per-project
  `###` subsections stay inside their toggle instead of ending it, labeled
  commit bullets (`작업: Commit <hash>:`) lose the hash prefix, and raw
  evidence-reference bullets (full hashes, task ids) stay local — three
  near-identical listings no longer render three times uncollapsed.

### Added
- `oms support-bundle [--out DIR] [--dry-run]`: redaction-first diagnostic
  bundle. Harness state is rich and therefore unsafe to copy ad hoc into a
  peer consultation or bug report. The bundle collects only bounded derived
  summaries (repo-state, state-verify, task and journal status, fail-ledger
  rows, artifact index counts — never artifact contents, raw logs, diffs,
  datasets, or credentials), strips the repo path, home directory, and
  hostname from every file, and then runs the shared sensitive-content
  scrubber — a file that still matches is replaced whole by an omission
  notice, the last line of defense for state that arrived past the writer
  rails. `MANIFEST.md` records what was included, what was omitted and why,
  and what is never collected. Ranked #5 by the 2026-08-01 peer council;
  its #3 (git merge protocol for `.oms`) was rejected on inspection — the
  standing contract keeps `.oms` out of git entirely, so the conflicts it
  would classify cannot exist — and #6 (timeout probes in the
  machine-conditional gate) was rejected because `shutil.which` cannot hang
  and execution probes would make skill links flap on transient daemon
  stalls.
- Project skills may declare a verification contract: an optional `verify:`
  frontmatter command naming the evidence the skill expects. The forge
  validated structure and sensitive content but not whether a skill's rails
  produce observable outcomes, which let skills drift into aspiration. Rails
  stay rails: validation is syntax-only (an empty or unparseable command
  fails), `skill-forge status` counts declared contracts, a new
  `skill-forge contracts` subcommand lists `name<TAB>command` rows, and
  `agent-task close` prints one reminder per contract — the harness never
  executes standing context on its own authority. Ranked #4 by the
  2026-08-01 peer council.
- `oms state-verify [--json]`: read-only consistency verdict over a repo's
  `.oms` tree. Shared state is the harness spine, and a dangling pointer or a
  contradictory packet is inherited as false confidence by every CLI that
  reads it — while each existing validator judged only its own family. The
  command composes those engines (`oms-run validate`, `artifact-index
  validate`, task and journal status) and adds the cross-family checks none
  of them owns: `CURRENT` pointers naming missing threads or runs, active
  task packets carrying `closed_at`, unparseable packet timestamps that make
  staleness checks silently pass, delegation markers whose worker process is
  dead, lock entries inside `.oms` (the locking contract keeps locks in
  `OMS_LOCK_DIR`), a missing `.oms/.gitignore`, and derived journal views
  behind `events.jsonl`. It never repairs; every finding names the command
  that would. Ranked #2 by the 2026-08-01 peer council.
- `oms provider-contract --check`: cross-CLI conformance gate. The 0.4.0
  GEMINI.md incident showed native-loader drift — one CLI following rules the
  other two retired — is the highest-risk failure class, and no single-script
  test proves the provider integration. The gate verifies loader parity on a
  throwaway fixture (identical managed block sets across `AGENTS.md`,
  `CLAUDE.md`, and an adopted `GEMINI.md`, stale base styles retired on a
  style switch), harness-state MCP registration parity (every registered
  provider reads one and the same `oms-mcp-server.py`), and the fail-open
  contract (hook scripts exit 0 without seeding `.oms` in an unadopted
  directory). Read-only outside its own temp fixture; a missing provider CLI
  is a note, never a failure. Ranked #1 by the 2026-08-01 peer council.

## [0.4.0] - 2026-08-01

### Added
- ML project skills installed by the ml template: `ml-experiment` (check the
  experiment board, pre-register hypothesis runs, gate long runs through the
  run ledger, capture a capsule, record outcomes) and `dataset-safety`
  (manifest registration with declared group keys, leakage and drift checks
  before training). Shipped as `templates/project-skills/`, installed per
  repo through skill-forge — so they exist only in ML projects, load through
  native project skill discovery in all three CLIs, and pass the same
  validation and scrubbing gate as any forged skill.
- Project skill forge (`oms skill-forge`): distill inspected repo facts —
  build/test invocations, repo-specific pitfalls, domain data rules — into
  project-scoped skills under `.oms/skills/`, linked into `.agents/skills/`
  and `.claude/skills/` so all three CLIs load them through native project
  skill discovery (no router entry, no manifest). Rails, not generation: a
  skill must pass the Agent Skills validation (name/dir match, routable
  description, 500-line budget) and the outbound scrubber — a project skill
  is standing context for every future session, so secret-shaped content is
  refused at the door. Links are hidden from git via project-private and
  withdrawn when a skill goes invalid; the doctor reports project-skill
  health per repo. `oms init` and spec-interview point at the flow, and
  `agent-task close` hints at promoting a lesson when a repeated failure
  was resolved in this repo (`OMS_SKILL_FORGE_HINT=0` opts out).
- Machine-conditional skills: a manifest entry may declare
  `"requires": ["cmd", ...]`, and the skill links, routes, and doctors only
  on machines where every listed command exists — link removes the links
  when a machine loses the command, the router never suggests a skill the
  session cannot load, and the doctor reports the skip as a note. First
  two: `slurm` (requires `sinfo`) — answer cluster questions from the
  private reference (partitions, node/GPU inventory, and the
  `sacctmgr show assoc`/`show qos` account limits) instead of re-probing
  nodes, submit with fail-fast discipline, digest and reconcile job logs —
  and `gpu-workstation` (requires `nvidia-smi`) — check VRAM first,
  serialize runs through the tsp queue, triage CUDA OOM in order. The
  general catalog stays exactly five everywhere else.
- A read-only harness-state MCP server (`oms-mcp-server.py`): task packet,
  fail-ledger, handoff digests, and Work Journal of a repository as MCP
  tools, so every MCP client reads the same state with no per-CLI hook code.
  `install-mcp.sh` registers it with Claude Code (user scope) and Codex;
  Antigravity receives it through a new `agy` plugin
  (`install-agy-plugin.sh`), which bakes this checkout's absolute paths at
  import time because `agy plugin install` copies the directory verbatim.
  All three paths verified live: claude reports the server Connected, and a
  headless `agy -p` call returned real task state through it. The consult
  permission profile now grants `mcp(*)` — probed on Antigravity 1.1.9,
  scoped `mcp(...)` targets do not match, the same non-convergence that
  makes `command(*)` the command rule. The agy plugin carries no hooks: a
  probe showed no hook event fires headlessly on 1.1.9, and the harness
  ships only surfaces it can verify. install.sh/update.sh wire both
  installers; uninstall removes both registrations.
- State-conditional router hints: the skill router injects the one thing
  native description matching cannot know — this repository's harness state.
  A stale active task packet, or two or more unresolved fail-ledger rows,
  produces a one-line reminder at most once per local day per repo
  (`OMS_STATE_HINTS=0` opts out; harness-adopted repos only; fail-open).
- `generate-slurm-reference.sh`: the honest name for what the script writes
  (a private cluster reference at `local/slurm.md`, not a skill).
  `generate-slurm-skill` remains as a compatibility shim and `oms` alias.
- Work Journal now has bounded top-level turn boundaries: prompt start performs
  local rollover plus one two-second pending Notion retry, work-time observers
  stay local, and an allowed Stop captures final `HEAD` then publishes only
  today's daily summary. GitHub SSH/HTTPS remotes share one normalized project
  identity. The default installer adds the official `ntn` CLI and delegates
  `gh`/Notion browser login to their CLIs, discovers a unique compatible data
  source, and persists no credential; non-interactive auto mode remains
  non-blocking while `--connect-services` makes incomplete setup fail.
- A `PreCompact` handoff-snapshot hook (`precompact-handoff.sh`): just before
  Claude Code or Codex compacts a session, a session-handoff digest is
  captured into the project's `.oms/handoffs/` while the transcript detail is
  still lossless. Registered for Claude by `install-claude-hooks.sh` and for
  Codex by the plugin; best-effort by contract (a hook that blocks compaction
  costs more than a missing digest), harness-adopted repos only, and it never
  falls back to newest-session guessing when the payload names a session it
  cannot resolve — a concurrent session's work must not be snapshotted under
  this one's label. `OMS_PRECOMPACT_HANDOFF=0` opts out.
- The doctor verifies the Claude hook registration it depends on. install.sh
  treats hook-registration failure as a warning and continues, so an install
  could report healthy with no skill routing at all; `check_claude_hooks` now
  reads `settings.json` and fails with the remedy when any of the four hooks
  or the HUD is missing, gated on a valid install receipt so bare checkouts
  and sandboxed homes stay quiet.
- Codex skill-picker metadata (`agents/openai.yaml`) for the four skills that
  lacked it; only trust-boundary had a display name and default prompt.
- `validate-skills.py` enforces Agent Skills spec conformance the other
  runtimes rely on: the skill name must match its directory, the description
  must be substantial enough to route on, and SKILL.md stays under the
  500-line progressive-disclosure budget.

- The daily Work Journal digest points at the newest session handoff. Handoffs
  are captured manually and loaded manually, and the docs' "loading it is your
  step" is exactly the step a fresh session forgets it has. The first prompt
  of a local day now carries one line — the handoff's name, age, and the
  `oms session-handoff show` command — when one was captured in the last 48
  hours. A pointer only: the digest never inlines another artifact's content.
- Failed Bash commands now feed the failure ledger by themselves. fail-ledger
  has recorded, gated, and named `oms advise` at the repeat threshold since it
  existed — but only when something called it, and the primary agent in the
  middle of a failure loop never did (the same gap the delegate repair-round
  advisor closed for workers). A Claude Code `PostToolUseFailure` hook now
  makes the call: on a failed Bash tool run in a harness-adopted repo it
  surfaces the ledger's prior context for that command into the conversation,
  then records the failure so the second identical attempt meets the advise
  threshold mechanically. Silent on first failures, matcher-scoped to Bash,
  skips interrupts/SIGPIPE, never seeds `.oms` into unadopted repos, fail-open
  with a 5s ceiling; `OMS_FAIL_LEDGER_HOOK=0` opts out.
- Install closes the one setup step that still lived in a person's head:
  Antigravity's headless permissions. A fresh install ran the tools installer,
  hooks, and plugins, then left the first `peer-ask`/`peer-review` council to
  discover that antigravity answers nothing without `permissions.allow` rules
  — a denial that exits 0 and looks like an empty opinion.
  `install.sh --peer-permissions` grants the consult profile at install time
  (through the existing `provider-permissions.sh`, `.bak` beside the widened
  settings); a default install instead reports exactly what a headless peer
  would be denied and names the flag, because widening another program's
  authority is not something an installer should do silently.
- Claude Code installs a compact local status-line HUD showing the active model,
  live context occupancy and token capacity, available five-hour/seven-day
  subscription usage, estimated session cost, and reasoning effort. It uses
  Claude Code's supplied JSON without API calls, handles pre-response nulls,
  strips terminal controls, preserves an existing user `statusLine`, updates
  only its own command path, and removes only its own entry on uninstall.

- Work Journal adds automatic local-first project memory with a small
  `oms journal` status/rebuild/sync/configure surface. Existing run, capsule,
  artifact, Agent State, review, CI/PR, and
  experiment/job receipts are projected through one fail-open observer into a
  schema-versioned append-only event log and deterministic daily/ISO-week
  Markdown. A rebuildable SQLite index makes normal deduplication,
  changed-period rendering, and summary export incremental, while the JSON
  projection is bounded to 256 recent descriptors. Stable source IDs make
  replay idempotent, late events rebuild old periods, commit rollover is
  local-only, and recursive sanitization excludes credentials, transcripts,
  raw logs, environments, and diffs. An optional standard-library Notion
  adapter stores only nonsecret connection metadata during install and mirrors
  finalized changed summaries as native blocks with stable-key upsert, bounded
  Retry-After-aware retries, and pending local sync state; no credential means
  no network.
- Work Journal grew the read half it was missing: capture without recall is a
  recorder, not memory. `oms journal show --today|--week|--period P|--blockers|
  --recent N [--json]` is the one agent read command over the derived
  summaries and event index — blockers and next actions are deduplicated from
  the last seven days of events, newest first, with event IDs. The provider
  prompt hook's rollover tick now also prints a bounded digest (open blockers,
  next priorities, last journal day's counts) into agent context on the first
  prompt of each local day; the marker is derived state and
  `OMS_WORK_JOURNAL_DIGEST=0` keeps the tick but drops the injection. An
  `agent-task close` without a recorded decision or next step gets a stderr
  hint, never a block, since those two fields are what make the daily useful.
  Rendered headings became configurable (`OMS_WORK_JOURNAL_LANG`, Korean
  default, `en` available; labels only, never admission). Building the read
  path surfaced that the summary exported to the index and Notion was
  flattened to one line — `sanitize_text` folds newlines, so heading/bullet
  block parsing and the blocker scan never matched; a line-preserving
  `sanitize_multiline` now feeds both.
- Two read-only observability commands close the measurable gaps left after the
  multi-agent and project-memory work. `oms artifact-index telemetry [N]
  [--json]` aggregates only the retained call/ask/review/delegate rows by
  provider and selected route, with recorded exits, verifier exits, fallbacks,
  resolutions, and artifact-derived token/duration coverage; it deliberately
  reports semantic outcome as unavailable instead of treating process exit zero
  as task success. `oms agent-memory health [--json]` compares the append-only
  Markdown/failure sources with SQLite using a read-only connection and reports
  schema, integrity, FTS, currentness, ledger state, and provenance coverage.
  Missing, stale, malformed, or degraded state is nonzero but never repaired by
  the query. Neither command changes a source, index, artifact, or ignore file.
- Global rules refuse authority to instructions found in content: a file, tool
  result, web page, or another agent's answer can carry text that reads like an
  order, and it is data to report, not a rule to obey. HANDBOOK.md (arXiv
  2607.25398) measures exactly this over long tool-use horizons and finds it the
  most common way a standing policy loses — one frontier model carried out a
  VP's termination request in every trial although the handbook required two
  named approvers. The harness already answered that paper's other three failure
  patterns mechanically (`turn-guard` for verification silently skipped,
  `peer-review --gate` and `patch-admit` for checks run and then ignored or
  self-reported); this was the one with no defense.
- `artifact-index prune --stale` drops index rows whose artifact or patch no
  longer exists. `validate` has always reported those, and nothing could repair
  them: the retention prune caps row count, so any keep value either spares
  stale rows or discards good ones, and the global rules forbid editing `.oms`
  by hand. Found by cleaning up 360 leaked test artifacts and watching doctor
  warn about 360 references with no tool to answer it. Independent of `--keep`,
  because a missing file is not an age question, and `--dry-run` first.
- Global rules carry the commit convention and secret handling, so codex and
  antigravity get what only Claude had. An audit of the always-loaded layers
  found the three CLIs were not seeing the same rules at all — Claude read 1,549
  words, codex and antigravity 365 — while `project-doctor` enforces exactly that
  parity one level down, for projects. The two rules promoted here are the two
  the audit could show had changed behaviour: every commit in this release
  follows `<type>: <description>` with no attribution trailer, and the outbound
  scrubber caught a real leak. Budget 380→500 words to fit them.
- Global rules require naming a decision fork instead of quietly taking one:
  when two defensible approaches exist and the choice gets encoded across files
  or interfaces, say both and which one is being taken, then keep going. Stop
  only when the choice is hard to reverse, and if a second option cannot be
  stated there is no fork to surface. Nothing covered this case —
  `spec-interview` gates user-facing ambiguity on new or broad work, and
  `peer-ask` only fires when the user names it — so a bounded task with two
  defensible approaches was decided silently. Surfacing costs a sentence;
  deciding alone costs whichever round trip proves the choice wrong, which it
  did this release when the gate split was built on environment variables. The
  rules budget moved 320→380 words and 140→150 lines to fit it: the smallest
  round bump with slack, since this file is standing context in every session on
  all three CLIs.
- CI runs the default install — tools enabled — which was the one path it never
  executed, and the path most users take. It is also what turned CI red for three
  commits: `install-tools` persists a PATH line into `.bashrc`, while the
  assertion left over from before tools were installed by default said that file
  must stay untouched. Every tool is stubbed as already present so each step
  short-circuits, keeping it a no-network test of the documented behaviour: a
  default install edits shell startup files and a `--no-tools` one does not.
  Verified to fail when the PATH persistence is removed.
- The path-spelling contract behind the macOS failure is pinned and now
  reproducible on every platform, since it cannot be reached through `TMPDIR` on
  Linux — GNU mktemp collapses the `//` that BSD mktemp keeps. Writing it down
  surfaced an asymmetry worth recording: copy mode compares realpaths and accepts
  any spelling, the symlink branch compares the `readlink` string and does not.
  Nothing depends on it because every caller derives its source from `$ROOT`, so
  the test pins what both modes share rather than freezing the difference as if
  it were designed.
- `tests/bsd-portability-smoke.sh` restores the fixtures that were deleted with
  the old macOS job: `detect-project-style`, `apply-project-template` +
  `project-doctor`, `job-digest`, `run-ledger`, and the run-tools set
  (`oms-run`, `run-capsule`, `data-manifest` leakage, `experiment-board`). They
  lean on `sed`, `awk`, `date`, and `sort`, which is the one breakage class a
  Linux-only gate cannot see. As inline CI steps they could not be run locally
  and vanished silently; as a test file they run in `check.sh` and on macOS.
- CI runs the install lifecycle in copy mode on Linux. Copy mode was provable
  only on the Windows runner — the slowest leg in the matrix — although forcing
  `OH_MY_SETTING_LINK_MODE=copy` passes on Linux end to end. A marker leak or a
  lost backup now fails in seconds instead of waiting for Windows.
- Lint runs as its own CI job. `check.sh` exits at the first failing stage, so
  one shellcheck nit would suppress every test result; `--lint-only`/`--no-lint`
  split the gate without running anything twice, and a run that would execute
  nothing is refused.
- `scripts/check-python.sh` syntax-checks the Python helpers. Every `.sh` here is
  linted while the `.py` files had nothing, and one of them now decides what the
  installer may delete. Compiled in memory, so a read-only gate does not write
  `__pycache__` into the tree it is certifying.

- A repeated failure now names an advisor on the primary agent's own path. The
  rules have always asked for an outside read after repeated failures, and
  `peer-delegate` does it from the second repair round; the primary agent's gate
  failures reached the ledger — since earlier in this release — and escalated
  nowhere, so it filed the row and tried the same thing again. `fail-ledger
  record` and `check` now name `oms advise` once the same fingerprint has failed
  twice unresolved (`OMS_ADVISE_AFTER_FAILURES` sets the threshold, `0` disables
  it), a resolve zeroes the run, and no model is called: the line costs one
  stderr write and the decision to spend an advisor call stays with the caller.
  `agent-task verify` silences the ledger's bookkeeping, which would have made
  this dead on arrival — the one line it exists to produce was going to a stderr
  the caller discarded — so the escalation is let through and the rest stays
  quiet. Verified end to end on the real path: first failure silent, second
  surfaces the advisor, a passing gate resolves the row.
- `doctor` reports a provider CLI that is installed but not logged in. Forcing the
  install guarantees binaries, which moves the failure one step later: a present
  but uncredentialed CLI answers nothing, and the harness used to discover that by
  spending a provider call on it. Each of the three keeps its credential in a
  local file (`~/.claude/.credentials.json`, `~/.codex/auth.json`,
  `~/.gemini/oauth_creds.json`), so this is answerable here — locally, with no
  network, for the same reason as the `gh` check. Presence of the file is all that
  is claimed; whether the credential is still valid is the provider's answer to
  give. Reported, never failed: an interactive login is not an install defect. The
  doctor fixtures fabricate all three, as they already do for `gh` and Antigravity
  permissions, so a clean install does not warn about something it cannot fix.
- A router precision test. Triggers are substring-matched against the whole
  prompt, so precision is a property of the manifest that nothing checked: the
  `trace` triggers were verified by hand against a re-implementation of the
  matcher, which is exactly the kind of check that is not repeated. The test
  drives the real hook over a corpus in both directions — five causal prompts
  must reach `trace`, five neighbouring ones about regression tests, flaky tests,
  refactors, commits and training scripts must not. A skill pushed into every
  session that mentions a test is a permanent context tax, and a router that
  cries wolf stops being read.

- `patch-admit` rejects a patch that quietly weakens the tests. The verifier gate
  already stopped a patch from rewriting the verify entrypoint, which left the
  other route to self-certification open: delete the assertions and the suite
  passes honestly. Measured on a fixture, the ladder read `verify: PASS`,
  `verifier: PASS` — nothing else objected, so the patch would have landed. The
  new gate counts assertion lines added and removed in the changed test surface
  (`tests/`, `spec/`, `*_test.*`, `test_*`, `*.test.*`) and fails on a net
  removal or a deleted test file, naming the count. `--allow-test-reduction` is
  the explicit promotion for consolidating or replacing coverage, mirroring
  `--allow-verifier-change`; adding assertions is unaffected, because a gate that
  fires on ordinary work is a gate people switch off. Adopted from harness
  designs that require operator promotion for any change to a safety surface, so
  an agent cannot relax its own guardrails.
- `trace` skill: explain an observed failure before changing anything. The gap was
  measured, not assumed — `research-method` covers designing an experiment,
  `fail-ledger` records what broke, and `autonomy-loop` says to change the
  hypothesis, but nothing said how to find a cause, which is the most expensive
  situation in this kind of work. It carries three things: the contract
  (observation, competing hypotheses, evidence for and against, the critical
  unknown, and the cheapest probe that separates the leaders), a ranking of
  evidence from controlled reproduction down to intuition, and the instruction to
  refute your own favourite first. It hands off to tools that already exist —
  `oms fail-ledger check --cmd` for prior evidence, `oms consult --all` for
  hypotheses from other model families, `oms advise --prompt` before an
  irreversible call. 289 words with no references, against a 1,502-word source
  built on Claude-Code-only team orchestration: the method transfers, the
  orchestration does not. Its test caps the size and asserts that the three
  commands it names are still the forms those tools document, because prose that
  has drifted from the interface it cites is the defect this repository keeps
  paying for.

- Recall now spans everything the harness already records, not just prose notes.
  The SQLite index takes the failure ledger as a third source, so one
  `agent-memory recall` answers "what do we know about this" across notes, pins,
  and what has already gone wrong — where `fail-ledger check --cmd` only ever
  answered "this byte-identical command failed before". Indexing is mechanical:
  a ledger row already carries the command, the exit code, and the failing line,
  and no model is asked to describe anything. Schema 3; a schema-2 database
  upgrades by re-deriving, because every row in it is derived from the Markdown
  logs and the JSONL ledger that are still on disk.
- Closing a task keeps the two lines worth recalling instead of dropping them.
  The packet already holds a `## Decisions` and a `## Last Failure` written
  during the work; close promoted only the goal and the next step. It now
  promotes the latest decision and pitfall as well — text that already exists,
  so the cost is zero tokens, and it is the only content in this flow a later
  recall can act on: a command and an exit code carry the symptom but never the
  reason.
- The failure ledger now sees the gate the primary agent actually runs.
  `agent-task verify` files a row when the stored verification command fails —
  carrying the failing line, written by the shell with no model involvement —
  and clears that row when the same command later passes, so the ledger answers
  "is this still broken" rather than "did this ever break". Recording only ever
  covered harness-mediated paths (`plan-run`, `patch-land`, `peer-delegate`),
  which is why one repo held two rows across 136 commits while the same gate
  failed the same way more than once in a single session. The readers were
  already wired: `patch-land.sh:305` and `plan-run.sh:220` ask `check --cmd`
  before acting, so a row filed here is consumed by machinery already running.
  `resolve` accepts `--cmd` as well as `--fingerprint`, because a caller that
  just watched a command pass holds the command, not the hash it was filed
  under.
- Settled by a three-model council, which disagreed usefully: hooking the
  project's `check.sh` was rejected because the project this serves has none
  (its gate is `uv run pytest`), and a `resolve --summary` field was dropped
  because `record --summary` already carries the diagnosis. Session-start
  memory injection is deferred until the write side is non-empty.
- Project memory now has a local SQLite query index at
  `.oms/memory/memory.sqlite3`. The append-only `shared.md` and `pins.md` logs
  remain the human-readable source of truth, so the change is reversible and
  existing projects migrate automatically on `oms init`, append, pin, search,
  or recall. Exact `search` keeps its case-insensitive substring contract;
  `recall` adds ranked local FTS without a model call or embedding download.
  Schema 2 links each new project note to a stable event ID, kind, active
  task/session hashes, current Git HEAD, dirty bit, and bounded state
  fingerprint. The append-only Markdown source carries that provenance through
  rebuilds, compact provider context omits it, and `search`/`recall --json`
  exposes it only on demand. Existing schema-1 databases and metadata-free
  notes migrate automatically with stable legacy IDs.
  Concurrent indexers use a bounded SQLite write transaction, `rebuild`
  recreates a damaged derived database from Markdown, and `doctor` checks its
  schema and integrity.
- Routing follows what each provider CLI can actually do, instead of asserting
  it. Two assertions in the table had gone stale while the CLIs moved, and a
  stale capability is not a harmless comment: antigravity was refused a
  `--effort` control it has had for a while ("antigravity has no effort flag"),
  and claude's `xhigh` and `max` were unreachable because the shared validator
  stopped at `high`. A capability snapshot now records each CLI's thinking
  mechanism, the scale it accepts, and its live model catalog; `model-doctor`
  writes it as a by-product of the probes it already runs, and routing only
  reads it — a route decision must not spawn a provider call. Where a catalog
  exists, a tier picks the first of its candidates that the catalog actually
  has, so a retired model slides to the next one instead of failing the call; a
  model named outright by an operator is still used as given.
- Codex has a local catalog after all: its app-server answers `model/list` over
  JSON-RPC without starting a conversation, so enumerating models costs a local
  process and no model call. It also reports each model's own reasoning-effort
  scale, which turns out to differ per model — `gpt-5.6-sol` accepts `ultra`,
  `gpt-5.6-luna` stops at `max`, `gpt-5.5` at `xhigh` — so effort is now
  validated against the model that was actually chosen, and the provider-level
  check uses the union of its models' scales. Both `model-doctor` and routing
  read catalogs through one function; `codex models` (which is not a
  subcommand) is gone. Claude Code has no catalog source and is reported as
  unverified rather than assumed good. Catalogs are probed once per doctor run
  and read back from the snapshot, rather than asked for twice: `agy models`
  started while a previous agy invocation is still exiting hangs until its
  timeout, so the second ask was intermittently no answer at all.
- Delegation depth is enforced instead of merely written down. The project
  rules said a worker does not recursively delegate; nothing checked it, and a
  worker that tried simply succeeded. Depth is now counted and capped
  (`OMS_DELEGATE_MAX_DEPTH`, default 1), because nesting multiplies: a worker
  that delegates can spawn workers that delegate, and cost, worktrees, and the
  number of things writing to one repository grow with the power, not the sum.
  The refusal names the alternative — do the work, or report what needs
  splitting so the caller fans out — and points at `oms consult`, which is
  read-only, widens nothing, and stays available at any depth.
- A model the account is not entitled to now falls back within its tier, the
  same way an unrecognised name does — both are availability problems, and
  dropping a tier is the answer to neither.
- A model whose own safeguard fires on a message is retried once on a different
  model. Verified live: Fable 5 refused a protein-ligand binding-affinity
  question about a real repository with *"our intentionally broad safeguards
  ... can sometimes flag legitimate coding, cybersecurity, and biology tasks"*
  and *"change your model"*. That is a property of the model, not a judgement
  on the request, and the remedy is the one the error names — so the deep tier
  now carries `opus` and then `sonnet` behind `fable`, and each retry uses a
  genuinely different model rather than the same one under an alias — the same
  broad filter can flag the second model as readily as the first, so the tier
  is walked rather than tried once. Bounded by `OMS_MODEL_SAFEGUARD_RETRIES`
  (default 2), and when every model in the tier flags the message the run says
  so instead of cycling. The artifact records each step as
  `model-fallback: reason=model-safeguard selected=<model>`. A considered
  refusal is a different message and is still never retried.
- A request the provider declines is reported as a decline rather than a
  generic failure: the artifact and the caller both say which provider and
  model declined it. It is deliberately not re-sent to another model, and a
  delegate spends no repair round on it. A decline is the provider's decision
  about the request; machinery that keeps asking a different model until one
  answers would be a way around that decision no matter what any individual
  request happens to be.
- Effort is chosen from the task and then fitted to the model, instead of a
  fixed low/medium/high map. Work that gates something irreversible — an
  advisor pass, a release, a review gate — takes one step more thinking,
  because that answer is acted on rather than retried; everything else keeps
  its tier's ordinary effort, since spending the top of a scale on routine work
  is how a budget disappears without anything improving. The result is then
  clamped to the scale the chosen model actually publishes, so the same release
  gate runs at `xhigh` on codex and claude and comes back to `high` on
  antigravity, whose scale stops there. `OMS_REASONING_NO_HEADROOM=1` turns the
  escalation off.
- A claude model name that stops resolving is recovered from instead of failing
  the tier. Claude Code publishes no catalog, so a pinned name cannot be checked
  before use — but it rejects an unknown one locally in about two seconds. Each
  claude tier now carries the pinned name first, because that is what makes an
  artifact say which model actually ran, and the CLI's own public alias
  (`fable`, `sonnet`, `haiku`) as the same-tier alternative. Dropping a tier is
  the answer to a busy model, not to a name the provider does not have.
- `provider-permissions` turns the last human-only setup step into a harness
  tool. Codex and Claude Code take authority per invocation and need nothing;
  antigravity needs standing rules, and finding out which ones took a session
  of probing. It now reports them (`--check`, which is what `doctor` runs),
  emits them (`--print`), or grants them (`--apply`, keeping the previous
  settings as `.bak`), in a `consult` or `delegate` profile. `unsandboxed` is
  granted one named command at a time and a wildcard is refused: that namespace
  *is* the sandbox boundary. A cold `uv run` needs `unsandboxed(uv)` only
  because uv's cache lives outside the worktree — a warm one, and every test
  the worker runs inside its worktree, needs nothing.
  `--allow-toolchains` does the same for the package managers in a short
  built-in list that are actually installed on the machine, so a second machine
  reaches the same state without rediscovering it one denial at a time. `pip`
  and `conda` (this setup uses uv), `docker` (its socket is root-equivalent),
  and `make`, `node`, `git` (they write in-tree) stay out of that list even when
  present.
- The README and `oms help` now say plainly what was only implied: install is
  the one command a person types, and everything after it is the agent's.

- The advisor now fires on its own after repeated failure. The rules told every
  agent to consult one "after repeated failures" and nothing ever did — this
  session is the evidence: an agent with those rules loaded pushed a dozen
  gates, hit the same class of mistake twice, and never once reached for
  `oms advise` unprompted. A line in a rules file competes with everything else
  in the context; a code path does not. `peer-delegate` now consults an advisor
  when a repair round has itself failed, injects the verdict into the next
  attempt as evidence rather than instruction, and records it in the artifact.
  Once per delegation, never on a first failure, and an unreachable advisor
  leaves the repair exactly as it was. `OMS_ADVISE_ON_REPEAT=0` opts out.

- A second audit council was asked where the harness is incomplete *and* where
  it is over-built. Three integrity holes it found are fixed: the worker guard
  compared `git status` categories, so a worker rewriting a file the user had
  already modified stayed invisible (it now hashes the diff bytes); thread
  appends read the max sequence and wrote it back without a lock, so parallel
  panel answers could claim the same number (now serialized, and the current
  pointer is bound to the task that opened it, so a new task cannot inherit an
  unrelated conversation); and landing now refuses to apply when the intent
  cannot be recorded, while `--recover` requires the patch to still hash to what
  the intent recorded and hands a changed one to a human instead of guessing.
- Acting on the same council's over-building findings, three things were cut
  back rather than grown: guard surfaces that cannot be attributed (untracked
  and ignored files, tracked content) now report instead of failing the run,
  because a user editing their own repo during a delegation looks identical to
  a worker doing it (`OMS_WORKER_GUARD_STRICT=1` restores hard failure);
  councils refuse an expansion above `OMS_COUNCIL_MAX_CALLS` (default 12) rather
  than quietly making 27 provider calls; and answer classification no longer
  treats brevity as a defect — only an empty body or a reply that is nothing but
  questions counts as a non-answer, since rejecting a concise correct answer
  spent another provider call for nothing.
- Shared memory has a declared write contract instead of an accidental one:
  `shared.md` and `pins.md` are append-only (a correction is another appended
  note), `summary.md` is derived and regenerated by `compact`, and the
  worker-authority guard now holds the two logs to that contract. Editing a note
  another agent wrote — the way a worker would rewrite what the next session
  reads as fact — is reported as `memory/shared.md had existing rows rewritten`.
  A regression test pins the writers to append-only so a later refactor cannot
  silently break what the guard depends on.
- Shared `.oms` state is checked by its contract instead of being trusted
  wholesale. Workers are handed `OMS_STATE_REPO` so they can append, so the
  guard allows growth but records each JSONL family's length and the hash of
  exactly those bytes: rewriting rows already there, truncating a ledger, or
  deleting a state file fails the run and names the file and what happened
  ("failures.jsonl was truncated (42 -> 3 bytes)"). This closes the last gap the
  live council raised — erasing the failure that blocks a retry, or forging a
  thread turn another agent reads as evidence, was previously invisible because
  `.oms` was excluded entirely.
- Worker-authority detection covers what a live four-model council said it
  missed: untracked and ignored content is compared by stat, so planting
  `sitecustomize.py` or swapping a file inside an ignored `.venv/` is caught
  (`git status --ignored` collapses a fully ignored directory to one entry and
  sees neither), and object-store wiring is compared too — alternates,
  `info/exclude`, grafts, shallow, linked-worktree registrations, and submodule
  config/HEAD under `.git/modules`. The stat scan is bounded by
  `OMS_WORKER_GUARD_MAX_FILES` (default 20000) and says when it truncated. The
  limits are now documented rather than implied: a reverted write, anything the
  worker reads, and anything outside the repository need process isolation a
  bash harness does not have.
- The verification gate answers at feature level, not per line of chatter:
  `scripts/check.sh` prints one `ok: <stage>` per suite and the smoke runner one
  `scripts-smoke: ok (N tests)` per shard, so a green run is ~25 lines instead of
  a few thousand. A failing stage still prints its full output, and
  `OMS_VERBOSE=1` restores everything — the detail is evidence for a failure, not
  something an agent should read (and pay for) on every pass.
- Panels reach the canonical councils: `peer-ask` and `peer-review` take
  `--tiers fast,balanced,deep` and accept a tier inside `--providers`
  (`codex:deep`), so one council can span providers and model tiers. Each target
  writes its own tier-labelled artifact, the same target twice is still refused
  (the `agy` alias included), and both tools now report how many independent
  model families answered — with a warning when they all share one, because
  agreement inside a family is replication, not corroboration. Target parsing
  and family accounting are shared with `oms consult` so the notation cannot
  drift.
- Worker authority is checked mechanically, not only stated in the brief.
  `peer-delegate` fingerprints the surfaces a worker must not touch — the primary
  worktree's tracked state, local git config, remotes, refs, and hooks — before
  and after every worker round. A change fails the run, names the surface, keeps
  the worktree as evidence, records the violation in the fail-ledger, and fails
  the executor: patch scope only ever inspected the patch, so a worker writing
  around it was invisible. This is detection, not a sandbox
  (`OMS_WORKER_GUARD_OFF=1` opts out).
- `oms consult` targets can name a tier or an exact model and may repeat a
  provider, so a panel is several models rather than one per CLI:
  `--to codex:deep --to codex:balanced --to claude:model=claude-opus-5` records
  each answer separately, and `--all --tiers deep,balanced` spans providers and
  tiers. Results are reported as "N targets answered, K independent model
  families", and a panel whose answers all share a family says so — repetition
  within one family is not corroboration.
- Worker tiers follow the role before the phase: a `decision-advisor` invoked
  under a `verify` phase resolved to the cheapest model, which is the wrong
  model for the most consequential read. Precedence is now explicit class >
  role > phase > default, role files can declare their own tier
  (`oms-model-class: deep`), `agent-call`/`agent-run` take `--operation NAME` so
  a caller declares the work phase instead of inheriting the calling script's
  label, and the route line records which rule decided
  (`class=deep (role)`).
- Patch landing is now a recoverable transaction: `patch-land` writes an intent
  row to `.oms/landings.jsonl` (patch hash, base sha, task, lease) before the
  irreversible apply and a completion row after lineage and plan finish, so a
  crash in between is visible instead of looking like nothing happened. `oms
  state` lists interrupted landings, and `patch-land --recover` decides from the
  tree whether the patch went in, records the lineage and plan completion the
  crash skipped (or releases the task and marks it abandoned), and is idempotent
  — it never applies anything itself.
- `oms run validate` grew a family registry: beyond parsing and schema drift it
  now checks the fields each reader indexes on (`run_id`, `landing_id`,
  `fingerprint`, thread `seq`/`role`, …), closed lifecycle sets (landing events,
  board status, thread roles), plan task states, and plan dependencies pointing
  at tasks that do not exist — the failure mode where valid JSON silently
  vanishes from derived views.
- Antigravity routing moves to the current Flash generation (`Gemini 3.6 Flash
  (Low)`/`(Medium)` for fast/balanced; deep stays `Gemini 3.1 Pro (High)`, the
  strongest reasoning model the CLI exposes).
- Verification that cannot go stale unnoticed: `agent-task verify` now binds a
  pass to a content-free fingerprint of the tree it passed on and to the
  contract that ran, `status` reports `verification: fresh|stale|none` (also in
  `--json`), and `close` refuses a packet whose declared verification is missing
  or stale unless `--reason TEXT` records why — closing promotes "Closed task"
  into shared memory, so an unverified close hands the next session a false
  green.
- `oms state` marks CI recorded for a different commit as STALE with the
  current HEAD and the refresh command (`ci.fresh`/`ci.current_sha` in `--json`),
  instead of printing an old green conclusion with no indication it is old.
- Cross-agent answers are checked mechanically before they count: `ma_answer_quality`
  classifies a provider reply as ok, thin, or empty (a CLI can exit 0 and return
  a banner or a clarifying question), threads label non-ok turns, `oms consult`
  falls back to one other peer when an automatically picked one fails or does
  not answer (never for a pinned `--to`, never after an outbound-gate block),
  and `--all` reports how many peers actually answered.
- `skill-doctor` rightsizes instruction files: it flags a `SKILL.md` over the
  load budget (`OMS_SKILL_WORDS`, default 900) and references/ that no SKILL.md
  links, so skills stay short routers into detail read only when relevant.
  Generated `PROJECT.md` now leads its notes with gotchas — the non-obvious
  decisions an agent would otherwise get wrong — rather than restating what the
  repo already shows.
- Cross-agent conversations (`agent-thread.sh`, `oms thread`) and one verb to
  use them (`agent-consult.sh`, `oms consult`). Every cross-agent call used to
  be one-shot — the peer answered into an artifact and the next call started
  from nothing — so agents could not exchange context. A thread is an
  append-only transcript in `.oms/threads/<id>.jsonl` that any provider reads
  and extends: `oms consult "question"` picks a peer that is not the caller,
  attaches the active task, shared memory, and the conversation so far, records
  both turns, and prints the answer; `--all` asks every installed peer in
  parallel into one thread. `--thread ID` also works on `agent-call`,
  `agent-run`, `peer-ask`, `peer-delegate`, and `advise`, so council answers,
  delegated patch outcomes, and advisor verdicts land in the same conversation.
  Turns are budget-truncated and pass the shared sensitive-content gate before
  they can be replayed into another provider's prompt. `oms state` lists open
  threads and `oms gc` sweeps only closed ones.
- Machine-readable views for the remaining state tools: `agent-task status
  --json`, `artifact-index list|latest|failures|unresolved --json`,
  `run-ledger list --json`, `run-capsule list --json`, `data-manifest list
  --json`, and `session-handoff list --json`, all schema-1 and empty-state safe.
- `oms state` and `oms init` now cover the project harness itself, not only
  `.oms` state: applied rule styles, `PROJECT.md` state, and any agent file
  visible to git, with hand-written agent rules reported as `unmanaged` rather
  than prompting for a template. `oms init` applies the git exclusion when a
  repo got `git init` after its template (the order the bootstrap flow uses,
  where the template step cannot), reports missing or half-applied rules ahead
  of task/plan advice, and takes `--no-private`. `remove-project-template`
  points at the exclusion it leaves behind.
- Local-only project harness (`project-private.sh`, `oms project-private`): the
  agent-facing files (`AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `PROJECT.md`, plus
  `--path` extras) are listed in a managed block in `.git/info/exclude`, so a
  public repo carries no harness residue and the project's own `.gitignore`
  stays untouched. `apply-project-template` applies it by default
  (`--no-private` / `OH_MY_SETTING_PRIVATE_AGENT_FILES=0` opts out),
  `project-doctor` warns when an agent file is visible to git, `oms init`
  points at the fix, and delegate/admission worktrees seed the local-only
  copies so workers still read the project rules without those copies reaching
  a patch. An already-tracked agent file is reported, never silently untracked
  (`apply --untrack` stages that explicitly).
- Machine-readable views for shared state: `fail-ledger list --json`,
  `experiment-board list --json`, and `oms-run timeline --json` emit schema-1
  JSON objects, and `repo-state --json` now declares `"schema": 1`.
- The `oms` dispatcher prints a `--help` recovery hint whenever a tool exits
  with the misuse convention (exit 2), and `oms list` descriptions are joined
  to the full first sentence of each tool's header instead of a truncated
  first line.
- Tool discoverability for agents: `agent-harness` and the global rules point
  at `oms list`, `.oms` state files are documented as read-only-by-hand, and
  the skill router gains English resume triggers plus generic data-leakage
  triggers routing to `research-method` (with an `oms data-manifest` pointer
  in its skill).
- Snapshot lifecycle contracts: machine and Slurm generators now support
  `--dry-run`/`--check`, write private atomic schema-marked files, and report
  broader local hardware context. Schema-2 receipts preserve `0`/`1`/`auto`
  snapshot policy, and transactional updates refresh applicable snapshots.
- `project-doctor --strict` for CI/release gates, including filled Slurm
  partition/account, resource, log, and checkpoint requirements past draft.
- Explicit `tsp-queue enqueue --allow-noqueue` opt-in for degraded background
  execution when task-spooler is unavailable.
- Slurm cluster references now capture all partition and node records, the
  current user's `sacctmgr` associations/accounts, QOS limits, and effective
  configured CPU, memory, and time defaults without inventing missing values.
- Automatic reasoning-effort routing alongside worker model classes:
  `fast` uses low, `balanced` medium, and `deep` high effort. Codex receives a
  `model_reasoning_effort` override, Claude receives `--effort`, and
  Antigravity uses Low/Medium/High model variants because its CLI has no
  independent effort flag. Capacity fallback lowers automatic effort with the
  model tier; executor and artifact metadata freeze and record the route.

- Provider-neutral worker model routing for `agent-call`, `agent-run`,
  `peer-ask`, `peer-review`, `peer-delegate`, `plan-run`, and `advise`.
  `fast`/`balanced`/`deep` classes map to each installed CLI, roles and
  operations select a class automatically, and capacity errors permit at most
  one lower-class retry. Exact models, mappings, and fallback policy remain
  overridable; write fallback is blocked after any worktree mutation. Artifact
  rows and delegation liveness retain the resolved route, while executor souls
  freeze their model contract with provider/scope/verification metadata.
- Bounded autonomous plan execution (`oms plan-run`): atomically claims one
  scoped task, delegates it in an isolated worktree with bounded repair, and
  stops in review unless `--land` explicitly sends it through `patch-land`.
  Machine-readable plan claims, known-failure gating, signal-safe lease release,
  and focused autonomy regressions keep the controller composable and fail-closed.
- Active task verification now executes and records the mechanical command;
  skipped or failed checks remain non-green. Plan tasks can reach `done` only
  through reviewed artifact/patch evidence and the fenced landing transition.
- Failure memory includes a content-free git-state fingerprint, allowing a
  justified retry after tracked changes while blocking an unchanged dead end.
- Prompt routing uses ASCII token boundaries for short English terms and rotates
  auto-recorded task packets when a genuinely different explicit goal arrives.
- Source installation refs: `--ref` and `OH_MY_SETTING_REF` can select `edge`,
  a tag, branch, or commit. The source installer follows the origin default
  branch in edge mode and pins exact refs to detached commits.
- Transactional updates: schema-2 receipts persist the install ref, profile,
  concrete components, managed targets, and previous successful commit.
  `update --check` is read-only, link/doctor failures restore HEAD, links, and
  receipt, and `update --rollback` returns to the prior success.
- Task-scoped executor souls (`agent-executor.sh`): model-proposed behavior is
  validated and hash-frozen while machine-owned metadata retains provider,
  task lease, base commit, path scope, and verification authority. Native and
  cross-CLI executors share the same brief; repair preserves the frozen soul.
- Patch admission now enforces plan/executor allowed and forbidden paths before
  verification, with deny precedence; artifact and liveness rows carry
  executor ID and soul hash provenance.
- `advise.sh` (`oms advise`): agent-agnostic advisor pass at decision points
  (before irreversible/high-risk decisions, after repeated failures, or at a
  release go/no-go). Composes an adversarial VERDICT/RISKS/MISSING/NEXT prompt, attaches
  unresolved fail-ledger rows, defaults to the first available provider that
  is not the caller (`OMS_ADVISOR_PROVIDER`/`--to` to pin), and routes through
  `agent-call.sh` (read-only, scrubbed). Gives Codex and Antigravity the same
  decision-point advisor Claude Code has natively.

- Skill router (`skill-router.sh` + `install-claude-hooks.sh`): a Claude Code
  UserPromptSubmit hook that matches each prompt against new per-skill
  `triggers` phrases (en+ko) in `skills.manifest.json` and injects a one-line
  skill hint, so skills fire at task time instead of relying on recall.
  Precision-first: max 2 suggestions per prompt, each skill hinted once per
  session, silent on no match, system prompts (slash commands, notifications)
  skipped, fail-open, `OMS_SKILL_ROUTER_OFF=1` kill switch. install/update
  register it via an additive `~/.claude/settings.json` merge (backup +
  idempotent + refuses invalid JSON; `OH_MY_SETTING_CLAUDE_HOOKS=0` opts out)
  and uninstall removes only its own entry. Claude-only by nature; Codex and
  Antigravity keep the skill picker plus a new AGENTS.md skill-consult rule.
- Terminal-verb wiring — every create now has a crash-path close: `gc` appends
  a close event to stale open runs (no spine event in `--days`; open runs no
  longer protect their capsules from GC forever), releases the claimed/running
  plan task coupled to a dead delegation marker (the two records describing
  the same dead worker are finally joined), and sweeps abandoned change-guards.
- `patch-land.sh` feeds the shared failure memory: a rejection is recorded in
  the fail-ledger fingerprinted by patch content, a retry of a known-rejected
  patch warns first, and a later successful land resolves the entry. The land
  row append is no longer silently swallowed, and `--plan-task` alone reads
  the patch path the plan task already stores.
- `agent-plan reclaim --include-review`: opts an abandoned review back to
  ready on its own clock (`updated`, default TTL 86400s), keeping
  artifact/patch; `oms state` flags stale reviews (`OMS_PLAN_REVIEW_TTL`).
- `change-guard.sh` liveness: snapshots stamp a start time (optional
  `OMS_GUARD_PID`), `status` and `oms state` flag an abandoned guard STALE
  (`OMS_GUARD_TTL`), and the snapshot write is atomic.
- `repo-state.sh --refresh-ci`: opt-in `ci-status record` before reading, so
  the CI section reflects the latest run in one command.
- CI `install-e2e` job: the real `install.sh` → `update.sh` → `uninstall.sh`
  lifecycle against a throwaway HOME — the installer path was previously only
  linted, never executed.

- Failure memory (`fail-ledger.sh`): durable `.oms/failures.jsonl` fingerprint
  ledger so the three agents stop repeating the same failing command across
  sessions — `record`/`check` (exit 3 on a known-unresolved failure)/`resolve`/
  `list`; sensitive commands refused. Surfaced in `oms state`.
- Delegation liveness: `multi-agent-delegate` writes `.oms/delegations/<id>.json`
  while a worker is in flight and removes it on exit; `oms state` shows live
  workers and flags dead-pid orphans (no daemon — the launcher is the writer).
- `ci-status.sh record`: appends the latest CI conclusion to `.oms/ci.jsonl`
  (deduped by sha); `oms state` shows the latest conclusion for HEAD's branch.
- `oms init` (`oms-init.sh`): seeds the `.oms/` skeleton + `.gitignore`
  (idempotent) and prints a next-actions checklist tailored to the detected
  project type — a first move for an agent landing in a fresh repo.
- `oms gc` (`gc.sh`): `--dry-run` by default; reclaims aged transient state
  (orphaned delegation markers, archived task packets, capsules of non-open
  runs, resolved failure rows) and delegates artifacts to `artifact-index
  prune`; never touches open runs, the active task, unresolved failures, or the
  append-only board.
- `oms-run validate` now walks every `.oms/**/*.jsonl` family and flags schema
  drift (rows below a family's expected schema) — the one place a future schema
  bump is signalled — in addition to the parse check.

- Role profiles (`agent-role.sh`): named, reusable worker personas as markdown
  in `.oms/roles/<name>.md` (global fallback `~/.oh-my-setting/local/roles`);
  `list`/`show`/`resolve`/`init`. `multi-agent-delegate.sh --role NAME` prepends
  the profile to the worker brief, and an `agent-plan` task's new `role` field is
  auto-injected when delegated via `--plan-task` — so the same reviewer /
  refactorer / test-writer role can drive any of the three providers.

- `repo-state.sh` (`oms state`): one read-only dashboard over all shared `.oms`
  state — active task goal/next, plan tasks by state with stale claims flagged,
  experiment board active/stale, current + open runs, latest artifact rows, and
  change-guard status; `--json` for machines. Answers "what is active, stale,
  or open here?" in one command instead of cat-ing five files.
- `patch-land.sh` (`oms patch-land`): the one mutating step that composes the
  trust boundary — clean-tree check → `patch-admit` ADMIT gate → `git apply` →
  land row in the artifact index → optional `--plan-task` finish. Nothing lands
  unless admission passes and the tree is clean.
- Claim heartbeat: `agent-plan.sh touch --id` and `experiment-board.sh touch
  --id` refresh a live claim's timestamp so a still-running worker is not
  reclaimed / flagged stale mid-run (the reclaim/stale TTL clock restarts).
- `scripts/oms` dispatcher, symlinked to `~/.local/bin/oms`: `oms <tool>`
  invokes any harness script by name from any of the three agent CLIs
  (`run` aliases `oms-run`); `oms list` prints every tool with its one-line
  purpose. Linked/unlinked/doctored with the install.
- `oms-run.sh new` writes a repo-scoped `.oms/runs/CURRENT` pointer and
  `oms-run.sh current` resolves the effective run id; `link` and the
  run-ledger/run-capsule/experiment-board auto-links fall back to a fresh
  CURRENT when `OMS_RUN_ID` is unset, so a second agent process joins the
  active run without env plumbing. Stale pointers expire
  (`OMS_RUN_CURRENT_TTL`, default 86400 s) instead of misjoining.
- Agent identity: `oms_detect_agent` (explicit `OMS_AGENT` > CLI env markers >
  generic "agent") now attributes memory notes, task bullets, board claims,
  capsules, and reconcile rows; spine link rows carry a new `agent` field
  (`link --agent` overrides; `show`/`timeline` display it).
- Delegate workers receive `OMS_STATE_REPO` — agent-memory/task/plan resolve
  to the primary repo's shared `.oms` instead of the empty throwaway
  worktree — and `OMS_AGENT=<provider>` for attribution.
- `agent-run.sh --task-id`/`--plan-task`, forwarded to the delegate for plan
  lineage and lifecycle coupling.
- AGENTS.md "Run Provenance & Coordination" is now a capability catalog with
  the `oms` invocation path; the agent-harness skill documents the plan DAG.

- `agent-plan.sh`: shared subtask DAG (`.oms/plan/tasks.json`) with per-task
  dependencies, path scope, and verify command; `ready`/`status` compute what is
  actionable now so work can be split across agents without collisions.
  `next`/`brief` emit a paste-able work brief; `next --claim --provider` is a
  pull-work primitive (exit 3 when nothing is actionable). All mutations and
  `next --claim` run under a file lock so concurrent agents cannot both claim the
  same task; adds `review`/`release` commands, a stricter lifecycle (finish only
  from claimed/running/review; a blocked task must be reopened before claim), and
  a `claimed_at` timestamp.
- `multi-agent-delegate.sh --task-id` and artifact-index lineage: every index
  row now records `base_sha` and any `task_id`, surfaced in `artifact-index list`,
  so a run traces back to the plan subtask and commit it came from.
- `change-guard.sh`: `forbidden_paths` task constraint (deny beats allow),
  documented in `agent-task.sh` help.
- `data-manifest.sh`: `--key-column` entity-overlap leakage (inchikey/scaffold/
  cluster/assay) and per-key fingerprints with `(id -> key)` mapping drift and
  empty-key counts (manifest schema 3).
- `run-ledger.sh`: each row records its gate decision (passed/skipped/recorded/
  none); `list` surfaces it.
- `project-doctor.sh`: flags an empty `## Commands`/`## Verification` once
  `PROJECT.md` is past draft.
- `LICENSE` (MIT), `SECURITY.md`, `CONTRIBUTING.md`, this changelog.
- `oms-run.sh close [id]` + `ls --open`: mark a run terminal and list
  open-vs-closed runs; close also clears a `CURRENT` pointer naming the run so
  later tool events stop auto-joining a finished run.
- `oms-run.sh timeline --agent NAME` / `--tool NAME`: filter the merged
  cross-stream timeline by who or which tool (case-insensitive substring).
- `experiment-board.sh list --stale` / `--owner NAME`: surface TTL-expired
  (reclaimable) claims and filter by claimer, instead of staleness only
  showing up at the next claim collision.
- `agent-memory.sh search PATTERN` (`--agent` author filter): recall over
  shared memory and pins by entry, replacing `show` cat-ing the whole file.
- `multi-agent-delegate.sh --plan-task ID` without `--prompt`/`--brief-file`
  hydrates the worker brief from the task, and without `--verify` uses the
  task's stored verify command — `delegate --to codex --plan-task t3` is now
  a complete one-liner.


### Changed
- Every install now exposes one five-skill, general-purpose catalog. Added a
  compact, language-neutral `trust-boundary` method for security-sensitive
  changes and threat models. Removed
  the project- or machine-specific `chem-bio-ml`, `ml-training`,
  `research-method`, `slurm-hpc`, and `tsp-queue` skills; their useful runtime
  commands and project templates remain available without occupying global
  skill context. Folded the `peer-ask`, `peer-review`, and `peer-delegate` skill
  front doors into `agent-harness` while retaining their authority-specific
  `oms` commands, and moved the private Slurm snapshot to `local/slurm.md`.

- The install brings the CLIs the harness coordinates instead of hoping they are
  already there: Claude Code, Codex, Antigravity, Node via nvm, uv, and now `gh`.
  Provider installation had been behind `--tools`, so the documented one-line
  install produced a harness whose council had one voice and whose
  `github-source`/`ci-status` had nothing to call; `doctor` then reported the
  missing peers as optional, which is true of the flag but not of the product.
  `OH_MY_SETTING_INSTALL_TOOLS` now defaults to 1, `--no-tools` is the explicit
  escape hatch for a machine that cannot have them, and `gh` is checked like the
  provider CLIs rather than as an optional extra. `gh` is installed without root
  — brew on macOS, otherwise the official release archive into `~/.local/bin`,
  reusing the one helper that owns that directory and persists it on PATH — with
  the release tag read from the API and a pinned fallback so a rate-limited API
  is not a failed install. Authentication stays a report, never an attempt: it is
  an interactive browser flow, and an unauthenticated `gh` is exactly the state
  where those two tools fail on their first call, so `doctor` warns and names
  `gh auth login`. That warning reads the credential locally — `hosts.yml`, or
  the token variables — rather than running `gh auth status`, which validates
  against the API: this doctor is local-only by design, and a health check that
  needs the network reports "unauthenticated" for a dropped connection. Because
  credentials live under `HOME`, the doctor fixtures now fabricate one the same
  way they already fabricate healthy Antigravity permissions, so a clean install
  is not reported as warning about something it cannot fix.

- `docs/COMPONENTS.md` is grouped by area instead of being one 56-row table. The
  Area column claimed a grouping the rows did not have — `Agent state` appeared
  in two separate blocks, as did `Peer agents` and `Maintenance` — so a reader
  who found one block had reason to believe they had seen the area. The widest
  cell was 3,695 characters, which no table renders readably, and prose that
  long had been damaged by successive insertions: the delegate entry's own main
  clause survived only as "`OMS_WORKER_GUARD_OFF=1` opts out, verifies it there"
  after the guard paragraph was spliced through the middle of it. Entries are now
  wrapped paragraphs under six area headings, the four that carried two subjects
  each were split so both halves are findable by name (delegation / repair rounds
  / worker-authority guard / depth cap, and capability snapshot / call-time model
  fallback), and the agy permission-namespace explanation moved out of `consult`
  into the entry about permissions, which had been restating its conclusion
  without the reasoning. Every original cell is preserved verbatim except those
  four; all 301 backticked identifiers survive.
- `ml-training` routes by symptom instead of by implementation subject, and the
  two peer skills that write now name the read-only alternative. The training
  index was headed optimizer / distributed / loss-masking / checkpoint /
  equivariance, so "resume does not reproduce" or "loss differs by rank" had to
  be translated into a subject first, and a wrong translation costs a debugging
  session in the wrong layer. `peer-delegate` and `peer-review` never mentioned
  each other or `consult`, so an agent that needed an opinion could land on the
  tool that edits a worktree. Skills whose references are modes or phases of one
  action — `peer-review`, `research-method`, `spec-interview` — were left alone,
  and `chem-bio-ml` already indexes by problem domain rather than by internal
  structure.
- The harness skill routes by what the agent is about to do, not by which
  subsystem a capability belongs to. Its index used to be split the way the code
  is — state-memory, plans-recovery, roles-executors, delegation-artifacts — so
  an agent had to map its own situation onto the harness's internal taxonomy
  before it could find anything, and the clusters that overlap in purpose
  (`agent-call`, `consult`, `peer-ask`, `advise` are all "ask another model
  read-only") looked like four unrelated choices. The entry is now a table of
  situations, and every row carries the authority it takes — read, worktree
  write, or repo write — because those clusters differ in blast radius, not just
  in shape, and a routing table that hides that would invite exactly the wrong
  kind of mistake. The references keep the detail and are all still linked.

- Removed the redundant `git-cli-workflow` custom skill; global policy already
  owns its complete local git/gh safety contract, and relinking removes stale
  owned skill links from all three providers.
- Converted `agent-harness` and `ml-training` into compact task routers with
  one-level references. Corrected ML guidance for semantic optimizer grouping,
  short schedules, unequal-count DDP gradients, portable DDP checkpoints, and
  opt-in static graphs.
- Minimal install is now the default: provider tools, `.bashrc` mutation,
  machine snapshots, auto-update timers, and the star prompt require explicit
  flags or `--full`; Codex plugin setup auto-detects an existing CLI. Standalone
  doctor runs treat provider CLIs and an uninstalled Codex plugin as optional
  unless strict checks are explicitly requested.
- New ML scaffolds create five core docs by default; `--full-docs` retains the
  complete 13-document scaffold and existing project files are never removed.
- Routine `status.sh` reports local paths without launching provider CLIs;
  `--verbose` opts into version and Codex plugin probes.
- Expanded shell validation to shared libraries, plugin hooks, and generated
  project checks; doctor now reports supported Bash 3.2 as healthy.
- Reduced routine harness overhead: prompt hooks no longer create `.oms` or
  active task packets for read-only questions, automatic task recording now
  requires `OMS_AUTO_TASK=1`, `update.sh` refreshes provider tools only with
  `--tools`, and the `oms list` public allowlist now also enforces dispatch so
  hidden install and hook scripts cannot be invoked by guessed filenames.
- Narrowed multi-agent review activation to explicit cross-agent review,
  release gates, and requested ML pre-training gates. Generated project loaders
  now allow clear bounded changes without waiting on unrelated draft choices.
- Reduced global `rules/global-AGENTS.md` from a harness manual to a compact policy layer:
  prompt-level provider/model ladders and routine advisor calls are gone;
  bounded model selection now lives in executable harness policy, ambiguous
  work alone triggers the spec gate, and detailed
  coordination routes through the `agent-harness` skill while parent judgment
  and executor scope fences stay.
- Split install-wide rules from the repository `AGENTS.md` overlay so working on
  oh-my-setting no longer injects the same global policy twice.
- Removed unused legacy prompt/template placeholders, consolidated duplicate
  plugin hook wrappers, and removed standalone workflow files in favor of
  their maintained skills.
- Renamed the cross-CLI tool family `multi-agent-*` to `peer-*` to stop
  colliding with generic in-app multi-agent features: `peer-ask.sh`,
  `peer-review.sh`, `peer-delegate.sh`, `lib/peer-common.sh`, skills
  `peer-ask`/`peer-review`/`peer-delegate`, and env vars `OMS_PEER_*`.
  Version 0.4 removes the deprecated `multi-agent-*.sh` shims; legacy
  `OMS_MULTI_AGENT_*` variables now fail explicitly with their `OMS_PEER_*`
  replacements instead of silently changing timeout behavior.

- MD/trigger layer strengthened so skills actually fire at task time: skill
  frontmatter descriptions now carry concrete "use when" phrases and Korean
  user wordings (agent-harness enumerates its whole surface — state/resume,
  fail-ledger, gc, patch-land, plan DAG, session handoff; git-cli, slurm-hpc,
  ops, research-method, delegate follow). multi-agent-delegate's body
  documents `--plan-task`/`--role`/`--repair`/`--no-verify` and routes the
  post-worker path through patch-admit/patch-land. AGENTS.md reframes `oms
  gc` as the crash-path recovery step and adds the new lifecycle levers
  (`oms state --refresh-ci`, `reclaim --include-review`, patch-land ↔
  fail-ledger, change-guard). Templates call tools via the `oms` dispatcher;
  README.ko.md mirrors the EN condensed structure; timeout env knobs
  documented in COMPONENTS.

- README "What's Inside" is now an eight-row capability table; the full
  per-script catalog moved to `docs/COMPONENTS.md` (no scripts removed). The
  task plan is grouped under "Agent state", not "Memory".
- Auto-update trigger defaults to **check-only** (records availability) instead
  of auto-applying; opt in with `OH_MY_SETTING_AUTO_UPDATE_MODE=apply`.


### Removed
- `scripts/backup.sh`, an orphan: not a public `oms` tool, undocumented,
  called by nothing but its own test, and superseded by update.sh's
  transactional snapshot/rollback of managed targets.
- Shared-state files are replaced with a same-directory rename. The
  task-packet and memory-summary writers staged their scratch under TMPDIR
  and `mv`'d it over the target; with TMPDIR on a different filesystem than
  the repo (tmpfs /tmp against an ext4 checkout — the ordinary case) that
  degrades to copy+unlink, and every concurrent reader — another agent
  session, the work-journal observer, repo-state — had a window where the
  file was empty or truncated. `agent-task status` answers `status: none`
  with exit 0 for exactly that window, the most plausible mechanism behind a
  load-dependent lifecycle-test flake (diagnosed with an advisor pass, whose
  capture-first assertion prescription is also in). gc now reclaims a
  crashed writer's `.oms-replace.*` scratch after an hour, and leaves fresh
  scratch to the writer that may still hold it.
- The default advisor and consult peers exclude the *detected* caller. Both
  pickers excluded `${OMS_AGENT:-}`, which interactive sessions never
  export, so an empty caller made the exclusion loop a no-op: a Claude
  session asking for an outside read got Claude — replication presented as
  independence, observed live when a flake advisory routed to the session's
  own family. One shared `oms_peer_caller` (OMS_AGENT when set, detected
  session identity otherwise, unknown excludes nothing) now feeds both, with
  `--to`/`OMS_ADVISOR_PROVIDER` overrides and the last-resort self-advice
  fallback unchanged; `tests/advisor-routing-smoke.sh` pins the contract as
  its own gate stage.
- Doctor counts unindexed artifacts with the same grace window `prune --files`
  deletes with. A live provider call writes its artifact before the index row
  lands, so the age-blind count warned about in-flight council runs and then
  prescribed a command that correctly deleted nothing — doctor complaining,
  the remedy shrugging, every time an ask/review was still executing. Prune
  also now says how many recent unindexed files it kept inside the grace
  window, so "deleted 0 orphan file(s)" explains itself.

- Three functions no dead-code sweep had caught, because nothing referenced them
  from anywhere including the tests: `oms_worker_surface_fingerprint` (one
  aggregate digest over every worker surface, superseded by the per-surface
  capture that lets a violation name what moved — the comment explaining that
  reason was left orphaned below it), `oms_capability_supports_effort`
  (superseded by `oms_capability_clamp_effort`, which answers the same question
  and maps onto the scale), and `ma_wait_stdin_file`.

- GitHub Release publication, release-only checksum tooling, documentation,
  and CI contracts. Installation now uses the repository source channel; exact
  tags, branches, or commits remain available through `--ref`.

- Deprecated `workflows/{spec-first,slurm-hpc,new-server}.md`, their global
  workflow link, and `multi-agent-{ask,review,delegate}.sh`. Upgrade cleanup
  restores the newest user workflow backup and preserves foreign targets.


### Fixed
- The smoke suite's source-state leak check no longer fires on the live
  session's own Work Journal writes. The `.oms` fingerprint excluded only
  `hooks/`, but the journal's turn boundaries (this cycle) rewrite
  `work-journal/index.json`/`index.sqlite3` on every prompt the agent running
  the suite submits — a run from an adopted repo failed with a fingerprint
  mismatch that had nothing to do with the change under test. `work-journal/`
  joins `hooks/` as an ambient-written subtree; tests exercise the journal
  only through temporary repos, so no coverage is lost.
- An existing project `GEMINI.md` is kept in sync with the other loaders.
  Antigravity reads it as a directory rule file and prefers it over
  `AGENTS.md`: verified live in a repository whose `AGENTS.md` carried the
  ml loader while a leftover `GEMINI.md` still carried general — agy
  reported the general template while Claude Code and Codex followed ml.
  One CLI silently obeying rules the other two retired is exactly what this
  project exists to prevent. The template now adopts `GEMINI.md` as a
  managed file when the project already has one (so the retired block is
  removed and the new loader written), and never creates one otherwise —
  agy reads `AGENTS.md` fine, so a new project needs no third loader.
- `GEMINI.md` stays a per-project harness file, and a test now pins it.
  An earlier entry in this same unreleased cycle dropped it from the
  default lists (project-private hiding, worktree seeding, template dedup)
  on the belief that no supported CLI reads a project `GEMINI.md`. That was
  wrong: Antigravity's own bundled rules documentation lists `GEMINI.md`,
  `AGENTS.md`, and `.agents/rules/*.md` as the directory-based rule paths it
  reads hierarchically. The practical risk was a project `GEMINI.md` no
  longer being hidden from git by default — agent context leaking into a
  public repo — and an agy worker in a delegate worktree losing project
  rules. Nothing pinned the contract, so the removal passed the gate;
  `tests/scripts-smoke.sh` now asserts the default harness-file set.
- Switching a project's base style now retires the previous one. Managed
  rule blocks are per-style, so applying `ml` over `general` left both in
  place and the agent read two loaders with conflicting rules — found by
  applying the ml template to a real research repo. Only the mutually
  exclusive base styles (general, ml) are replaced; the slurm overlay
  follows machine detection and is removed only by an explicit
  `remove-project-template.sh slurm`. The ml and slurm templates also point
  at `oms generate-slurm-reference` instead of the retired name.
- Session-handoff digests land in the project repo, not the harness checkout.
  The default output directory was the oh-my-setting checkout's own
  `.oms/handoffs/`, while the Work Journal's newest-handoff pointer scans the
  project's — every capture from another repo was invisible to the daily
  digest. `capture` now resolves the repo containing `--cwd`; `list`/`show`
  resolve `OMS_STATE_REPO`/`$PWD`.

- `skill-doctor` now checks the shared `~/.agents/skills` root and detects a
  duplicate name split across Codex's product-specific and shared overlays.
- `oms init` no longer misses exposed agent files when `grep -q` closes its
  status pipe after the first match. Under `pipefail`, the still-writing
  producer could receive SIGPIPE and invert the condition, so the late-`git
  init` recovery sometimes skipped `.git/info/exclude` only under CI timing.
  The status command now completes before its captured output is inspected.
- Peer-review mechanical verification now runs without the review provider's
  model-class, operation, or reasoning environment. A deep review gate could
  previously make an otherwise clean routing test observe the review phase and
  fail, even though the verifier was not itself a reviewer call.
- The residue scan sees flock lock files. It only ever walked mkdir-path lock
  directories, so a machine holding 12,427 flock lock files was told it had
  zero — which reads as "nothing here" rather than "this kind is not
  removable". They are permanent by design: unlinking a released flock file
  lets a waiter on the old inode and a new arrival on a fresh one both hold the
  lock. So doctor reports the count as a note, never a warning, and cleanup
  still leaves them alone. A large count means a run leaked its lock dir into a
  real HOME — a test without `OMS_LOCK_DIR` — not a broken install. The
  cleanup regression also builds its dead lock through the real acquire path
  now; the hand-made fixture is what let the gap hide.
- Provider-call accounting survives artifact retention. Duration and
  provider-reported tokens were readable only by parsing the artifact body, so
  deleting stale artifacts erased the record of what those calls had cost -
  pruning 360 leaked test artifacts this release took the parseable base down
  with them. The index row now caches both at write time, reusing the telemetry
  module rather than copying its regexes, and telemetry prefers the cached value
  while still parsing for rows written earlier. Artifact coverage still drops
  honestly when a file is gone; the numbers do not.
- Skill linking no longer lets quiet `grep` turn an enabled skill into a
  disabled one. The membership check used `printf | grep -q` under `pipefail`;
  when grep found an early match and closed, printf could receive EPIPE, invert
  the condition, and unlink that enabled skill. It surfaced as an intermittent
  doctor failure with `tsp-queue` missing only from Antigravity. The captured
  manifest is now CRLF-normalized once and passed to grep as a value, with a
  list larger than the pipe buffer pinning the behavior.
- The source-checkout leak guard names the paths that moved. It compared two
  opaque digests, so each time it fired it destroyed the evidence it had just
  detected — three occurrences in one day, none diagnosable afterwards. It now
  keeps a per-path manifest and prints the difference, verified by injecting a
  transient file and watching the guard report it by name.
- Entrypoints that write install state answer `--help` and refuse anything else.
  `link.sh` ignored its arguments and ran, so `link.sh --help` relinked a live
  install and moved canonical ownership to whichever checkout was asked for help
  — the receipt names an owner, so that is a transfer, not a no-op. It happened
  during this release's own analysis: 78 stray backup symlinks across the three
  agent skill roots, snapshot modes reset from `auto` to `0`, the rollback commit
  lost, and `doctor: failed`. `install-hooks.sh` had the same shape.
  `check-bash32.sh` exited 127 on `--help` because the file-argument form it
  gained this release treated the flag as a path, and `check-python.sh` and
  `skill-doctor.sh` had no usage at all. A regression pins the rule for all five.
- The Windows install could not copy a single skill. Windows Python writes text
  streams with CRLF, so every value bash reads back from a helper arrives with a
  trailing carriage return — command substitution strips the newline and leaves
  the CR. `link.sh` built `custom-skills/oh-my-setting-ops\r` from a manifest
  read and the copy failed on a source that was right there, reporting one
  unlabelled line naming a directory that plainly existed. Reproduced locally
  byte for byte, and the same defect was latent in every state word and path the
  install path reads back: a managed state of `current\r` matches no case branch,
  and `auto-update` runs from a timer where a CR would read as a foreign checkout
  and silently skip. Stripped at each point of consumption in the documented
  Windows lifecycle (`link`, `doctor`, `status`, `auto-update`, `oms`, and the
  install contract, including the managed-copy helper). The regression simulates
  a CRLF `python3`, so it fails on any platform rather than only on a Windows
  runner; verified to fail with the fix removed.
- The install lifecycle fixture compared paths that were the same directory
  spelled two ways. macOS `TMPDIR` ends in a slash, so the mktemp template left
  `//` in every derived path while the installer recorded `pwd -P` output with
  none; Git Bash reaches one directory both through its POSIX mount and through
  the drive-letter form. Both spellings failed ownership comparison. `HOME` and
  the checkout are now resolved once, the way the installer resolves them. GNU
  mktemp collapses the `//` and BSD mktemp does not, so this cannot be reproduced
  on Linux — the fixture asserts its own paths are normalized rather than letting
  the cause surface three assertions later as "managed target mismatch".
- `managed-target.py` failures name the operation and what was wrong with the
  path. `FileNotFoundError(path)` prints as nothing but the path, which is
  exactly how the first Windows failure arrived: unreadable.
- The gate split is a flag, not an environment variable. `OMS_CHECK_LINT=0 bash
  scripts/check.sh` exports into every descendant, so CI's smoke job leaked it
  into the suite it was running: one test asked for a gate that runs nothing and
  was refused, and the missing-shellcheck test skipped the check it exists for
  and recursed through every suite instead of failing fast — breaking the "no
  recursion" promise in its own comment. `--lint-only`/`--no-lint` reach only the
  process they are passed to. The condition is reproducible locally by exporting
  the old variable, which is how the fix was confirmed.
- The delegation liveness marker is written atomically. It was written through a
  shell redirect, so the file was created and truncated before the writer ran —
  and this marker exists precisely so another process can read what was running
  after a delegate dies abruptly. Any reader arriving in that window got a JSON
  error on the one record of the crash. Found as a flaky gate under parallel
  shards, which is the same race with a wider window; a sweep for the pattern
  found no other published state file written this way.
- Bash 3.2 could not parse `scripts/lib/peer-common.sh`, so every peer tool was
  dead on macOS while Linux stayed green — and CI said so, in the one job that
  has since been reworked. The cause is narrow and now pinned: bash 3.2 cannot
  parse a here-document inside `$( )` whose body holds an odd number of
  apostrophes, even behind a quoted `<<'PY'` delimiter, because its command
  substitution scanner still reads the body looking for the closing paren, takes
  the lone quote as the start of a literal, and swallows the `)`. A prose
  "operator's" in an inline Python heredoc was enough. Confirmed against real
  bash 3.2 in all four directions (apostrophe inside `$( )` fails; balanced
  apostrophes pass; the same heredoc outside `$( )` passes; odd double quotes are
  irrelevant). `check-bash32.sh` now rejects the construct statically, so every
  push catches what previously only a macOS runner could, and the whole shipped
  file set is verified to parse under a real 3.2.
- Locks stopped being keyed to `XDG_CACHE_HOME`. The comment directly above the
  function has always said why — an interactive session (set) and a cron session
  (unset) compute different lock dirs for the same state file and both enter the
  critical section — and `auto-update` runs from a systemd user timer, so the
  asymmetry is not hypothetical. The motive was one hermetic `check.sh` run, which
  `OMS_LOCK_DIR` now provides without the production path depending on an ambient
  variable it does not control. The existing stability test covered only
  `XDG_RUNTIME_DIR`; it now covers both, and neutralizes the sanctioned override
  first so the assertion cannot pass trivially.
- `oms-run validate` reported a healthy run index as invalid. Two unrelated
  families are both named `index.jsonl` — `artifacts/index.jsonl` is read by
  `kind`, `runs/index.jsonl` is the run-capsule roll-up read by `id` — and the
  contract was keyed on the basename, so it demanded `kind` from every capsule
  row. Matched by directory now, the way threads already were. Found by running
  the restored BSD fixtures, not by the gate.
- The machine snapshot names the distribution and the CPU model again. The
  portable rewrite reduced OS to `Linux-6.8.0-x86_64-with-glibc2.39` — the kernel
  line a second time, distro gone — and CPU to `x86_64`, which is the answer to a
  question nobody asked of a hardware snapshot. `/etc/os-release` and
  `/proc/cpuinfo` (then `sysctl machdep.cpu.brand_string`) are read first and the
  portable form is the fallback; Windows support is unaffected. The regression
  injects both sources as fixtures, because asserting that a `- CPU:` label
  exists is what let the content vanish.
- `ma_answer_quality` no longer fails open. Extracting the classifier from an
  inline heredoc to `lib/answer-quality.py` created a failure mode that could not
  exist before: a missing file or a python error left the verdict empty and
  `${verdict:-ok}` counted every answer as real — including the provider refusal
  text this checker was written to catch after a council reported two independent
  families when one had spoken. An unrunnable checker is now named on stderr and
  reported `blocked`.
- An absent managed target reports `missing` instead of `foreign`. Calling a file
  that is not there "someone else's" was untrue, and answering it through the copy
  inspector cost a python3 process per probe (~25ms measured) — doctor and status
  probe every managed target on every run, most of them absent on a partial
  install.
- `install-tools` reads the `gh` credential locally instead of calling
  `gh auth status`, which contacts GitHub. Same reason the check was made local in
  `doctor`: a captive or offline network should not stall the tail of an install
  over a note.
- Reinstalling can repair the managed `python3` shim it created. If the
  interpreter behind the shim moved, the PATH probe failed and the installer then
  refused to touch the shim and exited — no reinstall could fix what an install
  had written. A shim matching the managed shape is replaced; anything else at
  that path is still the user's launcher and is left alone. The ownership test
  moved to `platform.sh`, which is all `install.sh` sources when it decides.
- `oms` validates the receipt it falls back to and says so when nothing resolves.
  A copy install has no link to follow, so the dispatcher reads `source_root`
  from the receipt; it now requires the same schema-2 shape as the rest of the
  harness, and a failure names the broken contract instead of reporting a missing
  file for whichever subcommand was asked for.

- `doctor` no longer calls every current memory database invalid. It asserted
  schema version 2 as a literal while the helper had moved to 3, so any
  repository that had used memory reported `warn: memory database is invalid`
  and was told to run `rebuild` — which writes version 3 again, leaving advice
  that could never clear the warning. The expected version is now read out of
  `lib/agent-memory-db.py`, the file that writes it, so the two cannot drift
  again. The existing tests could not catch this: the warning path is exercised
  with a file that is not a database at all, and the clean-state check never
  built one, so nothing ever compared the doctor against a database the tool had
  actually produced. A test now does exactly that. Noticed only by running
  `doctor` after an install update — the gate (`check.sh`) never looked.
- The smoke suite's source-leak check no longer fires on the agent session that
  is running it. It fingerprints `.oms/` at shard start to catch a test writing
  into the checkout, but `hooks/` is the one subtree the live session writes on
  its own schedule: `UserPromptSubmit` appends a `route` event and `Stop` appends
  a `turn_guard` event to `hooks/events.jsonl`. So an agent that runs the gate in
  the background and then answers the user fails its own gate with "smoke suite
  mutated the source checkout .oms state" — a guaranteed false alarm in ordinary
  operation, and the reason this was chased twice. The event was timestamped
  three seconds inside a shard while the checkout's state was provably unchanged
  at the end, which is what a false alarm looks like: the check cannot tell a
  leaking test from the agent it is protecting. `hooks/` is now excluded, verified both ways against a copy of
  real state — an appended hook event is ignored, a file dropped anywhere else
  is still caught.
- `agent-memory recall` no longer presents a resolved failure as an open one. A
  resolution is its own append-only row carrying nothing but the fingerprint it
  clears, and the index parser was written against guessed field names — it
  looked for an event called `resolve` and a key called `state`, while
  `fail-ledger.sh` writes `resolved` and `state_fingerprint`. The resolution row
  therefore had no body, was dropped as empty, and every fixed failure stayed in
  the index looking live; recalling a solved problem as an open one is worse than
  not recalling it. Resolution is now folded in over two passes, a later failure
  on the same fingerprint re-opens it (the last event wins), the ledger's own
  kind distinguishes a failing verification contract from an arbitrary command,
  and a regression test covers all three states. Measured live against this
  repository's own ledger, which is where the defect surfaced.

- The capability refresh stopped asking each CLI for `--help` a second time.
  `model-doctor` already had that output, so the refresh added a duplicate
  invocation of every installed provider — slower everywhere, and in the test
  suite it turned fixture runs into real CLI calls, which made unrelated
  timing-sensitive tests fail at random. Caught by the suite failing twice on
  two different tests while three runs of the pre-capability commit passed. The
  doctor now hands its help output over, and three consecutive runs are clean.
  Routing's own binary-identity check is memoised per process and uses `stat`
  rather than starting a python interpreter.
- The worker guard was blind to new source files in any repository with a large
  ignored tree. It spent its entry budget in path order, so a real project whose
  `data/` and `runs/` hold tens of thousands of ignored files exhausted the cap
  alphabetically and never reached the handful of untracked source files — the
  exact places a stray worker write lands. Untracked-but-not-ignored paths are
  now scanned first, so truncation can only ever drop ignored churn, and the
  message says which class was cut: reaching the cap inside the ignored tree is
  a note, failing to cover the untracked files is a warning that coverage is
  partial.
- `peer-delegate` says when the caller's tree is dirty. The worktree is built
  from HEAD, which is the right isolation and was completely silent: a brief
  written about code the caller has on screen can describe something the worker
  will never find, and the whole round is spent against a tree that does not
  contain the problem. The count is printed once and recorded in the artifact,
  so whoever reviews the patch later knows which base it was written against.
- `peer-delegate` no longer reports a worker that could not act as a clean run.
  A CLI denied a tool it cannot prompt for exits 0 having printed only its
  refusal, and the empty patch that follows looks exactly like honest work on an
  already-correct tree. The worker status is now 126 ("found but could not
  execute"), the reason is quoted, and — like a missing CLI — no repair round is
  spent, because no rewording of a brief grants a permission.
- Landings are now serialized per repository. Everything from the clean-tree
  check through `git apply` was a check-then-act on the shared working tree with
  no lock, so two agents could both be admitted against the same base and both
  apply — each reviewed without the other's changes. A second lander is refused
  outright rather than queued, because the tree it was admitted against is the
  one the first lander is changing. `oms_hold_file_lock` holds a lock for the
  rest of a process, which `oms_with_file_lock`'s subshell cannot do when later
  steps need variables the earlier steps set.
- A council counted a provider refusal as an answer. Antigravity's headless mode
  cannot prompt for a tool permission, so it auto-denies anything outside
  `permissions.allow`, exits 0, and prints only its own explanation — text
  declarative and long enough to pass every answer-quality rule. The last live
  audit therefore reported "2 answered, 2 independent model families" when one
  provider had spoken. Answer quality now classifies a body that is *entirely*
  CLI diagnostics as `blocked` (an answer that merely discusses permissions is
  untouched), keeps it out of the answered and family counts, and quotes the
  provider's own reason so the operator knows what to fix. `doctor` reports the
  same thing before a call is spent, by checking whether antigravity's
  allow-list covers the commands a repository read needs. Probing the CLI
  showed a curated command list cannot work — a `command` rule is matched
  against the whole command line, so the first `cd x && rg y` a peer runs is
  denied — while `command(*)` is not the write authority it appears to be:
  under `--sandbox`, which is how this harness invokes agy, escaping to write
  needs a separate `unsandboxed` rule. The check now asks for `command(*)` plus
  `read_file(*)` instead of an unwinnable list of commands.

- Antigravity never received its prompt. `agy --print` takes the prompt as its
  value (`--prompt` is an alias), so a prompt piped on stdin was ignored and the
  next flag became the prompt — every antigravity answer was a reply to the
  literal string `--sandbox`, in councils, reviews, and delegated writes alike.
  The prompt is now passed as the flag value.
- A landing no longer reports `complete` when its records did not land: a failed
  lineage row or plan finish leaves `applied-pending-receipt`, which `oms state`
  shows and `patch-land --recover` retries until both records exist.
- `patch-land` rechecks the base commit and clean tree immediately before the
  apply and refuses when either moved during admission, so what lands is the
  combination that was verified.
- Antigravity model routing follows the published benchmarks: 3.6 Flash wins
  every coding and agentic suite against 3.1 Pro (SWE-Bench Pro, DeepSWE,
  Terminal-Bench, MLE-Bench) at roughly twice the speed, and Pro keeps only a
  narrow pure-reasoning lead, so all three tiers now use the 3.6 Flash variants.
- `model-doctor --live-models` no longer reports every Antigravity route as
  missing: the CLI lists its catalog as slugs (`gemini-3.6-flash-low`) while
  naming the same model `Gemini 3.6 Flash (Low)` on `--model`, so the comparison
  now normalizes both sides. The smoke fixture printed display names and hid the
  mismatch; it now uses the notation the real CLI emits.
- Receipt-owned branch and detached auto-updates now share `update.sh`'s
  rollback transaction instead of maintaining a second half-linked path.
- `update.sh --tools` now requests real provider CLI and uv upgrades instead of
  silently accepting already-installed binaries.
- `peer-ask --repo` now keeps its default artifacts under the selected state
  repository instead of leaking them into the caller's working directory.
- `fail-ledger` now accepts the documented `--repo` option and honors
  `OMS_STATE_REPO`, matching the other shared harness state commands.
- Removed the unused read-executor surface: executors are now write-worktree
  contracts only. `--mode worktree-write` remains an unadvertised compatibility
  no-op for existing callers; legacy `mode: read` metadata stays inspectable
  and retireable but cannot validate, start, or delegate.
- Mixed read/write requests such as `review and fix` now route to an isolated
  write worker instead of stopping at a read-only pass.
- Peer quorum lists reject duplicate providers after canonicalizing the `agy`
  alias, preventing one CLI from counting as multiple independent reviewers.
- Capacity fallback now treats ignored files as worktree mutations, recreates
  Antigravity read isolation before retrying, and removes ignored verification
  byproducts before repair.
- Dry-run and export-only passes validate their provider route and record the
  selected model; unknown Antigravity variants no longer claim an inferred
  reasoning effort that the CLI did not expose.
- Plan-bound executors can run through `plan-run` against their exact claimed
  provider and lease. Creation rejects invalid plan claims, signal cleanup
  preserves review evidence, and known failures key on resolved contracts.
- Legacy executor metadata without reasoning fields now honors an explicit
  caller effort instead of silently replacing it with automatic effort.

- Aligned advisor and spec gates across global rules, skills, templates, docs,
  and prompts: routine completion and clear bounded changes no longer trigger
  mandatory advisor/interview workflows.
- Hardened install ownership and parity: foreign symlinks round-trip through
  backup/restore, foreign doctor calls delegate to the canonical implementation,
  plugin identity uses the full marketplace ID, stale/missing expected plugins
  fail health checks, and auto-update refreshes hooks/plugins before reporting
  success. Repeated relinks preserve restoration backups, scheduled updates keep
  hook/plugin opt-outs, and unlink accepts pre-split global-rule links.
- Signal cleanup now terminates the full provider subprocess tree, preventing a
  cancelled read-only call from retaining output pipes until its timeout.
- GC now counts an empty compacted failure ledger as zero instead of producing
  a duplicate `0` value and an integer-comparison warning.
- `link.sh` now removes dangling skill links owned by the checkout (left
  behind when a skill is renamed or removed), so renames like
  `multi-agent-*` -> `peer-*` do not strand old links in agent skill dirs.
- Crash-atomicity where the harness diverged from its own tmp+mv standard:
  the `.oms/runs/CURRENT` pointer (read locklessly by every auto-linking
  tool), the change-guard snapshot, and `artifact-index prune` (now an atomic
  replace at the symlink target instead of truncate-in-place).
- Provider/verify timeouts escalate to SIGKILL via `--kill-after` (a worker
  that traps SIGTERM no longer survives the bound; probed for busybox), and
  `OMS_REQUIRE_TIMEOUT=1` refuses to run unbounded when no timeout binary
  exists instead of only warning.
- Run-id entropy no longer degrades to a bare pid when `/dev/urandom` is
  unreadable (pid+time+`$RANDOM` mix), and a failed urandom read no longer
  yields an empty suffix.
- Lock fallback under real contention and delegate SIGKILL recovery are now
  covered by tests (`OMS_LOCK_FORCE_MKDIR` two-writer race; `kill -9` mid-
  delegate → `gc` sweeps the orphan marker and releases the plan task).

- `patch-admit.sh`: a worktree apply failure was swallowed (`|| true`), so the
  syntax/verify gates could pass against the UNPATCHED tree — now recorded as an
  `apply-worktree` FAIL and the gates are skipped. numstat parsing split on
  whitespace (paths with spaces escaped the syntax gate) — now tab-delimited.
  The verifier-integrity gate was bypassable with `cd`/absolute `--verify`
  spellings — now matches by path and basename and also protects common build
  entrypoints (Makefile, package.json, pyproject.toml, …).
- bash 3.2: `multi-agent-review.sh` used `declare -A` (verdicts/`--gate` died on
  macOS) — replaced with newline-delimited records; `change-guard.sh begin`
  aborted under `set -u` with no `--allow` (unguarded array expansion) — fixed.
- `experiment-board.sh`: a stale-claim reclaim kept the dead original owner
  (broke `--owner` and attribution) — a (re)claim now reassigns the owner while
  touch/start/finish keep it.
- `multi-agent-common.sh`: an agy isolated read worktree leaked on
  INT/TERM/HUP — its temp dir is now residue-marked (prefix `oh-my-setting-*`)
  so `cleanup.sh`/doctor reclaim a signal-leaked worktree.
- `multi-agent-delegate.sh`: `REPO` is normalized to the git worktree root, so
  `--plan-task` verify hydration no longer silently drops when run from a
  subdirectory; the hydrated-brief temp file no longer leaks on a hydration
  failure.
- `oms-run.sh ls --open`: applied the open filter before taking the last N,
  hiding older still-open runs — now filters first, then slices.
- CI (`test.yml`): added a static bash-4ism gate (`declare -A`/`mapfile`/case-
  conversion) and put `scripts/oms` under shellcheck and macOS `bash -n`, since
  `bash -n` alone let runtime-only bash-4 constructs slip past.

- `patch-admit.sh` records each admission in the artifact index, so the report
  survives `artifact-index.sh prune --files` (which deletes unreferenced files
  under `.oms/artifacts/`); and it now fails closed when a patch modifies its
  own verifier (e.g. rewriting `scripts/check.sh` to self-certify), overridable
  with `--allow-verifier-change`.
- `change-guard.sh check` includes changes committed after `begin` (diffs the
  stored begin-HEAD against HEAD), so an agent that commits no longer escapes
  the allow/deny path scope; the stored begin-head is finally read.
- Run-cluster state (spine, default ledger, capsules, board, manifests,
  reconcile) anchors to the git worktree root instead of `$PWD`, so a
  subdirectory invocation no longer forks a second `.oms`; every run tool now
  also drops the `.oms/.gitignore` guard on first write.
- File locks live in a fixed `~/.cache/oh-my-setting/locks`: an interactive
  and a cron/ssh agent no longer compute different lock dirs (via
  `XDG_RUNTIME_DIR`) for the same state file, which defeated mutual exclusion.
- `doctor.sh` certifies symlink identity, not existence: a config or skill
  link resolving to a foreign/stale target fails as "linked elsewhere"
  (regular files where a link is expected also fail).
- bash 3.2: `declare -A` in the skill-doctor duplicate check aborted the whole
  check on macOS; replaced with a portable dedup.
- Provider namespace is canonical: `agy` normalizes to `antigravity` in
  `agent-run.sh` and `agent-plan.sh` claims; unknown provider names are
  rejected instead of polluting the board.
- Verify commands in `multi-agent-delegate.sh` and the review `--gate`
  backstop are bounded by `OMS_MULTI_AGENT_VERIFY_TIMEOUT` (default 10m); a
  hung test suite fails the run instead of wedging it forever.
- Antigravity read passes run in an isolated detached-HEAD worktree (or a
  scratch dir outside git): agy has no file-write-blocking flag, so stray
  writes are discarded instead of reaching the caller's tree.


### Security
- `data-manifest.sh` leakage fails closed when a recorded split file, id column,
  or key column is missing; manifest names reject path traversal.
- `run-ledger.sh` blocks a sensitive-looking gate skip `--reason` and no longer
  echoes the raw reason to stderr.
- CI workflow runs with `contents: read` permissions.


## [0.3.0]

- Baseline: cross-agent harness (Codex/Claude Code/Antigravity) with project
  templates, multi-agent review/delegate, run ledger/capsule, data manifest,
  Slurm/HPC helpers, and shared memory.
