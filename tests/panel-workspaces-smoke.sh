#!/usr/bin/env bash
set -euo pipefail
# A new canonical suite: no existing test exercises the workspace session, its key table or its controller.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-workspaces.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
OMS_WS_TEST_ROOT="$ROOT" OMS_WS_TEST_TMP="$TMP" python3 - <<'PY'
import ast
import faulthandler
import json
import os
from pathlib import Path
import pty
import select
import shlex
import shutil
import subprocess
import sys
import time

root = Path(os.environ.pop('OMS_WS_TEST_ROOT'))
tmp = Path(os.environ.pop('OMS_WS_TEST_TMP'))
for key in list(os.environ):
    if key.startswith(('OMS_', 'CLAUDE_CODE_', 'CODEX_')) or key in ('TMUX', 'TMUX_PANE', 'CLAUDECODE'):
        os.environ.pop(key, None)
sys.path.insert(0, str(root / 'scripts/lib'))
import panel_terminals as terminals
import panel_workspaces as workspaces
import panel_view
import terminal_panel
from unittest.mock import patch

repo = tmp / r'repo a$b and literal \$ with space #hash'
repo.mkdir()
home = tmp / 'home'
home.mkdir()
os.environ.update(XDG_STATE_HOME=str(tmp / 'state'), HOME=str(home), SHELL='/bin/bash', TERM='xterm')
faulthandler.dump_traceback_later(100, exit=True)
checked = 0
socket = str(tmp / 'tmux-socket')
marker = tmp / 'root-f6-fired'
client = master = None
tmux_bin = shutil.which('tmux')
native = False

def check(condition, message):
    global checked
    assert condition, message
    checked += 1

baseline_menu = panel_view.menu_rows(90, 24, True, False, False)
check(baseline_menu == [
    '╭─ Actions · press a key ────────────────────────────────────────────────────────────────╮',
    '│ START  [1] Codex [2] Claude [3] Resume [t] Task                                        │',
    '│ OTHER  [o] Rooms [9] Refresh [?] More [q] Quit                                         │',
    '╰────────────────────────────────────────────────────────────────────────────────────────╯'],
      'nonexpanded menu changed')
expanded_menu = panel_view.menu_rows(90, 24, True, True, False)
check('Terminals and jobs' in '\n'.join(expanded_menu), 'expanded terminal command missing')
expected_keys = [workspaces._key_command(repo, 'term-' + 'a' * 32, str(tmp / r'dollar $ literal \$ ${x} $_y $1 $$z end$'), action)
                 for _, action in workspaces.KEYS]
for expected in expected_keys:
    displayed = [terminals.tmux_display(token) for token in expected]
    check(terminals._same_tmux_tokens(displayed, expected), 'tmux key dollar-display escaping rejected')
    check(not terminals._same_tmux_tokens([token.replace('$', r'\$') for token in expected], expected)
          and not terminals._same_tmux_tokens(expected, expected),
          'ambiguous raw or all-dollar key display accepted')
    check(not terminals._same_tmux_tokens(displayed + ['extra'], expected),
          'extra workspace key payload accepted')

# The ordinary-shell entry resolves only the exact caller pane/session and never creates a main.
caller_env = dict(os.environ)
for name in ('TMUX', 'TMUX_PANE', 'OMS_PANEL_SESSION', 'OMS_ROOM_ID'):
    os.environ.pop(name, None)
os.environ.update(TMUX='fixture,123,0', TMUX_PANE='%7')
seen = []
choices = ['s', 'q']
def fake_run(argv, **kwargs):
    check(argv == ['tmux', 'display-message', '-p', '-t', '%7', '#{session_id}'],
          'menu used an unexpected tmux query')
    return subprocess.CompletedProcess(argv, 0, '$42\n', '')
with patch.object(terminal_panel, 'read_choice', side_effect=lambda *a, **k: choices.pop(0) if choices else 'q'), \
     patch.object(terminal_panel.subprocess, 'run', side_effect=fake_run), \
     patch.object(workspaces, 'open_workspace', side_effect=lambda repo_arg, session: seen.append((repo_arg, session))):
    check(terminal_panel.interactive(repo) == 0, 'menu did not return after q')
check(seen == [(repo, '$42')], 'menu did not open workspace for exact caller session: ' + repr(seen))

