# Work Journal

Work Journal is the harness's automatic project memory: a local, derived view
of outcomes that the existing control plane already knows. It is not another
run tracker.

## Ownership and storage

The existing run ledger remains authoritative for commands, gates, tests, and
metrics. Reproducibility capsules own configs, seeds, environment facts, and
outputs; artifact-index owns lineage; Agent State and handoffs own live context;
the research runner and reconcile records own experiments and jobs. Work
Journal keeps only a bounded, sanitized projection of those records plus
decision, blocker, and next-action annotations that have no natural home in an
existing schema.

Canonical Work Journal events are immutable schema-versioned rows in the
append-only `.oms/work-journal/events.jsonl`. A create-once `project.json`
keeps the local project identity stable; if it is lost, the identity is
recovered from canonical events before a new one is derived. The SQLite index
at `.oms/work-journal/index.sqlite3`, the bounded `index.json`, daily and weekly
Markdown, quarantine diagnostics, and Notion sync state are derived and can be
deleted and rebuilt from those events. `.oms/.gitignore` keeps all generated
state out of commits.

All lifecycle integrations call one internal observer. The observer uses the
existing process-safe file lock, writes events once by stable ID, and atomically
replaces derived files. Normal capture looks up deduplication in SQLite and
rewrites only the affected day and ISO week. `index.json` carries totals,
periods, and at most the 256 most recent active event descriptors; it is not an
ever-growing second event log. A missing, stale, or corrupt SQLite index is
recovered from JSONL. It releases the local lock before attempting optional
export under a separate non-blocking sync lock. Journal failures are fail-open:
they can emit a bounded diagnostic but never change the task, gate, patch, run,
or job result that was being observed.

## Capture and identity

Capture happens after durable run/capsule, validation, patch review/admission,
CI, job reconciliation, Agent State, and handoff writes. The provider prompt
hook first performs a local tick that reconciles `HEAD` and materializes dirty
periods, then gives at most one closed or pending Notion item a two-second
retry. This covers the first agent execution after a date or ISO-week rollover
without a daemon or scheduler. Once per local day it also injects a
bounded digest of at most three recent blockers and three next actions, plus a
one-line previous-day count and, when a session handoff digest was captured in
the last 48 hours, a one-line pointer to the newest one — a pointer only,
never the handoff's content. This is the automatic read path: the journal can
inform planning without loading its full history on every prompt. Set
`OMS_WORK_JOURNAL_DIGEST=0` to keep rollover capture but disable that injection.
Durable lifecycle observations remain local while work is running. On an
allowed top-level Stop, the final `HEAD` is reconciled and today's daily page is
force-synced once; stable content hashes make duplicate Stop delivery a no-op.

An event ID is derived, in order, from an authoritative source record ID, a
caller operation ID, or a hash of normalized stable source fields. Current time
is never a deduplication input. `occurred_at` is UTC RFC 3339; local date and ISO
week are computed with `OMS_WORK_JOURNAL_TIMEZONE` when set and the system
timezone otherwise. When an IANA database is unavailable, `system-local` mode
asks the operating system for the offset of each event timestamp instead of
freezing the current offset; this keeps Windows late-event conversion DST-safe.
If an explicitly configured non-UTC IANA zone cannot be loaded because the host
has no timezone database, capture continues in the visibly recorded
`system-local` mode rather than silently dropping the event. Install Python's
`tzdata` package when the configured zone must differ from the Windows host
zone. Late events therefore rematerialize their original day and week.
Corrections append a new event with `supersedes_event_id`; canonical events are
never edited.

Set `OMS_WORK_JOURNAL=0` to disable capture and materialization. Work Journal is
otherwise on by default.

Operational commands are intentionally small:

```bash
oms journal show --repo . --today
oms journal show --repo . --week
oms journal show --repo . --blockers
oms journal show --repo . --recent 20 --json
oms journal status --repo .
oms journal rebuild --repo .
oms journal sync --repo .
oms journal sync --repo . --force
oms journal sync --repo . --force --today
oms journal sync --repo . --force --recent-days 7
```

`show` reads only the requested summary or bounded indexed event slice.
`status` reports local size/counts and credential/sync health without exposing
the credential. `rebuild` is the explicit full JSONL scan and recreates all
derived views. Routine capture, reads, and sync use the SQLite index instead.
These commands share the same local and remote locks as automatic lifecycle
capture, so an explicit rebuild or sync cannot race an observer.
`sync --recent-days N` limits the mirror update to the last `N` local calendar
days and every ISO-week page that overlaps them. Combine it with `--force` when
the range includes today's open daily page or the current open week.

