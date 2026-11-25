import { expect, mock, test } from 'claude-code/testing'

import { MAX_STDOUT_CHARS, bodyLines, clean, headerLines, parseDashboard, shown } from '../hooks/view.js'
import { DASHBOARD, DEGRADED } from './fixture.ts'

// What Claude Code passes to a ui.render hook for this pane, apart from the app
const PANE = {
  plugin: 'oms-panel',
  component: 'Pane',
  requestId: 'oms-panel',
  viewport: { columns: 160, rows: 40 },
  props: {
    title: 'OMS',
    isFocused: true,
    bodyColumns: 64,
    placement: 'dock',
    scroll: { offset: 0, bodyRows: 30 },
    view: {},
  },
} as const

const ok = (report: unknown, exitCode = 0) => ({
  value: { exitCode, stdout: JSON.stringify(report), stderr: 'private stderr that must never be drawn' },
})

type Reply = (call: number) => unknown

// Answer every mods API call the mod makes, and record each process it starts.
function harness(on: any, reply: Reply) {
  const clock = mock.clock(on)
  const runs: Array<{ argv: string[]; init: Record<string, unknown> }> = []
  const opened: any[] = []
  on('session.cwd', () => ({ value: '/work' }))
  on('ui.open', (_$: unknown, e: any) => {
    opened.push(e)
    return { value: { isPlaced: true } }
  })
  on('process.run', (_$: unknown, e: any) => {
    runs.push({ argv: e.argv, init: e.init })
    return reply(runs.length)
  })
  // Stands for what Claude Code would draw in a pane this mod leaves alone
  on('ui.render', () => ({ type: 'Text', props: {}, children: ['drawn by Claude Code'] }))
  return { clock, runs, opened }
}

const texts = (lines: Array<{ text: string }>) => lines.map((item) => item.text)

test('parseDashboard accepts a clean or degraded snapshot and refuses everything else', async () => {
  expect(parseDashboard(ok(DASHBOARD).value).status).toBe('ready')
  expect(parseDashboard(ok(DEGRADED, 1).value).status).toBe('ready')

  const usage = parseDashboard({ exitCode: 2, stdout: '', stderr: 'usage: private detail' })
  expect(usage).toEqual({ status: 'error', message: 'oms dashboard exited with status 2' })
  expect(parseDashboard(undefined).status).toBe('error')
  expect(parseDashboard({ exitCode: 0, stdout: 'not json', stderr: '' }).message).toBe('oms dashboard did not print JSON')
  expect(parseDashboard(ok({ ...DASHBOARD, schema: 2 }).value).message).toMatch(/^unsupported contract \(schema 2, kind oms-dashboard\)/)
  expect(parseDashboard(ok({ ...DASHBOARD, kind: 'oms-panel' }).value).status).toBe('error')
  expect(parseDashboard(ok([]).value).status).toBe('error')
  const huge = { exitCode: 0, stdout: ' '.repeat(MAX_STDOUT_CHARS + 1), stderr: '' }
  expect(parseDashboard(huge).message).toMatch(/larger than 4 MiB/)
})

test('labels are cleaned and unknown is never shown as zero', async () => {
  // A terminal escape, a bidi override and a zero-width mark all become "?"
  expect(clean('a\u001b[31mb‮c​d\ne')).toBe('a?[31mb?c?d?e')
  expect(clean('abcdef', 4)).toBe('abc…')
  expect(shown(null)).toBe('unknown')
  expect(shown(undefined)).toBe('unknown')
  expect(shown('')).toBe('unknown')
  expect(shown({})).toBe('unknown')
  expect(shown(0)).toBe('0')
  // A line is bounded as a whole, however many labels it joins
  const wide = { ...DASHBOARD, scope: { allowed: Array(500).fill('a'.repeat(80)), forbidden: [], source: 'task' } }
  const scope = texts(bodyLines(wide, 'overview')).find((text) => text.startsWith('scope:'))
  expect(Array.from(scope ?? '').length).toBe(2000)

  const recent = texts(bodyLines(DASHBOARD, 'agents')).find((text) => text.includes('att_one'))
  expect(recent).toMatch(/tok unknown cost_microusd unknown/)
})

