#!/usr/bin/env python3
"""Pinned callback double for real protocol-1 helper tests."""
import json
import os
import sys
from pathlib import Path

request = json.load(sys.stdin)
assert sys.argv[1:] == ['internal', 'callback']
assert request['protocol'] == 1
command = request['command']
name = command['command']
with Path(os.environ['STAGING_CALLBACK_TRACE']).open('a') as trace:
    trace.write(json.dumps(command) + '\n')
if name == 'builtin':
    invocation = command['args']['invocation']
    assert invocation['name'] == 'agent.run'
    assert invocation['project'] == os.environ['SLUICE_PROJECT_ID']
    assert 'Declared outputs' in invocation['inputs']['spec']
    data = {'session': 'session', 'report': 'Ready with offline evidence.', 'final': ''}
    value = {'reply': 'data', 'data': data}
elif name == 'submission':
    value = {'reply': 'data', 'data': {}}
elif name == 'tool':
    assert command['args']['name'] == 'message_post'
    assert command['args']['args']['project'] == os.environ['SLUICE_PROJECT_ID']
    assert command['args']['args']['to'] == 'owner'
    assert command['args']['args']['needs_reply'] is False
    value = {'reply': 'data', 'data': {'id': 42}}
elif name == 'retry_on_failure':
    assert command['args']['step'] == 'lane-work'
    value = {'reply': 'ack'}
else:
    raise AssertionError(f'unconfigured offline callback: {name}')
json.dump({'protocol': 1, 'request_id': request['request_id'],
           'result': {'status': 'ok', 'value': value}}, sys.stdout)
