-- IDs are canonical lowercase UUIDv7 TEXT. NULL project_id denotes home scope,
-- never a project name. Project deletion is a tombstone; owners remove its rows.

-- One immutable home identity; format/schema versions are checked before writes.
CREATE TABLE home_meta (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  home_id TEXT NOT NULL CHECK (length(home_id) = 36),
  format_major INTEGER NOT NULL CHECK (format_major > 0),
  schema_version INTEGER NOT NULL CHECK (schema_version > 0),
  record_floor INTEGER NOT NULL DEFAULT 0 CHECK (record_floor >= 0),
  maintenance_settings TEXT NOT NULL DEFAULT '{}' CHECK (json_type(maintenance_settings) = 'object')
) STRICT;

-- Immutable identity; only live labels are unique. Settings use optimistic revision.
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
  -- Automatic retiring of done units (SPEC §6.11): the age in seconds, off when null, and
  -- the unit-name patterns it never removes, a JSON array of strings.
  prune_done_after INTEGER CHECK (prune_done_after IS NULL OR prune_done_after > 0),
  prune_keep TEXT CHECK (prune_keep IS NULL OR json_type(prune_keep) = 'array'),
  CHECK (icon_text IS NULL OR icon_hash IS NULL),
  CHECK ((icon_type IS NULL) = (icon_hash IS NULL))
) STRICT;
CREATE UNIQUE INDEX projects_live_name ON projects(name) WHERE deleted_at IS NULL;

-- One current validated ordered plan document per project, revision starts at one.
CREATE TABLE plans (
  project_id TEXT PRIMARY KEY NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  rev INTEGER NOT NULL CHECK (rev >= 1),
  doc TEXT NOT NULL CHECK (json_type(doc) = 'object')
) STRICT;

-- Authored edits survive feed trimming; seq deliberately has no records FK.
CREATE TABLE plan_edits (
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  rev INTEGER NOT NULL CHECK (rev >= 1), seq INTEGER NOT NULL CHECK (seq > 0),
  at TEXT NOT NULL, author TEXT NOT NULL, reason TEXT NOT NULL,
  ops TEXT NOT NULL CHECK (json_type(ops) = 'array'),
  PRIMARY KEY (project_id, rev)
) STRICT;

-- Current typed values distinguish an absent value from explicit JSON null.
CREATE TABLE inputs (
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  name TEXT NOT NULL, position INTEGER NOT NULL CHECK (position >= 0),
  declaration TEXT NOT NULL CHECK (json_valid(declaration)),
  value TEXT CHECK (value IS NULL OR json_valid(value)),
  generation INTEGER NOT NULL DEFAULT 1 CHECK (generation >= 1),
  PRIMARY KEY (project_id, name), UNIQUE (project_id, position)
) STRICT;

-- Current projection only; removed/reintroduced ids get a new generation.
-- Old results/run metadata do not reference this disposable projection.
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
CREATE INDEX steps_status ON steps(project_id, status);

-- Reservations and phase transitions persist before external launch; one active
-- attempt per project/step/generation/item. item_index=-1 is the scalar item.
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
CREATE UNIQUE INDEX attempts_active_item ON attempts(project_id, step_id, generation, item_index)
  WHERE phase <> 'terminal' AND step_id IS NOT NULL;
CREATE INDEX attempts_phase ON attempts(phase);

-- One run per attempt. Identity and frozen message range survive restarts;
-- prev_run is retained without a deletion-blocking FK for artifact cleanup.
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
  CHECK (step_id IS NULL OR project_id IS NOT NULL),
  FOREIGN KEY (project_id, attempt_id) REFERENCES attempts(project_id, attempt_id),
  UNIQUE (project_id, run_id)
) STRICT;
CREATE INDEX runs_live ON runs(project_id, finished_at);
CREATE INDEX runs_predecessor ON runs(prev_run);

-- Authoritative versioned submissions merge in terminalization, not from files.
CREATE TABLE submissions (
  run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
  project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  step_id TEXT, version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
  outputs TEXT NOT NULL CHECK (json_type(outputs) = 'object'), at TEXT NOT NULL,
  FOREIGN KEY (project_id, run_id) REFERENCES runs(project_id, run_id)
) STRICT;

