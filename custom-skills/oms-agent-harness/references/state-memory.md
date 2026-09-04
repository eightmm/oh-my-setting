# State, Memory, and Live Edits

Use project memory for stable preferences and recurring pitfalls; keep required
rules in checked-in policy. Prefer compact `context` or targeted `search` over
loading the full source log. Peer calls that attach memory also attach a
query-ranked "relevant recall" section keyed on the prompt — recency answers
"what happened lately", recall answers "what do we know about this".

```bash
oms agent-memory --repo . context
oms agent-memory --repo . search --text pgvector
oms agent-memory --repo . --json search --text pgvector
oms agent-memory --repo . recall --text "database migration"
oms agent-memory --repo . append --agent codex --text "Run the focused check first."
oms agent-memory --repo . append --source-file src/policy.py --source-line 42 --text "This invariant is enforced here."
oms agent-memory --repo . pin --agent codex --text "Current migration boundary: v2."
```

Use the active task packet for one short-lived handoff. Automatic prompt
recording is opt-in with `OMS_AUTO_TASK=1`.

```bash
oms agent-task --repo . init --goal "Ship the focused fix" --verify "bash scripts/check.sh"
oms agent-task --repo . update --state "Patch ready" --next "Run checks"
oms agent-task --repo . verify --verification "focused check passed"
oms agent-task --repo . close
```

At session start the resume hook prints one `[oms resume]` block: active task
(goal/next/verify), newest handoff pointer, unresolved failures with their
recorded next action. On the first prompt, the router separately warns once per
live neighboring session when another agent touched the same worktree in the
last 15 minutes; a clean `SessionEnd` retires that neighbor immediately. Heed
that warning before any dirty-tree `git add`. Treat the resume block as the
entry point; follow its pointers
(`oms session-handoff show`, `oms fail-ledger list`) before re-deriving state.

Work Journal automatically projects durable lifecycle receipts and explicit
Agent State outcomes into local daily/weekly summaries. Capture needs no manual
command: use the existing structured fields (`--result`, `--decision`,
`--last-failure`, `--next`) when those facts matter, and let the observer
reference the task receipt. Reading has exactly one command: when resuming
work in a repository, run `oms journal show --blockers` and
`oms journal show --today` (also `--week`, `--recent N`, `--json`) before
planning — that is where past sessions' decisions, failures, and next actions
live. The first prompt of each local day also injects a bounded digest
automatically. For maintenance, use `oms journal status`,
`oms journal rebuild`, or `oms journal sync --force`. Set
`OMS_WORK_JOURNAL=0` only when a project must opt out. Recurring blockers and
recorded decisions graduate into shared memory automatically once per local
day at the prompt tick, deduplicated against notes `agent-task close` already
promoted (`OMS_JOURNAL_AUTODISTILL=0` opts out); run `oms journal distill`
for an off-cycle pass — `--dry-run` previews what would graduate.

Before `close`, set `OMS_AGENT_TASK_CLOSE_MEMORY=0` when the outcome should not
be promoted into durable memory.

Use a change guard only when user edits or scope drift are plausible:

```bash
oms change-guard --repo . --allow scripts/ begin
oms change-guard --repo . check
oms change-guard --repo . end
```

At resume, prefer the ranked read-only queue over manually inspecting every
state family. Its optional repair mode is deliberately narrow.

```bash
oms inbox --repo .
oms inbox --repo . --fix-safe
```

Before a risky local refactor, preserve tracked staged and unstaged state. A
restore first verifies in a temporary worktree, defaults to dry-run, requires
the same HEAD, and creates an automatic recovery checkpoint when applied.

```bash
oms checkpoint create --label "before parser rewrite"
oms checkpoint verify <id>
oms checkpoint restore <id>          # dry-run
oms checkpoint restore <id> --apply
```

The skill router emits hints and records guarded routes. Disable it with
`OMS_SKILL_ROUTER_OFF=1`; disable the final verification guard with
`OMS_TURN_GUARD_OFF=1`.

## Project skills

A procedure you re-derived, or a failure that repeated and was then resolved,
is a lesson worth standing context. Forge it as a project skill: stored under
`.oms/skills/<name>/`, linked into `.agents/skills` and `.claude/skills` so
every CLI loads it natively, and kept out of git.

```bash
oms skill-forge add --name flaky-itest --file /tmp/skill.md   # or stdin
oms skill-forge list
oms skill-forge show flaky-itest
oms skill-forge remove flaky-itest
```

The forge validates before storing: frontmatter `name` matching the
directory, a description of 40-1024 characters (that is what routing
reads), a 500-line budget, only Agent Skills frontmatter fields at top
level (put extensions under the `metadata:` map), and a sensitive-content
scrub. An optional verification contract declares the evidence the skill
expects — declare it as `metadata:` then an indented `verify: <command>`;
`agent-task close` reminds about it and never executes it.

Scope: project-specific procedure only. Machine and cluster facts
(`sinfo`/`sacctmgr` output, GPU inventory) belong to `oms snapshot
[--cluster]` references consumed by the global `oms-gpu-workstation`/`oms-slurm`
skills; a habit useful in every repository is a candidate for the global
catalog, not a per-project copy. Keep the set small — a few strong skills
route better than many thin ones.

## Memory write contract

`shared.md` and `pins.md` are append-only. Correct a note by appending another;
never edit or trim one in place. `summary.md` is derived from `shared.md` and is
regenerated by `oms agent-memory compact`, so it is not a record. Delegated
workers are held to this mechanically: rewriting existing bytes in either log
fails the delegation.

`memory.sqlite3` is a derived project-local query index. Existing Markdown-only
projects are indexed automatically; use `db-path` to locate it and `rebuild` to
recreate it from the append-only sources. `search` is exact substring recall;
`recall` is ranked SQLite FTS. Neither sends text to a model or downloads an
embedding model.

New project-memory entries carry hidden, append-only provenance: a stable event
ID, note kind, active task/session hashes, current Git HEAD, dirty state, and a
bounded Git-state fingerprint. No branch name, command, or raw diff is stored.
The explicit `--source-file/--source-line` mode is the narrow path exception:
it records a tracked repository-relative path, committed blob oid, and exact
line hash. Such notes never enter compact context; `search`/`recall` validates
them against current `HEAD`, follows a uniquely moved line, and hides stale
facts unless `--include-stale` is requested for audit. Add `--json` for the
machine-readable citation verdict. Legacy notes receive stable derived IDs on
indexing. Use `oms run timeline` for the full project event timeline instead
of copying every commit or tool event into durable memory.
