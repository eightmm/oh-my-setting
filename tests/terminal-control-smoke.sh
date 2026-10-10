#!/usr/bin/env bash
set -euo pipefail
# A new canonical suite: operator-tools does not exercise managed shells/jobs.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-terminal-control.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
OMS_TERMINAL_TEST_ROOT="$ROOT" OMS_TERMINAL_TEST_TMP="$TMP" python3 - <<'PY'
import ast
from concurrent.futures import ThreadPoolExecutor
import contextlib
import io
import json
import os
from pathlib import Path
import pty
import select
import faulthandler
import shutil
import shlex
import signal
import subprocess
import sys
import time
from unittest.mock import patch

root = Path(os.environ.pop('OMS_TERMINAL_TEST_ROOT'))
tmp = Path(os.environ.pop('OMS_TERMINAL_TEST_TMP'))
for key in list(os.environ):
    if key.startswith('OMS_') or key in ('TMUX', 'TMUX_PANE'):
        os.environ.pop(key, None)
sys.path.insert(0, str(root / 'scripts/lib'))
import panel_jobs as jobs
import panel_terminals as terminals
import panel_workspaces as workspaces

repo = tmp / r'repo a$b and literal \$ with space #hash'
repo.mkdir()
os.environ['XDG_STATE_HOME'] = str(tmp / 'state')
other = tmp / 'other'
other.mkdir()
faulthandler.dump_traceback_later(45, exit=True)
checked = 0
native = False
socket = str(tmp / 'tmux-socket')
client = None
master = None
tracked = []


def check(condition, message):
    global checked
    assert condition, message
    checked += 1


def refused(action):
    try:
        action()
    except (ValueError, OSError):
        return
    raise AssertionError('unsafe action was accepted')


