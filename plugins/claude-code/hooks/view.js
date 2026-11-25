// Pure projection-to-lines logic for the oms-panel pane. No mods API here, so
// the same functions run in the pane and in plain unit tests.
//
// Input is the schema-1 `oms-dashboard` object documented in docs/DASHBOARD.md.
// Every label in it is untrusted text: it is cleaned and bounded before it is
// drawn, and nothing in it is ever executed.

export const SCHEMA = 1
export const KIND = 'oms-dashboard'
// Mirrors the dashboard's own rule: a larger source is unavailable, not truncated.
export const MAX_STDOUT_CHARS = 4 * 1024 * 1024
const MAX_LABEL = 200
const MAX_LINE = 2000
const MAX_ROWS = 8

export const TABS = [
  { id: 'now', label: 'Now', hotkey: '1' },
  { id: 'work', label: 'Work', hotkey: '2' },
  { id: 'results', label: 'Results', hotkey: '3' },
  { id: 'activity', label: 'Activity', hotkey: '4' },
]

const isObject = (value) => value !== null && typeof value === 'object' && !Array.isArray(value)
const list = (value) => (Array.isArray(value) ? value : [])

// Replace control and format characters (terminal escapes, bidi overrides,
// zero-width marks) with "?", then bound the length by code points.
export function clean(value, max = MAX_LABEL) {
  const text = String(value).replace(/[\p{Cc}\p{Cf}]/gu, '?')
  const points = Array.from(text)
  return points.length > max ? points.slice(0, Math.max(0, max - 1)).join('') + '…' : text
}

// Unknown is not zero: a missing value is shown as "unknown", never as 0 or "".
export function shown(value) {
  if (value === null || value === undefined || value === '') return 'unknown'
  if (typeof value === 'object') return 'unknown'
  return clean(value)
}

function counts(value) {
  if (!isObject(value)) return ''
  return Object.entries(value)
    .map(([key, n]) => clean(key, 40) + '=' + shown(n))
    .join(' ')
}

// One drawn line. A line is bounded as a whole, whatever number of labels it joins.
const line = (text, tone) => {
  const bounded = clean(text, MAX_LINE)
  return tone ? { text: bounded, tone } : { text: bounded }
}

// Turn one `$.process.run` result into what the pane keeps between redraws.
// stderr is never read: it may quote private record contents.
export function parseDashboard(result) {
  const exitCode = isObject(result) ? result.exitCode : undefined
  // 0 = every source collected, 1 = degraded view; both print the projection.
  if (exitCode !== 0 && exitCode !== 1) {
    return { status: 'error', message: 'oms dashboard exited with status ' + shown(exitCode) }
  }
  const stdout = typeof result.stdout === 'string' ? result.stdout : ''
  if (stdout.length > MAX_STDOUT_CHARS) {
    return { status: 'error', message: 'oms dashboard output is larger than 4 MiB; not shown' }
  }
  let report
  try {
    report = JSON.parse(stdout)
  } catch {
    return { status: 'error', message: 'oms dashboard did not print JSON' }
  }
  if (!isObject(report) || report.schema !== SCHEMA || report.kind !== KIND) {
    const schema = isObject(report) ? shown(report.schema) : 'unknown'
    const kind = isObject(report) ? shown(report.kind) : 'unknown'
    return {
      status: 'error',
      message: 'unsupported contract (schema ' + schema + ', kind ' + kind + '); this pane reads schema 1 oms-dashboard',
    }
  }
  return { status: 'ready', report }
}

// Lines above the tab body: where the snapshot is from and what failed to collect.
export function headerLines(snapshot) {
  if (!snapshot || snapshot.status === 'empty') return [line('no snapshot yet: press r to read oms dashboard', 'dim')]
  if (snapshot.status === 'error') {
    return [
      line('OMS UNAVAILABLE: ' + clean(snapshot.message), 'error'),
      line('check: oms dashboard --json', 'dim'),
    ]
  }
  const report = snapshot.report
  const repo = isObject(report.repo) ? report.repo : {}
  const out = [line(shown(repo.name) + ' · ' + shown(repo.branch), 'dim')]
  const sources = isObject(report.collection) ? report.collection.sources : null
  if (isObject(sources)) {
    for (const [source, status] of Object.entries(sources)) {
      if (status !== 'ok') out.push(line('COLLECTION FAILED ' + clean(source, 40) + ': ' + shown(status), 'error'))
    }
  }
  if (isObject(report.collection) && report.collection.ok !== true) {
    out.push(line('DEGRADED: unavailable sections are not known, not empty', 'warn'))
  }
  return out
}

function goalLine(report) {
  const goal = report.goal
  if (!isObject(goal) || !goal.text) {
    return report.collection?.sources?.state === 'ok'
      ? line('Goal: none recorded', 'dim') : line('Goal: unavailable', 'warn')
  }
  return line('Goal: ' + shown(goal.text))
}