test('each tab draws the recorded state and marks what it cannot know', async () => {
  const overview = texts(bodyLines(DASHBOARD, 'overview'))
  expect(overview).toContain('goal: operator fixture (plan)')
  expect(overview).toContain('scope: unbounded (none recorded)')
  expect(overview).toContain('task: none active')
  expect(overview).toContain('plan: 2 task(s)  ready=1 review=1  actionable=ready_task')
  expect(overview).toContain('attempts (OMS lifecycle): 2 total, 1 active  running=1 cancelled=1')
  expect(overview).toContain('acceptance: 1/2 verified  verified=1 missing=1')
  expect(overview).toContain('next decision: initialize_project (repo_write)')
  // An unknown tab falls back to the overview
  expect(texts(bodyLines(DASHBOARD, 'nope'))).toEqual(overview)

  const agents = bodyLines(DASHBOARD, 'agents')
  expect(texts(agents)).toContain('  att_two running claude/panel task=task_two role=lead model=model-test at=worktree:lead-one 2026-08-01T00:00:09Z')
  expect(agents.find((item) => item.text.includes('del_dead'))).toMatchObject({ tone: 'error' })
  expect(texts(agents)).toContain('  claude review model=unknown served=unreported exit=1 unresolved')

  const evidence = bodyLines(DASHBOARD, 'evidence')
  expect(evidence.find((item) => item.text.includes('review_task'))).toMatchObject({ tone: 'warn' })
  expect(evidence.find((item) => item.text.includes('ready_task'))).toMatchObject({ tone: 'ok' })
  expect(texts(evidence)).toContain('review gate: none recorded in window (not a pass)  seat answers=1')

  const attention = texts(bodyLines(DASHBOARD, 'attention'))
  expect(attention).toContain('  P1 unresolved-artifacts: 1 unresolved artifact outcome(s)')
  expect(attention).toContain('     next: oms artifact-index --repo . unresolved')

  // A null section is "not known": it is never drawn as none or zero
  const header = texts(headerLines({ status: 'ready', report: DEGRADED }))
  expect(header).toContain('COLLECTION FAILED state: degraded')
  expect(header).toContain('COLLECTION FAILED attempts: invalid lifecycle contract')
  const degraded = texts(bodyLines(DEGRADED, 'overview'))
  expect(degraded).toContain('goal: none recorded')
  expect(degraded).toContain('scope: unavailable (runtime projection)')
  expect(degraded).toContain('task: UNAVAILABLE or invalid')
  expect(degraded).toContain('plan: UNAVAILABLE or invalid')
  expect(degraded).toContain('attempts: UNAVAILABLE (lifecycle stream invalid)')
  expect(degraded).toContain('acceptance: UNAVAILABLE')
  expect(degraded).toContain('attention: UNAVAILABLE')
  expect(texts(bodyLines(DEGRADED, 'agents'))).toContain('delegations: UNAVAILABLE')
  expect(texts(bodyLines(DEGRADED, 'evidence'))).toContain('review gate: UNAVAILABLE')
})

test('session start registers /oms-panel so it can open while Claude works', async ($, on) => {
  const registered: unknown[] = []
  on('session.start', () => ({ cwd: '/work' }))
  on('command.register', (_$: unknown, e: any) => {
    registered.push(e)
    return { value: undefined }
  })
  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  expect(registered.length).toBe(1)
  expect(registered[0]).toMatchObject({ name: 'oms-panel', immediate: true })
})

