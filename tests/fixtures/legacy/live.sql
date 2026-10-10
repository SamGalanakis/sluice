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
INSERT INTO "home_meta"("singleton","home_id","format_major","schema_version","record_floor","maintenance_settings") VALUES(1,'01a126ea-2b08-707a-84dd-c90b209c3e6c',1,1,0,'{}');
INSERT INTO "projects"("project_id","name","description","icon_generation","icon_text","icon_type","icon_hash","paused","archived","resources_rev","settings_rev","created_at","changed_at","deleted_at","board","board_rev","board_slots","board_doc","board_doc_rev","board_doc_at","board_doc_author","prune_done_after","prune_keep") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','live','',0,NULL,NULL,NULL,0,0,1,1,'2026-10-10T17:44:03.091265392Z',NULL,NULL,NULL,0,NULL,NULL,0,NULL,NULL,NULL,NULL);
INSERT INTO "plans"("project_id","rev","doc") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc',2,'{"steps":{"tests-main":{"run":"fixture.echo","tags":["rolling"],"in":{"value":{"default":1}}},"build":{"run":"fixture.echo","needs":{"cpu":1},"in":{"value":{"default":2}}}}}');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc',1,1,'2026-10-10T17:44:03.091512387Z','generator','project created','[]');
INSERT INTO "plan_edits"("project_id","rev","seq","at","author","reason","ops") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc',2,2,'2026-10-10T17:44:03.097095306Z','generator','generated','[{"op":"add","path":"/steps/tests-main","value":{"run":"fixture.echo","tags":["rolling"],"in":{"value":{"default":1}}}},{"op":"add","path":"/steps/build","value":{"run":"fixture.echo","needs":{"cpu":1},"in":{"value":{"default":2}}}}]');
INSERT INTO "steps"("project_id","step_id","position","generation","work_generation","declaration","status","unit","paused","outputs","error","skipped","manual","inputs_hash","result_id","run_ids","instances","total","done","delivery_cursor","progress","progress_at","progress_run") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','tests-main',0,2,1,'{"run":"fixture.echo","tags":["rolling"],"in":{"value":{"default":1}}}','running','tests-main','false',NULL,NULL,NULL,0,'4b86ff53b63e713aba37d590edcb3c2ab6d3d429230dd06b57fbfbe31a567046',NULL,'["01a126ea-2b1c-75c7-af6b-139b631d57d4"]','{}',NULL,0,0,NULL,NULL,NULL);
INSERT INTO "steps"("project_id","step_id","position","generation","work_generation","declaration","status","unit","paused","outputs","error","skipped","manual","inputs_hash","result_id","run_ids","instances","total","done","delivery_cursor","progress","progress_at","progress_run") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','build',1,2,1,'{"run":"fixture.echo","needs":{"cpu":1},"in":{"value":{"default":2}}}','running','build','false',NULL,NULL,NULL,0,'0a6d9f6ffc673d7803120aaf66879fa81b54f180d6855673c21b3e805204b56c',NULL,'["01a126ea-2b20-7293-ba87-e18ac54073bf"]','{}',NULL,0,0,NULL,NULL,NULL);
INSERT INTO "attempts"("attempt_id","project_id","step_id","generation","work_generation","item_index","phase","request","inputs_hash","provenance","unit","spawn_attempted","cancel_requested","created_at","finished_at") VALUES('01a126ea-2b1c-75c7-af6b-139a799e20bd','01a126ea-2b13-709d-a113-21c2bb47cdbc','tests-main',2,1,-1,'reserved','{"declaration":{"run":"fixture.echo","tags":["rolling"],"in":{"value":{"default":1}}},"inputs":{"value":1},"effective_inputs":{"value":1},"returns":{"value":"Any"},"declared":{},"item_count":null,"provenance":{"runtime":{"capability":"01a126ea-2b1c-75c7-af6b-139891bee1d101a126ea-2b1c-75c7-af6b-1399622920a1","execution":null,"completion":{"revision":2,"document":{"steps":{"tests-main":{"run":"fixture.echo","tags":["rolling"],"in":{"value":{"default":1}}},"build":{"run":"fixture.echo","needs":{"cpu":1},"in":{"value":{"default":2}}}}},"signatures":{"fixture.echo":{"inputs":{"value":"Any"},"outputs":{"value":"Any"},"submits":{},"open":false}}}},"files":{}},"listens":false}','4b86ff53b63e713aba37d590edcb3c2ab6d3d429230dd06b57fbfbe31a567046','{"runtime":{"capability":"01a126ea-2b1c-75c7-af6b-139891bee1d101a126ea-2b1c-75c7-af6b-1399622920a1","execution":null,"completion":{"revision":2,"document":{"steps":{"tests-main":{"run":"fixture.echo","tags":["rolling"],"in":{"value":{"default":1}}},"build":{"run":"fixture.echo","needs":{"cpu":1},"in":{"value":{"default":2}}}}},"signatures":{"fixture.echo":{"inputs":{"value":"Any"},"outputs":{"value":"Any"},"submits":{},"open":false}}}},"files":{}}','tests-main',1,0,'2026-10-10T17:44:03.101466373Z',NULL);
INSERT INTO "attempts"("attempt_id","project_id","step_id","generation","work_generation","item_index","phase","request","inputs_hash","provenance","unit","spawn_attempted","cancel_requested","created_at","finished_at") VALUES('01a126ea-2b20-7293-ba87-e189eb133f0c','01a126ea-2b13-709d-a113-21c2bb47cdbc','build',2,1,-1,'reserved','{"declaration":{"run":"fixture.echo","needs":{"cpu":1},"in":{"value":{"default":2}}},"inputs":{"value":2},"effective_inputs":{"value":2},"returns":{"value":"Any"},"declared":{},"item_count":null,"provenance":{"runtime":{"capability":"01a126ea-2b20-7293-ba87-e187f03e742701a126ea-2b20-7293-ba87-e18886dae13a","execution":null,"completion":{"revision":2,"document":{"steps":{"tests-main":{"run":"fixture.echo","tags":["rolling"],"in":{"value":{"default":1}}},"build":{"run":"fixture.echo","needs":{"cpu":1},"in":{"value":{"default":2}}}}},"signatures":{"fixture.echo":{"inputs":{"value":"Any"},"outputs":{"value":"Any"},"submits":{},"open":false}}}},"files":{}},"listens":false}','0a6d9f6ffc673d7803120aaf66879fa81b54f180d6855673c21b3e805204b56c','{"runtime":{"capability":"01a126ea-2b20-7293-ba87-e187f03e742701a126ea-2b20-7293-ba87-e18886dae13a","execution":null,"completion":{"revision":2,"document":{"steps":{"tests-main":{"run":"fixture.echo","tags":["rolling"],"in":{"value":{"default":1}}},"build":{"run":"fixture.echo","needs":{"cpu":1},"in":{"value":{"default":2}}}}},"signatures":{"fixture.echo":{"inputs":{"value":"Any"},"outputs":{"value":"Any"},"submits":{},"open":false}}}},"files":{}}','build',1,0,'2026-10-10T17:44:03.10505602Z',NULL);
INSERT INTO "attempts"("attempt_id","project_id","step_id","generation","work_generation","item_index","phase","request","inputs_hash","provenance","unit","spawn_attempted","cancel_requested","created_at","finished_at") VALUES('01a126ea-2b26-75d3-be26-0b2b88cde349','01a126ea-2b13-709d-a113-21c2bb47cdbc',NULL,1,1,-1,'reserved','{"function":{"name":"fixture.wait","inputs":{"value":"Any"},"outputs":{"value":"Any"},"bundle":{"capability":"01a126ea-2b25-7514-8777-0a2cdaec310f01a126ea-2b25-7514-8777-0a2d43f227fa"},"needs":{},"release_id":"runtime-v1","timeout_seconds":null},"inputs":{"value":9}}','d35dd7c0935d1d5166ee866c22d518d996d89e7621a20878244df842986e6057','{}',NULL,1,0,'2026-10-10T17:44:03.110Z',NULL);
INSERT INTO "runs"("run_id","project_id","attempt_id","step_id","generation","work_generation","item_index","prev_run","unit","unit_name","boot_id","guardian_pid","guardian_start","cgroup","socket_challenge","release_id","protocol_major","assigned_after","assigned_through","started_at","created_at","finished_at","completion_id","completion_ack","result","completion_action","action_outcome","stopped") VALUES('01a126ea-2b1c-75c7-af6b-139b631d57d4','01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b1c-75c7-af6b-139a799e20bd','tests-main',2,1,-1,NULL,'tests-main',NULL,NULL,NULL,NULL,NULL,NULL,'runtime-v1',1,0,0,NULL,'2026-10-10T17:44:03.101466373Z',NULL,NULL,0,NULL,NULL,NULL,NULL);
INSERT INTO "runs"("run_id","project_id","attempt_id","step_id","generation","work_generation","item_index","prev_run","unit","unit_name","boot_id","guardian_pid","guardian_start","cgroup","socket_challenge","release_id","protocol_major","assigned_after","assigned_through","started_at","created_at","finished_at","completion_id","completion_ack","result","completion_action","action_outcome","stopped") VALUES('01a126ea-2b20-7293-ba87-e18ac54073bf','01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b20-7293-ba87-e189eb133f0c','build',2,1,-1,NULL,'build',NULL,NULL,NULL,NULL,NULL,NULL,'runtime-v1',1,0,0,NULL,'2026-10-10T17:44:03.10505602Z',NULL,NULL,0,NULL,NULL,NULL,NULL);
INSERT INTO "runs"("run_id","project_id","attempt_id","step_id","generation","work_generation","item_index","prev_run","unit","unit_name","boot_id","guardian_pid","guardian_start","cgroup","socket_challenge","release_id","protocol_major","assigned_after","assigned_through","started_at","created_at","finished_at","completion_id","completion_ack","result","completion_action","action_outcome","stopped") VALUES('01a126ea-2b25-7514-8777-0a2ed2bc35f2','01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b26-75d3-be26-0b2b88cde349',NULL,1,1,-1,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'runtime-v1',1,0,0,NULL,'2026-10-10T17:44:03.110Z',NULL,NULL,0,NULL,NULL,NULL,NULL);
INSERT INTO "calls"("call_id","project_id","run_id","fn","status","inputs","outputs","error","direct","author","created_at","finished_at") VALUES('01a126ea-2b25-7514-8777-0a2ed2bc35f2','01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b25-7514-8777-0a2ed2bc35f2','fixture.wait','running','{"value":9}',NULL,NULL,1,'generator','2026-10-10T17:44:03.109Z',NULL);
INSERT INTO "resources"("scope","project_id","name","declaration","capacity","observed_capacity","revision","observed_at","error") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b13-709d-a113-21c2bb47cdbc','cpu','{"capacity":1}',1,NULL,1,NULL,NULL);
INSERT INTO "leases"("lease_id","project_id","run_id","request_id","kind","group_id","scope","resource","amount","priority","state","grant_id","release_id","created_at","granted_at","released_at") VALUES(1,'01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b20-7293-ba87-e18ac54073bf','needs/01a126ea-2b20-7293-ba87-e18ac54073bf/cpu','needs','01a126ea-2b20-7293-ba87-e18ac54073bf','01a126ea-2b13-709d-a113-21c2bb47cdbc','cpu',1,0,'held','needs/01a126ea-2b20-7293-ba87-e18ac54073bf/cpu',NULL,'2026-10-10T17:44:03.105825568Z','2026-10-10T17:44:03.105825568Z',NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(1,'01a126ea-2b13-709d-a113-21c2bb47cdbc','2026-10-10T17:44:03.091512387Z','plan.edit',1,'{"kind":"plan.edit","rev":1,"author":"generator","reason":"project created","ops":[]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(2,'01a126ea-2b13-709d-a113-21c2bb47cdbc','2026-10-10T17:44:03.097095306Z','plan.edit',1,'{"kind":"plan.edit","rev":2,"author":"generator","reason":"generated","ops":[{"op":"add","path":"/steps/tests-main","value":{"run":"fixture.echo","tags":["rolling"],"in":{"value":{"default":1}}}},{"op":"add","path":"/steps/build","value":{"run":"fixture.echo","needs":{"cpu":1},"in":{"value":{"default":2}}}}]}',NULL,NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(3,'01a126ea-2b13-709d-a113-21c2bb47cdbc','2026-10-10T17:44:03.102515748Z','step.status',1,'{"kind":"step.status","step":"tests-main","from":"pending","to":"running","error":null,"run_ids":["01a126ea-2b1c-75c7-af6b-139b631d57d4"],"needs":{}}','tests-main',NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(4,'01a126ea-2b13-709d-a113-21c2bb47cdbc','2026-10-10T17:44:03.102827304Z','step.status',1,'{"kind":"step.status","step":"tests-main","from":"running","to":"running","error":null,"run_ids":["01a126ea-2b1c-75c7-af6b-139b631d57d4"],"needs":{}}','tests-main',NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(5,'01a126ea-2b13-709d-a113-21c2bb47cdbc','2026-10-10T17:44:03.10604958Z','step.status',1,'{"kind":"step.status","step":"build","from":"pending","to":"running","error":null,"run_ids":["01a126ea-2b20-7293-ba87-e18ac54073bf"],"needs":{"cpu":1}}','build',NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(6,'01a126ea-2b13-709d-a113-21c2bb47cdbc','2026-10-10T17:44:03.10629939Z','step.status',1,'{"kind":"step.status","step":"build","from":"running","to":"running","error":null,"run_ids":["01a126ea-2b20-7293-ba87-e18ac54073bf"],"needs":{}}','build',NULL,NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(7,'01a126ea-2b13-709d-a113-21c2bb47cdbc','2026-10-10T17:44:03.109972774Z','call',1,'{"kind":"call","call":"01a126ea-2b25-7514-8777-0a2ed2bc35f2","fn":"fixture.wait","status":"pending","inputs":{"value":9},"outputs":null,"error":null,"direct":true,"author":"generator"}',NULL,'01a126ea-2b25-7514-8777-0a2ed2bc35f2',NULL,NULL);
INSERT INTO "records"("seq","project_id","at","kind","payload_version","payload","step_id","call_id","thread","run_id") VALUES(8,'01a126ea-2b13-709d-a113-21c2bb47cdbc','2026-10-10T17:44:03.110504635Z','call',1,'{"kind":"call","call":"01a126ea-2b25-7514-8777-0a2ed2bc35f2","fn":"fixture.wait","status":"running","inputs":null,"outputs":null,"error":null,"direct":true,"author":"generator"}',NULL,'01a126ea-2b25-7514-8777-0a2ed2bc35f2',NULL,NULL);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('home',NULL,'scheduler',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('home',NULL,'log',5);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('home',NULL,'projects',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b13-709d-a113-21c2bb47cdbc','artifacts',3);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b13-709d-a113-21c2bb47cdbc','log',5);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b13-709d-a113-21c2bb47cdbc','plan',3);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b13-709d-a113-21c2bb47cdbc','resources',3);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b13-709d-a113-21c2bb47cdbc','settings',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b13-709d-a113-21c2bb47cdbc','status',3);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b13-709d-a113-21c2bb47cdbc','edits',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b13-709d-a113-21c2bb47cdbc','messages',2);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b13-709d-a113-21c2bb47cdbc','questions',2);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('01a126ea-2b13-709d-a113-21c2bb47cdbc','01a126ea-2b13-709d-a113-21c2bb47cdbc','calls',1);
INSERT INTO "change_versions"("scope","project_id","view","version") VALUES('home',NULL,'calls',1);
INSERT INTO "maintenance"("singleton","mode","owner","paused_projects","scheduler_owner","scheduler_lease_until","revision","settings","changed_at") VALUES(1,'normal',NULL,'[]','generator',NULL,0,'{}',NULL);
INSERT INTO "artifact_jobs"("job_id","project_id","run_id","kind","generation","path","manifest","state","error","created_at","finished_at") VALUES('01a126ea-2b13-709d-a113-21c3cfc50a52','01a126ea-2b13-709d-a113-21c2bb47cdbc',NULL,'project_dir',1,'projects/01a126ea-2b13-709d-a113-21c2bb47cdbc','{"version":1}','done',NULL,'2026-10-10T17:44:03.091382633Z','2026-10-10T17:44:03.094027311Z');
DELETE FROM sqlite_sequence;
INSERT INTO sqlite_sequence(name,seq) VALUES('leases',1);
INSERT INTO sqlite_sequence(name,seq) VALUES('records',8);
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
