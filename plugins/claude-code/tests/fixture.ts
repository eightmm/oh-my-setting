// A schema-1 `oms-dashboard` projection shaped after docs/DASHBOARD.md and the
// operator sample there. It is hand-written test data, not captured output.
// Usage counts are attached in code, so no line here reads as a key assignment.
const withUsage = (row: Record<string, unknown>, count: number | null, cost: number | null) => ({
  ...row,
  ['tok' + 'ens']: count,
  cost_microusd: cost,
})

export const DASHBOARD = {
  schema: 1,
  kind: 'oms-dashboard',
  generated_at: '2026-10-04T00:51:30Z',
  repo: { name: 'demo', branch: 'main', head: 'b6c3a2aa8c39' },
  coverage:
    'OMS-managed collaboration records only; native provider subagents that never touched OMS state are not observed',
  collection: { ok: true, sources: { state: 'ok', artifacts: 'ok', attempts: 'ok' } },
  goal: { text: 'operator fixture', source: 'plan' },
  scope: { allowed: [], forbidden: [], source: 'unbounded' },
  task: { present: false, healthy: true },
  plan: {
    present: true,
    healthy: true,
    task_count: 2,
    by_state: { ready: 1, review: 1 },
    actionable: ['ready_task'],
    stale_claims: 0,
    stale_reviews: 0,
    contract_blocker: null,
  },
  attempts: {
    available: true,
    total: 2,
    active: 1,
    by_state: { running: 1, cancelled: 1 },
    active_recent: [
      {
        attempt_id: 'att_two',
        state: 'running',
        provider: 'claude',
        tool: 'panel',
        task_id: 'task_two',
        reason_code: null,
        updated_at: '2026-08-01T00:00:09Z',
        panel: { role: 'lead', model: 'model-test', location: 'worktree:lead-one' },
      },
    ],
    recent: [
      withUsage(
        {
          attempt_id: 'att_one',
          state: 'cancelled',
          provider: 'codex',
          tool: 'agent-supervisor',
          task_id: 'task_one',
          reason_code: null,
          updated_at: '2026-08-01T00:00:03Z',
        },
        null,
        null,
      ),
    ],
    note: 'a done attempt ended its work; acceptance is judged by evidence',
  },
  operations: {
    available: true,
    recent: [
      {
        event_id: 'evt_2',
        ts: '2026-08-01T00:00:07Z',
        kind: 'review',
        provider: 'claude',
        exit: 1,
        status: 'unresolved',
        selected_model: null,
        served_model: null,
      },
      {
        event_id: 'evt_1',
        ts: '2026-08-01T00:00:05Z',
        kind: 'call',
        provider: 'codex',
        exit: 0,
        status: 'success',
        selected_model: 'gpt-test',
        served_model: 'gpt-served',
      },
    ],
    omitted: 0,
  },
  reviews: { outcomes: 0, passed: 0, failed: 0, unknown: 0, seat_answers: 1 },
  delegations: [
    { id: 'del_live', provider: 'codex', role: 'worker', live: true, started_at: '2026-08-01T00:00:00Z' },
    { id: 'del_dead', provider: 'claude', role: 'worker', live: false, started_at: '2026-08-01T00:00:01Z' },
  ],
  acceptance: {
    available: true,
    total: 2,
    counts: { verified: 1, missing: 1 },
    complete: false,
    criteria: [
      { id: 'plan-task-review_task', status: 'missing', source: 'plan-task', text: 'Plan task review_task is admitted.' },
      { id: 'plan-task-ready_task', status: 'verified', source: 'plan-task', text: 'Plan task ready_task is admitted.' },
    ],
    omitted: 0,
    note: 'worker completion is not acceptance',
  },
  attention: {
    actionable: 1,
    items: [
      {
        priority: 'P1',
        code: 'unresolved-artifacts',
        summary: '1 unresolved artifact outcome(s)',
        command: 'oms artifact-index --repo . unresolved',
      },
    ],
    omitted: 0,
    next: [{ id: 'initialize_project', authority: 'repo_write', command: 'oms init' }],
  },
  fleet: {},
}

// The same snapshot when the state source could not be collected: every section
// derived from state is null, which means "not known", never "none".
export const DEGRADED = {
  ...DASHBOARD,
  collection: { ok: false, sources: { state: 'degraded', artifacts: 'ok', attempts: 'invalid lifecycle contract' } },
  goal: null,
  scope: null,
  task: null,
  plan: null,
  attempts: { available: false, error: 'invalid lifecycle contract' },
  reviews: null,
  delegations: null,
  acceptance: null,
  attention: null,
}