test('/oms-panel opens the pane and runs only the read-only dashboard query', async ($, on) => {
  const { clock, runs, opened } = harness(on, () => ok(DASHBOARD))

  const answer = await $.command.run({ command: 'oms-panel', args: '' })
  // Nothing is printed in the transcript, and the query has not delayed the command
  expect(answer).toEqual({})
  expect(runs.length).toBe(0)
  expect(opened.length).toBe(1)
  expect(opened[0]).toMatchObject({ id: 'oms-panel', title: 'OMS', closeOnEscape: true })

  await clock.settle()
  expect(runs.length).toBe(1)
  expect(runs[0].argv).toEqual(['oms', 'dashboard', '--repo', '/work', '--json'])
  expect(runs[0].init).toMatchObject({ cwd: '/work', timeoutMs: 20000 })

  // The same tree is valid in the terminal and in the Desktop app
  for (const surface of ['terminal', 'desktop'] as const) {
    const ui = await $.ui.mount({ ...PANE, surface })
    expect(await ui.find({ type: 'Text', text: 'repo=demo  main@b6c3a2aa8c39  snapshot=2026-10-04T00:51:30Z' })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: 'goal: operator fixture (plan)' })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /^read-only · auto 15s$/ })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /private stderr/ })).toBeUndefined()

    await ui.press({ key: 'tab-agents' })
    expect(await ui.find({ type: 'Text', text: /^delegation: del_dead .*ORPHAN \(dead pid\)$/ })).toBeDefined()
    await ui.press({ key: 'tab-evidence' })
    expect(await ui.find({ type: 'Text', text: 'worker completion is not acceptance' })).toBeDefined()
    await ui.press({ key: 'tab-attention' })
    expect(await ui.find({ type: 'Text', text: '     next: oms artifact-index --repo . unresolved' })).toBeDefined()
    await ui.press({ key: 'tab-overview' })
    expect(await ui.find({ type: 'Text', text: 'task: none active' })).toBeDefined()
    await ui.unmount()
  }

  // Tabs redraw from the kept snapshot; only Refresh reads again
  expect(runs.length).toBe(1)
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await ui.press({ key: 'refresh' })
  expect(runs.length).toBe(2)
  await ui.unmount()

  // The suggested commands were drawn as text. Nothing but the query ever ran.
  for (const run of runs) expect(run.argv.slice(0, 2)).toEqual(['oms', 'dashboard'])
})

test('a degraded snapshot names the failed sources', async ($, on) => {
  const { clock } = harness(on, () => ok(DEGRADED, 1))
  await $.command.run({ command: 'oms-panel', args: '' })
  await clock.settle()

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await ui.find({ type: 'Text', text: 'COLLECTION FAILED state: degraded' })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: 'DEGRADED: unavailable sections are not known, not empty' })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: 'plan: UNAVAILABLE or invalid' })).toBeDefined()
})

test('a missing oms command or an unknown contract is reported, not guessed', async ($, on) => {
  const replies: Reply = (call) =>
    call === 1 ? { deny: 'spawn failed with a private path' } : ok({ ...DASHBOARD, schema: 2 })
  const { clock } = harness(on, replies)
  await $.command.run({ command: 'oms-panel', args: '' })
  await clock.settle()

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await ui.find({ type: 'Text', text: /^OMS UNAVAILABLE: the oms command could not be started or timed out/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /private path/ })).toBeUndefined()
  expect(await ui.find({ type: 'Text', text: /^goal:/ })).toBeUndefined()

  await ui.press({ key: 'refresh' })
  expect(await ui.find({ type: 'Text', text: /^OMS UNAVAILABLE: unsupported contract \(schema 2, kind oms-dashboard\)/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /^goal:/ })).toBeUndefined()
})

test('the pane reads again while it is drawn and stops once nobody draws it', async ($, on) => {
  const { clock, runs } = harness(on, () => ok(DASHBOARD))
  await $.command.run({ command: 'oms-panel', args: '' })
  await clock.settle()
  expect(runs.length).toBe(1)

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await clock.advance(15000)
  expect(runs.length).toBe(2)
  await ui.unmount()

  // With the drawing gone the timer cancels itself within two ticks
  await clock.advance(30000)
  const settled = runs.length
  await clock.advance(120000)
  expect(runs.length).toBe(settled)

  // Opening the pane again starts reading again
  await $.command.run({ command: 'oms-panel', args: '' })
  await clock.settle()
  expect(runs.length).toBe(settled + 1)
})

test('a pane drawn before any read says so and reads on r', async ($, on) => {
  const { runs } = harness(on, () => ok(DASHBOARD))
  // As after a reload of the module: the pane is still open, the snapshot is gone
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await ui.find({ type: 'Text', text: 'no snapshot yet: press r to read oms dashboard' })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: 'read-only · auto paused' })).toBeDefined()
  expect(runs.length).toBe(0)
  await ui.press({ key: 'refresh' })
  expect(runs.length).toBe(1)
  expect(await ui.find({ type: 'Text', text: 'goal: operator fixture (plan)' })).toBeDefined()
})

test("another mod's pane is left alone", async ($, on) => {
  harness(on, () => ok(DASHBOARD))
  const ui = await $.ui.mount({ ...PANE, requestId: 'someone-else', surface: 'terminal' })
  expect(await ui.find({ type: 'Text', text: 'drawn by Claude Code' })).toBeDefined()
  expect(await ui.find({ key: 'refresh' })).toBeUndefined()
})