function scopeLine(report) {
  const scope = report.scope
  if (!isObject(scope)) return line('scope: unavailable (runtime projection)', 'warn')
  const allowed = list(scope.allowed)
  const forbidden = list(scope.forbidden)
  if (allowed.length === 0 && forbidden.length === 0) return line('scope: unbounded (none recorded)', 'dim')
  return line(
    'scope: allowed=' + allowed.map((item) => clean(item, 60)).join(',') + ' (' + shown(scope.source) + ')' +
      '  forbidden=' + forbidden.map((item) => clean(item, 60)).join(','),
  )
}

function taskLine(report) {
  const task = report.task
  if (!isObject(task) || task.healthy !== true) return line('Project task: unavailable', 'warn')
  if (task.present !== true) return line('Project task: none active', 'dim')
  return line(
    (task.stale === true ? 'Previous task: ' : 'Project task: ') + shown(task.task_id) + ' · ' + shown(task.status),
    task.stale === true ? 'warn' : undefined,
  )
}

function planLine(report) {
  const plan = report.plan
  if (!isObject(plan) || plan.healthy !== true) return line('plan: UNAVAILABLE or invalid', 'warn')
  if (plan.present !== true) return line('plan: none', 'dim')
  const actionable = list(plan.actionable)
  return line(
    'plan: ' + shown(plan.task_count) + ' task(s)  ' + counts(plan.by_state) +
      (actionable.length ? '  actionable=' + actionable.map((item) => clean(item, 60)).join(',') : ''),
  )
}

function attemptsLine(report) {
  const attempts = report.attempts
  if (!isObject(attempts) || attempts.available !== true) {
    return line('attempts: UNAVAILABLE (lifecycle stream invalid)', 'warn')
  }
  return line(
    'attempts (OMS lifecycle): ' + shown(attempts.total) + ' total, ' + shown(attempts.active) + ' open  ' +
      counts(attempts.by_state),
  )
}

function acceptanceLine(report) {
  const acceptance = report.acceptance
  if (!isObject(acceptance) || acceptance.available !== true) return line('Verification: unavailable', 'warn')
  if (acceptance.total === 0) return line('Verification: no criteria recorded', 'warn')
  const verified = isObject(acceptance.counts) ? acceptance.counts.verified : null
  return line(
    'Verification: ' + shown(verified) + '/' + shown(acceptance.total) + ' criteria verified',
    acceptance.complete === true ? 'ok' : undefined,
  )
}

function attentionLine(report) {
  const attention = report.attention
  if (!isObject(attention)) return line('attention: UNAVAILABLE', 'warn')
  return line('attention: ' + shown(attention.actionable) + ' item(s)', attention.actionable > 0 ? 'warn' : undefined)
}

// The next decision is shown as text only. The pane never runs it.
function nextLines(report) {
  const next = isObject(report.attention) ? list(report.attention.next)[0] : null
  if (!isObject(next)) return []
  return [
    line('next decision: ' + shown(next.id) + ' (' + shown(next.authority) + ')'),
    line('  ' + shown(next.command), 'dim'),
  ]
}

function omitted(value) {
  return typeof value === 'number' && value > 0 ? [line('  … ' + value + ' more not shown', 'dim')] : []
}

function now(report) {
  const attempts = report.attempts
  return [
    goalLine(report),
    acceptanceLine(report),
    taskLine(report),
    attempts?.available === true
      ? line('Open attempts: ' + shown(attempts.active)) : line('Open attempts: unavailable', 'warn'),
  ]
}

function attemptRow(row, withUsage) {
  if (!isObject(row)) return line('  (unreadable row)', 'warn')
  const panel = isObject(row.panel) ? row.panel : null
  let text =
    '  ' + shown(row.attempt_id) + ' ' + shown(row.state) + ' ' + shown(row.provider) + '/' + shown(row.tool) +
    ' task=' + shown(row.task_id)
  if (panel) {
    text += ' role=' + shown(panel.role) + ' model=' + shown(panel.model) + ' at=' + shown(panel.location)
  }
  if (withUsage) text += ' tok ' + shown(row.tokens) + ' cost_microusd ' + shown(row.cost_microusd)
  return line(text + ' ' + shown(row.updated_at))
}