os.environ.pop('TMUX', None)
choices = ['s', 'g', 'q']
previouses = []
with patch.object(terminal_panel, 'read_choice', side_effect=lambda *a, **k: (previouses.append(a[2]), choices.pop(0) if choices else 'q')[-1]), \
     patch.object(workspaces, 'open_workspace', side_effect=AssertionError('workspace opened without tmux')), \
     patch.object(terminal_panel, 'open_native', side_effect=AssertionError('menu started a model')):
    check(terminal_panel.interactive(repo) == 0, 'non-tmux refusal did not remain in menu')
check(previouses[1].startswith('error: terminals and jobs require a tmux pane'),
      'non-tmux caller was not refused with bounded feedback')

os.environ.update(TMUX='fixture,123,0', TMUX_PANE='%8')
choices = ['s', 'q']
previouses = []
def foreign_run(argv, **kwargs):
    return subprocess.CompletedProcess(argv, 0, '$99\n', '')
with patch.object(terminal_panel, 'read_choice', side_effect=lambda *a, **k: (previouses.append(a[2]), choices.pop(0) if choices else 'q')[-1]), \
     patch.object(terminal_panel.subprocess, 'run', side_effect=foreign_run), \
     patch.object(workspaces, 'open_workspace', side_effect=ValueError('caller pane is not in the origin session')), \
     patch.object(terminal_panel, 'open_native', side_effect=AssertionError('menu started a model')):
    check(terminal_panel.interactive(repo) == 0, 'foreign caller refusal did not return to menu')
check(previouses[1].startswith('error: caller pane is not in the origin session'),
      'foreign caller was not refused with bounded feedback')
os.environ.clear()
os.environ.update(caller_env)


def tmux(*args):
    done = subprocess.run([tmux_bin, '-S', socket, *args], stdin=subprocess.DEVNULL, capture_output=True,
                          text=True, timeout=8)
    return done.stdout.replace('\r', '').strip()


def front(*args, pane=None, session_env=True):
    """The shipped front door, as a process whose caller pane is given by TMUX_PANE."""
    env = dict(os.environ)
    env.pop('TMUX_PANE', None)
    if session_env:
        env['TMUX'] = socket + ',' + server_pid + ',0'
    if pane:
        env['TMUX_PANE'] = pane
    return subprocess.run(['bash', str(root / 'scripts/oms'), 'panel', 'terminal', *args], env=env,
                          stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30)


def controls(pane, session=None):
    return front('controls', '--repo', str(repo), '--session', session or home_session, pane=pane)


def drain(seconds=.5):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if select.select([master], [], [], .05)[0]:
            try:
                os.read(master, 65536)
            except OSError:
                return


def press(data):
    os.write(master, data)
    drain(.7)


def screen(pane):
    return tmux('capture-pane', '-p', '-t', pane, '-S', '-200')