-- Call truth survives record trimming; terminal cleanup is explicit and guarded.
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
CREATE INDEX calls_status ON calls(project_id, status, direct);
CREATE INDEX calls_retention ON calls(finished_at);

-- Result ids never change. Retain snapshots independently of current steps and
-- attempts, including manual, failed, skipped, stale and removed-step outcomes.
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
CREATE INDEX results_step ON step_results(project_id, step_id, generation, recorded_at);
CREATE INDEX results_removed ON step_results(project_id, removed_at);
-- Removal metadata may be stamped later; recorded result content never changes.
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

-- Home/project capacities are distinct; lowering capacity preserves live holds.
CREATE TABLE resources (
  scope TEXT NOT NULL, project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  name TEXT NOT NULL, declaration TEXT NOT NULL CHECK (json_valid(declaration)),
  capacity INTEGER CHECK (capacity IS NULL OR capacity >= 0),
  observed_capacity INTEGER CHECK (observed_capacity IS NULL OR observed_capacity >= 0),
  revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1), observed_at TEXT,
  error TEXT CHECK (error IS NULL OR json_type(error) = 'object'),
  CHECK (scope = coalesce(project_id, 'home')), PRIMARY KEY (scope, name)
) STRICT;

-- Equal-priority requests are FIFO by global LeaseId. Request identity and
-- grant/release identity are durable, independent of the bounded record feed.
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
CREATE INDEX leases_queue ON leases(scope, resource, priority DESC, lease_id) WHERE state = 'waiting';
CREATE INDEX leases_holds ON leases(run_id, state);

-- IDs equal their original record seq; no records FK because messages outlive
-- trimming. First answering reply is resolved atomically; claims are permanent.
CREATE TABLE messages (
  id INTEGER PRIMARY KEY CHECK (id > 0),
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  thread TEXT NOT NULL, "from" TEXT NOT NULL, "to" TEXT, title TEXT, body TEXT NOT NULL,
  needs_reply INTEGER NOT NULL DEFAULT 0 CHECK (needs_reply IN (0,1)),
  reply_to INTEGER, answer TEXT CHECK (answer IS NULL OR json_type(answer) = 'object'),
  ui TEXT, input TEXT, data TEXT CHECK (data IS NULL OR json_valid(data)),
  run_id TEXT, at TEXT NOT NULL, claimed_by TEXT,
  resolved_by INTEGER, closed_at TEXT,
  FOREIGN KEY (project_id, reply_to) REFERENCES messages(project_id, id),
  FOREIGN KEY (project_id, resolved_by) REFERENCES messages(project_id, id),
  UNIQUE (project_id, id)
) STRICT;
CREATE INDEX messages_thread ON messages(project_id, thread, id);
CREATE INDEX messages_address ON messages(project_id, "to", id);
CREATE INDEX messages_questions ON messages(project_id, id) WHERE needs_reply = 1 AND resolved_by IS NULL AND closed_at IS NULL;
CREATE INDEX messages_replies ON messages(reply_to, id);

-- Ask take-up matches step/title/generation/item lineage; only one live asker.
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
CREATE UNIQUE INDEX question_live_asker ON question_attachments(project_id, message_id) WHERE detached_at IS NULL;
CREATE INDEX question_lineage ON question_attachments(project_id, step_id, generation, item_index, title);

-- Reservation assigns messages; start acknowledgement advances the step cursor.
-- Scatter gets a distinct delivery row per run. A failed reservation consumes none.
CREATE TABLE message_deliveries (
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  run_id TEXT NOT NULL REFERENCES runs(run_id), message_id INTEGER NOT NULL,
  assigned_at TEXT NOT NULL, acknowledged_at TEXT,
  PRIMARY KEY (project_id, run_id, message_id),
  FOREIGN KEY (project_id, message_id) REFERENCES messages(project_id, id) ON DELETE CASCADE,
  FOREIGN KEY (project_id, run_id) REFERENCES runs(project_id, run_id)
) STRICT;

-- Thread/stream-specific owner reads and consumptive next cursors are separate.
CREATE TABLE readers (
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  identity TEXT NOT NULL, stream TEXT NOT NULL, thread TEXT NOT NULL DEFAULT '',
  cursor INTEGER NOT NULL DEFAULT 0 CHECK (cursor >= 0),
  unread_alert_min INTEGER CHECK (unread_alert_min IS NULL OR unread_alert_min >= 0),
  heartbeat_at TEXT, PRIMARY KEY (project_id, identity, stream, thread)
) STRICT;

