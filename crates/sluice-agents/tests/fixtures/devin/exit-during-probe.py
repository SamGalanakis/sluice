#!/usr/bin/python3
"""Scripted tmux client: Devin journals its last hooks and exits during the pane probe or,
with exit-on-client present, during the next client command after a live probe."""
import json
import os
import pathlib
import shlex
import sys

run = pathlib.Path.cwd()
args = sys.argv[1:]
pending = run / 'exit-hooks.json'
on_client = (run / 'exit-on-client').exists()


def publish():
    config = json.loads((run / 'devin-config.json').read_text())
    command = config['hooks']['Stop'][-1]['hooks'][0]['command']
    env = dict(word.split('=', 1) for word in shlex.split(command)[:2])
    with open(env['SLUICE_DEVIN_JOURNAL'], 'a') as journal:
        for hook in json.loads(pending.read_text()):
            journal.write(json.dumps({'invocation': env['SLUICE_DEVIN_INVOCATION'], 'hook': hook}) + '\n')
        journal.flush()
        os.fsync(journal.fileno())
    pending.unlink()
    (run / 'pane-dead').touch()


if 'list-panes' in args:
    if pending.exists() and not on_client:
        publish()
    print('%0 1' if (run / 'pane-dead').exists() else '%0 0')
elif pending.exists() and on_client:
    publish()
    sys.stderr.write('no current target\n')
    sys.exit(1)
elif (run / 'pane-dead').exists():
    sys.stderr.write('no server running\n')
    sys.exit(1)
elif 'capture-pane' in args:
    print('❭ Ask Devin to build features, fix bugs, or work on your code')