## Summaries

Daily files use this stable shape:

```markdown
# Daily Work Journal — 2026-07-31

## 한눈에 보기
## 핵심 진전
## 프로젝트별 작업
### project-name
## 검증된 것
## 아직 검증되지 않은 것
## 실패하거나 보류한 접근
## 의사결정
## Blockers
## 다음 우선순위
```

`한눈에 보기` / `At a glance` is a mechanical period summary: event count,
passed and unverified checks, failed or parked outcomes, and distinct decision,
blocker, and next-action counts. It does not infer completion or progress. A
short decision, blocker, or next action also carries its short related outcome
so a sentence such as "retry it" is not detached from the work it refers to;
long context is left out instead of making the page noisier.

Labels follow the environment: `OMS_WORK_JOURNAL_LANG` wins, then
`oms journal configure --lang ko|en`, then the locale (`ko*` renders Korean),
then English. After changing the language, run `oms journal rebuild --repo .`
to re-render existing daily/weekly views. This changes only derived Markdown,
never canonical events. Renderer upgrades similarly mark every indexed period
dirty and recreate those derived views on the next materialization.

Weekly files group progress, verified outcomes, repeated blockers, decisions,
comparable experiment series, and next priorities directly from structured
events—not from daily prose. Weekly decisions and next actions carry their
local date; the daily page itself already supplies that context. Claims cite an
event ID and, when available, the authoritative evidence reference. Metrics
are compared only when name, unit, dataset/split, and evaluation conditions
match; Work Journal never invents progress percentages or treats a successful
process exit as a scientific conclusion.

Representative generated weekly content is deliberately evidence-first:

```markdown
# Weekly Work Journal — 2026-W31

## 완료하거나 검증한 작업

- Focused validation passed [wj_<event-id>; evidence run-ledger:<record-id>]

## 비교 가능한 실험 추세

- val_auc (unit=ratio, dataset=D, split=scaffold, conditions=seed=1):
  0.70 → 0.80 [wj_<first>; wj_<second>]
```

Research projections may include an observed commit, config reference or hash,
dataset/split, seed, metric, artifact/checkpoint, run/job ID, execution
environment, verdict, and next experiment. Missing fields stay missing.
Transcripts, raw stdout/stderr, full environments, full diffs, and arbitrary
metric parsing are deliberately excluded.

## Reading the journal

Reading is always local; the Notion mirror is never in the agent read path.
`oms journal show` is the one read command:

- `oms journal show --today`, `--week`, or `--period 2026-W31` prints the
  daily or weekly summary for that period.
- `oms journal show --blockers` lists deduplicated blocker and next-action
  annotations observed in the last seven days, newest occurrence first, each
  with its event ID. Annotations have no resolution record, so "open" means
  recently observed and not superseded.
- `oms journal show --recent 20` prints the most recent events one line each.
- `--json` returns the same data structurally in any mode.

When resuming work in a repository, read `--blockers` and `--today` before
planning: they carry exactly the decisions, failures, and next actions that
earlier sessions recorded.

The prompt hook's bounded once-per-local-day digest, described under capture,
is the automatic counterpart to these commands; its marker is derived state
and can be deleted.

## Data safety and fallback

Only allowlisted structured fields enter an event. A recursive sanitizer runs
before local persistence and again before export. It redacts secret-shaped
keys and values, Authorization/Bearer material, credential-bearing URLs and git
remotes, and bounds nested strings and collections. Machine paths are reduced
to safe references. A sanitizer is not permission to persist raw input: source
adapters never copy transcripts, environment dumps, raw logs, or diffs.

Template Markdown is canonical. The repository does not make a recursive agent
call to rewrite it; an optional enrichment failure or timeout always falls back
to the template.

## Notion mirror

The normal interactive installer owns the complete connection flow:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

It installs `ntn` through npm, delegates browser authorization to `ntn login`,
and discovers an accessible data source with the required schema. If discovery
finds none or more than one, select the target explicitly:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh \
  | bash -s -- --connect-services --notion-data-source <data-source-id>
