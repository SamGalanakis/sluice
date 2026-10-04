"""Relocate copied private Codex paths; only a labelled scratch or owner backup."""
import json
from pathlib import Path
import sqlite3
import sys

old, home = map(lambda p: Path(p).resolve(), sys.argv[1:])
assert home != old and home != Path('/home/sam/.sluice')
assert not str(home).startswith('/home/sam/.sluice/')
for mapping in (home / 'codex-native-sessions').glob('*.json'):
    value = json.loads(mapping.read_text())
    original = Path(value['home'])
    assert original.is_relative_to(old / 'codex-native-homes')
    private = home / original.relative_to(old)
    value['home'] = str(private)
    mapping.write_text(json.dumps(value))
    for database in private.glob('state_*.sqlite'):
        con = sqlite3.connect(database)
        try:
            columns = {r[1] for r in con.execute('PRAGMA table_info(threads)')}
            if 'rollout_path' in columns:
                for rowid, path in con.execute('SELECT rowid,rollout_path FROM threads').fetchall():
                    if path and Path(path).is_relative_to(old):
                        con.execute('UPDATE threads SET rollout_path=? WHERE rowid=?',
                                    (str(home / Path(path).relative_to(old)), rowid))
                con.commit()
        finally:
            con.close()