-- AUTOINCREMENT prevents committed seq reuse after trim, including an empty feed.
-- Lift filter fields, but keep explicit nulls in the typed versioned event payload.
CREATE TABLE records (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  at TEXT NOT NULL, kind TEXT NOT NULL,
  payload_version INTEGER NOT NULL DEFAULT 1 CHECK (payload_version >= 1),
  payload TEXT NOT NULL CHECK (json_type(payload) = 'object'),
  step_id TEXT, call_id TEXT, thread TEXT, run_id TEXT,
  CHECK (coalesce(json_type(payload, '$.kind') = 'text' AND json_extract(payload, '$.kind') = kind, 0))
) STRICT;
CREATE INDEX records_project ON records(project_id, seq);
CREATE INDEX records_kind ON records(project_id, kind, seq);
CREATE INDEX records_run ON records(run_id, seq);
CREATE INDEX records_call ON records(call_id, seq);
CREATE INDEX records_thread ON records(project_id, thread, seq);

-- Commit increments once per touched scope/view; home scope uses project_id NULL.
CREATE TABLE change_versions (
  scope TEXT NOT NULL, project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  view TEXT NOT NULL, version INTEGER NOT NULL CHECK (version >= 1),
  CHECK (scope = coalesce(project_id, 'home')), PRIMARY KEY (scope, view)
) STRICT;

-- One durable home admission fence and scheduler lease; drain ownership uses IDs.
CREATE TABLE maintenance (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  mode TEXT NOT NULL DEFAULT 'normal' CHECK (mode IN ('normal','drain','cutover')),
  owner TEXT, paused_projects TEXT NOT NULL DEFAULT '[]' CHECK (json_type(paused_projects) = 'array'),
  scheduler_owner TEXT, scheduler_lease_until TEXT, revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
  settings TEXT NOT NULL DEFAULT '{}' CHECK (json_type(settings) = 'object'), changed_at TEXT
) STRICT;
INSERT INTO maintenance(singleton) VALUES (1);

-- Filesystem jobs are id/generation keyed, so name reuse cannot change their target.
CREATE TABLE artifact_jobs (
  job_id TEXT PRIMARY KEY NOT NULL,
  project_id TEXT REFERENCES projects(project_id), run_id TEXT,
  kind TEXT NOT NULL, generation INTEGER NOT NULL CHECK (generation >= 1),
  path TEXT NOT NULL, manifest TEXT NOT NULL DEFAULT '{}' CHECK (json_type(manifest) = 'object'),
  state TEXT NOT NULL CHECK (state IN ('pending','running','done','failed')),
  error TEXT CHECK (error IS NULL OR json_type(error) = 'object'),
  created_at TEXT NOT NULL, finished_at TEXT, UNIQUE (path, generation, kind)
) STRICT;
CREATE INDEX artifact_jobs_pending ON artifact_jobs(state, created_at);

-- Session metadata follows runs and immutable projects, not renamed labels.
CREATE TABLE sessions (
  run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(run_id),
  project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
  engine TEXT NOT NULL, cwd TEXT NOT NULL, session_id TEXT NOT NULL,
  metadata TEXT NOT NULL DEFAULT '{}' CHECK (json_type(metadata) = 'object'), recorded_at TEXT NOT NULL,
  FOREIGN KEY (project_id, run_id) REFERENCES runs(project_id, run_id)
) STRICT;
CREATE INDEX sessions_identity ON sessions(engine, cwd, session_id);

-- Reserve once per owner question; uncertain/failed external sends never replay.
CREATE TABLE notification_attempts (
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  message_id INTEGER NOT NULL, attempt_id TEXT NOT NULL UNIQUE,
  outcome TEXT NOT NULL CHECK (outcome IN ('reserved','dispatched','failed','uncertain')),
  reserved_at TEXT NOT NULL, finished_at TEXT, stderr TEXT,
  error TEXT CHECK (error IS NULL OR json_type(error) = 'object'),
  PRIMARY KEY (project_id, message_id),
  FOREIGN KEY (project_id, message_id) REFERENCES messages(project_id, id) ON DELETE CASCADE
) STRICT;

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