def await_row(ident, predicate, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        row = next(item for item in jobs.list_jobs(repo) if item['id'] == ident)
        if predicate(row):
            return row
        time.sleep(.04)
    raise AssertionError('job did not reach expected state: ' + str(row))


def launch(code, label='Fixture job'):
    row = jobs.run_job(repo, session, [sys.executable, '-c', code], label=label)
    tracked.append(row['id'])
    return row


def record(ident):
    with terminals.Store(repo, 'jobs', ident) as store:
        return store.read()


def write_record(ident, row):
    with terminals.Store(repo, 'jobs', ident) as store:
        with store.lock():
            store.write(row)


try:
    launcher = terminals._shell_command(repo, 'term-' + 'a' * 32,
                                        str(tmp / 'state' / r'dollar $ and literal \$'))
    # tmux 3.4 escapes a dollar only before [A-Za-z_{]; each expected string has exactly one display form.
    check(terminals.tmux_display('a$b c#d') == 'a\\$b c#d' and terminals.tmux_display('$1 $$x tr$') == '$1 $\\$x tr$'
          and terminals.tmux_display('${x} $_x') == '\\${x} \\$_x', 'dollar display rule changed')
    check(terminals._same_launcher(shlex.quote(terminals.tmux_display(launcher)), launcher),
          'tmux dollar-display escaping rejected')
    for plain in ('x a$b', 'x ${y}', 'x $_z', 'x $$q'):
        check(terminals._same_launcher(shlex.quote(terminals.tmux_display(plain)), plain),
              'encoded dollar launcher rejected: ' + plain)
        check(not terminals._same_launcher(shlex.quote(plain), plain), 'unencoded dollar launcher accepted: ' + plain)
    literal = 'x a\\$b'
    check(terminals._same_launcher(shlex.quote(terminals.tmux_display(literal)), literal),
          'literal backslash-dollar launcher rejected')
    check(not terminals._same_launcher(shlex.quote(terminals.tmux_display('x a$b')), literal)
          and not terminals._same_launcher(shlex.quote('x a$b'), literal)
          and not terminals._same_launcher(shlex.quote(literal), literal),
          'altered raw backslash-dollar launcher accepted')
    check(terminals.tmux_display('a\x01b') is None and not terminals._same_launcher(shlex.quote('a\\001b'), 'a\x01b')
          and not terminals._same_tmux_tokens(['a\x01b'], ['a\x01b']), 'control characters were not refused')
    for changed in (launcher + ' ; touch injected', launcher + ' extra'):
        check(not terminals._same_launcher(changed, launcher), 'modified launcher command accepted')
    # No tmux server is reused, including when the suite runs inside a real panel.
    tmux = shutil.which('tmux')
    if tmux:
        probe = subprocess.run([tmux, '-S', socket, '-f', '/dev/null', 'new-session', '-d',
                                '-s', 'fixture', '-x', '90', '-y', '28'], capture_output=True, text=True)
        native = probe.returncode == 0
        if not native:
            print('BLOCKED native tmux+PTY: ' + probe.stderr.strip())
    else:
        print('BLOCKED native tmux+PTY: tmux unavailable')
    if native:
        raw = subprocess.check_output([tmux, '-S', socket, 'display-message', '-p', '-t', 'fixture',
                                       '#{pid}\t#{session_id}\t#{pane_id}'], text=True).strip().split('\t')
        server_pid, session, original_pane = raw
        os.environ['TMUX'] = socket + ',' + server_pid + ',0'
        os.environ['TMUX_PANE'] = original_pane
        terminals.tmux('set-option', '-t', session, '@oms_panel_repo', str(repo))
        original_pid = terminals.tmux('display-message', '-p', '-t', original_pane, '#{pane_pid}')
        original_options = terminals.tmux('show-options', '-g')
    else:
        # Controlled subprocess tests still exercise the real private-file,
        # subreaper, pidfd and job lifecycle. Only tmux session proof is a fixture.
        bindir = tmp / 'bin'
        bindir.mkdir()
        shim = bindir / 'tmux'
        shim.write_text('#!' + sys.executable + '\nimport sys\na=sys.argv[1:]\n'
                        'if a[0]=="display-message": print("$1\\t' + str(os.getpid()) + '\\tfixture-socket")\n'
                        'elif a[0]=="show-options": print(' + repr(str(repo)) + ')\n'
                        'else: sys.exit(2)\n')
        shim.chmod(0o700)
        os.environ['PATH'] = str(bindir) + os.pathsep + os.environ.get('PATH', '')
        session = '$1'

    for module in ('panel_terminals.py', 'panel_jobs.py', 'panel_workspaces.py'):
        ast.parse((root / 'scripts/lib' / module).read_text(), feature_version=(3, 9))
    check(True, 'Python 3.9 grammar')
    with patch.dict(os.environ, {'OMS_HARNESS_CHILD': '1'}):
        refused(lambda: terminals.open_terminal(repo, session))
        refused(lambda: jobs.run_job(repo, session, ['true']))
        refused(lambda: jobs.list_jobs(repo))
    check(not (repo / '.oms').exists(), 'worker calls created private state')
    refused(lambda: jobs.run_job(other, session, ['true']))
    refused(lambda: jobs.run_job(repo, 'fixture', ['true']))
    check(not (other / '.oms').exists(), 'foreign session created private state')

    row = launch("print('bounded output'); print('\\x1b[31mcolor\\x1b[0m')")
    done = await_row(row['id'], lambda value: value['state'] == 'exited')
    check(done['exit_code'] == 0 and done['capabilities']['restart'], 'completed job state')
    output = jobs.job_logs(repo, row['id'], 1)
    check('bounded output' in output['text'] and '\x1b' not in output['text'], 'safe explicit logs')
    check('argv' not in json.dumps(jobs.list_jobs(repo)) and 'bounded output' not in json.dumps(jobs.list_jobs(repo)),
          'public job rows contain private recipe/logs')
    refused(lambda: jobs.interrupt_job(repo, row['id'], 1))
    refused(lambda: jobs.job_logs(repo, row['id'], 99))
    check(True, 'exited and stale generations reject mutation')

    retained = launch("print('retained before restart')")
    await_row(retained['id'], lambda value: value['state'] == 'exited')
    before = record(retained['id'])
    retained_path = terminals.state_directory(repo) / 'jobs' / retained['id'] / 'output'
    old_bytes = retained_path.read_bytes()
    real_proof = terminals.session_proof
    proofs = 0
    def reject_second(*args):
        global proofs
        proofs += 1
        if proofs == 2:
            raise ValueError('session changed during restart')
        return real_proof(*args)
    with patch.object(terminals, 'session_proof', side_effect=reject_second):
        refused(lambda: jobs.restart_job(repo, session, retained['id'], 1))
    check(proofs == 2 and record(retained['id']) == before and retained_path.read_bytes() == old_bytes,
          'prelaunch refusal changed completed generation or log')
    jobs.restart_job(repo, session, retained['id'], 1)
    check(await_row(retained['id'], lambda value: value['state'] == 'exited')['generation'] == 2,
          'safe retry after prelaunch refusal')

    uncertain = launch("print('old output survives spawn error')")
    await_row(uncertain['id'], lambda value: value['state'] == 'exited')
    uncertain_path = terminals.state_directory(repo) / 'jobs' / uncertain['id'] / 'output'
    uncertain_bytes = uncertain_path.read_bytes()
    uncertain_before = record(uncertain['id'])
    real_popen = subprocess.Popen
    def reject_spawn(argv, *args, **kwargs):
        if '_supervise' in argv:
            raise OSError('controlled spawn error')
        return real_popen(argv, *args, **kwargs)
    with patch.object(jobs.subprocess, 'Popen', side_effect=reject_spawn):
        refused(lambda: jobs.restart_job(repo, session, uncertain['id'], 1))
    check(record(uncertain['id']) == uncertain_before and uncertain_path.read_bytes() == uncertain_bytes,
          'failed constructor changed a completed generation or its log')
    jobs.restart_job(repo, session, uncertain['id'], 1)
    check(await_row(uncertain['id'], lambda value: value['state'] == 'exited')['generation'] == 2,
          'safe retry after failed supervisor constructor')

    rollback_job = launch("print('log preserved on publication failure')")
    await_row(rollback_job['id'], lambda value: value['state'] == 'exited')
    rollback_before = record(rollback_job['id'])
    rollback_path = terminals.state_directory(repo) / 'jobs' / rollback_job['id'] / 'output'
    rollback_bytes = rollback_path.read_bytes()
    real_write = terminals.Store.write
    def fail_new_generation(store, value, name='record'):
        if value.get('id') == rollback_job['id'] and value.get('generation') == 2:
            raise OSError('controlled record publication error')
        return real_write(store, value, name)
    with patch.object(terminals.Store, 'write', fail_new_generation):
        refused(lambda: jobs.restart_job(repo, session, rollback_job['id'], 1))
    check(record(rollback_job['id']) == rollback_before and rollback_path.read_bytes() == rollback_bytes,
          'unreleased generation publication failure lost completed history')

    counter = tmp / 'restart-count'
    restarted = launch("from pathlib import Path; p=Path(" + repr(str(counter)) + "); "
                       "p.write_text(p.read_text()+'x' if p.exists() else 'x')")
    await_row(restarted['id'], lambda value: value['state'] == 'exited')
    def restart():
        try:
            return jobs.restart_job(repo, session, restarted['id'], 1)
        except ValueError:
            return None
    with ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(lambda unused: restart(), range(2)))
    check(sum(item is not None for item in results) == 1, 'concurrent restart launched twice')
    done = await_row(restarted['id'], lambda value: value['state'] == 'exited')
    check(done['generation'] == 2 and counter.read_text() == 'xx', 'restart recipe/generation')
    refused(lambda: jobs.stop_job(repo, restarted['id'], 1))

    natural_child = launch("import os,time; p=os.fork(); os._exit(7) if p else None; "
                           "os.setsid(); time.sleep(.3); print('child finished',flush=True)")
    done = await_row(natural_child['id'], lambda value: value['state'] == 'exited')
    check(done['exit_code'] == 7 and 'child finished' in jobs.job_logs(repo, natural_child['id'], 1)['text'],
          'natural descendant completion lost leader status or child output')

    interrupt = launch("import signal,time,sys; signal.signal(signal.SIGINT, lambda *a: sys.exit(23)); "
                       "print('ready', flush=True); time.sleep(10)")
    deadline = time.monotonic() + 3
    while 'ready' not in jobs.job_logs(repo, interrupt['id'], 1)['text'] and time.monotonic() < deadline:
        time.sleep(.03)
    check('ready' in jobs.job_logs(repo, interrupt['id'], 1)['text'], 'interrupt fixture readiness timed out')
    jobs.interrupt_job(repo, interrupt['id'], 1)
    done = await_row(interrupt['id'], lambda value: value['state'] == 'exited')
    check(done['exit_code'] == 23, 'SIGINT reached only registered job')

    surviving = launch("import os,time; p=os.fork(); "
                       "os._exit(0) if p else None; os.setsid(); "
                       "print('surviving child',flush=True); time.sleep(10)")
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        private = record(surviving['id'])
        leader = terminals.process(private.get('leader_pid'))
        if leader and leader['state'] == 'Z':
            break
        time.sleep(.03)
    check(leader and leader['state'] == 'Z', 'leader exit fixture did not occur')
    live = next(item for item in jobs.list_jobs(repo) if item['id'] == surviving['id'])
    check(live['state'] == 'running' and live['capabilities']['stop'], 'leader exit hid surviving descendant')
    refused(lambda: jobs.restart_job(repo, session, surviving['id'], 1))
    check(jobs.stop_job(repo, surviving['id'], 1)['state'] == 'exited', 'adopted setsid child not stopped')

    stubborn = launch("import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                      "print('ready',flush=True); time.sleep(10)")
    deadline = time.monotonic() + 3
    while 'ready' not in jobs.job_logs(repo, stubborn['id'], 1)['text'] and time.monotonic() < deadline:
        time.sleep(.03)
    check('ready' in jobs.job_logs(repo, stubborn['id'], 1)['text'], 'stubborn fixture readiness timed out')
    private = record(stubborn['id'])
    stale = dict(private, supervisor_identity='0' * 64)
    write_record(stubborn['id'], stale)
    # Restore immediately: requests must refuse stale evidence without signaling.
    refused(lambda: jobs.stop_job(repo, stubborn['id'], 1))
    check(terminals.matches(private['leader_pid'], private['leader_identity']), 'stale request signaled live job')
    write_record(stubborn['id'], private)
    # Use a separate bounded fixture for escalation; a supervisor may already
    # have noticed deliberate record corruption and must fail closed.
    state = next(item for item in jobs.list_jobs(repo) if item['id'] == stubborn['id'])
    if state['state'] == 'running':
        done = jobs.stop_job(repo, stubborn['id'], 1)
        check(done['state'] == 'exited' and done['exit_code'] == -signal.SIGKILL,
              'owned escalation failed: state=%s exit_code=%s' % (done['state'], done['exit_code']))
    else:
        if terminals.matches(private['leader_pid'], private['leader_identity']):
            jobs._signal_owned([terminals.process(private['leader_pid'])], signal.SIGKILL)
        check(state['state'] == 'unknown', 'lost supervisor must remain unknown')

    escalation = launch("import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                        "print('ready',flush=True); time.sleep(10)")
    deadline = time.monotonic() + 3
    while 'ready' not in jobs.job_logs(repo, escalation['id'], 1)['text'] and time.monotonic() < deadline:
        time.sleep(.03)
    check('ready' in jobs.job_logs(repo, escalation['id'], 1)['text'], 'escalation fixture readiness timed out')
    done = jobs.stop_job(repo, escalation['id'], 1)
    check(done['state'] == 'exited' and done['exit_code'] == -signal.SIGKILL,
          'bounded owned escalation failed: state=%s exit_code=%s' % (done['state'], done['exit_code']))

    # Completion publication is held open inside the supervisor's record lock
    # (leader reaped, record still stopping). Observers must wait, never
    # report the reaped-but-unpublished window as unknown. The startup hook is a
    # private fixture; the supervisor argv and product code are untouched.
    hook = tmp / 'hold-hook'
    hook.mkdir()
    marker, release = hook / 'publishing', hook / 'release'
    (hook / 'sitecustomize.py').write_text(
        'import os, sys, time\n'
        'if len(sys.argv) > 2 and sys.argv[1] == "_supervise" and os.environ.get("PANEL_TEST_HOLD"):\n'
        '    sys.path.insert(0, ' + repr(str(root / 'scripts/lib')) + ')\n'
        '    import panel_terminals as t\n'
        '    original = t.Store.write\n'
        '    def write(self, value, name="record"):\n'
        '        if (name == "record" and value.get("state") == "exited"\n'
        '                and value.get("completion") == "reaped-all-children"):\n'
        '            open(os.environ["PANEL_TEST_HOLD"] + "/publishing", "w").close()\n'
        '            end = time.monotonic() + 2\n'
        '            while time.monotonic() < end and not os.path.exists(os.environ["PANEL_TEST_HOLD"] + "/release"):\n'
        '                time.sleep(.01)\n'
        '        return original(self, value, name)\n'
        '    t.Store.write = write\n')
    with patch.dict(os.environ, {'PYTHONPATH': str(hook), 'PANEL_TEST_HOLD': str(hook)}):
        handoff = launch("import time; print('ready',flush=True); time.sleep(10)")
    try:
        deadline = time.monotonic() + 3
        while 'ready' not in jobs.job_logs(repo, handoff['id'], 1)['text'] and time.monotonic() < deadline:
            time.sleep(.03)
        check('ready' in jobs.job_logs(repo, handoff['id'], 1)['text'], 'handoff fixture readiness timed out')
        with ThreadPoolExecutor(max_workers=2) as pool:
            stopping = pool.submit(jobs.stop_job, repo, handoff['id'], 1)
            deadline = time.monotonic() + 6
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.01)
            check(marker.exists(), 'completion publication was never reached')
            listing = pool.submit(jobs.list_jobs, repo)
            time.sleep(.4)
            check(not stopping.done() and not listing.done(),
                  'observers returned while the supervisor held the reaped-unpublished window')
            release.write_text('go')
            final = stopping.result(timeout=8)
            listed = next(item for item in listing.result(timeout=8) if item['id'] == handoff['id'])
        check(final['state'] == 'exited' and final['exit_code'] == -signal.SIGTERM and
              listed['state'] == 'exited' and listed['exit_code'] == -signal.SIGTERM,
              'handoff observed %s/%s' % (final, listed))
    finally:
        release.write_text('go')

    literal = jobs.run_job(repo, session, [sys.executable, '-c', 'import sys; print(sys.argv[1])',
                                         '; touch should-not-exist'], cwd=repo)
    tracked.append(literal['id'])
    await_row(literal['id'], lambda value: value['state'] == 'exited')
    check(not (repo / 'should-not-exist').exists(), 'argv was interpreted as shell syntax')
    check('; touch should-not-exist' in jobs.job_logs(repo, literal['id'], 1)['text'], 'argv argument changed')
    env_job = launch("import os; print('authority=' + str(any(k.startswith('OMS_') for k in os.environ)))")
    await_row(env_job['id'], lambda value: value['state'] == 'exited')
    check('authority=False' in jobs.job_logs(repo, env_job['id'], 1)['text'], 'job inherited OMS execution identity')

    noisy = launch("import os; [os.write(1,b'x'*65536) for _ in range(40)]; print('drained')")
    done = await_row(noisy['id'], lambda value: value['state'] == 'exited')
    logfile = terminals.state_directory(repo) / 'jobs' / noisy['id'] / 'output'
    check(logfile.stat().st_size == jobs.LOG_LIMIT and done['log_truncated'], 'log retention exceeded bound')
    check(done['exit_code'] == 0 and jobs.job_logs(repo, noisy['id'], 1, maximum=100)['truncated'],
          'output beyond limit blocked child or hid truncation')

    # All unsafe objects are restored before the next case; no background writer
    # touches these already-completed generations.
    safe_id = row['id']
    folder = terminals.state_directory(repo) / 'jobs' / safe_id
    for name in ('record', 'recipe', 'output'):
        path = folder / name
        backup = folder / ('saved-' + name)
        path.rename(backup)
        for shape in ('symlink', 'fifo', 'hardlink', 'public-mode'):
            if shape == 'symlink':
                path.symlink_to(backup)
            elif shape == 'fifo':
                os.mkfifo(path, 0o600)
            elif shape == 'hardlink':
                os.link(backup, path)
            else:
                path.write_bytes(backup.read_bytes())
                path.chmod(0o644)
            before = backup.read_bytes()
            refused(lambda: jobs.job_logs(repo, safe_id, 1))
            refused(lambda: jobs.restart_job(repo, session, safe_id, 1))
            check(backup.read_bytes() == before, 'unsafe file was mutated')
            path.unlink()
        backup.rename(path)
    saved = record(safe_id)
    for changed in (dict(saved, repo=str(other)), dict(saved, uid=os.getuid() + 1)):
        write_record(safe_id, changed)
        refused(lambda: jobs.restart_job(repo, session, safe_id, 1))
        check(record(safe_id) == changed, 'foreign record was mutated')
    write_record(safe_id, saved)
    count = len(terminals.identifiers(repo, 'jobs'))
    with patch.object(terminals, 'MAX_ITEMS', count):
        refused(lambda: jobs.run_job(repo, session, [sys.executable, '-c', 'pass']))
        refused(lambda: jobs.forget_job(repo, safe_id, 2))
        extra = folder / 'extra'
        extra.write_text('unsafe unexpected history')
        extra.chmod(0o600)
        refused(lambda: jobs.forget_job(repo, safe_id, 1))
        check(folder.exists(), 'unexpected file was partly cleaned')
        extra.unlink()
        check(jobs.forget_job(repo, safe_id, 1)['forgotten'], 'completed job cleanup refused')
        check(not folder.exists(), 'completed job history/log survived cleanup')
        replacement = launch("print('reclaimed allocation')")
        await_row(replacement['id'], lambda value: value['state'] == 'exited')
        check(True, 'completed job cleanup freed allocation capacity')
    with contextlib.redirect_stdout(io.StringIO()):
        check(jobs.cli_main(['list', '--repo', str(repo)]) == 0, 'job CLI list adapter')

    if native:
        for key in ('OMS_ROOM_ID', 'OMS_ATTEMPT_ID', 'OMS_HARNESS_CHILD', 'CLAUDECODE', 'CODEX_THREAD_ID'):
            terminals.tmux('set-environment', '-t', session, key, 'inherited-fixture')
        terminal = terminals.open_terminal(repo, session, cwd=repo, label='Ordinary fixture')
        ident = terminal['id']
        with terminals.Store(repo, 'terminals', ident) as store:
            binding = store.read()
        check(terminal['state'] == 'running' and not terminal['capabilities']['restart'], 'interactive terminal state')
        launcher = terminals._shell_command(repo, ident, binding['state_home'])
        check(terminals._same_launcher(shlex.quote(terminals.tmux_display(launcher)), launcher),
              'tmux outer-quoted launcher representation rejected')
        # Real tmux serialization at every metadata boundary, with one canonical display per expected string.
        tricky = ['dollar $ spaces #', 'a\\$b', '${x}', '$_x', '$1', '$$x', 'trail$', "q'a`b\"c", 'a\\\\$b', '$ü']
        for index, value in enumerate(tricky):
            terminals.tmux('set-option', '-t', session, '@oms_probe', value)
            check(terminals.tmux('show-options', '-v', '-t', session, '@oms_probe') == terminals.tmux_display(value),
                  'owner-marker display differs from tmux: ' + repr(value))
            owned = tmp / ('repo-' + value)
            owned.mkdir()
            terminals.tmux('new-session', '-d', '-s', 'own%d' % index, 'sleep 600')
            own_id = terminals.tmux('display-message', '-p', '-t', 'own%d' % index, '#{session_id}')
            terminals.tmux('set-option', '-t', own_id, '@oms_panel_repo', str(owned.resolve()))
            check(terminals.session_proof(owned, own_id)['session'] == own_id, 'owner marker rejected: ' + repr(value))
            for foreign in (tmp, tmp / 'other-$x'):
                foreign.mkdir(exist_ok=True)
                try:
                    terminals.session_proof(foreign, own_id)
                except ValueError:
                    pass
                else:
                    check(False, 'foreign owner marker accepted')
            terminals.tmux('bind-key', '-T', 'probe', 'a', 'run-shell', value)
            listed = shlex.split(terminals.tmux('list-keys', '-T', 'probe'))
            check(terminals._same_tmux_tokens(listed, ['bind-key', '-T', 'probe', 'a', 'run-shell', value]),
                  'list-keys display differs from tmux: ' + repr(value))
            check(not terminals._same_tmux_tokens(listed, ['bind-key', '-T', 'probe', 'a', 'run-shell', value + 'x'])
                  and not terminals._same_tmux_tokens(listed + ['x'], ['bind-key', '-T', 'probe', 'a', 'run-shell', value]),
                  'list-keys comparison too loose: ' + repr(value))
            terminals.tmux('unbind-key', '-a', '-T', 'probe')
            command = 'exec ' + shlex.join(['sh', '-c', 'sleep 600', '_', value])
            terminals.tmux('new-session', '-d', '-s', 'cmd%d' % index, command)
            displayed = terminals.tmux('display-message', '-p', '-t', 'cmd%d' % index, '#{pane_start_command}')
            check(terminals._same_launcher(displayed, command), 'pane_start_command differs from tmux: ' + repr(value))
            other = 'exec ' + shlex.join(['sh', '-c', 'sleep 600', '_', value.replace('$', '$$') if '$' in value else value + '$'])
            check(not terminals._same_launcher(displayed, other), 'altered pane command accepted: ' + repr(value))
        # Literal backslash-dollar expected: the command with the backslash stripped must stay distinct.
        terminals.tmux('new-session', '-d', '-s', 'altered', 'exec ' + shlex.join(['sh', '-c', 'sleep 600', '_', 'a$b']))
        altered = terminals.tmux('display-message', '-p', '-t', 'altered', '#{pane_start_command}')
        check(not terminals._same_launcher(altered, 'exec ' + shlex.join(['sh', '-c', 'sleep 600', '_', 'a\\$b'])),
              'altered literal backslash-dollar command accepted')
        for changed in (launcher + ' ; touch injected', launcher + ' extra'):
            check(not terminals._same_launcher(changed, launcher), 'modified launcher command accepted')
        client, master = pty.fork()
        if client == 0:
            os.environ['TERM'] = 'xterm-256color'
            os.execv(tmux, [tmux, '-S', socket, 'attach-session', '-t', session])
        time.sleep(.2)
        terminals.focus_terminal(repo, ident)
        os.write(master, b"printf 'SHELL_TYPED_OK\\n'; printf '%s' \"$OMS_ROOM_ID$OMS_ATTEMPT_ID$OMS_HARNESS_CHILD$CLAUDECODE$CODEX_THREAD_ID\" > authority-check\n")
        deadline = time.monotonic() + 4
        while time.monotonic() < deadline:
            if select.select([master], [], [], .02)[0]:
                os.read(master, 65536)
            if (repo / 'authority-check').exists() and 'SHELL_TYPED_OK' in terminals.read_terminal(repo, ident)['text']:
                break
        check((repo / 'authority-check').read_text() == '', 'inherited authority reached ordinary shell')
        check('SHELL_TYPED_OK' in terminals.read_terminal(repo, ident)['text'], 'native PTY typing/output failed')
        check(terminals.read_terminal(repo, ident, maximum=4)['truncated'], 'terminal read truncation missing')
        alongside = launch("import time; time.sleep(10)")
        check(jobs.stop_job(repo, alongside['id'], 1)['state'] == 'exited', 'job alongside terminal did not stop')
        check(terminals.matches(binding['pid'], binding['identity']), 'job stop changed interactive shell PID')
        foreign_session = terminals.tmux('new-session', '-d', '-P', '-F', '#{session_id}', '-s', 'foreign')
        terminals.tmux('set-option', '-t', foreign_session, '@oms_panel_repo', str(other))
        refused(lambda: terminals.open_terminal(repo, foreign_session))
        refused(lambda: jobs.run_job(repo, foreign_session, ['true']))
        check(True, 'exact foreign session rejected before launch')
        check(terminals.tmux('display-message', '-p', '-t', original_pane, '#{pane_pid}') == original_pid,
              'native shell PID changed')
        check(terminals.tmux('show-options', '-g') == original_options, 'global tmux settings changed')
        with patch.dict(os.environ, {'OMS_HARNESS_DELEGATE_DEPTH': '1'}):
            refused(lambda: terminals.focus_terminal(repo, ident))
            refused(lambda: terminals.read_terminal(repo, ident))
        terminals.tmux('set-option', '-p', '-t', binding['pane'], '@oms_terminal_id', 'foreign')
        refused(lambda: terminals.focus_terminal(repo, ident))
        refused(lambda: terminals.read_terminal(repo, ident))
        terminals.tmux('set-option', '-p', '-t', binding['pane'], '@oms_terminal_id', ident)
        terminals.tmux('respawn-pane', '-k', '-t', binding['pane'], 'exec sh')
        refused(lambda: terminals.read_terminal(repo, ident))
        check(terminals.list_terminals(repo)[0]['state'] == 'unknown', 'replaced pane was adopted')
        refused(lambda: terminals.forget_terminal(repo, ident))
        with patch.object(terminals, 'MAX_ITEMS', 1):
            refused(lambda: terminals.open_terminal(repo, session))
            terminals.tmux('kill-window', '-t', binding['window'])
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline and not terminals.terminal_gone(repo, binding):
                time.sleep(.03)
            check(terminals.terminal_gone(repo, binding), 'exact dead terminal not proven')
            check(terminals.forget_terminal(repo, ident)['forgotten'], 'dead terminal cleanup refused')
            check(terminals.open_terminal(repo, session)['state'] == 'running',
                  'terminal cleanup did not free allocation capacity')

        ws_id = terminals.new_store(repo, 'workspaces', 'term')
        ws_session = terminals.tmux('new-session', '-d', '-P', '-F', '#{session_id}',
                                    '-s', 'cleanup-workspace', 'sleep 30')
        terminals.tmux('set-option', '-t', ws_session, '@oms_panel_repo', str(repo))
        ws_pid = int(terminals.tmux('display-message', '-p', '-t', ws_session, '#{pane_pid}'))
        ws_process = terminals.process(ws_pid)
        ws_row = dict(schema=1, id=ws_id, repo=str(repo), uid=os.getuid(), kind='workspace',
                      label='Cleanup workspace', state_home=str(terminals.state_home()),
                      origin=terminals.session_proof(repo, session), session=ws_session,
                      pid=ws_pid, identity=ws_process['identity'])
        with terminals.Store(repo, 'workspaces', ws_id) as store:
            store.write(ws_row)
        refused(lambda: workspaces.forget_workspace(repo, ws_id))
        with patch.object(terminals, 'MAX_ITEMS', 1):
            refused(lambda: terminals.new_store(repo, 'workspaces', 'term'))
            terminals.tmux('kill-session', '-t', ws_session)
            with patch.object(workspaces, '_gone', side_effect=ValueError('collection failed')):
                refused(lambda: workspaces.forget_workspace(repo, ws_id))
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline and not workspaces.workspace_gone(repo, ws_row):
                time.sleep(.03)
            check(workspaces.workspace_gone(repo, ws_row), 'exact dead workspace not proven')
            check(workspaces.forget_workspace(repo, ws_id)['forgotten'], 'dead workspace cleanup refused')
            check(terminals.new_store(repo, 'workspaces', 'term').startswith('term-'),
                  'workspace cleanup did not free allocation capacity')
        with patch.object(terminals, 'process_gone', return_value=True), patch.object(terminals, 'tmux', side_effect=AssertionError('dead server queried')):
            check(terminals.terminal_gone(repo, binding) and workspaces.workspace_gone(repo, ws_row),
                  'confirmed original server/launcher disappearance could not reclaim old records')
        with patch.object(terminals, 'process_gone', return_value=False):
            check(not terminals.terminal_gone(repo, binding) and not workspaces.workspace_gone(repo, ws_row),
                  'uncertain process observation enabled cleanup')
        check(terminals.tmux('display-message', '-p', '-t', original_pane, '#{pane_pid}') == original_pid,
              'foreign/replaced pane handling harmed native shell')
    check(not (repo / '.oms').exists(), 'private logs or recipes entered shared repository state')
    print('PASS: %s managed terminal/job assertions (%s)' %
          (checked, 'actual private tmux+PTY and subprocesses' if native else 'controlled subprocesses; native blocked'))
finally:
    for ident in tracked:
        try:
            row = record(ident)
            if jobs._state(row) == 'running':
                jobs.stop_job(repo, ident, row['generation'])
        except (OSError, ValueError):
            pass
    if native:
        subprocess.run([tmux, '-S', socket, 'kill-server'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if master is not None:
        os.close(master)
    if client:
        os.waitpid(client, 0)

# Missing native evidence is a non-green result, not a success-shaped skip.
faulthandler.cancel_dump_traceback_later()
sys.exit(0 if native else 77)
PY
