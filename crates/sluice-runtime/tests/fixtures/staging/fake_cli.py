#!/usr/bin/env python3
"""Strict canned executable. A command absent from the fixture fails closed."""
import json
import os
import sys
from pathlib import Path

name = Path(sys.argv[0]).name
argv = [name, *sys.argv[1:]]
trace = Path(os.environ['STAGING_TRACE'])
with trace.open('a') as f:
    f.write(json.dumps({'argv': argv, 'cwd': os.getcwd()}) + '\n')
responses = json.loads(os.environ['STAGING_RESPONSES'])
for response in responses:
    if response['argv'] == argv:
        for path, contents in response.get('writes', {}).items():
            target = Path(path)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(contents)
        print(response.get('stdout', ''), end='')
        print(response.get('stderr', ''), end='', file=sys.stderr)
        sys.exit(response.get('code', 0))
print(f'unconfigured offline command: {argv}', file=sys.stderr)
sys.exit(91)
