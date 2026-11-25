// oms-panel: a read-only pane over `oms dashboard --json` for Claude Code.
//
// The only process this mod ever starts is that one read-only query. It writes
// no OMS state, sends nothing to Claude, and never runs a command that the
// projection names: suggested commands are drawn as text for the person to run.
import { TABS, bodyLines, headerLines, parseDashboard } from './view.js'

// The pane's id, used to open it and to recognize it when drawing
const PANE = 'oms-panel'
const COMMAND = 'oms-panel'
// How often an open pane re-reads the projection
const REFRESH_MS = 15_000
// Give up on one query well before the 30 second default
const RUN_TIMEOUT_MS = 20_000
// Width to ask for beside the transcript; the tab row needs about 46 cells
const PANE_COLUMNS = 64

// What the pane shows. All of it can be lost on a reload: the next refresh rebuilds it.
let tab = 'now'
let snapshot = { status: 'empty' }
let isRunning = false
let timer = null
// True once the pane was drawn after the latest refresh. A pane nobody draws
// (closed, or hidden behind another pane) stops the timer on its next tick.
let wasDrawn = false

const TONES = {
  error: { color: 'red' },
  warn: { color: 'yellow' },
  ok: { color: 'green' },
  dim: { dimColor: true },
}

function stopTimer() {
  if (timer) timer.cancel()
  timer = null
}

// Run the one read-only query and keep its result for the next draw.
async function refresh($) {
  if (isRunning) return
  isRunning = true
  try {
    let repo = ''
    try {
      const cwd = await $.session.cwd()
      if (typeof cwd === 'string') repo = cwd
    } catch {
      // Fall back to the directory the query starts in
    }
    const result = repo
      ? await $.process.run(['oms', 'dashboard', '--repo', repo, '--json'], { cwd: repo, timeoutMs: RUN_TIMEOUT_MS })
      : await $.process.run(['oms', 'dashboard', '--json'], { timeoutMs: RUN_TIMEOUT_MS })
    snapshot = parseDashboard(result)
  } catch {
    // The reason is not shown: it can quote paths or output from the failed start
    snapshot = { status: 'error', message: 'the oms command could not be started or timed out; is oh-my-setting installed?' }
  } finally {
    isRunning = false
  }
  $.ui.invalidate('ui.render')
}

export function register(on) {
  // Runs before the first prompt, and again after a reload
  on('session.start', async ($, e, next) => {
    try {
      // immediate: the pane can be opened while Claude is working
      await $.command.register({
        name: COMMAND,
        description: 'Open OMS to see current work and verified results',
        immediate: true,
      })
    } catch {
      // A taken name leaves the session without the command, not without OMS
    }
    return next(e)
  })

  // Runs when the person types /oms-panel
  on('command.run', { command: COMMAND }, async ($) => {
    tab = 'now'
    await $.ui.open({ id: PANE, title: 'OMS', focus: true, closeOnEscape: true, columns: PANE_COLUMNS })
    wasDrawn = true
    // Read outside this event, so the command returns at once
    $.clock.after(0, () => refresh($))
    if (!timer) {
      timer = $.clock.every(REFRESH_MS, () => {
        if (!wasDrawn) return stopTimer()
        wasDrawn = false
        return refresh($)
      })
    }
    // Print nothing in the transcript
    return {}
  })

  // Runs when a pane is about to close: stop reading for a pane nobody sees
  on('ui.close', async ($, e, next) => {
    if (e.id === PANE) stopTimer()
    return next(e)
  })

  // Runs each time Claude Code draws a pane
  on('ui.render', { component: 'Pane' }, async ($, e, next) => {
    // Leave other mods' panes alone
    if (e.requestId !== PANE) return next(e)
    wasDrawn = true
    const { Box, Text, Button } = $.ui.resolve(e)

    const textLine = (item) =>
      Text({
        wrap: 'truncate-end',
        ...(TONES[item.tone] ?? {}),
        children: [item.text],
      })

    const tabs = TABS.map((item) =>
      Button({
        key: 'tab-' + item.id,
        label: item.label,
        hotkey: item.hotkey,
        plain: true,
        // Dim the tabs that aren't open
        dimColor: tab !== item.id,
        onPress: () => {
          tab = item.id
          $.ui.invalidate('ui.render')
        },
      }),
    )

    const lines = [
      ...headerLines(snapshot),
      ...(snapshot.status === 'ready' ? bodyLines(snapshot.report, tab) : []),
    ]

    return Box({
      flexDirection: 'column',
      children: [
        // First row: the tabs. It stays short, because the pane's close mark
        // is drawn over the last cell of this row in the terminal.
        Box({ flexDirection: 'row', columnGap: 2, children: tabs }),
        Box({
          flexDirection: 'row',
          columnGap: 2,
          children: [
            Button({
              key: 'refresh',
              label: 'Refresh',
              hotkey: 'r',
              plain: true,
              onPress: () => refresh($),
            }),
            Text({
              dimColor: true,
              wrap: 'truncate-end',
              children: ['read-only · ' + (timer ? 'auto ' + REFRESH_MS / 1000 + 's' : 'auto paused')],
            }),
          ],
        }),
        ...lines.map(textLine),
      ],
    })
  })
}