function work(report) {
  const out = [planLine(report), taskLine(report)]
  if (report.task?.healthy === true && report.task.present === true) {
    out.push(line('Task verification: ' + shown(report.task.verification)))
  }
  out.push(attemptsLine(report))
  const attempts = report.attempts
  if (isObject(attempts) && attempts.available === true) {
    const open = list(attempts.active_recent).slice(0, MAX_ROWS)
    out.push(line(open.length ? 'Recent open attempts:' : 'Recent open attempts: none', open.length ? undefined : 'dim'))
    for (const row of open) out.push(attemptRow(row, false))
  }
  if (report.delegations === null || report.delegations === undefined) {
    out.push(line('delegations: UNAVAILABLE', 'warn'))
  } else {
    const delegations = list(report.delegations).slice(0, 5)
    if (delegations.length === 0) out.push(line('delegations: none', 'dim'))
    for (const row of delegations) {
      if (!isObject(row)) continue
      const status = row.live === true ? 'live' : row.live === false ? 'ORPHAN (dead pid)' : 'unknown'
      out.push(
        line(
          'delegation: ' + shown(row.id) + ' ' + shown(row.provider) + ' role=' + shown(row.role) +
            ' started=' + shown(row.started_at) + ' ' + status,
          row.live === false ? 'error' : undefined,
        ),
      )
    }
  }
  if (report.goal?.source) out.push(line('Goal source: ' + shown(report.goal.source), 'dim'))
  out.push(scopeLine(report))
  return out
}

function activity(report) {
  const out = [
    line('Snapshot: ' + shown(report.generated_at) + ' · HEAD ' + shown(report.repo?.head), 'dim'),
    ...attention(report),
  ]
  const attempts = report.attempts
  if (attempts?.available === true) {
    const recent = list(attempts.recent).slice(0, MAX_ROWS)
    out.push(line(recent.length ? 'recent outcomes:' : 'recent outcomes: none recorded', recent.length ? undefined : 'dim'))
    for (const row of recent) out.push(attemptRow(row, true))
  } else {
    out.push(line('recent outcomes: unavailable', 'warn'))
  }
  const operations = report.operations
  if (!isObject(operations) || operations.available !== true) {
    out.push(line('operations: UNAVAILABLE (invalid artifact contract)', 'warn'))
  } else {
    const rows = list(operations.recent).slice(0, MAX_ROWS)
    out.push(
      line(rows.length ? 'operations (newest first):' : 'operations: none recorded', rows.length ? undefined : 'dim'),
    )
    for (const row of rows) {
      if (!isObject(row)) continue
      out.push(
        line(
          '  ' + shown(row.provider) + ' ' + shown(row.kind) + ' model=' + shown(row.selected_model) +
            ' served=' + (row.served_model ? shown(row.served_model) : 'unreported') +
            ' exit=' + shown(row.exit) + ' ' + shown(row.status),
          typeof row.exit === 'number' && row.exit !== 0 ? 'warn' : undefined,
        ),
      )
    }
    out.push(...omitted(operations.omitted))
  }
  out.push(line('coverage: ' + shown(report.coverage), 'dim'))
  return out
}

const CRITERION_TONE = { verified: 'ok', failed: 'error', stale: 'warn', missing: 'warn', inconclusive: 'warn' }

function results(report) {
  const out = [acceptanceLine(report)]
  const acceptance = report.acceptance
  if (isObject(acceptance) && acceptance.available === true) {
    if (isObject(acceptance.counts)) out.push(line(counts(acceptance.counts), 'dim'))
    for (const row of list(acceptance.criteria).slice(0, MAX_ROWS)) {
      if (!isObject(row)) continue
      out.push(
        line(
          '  ' + shown(row.status) + ' ' + shown(row.id) + ' [' + shown(row.source) + '] ' + shown(row.text),
          CRITERION_TONE[row.status],
        ),
      )
    }
    out.push(...omitted(acceptance.omitted))
    out.push(line('worker completion is not acceptance', 'dim'))
  }
  const reviews = report.reviews
  if (!isObject(reviews)) {
    out.push(line('review gate: UNAVAILABLE', 'warn'))
  } else if (typeof reviews.outcomes === 'number' && reviews.outcomes > 0) {
    out.push(
      line(
        'review gate: recorded window pass=' + shown(reviews.passed) + ' fail=' + shown(reviews.failed) +
          ' unknown=' + shown(reviews.unknown) + '  seat answers=' + shown(reviews.seat_answers),
        reviews.failed > 0 ? 'warn' : undefined,
      ),
    )
  } else {
    out.push(
      line('review gate: none recorded in window (not a pass)  seat answers=' + shown(reviews.seat_answers), 'dim'),
    )
  }
  return out
}

function attention(report) {
  const out = [attentionLine(report)]
  const value = report.attention
  if (isObject(value)) {
    for (const row of list(value.items).slice(0, MAX_ROWS)) {
      if (!isObject(row)) continue
      out.push(line('  ' + shown(row.priority) + ' ' + shown(row.code) + ': ' + shown(row.summary)))
      out.push(line('     next: ' + shown(row.command), 'dim'))
    }
    out.push(...omitted(value.omitted))
  }
  out.push(...nextLines(report))
  return out
}

const BODIES = { now, work, results, activity }

// Lines for one tab. An unknown tab falls back to Now.
export function bodyLines(report, tab) {
  if (!isObject(report)) return []
  return (BODIES[tab] ?? now)(report)
}
