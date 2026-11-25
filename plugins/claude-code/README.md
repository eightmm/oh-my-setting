# OMS for Claude Code

Claude Code reads MCP tool results as text and does not render the MCP Apps
HTML that the Codex and ChatGPT Work panels use. This plugin is the Claude Code
counterpart: a [mod](https://code.claude.com/docs/en/plugins/mods/overview)
that draws the same recorded OMS state in a native pane beside the transcript.

It is a renderer over one existing contract, the schema-1 `oms-dashboard`
projection described in [the dashboard guide](../../docs/DASHBOARD.md). It adds
no OMS state, verb, MCP tool or store.

## Use

Requires Claude Code v2.1.287 or later and an oh-my-setting install that puts
`oms` on `PATH`.

```bash
claude --plugin-dir plugins/claude-code
```

At the Claude Code prompt, run `/oms-panel` to open **OMS**. The pane opens on **Now**. Claude Code chooses its placement for the available terminal width.

| Key | Action |
| --- | --- |
| `1` Now | Project goal, verification criteria, current task and open attempts |
| `2` Work | Plan states, recent open attempts, delegated work and scope |
| `3` Results | Acceptance criteria and recorded reviews |
| `4` Activity | Recent outcomes, operations, diagnostics and suggested commands |
| `r` | Read the projection again |
| Esc | Close the pane |

Open attempts include work waiting for review or blocked on a dependency.

An open pane reads again every 15 seconds. A pane that is closed or no longer
drawn stops reading by its next tick, and `/oms-panel` starts it again.

For use across projects, add the repository in Claude Code with
`/plugin marketplace add eightmm/oh-my-setting`. In `/plugin` → **Discover**,
select **oms-panel** from **oh-my-setting** and choose **user** scope.
The separate **oms-skills** entry provides the optional skills.

## What it does and does not do

- The only process it starts is `oms dashboard --repo <session directory> --json`.
- It never runs a command named in the projection. Next actions are drawn as
  text for the person to run.
- It sends nothing to Claude and prints nothing in the transcript.
- Labels are untrusted: control and format characters are drawn as `?`, and
  every label is bounded.
- A missing value is drawn as `unknown`, and a section whose source failed is
  drawn as `UNAVAILABLE`, never as zero or none.
- stderr of the query is never drawn. A failed start, a non-JSON answer, or a
  contract other than schema 1 `oms-dashboard` shows `OMS UNAVAILABLE` with
  the reason and no data.
- Its hooks have no `.catch` handler on purpose: a hook that fails is skipped,
  so the session and the pane's close keep working without it.

Opening the pane does not select a Main, start a run, or change execution
ownership. Control actions stay with the existing OMS commands.

## Check

```bash
claude plugin validate plugins/claude-code
claude plugin test plugins/claude-code
```

The tests need no session, sign-in or network. They cover the projection
parser, label cleaning, each tab against a clean and a degraded fixture, the
exact query argv, the element tree for the terminal and the Desktop app, and
the refresh timer. Hosted CI does not run them, because it has no Claude Code.

## Evidence

| Claim | Evidence |
| --- | --- |
| Hooks module loads its events and calls | `claude plugin validate`, Claude Code 2.1.331 |
| Element tree is valid for terminal and Desktop | `claude plugin test` fixture mounts, Claude Code 2.1.331 |
| Pane placement, colors, focus and hotkeys in a real session | not verified |
| Query against a real `oms` build and repository | not verified; the fixture is hand-written from the documented schema |
| Timer stop on Esc through the `ui.close` event | not verified; tests cover the not-drawn stop only |
| VS Code extension, WSL, SSH and cloud sessions | not verified |

Live check, in a repository with OMS state:

1. `claude --plugin-dir plugins/claude-code`, then `/plugin` lists `oms-panel` as an active mod.
2. `/oms-panel` opens the pane, and the header shows this repository, branch and HEAD.
3. Keys `1` to `4` switch tabs and `r` refreshes. The tab row fits the pane width.
4. The pane content matches `oms dashboard --repo .` for the same snapshot.
5. With `oms` off `PATH`, the pane shows `OMS UNAVAILABLE` and the session keeps working.
6. After Esc, no further `oms dashboard` process starts.