```

On an existing install, the equivalent repair is
`~/.oh-my-setting/scripts/connect-services.sh --required`; add
`--data-source-id ID` only to resolve ambiguous discovery. `--required` makes
an incomplete login or target selection fail visibly. The installer's default
`auto` mode instead prints exact follow-up commands and continues when no
interactive terminal is available.

On a machine without a usable OS keychain (headless hosts, sandboxed agent
sessions), `connect-services` detects ntn's keychain error, switches the flow
to ntn's file-based credential store (`NOTION_KEYRING=0`), names that mode in
its login hint, and persists `keyring: "file"` alongside the `cli_command`
transport choice in `work-journal.json` — so hooks and future sessions
inherit the working store from config, with no dependence on PATH order or
ambient environment variables. `OMS_NOTION_KEYRING`/`OMS_NOTION_CLI` still
override per invocation.

The installer validates access and the property schema. It persists only the
target, property names, and `ntn` transport choice in
`$XDG_CONFIG_HOME/oh-my-setting/work-journal.json` (normally
`~/.config/oh-my-setting/work-journal.json`; `%LOCALAPPDATA%` is used by native
Windows Python). `ntn` keeps workspace credentials in the operating system's
credential store; the harness invokes `ntn api` and never extracts them. A
legacy unattended path may still provide `OMS_WORK_JOURNAL_NOTION_TOKEN` to the
agent process, but that value is never written to config, a project, or an
install receipt. Codex's connected Notion app credential likewise belongs to
the connector and cannot be extracted or reused by a local shell script.

When the CLI session and configured target are present, work-time observers
only update local state. A prompt-start check retries at most one closed or
pending summary within two seconds. An allowed Stop publishes today's daily
summary within the normal eight-second sync budget. The current ISO week stays
local until it closes unless `oms journal sync --force` is requested. This
gives each top-level turn two bounded checkpoints without adding network work
between them.

`OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID` can override the persisted target and
uses the current Notion data source API.
`OMS_WORK_JOURNAL_NOTION_DATABASE_ID` remains a compatibility path for
single-source databases on the 2022 API. With the credential or both target
values absent, the adapter is disabled and performs no network call. If both
target values are set, the data source takes precedence. A hashed target and
property-schema fingerprint scopes local sync state, so changing the target
resynchronizes summaries instead of reusing page IDs from the old mirror.

The mirror writes native Notion heading, paragraph, list, to-do, callout,
toggle, and divider blocks rather than one large code block. Daily decisions
remain prominent callouts; weekly decisions stay a compact dated list so a busy
week does not open as a wall of callout boxes. Result and interpretation detail
stays visually attached to its work item, while evidence IDs remain in the
complete local summary. It uses a stable summary key and content hash:
unchanged content is a no-op, changed content updates the known page, and
missing local sync state first queries by key before creating a page.
HTTP 429 and retryable 5xx responses use bounded retries and respect
`Retry-After`. Delays longer than the small inline allowance become `pending`
with `next_retry_at` instead of sleeping in a lifecycle hook. Each sync tick has
an eight-second total network budget; timeouts and permanent 4xx responses
remain pending or failed locally without touching the canonical summary.

The indexed/exportable summary remains bounded. If a very busy period exceeds
the display budget, complete lines from overview, decisions, blockers, and next
actions are kept ahead of low-signal listings and an explicit omission marker
is added. The local Markdown file remains complete, and `Has Blocker` is
computed from that full rendering rather than the bounded mirror. Individual
bounded fields end with `…` when truncated.

The target data source must provide these properties:

| Name | Type | Purpose |
|---|---|---|
| `Name` | title | Human-readable summary title |
| `Work Journal Key` | rich text | Stable project/kind/period upsert key |
| `Content Hash` | rich text | Unchanged-content detection |
| `Project` | rich text | Project filter |
| `Kind` | select | `daily` or `weekly` |
| `Period` | date | Day, or Monday of an ISO week |
| `Has Blocker` | checkbox | Review filter |

Names are configurable with the corresponding
`OMS_WORK_JOURNAL_NOTION_*_PROPERTY` variables. `oms journal status --json`
reports whether the target is configured, whether a credential is available,
and local synced/pending/failed counts.

The `Project`, `Kind`, `Period`, and `Has Blocker` properties are what make
the mirror browsable rather than a pile of pages. Recommended views on the
Notion side: a calendar or timeline over `Period`, a `Has Blocker` filter for
stuck work, grouping by `Project` when several repositories share one
database, and a `Kind` filter separating daily pages from weekly rollups.

GitHub SSH and HTTPS remotes normalize to the same lowercase
`github.com/owner/repository` project identity, so switching protocols does not
split the journal. Duplicate prevention is serialized and guaranteed within
one local state root.
The remote lock is non-blocking: when another sync is active, local capture
still completes and the mirror is retried by a later start or finish boundary.
The remote key lookup also reduces duplicates after local sync-state loss.
Notion does not offer a uniqueness constraint, so two machines or unrelated
writers racing against the same database can still create duplicate pages; the
local canonical journal remains correct and the mirror can be discarded and
resynchronized.