def wait_for(pane, text, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if text in screen(pane):
            return
        time.sleep(.05)
    raise AssertionError('missing %r on %s: %s' % (text, pane, screen(pane)))


def type_to(pane, *lines, wait=.25):
    for line in lines:
        tmux('send-keys', '-t', pane, '-l', line)
        tmux('send-keys', '-t', pane, 'Enter')
        time.sleep(wait)


def client_session():
    return tmux('list-clients', '-F', '#{session_name}')


def workspace_rows():
    rows = []
    for ident in terminals.identifiers(repo, 'workspaces'):
        with terminals.Store(repo, 'workspaces', ident) as store:
            rows.append(store.read())
    return rows


try:
    if not tmux_bin:
        print('BLOCKED native tmux+PTY: tmux unavailable')
    else:
        env = dict(os.environ, OMS_FAKE_AUTH='secret', CLAUDECODE='1', CODEX_THREAD_ID='thread')
        probe = subprocess.run([tmux_bin, '-S', socket, '-f', '/dev/null', 'new-session', '-d', '-s', 'origin',
                                '-x', '90', '-y', '28', 'cat -v'], env=env, stdin=subprocess.DEVNULL,
                               capture_output=True, text=True)
        native = probe.returncode == 0
        if not native:
            print('BLOCKED native tmux+PTY: ' + probe.stderr.strip())
    if native:
        ast.parse((root / 'scripts/lib/panel_workspaces.py').read_text(), feature_version=(3, 9))
        check(True, 'Python 3.9 grammar')
        server_pid, home_session, origin_pane, origin_window = tmux(
            'display-message', '-p', '-t', 'origin', '#{pid}\t#{session_id}\t#{pane_id}\t#{window_id}').split('\t')
        tmux('set-option', '-t', home_session, '@oms_panel_repo', str(repo))
        for index, home in enumerate((r'/tmp/d $ x #', r'/tmp/${x} $_y $1 $$z end$', r'/tmp/lit \$b \\$c')):
            table = 'dollar%d' % index
            native_keys = [(key, workspaces._key_command(repo, 'term-' + 'b' * 32, home, action)) for key, action in workspaces.KEYS]
            for key, command in native_keys:
                tmux('bind-key', '-T', table, key, *command)
            bound = [shlex.split(line) for line in tmux('list-keys', '-T', table).splitlines()]
            for key, command in native_keys:
                want = ['bind-key', '-T', table, key, *command]
                check(sum(terminals._same_tmux_tokens(actual, want) for actual in bound) == 1,
                      'native list-keys display rejected for ' + home)
                check(not any(terminals._same_tmux_tokens(actual, ['bind-key', '-T', table, key, *command[:-1], command[-1] + ' ']) for actual in bound),
                      'native list-keys display too loose for ' + home)
        # What bind_main_keys installs for a main: a root key tied to the repository marker.
        tmux('bind-key', '-n', 'F6', 'if-shell', '-F', '#{@oms_panel_repo}', 'run-shell "touch %s"' % marker, 'send-keys F6')
        tmux('new-session', '-d', '-s', 'foreign', '-x', '90', '-y', '28', 'cat -v')
        foreign_pane = tmux('display-message', '-p', '-t', 'foreign', '#{pane_id}')
        pid, master = pty.fork()
        if pid == 0:
            os.execvp(tmux_bin, [tmux_bin, '-S', socket, '-f', '/dev/null', 'attach', '-t', 'origin'])
        client = pid
        drain(1)
        os.environ['TMUX'] = socket + ',' + server_pid + ',0'
        before = {'pid': tmux('display-message', '-p', '-t', origin_pane, '#{pane_pid}'),
                  'globals': tmux('show-options', '-g'), 'root': tmux('list-keys', '-T', 'root'),
                  'windows': tmux('list-windows', '-t', home_session, '-F', '#{window_id}:#{window_panes}')}
        check(client_session() == 'origin', 'fixture client did not attach')

        # Refusals before anything exists.
        check(controls(origin_pane, '$99').returncode == 1, 'unknown origin session accepted')
        check(controls(foreign_pane).returncode == 1, 'caller from a different session accepted')
        check(controls(None).returncode == 1, 'missing caller pane accepted')
        check(front('controls', '--repo', str(repo), '--session', tmux('display-message', '-p', '-t', 'foreign', '#{session_id}'),
                    pane=foreign_pane).returncode == 1, 'session not owned by the repository accepted')
        check(not workspace_rows() and tmux('list-sessions', '-F', '#{session_name}').split() == ['foreign', 'origin'],
              'refusals created workspace state')

        opened = controls(origin_pane)
        check(opened.returncode == 0, 'controls front door failed: ' + opened.stderr)
        public = json.loads(opened.stdout)
        check(public['kind'] == 'workspace' and public['state'] == 'running' and public['capabilities'] == {'focus': True, 'forget': False},
              'public workspace row')
        check(str(repo) not in opened.stdout and str(tmp / 'state') not in opened.stdout, 'public row leaked a path')
        (record,) = workspace_rows()
        ws, control = record['session'], record['pane']
        check(public['id'] == record['id'] and record['id'].startswith('term-') and record['session'] != home_session,
              'workspace identity')
        check(client_session().startswith('oms_ws_'), 'focus did not move the client to the workspace')
        check(tmux('display-message', '-p', '-t', ws, '#{pane_id}') == control, 'control pane is not focused')
        wait_for(control, 'Terminals and jobs')
        wait_for(control, 'F4 controls   F5 back to origin')
        check('u refresh' in screen(control), 'refresh key not advertised')
        check(not (repo / '.oms').exists(), 'workspace wrote shared repository state')

        # Three ordinary shells, one job, explicit actions; all through the real control pane.
        for name in ('alpha', 'beta', 'gamma'):
            type_to(control, 'o', name)
            wait_for(control, 'Shell opened.')
        type_to(control, 'u')
        wait_for(control, '3 shown')
        shells = terminals.list_terminals(repo)
        check(len(shells) == 3 and all(s['state'] == 'running' for s in shells), 'three shells are not running')
        rows = []
        for ident in terminals.identifiers(repo, 'terminals'):
            with terminals.Store(repo, 'terminals', ident) as store:
                rows.append(store.read())
        check({r['session'] for r in rows} == {ws}, 'shells escaped the workspace session')
        check(tmux('list-windows', '-t', home_session, '-F', '#{window_id}:#{window_panes}') == before['windows'],
              'origin windows changed')
        check(len(tmux('list-windows', '-t', ws, '-F', '#{window_id}').split()) == 4, 'workspace windows')

        type_to(control, 'r', "python3 -c 'import time; print(\"JOBREADY\", flush=True); time.sleep(60)'", 'job one')
        wait_for(control, 'Job started.')
        type_to(control, 'u')
        wait_for(control, 'job')
        (job,) = [j for j in __import__('panel_jobs').list_jobs(repo)]
        check(job['state'] == 'running' and 'JOBREADY' not in json.dumps(job) and 'argv' not in json.dumps(job),
              'public job row or auto log read')
        type_to(control, '4')
        wait_for(control, 'Action (Enter to go back)')
        type_to(control, 'l')
        wait_for(control, 'JOBREADY')
        type_to(control, '4')
        type_to(control, 's')
        wait_for(control, 'Stop requested.')
        deadline = time.monotonic() + 8
        while __import__('panel_jobs').list_jobs(repo)[0]['state'] != 'exited' and time.monotonic() < deadline:
            time.sleep(.1)
        check(__import__('panel_jobs').list_jobs(repo)[0]['state'] == 'exited', 'job stop did not complete')
        type_to(control, 'u')
        wait_for(control, 'hidden (f shows)')

        # Native authority is cleared in the shells; their keys reach the shell, not root bindings.
        shell_pane = rows[0]['pane']
        shell_window = rows[0]['window']
        type_to(shell_pane, 'printf "AUTH[%s%s%s]\\n" "$OMS_FAKE_AUTH" "$CLAUDECODE" "$CODEX_THREAD_ID"')
        wait_for(shell_pane, 'AUTH[]')
        type_to(shell_pane, 'cat -v')
        tmux('select-window', '-t', shell_window)
        tmux('select-pane', '-t', shell_pane)
        press(b'\x1b[17~')
        press(b'\x1b[18~')
        wait_for(shell_pane, '^[[17~')
        check('^[[18~' in screen(shell_pane) and not marker.exists(), 'F6/F7 did not pass through to the workspace shell')
        press(b'\x1bOS')
        check(tmux('display-message', '-p', '-t', ws, '#{pane_id}') == control, 'F4 did not focus the control pane')
        tmux('select-window', '-t', shell_window)
        tmux('select-pane', '-t', shell_pane)
        press(b'\x1b[15~')
        check(client_session() == 'origin' and tmux('display-message', '-p', '-t', 'origin', '#{pane_id}') == origin_pane,
              'F5 did not return to the origin pane')
        press(b'\x1b[17~')
        check(marker.exists(), 'original root F6 binding no longer works')

        # Reuse: same caller, same workspace.
        again = controls(origin_pane)
        check(again.returncode == 0 and json.loads(again.stdout)['id'] == public['id'], 'live workspace was not reused')
        check(len(workspace_rows()) == 1 and client_session() == tmux('display-message', '-p', '-t', ws, '#{session_name}'),
              'reuse created state or did not focus')
        press(b'\x1b[15~')

        # Binding replacement refuses and never adopts.
        tmux('set-option', '-t', ws, 'key-table', 'root')
        refused_table = controls(origin_pane)
        tmux('set-option', '-t', ws, 'key-table', record['key_table'])
        check(refused_table.returncode == 1 and len(workspace_rows()) == 1, 'changed key table accepted')
        # Key callbacks are proven by exact payload, not by key names; tampering refuses and is never repaired.
        def rebind(key, *payload, flags=()):
            tmux('bind-key', *flags, '-T', record['key_table'], key, *payload)

        def restore():
            tmux('unbind-key', '-a', '-T', record['key_table'])
            for key, action in workspaces.KEYS:
                tmux('bind-key', '-T', record['key_table'], key,
                     *workspaces._key_command(repo, record['id'], record['state_home'], action))

        check(controls(origin_pane).returncode == 0, 'untampered key bindings refused')
        press(b'\x1b[15~')
        good_f5 = workspaces._key_command(repo, record['id'], record['state_home'], 'origin')
        good_f4 = workspaces._key_command(repo, record['id'], record['state_home'], 'focus')
        for name, change in (
                ('same-key callback replaced', lambda: rebind('F5', 'display-message', 'hijacked')),
                ('focus payload on the origin key', lambda: rebind('F5', *good_f4)),
                ('payload suffix changed', lambda: rebind('F5', good_f5[0], good_f5[1], good_f5[2] + ' ; true')),
                ('repeat flag added', lambda: rebind('F5', *good_f5, flags=('-r',))),
                ('extra key bound', lambda: rebind('F8', *good_f5)),
                ('key removed', lambda: tmux('unbind-key', '-T', record['key_table'], 'F5'))):
            change()
            refused = controls(origin_pane)
            restore()
            check(refused.returncode == 1 and len(workspace_rows()) == 1, name + ' was accepted')
        check(controls(origin_pane).returncode == 0, 'restored key bindings refused')
        press(b'\x1b[15~')

        # Flagged workers cannot drive the callbacks, and refuse before any navigation.
        tmux('select-window', '-t', shell_window)
        tmux('select-pane', '-t', shell_pane)
        tmux('switch-client', '-t', ws)
        active = (tmux('display-message', '-p', '-t', ws, '#{window_id}\t#{pane_id}'), client_session())
        for flags in ({'OMS_HARNESS_CHILD': '1'}, {'OMS_HARNESS_DELEGATE_DEPTH': '1'}):
            for action in ('focus', 'origin'):
                worker = subprocess.run([sys.executable, str(root / 'scripts/lib/panel_workspaces.py'), '_key', str(repo),
                                         record['id'], record['state_home'], action, ws, shell_pane],
                                        env=dict(os.environ, **flags), capture_output=True, text=True, stdin=subprocess.DEVNULL)
                check(worker.returncode == 1 and 'workers cannot' in worker.stderr, 'worker callback was not refused')
        check((tmux('display-message', '-p', '-t', ws, '#{window_id}\t#{pane_id}'), client_session()) == active,
              'worker callback navigated')
        press(b'\x1bOS')
        check(tmux('display-message', '-p', '-t', ws, '#{pane_id}') == control, 'ordinary F4 stopped working')
        press(b'\x1b[15~')
        check(client_session() == 'origin', 'ordinary F5 stopped working')

        wrong = subprocess.run([sys.executable, str(root / 'scripts/lib/panel_workspaces.py'), '_key', str(repo), record['id'],
                                record['state_home'], 'focus', ws, foreign_pane], env=dict(os.environ, TMUX=os.environ['TMUX']),
                               capture_output=True, text=True, stdin=subprocess.DEVNULL)
        check(wrong.returncode == 1 and 'not in this workspace' in wrong.stderr, 'foreign caller pane drove the key helper')
        saved = json.dumps(record)
        with terminals.Store(repo, 'workspaces', record['id']) as store:
            with store.lock():
                store.write(dict(record, pane='%999'))
        check(controls(origin_pane).returncode == 1 and len(workspace_rows()) == 1, 'replaced pane metadata accepted')
        with terminals.Store(repo, 'workspaces', record['id']) as store:
            with store.lock():
                store.write(json.loads(saved))
        tmux('respawn-pane', '-k', '-t', control, 'sleep 300')
        check(controls(origin_pane).returncode == 1 and len(workspace_rows()) == 1, 'replaced control pane accepted')
        tmux('select-window', '-t', shell_window)
        press(b'\x1bOS')
        check(tmux('display-message', '-p', '-t', ws, '#{pane_id}') != control and
              tmux('display-message', '-p', '-t', ws, '#{window_id}') == shell_window, 'F4 adopted a replaced pane')
        check(client_session().startswith('oms_ws_') or client_session() == 'origin', 'client lost')

        # A positively gone workspace session is not blocking; its record stays untouched.
        stale = (tmp / 'stale.json')
        stale.write_text(json.dumps(record))
        tmux('kill-session', '-t', ws)
        fresh = controls(origin_pane)
        check(fresh.returncode == 0 and json.loads(fresh.stdout)['id'] != public['id'], 'gone workspace blocked a new one')
        check(len(workspace_rows()) == 2 and any(r == json.loads(saved) for r in workspace_rows()), 'old record repaired or removed')

        # Concurrent openers: a held first opener serializes the second; exactly one workspace results.
        tmux('switch-client', '-t', 'origin')
        for row in workspace_rows():
            if row['session'] in tmux('list-sessions', '-F', '#{session_id}').split():
                tmux('kill-session', '-t', row['session'])
        known = len(workspace_rows())
        gate = tmp / 'gate'
        gate.mkdir()
        opener = (
            "import json, sys, time\nfrom pathlib import Path\nsys.path.insert(0, sys.argv[1])\n"
            "import panel_workspaces as w\nrepo, session, name, gate = sys.argv[2], sys.argv[3], sys.argv[4], Path(sys.argv[5])\n"
            "real = w._related\n"
            "def related(*args):\n    (gate / (name + '-query')).touch()\n    found = real(*args)\n"
            "    if name == 'first':\n        (gate / 'first-queried').touch()\n"
            "        end = time.monotonic() + 10\n"
            "        while not (gate / 'release').exists() and time.monotonic() < end:\n            time.sleep(.02)\n"
            "    return found\n"
            "w._related = related\nw._navigate = lambda *args: (gate / (name + '-navigated')).touch()\n"
            "try:\n    print(json.dumps(w.open_workspace(repo, session)))\nexcept ValueError as error:\n"
            "    print(error, file=sys.stderr)\n    sys.exit(1)\n")
        procs = []

        def launch(name):
            procs.append(subprocess.Popen([sys.executable, '-c', opener, str(root / 'scripts/lib'), str(repo), home_session,
                                           name, str(gate)], env=dict(os.environ, TMUX_PANE=origin_pane),
                                          stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True))
            return procs[-1]

        try:
            first = launch('first')
            deadline = time.monotonic() + 10
            while not (gate / 'first-queried').exists() and time.monotonic() < deadline:
                time.sleep(.02)
            check((gate / 'first-queried').exists(), 'first opener never queried')
            second = launch('second')
            time.sleep(1)
            check(second.poll() is None and not (gate / 'second-query').exists(),
                  'second opener reached the related query while the first held the root lock')
            (gate / 'release').touch()
            results = [(p.wait(timeout=20),) + p.communicate() for p in (first, second)]
        finally:
            (gate / 'release').touch()
            for p in procs:
                if p.poll() is None:
                    p.kill()
                    p.wait()
        check(results[0][0] == 0, 'first opener failed: ' + results[0][2])
        created = workspace_rows()
        check(len(created) == known + 1, 'concurrent openers created %s workspaces' % (len(created) - known))
        if results[1][0] == 0:
            check(json.loads(results[1][1])['id'] == json.loads(results[0][1])['id'] and (gate / 'second-navigated').exists(),
                  'second opener did not reuse the first workspace')
        else:
            check('busy' in results[1][2] and not (gate / 'second-navigated').exists(), 'second opener refused unexpectedly')
        later = controls(origin_pane)
        check(later.returncode == 0 and len(workspace_rows()) == known + 1, 'later opener failed or duplicated: %s %s' % (later.stderr, len(workspace_rows()) - known))

        check(tmux('display-message', '-p', '-t', origin_pane, '#{pane_pid}') == before['pid'], 'origin PID changed')
        check(tmux('show-options', '-g') == before['globals'] and tmux('list-keys', '-T', 'root') == before['root'],
              'global options or root keys changed')
        check(not (repo / '.oms').exists() and not (tmp / 'state' / 'oh-my-setting' / 'panel-control').is_symlink(),
              'room or shared artifacts appeared')
        print('PASS: %s workspace assertions (actual private tmux+PTY)' % checked)
finally:
    if native:
        subprocess.run([tmux_bin, '-S', socket, 'kill-server'], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL)
    if master is not None:
        os.close(master)
    if client:
        os.waitpid(client, 0)

# Missing native evidence is a non-green result, not a success-shaped skip.
faulthandler.cancel_dump_traceback_later()
if not native:
    print('PASS: %s controlled workspace/menu assertions (native tmux+PTY blocked)' % checked)
sys.exit(0 if native else 77)
PY
