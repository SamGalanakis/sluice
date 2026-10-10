PRAGMA foreign_keys=OFF;
BEGIN;
CREATE TABLE home_meta (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  home_id TEXT NOT NULL CHECK (length(home_id) = 36),
  format_major INTEGER NOT NULL CHECK (format_major > 0),
  schema_version INTEGER NOT NULL CHECK (schema_version > 0),
  record_floor INTEGER NOT NULL DEFAULT 0 CHECK (record_floor >= 0),
  maintenance_settings TEXT NOT NULL DEFAULT '{}' CHECK (json_type(maintenance_settings) = 'object')
) STRICT;
CREATE TABLE projects (
  project_id TEXT PRIMARY KEY NOT NULL CHECK (length(project_id) = 36),
  name TEXT NOT NULL CHECK (length(name) > 0),
  description TEXT NOT NULL DEFAULT '',
  icon_generation INTEGER NOT NULL DEFAULT 0 CHECK (icon_generation >= 0),
  icon_text TEXT, icon_type TEXT, icon_hash TEXT,
  paused INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)),
  archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1)),
  resources_rev INTEGER NOT NULL DEFAULT 0 CHECK (resources_rev >= 0),
  settings_rev INTEGER NOT NULL DEFAULT 1 CHECK (settings_rev >= 1),
  created_at TEXT NOT NULL, changed_at TEXT, deleted_at TEXT,
  board TEXT,
  board_rev INTEGER NOT NULL DEFAULT 0 CHECK (board_rev >= 0),
  board_slots TEXT,
  board_doc TEXT,
  board_doc_rev INTEGER NOT NULL DEFAULT 0 CHECK (board_doc_rev >= 0),
  board_doc_at TEXT, board_doc_author TEXT,
  -- Automatic retiring of done units (SPEC §6.11): the age in seconds, off when null, and
  -- the unit-name patterns it never removes, a JSON array of strings.
  prune_done_after INTEGER CHECK (prune_done_after IS NULL OR prune_done_after > 0),
  prune_keep TEXT CHECK (prune_keep IS NULL OR json_type(prune_keep) = 'array'),
  CHECK (icon_text IS NULL OR icon_hash IS NULL),
  CHECK ((icon_type IS NULL) = (icon_hash IS NULL))
) STRICT;
CREATE TABLE plans (
  project_id TEXT PRIMARY KEY NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  rev INTEGER NOT NULL CHECK (rev >= 1),
  doc TEXT NOT NULL CHECK (json_type(doc) = 'object')
) STRICT;
CREATE TABLE plan_edits (
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  rev INTEGER NOT NULL CHECK (rev >= 1), seq INTEGER NOT NULL CHECK (seq > 0),
  at TEXT NOT NULL, author TEXT NOT NULL, reason TEXT NOT NULL,
  ops TEXT NOT NULL CHECK (json_type(ops) = 'array'),
  PRIMARY KEY (project_id, rev)
) STRICT;
CREATE TABLE inputs (
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  name TEXT NOT NULL, position INTEGER NOT NULL CHECK (position >= 0),
  declaration TEXT NOT NULL CHECK (json_valid(declaration)),
  value TEXT CHECK (value IS NULL OR json_valid(value)),
  generation INTEGER NOT NULL DEFAULT 1 CHECK (generation >= 1),
  PRIMARY KEY (project_id, name), UNIQUE (project_id, position)
) STRICT;
CREATE TABLE steps (
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  step_id TEXT NOT NULL CHECK (step_id NOT IN ('owner', 'orchestrator')),
  position INTEGER NOT NULL CHECK (position >= 0),
  generation INTEGER NOT NULL DEFAULT 1 CHECK (generation >= 1),
  work_generation INTEGER NOT NULL DEFAULT 1 CHECK (work_generation >= 1),
  declaration TEXT NOT NULL CHECK (json_type(declaration) = 'object'),
  status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','running','succeeded','failed','stale','skipped')),
  unit TEXT, paused TEXT CHECK (paused IS NULL OR json_type(paused) IN ('true','false','text')),
  outputs TEXT CHECK (outputs IS NULL OR json_type(outputs) = 'object'),
  error TEXT CHECK (error IS NULL OR json_type(error) = 'object'),
  skipped TEXT, manual INTEGER NOT NULL DEFAULT 0 CHECK (manual IN (0,1)),
  inputs_hash TEXT, result_id TEXT REFERENCES step_results(result_id),
  run_ids TEXT NOT NULL DEFAULT '[]' CHECK (json_type(run_ids) = 'array'),
  instances TEXT NOT NULL DEFAULT '{}' CHECK (json_type(instances) = 'object'),
  total INTEGER CHECK (total IS NULL OR total >= 0),
  done INTEGER NOT NULL DEFAULT 0 CHECK (done >= 0 AND (total IS NULL OR done <= total)),
  delivery_cursor INTEGER NOT NULL DEFAULT 0 CHECK (delivery_cursor >= 0),
  -- step_progress: the current run's latest values, never final; cleared when a run starts.
  progress TEXT CHECK (progress IS NULL OR json_type(progress) = 'object'),
  progress_at TEXT, progress_run TEXT,
  PRIMARY KEY (project_id, step_id), UNIQUE (project_id, position),
  FOREIGN KEY (project_id, result_id) REFERENCES step_results(project_id, result_id)
) STRICT;
CREATE TABLE attempts (
  attempt_id TEXT PRIMARY KEY NOT NULL,
  project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  step_id TEXT, generation INTEGER NOT NULL DEFAULT 1 CHECK (generation >= 1),
  work_generation INTEGER NOT NULL DEFAULT 1 CHECK (work_generation >= 1),
  item_index INTEGER NOT NULL DEFAULT -1 CHECK (item_index >= -1),
  phase TEXT NOT NULL CHECK (phase IN ('reserved','claimed','executing','completing','terminal')),
  request TEXT NOT NULL CHECK (json_type(request) = 'object'),
  inputs_hash TEXT NOT NULL, provenance TEXT NOT NULL DEFAULT '{}' CHECK (json_type(provenance) = 'object'),
  unit TEXT, spawn_attempted INTEGER NOT NULL DEFAULT 0 CHECK (spawn_attempted IN (0,1)),
  cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0,1)),
  created_at TEXT NOT NULL, finished_at TEXT,
  CHECK (step_id IS NULL OR project_id IS NOT NULL),
  UNIQUE (project_id, attempt_id)
) STRICT;
CREATE TABLE runs (
  run_id TEXT PRIMARY KEY NOT NULL,
  project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  attempt_id TEXT NOT NULL UNIQUE REFERENCES attempts(attempt_id),
  step_id TEXT, generation INTEGER NOT NULL DEFAULT 1 CHECK (generation >= 1),
  work_generation INTEGER NOT NULL DEFAULT 1 CHECK (work_generation >= 1),
  item_index INTEGER NOT NULL DEFAULT -1 CHECK (item_index >= -1),
  prev_run TEXT, unit TEXT, unit_name TEXT UNIQUE, boot_id TEXT,
  guardian_pid INTEGER, guardian_start TEXT, cgroup TEXT, socket_challenge TEXT,
  release_id TEXT, protocol_major INTEGER NOT NULL DEFAULT 1 CHECK (protocol_major >= 1),
  assigned_after INTEGER NOT NULL DEFAULT 0 CHECK (assigned_after >= 0),
  assigned_through INTEGER NOT NULL DEFAULT 0 CHECK (assigned_through >= assigned_after),
  started_at TEXT, created_at TEXT NOT NULL, finished_at TEXT,
  completion_id TEXT UNIQUE,
  completion_ack INTEGER NOT NULL DEFAULT 0 CHECK (completion_ack IN (0,1)),
  result TEXT CHECK (result IS NULL OR json_type(result) = 'object'),
  completion_action TEXT CHECK (completion_action IS NULL OR json_type(completion_action) = 'object'),
  action_outcome TEXT CHECK (action_outcome IS NULL OR json_type(action_outcome) = 'object'),
  stopped TEXT CHECK (stopped IS NULL OR json_type(stopped) = 'object'),
  CHECK (step_id IS NULL OR project_id IS NOT NULL),
  FOREIGN KEY (project_id, attempt_id) REFERENCES attempts(project_id, attempt_id),
  UNIQUE (project_id, run_id)
) STRICT;
CREATE TABLE submissions (
  run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
  project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  step_id TEXT, version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
  outputs TEXT NOT NULL CHECK (json_type(outputs) = 'object'), at TEXT NOT NULL,
  FOREIGN KEY (project_id, run_id) REFERENCES runs(project_id, run_id)
) STRICT;
CREATE TABLE calls (
  call_id TEXT PRIMARY KEY NOT NULL,
  project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  run_id TEXT UNIQUE REFERENCES runs(run_id), fn TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('pending','running','succeeded','failed')),
  inputs TEXT NOT NULL CHECK (json_type(inputs) = 'object'),
  outputs TEXT CHECK (outputs IS NULL OR json_type(outputs) = 'object'),
  error TEXT CHECK (error IS NULL OR json_type(error) = 'object'),
  direct INTEGER NOT NULL DEFAULT 0 CHECK (direct IN (0,1)),
  author TEXT, created_at TEXT NOT NULL, finished_at TEXT,
  FOREIGN KEY (project_id, run_id) REFERENCES runs(project_id, run_id)
) STRICT;
CREATE TABLE step_results (
  result_id TEXT PRIMARY KEY NOT NULL,
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  step_id TEXT NOT NULL, generation INTEGER NOT NULL CHECK (generation >= 1),
  work_generation INTEGER NOT NULL DEFAULT 1 CHECK (work_generation >= 1),
  attempt_id TEXT, unit TEXT, declaration TEXT NOT NULL CHECK (json_type(declaration) = 'object'),
  inputs TEXT CHECK (inputs IS NULL OR json_type(inputs) = 'object'), inputs_hash TEXT,
  status TEXT NOT NULL CHECK (status IN ('succeeded','failed','stale','skipped')),
  outputs TEXT CHECK (outputs IS NULL OR json_type(outputs) = 'object'),
  error TEXT CHECK (error IS NULL OR json_type(error) = 'object'),
  manual INTEGER NOT NULL DEFAULT 0 CHECK (manual IN (0,1)),
  run_ids TEXT NOT NULL DEFAULT '[]' CHECK (json_type(run_ids) = 'array'),
  recorded_at TEXT NOT NULL, removed_at TEXT,
  UNIQUE (project_id, result_id)
) STRICT;
CREATE TABLE resources (
  scope TEXT NOT NULL, project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  name TEXT NOT NULL, declaration TEXT NOT NULL CHECK (json_valid(declaration)),
  capacity INTEGER CHECK (capacity IS NULL OR capacity >= 0),
  observed_capacity INTEGER CHECK (observed_capacity IS NULL OR observed_capacity >= 0),
  revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1), observed_at TEXT,
  error TEXT CHECK (error IS NULL OR json_type(error) = 'object'),
  CHECK (scope = coalesce(project_id, 'home')), PRIMARY KEY (scope, name)
) STRICT;
CREATE TABLE leases (
  lease_id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  run_id TEXT NOT NULL REFERENCES runs(run_id), request_id TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL DEFAULT 'section' CHECK (kind IN ('section','needs')),
  group_id TEXT,
  scope TEXT NOT NULL, resource TEXT NOT NULL, amount INTEGER NOT NULL CHECK (amount >= 0),
  priority INTEGER NOT NULL DEFAULT 0,
  state TEXT NOT NULL CHECK (state IN ('waiting','held','released','cancelled')),
  grant_id TEXT UNIQUE, release_id TEXT UNIQUE,
  created_at TEXT NOT NULL, granted_at TEXT, released_at TEXT,
  FOREIGN KEY (scope, resource) REFERENCES resources(scope, name),
  FOREIGN KEY (project_id, run_id) REFERENCES runs(project_id, run_id)
) STRICT;
CREATE TABLE messages (
  id INTEGER PRIMARY KEY CHECK (id > 0),
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  thread TEXT NOT NULL, "from" TEXT NOT NULL, "to" TEXT, title TEXT, body TEXT NOT NULL,
  needs_reply INTEGER NOT NULL DEFAULT 0 CHECK (needs_reply IN (0,1)),
  reply_to INTEGER, answer TEXT CHECK (answer IS NULL OR json_type(answer) = 'object'),
  ui TEXT, input TEXT, data TEXT CHECK (data IS NULL OR json_valid(data)),
  run_id TEXT, at TEXT NOT NULL, claimed_by TEXT,
  resolved_by INTEGER, closed_at TEXT, read_at TEXT,
  FOREIGN KEY (project_id, reply_to) REFERENCES messages(project_id, id),
  FOREIGN KEY (project_id, resolved_by) REFERENCES messages(project_id, id),
  UNIQUE (project_id, id)
) STRICT;
CREATE TABLE question_attachments (
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  message_id INTEGER NOT NULL, run_id TEXT NOT NULL REFERENCES runs(run_id),
  step_id TEXT NOT NULL, generation INTEGER NOT NULL CHECK (generation >= 1),
  item_index INTEGER NOT NULL DEFAULT -1 CHECK (item_index >= -1), title TEXT NOT NULL,
  attached_at TEXT NOT NULL, detached_at TEXT,
  PRIMARY KEY (project_id, message_id, run_id),
  FOREIGN KEY (project_id, message_id) REFERENCES messages(project_id, id) ON DELETE CASCADE,
  FOREIGN KEY (project_id, run_id) REFERENCES runs(project_id, run_id)
) STRICT;
CREATE TABLE message_deliveries (
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  run_id TEXT NOT NULL REFERENCES runs(run_id), message_id INTEGER NOT NULL,
  assigned_at TEXT NOT NULL, acknowledged_at TEXT,
  PRIMARY KEY (project_id, run_id, message_id),
  FOREIGN KEY (project_id, message_id) REFERENCES messages(project_id, id) ON DELETE CASCADE,
  FOREIGN KEY (project_id, run_id) REFERENCES runs(project_id, run_id)
) STRICT;
CREATE TABLE readers (
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  identity TEXT NOT NULL, stream TEXT NOT NULL, thread TEXT NOT NULL DEFAULT '',
  cursor INTEGER NOT NULL DEFAULT 0 CHECK (cursor >= 0),
  unread_alert_min INTEGER CHECK (unread_alert_min IS NULL OR unread_alert_min >= 0),
  heartbeat_at TEXT, PRIMARY KEY (project_id, identity, stream, thread)
) STRICT;
CREATE TABLE records (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  at TEXT NOT NULL, kind TEXT NOT NULL,
  payload_version INTEGER NOT NULL DEFAULT 1 CHECK (payload_version >= 1),
  payload TEXT NOT NULL CHECK (json_type(payload) = 'object'),
  step_id TEXT, call_id TEXT, thread TEXT, run_id TEXT,
  CHECK (coalesce(json_type(payload, '$.kind') = 'text' AND json_extract(payload, '$.kind') = kind, 0))
) STRICT;
CREATE TABLE change_versions (
  scope TEXT NOT NULL, project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  view TEXT NOT NULL, version INTEGER NOT NULL CHECK (version >= 1),
  CHECK (scope = coalesce(project_id, 'home')), PRIMARY KEY (scope, view)
) STRICT;
CREATE TABLE maintenance (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  mode TEXT NOT NULL DEFAULT 'normal' CHECK (mode IN ('normal','drain','cutover')),
  owner TEXT, paused_projects TEXT NOT NULL DEFAULT '[]' CHECK (json_type(paused_projects) = 'array'),
  scheduler_owner TEXT, scheduler_lease_until TEXT, revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
  settings TEXT NOT NULL DEFAULT '{}' CHECK (json_type(settings) = 'object'), changed_at TEXT
) STRICT;
CREATE TABLE artifact_jobs (
  job_id TEXT PRIMARY KEY NOT NULL,
  project_id TEXT REFERENCES projects(project_id), run_id TEXT,
  kind TEXT NOT NULL, generation INTEGER NOT NULL CHECK (generation >= 1),
  path TEXT NOT NULL, manifest TEXT NOT NULL DEFAULT '{}' CHECK (json_type(manifest) = 'object'),
  state TEXT NOT NULL CHECK (state IN ('pending','running','done','failed')),
  error TEXT CHECK (error IS NULL OR json_type(error) = 'object'),
  created_at TEXT NOT NULL, finished_at TEXT, UNIQUE (path, generation, kind)
) STRICT;
CREATE TABLE sessions (
  run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(run_id),
  project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  engine TEXT NOT NULL, cwd TEXT NOT NULL, session_id TEXT NOT NULL,
  metadata TEXT NOT NULL DEFAULT '{}' CHECK (json_type(metadata) = 'object'), recorded_at TEXT NOT NULL,
  FOREIGN KEY (project_id, run_id) REFERENCES runs(project_id, run_id)
) STRICT;
CREATE TABLE notification_attempts (
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  message_id INTEGER NOT NULL, attempt_id TEXT NOT NULL UNIQUE,
  outcome TEXT NOT NULL CHECK (outcome IN ('reserved','dispatched','failed','uncertain')),
  reserved_at TEXT NOT NULL, finished_at TEXT, stderr TEXT,
  error TEXT CHECK (error IS NULL OR json_type(error) = 'object'),
  PRIMARY KEY (project_id, message_id),
  FOREIGN KEY (project_id, message_id) REFERENCES messages(project_id, id) ON DELETE CASCADE
) STRICT;
INSERT INTO "home_meta"("singleton","home_id","format_major","schema_version","record_floor","maintenance_settings") VALUES(1,'01a126ea-2808-750a-bf34-ad100981e812',1,1,7,'{"record_floors":{"01a126ea-2815-707e-a7ed-9034d4906573":7}}');
INSERT INTO "projects"("project_id","name","description","icon_generation","icon_text","icon_type","icon_hash","paused","archived","resources_rev","settings_rev","created_at","changed_at","deleted_at","board","board_rev","board_slots","board_doc","board_doc_rev","board_doc_at","board_doc_author","prune_done_after","prune_keep") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','alpha','',0,NULL,NULL,NULL,0,0,1,1,'2026-10-10T17:44:02.32516525Z',NULL,NULL,NULL,0,NULL,NULL,0,NULL,NULL,NULL,NULL);
INSERT INTO "projects"("project_id","name","description","icon_generation","icon_text","icon_type","icon_hash","paused","archived","resources_rev","settings_rev","created_at","changed_at","deleted_at","board","board_rev","board_slots","board_doc","board_doc_rev","board_doc_at","board_doc_author","prune_done_after","prune_keep") VALUES('01a126ea-29c6-72e9-a93b-2c6e39d0be03','beta','',0,NULL,NULL,NULL,0,0,0,1,'2026-10-10T17:44:02.758913908Z',NULL,NULL,'root = Doc("beta")',1,'{"notes":{"markdown":"m","at":"2026-10-01T00:00:00Z","author":"owner"}}',NULL,0,NULL,NULL,NULL,NULL);
INSERT INTO "projects"("project_id","name","description","icon_generation","icon_text","icon_type","icon_hash","paused","archived","resources_rev","settings_rev","created_at","changed_at","deleted_at","board","board_rev","board_slots","board_doc","board_doc_rev","board_doc_at","board_doc_author","prune_done_after","prune_keep") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17','gamma','',0,NULL,NULL,NULL,0,1,0,2,'2026-10-10T17:44:02.862661345Z','2026-10-10T17:44:02.878229719Z',NULL,NULL,0,NULL,NULL,0,NULL,NULL,NULL,NULL);
INSERT INTO "projects"("project_id","name","description","icon_generation","icon_text","icon_type","icon_hash","paused","archived","resources_rev","settings_rev","created_at","changed_at","deleted_at","board","board_rev","board_slots","board_doc","board_doc_rev","board_doc_at","board_doc_author","prune_done_after","prune_keep") VALUES('01a126ea-2a3f-708e-9298-7a87f0bba7a2','delta','',0,NULL,NULL,NULL,0,1,0,3,'2026-10-10T17:44:02.879187501Z','2026-10-10T17:44:02.887707358Z','2026-10-10T17:44:02.887707358Z',NULL,0,NULL,NULL,0,NULL,NULL,NULL,NULL);
INSERT INTO "plans"("project_id","rev","doc") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',21,'{"inputs":{"repo":{"type":"string","doc":"The repo"},"limit":{"type":"int","doc":"How many"}},"outputs":{},"steps":{"c":{"run":"fixture.echo","paused":"after review","priority":7,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}},"a":{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}}},"b":{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}},"paused":"hold b"},"f":{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[1,2,3]}},"paused":true},"g":{"run":"fixture.echo","after":["unit:build?"],"in":{"value":{"source":"c/value"}},"doc":"Gate on build","tags":["late"]},"h":{"run":"fixture.echo","after":["unit:build?","a"],"in":{"value":{"source":"c/value"}},"doc":"Gate on build","tags":["late"]},"fig-1-fork":{"run":"fixture.echo","in":{"value":{"default":"fork"}},"tags":["unit:fig-1","wave-3"]},"fig-1-work":{"run":"fixture.echo","after":["fig-1-fork"],"in":{"value":{"source":"fig-1-fork/value"}},"tags":["unit:fig-1","wave-3"]}}}');
INSERT INTO "plans"("project_id","rev","doc") VALUES('01a126ea-29c6-72e9-a93b-2c6e39d0be03',1,'{"steps":{}}');
INSERT INTO "plans"("project_id","rev","doc") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17',5,'{"steps":{"x":{"run":"fixture.submit","in":{"value":{"source":"k"}},"outputs":{"extra":"string"}}},"inputs":{"k":"boolean"},"outputs":{"out":{"source":"x/value"}}}');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',1,1,'2026-10-10T17:44:02.325493378Z','generator','project created','[]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',2,2,'2026-10-10T17:44:02.332549359Z','generator','generated','[{"op":"add","path":"/steps/a","value":{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}}}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',3,3,'2026-10-10T17:44:02.336400948Z','generator','generated','[{"op":"add","path":"/inputs","value":{"repo":"string","limit":{"type":"int","doc":"How many"}}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',4,4,'2026-10-10T17:44:02.34102365Z','generator','generated','[{"op":"add","path":"/steps/b","value":{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}}}},{"op":"add","path":"/steps/c","value":{"run":"fixture.echo","paused":"after review","priority":5,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',5,14,'2026-10-10T17:44:02.383959485Z','generator','generated','[{"op":"add","path":"/outputs","value":{"result":{"source":"b/value"}}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',6,15,'2026-10-10T17:44:02.390379018Z','generator','generated','[{"op":"replace","path":"/steps","value":{"c":{"run":"fixture.echo","paused":"after review","priority":5,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}},"a":{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}}},"d":{"run":"fixture.echo","in":{"value":{"default":"d"}}},"b":{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}}}}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',7,16,'2026-10-10T17:44:02.396333115Z','generator','remove d','[{"op":"remove","path":"/steps/d"}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',8,17,'2026-10-10T17:44:02.401937183Z','generator','add e','[{"op":"add","path":"/steps/e","value":{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[1,2,3]}},"paused":true}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',9,18,'2026-10-10T17:44:02.407559636Z','generator','generated','[{"op":"move","from":"/steps/e","path":"/steps/f"}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',10,19,'2026-10-10T17:44:02.411861954Z','generator','generated','[{"op":"remove","path":"/outputs"}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',11,20,'2026-10-10T17:44:02.415434388Z','generator','generated','[{"op":"add","path":"/outputs","value":{}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',12,21,'2026-10-10T17:44:02.418867139Z','generator','generated','[{"op":"replace","path":"/inputs/repo","value":{"type":"string","doc":"The repo"}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',13,22,'2026-10-10T17:44:02.422683904Z','generator','add g','[{"op":"add","path":"/steps/g","value":{"run":"fixture.echo","after":["unit:build?"],"in":{"value":{"source":"c/value"}}}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',14,23,'2026-10-10T17:44:02.427386735Z','generator','generated','[{"op":"replace","path":"","value":{"inputs":{"repo":{"type":"string","doc":"The repo"},"limit":{"type":"int","doc":"How many"}},"outputs":{},"steps":{"c":{"run":"fixture.echo","paused":"after review","priority":5,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}},"a":{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}}},"b":{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}}},"f":{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[1,2,3]}},"paused":true},"g":{"run":"fixture.echo","after":["unit:build?"],"in":{"value":{"source":"c/value"}},"doc":"Gate on build"}}}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',15,24,'2026-10-10T17:44:02.431552216Z','generator','generated','[{"op":"test","path":"/steps/g/run","value":"fixture.echo"},{"op":"add","path":"/steps/g/tags","value":["late"]}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',16,25,'2026-10-10T17:44:02.43536917Z','generator','generated','[{"op":"copy","from":"/steps/g","path":"/steps/h"}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',17,26,'2026-10-10T17:44:02.441414499Z','generator','unit','[{"op":"add","path":"/steps/fig-1-fork","value":{"run":"fixture.echo","in":{"value":{"default":"fork"}},"tags":["unit:fig-1"]}},{"op":"add","path":"/steps/fig-1-work","value":{"run":"fixture.echo","after":["fig-1-fork"],"in":{"value":{"source":"fig-1-fork/value"}},"tags":["unit:fig-1"]}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',18,27,'2026-10-10T17:44:02.455698625Z','generator','hold b','[{"op":"add","path":"/steps/b/paused","value":"hold b"}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',19,28,'2026-10-10T17:44:02.538590267Z','generator','tag','[{"op":"replace","path":"/steps/fig-1-fork/tags","value":["unit:fig-1","wave-3"]},{"op":"replace","path":"/steps/fig-1-work/tags","value":["unit:fig-1","wave-3"]}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',20,29,'2026-10-10T17:44:02.610588084Z','generator','gate h','[{"op":"replace","path":"/steps/h/after","value":["unit:build?","a"]}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2815-707e-a7ed-9034d4906573',21,30,'2026-10-10T17:44:02.651693504Z','generator','prioritize c','[{"op":"replace","path":"/steps/c","value":{"run":"fixture.echo","paused":"after review","priority":7,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-29c6-72e9-a93b-2c6e39d0be03',1,38,'2026-10-10T17:44:02.759231215Z','generator','project created','[]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17',1,40,'2026-10-10T17:44:02.862958675Z','generator','project created','[]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17',2,41,'2026-10-10T17:44:02.869069006Z','generator','generated','[{"op":"add","path":"/steps/x","value":{"run":"fixture.submit","in":{"value":{"default":true}},"outputs":{"extra":"string"}}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17',3,42,'2026-10-10T17:44:02.871676674Z','generator','generated','[{"op":"add","path":"/inputs","value":{"k":"boolean"}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17',4,43,'2026-10-10T17:44:02.874101699Z','generator','generated','[{"op":"replace","path":"/steps/x/in/value","value":{"source":"k"}}]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17',5,44,'2026-10-10T17:44:02.876677016Z','generator','generated','[{"op":"add","path":"/outputs","value":{"out":{"source":"x/value"}}}]');
INSERT INTO "inputs"("project_id","name","position","declaration","value","generation") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','repo',0,'{"type":"string","doc":"The repo"}','"/srv/repo"',3);
INSERT INTO "inputs"("project_id","name","position","declaration","value","generation") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','limit',1,'{"type":"int","doc":"How many"}','3',3);
INSERT INTO "inputs"("project_id","name","position","declaration","value","generation") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17','k',0,'{"type":"boolean","doc":null}',NULL,3);
INSERT INTO "steps"("project_id","step_id","position","generation","work_generation","declaration","status","unit","paused","outputs","error","skipped","manual","inputs_hash","result_id","run_ids","instances","total","done","delivery_cursor","progress","progress_at","progress_run") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','a',1,2,1,'{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}}}','succeeded','build','false','{"value":1}',NULL,NULL,0,'4b86ff53b63e713aba37d590edcb3c2ab6d3d429230dd06b57fbfbe31a567046','01a126ea-2839-7196-9b99-3e2e29373b8f','["01a126ea-282f-7236-a0a1-76ac224c360d"]','{}',NULL,0,0,NULL,NULL,NULL);
INSERT INTO "steps"("project_id","step_id","position","generation","work_generation","declaration","status","unit","paused","outputs","error","skipped","manual","inputs_hash","result_id","run_ids","instances","total","done","delivery_cursor","progress","progress_at","progress_run") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','b',2,4,1,'{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}},"paused":"hold b"}','succeeded','build','"hold b"','{"value":1}',NULL,NULL,0,'4b86ff53b63e713aba37d590edcb3c2ab6d3d429230dd06b57fbfbe31a567046','01a126ea-2849-73e7-ae65-14934ff09ebe','["01a126ea-283f-751e-9e47-f6be278d1c10"]','{}',NULL,0,0,NULL,NULL,NULL);
INSERT INTO "steps"("project_id","step_id","position","generation","work_generation","declaration","status","unit","paused","outputs","error","skipped","manual","inputs_hash","result_id","run_ids","instances","total","done","delivery_cursor","progress","progress_at","progress_run") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','c',0,4,1,'{"run":"fixture.echo","paused":"after review","priority":7,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}}','pending','c','"after review"',NULL,NULL,NULL,0,NULL,NULL,'[]','{}',NULL,0,0,NULL,NULL,NULL);
INSERT INTO "steps"("project_id","step_id","position","generation","work_generation","declaration","status","unit","paused","outputs","error","skipped","manual","inputs_hash","result_id","run_ids","instances","total","done","delivery_cursor","progress","progress_at","progress_run") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','f',3,9,1,'{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[1,2,3]}},"paused":true}','pending','f','true',NULL,NULL,NULL,0,NULL,NULL,'[]','{}',NULL,0,0,NULL,NULL,NULL);
INSERT INTO "steps"("project_id","step_id","position","generation","work_generation","declaration","status","unit","paused","outputs","error","skipped","manual","inputs_hash","result_id","run_ids","instances","total","done","delivery_cursor","progress","progress_at","progress_run") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','g',4,13,1,'{"run":"fixture.echo","after":["unit:build?"],"in":{"value":{"source":"c/value"}},"doc":"Gate on build","tags":["late"]}','pending','g','false',NULL,NULL,NULL,0,NULL,NULL,'[]','{}',NULL,0,0,NULL,NULL,NULL);
INSERT INTO "steps"("project_id","step_id","position","generation","work_generation","declaration","status","unit","paused","outputs","error","skipped","manual","inputs_hash","result_id","run_ids","instances","total","done","delivery_cursor","progress","progress_at","progress_run") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','h',5,16,1,'{"run":"fixture.echo","after":["unit:build?","a"],"in":{"value":{"source":"c/value"}},"doc":"Gate on build","tags":["late"]}','pending','h','false',NULL,NULL,NULL,0,NULL,NULL,'[]','{}',NULL,0,0,NULL,NULL,NULL);
INSERT INTO "steps"("project_id","step_id","position","generation","work_generation","declaration","status","unit","paused","outputs","error","skipped","manual","inputs_hash","result_id","run_ids","instances","total","done","delivery_cursor","progress","progress_at","progress_run") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','fig-1-fork',6,17,1,'{"run":"fixture.echo","in":{"value":{"default":"fork"}},"tags":["unit:fig-1","wave-3"]}','failed','fig-1','false','{}','{"error":"process_lost","message":"guardian gone"}',NULL,0,'8f2d94f4253e08f77f0d140487ad86c6563dd116df68643f2cb66fc6ef0ab9f1','01a126ea-29ba-7504-aa57-95c5558f4303','["01a126ea-29a0-70f1-abea-ed771001d24e"]','{}',NULL,0,0,NULL,NULL,NULL);
INSERT INTO "steps"("project_id","step_id","position","generation","work_generation","declaration","status","unit","paused","outputs","error","skipped","manual","inputs_hash","result_id","run_ids","instances","total","done","delivery_cursor","progress","progress_at","progress_run") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','fig-1-work',7,17,1,'{"run":"fixture.echo","after":["fig-1-fork"],"in":{"value":{"source":"fig-1-fork/value"}},"tags":["unit:fig-1","wave-3"]}','pending','fig-1','false',NULL,NULL,NULL,0,NULL,NULL,'[]','{}',NULL,0,0,NULL,NULL,NULL);
INSERT INTO "steps"("project_id","step_id","position","generation","work_generation","declaration","status","unit","paused","outputs","error","skipped","manual","inputs_hash","result_id","run_ids","instances","total","done","delivery_cursor","progress","progress_at","progress_run") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17','x',0,2,1,'{"run":"fixture.submit","in":{"value":{"source":"k"}},"outputs":{"extra":"string"}}','pending','x','false',NULL,NULL,NULL,0,NULL,NULL,'[]','{}',NULL,0,0,NULL,NULL,NULL);
INSERT INTO "attempts"("attempt_id","project_id","step_id","generation","work_generation","item_index","phase","request","inputs_hash","provenance","unit","spawn_attempted","cancel_requested","created_at","finished_at") VALUES('01a126ea-282f-7236-a0a1-76ab334bb396','01a126ea-2815-707e-a7ed-9034d4906573','a',2,1,-1,'terminal','{"declaration":{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}}},"inputs":{"value":1},"effective_inputs":{"value":1},"returns":{"value":"Any"},"declared":{},"item_count":null,"provenance":{"runtime":{"capability":"01a126ea-282f-7236-a0a1-76a94a3dd2ab01a126ea-282f-7236-a0a1-76aa1c555c5e","execution":null,"completion":{"revision":4,"document":{"steps":{"a":{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}},"doc":"tampered"},"b":{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}}},"c":{"run":"fixture.echo","paused":"after review","priority":5,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}}},"inputs":{"repo":"string","limit":{"type":"int","doc":"How many"}}},"signatures":{"fixture.echo":{"inputs":{"value":"Any"},"outputs":{"value":"Any"},"submits":{},"open":false}}}},"files":{}},"listens":false,"runtime_reconciliation_error":null}','4b86ff53b63e713aba37d590edcb3c2ab6d3d429230dd06b57fbfbe31a567046','{"runtime":{"capability":"01a126ea-282f-7236-a0a1-76a94a3dd2ab01a126ea-282f-7236-a0a1-76aa1c555c5e","execution":null,"completion":{"revision":4,"document":{"steps":{"a":{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}},"doc":"tampered"},"b":{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}}},"c":{"run":"fixture.echo","paused":"after review","priority":5,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}}},"inputs":{"repo":"string","limit":{"type":"int","doc":"How many"}}},"signatures":{"fixture.echo":{"inputs":{"value":"Any"},"outputs":{"value":"Any"},"submits":{},"open":false}}}},"files":{}}','build',1,0,'2026-10-10T17:44:02.352718742Z','2026-10-10T17:44:02.360974572Z');
INSERT INTO "attempts"("attempt_id","project_id","step_id","generation","work_generation","item_index","phase","request","inputs_hash","provenance","unit","spawn_attempted","cancel_requested","created_at","finished_at") VALUES('01a126ea-283f-751e-9e47-f6bd889b8d8d','01a126ea-2815-707e-a7ed-9034d4906573','b',4,1,-1,'terminal','{"declaration":{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}}},"inputs":{"value":1},"effective_inputs":{"value":1},"returns":{"value":"Any"},"declared":{},"item_count":null,"provenance":{"runtime":{"capability":"01a126ea-283f-751e-9e47-f6bbf7fdb15101a126ea-283f-751e-9e47-f6bc3f793b2e","execution":null,"completion":{"revision":4,"document":{"steps":{"a":{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}}},"b":{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}}},"c":{"run":"fixture.echo","paused":"after review","priority":5,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}}},"inputs":{"repo":"string","limit":{"type":"int","doc":"How many"}}},"signatures":{"fixture.echo":{"inputs":{"value":"Any"},"outputs":{"value":"Any"},"submits":{},"open":false}}}},"files":{}},"listens":false,"runtime_reconciliation_error":null}','4b86ff53b63e713aba37d590edcb3c2ab6d3d429230dd06b57fbfbe31a567046','{"runtime":{"capability":"01a126ea-283f-751e-9e47-f6bbf7fdb15101a126ea-283f-751e-9e47-f6bc3f793b2e","execution":null,"completion":{"revision":4,"document":{"steps":{"a":{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}}},"b":{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}}},"c":{"run":"fixture.echo","paused":"after review","priority":5,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}}},"inputs":{"repo":"string","limit":{"type":"int","doc":"How many"}}},"signatures":{"fixture.echo":{"inputs":{"value":"Any"},"outputs":{"value":"Any"},"submits":{},"open":false}}}},"files":{}}','build',1,0,'2026-10-10T17:44:02.368820349Z','2026-10-10T17:44:02.377077061Z');
INSERT INTO "attempts"("attempt_id","project_id","step_id","generation","work_generation","item_index","phase","request","inputs_hash","provenance","unit","spawn_attempted","cancel_requested","created_at","finished_at") VALUES('01a126ea-29a0-70f1-abea-ed76d925230c','01a126ea-2815-707e-a7ed-9034d4906573','fig-1-fork',17,1,-1,'terminal','{"declaration":{"run":"fixture.echo","in":{"value":{"default":"fork"}},"tags":["unit:fig-1","wave-3"]},"inputs":{"value":"fork"},"effective_inputs":{"value":"fork"},"returns":{"value":"Any"},"declared":{},"item_count":null,"provenance":{"runtime":{"capability":"01a126ea-29a0-70f1-abea-ed74227dc2ce01a126ea-29a0-70f1-abea-ed756738f29e","execution":null,"completion":{"revision":21,"document":{"inputs":{"repo":{"type":"string","doc":"The repo"},"limit":{"type":"int","doc":"How many"}},"outputs":{},"steps":{"c":{"run":"fixture.echo","paused":"after review","priority":7,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}},"a":{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}}},"b":{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}},"paused":"hold b"},"f":{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[1,2,3]}},"paused":true},"g":{"run":"fixture.echo","after":["unit:build?"],"in":{"value":{"source":"c/value"}},"doc":"Gate on build","tags":["late"]},"h":{"run":"fixture.echo","after":["unit:build?","a"],"in":{"value":{"source":"c/value"}},"doc":"Gate on build","tags":["late"]},"fig-1-fork":{"run":"fixture.echo","in":{"value":{"default":"fork"}},"tags":["unit:fig-1","wave-3"]},"fig-1-work":{"run":"fixture.echo","after":["fig-1-fork"],"in":{"value":{"source":"fig-1-fork/value"}},"tags":["unit:fig-1","wave-3"]}}},"signatures":{"fixture.echo":{"inputs":{"value":"Any"},"outputs":{"value":"Any"},"submits":{},"open":false}}}},"files":{}},"listens":false,"runtime_reconciliation_error":null}','8f2d94f4253e08f77f0d140487ad86c6563dd116df68643f2cb66fc6ef0ab9f1','{"runtime":{"capability":"01a126ea-29a0-70f1-abea-ed74227dc2ce01a126ea-29a0-70f1-abea-ed756738f29e","execution":null,"completion":{"revision":21,"document":{"inputs":{"repo":{"type":"string","doc":"The repo"},"limit":{"type":"int","doc":"How many"}},"outputs":{},"steps":{"c":{"run":"fixture.echo","paused":"after review","priority":7,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}},"a":{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}}},"b":{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}},"paused":"hold b"},"f":{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[1,2,3]}},"paused":true},"g":{"run":"fixture.echo","after":["unit:build?"],"in":{"value":{"source":"c/value"}},"doc":"Gate on build","tags":["late"]},"h":{"run":"fixture.echo","after":["unit:build?","a"],"in":{"value":{"source":"c/value"}},"doc":"Gate on build","tags":["late"]},"fig-1-fork":{"run":"fixture.echo","in":{"value":{"default":"fork"}},"tags":["unit:fig-1","wave-3"]},"fig-1-work":{"run":"fixture.echo","after":["fig-1-fork"],"in":{"value":{"source":"fig-1-fork/value"}},"tags":["unit:fig-1","wave-3"]}}},"signatures":{"fixture.echo":{"inputs":{"value":"Any"},"outputs":{"value":"Any"},"submits":{},"open":false}}}},"files":{}}','fig-1',1,0,'2026-10-10T17:44:02.721577883Z','2026-10-10T17:44:02.74636607Z');
INSERT INTO "runs"("run_id","project_id","attempt_id","step_id","generation","work_generation","item_index","prev_run","unit","unit_name","boot_id","guardian_pid","guardian_start","cgroup","socket_challenge","release_id","protocol_major","assigned_after","assigned_through","started_at","created_at","finished_at","completion_id","completion_ack","result","completion_action","action_outcome","stopped") VALUES('01a126ea-282f-7236-a0a1-76ac224c360d','01a126ea-2815-707e-a7ed-9034d4906573','01a126ea-282f-7236-a0a1-76ab334bb396','a',2,1,-1,NULL,'build',NULL,NULL,NULL,NULL,NULL,NULL,'runtime-v1',1,0,0,NULL,'2026-10-10T17:44:02.352718742Z','2026-10-10T17:44:02.360974572Z','complete-01a126ea-282f-7236-a0a1-76ac224c360d',1,'{"status":"succeeded","outputs":{"value":1},"error":null,"result":"01a126ea-2839-7196-9b99-3e2e29373b8f","action":null}',NULL,NULL,NULL);
INSERT INTO "runs"("run_id","project_id","attempt_id","step_id","generation","work_generation","item_index","prev_run","unit","unit_name","boot_id","guardian_pid","guardian_start","cgroup","socket_challenge","release_id","protocol_major","assigned_after","assigned_through","started_at","created_at","finished_at","completion_id","completion_ack","result","completion_action","action_outcome","stopped") VALUES('01a126ea-283f-751e-9e47-f6be278d1c10','01a126ea-2815-707e-a7ed-9034d4906573','01a126ea-283f-751e-9e47-f6bd889b8d8d','b',4,1,-1,NULL,'build',NULL,NULL,NULL,NULL,NULL,NULL,'runtime-v1',1,0,0,NULL,'2026-10-10T17:44:02.368820349Z','2026-10-10T17:44:02.377077061Z','complete-01a126ea-283f-751e-9e47-f6be278d1c10',1,'{"status":"succeeded","outputs":{"value":1},"error":null,"result":"01a126ea-2849-73e7-ae65-14934ff09ebe","action":null}',NULL,NULL,NULL);
INSERT INTO "runs"("run_id","project_id","attempt_id","step_id","generation","work_generation","item_index","prev_run","unit","unit_name","boot_id","guardian_pid","guardian_start","cgroup","socket_challenge","release_id","protocol_major","assigned_after","assigned_through","started_at","created_at","finished_at","completion_id","completion_ack","result","completion_action","action_outcome","stopped") VALUES('01a126ea-29a0-70f1-abea-ed771001d24e','01a126ea-2815-707e-a7ed-9034d4906573','01a126ea-29a0-70f1-abea-ed76d925230c','fig-1-fork',17,1,-1,NULL,'fig-1',NULL,NULL,NULL,NULL,NULL,NULL,'runtime-v1',1,0,0,NULL,'2026-10-10T17:44:02.721577883Z','2026-10-10T17:44:02.74636607Z','complete-01a126ea-29a0-70f1-abea-ed771001d24e',1,'{"status":"failed","outputs":{},"error":{"error":"process_lost","message":"guardian gone"},"result":"01a126ea-29ba-7504-aa57-95c5558f4303","action":null}',NULL,NULL,NULL);
INSERT INTO "step_results"("result_id","project_id","step_id","generation","work_generation","attempt_id","unit","declaration","inputs","inputs_hash","status","outputs","error","manual","run_ids","recorded_at","removed_at") VALUES('01a126ea-2839-7196-9b99-3e2e29373b8f','01a126ea-2815-707e-a7ed-9034d4906573','a',2,1,'01a126ea-282f-7236-a0a1-76ab334bb396','build','{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}}}','{"value":1}','4b86ff53b63e713aba37d590edcb3c2ab6d3d429230dd06b57fbfbe31a567046','succeeded','{"value":1}',NULL,0,'["01a126ea-282f-7236-a0a1-76ac224c360d"]','2026-10-10T17:44:02.361497336Z',NULL);
INSERT INTO "step_results"("result_id","project_id","step_id","generation","work_generation","attempt_id","unit","declaration","inputs","inputs_hash","status","outputs","error","manual","run_ids","recorded_at","removed_at") VALUES('01a126ea-2849-73e7-ae65-14934ff09ebe','01a126ea-2815-707e-a7ed-9034d4906573','b',4,1,'01a126ea-283f-751e-9e47-f6bd889b8d8d','build','{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}}}','{"value":1}','4b86ff53b63e713aba37d590edcb3c2ab6d3d429230dd06b57fbfbe31a567046','succeeded','{"value":1}',NULL,0,'["01a126ea-283f-751e-9e47-f6be278d1c10"]','2026-10-10T17:44:02.37758129Z',NULL);
INSERT INTO "step_results"("result_id","project_id","step_id","generation","work_generation","attempt_id","unit","declaration","inputs","inputs_hash","status","outputs","error","manual","run_ids","recorded_at","removed_at") VALUES('01a126ea-29ba-7504-aa57-95c5558f4303','01a126ea-2815-707e-a7ed-9034d4906573','fig-1-fork',17,1,'01a126ea-29a0-70f1-abea-ed76d925230c','fig-1','{"run":"fixture.echo","in":{"value":{"default":"fork"}},"tags":["unit:fig-1","wave-3"]}','{"value":"fork"}','8f2d94f4253e08f77f0d140487ad86c6563dd116df68643f2cb66fc6ef0ab9f1','failed','{}','{"error":"process_lost","message":"guardian gone"}',0,'["01a126ea-29a0-70f1-abea-ed771001d24e"]','2026-10-10T17:44:02.746821728Z',NULL);
INSERT INTO "resources"("scope","project_id","name","declaration","capacity","observed_capacity","revision","observed_at","error") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','01a126ea-2815-707e-a7ed-9034d4906573','cpu','{"capacity":2}',2,NULL,1,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(8,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.35413573Z','step.status',1,'{"kind":"step.status","step":"a","from":"pending","to":"running","error":null,"run_ids":["01a126ea-282f-7236-a0a1-76ac224c360d"],"needs":{}}','a',NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(9,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.354515504Z','step.status',1,'{"kind":"step.status","step":"a","from":"running","to":"running","error":null,"run_ids":["01a126ea-282f-7236-a0a1-76ac224c360d"],"needs":{}}','a',NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(10,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.362026882Z','step.status',1,'{"kind":"step.status","step":"a","from":"running","to":"succeeded","error":null,"run_ids":["01a126ea-282f-7236-a0a1-76ac224c360d"],"needs":{}}','a',NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(11,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.370257785Z','step.status',1,'{"kind":"step.status","step":"b","from":"pending","to":"running","error":null,"run_ids":["01a126ea-283f-751e-9e47-f6be278d1c10"],"needs":{}}','b',NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(12,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.370644433Z','step.status',1,'{"kind":"step.status","step":"b","from":"running","to":"running","error":null,"run_ids":["01a126ea-283f-751e-9e47-f6be278d1c10"],"needs":{}}','b',NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(13,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.378097441Z','step.status',1,'{"kind":"step.status","step":"b","from":"running","to":"succeeded","error":null,"run_ids":["01a126ea-283f-751e-9e47-f6be278d1c10"],"needs":{}}','b',NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(14,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.383959485Z','plan.edit',1,'{"kind":"plan.edit","rev":5,"author":"generator","reason":"generated","ops":[{"op":"add","path":"/outputs","value":{"result":{"source":"b/value"}}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(15,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.390379018Z','plan.edit',1,'{"kind":"plan.edit","rev":6,"author":"generator","reason":"generated","ops":[{"op":"replace","path":"/steps","value":{"c":{"run":"fixture.echo","paused":"after review","priority":5,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}},"a":{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}}},"d":{"run":"fixture.echo","in":{"value":{"default":"d"}}},"b":{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}}}}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(16,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.396333115Z','plan.edit',1,'{"kind":"plan.edit","rev":7,"author":"generator","reason":"remove d","ops":[{"op":"remove","path":"/steps/d"}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(17,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.401937183Z','plan.edit',1,'{"kind":"plan.edit","rev":8,"author":"generator","reason":"add e","ops":[{"op":"add","path":"/steps/e","value":{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[1,2,3]}},"paused":true}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(18,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.407559636Z','plan.edit',1,'{"kind":"plan.edit","rev":9,"author":"generator","reason":"generated","ops":[{"op":"move","from":"/steps/e","path":"/steps/f"}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(19,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.411861954Z','plan.edit',1,'{"kind":"plan.edit","rev":10,"author":"generator","reason":"generated","ops":[{"op":"remove","path":"/outputs"}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(20,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.415434388Z','plan.edit',1,'{"kind":"plan.edit","rev":11,"author":"generator","reason":"generated","ops":[{"op":"add","path":"/outputs","value":{}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(21,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.418867139Z','plan.edit',1,'{"kind":"plan.edit","rev":12,"author":"generator","reason":"generated","ops":[{"op":"replace","path":"/inputs/repo","value":{"type":"string","doc":"The repo"}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(22,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.422683904Z','plan.edit',1,'{"kind":"plan.edit","rev":13,"author":"generator","reason":"add g","ops":[{"op":"add","path":"/steps/g","value":{"run":"fixture.echo","after":["unit:build?"],"in":{"value":{"source":"c/value"}}}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(23,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.427386735Z','plan.edit',1,'{"kind":"plan.edit","rev":14,"author":"generator","reason":"generated","ops":[{"op":"replace","path":"","value":{"inputs":{"repo":{"type":"string","doc":"The repo"},"limit":{"type":"int","doc":"How many"}},"outputs":{},"steps":{"c":{"run":"fixture.echo","paused":"after review","priority":5,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}},"a":{"run":"fixture.echo","tags":["unit:build"],"in":{"value":{"default":1}}},"b":{"run":"fixture.echo","tags":["unit:build","exit"],"after":["a"],"in":{"value":{"source":"a/value"}}},"f":{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[1,2,3]}},"paused":true},"g":{"run":"fixture.echo","after":["unit:build?"],"in":{"value":{"source":"c/value"}},"doc":"Gate on build"}}}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(24,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.431552216Z','plan.edit',1,'{"kind":"plan.edit","rev":15,"author":"generator","reason":"generated","ops":[{"op":"test","path":"/steps/g/run","value":"fixture.echo"},{"op":"add","path":"/steps/g/tags","value":["late"]}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(25,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.43536917Z','plan.edit',1,'{"kind":"plan.edit","rev":16,"author":"generator","reason":"generated","ops":[{"op":"copy","from":"/steps/g","path":"/steps/h"}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(26,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.441414499Z','plan.edit',1,'{"kind":"plan.edit","rev":17,"author":"generator","reason":"unit","ops":[{"op":"add","path":"/steps/fig-1-fork","value":{"run":"fixture.echo","in":{"value":{"default":"fork"}},"tags":["unit:fig-1"]}},{"op":"add","path":"/steps/fig-1-work","value":{"run":"fixture.echo","after":["fig-1-fork"],"in":{"value":{"source":"fig-1-fork/value"}},"tags":["unit:fig-1"]}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(27,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.455698625Z','plan.edit',1,'{"kind":"plan.edit","rev":18,"author":"generator","reason":"hold b","ops":[{"op":"add","path":"/steps/b/paused","value":"hold b"}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(28,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.538590267Z','plan.edit',1,'{"kind":"plan.edit","rev":19,"author":"generator","reason":"tag","ops":[{"op":"replace","path":"/steps/fig-1-fork/tags","value":["unit:fig-1","wave-3"]},{"op":"replace","path":"/steps/fig-1-work/tags","value":["unit:fig-1","wave-3"]}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(29,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.610588084Z','plan.edit',1,'{"kind":"plan.edit","rev":20,"author":"generator","reason":"gate h","ops":[{"op":"replace","path":"/steps/h/after","value":["unit:build?","a"]}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(30,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.651693504Z','plan.edit',1,'{"kind":"plan.edit","rev":21,"author":"generator","reason":"prioritize c","ops":[{"op":"replace","path":"/steps/c","value":{"run":"fixture.echo","paused":"after review","priority":7,"needs":{"cpu":1},"in":{"value":{"source":"repo"}}}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(31,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.690002681Z','unit.settled',1,'{"kind":"unit.settled","unit":"build","work":1,"steps":[{"id":"a","status":"succeeded","held":false,"outputs":{"value":1},"omitted":[]},{"id":"b","status":"succeeded","held":false,"outputs":{"value":1},"omitted":[]}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(32,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.690235128Z','unit.settled',1,'{"kind":"unit.settled","unit":"f","work":1,"steps":[{"id":"f","status":"pending","held":true,"outputs":{},"omitted":[]}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(33,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.690409016Z','unit.settled',1,'{"kind":"unit.settled","unit":"g","work":1,"steps":[{"id":"g","status":"pending","held":true,"outputs":{},"omitted":[]}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(34,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.690583284Z','unit.settled',1,'{"kind":"unit.settled","unit":"h","work":1,"steps":[{"id":"h","status":"pending","held":true,"outputs":{},"omitted":[]}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(35,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.722687822Z','step.status',1,'{"kind":"step.status","step":"fig-1-fork","from":"pending","to":"running","error":null,"run_ids":["01a126ea-29a0-70f1-abea-ed771001d24e"],"needs":{}}','fig-1-fork',NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(36,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.722990832Z','step.status',1,'{"kind":"step.status","step":"fig-1-fork","from":"running","to":"running","error":null,"run_ids":["01a126ea-29a0-70f1-abea-ed771001d24e"],"needs":{}}','fig-1-fork',NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(37,'01a126ea-2815-707e-a7ed-9034d4906573','2026-10-10T17:44:02.747192826Z','step.status',1,'{"kind":"step.status","step":"fig-1-fork","from":"running","to":"failed","error":{"error":"process_lost","message":"guardian gone"},"run_ids":["01a126ea-29a0-70f1-abea-ed771001d24e"],"needs":{}}','fig-1-fork',NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(38,'01a126ea-29c6-72e9-a93b-2c6e39d0be03','2026-10-10T17:44:02.759231215Z','plan.edit',1,'{"kind":"plan.edit","rev":1,"author":"generator","reason":"project created","ops":[]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(39,'01a126ea-29c6-72e9-a93b-2c6e39d0be03','2026-10-10T17:44:02.831386509Z','project.board',1,'{"kind":"project.board","rev":1,"cleared":false,"reason":null,"author":"generator"}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(40,'01a126ea-2a2e-7112-bf61-0d0a49c77c17','2026-10-10T17:44:02.862958675Z','plan.edit',1,'{"kind":"plan.edit","rev":1,"author":"generator","reason":"project created","ops":[]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(41,'01a126ea-2a2e-7112-bf61-0d0a49c77c17','2026-10-10T17:44:02.869069006Z','plan.edit',1,'{"kind":"plan.edit","rev":2,"author":"generator","reason":"generated","ops":[{"op":"add","path":"/steps/x","value":{"run":"fixture.submit","in":{"value":{"default":true}},"outputs":{"extra":"string"}}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(42,'01a126ea-2a2e-7112-bf61-0d0a49c77c17','2026-10-10T17:44:02.871676674Z','plan.edit',1,'{"kind":"plan.edit","rev":3,"author":"someone-else","reason":"generated","ops":[{"op":"add","path":"/inputs","value":{"k":"boolean"}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(43,'01a126ea-2a2e-7112-bf61-0d0a49c77c17','2026-10-10T17:44:02.874101699Z','plan.edit',1,'{"kind":"plan.edit","rev":4,"author":"generator","reason":"generated","ops":[{"op":"replace","path":"/steps/x/in/value","value":{"source":"k"}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(44,'01a126ea-2a2e-7112-bf61-0d0a49c77c17','2026-10-10T17:44:02.876677016Z','plan.edit',1,'{"kind":"plan.edit","rev":5,"author":"generator","reason":"generated","ops":[{"op":"add","path":"/outputs","value":{"out":{"source":"x/value"}}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(45,'01a126ea-2a2e-7112-bf61-0d0a49c77c17','2026-10-10T17:44:02.878117347Z','project.archive',1,'{"kind":"project.archive","archived":true,"reason":null,"author":"generator"}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(49,NULL,'2026-10-10T17:44:02.887773713Z','project.delete',1,'{"kind":"project.delete","project_id":"01a126ea-2a3f-708e-9298-7a87f0bba7a2","name":"delta","author":"generator"}',NULL,NULL,NULL,NULL);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('home',NULL,'scheduler',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('home',NULL,'log',45);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('home',NULL,'projects',8);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','01a126ea-2815-707e-a7ed-9034d4906573','artifacts',3);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','01a126ea-2815-707e-a7ed-9034d4906573','log',32);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','01a126ea-2815-707e-a7ed-9034d4906573','plan',23);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','01a126ea-2815-707e-a7ed-9034d4906573','resources',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','01a126ea-2815-707e-a7ed-9034d4906573','settings',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','01a126ea-2815-707e-a7ed-9034d4906573','status',9);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','01a126ea-2815-707e-a7ed-9034d4906573','edits',20);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','01a126ea-2815-707e-a7ed-9034d4906573','messages',3);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2815-707e-a7ed-9034d4906573','01a126ea-2815-707e-a7ed-9034d4906573','questions',3);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-29c6-72e9-a93b-2c6e39d0be03','01a126ea-29c6-72e9-a93b-2c6e39d0be03','artifacts',3);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-29c6-72e9-a93b-2c6e39d0be03','01a126ea-29c6-72e9-a93b-2c6e39d0be03','log',2);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-29c6-72e9-a93b-2c6e39d0be03','01a126ea-29c6-72e9-a93b-2c6e39d0be03','plan',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-29c6-72e9-a93b-2c6e39d0be03','01a126ea-29c6-72e9-a93b-2c6e39d0be03','settings',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-29c6-72e9-a93b-2c6e39d0be03','01a126ea-29c6-72e9-a93b-2c6e39d0be03','status',2);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-29c6-72e9-a93b-2c6e39d0be03','01a126ea-29c6-72e9-a93b-2c6e39d0be03','board',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17','01a126ea-2a2e-7112-bf61-0d0a49c77c17','artifacts',3);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17','01a126ea-2a2e-7112-bf61-0d0a49c77c17','log',6);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17','01a126ea-2a2e-7112-bf61-0d0a49c77c17','plan',5);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17','01a126ea-2a2e-7112-bf61-0d0a49c77c17','settings',2);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17','01a126ea-2a2e-7112-bf61-0d0a49c77c17','status',2);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a2e-7112-bf61-0d0a49c77c17','01a126ea-2a2e-7112-bf61-0d0a49c77c17','edits',4);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a3f-708e-9298-7a87f0bba7a2','01a126ea-2a3f-708e-9298-7a87f0bba7a2','artifacts',6);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a3f-708e-9298-7a87f0bba7a2','01a126ea-2a3f-708e-9298-7a87f0bba7a2','log',4);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a3f-708e-9298-7a87f0bba7a2','01a126ea-2a3f-708e-9298-7a87f0bba7a2','plan',3);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a3f-708e-9298-7a87f0bba7a2','01a126ea-2a3f-708e-9298-7a87f0bba7a2','settings',3);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a3f-708e-9298-7a87f0bba7a2','01a126ea-2a3f-708e-9298-7a87f0bba7a2','status',3);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a3f-708e-9298-7a87f0bba7a2','01a126ea-2a3f-708e-9298-7a87f0bba7a2','edits',2);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('home',NULL,'maintenance',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a3f-708e-9298-7a87f0bba7a2','01a126ea-2a3f-708e-9298-7a87f0bba7a2','messages',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a3f-708e-9298-7a87f0bba7a2','01a126ea-2a3f-708e-9298-7a87f0bba7a2','outcomes',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a3f-708e-9298-7a87f0bba7a2','01a126ea-2a3f-708e-9298-7a87f0bba7a2','questions',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a3f-708e-9298-7a87f0bba7a2','01a126ea-2a3f-708e-9298-7a87f0bba7a2','readers',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2a3f-708e-9298-7a87f0bba7a2','01a126ea-2a3f-708e-9298-7a87f0bba7a2','resources',1);
INSERT INTO "maintenance"("singleton","mode","owner","paused_projects","scheduler_owner","scheduler_lease_until","revision","settings","changed_at") VALUES(1,'normal',NULL,'[]','generator',NULL,0,'{"settlements":{"01a126ea-2815-707e-a7ed-9034d4906573/c":{"signature":[["c",4,1]],"work":1},"01a126ea-2815-707e-a7ed-9034d4906573/build":{"signature":[["a",2,1],["b",4,1]],"work":1},"01a126ea-2815-707e-a7ed-9034d4906573/f":{"signature":[["f",9,1]],"work":1},"01a126ea-2815-707e-a7ed-9034d4906573/g":{"signature":[["g",13,1]],"work":1},"01a126ea-2815-707e-a7ed-9034d4906573/h":{"signature":[["h",16,1]],"work":1}}}',NULL);
INSERT INTO "artifact_jobs"("job_id","project_id","run_id","kind","generation","path","manifest","state","error","created_at","finished_at") VALUES('01a126ea-2815-707e-a7ed-903505451f8f','01a126ea-2815-707e-a7ed-9034d4906573',NULL,'project_dir',1,'projects/01a126ea-2815-707e-a7ed-9034d4906573','{"version":1}','done',NULL,'2026-10-10T17:44:02.325337625Z','2026-10-10T17:44:02.328518412Z');
INSERT INTO "artifact_jobs"("job_id","project_id","run_id","kind","generation","path","manifest","state","error","created_at","finished_at") VALUES('01a126ea-29c7-71e3-8bc2-59115c7af011','01a126ea-29c6-72e9-a93b-2c6e39d0be03',NULL,'project_dir',1,'projects/01a126ea-29c6-72e9-a93b-2c6e39d0be03','{"version":1}','done',NULL,'2026-10-10T17:44:02.759098395Z','2026-10-10T17:44:02.819340375Z');
INSERT INTO "artifact_jobs"("job_id","project_id","run_id","kind","generation","path","manifest","state","error","created_at","finished_at") VALUES('01a126ea-2a2e-7112-bf61-0d0bcde46da2','01a126ea-2a2e-7112-bf61-0d0a49c77c17',NULL,'project_dir',1,'projects/01a126ea-2a2e-7112-bf61-0d0a49c77c17','{"version":1}','done',NULL,'2026-10-10T17:44:02.862829572Z','2026-10-10T17:44:02.865836632Z');
INSERT INTO "artifact_jobs"("job_id","project_id","run_id","kind","generation","path","manifest","state","error","created_at","finished_at") VALUES('01a126ea-2a3f-708e-9298-7a885597e46a','01a126ea-2a3f-708e-9298-7a87f0bba7a2',NULL,'project_dir',1,'projects/01a126ea-2a3f-708e-9298-7a87f0bba7a2','{}','failed','{"deleted":true}','2026-10-10T17:44:02.87927698Z','2026-10-10T17:44:02.886829706Z');
INSERT INTO "artifact_jobs"("job_id","project_id","run_id","kind","generation","path","manifest","state","error","created_at","finished_at") VALUES('01a126ea-2a46-71c8-82bf-f270d112b6ef','01a126ea-2a3f-708e-9298-7a87f0bba7a2',NULL,'cleanup',1,'projects/01a126ea-2a3f-708e-9298-7a87f0bba7a2','{"version":1}','done',NULL,'2026-10-10T17:44:02.886951385Z','2026-10-10T17:44:02.88996645Z');
DELETE FROM sqlite_sequence;
INSERT INTO sqlite_sequence(name,seq) VALUES('records',49);
CREATE UNIQUE INDEX projects_live_name ON projects(name) WHERE deleted_at IS NULL;
CREATE INDEX steps_status ON steps(project_id, status);
CREATE UNIQUE INDEX attempts_active_item ON attempts(project_id, step_id, generation, item_index)
  WHERE phase <> 'terminal' AND step_id IS NOT NULL;
CREATE INDEX attempts_phase ON attempts(phase);
CREATE INDEX runs_live ON runs(project_id, finished_at);
CREATE INDEX runs_predecessor ON runs(prev_run);
CREATE INDEX calls_status ON calls(project_id, status, direct);
CREATE INDEX calls_retention ON calls(finished_at);
CREATE INDEX results_step ON step_results(project_id, step_id, generation, recorded_at);
CREATE INDEX results_removed ON step_results(project_id, removed_at);
CREATE TRIGGER step_results_immutable BEFORE UPDATE ON step_results
WHEN OLD.result_id IS NOT NEW.result_id OR OLD.project_id IS NOT NEW.project_id
  OR OLD.step_id IS NOT NEW.step_id OR OLD.generation IS NOT NEW.generation
  OR OLD.work_generation IS NOT NEW.work_generation OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.unit IS NOT NEW.unit OR OLD.declaration IS NOT NEW.declaration
  OR OLD.inputs IS NOT NEW.inputs OR OLD.inputs_hash IS NOT NEW.inputs_hash
  OR OLD.status IS NOT NEW.status OR OLD.outputs IS NOT NEW.outputs
  OR OLD.error IS NOT NEW.error OR OLD.manual IS NOT NEW.manual
  OR OLD.run_ids IS NOT NEW.run_ids OR OLD.recorded_at IS NOT NEW.recorded_at
BEGIN SELECT RAISE(ABORT, 'result snapshot is immutable'); END;
CREATE INDEX leases_queue ON leases(scope, resource, priority DESC, lease_id) WHERE state = 'waiting';
CREATE INDEX leases_holds ON leases(run_id, state);
CREATE INDEX messages_thread ON messages(project_id, thread, id);
CREATE INDEX messages_address ON messages(project_id, "to", id);
CREATE INDEX messages_questions ON messages(project_id, id) WHERE needs_reply = 1 AND resolved_by IS NULL AND closed_at IS NULL;
CREATE INDEX messages_replies ON messages(reply_to, id);
CREATE UNIQUE INDEX question_live_asker ON question_attachments(project_id, message_id) WHERE detached_at IS NULL;
CREATE INDEX question_lineage ON question_attachments(project_id, step_id, generation, item_index, title);
CREATE INDEX records_project ON records(project_id, seq);
CREATE INDEX records_kind ON records(project_id, kind, seq);
CREATE INDEX records_run ON records(run_id, seq);
CREATE INDEX records_call ON records(call_id, seq);
CREATE INDEX records_thread ON records(project_id, thread, seq);
CREATE INDEX artifact_jobs_pending ON artifact_jobs(state, created_at);
CREATE INDEX sessions_identity ON sessions(engine, cwd, session_id);
CREATE VIEW outcomes AS SELECT * FROM step_results WHERE removed_at IS NOT NULL;
CREATE VIEW log AS SELECT seq, project_id, at, kind, payload_version,
  json_set(payload, '$.seq', seq, '$.at', at, '$.project', project_id) AS record FROM records;
CREATE VIEW step_changes AS SELECT seq, project_id, at, step_id,
  json_extract(payload, '$.from') AS "from", json_extract(payload, '$.to') AS "to",
  json_extract(payload, '$.error') AS error FROM records WHERE kind = 'step.status';
CREATE VIEW edits AS SELECT * FROM plan_edits;
CREATE VIEW board_slots AS SELECT p.project_id AS project_id, s.key AS key,
  json_extract(s.value, '$.markdown') AS markdown, json_extract(s.value, '$.at') AS updated_at,
  json_extract(s.value, '$.author') AS author
  FROM projects p, json_each(p.board_slots) s WHERE p.deleted_at IS NULL;
CREATE VIEW questions AS SELECT m.*,
  CASE WHEN closed_at IS NOT NULL THEN 'closed' WHEN resolved_by IS NOT NULL THEN 'answered' ELSE 'open' END AS state,
  EXISTS (SELECT 1 FROM question_attachments q JOIN runs r ON r.run_id = q.run_id
    WHERE q.project_id = m.project_id AND q.message_id = m.id AND q.detached_at IS NULL AND r.finished_at IS NULL) AS waiting
  FROM messages m WHERE needs_reply = 1;
COMMIT;
PRAGMA application_id=1397511491;
PRAGMA user_version=1;
