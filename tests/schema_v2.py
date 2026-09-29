"""The schema of version 2 (sluice.db before the `readers` table), exactly as `db.SCHEMA` was:
the migration tests build real version-2 files with it."""

SCHEMA_V2 = """
CREATE TABLE projects (
  name TEXT PRIMARY KEY NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1)),
  paused INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)),
  icon_text TEXT,
  icon_type TEXT,
  icon BLOB,
  icon_hash TEXT,
  created TEXT NOT NULL,
  ver INTEGER NOT NULL DEFAULT 0,
  changed TEXT,
  CHECK (icon_text IS NULL OR icon IS NULL),
  CHECK ((icon IS NULL) = (icon_type IS NULL) AND (icon IS NULL) = (icon_hash IS NULL))
) STRICT;
CREATE TABLE plans (
  project TEXT PRIMARY KEY NOT NULL REFERENCES projects ON DELETE CASCADE,
  rev INTEGER NOT NULL CHECK (rev >= 1),
  doc TEXT NOT NULL CHECK (json_type(doc) = 'object')
) STRICT;
CREATE TABLE plan_edits (
  project TEXT NOT NULL REFERENCES projects ON DELETE CASCADE,
  rev INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  at TEXT NOT NULL,
  author TEXT NOT NULL,
  reason TEXT NOT NULL,
  ops TEXT NOT NULL CHECK (json_type(ops) = 'array'),
  PRIMARY KEY (project, rev)
) STRICT;
CREATE TABLE states (
  project TEXT PRIMARY KEY NOT NULL REFERENCES projects ON DELETE CASCADE,
  doc TEXT NOT NULL CHECK (json_type(doc) = 'object')
) STRICT;
CREATE TABLE calls (
  call TEXT PRIMARY KEY NOT NULL,
  project TEXT REFERENCES projects ON DELETE CASCADE,
  fn TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'succeeded', 'failed')),
  inputs TEXT NOT NULL CHECK (json_type(inputs) = 'object'),
  outputs TEXT,
  error TEXT,
  direct INTEGER NOT NULL DEFAULT 0 CHECK (direct IN (0, 1)),
  pid INTEGER,
  pid_start TEXT,
  created TEXT NOT NULL,
  finished TEXT
) STRICT;
CREATE TABLE submissions (
  project TEXT NOT NULL REFERENCES projects ON DELETE CASCADE,
  run TEXT NOT NULL,
  step TEXT NOT NULL,
  outputs TEXT NOT NULL CHECK (json_type(outputs) = 'object'),
  at TEXT NOT NULL,
  PRIMARY KEY (project, run)
) STRICT;
CREATE TABLE inbox (
  project TEXT NOT NULL REFERENCES projects ON DELETE CASCADE,
  n INTEGER NOT NULL,
  title TEXT NOT NULL,
  body TEXT,
  ui TEXT,
  input TEXT,
  sender TEXT,
  status TEXT NOT NULL CHECK (status IN ('open', 'answered', 'closed')),
  created TEXT NOT NULL,
  answer TEXT,
  answered TEXT,
  closed TEXT,
  reason TEXT,
  PRIMARY KEY (project, n)
) STRICT;
CREATE TABLE deletions (
  name TEXT PRIMARY KEY NOT NULL,
  token TEXT NOT NULL,
  at TEXT NOT NULL
) STRICT;
CREATE TABLE records (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  project TEXT REFERENCES projects ON DELETE CASCADE,
  at TEXT NOT NULL,
  kind TEXT NOT NULL,
  step TEXT,
  call TEXT,
  thread TEXT,
  run TEXT,
  data TEXT NOT NULL CHECK (json_type(data) = 'object')
) STRICT;
CREATE INDEX records_project ON records (project, seq);
CREATE INDEX records_kind ON records (project, kind, seq);
CREATE INDEX records_thread ON records (project, thread, seq);
CREATE INDEX records_call ON records (call);
CREATE INDEX records_run ON records (run);
CREATE INDEX calls_status ON calls (project, status);
CREATE INDEX inbox_status ON inbox (project, status);

CREATE TABLE IF NOT EXISTS outcomes (
  project TEXT NOT NULL REFERENCES projects ON DELETE CASCADE,
  step TEXT NOT NULL,
  rev INTEGER NOT NULL,
  unit TEXT,
  fn TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('succeeded', 'failed', 'skipped', 'stale')),
  outputs TEXT,
  error TEXT,
  started TEXT,
  finished TEXT,
  run_ids TEXT,
  manual INTEGER NOT NULL DEFAULT 0 CHECK (manual IN (0, 1)),
  removed TEXT NOT NULL,
  author TEXT,
  reason TEXT,
  PRIMARY KEY (project, step, rev)
) STRICT;
CREATE INDEX IF NOT EXISTS outcomes_unit ON outcomes (project, unit);

CREATE VIEW steps AS
SELECT p.project, s.key AS step, s.value ->> '$.run' AS fn,
       COALESCE(e.entry ->> '$.status', 'pending') AS status,
       e.entry ->> '$.started' AS started, e.entry ->> '$.finished' AS finished,
       e.entry ->> '$.error' AS error, e.entry -> '$.outputs' AS outputs,
       COALESCE(e.entry ->> '$.manual', 0) AS manual, e.entry ->> '$.skipped' AS skipped,
       s.value ->> '$.paused' AS paused, s.value -> '$.tags' AS tags,
       s.value -> '$.after' AS after, s.value ->> '$.when' AS "when",
       s.value ->> '$.doc' AS doc, e.entry -> '$.run_ids' AS run_ids,
       e.entry ->> '$.done' AS done, e.entry ->> '$.total' AS total, e.entry AS entry,
       pr.paused AS project_paused
FROM plans p
JOIN projects pr ON pr.name = p.project
JOIN json_each(p.doc, '$.steps') s
LEFT JOIN (SELECT st.project, x.key AS step, x.value AS entry
           FROM states st, json_each(st.doc, '$.steps') x) e
  ON e.project = p.project AND e.step = s.key;
CREATE VIEW messages AS
SELECT project, seq, at, thread, data ->> '$.from' AS "from", data ->> '$.to' AS "to",
       data ->> '$.body' AS body, COALESCE(data ->> '$.needs_reply', 1) AS needs_reply,
       data -> '$.data' AS data
FROM records WHERE kind = 'message';
CREATE VIEW step_changes AS
SELECT project, seq, at, step, data ->> '$.from' AS "from", data ->> '$.to' AS "to",
       data ->> '$.error' AS error, data -> '$.run_ids' AS run_ids
FROM records WHERE kind = 'step.status';
CREATE VIEW edits AS
SELECT project, rev, seq, at, author, reason, ops FROM plan_edits;
CREATE VIEW log AS
SELECT project, seq, at, kind, json_insert(data, '$.seq', seq, '$.at', at, '$.kind', kind)
       AS record
FROM records;
CREATE TRIGGER plans_insert AFTER INSERT ON plans WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.project; END;
CREATE TRIGGER plans_update AFTER UPDATE ON plans WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.project; END;
CREATE TRIGGER plans_delete AFTER DELETE ON plans WHEN OLD.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = OLD.project; END;
CREATE TRIGGER plan_edits_insert AFTER INSERT ON plan_edits WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.project; END;
CREATE TRIGGER plan_edits_update AFTER UPDATE ON plan_edits WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.project; END;
CREATE TRIGGER plan_edits_delete AFTER DELETE ON plan_edits WHEN OLD.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = OLD.project; END;
CREATE TRIGGER states_insert AFTER INSERT ON states WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1, changed = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE name = NEW.project; END;
CREATE TRIGGER states_update AFTER UPDATE ON states WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1, changed = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE name = NEW.project; END;
CREATE TRIGGER states_delete AFTER DELETE ON states WHEN OLD.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1, changed = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE name = OLD.project; END;
CREATE TRIGGER calls_insert AFTER INSERT ON calls WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.project; END;
CREATE TRIGGER calls_update AFTER UPDATE ON calls WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.project; END;
CREATE TRIGGER calls_delete AFTER DELETE ON calls WHEN OLD.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = OLD.project; END;
CREATE TRIGGER submissions_insert AFTER INSERT ON submissions WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.project; END;
CREATE TRIGGER submissions_update AFTER UPDATE ON submissions WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.project; END;
CREATE TRIGGER submissions_delete AFTER DELETE ON submissions WHEN OLD.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = OLD.project; END;
CREATE TRIGGER inbox_insert AFTER INSERT ON inbox WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.project; END;
CREATE TRIGGER inbox_update AFTER UPDATE ON inbox WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.project; END;
CREATE TRIGGER inbox_delete AFTER DELETE ON inbox WHEN OLD.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = OLD.project; END;
CREATE TRIGGER records_insert AFTER INSERT ON records WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.project; END;
CREATE TRIGGER records_update AFTER UPDATE ON records WHEN NEW.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.project; END;
CREATE TRIGGER records_delete AFTER DELETE ON records WHEN OLD.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1 WHERE name = OLD.project; END;
CREATE TRIGGER projects_update AFTER UPDATE ON projects WHEN NEW.ver = OLD.ver BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.name; END;
"""
