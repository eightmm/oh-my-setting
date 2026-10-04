# OMS terminal control panel

Run `oms` in a terminal to open the human control panel. `oms --help` and bare
`oms` with redirected input/output retain the command help; agents keep using
the existing intent-specific tools. The panel uses the same OMS records and
does not create another task store, replace provider authentication, or install
software.

```bash
oms panel --repo .                         # auto layout
oms panel --repo . --layout inline         # portable menu, no tmux needed
oms panel --repo . --layout split          # requires an already-installed tmux
oms panel --repo . --json                  # read-only status/availability
oms panel --repo . --launch codex          # native interactive session
oms panel --repo . --launch claude --resume SESSION_ID
oms panel --repo . --launch codex --model MODEL --dry-run
```

`auto` chooses split on POSIX when tmux is installed and the terminal is not
already inside tmux. Otherwise it uses inline. `split` creates an independent
`oms-panel-*` tmux session with a control window and a dashboard refreshed every
five seconds. Opening a provider adds a native conversation window with its own
dashboard sidebar. Existing provider windows are preserved; use tmux's window
picker (`Ctrl-b w` with its default bindings) to move between them. Custom tmux
key bindings still apply. Detaching with `q` in the control pane or `Ctrl-b d`
preserves the sessions and their running work. The returned session name can be
reattached with tmux; no existing tmux session or user configuration is replaced.
Only failed construction of the new OMS session is rolled back.

Inline suspends the menu while the native CLI owns the terminal, and returns
when the CLI exits. Its status refreshes on return or Refresh; there is no
permanent sidebar in this fallback. A native CLI still requires its own working
installation and login. `installed` means executable found on PATH, not proven
authentication or account/model availability. Missing providers do not block
read-only status. `--dry-run` starts no process and writes no state.

## Menu

| Action | Behavior |
| --- | --- |
| Codex / Claude | Start a top-level native conversation in the selected repository |
| Resume | Select a provider and an exact native session ID; never choose the latest implicitly |
| Main | Choose whose identity owns subsequent panel peer calls |
| Ask peer | One explicit read-only `consult` to the other provider |
| Implement | Scoped brief file plus mandatory verifier; `peer-delegate` returns an isolated patch |
| Review | Question plus mandatory verifier; `peer-review --gate` records the gate outcome |
| Details / Refresh | Read the shared dashboard; no provider call |
| Quit | Exit inline, or detach the owned split session without cancelling work |

The menu shows who calls whom before collecting the brief. Selecting Implement
authorizes that bounded call, not applying its patch or publishing it. The
verification command runs project code with the operator's permissions. No
permission-bypass flags, automatic patch application, commits, push, install,
automatic classification or provider fan-out are added.

## Both providers can orchestrate

A top-level Codex session can ask Claude to investigate, implement or review;
a top-level Claude session can do the reverse through the same OMS front doors.
The launcher exports the provider identity and places this checkout's `oms` on
PATH so both use the panel's code revision. It supplies scoped collaboration
instructions without replacing existing system/developer instructions or
native permissions. Claude receives an appended system prompt. Codex receives
a readiness prompt as its first user turn; that turn can use the account's
normal quota even when no implementation task has been entered.

The native main session owns the user's goal, worker brief, admission and
acceptance. Workers return findings/artifacts/patches. They cannot reopen the
panel as owners or recursively call peers: the panel preserves the existing
harness-child/delegation-depth checks. Exchanging roles means choosing another
top-level owner, not promoting a delegated child. Direct external CLIs remain
outside OMS's enforcement boundary.

Provider conversations remain native and separate. Shared OMS task, thread,
artifact and verification references carry cross-provider evidence; the panel
does not copy entire chat histories or inject user input into an already-running
native conversation. Opening a second main session does not serialize its file
edits; give implementation to isolated workers and coordinate owner edits.

`--json` returns schema 1, kind `oms-panel`, a nested `oms-dashboard`, provider
`installed` booleans and `split_available`. Exit 0 is successful collection, 1
is unavailable/degraded collection or a launch failure, 2 is invalid usage or a
worker attempting owner actions, and 130 is an interrupted launch. Native
launches propagate the native process exit. The coverage and unknown/acceptance
semantics are those of [the dashboard](DASHBOARD.md); native subagents that do
not write OMS records are not counted as managed workers.

## Development verification

The existing operator-tools suite exercises both directions through real OMS
consultation, isolated delegation, mechanical verification, review and artifact
writers with fixture provider executables. It checks the original checkout is
unchanged, recursive owner launch is rejected, exact session arguments and
identity reach the native process, and the terminal restores after an inline
session. POSIX PTY checks run when supported. These tests do not claim a paid
live model followed the bootstrap instructions or that login succeeded.
