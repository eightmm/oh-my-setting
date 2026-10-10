#!/usr/bin/env python3
"""Actual local operations keep reference publication visible to capture GC.

These attempts describe this writer, never the calling model or its authority.
A crash or unsuccessful publication deliberately remains nonterminal.
"""
import os
from pathlib import Path
import runpy
import sys

HERE = Path(__file__).resolve().parent
E = runpy.run_path(str(HERE / 'agent-events.py'))
C = runpy.run_path(str(HERE / 'landing-capture.py'))


def transition(repo, attempt, state):
    E['append_lifecycle'](Path(repo), E['new_event'](
        attempt, 0, 'attempt.state_changed', from_state=None, to_state=state,
        reason_code='reference-publication',
        actor={'kind': 'system', 'name': 'landing-reference'}))


def begin(repo, tool, reference_input=""):
    if tool not in ('agent-task', 'patch-admit'):
        raise ValueError('unsupported reference writer')
    attempt = E['create_attempt'](Path(repo), provider='local', tool=tool,
                                  refs={'reference_writer': 'landing-reference-v1',
                                        'reference_input_sha256': C['digest'](os.path.abspath(reference_input).encode()) if reference_input else ''})
    # Emit the ID before transitions so the shell can retain it on failure.
    print(attempt, flush=True)
    transition(repo, attempt, 'starting')
    transition(repo, attempt, 'working')


def finish(repo, attempt, result):
    _, projection = E['load_projection'](Path(repo))
    current = projection.get(attempt)
    created = [row for row in E['read_rows'](E['event_path'](Path(repo)))
               if row.get('attempt_id') == attempt and row.get('event_type') == 'attempt.created']
    if (not current or len(created) != 1 or current.get('provider') != 'local' or
            current.get('tool') not in ('agent-task', 'patch-admit') or
            created[0].get('refs', {}).get('reference_writer') != 'landing-reference-v1' or
            current.get('parent_attempt_id')):
        raise ValueError('not an owned local reference operation')
    # The packet validator already ends proven no-write refusals while locked.
    # Its outer wrapper must not convert that terminal refusal to blocked.
    if current.get('terminal') is True:
        return
    if result == 'refuse-no-write':
        states = ('failed',)
    elif result == '0':
        states = ('verifying', 'review', 'done')
    else:
        states = ('blocked',)
    for state in states:
        transition(repo, attempt, state)


def supported(repo, path):
    path = os.path.realpath(path)
    return any(os.path.commonpath([path, os.path.join(repo, '.oms', name)]) ==
               os.path.join(repo, '.oms', name)
               for name in ('task', 'tasks', 'plan', 'artifacts'))


def aliases(repo):
    for item in C['reservations'](repo).values():
        for path in (os.path.join(item['directory'], item['landing_id'] + '.patch'),
                     os.path.join(repo, '.oms', 'landing-patches', item['landing_id'] + '.patch')):
            yield path, {value.encode() for value in C['reference_spellings'](repo, path)}


def validate(repo, target, values):
    raw = '\n'.join(values).encode()
    # Only designated section/note arguments are files. A metadata value
    # may itself be a managed path, whose hardlinks require indexed_bytes.
    content_positions = {'agent_task_replace_section_unlocked': 3,
                         'agent_task_append_bullet_unlocked': 4,
                         'agent_task_verify_finalize_unlocked': 4}
    inputs = [target] if os.path.lexists(target) else []
    position = content_positions.get(values[0]) if values else None
    if position is not None:
        inputs.append(values[position])
    for value in inputs:
        content, _ = C['regular'](value)
        raw += b'\n' + content
    found = False
    for path, forms in aliases(repo):
        if any(form in raw for form in forms):
            found = True
            if C['indexed_bytes'](repo, path) is None:
                raise ValueError('managed reference ownership unavailable')
    if found and not supported(repo, target):
        raise ValueError('managed references require a scanned OMS evidence namespace')


def main():
    action, repo = sys.argv[1:3]
    repo = os.path.realpath(repo)
    args = sys.argv[3:]
    if action == 'begin':
        begin(repo, *args)
    elif action == 'finish':
        attempt, result = args
        finish(repo, attempt, result)
    elif action == 'admission':
        C['record_admission'](repo, *args)
    elif action == 'task':
        validate(repo, args[0], args[1:])
    elif action == 'admit':
        patch, report = args
        paths = {path for path, _ in aliases(repo)}
        known = os.path.abspath(patch) in paths or os.path.realpath(patch) in paths
        if known:
            if os.path.abspath(patch) != os.path.realpath(patch):
                raise ValueError('redirected managed admission input')
            if C['indexed_bytes'](repo, patch) is None:
                raise ValueError('managed admission input ownership unavailable')
            if not supported(repo, report):
                raise ValueError('managed admission requires an internal report')
        print('1' if known else '0')
    else:
        raise ValueError('unsupported reference operation')


if __name__ == '__main__':
    try:
        main()
    except Exception as exc:
        print('landing-reference: ' + str(exc), file=sys.stderr)
        sys.exit(2)
