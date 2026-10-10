# Plan rows: the schema-3 contract

This is the single reference the plan-normalize lanes (B to H) build against. It replaces the
stored plan document with normalized rows: edits cost what they change, reads can be scoped, and
agents get scoped read tools and one atomic edit tool. Lane A pinned it on `rw/pn-contracts`
(base `5cb6bb7`) and revised it after the astra review (round 2, rebased on `775d57c`, the
shipped dashboard redesign). The design follows the study `plan-normalize.md`: where the study
left a choice open this document decides it (marked **Decision**), and where the study is wrong
about the code it says so (marked **Correction**). Section 13 lists every decision, and which
ones round 2 changed.

Owner rulings (2026-10-10) this contract rests on:

1. **Schema 3**, reached by a drained, fenced migration (§10). AGENTS.md's "SCHEMA_VERSION
   stays 1" rule is replaced (§10.9).
2. **Previews default to `preview_scope: "impact"`**. `"all"` is allowed only with
   `dry_run: true` (§6.3).
3. **`plan_patch` is dropped.** `plan_get` remains the full export, assembled from rows. The
   plan is exported as a document on demand; it is never stored or written as state: no dual
   write, no derived copy.
4. **The drain has a deadline.** Notice, drain, deadline, migration: runs still live at the
   deadline have their cancel requested by the deploy tooling with a reason naming the cutover,
   and the report says how each actually ended, so their orchestrators can retry them
   afterwards (§10.1).

The Rust types are in `sluice_model::plan_rows` (written `plan_rows::X` below). Every JSON block
marked `fixture=<name>` is the file `crates/sluice-model/tests/fixtures/plan_rows/<name>.json`;
`crates/sluice-model/tests/plan_rows.rs` checks that each block equals its file and that each
file round-trips through its type. Change one, change both.

## 1. The study checked against the code

| Study claim | At `5cb6bb7` |
|---|---|
| Readers require exactly 23 tables | **Confirmed.** `schema.rs::verify_schema` refuses any count but 23 (`home_meta` … `notification_attempts`, `sqlite_%` excluded), for readers and writers alike, and refuses a `schema_version` other than 1 (a writer also takes 2). Any new table, or schema 3, makes every older binary refuse the home: the cutover is necessarily incompatible. |
| Schema 2 is the historical board interim | **Confirmed.** `48800ff` shipped the board columns as schema 2; `c554868` returned to 1 and made the writer mark a schema-2 home as 1 (`BOARD_INTERIM_SCHEMA`). 3 is the next free version. |
| The initializer stores `{"steps":{}}` | **Confirmed.** Project creation (the coordinator's `ProjectCreate`) uses `projects::EmptyPlanInitializer`: `plans.doc` is `{"steps":{}}`, `plan_edits` rev 1 has `ops` `[]` and reason `project created`, and a `plan.edit` record with empty `ops` is written. SPEC §6.1 and `docs/agent/plans.md` say a new plan is `{"inputs": {}, "outputs": {}, "steps": {}}`, which the code never produced. `plans::initialize_plan` (any plan at rev 1, no `plan_edits` row) is used by tests and the dashboard fixture, and was used by the Python home's importer: lash, figments and sluice on the live home have no rev-1 row (§10.4.1, round 3). |
| `FrozenPlan` is used in completion only for project identity | **Confirmed, with a correction.** `attempts::complete_frozen` reads the admitted `PlanContext` only for `context.project != id.project`; outputs are checked against the attempt's frozen `returns` and `declared`. **Correction:** the snapshot is stored twice, at `attempts.request.provenance.runtime.completion` and at `attempts.provenance.runtime.completion` (the reservation copies `provenance` into both), and the coordinator's completion path (`coordinator.rs`, the `Callback` completion) fails the completion when the snapshot does not decode, so a schema-3 completion must stop reading it before any attempt lacks it. |
| `plans.doc` readers | **Confirmed, with additions** the study missed (§9): `projects::last_retirement` reads `json_array_length(plan_edits.ops)`; `projects::project_delete` deletes from `plans` and `plan_edits` by name; `coordinator.rs`'s `EditLog` logs `preview.ops.len()`; `dispatch_ext.rs` builds an `EditPreview` for the input preview; `views/step.rs::load_detail` searches `plan.edit` payloads for the JSON pointer `/steps/<id>/paused` to say who paused a step; the public `edits` view is `SELECT * FROM plan_edits`, so its columns are a `query` contract. `naming.rs::for_project` reads `plans.doc` (the study is right that the brief missed it). |
| Patch examples live in `threads.md` and the fn-helper docs | **Correction.** `plan_patch` appears in `docs/agent/{board,plans,instructions,examples}.md` and `docs/rust/schemas.json`; not in `threads.md`, and not in `python/`. |
| Migration processes "archived/deleted projects" | **Correction.** `project_delete` deletes a project's `plans`, `plan_edits`, `steps`, `inputs` and `records` rows. A deleted project has no plan to convert; an archived one does and is converted like a live one. |
| Input declarations are normalized in the projection | **Confirmed.** `plans.rs::projection` writes `{"type": …, "doc": …}` (with `"doc": null`) whatever was written; only `plans.doc` keeps the authored form. |
| `plan_get` compiles | **Confirmed.** It calls `coordinator::context`, which parses and validates against the fn catalog, so a broken catalog breaks the export. |
| Preview lists every ready step | **Confirmed.** `gates::simulate_edit` reconciles and evaluates the whole plan before and after. |

Two facts the study did not state, which this contract relies on:

- **A pure reorder is a no-op today.** `edit_effect` decides "changes nothing" with
  `hash::data_equal_maps`, which ignores key order, so a patch that only moves steps commits
  nothing. Schema 3 makes order explicit (`order.set`, §7.6).
- **No old guardian survives the cutover.** Every run pinned to an old release has ended or been
  cancelled before conversion (§10.1), so nothing in schema 3 keeps old reply shapes for older
  guardians (today's `board_warnings` omission for older runs ends with it).

## 2. Schema 3 storage

### 2.1 Version and verification

- `SCHEMA_VERSION` is 3; `home_meta.schema_version` and `PRAGMA user_version` are 3, set in the
  conversion's transaction (§10) or by a fresh home's creation.
- A fresh home is created from `migrations/0003.sql`, the complete schema-3 DDL: 0001.sql's
  tables with every `ADDED_COLUMNS` entry folded in, the changes below, and the four new
  tables. `ADDED_COLUMNS` and `ADDED_VIEWS` start empty in schema 3. **Decision:** the retired
  `projects.board_slots` column and `board_slots` view are dropped: they were kept only for
  pinned older releases, which cannot exist after the cutover.
- `verify_schema` counts **27** tables: the 23 of schema 1 plus `plan_outputs`, `plan_refs`,
  `plan_edges` and `step_tags`.
- A schema-3 writer or reader opening a home at schema 1 or 2 refuses with
  `StoreError::MigrationRequired { found }`, message `this home is at schema <n>; run "sluice
  home migrate" to bring it to schema 3`. It never converts on open; only `sluice home migrate`
  and a restore's private destination are converted (§10.2). A schema-1 binary refuses a
  schema-3 home by itself (`unsupported schema version 3; expected 1`).
- A converted home and a fresh home are **schema-equivalent**: the same tables, columns
  (`pragma_table_xinfo`: name, type, not-null, default, primary key, hidden/generated), indexes
  (`pragma_index_xinfo`), triggers and views. Lane H tests it.
- `RECORD_PAYLOAD_VERSION` is 2 (§5.2). Every record of a converted home is rewritten to 2.

### 2.2 DDL

Tables 0001.sql already has and schema 3 changes, then the new ones. Columns not shown keep
their 0001.sql definition (with the `ADDED_COLUMNS` folded in).

```sql
-- The plan's header: no document.
CREATE TABLE plans (
  project_id TEXT PRIMARY KEY NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  rev INTEGER NOT NULL CHECK (rev >= 1),
  -- The present root sections in document order, compact JSON, "steps" always present.
  root_order TEXT NOT NULL CHECK (root_order IN (
    '["steps"]',
    '["inputs","steps"]', '["steps","inputs"]',
    '["outputs","steps"]', '["steps","outputs"]',
    '["inputs","outputs","steps"]', '["inputs","steps","outputs"]',
    '["outputs","inputs","steps"]', '["outputs","steps","inputs"]',
    '["steps","inputs","outputs"]', '["steps","outputs","inputs"]')),
  state_epoch INTEGER NOT NULL DEFAULT 0 CHECK (state_epoch >= 0)
) STRICT;

-- Authored edits survive feed trimming; seq deliberately has no records FK.
CREATE TABLE plan_edits (
  project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
  rev INTEGER NOT NULL CHECK (rev >= 1), seq INTEGER NOT NULL CHECK (seq > 0),
  at TEXT NOT NULL, author TEXT NOT NULL, reason TEXT NOT NULL,
  changes TEXT NOT NULL CHECK (json_type(changes) = 'array'),
  PRIMARY KEY (project_id, rev)
) STRICT;

-- inputs: unchanged columns. `declaration` is now the authored declaration exactly as written
-- ("string", {"type": …} or {"type": …, "doc": …}); `value` stays the runtime value.

-- steps: unchanged columns except these five (`unit` becomes NOT NULL; `paused` changes; `run`,
-- `priority` and `needs` are new). Each of the last four is a plain column whose CHECK holds it
-- equal to the declaration, so no write can let it drift, and the writer sets it with that
-- same expression (§4). They are not generated columns: SQLite never reads an index holding a
-- generated column as covering (checked on the bundled 3.53.2, VIRTUAL and STORED alike), and
-- compact reads must not touch the declaration.
--   unit     TEXT NOT NULL                       -- rebuildable index (§2.3)
--   paused   TEXT NOT NULL CHECK (paused IS (
--              CASE json_type(declaration, '$.paused')
--                WHEN 'true' THEN 'true'
--                WHEN 'text' THEN json_quote(json_extract(declaration, '$.paused'))
--                ELSE 'false' END))
--   run      TEXT NOT NULL CHECK (run IS json_extract(declaration, '$.run'))
--   priority INTEGER NOT NULL CHECK (priority IS coalesce(json_extract(declaration, '$.priority'), 0))
--   needs    TEXT CHECK (needs IS json_extract(declaration, '$.needs'))
-- The schema-1 index steps_status is replaced by the covering indexes below.
CREATE INDEX steps_compact ON steps(project_id, position, step_id, run, priority, unit, paused, status);
CREATE INDEX steps_unit_compact ON steps(project_id, unit, position, step_id, run, priority, paused, status);
CREATE INDEX steps_status_compact ON steps(project_id, status, position, step_id, run, priority, unit, paused);
-- The steps that need resources, for admission competitors (§6.3) and the settings page.
CREATE INDEX steps_needs ON steps(project_id, status, step_id, needs, priority) WHERE needs IS NOT NULL;

CREATE TABLE plan_outputs (
  project_id TEXT NOT NULL REFERENCES plans(project_id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  position INTEGER NOT NULL CHECK (position >= 0),
  binding TEXT NOT NULL CHECK (json_type(binding) = 'object'),
  PRIMARY KEY (project_id, name),
  UNIQUE (project_id, position)
) STRICT;

CREATE TABLE plan_refs (
  project_id TEXT NOT NULL REFERENCES plans(project_id) ON DELETE CASCADE,
  consumer_kind TEXT NOT NULL CHECK (consumer_kind IN ('step', 'output')),
  consumer_id TEXT NOT NULL,
  slot TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  kind TEXT NOT NULL CHECK (kind IN ('binding', 'gate', 'output')),
  source_kind TEXT NOT NULL CHECK (source_kind IN ('step', 'input', 'unit')),
  source_id TEXT NOT NULL,
  source_port TEXT NOT NULL DEFAULT '',
  source_path TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (project_id, consumer_kind, consumer_id, slot, ordinal),
  CHECK ((consumer_kind = 'output') = (kind = 'output'))
) STRICT;
CREATE INDEX plan_refs_source ON plan_refs(project_id, source_kind, source_id, consumer_kind, consumer_id);

CREATE TABLE plan_edges (
  project_id TEXT NOT NULL,
  source_step TEXT NOT NULL,
  target_step TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('data', 'gate')),
  via_unit TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (project_id, source_step, target_step, kind, via_unit),
  FOREIGN KEY (project_id, source_step) REFERENCES steps(project_id, step_id) ON DELETE CASCADE,
  FOREIGN KEY (project_id, target_step) REFERENCES steps(project_id, step_id) ON DELETE CASCADE
) STRICT;
CREATE INDEX plan_edges_target ON plan_edges(project_id, target_step, source_step);

CREATE TABLE step_tags (
  project_id TEXT NOT NULL,
  step_id TEXT NOT NULL,
  tag TEXT NOT NULL,
  PRIMARY KEY (project_id, step_id, tag),
  FOREIGN KEY (project_id, step_id) REFERENCES steps(project_id, step_id) ON DELETE CASCADE
) STRICT;
CREATE INDEX step_tags_select ON step_tags(project_id, tag, step_id);

-- The execution-state witness (§3): any write a preparation could have read moves it.
CREATE TRIGGER steps_state_epoch AFTER UPDATE OF status, outputs, inputs_hash, skipped, error,
  manual, result_id, work_generation, instances ON steps
BEGIN UPDATE plans SET state_epoch = state_epoch + 1 WHERE project_id = NEW.project_id; END;
CREATE TRIGGER inputs_state_epoch AFTER UPDATE OF value ON inputs
BEGIN UPDATE plans SET state_epoch = state_epoch + 1 WHERE project_id = NEW.project_id; END;
CREATE TRIGGER projects_state_epoch AFTER UPDATE OF paused ON projects
BEGIN UPDATE plans SET state_epoch = state_epoch + 1 WHERE project_id = NEW.project_id; END;
CREATE TRIGGER resources_insert_state_epoch AFTER INSERT ON resources WHEN NEW.project_id IS NOT NULL
BEGIN UPDATE plans SET state_epoch = state_epoch + 1 WHERE project_id = NEW.project_id; END;
CREATE TRIGGER resources_update_state_epoch AFTER UPDATE ON resources WHEN NEW.project_id IS NOT NULL
BEGIN UPDATE plans SET state_epoch = state_epoch + 1 WHERE project_id = NEW.project_id; END;
CREATE TRIGGER resources_delete_state_epoch AFTER DELETE ON resources WHEN OLD.project_id IS NOT NULL
BEGIN UPDATE plans SET state_epoch = state_epoch + 1 WHERE project_id = OLD.project_id; END;

-- A removed consumer takes its references with it, whatever removed it (an edit, a prune, a
-- unit removal, a project deletion): plan_refs cannot cascade from steps, whose consumers it
-- shares with plan outputs.
CREATE TRIGGER steps_refs_delete AFTER DELETE ON steps
BEGIN DELETE FROM plan_refs WHERE project_id = OLD.project_id AND consumer_kind = 'step'
  AND consumer_id = OLD.step_id; END;
CREATE TRIGGER plan_outputs_refs_delete AFTER DELETE ON plan_outputs
BEGIN DELETE FROM plan_refs WHERE project_id = OLD.project_id AND consumer_kind = 'output'
  AND consumer_id = OLD.name; END;

-- Public view for `query` (§2.6).
CREATE VIEW edits AS SELECT project_id, rev, seq, at, author, reason, changes FROM plan_edits;
```

**Decisions** in this DDL, against the study's:

- `plans.root_order` is constrained to its eleven possible values instead of "a JSON array".
- `plan_edits` has no `base_rev` (it is always `rev - 1`) and no `change_version` (the schema
  and the record payload version say it). The record has neither either (§5.2).
- `steps.paused`, `run`, `priority` and `needs` are plain columns held equal to the
  declaration by a `CHECK` (no writer can let them drift), written with the CHECK's own
  expression. **Round 2:** they were generated columns, but SQLite never uses an index holding a
  generated column as a covering index, so every compact read would have loaded the row and its
  declaration. `steps.unit` stays a maintained index column: a step's unit needs its tag list
  searched, which a `CHECK` cannot express; `verify` compares it.
- Compact reads must not decode declarations, so they go through the three **covering**
  indexes, one per access path (by position, by unit, by status); `EXPLAIN QUERY PLAN` says
  `USING COVERING INDEX` for each (lane H, §11). `steps_needs` is the partial covering index of
  the steps that need resources: admission competitors and the settings page read it without a
  declaration. The study's `steps_run_position` is dropped: no tool filters by fn.
- The `plan_refs` triggers delete a removed step's or output's references in the transaction
  that removes it, so no removal path can leave a reference that would falsely block a later
  removal of its source (round 2).
- `state_epoch` is maintained by triggers, so no write path can forget it. Over-counting is
  harmless (an extra re-preparation), under-counting is a bug the triggers rule out. Leases do
  **not** move it: they affect only the advisory `would_queue`, and moving the epoch on every
  grant and release would make busy projects re-prepare edits for nothing.
- The `CHECK` on `plan_refs` ties `kind = 'output'` to output consumers.

Table order in `project_delete` becomes: … `steps`, `step_results`, `inputs`, `plan_refs`,
`plan_outputs`, `plan_edits`, `plans` … (`plan_edges` and `step_tags` go with `steps`).

### 2.3 Authoritative rows and rebuildable indexes

| Storage | What it is |
|---|---|
| `plans.rev` | The authored revision (§3). |
| `plans.root_order` | Authoritative: the present root sections and their order. |
| `inputs.(name, position, declaration)` | Authoritative: each plan input as written, and its place. |
| `plan_outputs.(name, position, binding)` | Authoritative: each plan output as written, and its place. |
| `steps.(step_id, position, declaration)` | Authoritative: each step as written (bindings, outputs, scatter, tags, pause, gates, needs, priority), and its place. |
| `inputs.value`, `steps.status` … `progress_run`, `generation`, `work_generation` | Runtime state, as in schema 1. |
| `steps.unit`, `step_tags`, `plan_refs`, `plan_edges` | **Rebuildable indexes**, written from the declarations in the same transaction as the change that makes them (a removed consumer's rows go by cascade or trigger). Never an edit target; `verify` rebuilds and compares them, and lane H requires exact equality after every removal and prune (§11). |
| `steps.paused`, `steps.run`, `steps.priority`, `steps.needs` | Copies of declaration fields, held equal to it by `CHECK`s. |
| `plans.state_epoch` | The execution-state witness, moved by triggers. |

The plan **document** is never stored. `export_plan` (§8) assembles it from the authoritative
rows on demand.

### 2.4 Positions and root order

- A collection's positions are unique per project and order it. Ties cannot occur
  (`UNIQUE (project_id, position)`); readers order by `position` alone, `step_id` only as a
  formal tie-break.
- A new row takes `max(position) + 1` of its collection (0 when empty), the maximum read
  through the unique index. Removing a row leaves a gap; nothing ever renumbers survivors except
  `order.set` (§7.6), which rewrites the whole collection to `0 … n-1` in its new order.
- `root_order` holds the sections present in the export, in its order. A section may be present
  and empty. **Decision:** a put into a section `root_order` lacks adds the section (a
  `header.put` change in the same edit), inserted before the first present section that follows
  it in the canonical order `inputs`, `outputs`, `steps`, else at the end. Removing a section's
  last row leaves the section present. Only legacy history (§10.6) ever removes a section.
- **Decision:** a project created in schema 3 starts with `root_order`
  `["inputs","outputs","steps"]` and no rows, exporting `{"inputs": {}, "outputs": {}, "steps":
  {}}` as SPEC §6.1 and the agent docs have always said. Its rev-1 history entry is that one
  `header.put`. Converted projects keep their real origin `["steps"]` (§10.6).

### 2.5 Index rows: exact derivation

The model derives every index row from declarations alone (no fn manifests), given which bare
names are steps (inputs and steps share one namespace):

**`steps.unit`**: the part after `unit:` of the first `unit:` tag, else the step id.

**`step_tags`**: one row per distinct tag of `declaration.tags`.

**`plan_refs`** of a step (consumer `step`, the step id):

| Declaration | `slot` | `ordinal` | `kind` | source |
|---|---|---|---|---|
| `in.<x>` = `{"source": "<ref>"}` | `in.<x>` | 0 | `binding` | the ref |
| `in.<x>` = `{"source": ["<ref>", …]}` | `in.<x>` | index in the list | `binding` | each ref |
| `after[i]` = `s` or `s?` (a step) | `after` | `i` | `gate` | `step`, `s`, port `""`, path `""` |
| `after[i]` = `unit:u` or `unit:u?` | `after` | `i` | `gate` | `unit`, `u`, port `""`, path `""` |
| `after[i]` = `r` or `!r` (a ref) | `after` | `i` | `gate` | the ref |

`{"default": …}` and `{"file": …}` bindings make no row. A ref `<input>[.<path>]` is source
`input`, id `<input>`, port `""`; a ref `<step>/<output>[.<path>]` is source `step`, id
`<step>`, port `<output>`; `path` is the dotted remainder without its leading dot (`gate/items.0`
→ step `gate`, port `items`, path `0`). A plan output's single row is consumer `output` (its
name), slot `source`, ordinal 0, kind `output`. A gate entry the model cannot classify (a bare
name that is neither a step nor an input) makes no row; validation refuses it anyway.

**`plan_edges`** (target depends on source; both are current steps): one `data` edge per
distinct step source of the target's `binding` refs; one `gate` edge per distinct step source
of its step gates and boolean-ref gates; and for each `unit:u` gate, one `gate` edge from each
current exit step of `u` with `via_unit = u`. These are exactly `plan::expanded_dependencies`,
split by kind. A unit's exit steps are its steps tagged `exit`, else its sinks over the edges
inside the unit (`units::derive_units`). When a unit's exits change, every step gating on
`unit:u` (found through `plan_refs_source`) has its incoming edges rewritten.

`FullStep.references` (§7.4) are the step's `plan_refs` rows in `(slot, ordinal)` order.

### 2.6 Views for `query`

`edits` keeps its name with the columns `project_id, rev, seq, at, author, reason, changes`
(`ops` is gone). `outcomes`, `log`, `step_changes` and `questions` are unchanged; `board_slots` is
dropped. The new tables are public to `query` as they are. SPEC §3 says so.

## 3. Revisions, epochs and tokens

- **`rev`** is the authored plan revision, as in schema 1: one nonempty edit adds exactly one;
  input values, statuses, progress, pauses of the project and recipe-file changes never move it.
- **`state_epoch`** (`plan_rows::StateEpoch`) is the project's execution-state witness: the
  triggers of §2.2 add one whenever a step's status, outputs, inputs hash, skip reasons, error,
  manual flag, result, work generation or scatter instances change, a plan input's value
  changes, the project's pause changes, or the project's resources change. Progress, messages,
  leases and records never move it. It only grows.
- **`recipe_generation`** (`plan_rows::RecipeGeneration`) is the first 16 hex digits of the
  SHA-256 of the recipe listing the project sees: for the home's `recipes/` then the project's
  `projects/<id>/recipes/`, each `.json` file's name, size and modification time in
  nanoseconds, sorted by name (the same inputs as today's `naming::recipe_files`). **Decision:**
  a digest, not a counter, because recipe files change outside sluice. Compared for equality
  only.
- **`catalog_generation`** (`plan_rows::CatalogGeneration`) counts the coordinator's fn-catalog
  publications in memory. It restarts with the coordinator, which drops every prepared edit.
- **`board_rev`** is `projects.board_rev`: board warnings depend on the board program.

`plan_rows::ValidationTokens` is the five together: what a prepared edit was worked out from.
`plan_rev`, `state_epoch` and `board_rev` are re-read and compared inside the writer
transaction; `catalog_generation` and `recipe_generation` are compared by the coordinator
immediately before it hands the edit to the writer (the writer never touches files).

## 4. The edit transaction

Every edit, whatever tool made it, goes through one pipeline (lanes C and D), and the same
store command commits it (lane B).

**Preparation** (outside the writer, in one read snapshot): resolve the project; if the request
gave `rev` and the plan is at another one, refuse `conflict` (`plan is at rev N`,
`current_rev`) without preparing. Read the tokens, apply the operations to a candidate (§7.6),
validate it incrementally (§6.1), reconcile the affected runtime state, and build the preview
(§6.3). What preparation reads is its **read set** (§6.4), read in rounds within the one
snapshot; it is wider than the preview's affected set. The result is a
`plan_rows::PreparedPlanEdit` (§8): the store payload `commit: PlanEditCommit` (`tokens`, the
caller's `rev`, `author`, `reason`, `rows: RowDelta`: the logged changes plus every index row
they imply, `state: StateDelta`: removed and added steps, status transitions, and `prune`), the
reply's parts (`preview`, `steps`, `board_warnings`, and for `step_set_input` and `plan_prune`
their reports), and `compiled`: the certified compiled candidate (§8.1). A dry run returns the
preview here and never reaches the writer; its candidate is dropped.

**Commit** (`plans::commit_plan_edit`, in the writer, given the `PlanEditCommit` only):

1. Check the project is live and the home admits plan edits (drain, §10.1).
2. Re-read `plans.rev`, `plans.state_epoch` and `projects.board_rev`. A request `rev` that is
   not current is `conflict` with `current_rev`. Any other token mismatch returns
   `CommitOutcome::Stale`: nothing is written and the coordinator prepares again.
3. If `rows.changes` is empty, write nothing: the reply is the edit result at the current `rev`
   with `preview.changes` `[]`.
4. For each step in `state.removed`: archive its outcome (`snapshot_result` when finished, then
   `removed_at`), then delete its row. Its `step_tags` and `plan_edges` go by cascade and its
   consumer `plan_refs` (`consumer_kind = 'step'`, `consumer_id` the step) by the
   `steps_refs_delete` trigger, in this transaction (round 2).
5. Apply `rows.changes` (a set, §5.1): `header.put` updates `root_order`; deletes remove input
   rows (with their values) and output rows (their `plan_refs` by trigger); puts insert or
   update `(position, declaration)`, and a step put sets `paused`, `run`, `priority` and `needs`
   with the expressions their `CHECK`s state (§2.2). A put that moves a row first moves it above
   the collection's current maximum, so no two rows ever share a position mid-transaction. A
   newly inserted step row (`state.added`) is `pending` with `generation` the new revision.
6. Write the index rows: for each `rows.step_index` entry set `steps.unit` and replace the
   step's `step_tags` and its consumer `plan_refs`; for each `rows.output_refs` entry replace
   that output's `plan_refs`; for each `rows.edges` entry replace that target's incoming
   `plan_edges`.
7. Apply `state.transitions` (status, skip reasons, error; a finished one snapshots its
   result) with a `step.status` record each, as `write_status_changes` does today.
8. Append the `plan.edit` record (payload version 2, §5.2) and insert the `plan_edits` row with
   the record's `seq` and `at` and the same `changes`.
9. Set `plans.rev` to the new revision; mark the `plan` and `edits` change views (and `status`
   when step 4 or 7 wrote anything).

Steps 4 to 9 commit together or not at all. The triggers move `state_epoch` as a side effect.
After `Committed`, the coordinator installs `compiled` in its plan cache (§8.1); after `Stale`,
a conflict or any error it drops it.

**Re-preparation** (round 2; replaces decision 11's writer fallback). A stale commit is
prepared again from a new snapshot, outside the writer, at most `EDIT_TRIES` (3) preparations
in all. When the third is stale too, the edit is refused `busy` with `retryable: true`
(`PlanRowsError::EditContended`, message `the plan's state kept changing while this edit was
prepared (3 tries); send it again`); nothing was written. **Decision:** preparation never runs
in the writer, incremental or not: an affected set can be the whole plan (a high-fanout input,
staleness down a long chain, every ready competitor of a resource), so "incremental" is no bound
on writer time. A caller retries the edit as it would any `busy`; an explicit `rev` still means
what it meant. Lane D's barrier test (§11) holds it.

**Running steps.** A step running in the base may, in the candidate, differ only in `paused`
and `tags`, and may not be removed (`steps.<id>: cannot remove or change a running step except
paused and tags`). Because a held `state_epoch` means no status changed since preparation, the
writer does not check this again.

**Net effect per id.** What counts is each id before and after the whole edit: an id present in
both is *changed* (its runtime state follows the change rules) even if the operations removed
and re-added it; an id only in the candidate is *added*; an id only in the base is *removed*.

## 5. Logged changes and history

### 5.1 The change union

`plan_rows::PlanChange` is what history records: resolved rows, never the operations that
produced them, so a later recipe change cannot change what an old edit did. A changed step
carries its whole declaration; untouched steps never appear; index rows and runtime state never
appear.

```json fixture=plan_change.all
[
  {
    "op": "header.put",
    "root_order": [
      "inputs",
      "outputs",
      "steps"
    ]
  },
  {
    "op": "input.delete",
    "name": "old_repo"
  },
  {
    "op": "output.delete",
    "name": "draft"
  },
  {
    "op": "step.delete",
    "step": "scratch-a"
  },
  {
    "op": "input.put",
    "name": "repo",
    "position": 0,
    "declaration": "string"
  },
  {
    "op": "output.put",
    "name": "notes",
    "position": 0,
    "binding": {
      "source": "notes/final"
    }
  },
  {
    "op": "step.put",
    "step": "review",
    "position": 503,
    "declaration": {
      "run": "agent.review",
      "in": {
        "spec": {
          "source": "work/final"
        }
      }
    }
  }
]
```

A revision's changes are a **set**: at most one change per collection and key, listed
`header.put`, then `input.delete`, `output.delete`, `step.delete` (each by key), then
`input.put`, `output.put`, `step.put` (each by position). An edit preview lists the same.

### 5.2 The `plan.edit` record and the `plan_edits` row

The `plan.edit` record's payload (version 2) is `plan_rows::PlanEditEvent`: `{kind, rev,
author, reason, changes}`; `ops` is gone. The log returns it with `seq`, `at` and `project`:

```json fixture=plan_edit.record
{
  "seq": 88213,
  "at": "2026-10-10T14:02:11Z",
  "project": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
  "kind": "plan.edit",
  "rev": 42,
  "author": "orchestrator",
  "reason": "Review the work before the release",
  "changes": [
    {
      "op": "input.put",
      "name": "reviewer",
      "position": 2,
      "declaration": {
        "type": "string",
        "doc": "Who reviews"
      }
    },
    {
      "op": "step.put",
      "step": "release",
      "position": 12,
      "declaration": {
        "run": "agent.run",
        "tags": [
          "unit:release"
        ],
        "after": [
          "tests-main",
          "review"
        ],
        "in": {
          "engine": {
            "default": "claude"
          },
          "spec": {
            "default": "Cut the release"
          }
        }
      }
    },
    {
      "op": "step.put",
      "step": "review",
      "position": 505,
      "declaration": {
        "run": "agent.review",
        "after": [
          "work"
        ],
        "in": {
          "spec": {
            "source": "work/final"
          }
        }
      }
    }
  ]
}
```

The `plan_edits` row stores `rev, seq, at, author, reason` and the same `changes` array.
`RECORD_PAYLOAD_VERSION` becomes 2 for every kind (**Decision:** one version per home, so the
runtime has one decoder; kinds other than `plan.edit` keep their content). `events::Event::PlanEdit`
becomes `{rev, author, reason, changes: Vec<PlanChange>}`.

### 5.3 Rebuilding a plan from history

Declaration history rebuilds the plan; a database backup restores runtime state (the retained
`plan.input`, `step.output` and `step.retry` records are trimmed with the log and cannot rebuild
execution state). Rebuild starts from no rows, applies each revision's changes in revision order
(as a set: deletes, then puts), and derives the indexes. It never re-expands recipes and never
validates a historical revision against today's fn manifests. Lane H tests that rebuilding every
converted project reproduces its rows.

### 5.4 `plan_history`

`plan_history(project, since_rev?, after_seq?, limit=200)` returns `plan_rows::PlanHistoryPage`:
the plan's edits (from `plan_edits`, never trimmed) and the log's retained `plan.input`,
`step.output` and `step.retry` records, merged by `seq`, oldest first. `since_rev` keeps entries
whose `rev` is greater, `after_seq` those whose `seq` is greater; both apply together. `limit`
is 1 to 1000 (larger reads as 1000, 0 is `bad_request`). `next_after_seq` is the last entry's
`seq` when more entries match, else null.

```json fixture=plan_history.request
{
  "project": "lash",
  "after_seq": 88100,
  "limit": 3
}
```

```json fixture=plan_history.reply
{
  "project": {
    "project_id": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
    "name": "lash"
  },
  "entries": [
    {
      "seq": 88140,
      "at": "2026-10-10T13:40:02Z",
      "project": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
      "kind": "plan.input",
      "rev": 41,
      "author": "owner",
      "reason": "",
      "name": "reviewer_default",
      "value": "sam"
    },
    {
      "seq": 88177,
      "at": "2026-10-10T13:51:45Z",
      "project": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
      "kind": "step.retry",
      "rev": 41,
      "author": "orchestrator",
      "reason": "flake",
      "step": "tests-main",
      "work": 4
    },
    {
      "seq": 88213,
      "at": "2026-10-10T14:02:11Z",
      "project": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
      "kind": "plan.edit",
      "rev": 42,
      "author": "orchestrator",
      "reason": "Review the work before the release",
      "changes": [
        {
          "op": "input.put",
          "name": "reviewer",
          "position": 2,
          "declaration": {
            "type": "string",
            "doc": "Who reviews"
          }
        },
        {
          "op": "step.put",
          "step": "release",
          "position": 12,
          "declaration": {
            "run": "agent.run",
            "tags": [
              "unit:release"
            ],
            "after": [
              "tests-main",
              "review"
            ],
            "in": {
              "engine": {
                "default": "claude"
              },
              "spec": {
                "default": "Cut the release"
              }
            }
          }
        },
        {
          "op": "step.put",
          "step": "review",
          "position": 505,
          "declaration": {
            "run": "agent.review",
            "after": [
              "work"
            ],
            "in": {
              "spec": {
                "source": "work/final"
              }
            }
          }
        }
      ]
    }
  ],
  "next_after_seq": 88213
}
```

A converted project's rev 1 reads:

```json fixture=plan_history.origin
{
  "seq": 12,
  "at": "2026-10-03T09:00:00Z",
  "project": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
  "kind": "plan.edit",
  "rev": 1,
  "author": "cli",
  "reason": "project created",
  "changes": [
    {
      "op": "header.put",
      "root_order": [
        "steps"
      ]
    }
  ]
}
```

## 6. Validation

### 6.1 What each change re-checks

Preparation validates the candidate incrementally against a certified compiled base. Each row
of the matrix names what a change must re-check; anything it does not name keeps the base's
certificate.

| Change | Re-checks |
|---|---|
| Added or changed step | Its id (format, reserved `owner`/`orchestrator`, the shared namespace against every plan input of the candidate); its `run` visible to the project; required fn inputs bound and no unknown inputs (extra inputs and declared outputs only on open fns); literal defaults against their types; file-binding syntax; scatter; declared outputs; tags (format, one `unit:` tag, the singleton-unit collision rule); gate syntax and each gate's referent; priority; `needs` against the project's resources **only if `needs` changed**. Every source it reads must exist and fit. |
| Added or changed plan input (`input.put`) (round 2) | Its name (format `^[a-z0-9][a-z0-9_-]*$`) and its declaration (a type string, or `{"type": …}` / `{"type": …, "doc": …}` with a valid type), at `inputs.<name>`; the shared namespace against **every** step of the candidate, untouched ones included, reported as today's whole-plan pass reports it, at the step: `steps.<id>: plan inputs and steps share one namespace`; the input's current runtime value, if it has one, against the new type, at `inputs.<name>` (as `validate_input_values`); and every reader of the input: each binding (nested paths and fan-in elements included), boolean gate and plan output that reads it (`plan_refs_source`, `source_kind = 'input'`), re-typed against the new declaration. |
| Added or changed plan output (`output.put`) (round 2) | Its name format, at `outputs.<name>`; the binding's shape (`outputs.<name>: expected {"source": "<ref>"}`); and its source: the step or plan input it names exists in the candidate, the step has that output, and the path is valid for its type (`Plan::reference_type`: `outputs.<name>: unknown step <s>`, `… unknown plan input <i>`, `… step <s> (fn <f>) has no output <o>`, or the path's error). Plan outputs have no readers, so nothing else is re-checked. |
| Changed step output type (a changed `run`, `outputs` or `scatter`) | Every reader of the step's outputs: bindings (including nested paths and fan-in elements), boolean gates and plan outputs. |
| Removed step or input | Every surviving reference to it, from steps and plan outputs, is refused (`steps.<id>.in.<x>: unknown step <s>`, `outputs.<name>: unknown plan input <i>`, …). A removed plan output has no readers. |
| Changed tags, `after` or a binding that moves a step between units, or changes edges inside a unit, or adds or removes an `exit` tag | The old and new unit's membership, entry and exit steps; every `unit:u` gate on either unit (existence, "a unit cannot depend on its own exits"); their expanded edges. |
| Any added edge | Cycles: remove superseded edges, add the batch's new edges, then search from each added edge's target for its source. The whole batch is checked together, so two edges harmless alone cannot form a cycle together. |
| Recipe expansion (`unit.add`) | Params, substitution, id collisions with the candidate, suffix overrides (`after`, `inputs`), reserved tags, the new unit's entry steps, and the expanded steps' external references. |
| Running step (base) | The net change is only `paused` and `tags`; not removed. |
| Prune | The selected units are done; the surviving-reference closure; each removed step's result is the one the store's evidence froze at the age cutoff. |
| Board references | Board warnings for removed steps and changed tag selections (never a refusal). |

Existing exception policies stay: a lowered resource capacity never refuses an edit that does
not change a step's `needs`, and a retired fn model string never refuses an edit that does not
change that step. Error paths are those of today's whole-plan validation
(`steps.notes.in.cwd: repo is int, which does not fit string: int is not string`).

The matrix is checked differentially (lane C against lane H's reference compiler): for any base
plan and operations, the incremental validator accepts exactly when today's whole-plan
compilation of the candidate does, with the same errors. The review's two counterexamples to
the first matrix are pinned, with three more of the new rows, as the differential fixture
`validation.differential` (`crates/sluice-model/tests/plan_rows.rs` checks each `candidate`
against today's compiler now; lane H's harness applies each case's `ops` to its `base` and
compares the incremental result with them):

```json fixture=validation.differential
[
  {
    "case": "output_put_missing_source",
    "base": {
      "steps": {
        "work": {
          "run": "custom.open"
        }
      }
    },
    "ops": [
      {
        "op": "output.put",
        "name": "notes",
        "source": "review/final"
      }
    ],
    "candidate": {
      "outputs": {
        "notes": {
          "source": "review/final"
        }
      },
      "steps": {
        "work": {
          "run": "custom.open"
        }
      }
    },
    "errors": [
      "outputs.notes: unknown step review"
    ]
  },
  {
    "case": "input_put_collides_with_untouched_step",
    "base": {
      "steps": {
        "repo": {
          "run": "custom.open"
        }
      }
    },
    "ops": [
      {
        "op": "input.put",
        "name": "repo",
        "declaration": "string"
      }
    ],
    "candidate": {
      "inputs": {
        "repo": "string"
      },
      "steps": {
        "repo": {
          "run": "custom.open"
        }
      }
    },
    "errors": [
      "steps.repo: plan inputs and steps share one namespace"
    ]
  },
  {
    "case": "output_put_missing_input",
    "base": {
      "steps": {}
    },
    "ops": [
      {
        "op": "output.put",
        "name": "notes",
        "source": "brief"
      }
    ],
    "candidate": {
      "outputs": {
        "notes": {
          "source": "brief"
        }
      },
      "steps": {}
    },
    "errors": [
      "outputs.notes: unknown plan input brief"
    ]
  },
  {
    "case": "input_put_bad_name",
    "base": {
      "steps": {}
    },
    "ops": [
      {
        "op": "input.put",
        "name": "Repo",
        "declaration": "string"
      }
    ],
    "candidate": {
      "inputs": {
        "Repo": "string"
      },
      "steps": {}
    },
    "errors": [
      "inputs.Repo: ids match ^[a-z0-9][a-z0-9_-]*$"
    ]
  },
  {
    "case": "input_put_value_no_longer_fits",
    "base": {
      "inputs": {
        "limit": "string"
      },
      "steps": {}
    },
    "values": {
      "limit": "ten"
    },
    "ops": [
      {
        "op": "input.put",
        "name": "limit",
        "declaration": "int"
      }
    ],
    "candidate": {
      "inputs": {
        "limit": "int"
      },
      "steps": {}
    },
    "errors": [
      "inputs.limit: expected int, got \"ten\""
    ]
  }
]
```

`values` are the plan inputs' current runtime values, checked against the candidate's
declarations as `Plan::validate_input_values` does; `errors` are the `PathError`s in order.

### 6.2 Whole-plan passes that remain

A whole-plan pass (decoding every declaration, or compiling every step) runs only for: a cold
compile (cache miss), `verify`, conversion (§10), rebuild (§5.3), `plan_get` (export, no
compile), `preview_scope: "all"`, `order.set` on `steps` (positions only, no compile), and a fn
catalog publication that changes a signature some step uses (its users and their readers are
re-checked before a new certificate). Nothing else may decode or compile the whole plan; lane H
counts it (§11).

### 6.3 Preview scope

`preview_scope: "impact"` (the default) describes the edited steps and what the edit changes for
others. Its **affected set** is: every step the edit adds or changes; every step whose
reconciled state (status, skip reasons, error, inputs hash, readiness) the edit changes; and,
for each step whose `needs` or `priority` changed, the ready steps competing for the same
resources. The affected set decides what the preview lists, not what preparation may read
(§6.4). Within the affected set: `would_start` lists the pending, ready, executable steps
(never `core.external`), `would_queue` those of them admission would queue on cached
capacities, `would_skip` and `would_stale` the steps becoming skipped or stale, and `errors` the
failures the reconciliation produces.

`preview_scope: "all"` is today's preview: the whole plan reconciled and simulated before and
after. It is allowed only with `dry_run: true`; otherwise `bad_request` (`preview_scope "all"
needs dry_run: true`). The reply's `preview.scope` says which was used. A preview never runs a
fn or a capacity callback.

### 6.4 The preparation read set (round 2)

Reconciling and validating the affected set needs state outside it: changing `b` to read `a/out`
needs `a`'s status and output though `a` is unchanged, and a gate needs its referent's status.
So preparation reads a **read set** (`plan_rows::PreparationReads`), separate from the affected
set, in one read snapshot (`plans::read_scoped_state`, §8):

| Read | What | Why |
|---|---|---|
| `steps` | Status, outputs, inputs hash, skip reasons, error, instances (scatter and fan-in) and result of: every step in the affected set; every **incoming source** of each (each step its bindings read, fan-in elements included, and each step its gates name or read a boolean from), unchanged ones included; the **exit steps** of every unit an affected step gates on with `unit:u`; and each admission competitor below. | Reference resolution (`gates::resolve`) and gate evaluation read the source's status and outputs; staleness compares inputs hashes; a unit gate reads its exits. |
| `inputs` | Current values of every plan input an affected step or a changed output reads, and of every input the edit puts or removes. | Bindings and boolean gates read values; `input.put` re-checks the value (§6.1). |
| `project_pause` | The project's pause. | A paused project starts nothing (`would_start`). |
| `resources` | For each resource an affected step needs: its cached capacity and limit, and its `waiting` and `held` leases. | `would_queue`. |
| `competitors` | For each such resource, the pending steps that need it (`steps_needs`, no declaration read) with their priority; their own incoming sources join `steps` so their readiness can be decided. | Admission order among ready steps. |

Preparation reads in rounds inside the snapshot: it starts from the operations' targets (and a
unit operation's members); each round adds the sources, input values, unit exits, resources and
competitors of the steps the previous round added to the affected set; it ends when a round adds
none. A step outside the read set is never consulted: `StateSnapshot::status`'s default of
`pending` for a missing step must not stand in for an unread one (lane C asserts it in debug
builds). The read set is bounded by the affected set and its one-hop neighbourhood, not by the
plan; lane H counts `state_rows_read` (§11).

## 7. Tools

### 7.1 Conventions

- Arguments are flat, the same over MCP, HTTP, `ctx.tool` and `sluice tool`. In every example
  below `project` is the public string (a current name or `id:<uuid>`); on the wire it is a
  `ProjectSelector`. The new request types are flat and their field names are the public names,
  so no renames apply. `units`, `steps` and `status` accept a single string as a one-item list.
- `author` defaults as SPEC §12.3 says. `reason` is required by `plan_edit`, `unit_update` and
  `unit_remove` (**Decision**: an atomic batch says why; the typed tools keep `reason=""`).
- Every edit tool takes `rev?`, `dry_run=false`, `preview_scope="impact"`, `reason`,
  `author?`, and returns `plan_rows::EditResult` (or, with `dry_run`, `plan_rows::EditPreview`).
- Errors are `PublicError`s. The new refusals and their exact messages are in §7.11.

### 7.2 `plan_get`

`plan_get(project)` → `plan_rows::PlanGetResult` `{project, rev, plan}`: the document assembled
from the rows (`export_plan`), never compiled, so it works while the project's fn catalog is
broken. Its sections and keys come out in `root_order` and position order, each declaration as
written.

```json fixture=plan_get.reply
{
  "project": {
    "project_id": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7c",
    "name": "demo"
  },
  "rev": 7,
  "plan": {
    "inputs": {
      "repo": "string",
      "tasks": {
        "type": "string[]",
        "doc": "One task per item"
      }
    },
    "outputs": {
      "notes": {
        "source": "notes/final"
      }
    },
    "steps": {
      "work": {
        "run": "agent.run",
        "scatter": "spec",
        "tags": [
          "unit:build"
        ],
        "in": {
          "engine": {
            "default": "devin"
          },
          "cwd": {
            "source": "repo"
          },
          "spec": {
            "source": "tasks"
          }
        }
      },
      "gate": {
        "run": "core.collect",
        "tags": [
          "unit:build",
          "exit"
        ],
        "in": {
          "items": {
            "source": [
              "work/final"
            ]
          }
        }
      },
      "notes": {
        "run": "agent.run",
        "after": [
          "unit:build"
        ],
        "in": {
          "engine": {
            "default": "claude"
          },
          "cwd": {
            "source": "repo"
          },
          "spec": {
            "source": "gate/items.0"
          }
        }
      }
    }
  }
}
```

### 7.3 `plan_read`

`plan_read(project, units?, steps?, status?, recipe?, compact=true, limit=200, cursor?)` →
`plan_rows::PlanReadResult`.

- **Filters.** `units` (unit names), `steps` (step ids), `status` (stored statuses: `pending`,
  `running`, `succeeded`, `failed`, `stale`, `skipped`; never the dashboard's words) and
  `recipe` (a recipe name: the units it matches now, SPEC §6.8). A list matches any of its
  values; filters combine with AND; an absent filter is unrestricted; an empty list matches
  nothing. A name that matches nothing is not an error (**Decision**: `not_found` is for
  `step_get` and `unit_get`).
- **Order** is `(position, id)`.
- **`compact`** (default) gives `CompactStep`s, read from the covering indexes without decoding
  any declaration; `compact: false` gives `FullStep`s.
- **`limit`** is 1 to 1000; larger reads as 1000; 0 is `bad_request`. **Decision:** the
  default is 200, as for `log_read`, `query` and `plan_history` (the study's 100 would make the
  tools' shared default machinery special-case one tool).
- **`recipe`** in each step is the recipe its unit matches now, or null. Matching keeps today's
  precedence (the project's recipes, then the home's, each by name; the first whose
  `{unit}-` step ids and fns match), and is cached per `(rev, recipe_generation)`. The first
  read after a recipe or plan change may match every unit (cold); later reads are warm.
- **Reply.** `rev`, `state_epoch` and `recipe_generation` are as the read's snapshot saw them.
  `next_cursor` is set when more steps match, else null.

```json fixture=plan_read.request
{
  "project": "lash",
  "units": [
    "normalize"
  ],
  "status": [
    "pending",
    "failed"
  ],
  "limit": 2
}
```

```json fixture=plan_read.reply
{
  "project": {
    "project_id": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
    "name": "lash"
  },
  "rev": 41,
  "state_epoch": 9120,
  "recipe_generation": "5f0c1e9a7b3d2468",
  "steps": [
    {
      "id": "normalize-fork",
      "unit": "normalize",
      "recipe": "lane",
      "position": 502,
      "run": "git.fork",
      "status": "failed",
      "paused": false,
      "priority": 0
    },
    {
      "id": "normalize-work",
      "unit": "normalize",
      "recipe": "lane",
      "position": 503,
      "run": "agent.run",
      "status": "pending",
      "paused": "after the release",
      "priority": 10
    }
  ],
  "next_cursor": "v1:8a7c2b92c2848cc3:41:9120:-:503:normalize-work"
}
```

The next page passes the cursor back with the same project and filters:

```json fixture=plan_read.next.request
{
  "project": "lash",
  "units": [
    "normalize"
  ],
  "status": [
    "pending",
    "failed"
  ],
  "limit": 2,
  "cursor": "v1:8a7c2b92c2848cc3:41:9120:-:503:normalize-work"
}
```

A full read, of the plan in §7.2:

```json fixture=plan_read.full.request
{
  "project": "demo",
  "steps": [
    "notes"
  ],
  "compact": false
}
```

```json fixture=plan_read.full.reply
{
  "project": {
    "project_id": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7c",
    "name": "demo"
  },
  "rev": 7,
  "state_epoch": 12,
  "recipe_generation": "0e4d1c2b3a596877",
  "steps": [
    {
      "id": "notes",
      "unit": "notes",
      "recipe": null,
      "position": 2,
      "run": "agent.run",
      "status": "pending",
      "paused": false,
      "priority": 0,
      "spec": {
        "run": "agent.run",
        "after": [
          "unit:build"
        ],
        "in": {
          "engine": {
            "default": "claude"
          },
          "cwd": {
            "source": "repo"
          },
          "spec": {
            "source": "gate/items.0"
          }
        }
      },
      "references": [
        {
          "kind": "gate",
          "slot": "after",
          "ordinal": 0,
          "source_kind": "unit",
          "source_id": "build",
          "source_port": "",
          "source_path": ""
        },
        {
          "kind": "binding",
          "slot": "in.cwd",
          "ordinal": 0,
          "source_kind": "input",
          "source_id": "repo",
          "source_port": "",
          "source_path": ""
        },
        {
          "kind": "binding",
          "slot": "in.spec",
          "ordinal": 0,
          "source_kind": "step",
          "source_id": "gate",
          "source_port": "items",
          "source_path": "0"
        }
      ]
    }
  ],
  "next_cursor": null
}
```

**Cursors.** A cursor is opaque to callers. Its text is
`v1:<filter digest>:<rev>:<state epoch or ->:<recipe generation or ->:<position>:<step id>`
(`plan_rows::PlanCursor`). The filter digest (`PlanReadFilter::digest`) is the first 16 hex
digits of the SHA-256 of the compact JSON
`{"project":"<uuid>","units":…,"steps":…,"status":…,"recipe":…}`, each list sorted and
deduplicated, an absent filter null. **Decision:** a cursor always binds `rev`; it binds
`state_epoch` only when the read filters on `status`, and `recipe_generation` only when it
filters on `recipe`. Paging an unfiltered plan therefore survives status changes (a page shows
statuses as of its own read); paging by status or recipe does not.

- A cursor that does not parse: `bad_request` (`cursor is not one plan_read returned`).
- A cursor whose digest is not this request's project and filters: `bad_request` (`cursor
  belongs to another query: …`).
- A bound token that changed: `cursor_expired` (`the plan changed since this cursor was issued;
  read again without cursor`).

```json fixture=cursor
{
  "project_id": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
  "filter": {
    "units": [
      "normalize"
    ],
    "status": [
      "pending",
      "failed"
    ]
  },
  "digest": "8a7c2b92c2848cc3",
  "rev": 41,
  "state_epoch": 9120,
  "recipe_generation": null,
  "position": 503,
  "step": "normalize-work",
  "text": "v1:8a7c2b92c2848cc3:41:9120:-:503:normalize-work"
}
```

### 7.4 `step_get`

`step_get(project, step, compact=false)` → `plan_rows::StepGetResult` `{project, rev,
state_epoch, step}`. A step that does not exist is `not_found` (`no step <id>`). `FullStep` is
the compact fields plus `spec` (the declaration as written) and `references` (§2.5).

```json fixture=step_get.request
{
  "project": "lash",
  "step": "normalize-work"
}
```

```json fixture=step_get.reply
{
  "project": {
    "project_id": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
    "name": "lash"
  },
  "rev": 41,
  "state_epoch": 9120,
  "step": {
    "id": "normalize-work",
    "unit": "normalize",
    "recipe": "lane",
    "position": 503,
    "run": "agent.run",
    "status": "pending",
    "paused": "after the release",
    "priority": 10,
    "spec": {
      "run": "agent.run",
      "tags": [
        "unit:normalize"
      ],
      "after": [
        "normalize-fork"
      ],
      "paused": "after the release",
      "priority": 10,
      "in": {
        "engine": {
          "default": "claude"
        },
        "cwd": {
          "source": "normalize-fork/worktree"
        },
        "spec": {
          "file": "/workspace/notes/lash/normalize.md"
        }
      }
    },
    "references": [
      {
        "kind": "gate",
        "slot": "after",
        "ordinal": 0,
        "source_kind": "step",
        "source_id": "normalize-fork",
        "source_port": "",
        "source_path": ""
      },
      {
        "kind": "binding",
        "slot": "in.cwd",
        "ordinal": 0,
        "source_kind": "step",
        "source_id": "normalize-fork",
        "source_port": "worktree",
        "source_path": ""
      }
    ]
  }
}
```

### 7.5 `unit_get`

`unit_get(project, unit, compact=false)` → `plan_rows::UnitGetResult`. A unit with no steps is
`not_found` (`no unit <name>`). `unit` is `{id, recipe, entry_steps, exit_steps, done, settled,
steps}`: entry and exit steps as SPEC §6.7 derives them, `done` and `settled` as SPEC §6.7
defines them, `steps` in position order.

```json fixture=unit_get.request
{
  "project": "lash",
  "unit": "normalize",
  "compact": true
}
```

```json fixture=unit_get.reply
{
  "project": {
    "project_id": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
    "name": "lash"
  },
  "rev": 41,
  "state_epoch": 9120,
  "recipe_generation": "5f0c1e9a7b3d2468",
  "unit": {
    "id": "normalize",
    "recipe": "lane",
    "entry_steps": [
      "normalize-fork"
    ],
    "exit_steps": [
      "normalize-land"
    ],
    "done": false,
    "settled": true,
    "steps": [
      {
        "id": "normalize-fork",
        "unit": "normalize",
        "recipe": "lane",
        "position": 502,
        "run": "git.fork",
        "status": "failed",
        "paused": false,
        "priority": 0
      },
      {
        "id": "normalize-work",
        "unit": "normalize",
        "recipe": "lane",
        "position": 503,
        "run": "agent.run",
        "status": "pending",
        "paused": "after the release",
        "priority": 10
      },
      {
        "id": "normalize-land",
        "unit": "normalize",
        "recipe": "lane",
        "position": 504,
        "run": "agent.run",
        "status": "pending",
        "paused": false,
        "priority": 0
      }
    ]
  }
}
```

### 7.6 `plan_edit`

`plan_edit(project, ops, rev?, dry_run=false, preview_scope="impact", start=true, reason,
author?)` (`plan_rows::PlanEditRequest`) applies `ops` as one edit.

- `ops` is a closed union (`plan_rows::PlanOp`, below); an unknown `op` or field is
  `bad_request`. An empty `ops` in a `plan_edit` request is `bad_request` (`ops: name at
  least one operation`). That refusal, and the ones for empty lists below, apply to what a
  caller **supplies** (`PlanEditRequest::check_supplied`, `UnitUpdate::check_supplied`), never
  to what a typed tool lowers to (§7.8).
- Operations apply **in order to one candidate**: each sees what the ones before it did (a
  `step.update` can follow the `step.add` of the same step; an `edge.add` on `unit:u` can follow
  the `unit.add` that made `u`). The candidate is then validated once (§6.1) and committed whole
  or not at all, so a source and its readers can be removed together.
- A malformed supplied operation is `bad_request` naming the first one: an empty
  `step.update` `changes` (`ops[0].changes: name at least one field`), `step.remove` `steps`
  (`ops[1].steps: name at least one step`), `edge.add` or `edge.remove` `after` (`ops[2].after:
  name at least one entry`), `unit.update` `changes` (`ops[3].changes: name at least one
  step`) or one member's changes in it (`ops[3].changes.<step>: name at least one field`).
  `order.set` of an empty collection with `ids: []` is valid (it changes nothing).
- A refusal of an operation itself (a missing target, an existing id, a non-member) is
  collected for every operation, and all are returned in one `invalid` with paths
  `ops[<i>].<field>: …`; validation of the candidate runs only when no operation was refused,
  and its errors have plan paths (`steps.<id>.in.<x>: …`).
- `start: false` adds `"paused": true` to every step the edit adds (by net effect, §4) whose
  declaration has no `paused`. That key is part of the logged `step.put`.
- `rev` is required when `ops` holds `order.set` (`bad_request` otherwise).
- The reply's `steps` are the ids the edit's `step.add` and `unit.add` operations added, in
  operation order (a unit's in recipe order); absent when none (**Decision**).
- An edit whose net effect changes nothing commits nothing: the reply has the current `rev` and
  empty `preview.changes`, and no record or history row is written.
- `check_supplied` also refuses `preview_scope: "all"` without `dry_run` and `order.set`
  without `rev`, in that order after the empty checks; the first refusal is returned.

| `op` | Fields | Effect | Refused (path `ops[i]…`) |
|---|---|---|---|
| `input.put` | `name`, `declaration` | Declare the input (appended), or replace its declaration in place. Adds the `inputs` section if absent. | — (validation checks the name, type and current value) |
| `input.remove` | `name` | Remove the input and its value. | no such input |
| `output.put` | `name`, `source` | Declare `{"source": source}` (appended), or replace it in place. Adds `outputs` if absent. | — |
| `output.remove` | `name` | Remove the output. | no such output |
| `step.add` | `step`, `spec` | Add the step (appended) with `spec` as its declaration. | the id exists |
| `step.update` | `step`, `changes` (`StepChanges`) | Each given field replaces that key (an existing key keeps its place in the declaration, a new one is appended); null removes it; `in` replaces the whole map. | no such step |
| `step.remove` | `steps` | Remove each step. | an id that is not a step |
| `edge.add` | `step` (an id or `unit:<u>`: its entry steps), `after` | Append each entry not already in the step's `after`. | no such step or unit; an entry that does not compile |
| `edge.remove` | `step`, `after` | Remove each listed entry; an empty `after` key is removed. | as `edge.add` |
| `unit.add` | `recipe`, `unit`, `params={}`, `after={}`, `inputs={}`, `tags=[]` | Expand the recipe (SPEC §6.8) into the candidate; new steps appended in recipe order. | no recipe; a broken recipe; params; a step id that exists; unknown suffix; reserved tags |
| `unit.update` | `unit`, `changes` (`{step id: StepChanges}`) | `step.update` of each listed member. Never regenerates from the recipe. | no such unit; a key that is not a member |
| `unit.remove` | `unit` | Remove every member. Outcomes are archived as for any removal. | no such unit |
| `order.set` | `collection` (`steps`, `inputs`, `outputs`), `ids` | Rewrite the collection's positions to `0 … n-1` in this order: O(collection). | `ids` not exactly the current members (each missing, unknown or repeated id named) |

`StepChanges` is a closed object of the ten step keys (`run`, `in`, `scatter`, `doc`,
`outputs`, `paused`, `after`, `tags`, `needs`, `priority`). **Decision:** an unknown key
(including the removed `when`) is `bad_request` at decoding, not a validation error, and
`step_update`'s `changes` uses the same type.

Every operation, one each:

```json fixture=plan_op.all
[
  {
    "op": "input.put",
    "name": "repo",
    "declaration": "string"
  },
  {
    "op": "input.remove",
    "name": "old_repo"
  },
  {
    "op": "output.put",
    "name": "notes",
    "source": "notes/final"
  },
  {
    "op": "output.remove",
    "name": "draft"
  },
  {
    "op": "step.add",
    "step": "review",
    "spec": {
      "run": "agent.review",
      "in": {
        "spec": {
          "source": "work/final"
        }
      }
    }
  },
  {
    "op": "step.update",
    "step": "review",
    "changes": {
      "priority": 5,
      "doc": null
    }
  },
  {
    "op": "step.remove",
    "steps": [
      "scratch-a",
      "scratch-b"
    ]
  },
  {
    "op": "edge.add",
    "step": "unit:release",
    "after": [
      "unit:normalize"
    ]
  },
  {
    "op": "edge.remove",
    "step": "release-land",
    "after": [
      "tests-main?"
    ]
  },
  {
    "op": "unit.add",
    "recipe": "lane",
    "unit": "normalize",
    "params": {
      "ticket": "FIG-6001"
    },
    "after": {
      "fork": [
        "main-green"
      ]
    },
    "inputs": {
      "work": {
        "engine": "claude"
      }
    },
    "tags": [
      "wave-3"
    ]
  },
  {
    "op": "unit.update",
    "unit": "normalize",
    "changes": {
      "normalize-work": {
        "priority": 10
      }
    }
  },
  {
    "op": "unit.remove",
    "unit": "legacy"
  },
  {
    "op": "order.set",
    "collection": "inputs",
    "ids": [
      "repo",
      "reviewer"
    ]
  }
]
```

An edit, and its reply:

```json fixture=plan_edit.request
{
  "project": "lash",
  "rev": 41,
  "reason": "Review the work before the release",
  "ops": [
    {
      "op": "step.add",
      "step": "review",
      "spec": {
        "run": "agent.review",
        "after": [
          "work"
        ],
        "in": {
          "spec": {
            "source": "work/final"
          }
        }
      }
    },
    {
      "op": "edge.add",
      "step": "release",
      "after": [
        "review"
      ]
    },
    {
      "op": "input.put",
      "name": "reviewer",
      "declaration": {
        "type": "string",
        "doc": "Who reviews"
      }
    }
  ]
}
```

```json fixture=plan_edit.reply
{
  "project": {
    "project_id": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
    "name": "lash"
  },
  "rev": 42,
  "preview": {
    "scope": "impact",
    "changes": [
      {
        "op": "input.put",
        "name": "reviewer",
        "position": 2,
        "declaration": {
          "type": "string",
          "doc": "Who reviews"
        }
      },
      {
        "op": "step.put",
        "step": "release",
        "position": 12,
        "declaration": {
          "run": "agent.run",
          "tags": [
            "unit:release"
          ],
          "after": [
            "tests-main",
            "review"
          ],
          "in": {
            "engine": {
              "default": "claude"
            },
            "spec": {
              "default": "Cut the release"
            }
          }
        }
      },
      {
        "op": "step.put",
        "step": "review",
        "position": 505,
        "declaration": {
          "run": "agent.review",
          "after": [
            "work"
          ],
          "in": {
            "spec": {
              "source": "work/final"
            }
          }
        }
      }
    ],
    "would_start": [
      "review"
    ],
    "would_queue": [],
    "would_skip": [],
    "would_stale": [],
    "errors": []
  },
  "steps": [
    "review"
  ]
}
```

A full dry run:

```json fixture=plan_edit.dry_run.request
{
  "project": "lash",
  "ops": [
    {
      "op": "step.update",
      "step": "tests-main",
      "changes": {
        "paused": null
      }
    }
  ],
  "dry_run": true,
  "preview_scope": "all",
  "reason": "Would resuming tests-main start anything else?"
}
```

```json fixture=plan_edit.dry_run.reply
{
  "scope": "all",
  "changes": [
    {
      "op": "step.put",
      "step": "tests-main",
      "position": 3,
      "declaration": {
        "run": "agent.run",
        "tags": [
          "unit:tests-main",
          "rolling"
        ],
        "in": {
          "engine": {
            "default": "codex"
          },
          "spec": {
            "default": "Run the main tests"
          }
        }
      }
    }
  ],
  "would_start": [
    "tests-main",
    "normalize-fork"
  ],
  "would_queue": [
    "normalize-fork"
  ],
  "would_skip": [],
  "would_stale": [],
  "errors": []
}
```

### 7.7 `unit_update` and `unit_remove`

`unit_update(project, unit, changes, rev?, dry_run=false, preview_scope="impact", reason,
author?)` is `plan_edit` with one `unit.update`; its reply's `steps` are the members it changed.
`unit_remove(project, unit, rev?, dry_run=false, preview_scope="impact", reason, author?)` is
`plan_edit` with one `unit.remove`; `steps` are the removed members in position order. A
running member refuses `unit_remove` (§4); a surviving reference to a member refuses it through
validation.

```json fixture=unit_update.request
{
  "project": "lash",
  "unit": "normalize",
  "changes": {
    "normalize-work": {
      "priority": 10
    },
    "normalize-land": {
      "paused": "after the release"
    }
  },
  "reason": "Prioritize normalization"
}
```

```json fixture=unit_remove.request
{
  "project": "lash",
  "unit": "legacy",
  "rev": 44,
  "reason": "Superseded by FIG-6010"
}
```

```json fixture=unit_remove.reply
{
  "project": {
    "project_id": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
    "name": "lash"
  },
  "rev": 45,
  "preview": {
    "scope": "impact",
    "changes": [
      {
        "op": "step.delete",
        "step": "legacy-fork"
      },
      {
        "op": "step.delete",
        "step": "legacy-land"
      },
      {
        "op": "step.delete",
        "step": "legacy-work"
      }
    ],
    "would_start": [],
    "would_queue": [],
    "would_skip": [],
    "would_stale": [],
    "errors": []
  },
  "steps": [
    "legacy-fork",
    "legacy-work",
    "legacy-land"
  ]
}
```

### 7.8 The typed edit tools, lowered

Each typed tool resolves its own arguments against the base (selections by `steps`/`tags`
through `step_tags`) and becomes a list of `PlanOp`s for the one pipeline. It keeps its public
arguments, gains `preview_scope` (in `EditOptions`), and keeps its own error kinds for its own
refusals (`not_found` for an unknown step, `bad_request` for an existing one), which the
pipeline reports per operation and the tool maps back.

**Typed no-ops (round 2).** Lowering may produce an **empty normalized list**: an edge already
there, tags or pauses already as asked, a prune whose closure is empty. The pipeline then
prepares nothing, commits nothing and keeps the revision: the reply is the edit result at the
current `rev` with empty `preview.changes`, the tool's `steps` report (`step_pause`'s selection,
`unit_tag`'s unit, `plan_prune`'s `[]` with its `units` and `kept`), and no record or history
row. Lowering never constructs an operation with an empty list or empty changes (no
`step.remove {steps: []}`, no `step.update` with no field, no `edge.add` with no entry), so the
supplied-shape refusals of §7.6 never fire on a typed tool. One typed tool keeps its own
refusal: `step_set_input` whose inputs change no selected step is `bad_request` (`step_set_input
changes nothing`), as today.

| Tool | Operations |
|---|---|
| `step_add` | `step.add {step, spec}` with the tool's `start` |
| `step_update` | `step.update {step, changes}` |
| `step_remove` | `step.remove {steps: selection}` |
| `step_pause` | one `step.update {paused}` per selected step whose pause changes (today's rules for the reason); none when no pause changes |
| `unit_tag` | one `step.update {tags}` per member whose tags change; none when none changes |
| `edge_add`, `edge_remove` | `edge.add` / `edge.remove` with the entries not already there (for `edge_remove`: those there); none when that leaves no entry |
| `unit_add` | `unit.add` with the tool's `start` |
| `step_set_input` | one `step.update {in}` per selected, non-running step that takes every input and whose bindings change; its report is kept (`InputEditResult`); no such step is its own `bad_request` |
| `plan_prune` | `step.remove {steps: the closure}` when the closure is not empty, else nothing; committed only while the store's age evidence holds; its report is kept (`PruneResult`) |
| `plan_set_input` | not a plan edit: a runtime value (moves `state_epoch`, never `rev`) |

```json fixture=plan_prune.reply
{
  "project": {
    "project_id": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
    "name": "lash"
  },
  "rev": 46,
  "preview": {
    "scope": "impact",
    "changes": [
      {
        "op": "step.delete",
        "step": "fig-5570-land"
      },
      {
        "op": "step.delete",
        "step": "fig-5570-work"
      }
    ],
    "would_start": [],
    "would_queue": [],
    "would_skip": [],
    "would_stale": [],
    "errors": []
  },
  "steps": [
    "fig-5570-work",
    "fig-5570-land"
  ],
  "units": [
    "fig-5570"
  ],
  "kept": [
    {
      "unit": "fig-5571",
      "step": "release"
    },
    {
      "unit": "tests-main",
      "keep": "tests-*"
    }
  ]
}
```

```json fixture=step_set_input.reply
{
  "project": {
    "project_id": "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b",
    "name": "lash"
  },
  "rev": 47,
  "preview": {
    "scope": "impact",
    "changes": [
      {
        "op": "step.put",
        "step": "release",
        "position": 12,
        "declaration": {
          "run": "agent.run",
          "tags": [
            "unit:release"
          ],
          "after": [
            "tests-main",
            "review"
          ],
          "in": {
            "engine": {
              "default": "codex"
            },
            "spec": {
              "default": "Cut the release"
            }
          }
        }
      }
    ],
    "would_start": [],
    "would_queue": [],
    "would_skip": [],
    "would_stale": [],
    "errors": []
  },
  "changed": [
    "release"
  ],
  "running": [
    "tests-main"
  ],
  "unsupported": [
    {
      "step": "gate",
      "inputs": [
        "engine"
      ]
    }
  ]
}
```

### 7.9 `plan_view`

`plan_view(project, format="mermaid", all=false, units?, steps?, status?, recipe?)`
(`plan_rows::PlanViewQuery`) draws the selected steps (the filters as `plan_read`'s). Each step
outside the selection that an edge joins to a selected step is drawn as a boundary node
`ext_<id>["<id> · outside"]:::outside` with that edge, and a comment `%% <n> steps outside the
selection drawn as boundary nodes` says so; a partial graph never implies an edge is gone.
`all` keeps done units in the selection; without it they are left out and counted as today.

```json fixture=plan_view.request
{
  "project": "lash",
  "units": [
    "normalize"
  ],
  "format": "mermaid"
}
```

### 7.10 Wire registration

| Tool | `CommandRequest` | Reply |
|---|---|---|
| `plan_get` | `PlanGet { project }` | `CommandReply::Plan(PlanGetResult)` (new) |
| `plan_read` | `PlanRead(PlanRead)` (new) | `CommandReply::PlanRead(PlanReadResult)` (new) |
| `step_get` | `StepGet(StepGet)` (new) | `CommandReply::Step(StepGetResult)` (new) |
| `unit_get` | `UnitGet(UnitGet)` (new) | `CommandReply::Unit(UnitGetResult)` (new) |
| `plan_edit` | `PlanEdit(PlanEditRequest)` (new) | `Edit(EditResult)` / `Preview(EditPreview)` |
| `unit_update` | `UnitUpdate(UnitUpdate)` (new) | `Edit` / `Preview` |
| `unit_remove` | `UnitRemove(UnitRemove)` (new) | `Edit` / `Preview` |
| `plan_view` | `PlanView(PlanViewQuery)` (was a struct variant) | `Data` (text), unchanged |
| `plan_history` | `PlanHistory(PlanHistoryQuery)` (was a struct variant) | `CommandReply::History(PlanHistoryPage)` (new) |
| `plan_patch` | removed with `PlanPatch`, `PatchOperation` | — |

`CommandReply::Edit`, `Preview`, `Inputs` and `Pruned` carry the `plan_rows` types; the
`commands` ones are deleted. `compose::reply_value` returns each new reply's object as is.
Registration touches, together: the CLI `TOOLS` table and `normalize_args`, MCP `DESCRIPTIONS`,
`renames` (none for the new tools) and `default_value` (exempt `reason` for the three tools that
require it), the single-string list fields (`units`, `status`), `drain::check_command` (fence
`PlanEdit`, `UnitUpdate`, `UnitRemove`; drop `PlanPatch`), `served_while_adopting` (add
`PlanRead`, `StepGet`, `UnitGet`), `edit_project` and `project_mutation` (so a run's callback
may edit its own project with the new tools), `edit::edit_label`, and `docs/rust/schemas.json`.

### 7.11 Errors

| Case | Kind | Message (`plan_rows::PlanRowsError`) |
|---|---|---|
| `limit` 0 | `bad_request` | `limit must be 1 to 1000` |
| empty `ops` | `bad_request` | `ops: name at least one operation` |
| a supplied empty `changes`, `steps` or `after` (§7.6) | `bad_request` | `<path>: name at least one field`, `… step`, `… entry` |
| `preview_scope: "all"` without `dry_run` | `bad_request` | `preview_scope "all" needs dry_run: true` |
| `order.set` without `rev` | `bad_request` | `order.set needs rev: the revision whose order it lists` |
| unreadable cursor | `bad_request` | `cursor is not one plan_read returned` |
| another query's cursor | `bad_request` | `cursor belongs to another query: pass the same project and filters, or no cursor` |
| a bound token changed | `cursor_expired` | `the plan changed since this cursor was issued; read again without cursor` |
| `step_get` of a missing step | `not_found` | `no step <id>` |
| `unit_get` of a missing unit | `not_found` | `no unit <name>` |
| explicit stale `rev` | `conflict` | `plan is at rev <n>`, with `current_rev` |
| the third preparation stale too (§4) | `busy`, `retryable: true` | `the plan's state kept changing while this edit was prepared (3 tries); send it again` |
| refused operations or candidate | `invalid` | `invalid plan edit`, `errors` with `ops[i]…` or plan paths |
| a schema-changing restore of a backup holding live work (§10.2) | `invalid` | `this backup holds live work (<a> attempts, <r> runs, <l> leases, <c> calls); a schema-changing restore needs a backup taken with nothing live` |

```json fixture=errors
[
  {
    "case": "limit",
    "error": {
      "error": "bad_request",
      "message": "limit must be 1 to 1000"
    }
  },
  {
    "case": "no_ops",
    "error": {
      "error": "bad_request",
      "message": "ops: name at least one operation"
    }
  },
  {
    "case": "no_changes",
    "error": {
      "error": "bad_request",
      "message": "ops[0].changes: name at least one field"
    }
  },
  {
    "case": "no_steps",
    "error": {
      "error": "bad_request",
      "message": "ops[1].steps: name at least one step"
    }
  },
  {
    "case": "no_entries",
    "error": {
      "error": "bad_request",
      "message": "ops[2].after: name at least one entry"
    }
  },
  {
    "case": "preview_all_needs_dry_run",
    "error": {
      "error": "bad_request",
      "message": "preview_scope \"all\" needs dry_run: true"
    }
  },
  {
    "case": "order_needs_rev",
    "error": {
      "error": "bad_request",
      "message": "order.set needs rev: the revision whose order it lists"
    }
  },
  {
    "case": "cursor_malformed",
    "error": {
      "error": "bad_request",
      "message": "cursor is not one plan_read returned"
    }
  },
  {
    "case": "cursor_mismatch",
    "error": {
      "error": "bad_request",
      "message": "cursor belongs to another query: pass the same project and filters, or no cursor"
    }
  },
  {
    "case": "cursor_expired",
    "error": {
      "error": "cursor_expired",
      "message": "the plan changed since this cursor was issued; read again without cursor"
    }
  },
  {
    "case": "no_step",
    "error": {
      "error": "not_found",
      "message": "no step normalize-wrk"
    }
  },
  {
    "case": "no_unit",
    "error": {
      "error": "not_found",
      "message": "no unit normalise"
    }
  },
  {
    "case": "stale_rev",
    "error": {
      "error": "conflict",
      "message": "plan is at rev 43",
      "current_rev": 43
    }
  },
  {
    "case": "edit_contended",
    "error": {
      "error": "busy",
      "message": "the plan's state kept changing while this edit was prepared (3 tries); send it again",
      "retryable": true
    }
  },
  {
    "case": "invalid_ops",
    "error": {
      "error": "invalid",
      "message": "invalid plan edit",
      "errors": [
        "ops[1].step: no step relase",
        "steps.review.in.spec: work/final is int, which does not fit string: int is not string"
      ]
    }
  },
  {
    "case": "live_work_in_backup",
    "error": {
      "error": "invalid",
      "message": "this backup holds live work (2 attempts, 2 runs, 1 leases, 0 calls); a schema-changing restore needs a backup taken with nothing live",
      "errors": []
    }
  }
]
```

## 8. Store and model APIs

Every store read takes the caller's connection (a read snapshot, or `tx.sql()` in the writer)
and returns `crate::Result`. The row and projection types are in `plan_rows`.

```rust
// sluice-store, plans.rs (lane B)
pub fn plan_revision(sql: &Connection, project: ProjectId) -> Result<Revision>;
pub fn plan_header(sql: &Connection, project: ProjectId) -> Result<PlanHeader>;
pub fn read_plan_rows(sql: &Connection, project: ProjectId) -> Result<PlanRows>;
pub fn read_steps(sql: &Connection, project: ProjectId, selection: &RowSelection,
                  projection: StepProjection) -> Result<StepRows>;
pub fn read_references(sql: &Connection, project: ProjectId,
                       selection: &ReferenceSelection) -> Result<ReferenceRows>;
pub fn read_graph(sql: &Connection, project: ProjectId, selection: &RowSelection)
    -> Result<GraphRows>;
pub fn read_scoped_state(sql: &Connection, project: ProjectId, reads: &PreparationReads)
    -> Result<ScopedState>;                                   // §6.4 (round 2)
pub fn export_plan(sql: &Connection, project: ProjectId) -> Result<ExportedPlan>;
pub fn commit_plan_edit(tx: &mut WriteTransaction<'_>, project: ProjectId,
                        commit: &PlanEditCommit, prune: Option<&PruneEligibility>)
    -> Result<CommitOutcome>;   // Committed(Revision) | Stale
pub fn history(sql: &Connection, project: ProjectId, since_rev: Option<Revision>,
               after_seq: Option<RecordSeq>, limit: u32)
    -> Result<(Vec<HistoryRecord>, Option<RecordSeq>)>;   // (page, next_after_seq)
pub enum CommitOutcome { Committed(Revision), Stale }

// sluice-model (lane C)
pub fn compile_rows(rows: &PlanRows, signatures: &impl SignatureProvider)
    -> Result<Plan, Vec<PathError>>;
pub fn step_index(step: &StepId, declaration: &JsonMap, is_step: &dyn Fn(&str) -> bool)
    -> StepIndexRows;
pub fn output_references(name: &str, binding: &JsonMap) -> Vec<ReferenceRow>;
pub fn preparation_reads(base: &Plan, ops: &[PlanOp], round: &ScopedState)
    -> PreparationReads;                                      // the next round's reads, §6.4
pub fn prepare_plan_edit(base: &EditBase<'_, impl SignatureProvider>, ops: Vec<PlanOp>,
                         options: EditOptions3) -> Result<PreparedPlanEdit, PublicError>;

// sluice-model, pure (lane B, beside the types)
impl PlanRows {
    pub fn to_document(&self) -> JsonMap;                         // export
    pub fn from_document(document: &JsonMap, base: Option<&PlanRows>) -> PlanRows; // conversion
}
```

Rules:

- `read_steps` with `StepProjection::Compact` selects only covering-index columns and never
  reads `declaration`; `Full` reads it. `RowSelection.units` is already resolved (a `recipe`
  filter becomes units in the caller, lane D's naming); `after` is the cursor's keyset;
  `limit` reads one extra row to set `more`.
- `read_graph` returns the selected steps (compact), every `plan_edges` row with a selected
  endpoint, and the other endpoints as `boundary`.
- `read_scoped_state` reads exactly the read set (§6.4): the listed steps' state, the listed
  inputs' values, the project's pause, the listed resources' capacities and live leases, and
  each listed competitor resource's pending steps from `steps_needs`. It never reads a
  declaration and never a step outside the list; it counts `state_rows_read` (§11).
- `export_plan` and `read_plan_rows` are whole-plan reads (counted, §11).
- `compile_rows` never goes through a document: it compiles from rows. **Decision:** the
  compiled type keeps the name `Plan` and its query methods (`inputs`, `outputs`, `steps`,
  `units`, `dependencies`, `topological_order`, `reference_type`) so readers port mechanically;
  it loses `document`, `transport`, `patch` and `parse*`. Its maps are persistent, so a
  candidate built from a base shares structure with it (O(change), never a clone of an
  `IndexMap`).
- `step_index` and `output_references` are pure and manifest-free (§2.5); the store writes what
  they return, `verify` recomputes and compares.
- `EditBase` is the certified compiled base (`Arc<Plan>` with the `catalog_generation` it was
  compiled against), its tokens, the `ScopedState` of the read set (§6.4, all rounds), the
  signature provider for the current catalog (it answers for every fn the project can see, so
  a step the edit adds may use a fn the base never used), the recipe catalog, cached capacities
  and resource limits; `EditOptions3` is `rev`, `dry_run`, `preview_scope`, `start`, `reason`,
  `author` (lane C names them finally; the fields are pinned here).
- `PlanRows::from_document` assigns positions by §10.6's rule relative to `base`; with no base,
  `0 … n-1`.

### 8.1 The compiled-plan handoff (round 2)

`RowDelta` holds declarations and index rows, not compiled signatures, and a step the edit adds
can use a fn the base plan never used, so the cache cannot be moved forward from the store's
payload. **Decision:** preparation produces the compiled candidate itself.

- `prepare_plan_edit` returns `PreparedPlanEdit { commit: PlanEditCommit, dry_run, preview,
  steps, board_warnings, inputs, compiled: CertifiedPlan }`. `PlanEditCommit` is all the store gets:
  `tokens`, `rev`, `author`, `reason`, `rows: RowDelta`, `state: StateDelta`, `prune`. The
  store's input stays rows and state; it never sees a compiled plan.
- `CertifiedPlan` is `{plan: Arc<Plan>, base_rev, catalog_generation}`: the candidate compiled
  (incrementally, sharing structure with the base) against the signature provider of
  `catalog_generation`, and validated by §6.1. It describes the plan at `base_rev + 1` when
  `commit.rows.changes` is not empty, else it is the base itself.
- The runtime's plan cache is `(project) → (rev, catalog_generation, Arc<Plan>)`, built cold by
  `compile_rows`. After `CommitOutcome::Committed(rev)` the coordinator installs `compiled.plan`
  as `(rev, compiled.catalog_generation)` **only if** the cache still holds `(base_rev, the same
  catalog_generation)` for the project; otherwise (another edit was installed first, or the
  catalog was republished) it drops the candidate and the next reader compiles cold. After
  `Stale`, a `conflict`, any error, or a dry run, the candidate is dropped.
- A catalog publication that changes a signature some step uses invalidates the entry (§6.2).

The runtime (lane D) owns the plan cache, `recipe_generation(home, project) ->
RecipeGeneration`, and naming keyed by `(rev, recipe_generation)`.

## 9. Readers to migrate

Every reader of `plans.doc`, of `plan_edits.ops`, of a whole compiled document, of the document
parser or of `FrozenPlan`, at `775d57c` (the shipped dashboard redesign; round 2 re-checked the
dashboard rows against it), and the lane that moves it. A lane may not leave one behind: lane H
greps for `plans.doc`, `doc FROM plans`, `.document()`, `transport()`, `Plan::parse`,
`PatchOperation`, `plan_patch` and `FrozenPlan` outside the converter, lane H's reference crate
and fixtures.

| File: functions | Reads | Replacement | Lane |
|---|---|---|---|
| `sluice-store/src/plans.rs`: `initialize_plan`, `check_context`, `prune_eligible`, `apply_edit`, `edit_effect`, `commit_effect`, `Current`, `EditEffect`, `wire_step`, `projection`, `write_projection`, `sync_projection`, `Witness`, `history` | `plans.doc`, whole document | Rows (§8), `commit_plan_edit`, revision and token checks, history paging (§5.4) | B |
| `plans.rs`: `reconcile`, `set_input`, `step_set_output`, `settle_inline`, `fail_unlaunched`, `step_retry`, `step_cancel` (all through `check_context`) | whole-document equality | `plans.rev` equality with the compiled base | B |
| `sluice-store/src/projects.rs`: `EmptyPlanInitializer` | writes `plans.doc`, `plan_edits.ops` | header `["inputs","outputs","steps"]`, rev-1 `header.put` | B |
| `projects.rs`: `last_retirement` | `json_array_length(plan_edits.ops)` | the count of `step.delete` changes | B |
| `projects.rs`: `project_delete` | table list | §2.2's order with the new tables | B |
| `sluice-store/src/schema.rs`, `migrations/`, `records.rs`, `backup.rs` (`restore_into_fresh_home`), `query.rs` (view docs) | schema 1 | §2, §10 | B |
| `sluice-store/src/resources.rs`: `admit_order` | `plans.doc` equality | `plans.rev` equality | D |
| `sluice-store/src/attempts.rs`: `reserve` (`wire_step`), `CompletionContext`, `complete_frozen` | compiled document | the step's stored declaration; completion from the attempt's frozen contract and identity | D |
| `sluice-runtime/src/coordinator.rs`: `context`, `PlanCache`, `CachedPlan`, `OutsideEdit`, `commit_outside`, `board_drops`, `callback_mutation`, `mutate_project`, the completion path (`FrozenPlan` decode), `EditLog` (`preview.ops.len()`) | `plans.doc`, document, snapshot | cold `compile_rows`, the certified candidate installed after commit (§8.1), tokens, the read set (§6.4), no snapshot, `changes.len()` | D |
| `coordinator.rs`: `PlanGet` dispatch | compiles | `export_plan`, no compile | E |
| `coordinator.rs`: `served_while_adopting`, `edit_project`, `project_mutation`, `callback` allowlist | command lists | §7.10 | E |
| `sluice-runtime/src/execution.rs`: `FrozenPlan`, `FrozenSignature`, `FrozenDeclaration`; `scheduler.rs` admission provenance | stores the document in attempts | deleted; provenance keeps `capability`, `execution`, `files`; `rev` as scalar `admitted_rev` | D |
| `sluice-runtime/src/watch.rs`: `record_settlements` | `plans.doc` equality | `plans.rev` equality; affected units only | D |
| `sluice-runtime/src/verify.rs`: `verify`, `check_project` | `plans.doc` | `read_plan_rows` + `compile_rows`; rebuild and compare the indexes | D |
| `sluice-runtime/src/naming.rs`: `for_project` | `plans.doc` steps | compact rows (`id`, `run`, unit) for matching, declarations only of matched units' steps for params and titles; keyed by `(rev, recipe_generation)` | D |
| `sluice-runtime/src/dispatch_ext.rs`: `edit_extension`, the input preview (`EditPreview` built at the `PlanSetInput` dry run), `step_context` | compiled document | the pipeline; selected step, its sources and inputs, frozen attempt data | D |
| `dispatch_ext.rs`: `render_plan_view`, `PlanHistory` dispatch | compiled plan, unpaged history | `read_graph` with filters and boundary nodes; history paging (§5.4) | E |
| `sluice/src/me.rs`: `plan_at`, `context` | `plans.doc` | `step_context`'s row-based context; run-specific frozen inputs kept | D |
| `sluice/src/cli.rs`: `TOOLS`, `normalize_args`, `fill` (author) | `plan_patch` | §7.10 | E |
| `sluice-web/src/mcp.rs` | `plan_patch` | §7.10 | E |
| `sluice-web/src/views/board.rs`: `PlanCache::compile`, `load_board`, `load_step`, `plan_mermaid` | `plans.doc`, compiles | revision- and catalog-keyed compiled cache from rows; `read_graph` for the mermaid | F |
| `sluice-web/src/views/panel.rs`: `load`, `gather` | `plans.doc` (`gather`'s `plan_edits.seq` check stays) | rows of the steps, tags and units the board program names | F |
| `sluice-web/src/settings.rs`: `SettingsState::snapshot` | `plans.doc` | resource-needing steps' compact rows and `needs` | F |
| `sluice-web/src/views/log.rs`: `load` (the retire fold's `ops.len()`), `summary` | `Event::PlanEdit.ops` | `changes` (the fold counts `step.delete`) | F |
| `sluice-web/src/views/step.rs`: `load_detail` ("who paused it") | `plan.edit` payloads searched for the pointer `"/steps/<id>/paused"` | the latest actual pause transition in the step's current incarnation (§9.1) | F |
| `sluice-web/src/views/gallery.rs`: `step_band_part` (added by the redesign) | `Plan::parse_json` of an inline document | `compile_rows` of hand-built `PlanRows` (or lane C's test constructor) | F |
| `sluice-web/src/views.rs` (`plans: PlanCache`), `examples/dashboard_fixture/` | compiled cache; `initialize_plan`, `DELETE FROM plans` | rows; fixtures build through `project_create` and edits | F |
| `sluice-model/src/plan.rs`: `Plan.document`, `PlanDocument::compile`, `Plan::parse*`, `patch`, `ancestor_orders`, `restore_order`, `PlanPatchData`, `prepare_patch`, `apply_edit` | document | `compile_rows`, the candidate of §8.1; the RFC 6902 replay moves into the converter only, and the whole compiler into lane H's test-only reference crate first (§12) | C (B for the converter's copy, H for the reference) |
| `sluice-model/src/edit.rs`: `PlanEdit`, `PreparedEdit`, `prepare_edit`, `EditSnapshot` | document | `prepare_plan_edit`, `PreparedPlanEdit` | C |
| `sluice-model/src/recipe.rs`: `Recipe::expand` (`document` for collisions and staging) | document | the candidate's ids and an incremental compile of the expanded unit | C |
| `sluice-model/src/gates.rs`: `simulate_edit` | both whole plans | impact simulation; the whole one only for `preview_scope: "all"` | C |
| `sluice-model/src/naming.rs`: `name_plan` (a steps JSON map) | document steps | rows | D |
| `sluice-model/src/commands.rs`: `PlanPatch`, `PatchOperation`, `EditPreview`, `EditResult`, `InputEditResult`, `PruneResult`; `events.rs`: `Event::PlanEdit.ops` | — | `plan_rows` types | E (commands), B (events) |
| `docs/agent/{board,plans,instructions,examples}.md`, `docs/rust/schemas.json` | `plan_patch` | the new tools and onboarding (§12, lane E) | E |
| Readers of `plans.rev` alone (`projects::list`, `naming` key, `panel::gather`, `views/step.rs::load_detail`, `dispatch_ext` revision checks, `coordinator` revision checks) | `plans.rev` | unchanged | — |

Tests that build plans through `initialize_plan`, `DELETE FROM plans` or `plan_patch`
(`sluice-store/tests/{commands,completion,results,lifetimes,review_p2,leases,messages,records,query}.rs`,
`sluice-runtime/tests/{coordinator,next,verify,python_helper,edit_replies,retire,adopt_fast,dispatch_ext,scheduler,plan_scale,agent_models}.rs`,
`sluice/tests/{compose,me,cli,release,g1a,install,tool_descriptions}.rs` and
`acceptance/{fn_launched,engines}.rs`, `sluice-web/tests/{page_load,project_board,step,mcp}.rs`,
`board_fixture/`, `neutral/`, `sluice-model/tests/{review_regressions,plan_semantics,units_order}.rs`)
move with the lane that owns the
code they exercise; each is ported to the behaviour it meant, never by rewriting SQL strings
mechanically.

### 9.1 Who paused a step (round 2)

Every `step.put` carries the whole declaration, so a later tag or doc edit of a paused step
also holds `paused`; "the latest put with a `paused` key" would credit that edit. The step page
attributes the pause to the latest **actual transition**: walking the project's `plan_edits`
newest first and, within each row, the changes for this step (`json_each` over `changes`,
`op = 'step.put' AND step = <id>`, or `op = 'step.delete' AND step = <id>`):

1. A `step.delete` of the step ends the walk: what came before belongs to an earlier
   incarnation of the id and is never credited.
2. Compare each `step.put`'s `paused` (absent reads as not paused) with the `paused` of the
   step's next older put in the same incarnation (none, when the walk reaches a delete or rev 1:
   the step was added, and adding it paused is a transition).
3. The first put, newest first, whose `paused` differs (paused from not, not from paused, or a
   reason changed to another reason or to `true`) is the edit credited: its `author` and `at`.

The step's current declaration must be paused for the line to show at all (today's `hold`).
The walk stops at the first transition, so a step paused long ago and edited often reads back
through its own puts only; lane F tests a pause, then a tag edit, then a doc edit (the pause's
author stays), a reason change (the new author), and a remove and re-add (only the new
incarnation counts).

## 10. Migration contract

### 10.1 The cutover

The cutover never waits forever: it has a deadline (owner ruling). The deploy tooling (lane G)
runs it as `scripts/deploy --schema-cutover --deadline <RFC 3339 time or +<n>m>
[--cancel-grace <seconds, default 300>] [--settle-timeout <seconds, default 300>] [--dry-run]
[REF]`.

1. **Rehearse** (before the notice): build and gate the candidate, then run the rehearsal
   helper (§10.10) on a private copy of the home. On the copy, the **old** release, with a test
   adoption host that cannot reach production services, plays steps 3 to 7 (the drain, the
   cancels, the lost-process settlement); only once the copy has zero blockers does the
   candidate convert it and `scripts/compat-check --incompatible --copy` run against it. A
   failure stops here; nothing live changed.
2. **Notice.** The orchestrator tells every Claude session using sluice the cutover deadline.
   The tooling prints the text to send: `sluice cutover to schema 3 at <deadline>: new work is
   refused from now; runs still live at <deadline> get a cancel request ("<reason>") — check
   how each ended in the cutover report and retry it after the cutover.`
3. **Drain**, from the notice: `sluice drain --author cutover` under the old release. It pauses
   every project not paused, records them, and refuses new plan work and user calls (SPEC §2.6).
4. **Wait** until `drain` status has no blockers or the deadline comes, whichever is first.
5. **Fence**, then cancel (round 2: the review settled the order; the cancel-first fallback is
   gone). `install fence "schema-3 cutover"` at that moment, before any cancel. The fence
   refuses a non-maintenance coordinator start and every new payload launch
   (`Installation::admission_guard`, `payload_guard`), while the running old coordinator keeps
   serving commands: `step_cancel` is not among the commands the drain refuses
   (`drain::check_command`) and commits in an ordinary writer transaction. If the old
   coordinator is not running, the tooling starts it with `--maintenance` first.
6. **Request the cancels and verify them.** From a read-only snapshot, list the live step
   attempts (`phase <> 'terminal'`, a `step_id`) and group them by distinct (project, step).
   For each, **one** `step_cancel` (old release) with author `cutover` and reason `R` =
   `schema-3 cutover at <deadline>: stopped at the deadline; retry it after the cutover`. It
   writes the existing records (`step.cancel {step, author: "cutover", reason: R}`) and sets
   `cancel_requested` on the step's nonterminal attempts. Then re-read and **verify the
   intent**: every attempt of each cancelled step that is still nonterminal has
   `cancel_requested = 1`.
   - **A refused cancel stops the cutover still fenced.** The old dispatch compiles the whole
     plan against the current catalog before it cancels (`coordinator::context`), so a missing
     or changed fn refuses it (`invalid`, `invalid stored plan`) before any intent is stored;
     `step_cancel` also refuses a step that is no longer running (`step <id> must be running or
     pending external work`). Before counting a refusal, the tooling re-reads: a step whose
     attempts all became terminal meanwhile is not one. On a refusal, or an attempt left
     without intent, nothing is stopped: the drain and the fence stay, and the report's
     `refused` (`plan_rows::CancelRefusal`, fixture `cutover.refused`) names each affected
     project, step, run and attempt with the refusal's kind and message. The operator removes
     the cause (restores the fn, or ends the step by hand) and runs the cutover again, or
     unfences and releases. **Decision:** no predecessor release removes the cancel's compile
     dependency; a refusal needs catalog drift on a drained home, and stopping fenced is safe.
   - A unit is stopped only for an attempt whose cancel intent was verified, or for a direct
     call (a call has no cancel).
7. **Settle, then the bounded zero-blocker check.** Wait up to `--cancel-grace` for the
   cancelled runs to end. Then stop the transient unit of every run still live (step runs with
   verified intent, and direct calls); the old coordinator records each end through its
   existing completion and adoption paths. Then re-read the drain blockers (attempts not
   terminal, runs not finished, leases waiting or held, calls running) until none remain, for at
   most `--settle-timeout`; if any remain, stop fenced and report them. A cancel
   acknowledgement never proves the bookkeeping finished: only zero blockers lets the cutover
   stop services or convert. No new record kind.
8. **Stop** the coordinator, serve and loop units and verify that no process of the home is
   left (guardians, their children, database readers): by unit and cgroup, never by `pgrep -f`.
9. **Back up** the database (backup API) to `<install>/backups/pre-schema3-<timestamp>.db`.
10. **Migrate**: the candidate's `sluice home migrate` (§10.2) under the fence, holding the
    home's writer lock. Its preconditions (§10.3) are checked again inside its transaction.
11. **Select and start** the candidate (`coordinator --maintenance`, serve, loop) and verify:
    integrity, `plan_get` of every project equals the backup's `plans.doc` (§10.5),
    `plan_history` reaches rev 1, the dashboard answers.
12. **Unfence, release.** `install unfence`, then `release` with author `cutover`, which
    unpauses exactly the drain's recorded projects.
13. **Report** one line, `cutover <sha> · schema 3 · <n> projects · <m> revisions converted ·
    cancel requested for <k> runs · <c> calls stopped`, then one line per affected run,
    `stopped <project> <step or call:<id>> <run> requested=<cancel|stop>
    outcome=<succeeded|failed:<error kind>> advice=<none|retry|read-then-retry|call-again>`,
    appended to `<install>/deploy.log`; the whole `plan_rows::CutoverReport` (fixture
    `cutover.report`) is written to `<install>/cutover-<timestamp>.json` so the orchestrators
    can retry.

**What a cancel request ends in** (round 2; replaces decision 26's promise that every stopped
step ends `failed` with `cancelled`). The cutover records cancellation **intent**; it does not
decide outcomes, and existing completion precedence holds (`attempts::complete_frozen`):

- a run whose settle intent was recorded (`step_settle` with a submission) completes
  `succeeded` with the settled outputs, cancel or not (`cancelled && settle.is_none()`);
- a run that finishes by itself before its unit is stopped records its own result;
- a cancelled run otherwise records `failed` with `cancelled` (`cancel requested`); a stopped
  call records `failed` with `process_lost`;
- a scatter step's status is decided over its instances, and its error is the first failed
  instance's, which may be a failure from before the cancel.

So the report lists each affected run's **actual terminal outcome** beside the request, read
after step 7: the run's own completion (`outcome`) and its step's resulting status and error
(`step_status`, `step_error`). Advice follows the outcome, never the request: a succeeded run
(or a step that succeeded as a whole) → `none`; a step failed with `cancelled` → `retry`; a step
failed with any other error (a scatter instance's earlier failure, a run's own failure, a
step's `process_lost`) → `read-then-retry`, because the error is the step's own and may come
back; a call → `call-again`. The report never restricts outcomes to `cancelled` or
`process_lost`.

```json fixture=cutover.report
{
  "sha": "4f1c2d3e",
  "schema": 3,
  "deadline": "2026-10-12T18:00:00Z",
  "reason": "schema-3 cutover at 2026-10-12T18:00:00Z: stopped at the deadline; retry it after the cutover",
  "projects": 6,
  "revisions": 1874,
  "stopped": [
    {
      "project": "lash",
      "step": "normalize-work",
      "run": "0199c3b0-1a2b-7c3d-8e4f-5a6b7c8d9e0f",
      "requested": "cancel",
      "outcome": {
        "status": "failed",
        "error": "cancelled"
      },
      "step_status": "failed",
      "step_error": "cancelled",
      "advice": "retry"
    },
    {
      "project": "lash",
      "step": "review-work",
      "run": "0199c3b0-2b3c-7d4e-8f50-6b7c8d9e0f1a",
      "requested": "cancel",
      "outcome": {
        "status": "succeeded"
      },
      "step_status": "succeeded",
      "advice": "none"
    },
    {
      "project": "lash",
      "step": "tests-scatter",
      "run": "0199c3b0-3c4d-7e5f-8061-7c8d9e0f1a2b",
      "requested": "cancel",
      "outcome": {
        "status": "failed",
        "error": "cancelled"
      },
      "step_status": "failed",
      "step_error": "fn_failure",
      "advice": "read-then-retry"
    },
    {
      "project": null,
      "call": "0199c3b0-4d5e-7f60-8172-8d9e0f1a2b3c",
      "run": "0199c3b0-4d5e-7f60-8172-8d9e0f1a2b3d",
      "requested": "stop",
      "outcome": {
        "status": "failed",
        "error": "process_lost"
      },
      "advice": "call-again"
    }
  ],
  "refused": []
}
```

A cutover stopped by a refused cancel writes the same report with `stopped` empty and
`refused` filled, and says `cutover stopped fenced: <k> cancels refused` on its one line:

```json fixture=cutover.refused
{
  "project": "lash",
  "step": "normalize-work",
  "runs": [
    "0199c3b0-1a2b-7c3d-8e4f-5a6b7c8d9e0f"
  ],
  "attempts": [
    "0199c3b0-1a2b-7c3d-8e4f-5a6b7c8d9e10"
  ],
  "error": {
    "error": "invalid",
    "message": "invalid stored plan",
    "errors": [
      "steps.normalize-work.run: unknown fn agent.lane"
    ]
  }
}
```

`--dry-run` runs step 1 and prints the notice and what steps 6 and 7 would do now (each live
run's project, step or call, run id, start time, pinned release, and whether its step has
settle intent); it changes nothing. Neither `--skip-compat` nor an earlier rehearsal skips step
6's verification, step 7's zero-blocker check or the converter's own preconditions.

**Lane G** pins these details: `--candidate DIR` takes the release `cutover-rehearse` checked
instead of building REF; `--rehearsal FILE` takes that helper's passing `--json` report for the
same candidate and selected release instead of rehearsing again (step 1 still happens, earlier);
`--skip-compat` is refused with `--schema-cutover`. The old release drains with `SLUICE_AUTHOR=cutover
sluice drain --no-wait` (its `drain` has no `--author`). In step 6 a step whose live attempts all
carry `cancel_requested` already (a settle sets it) is not a refusal even when its cancel is
refused. The report is written once step 7 ends (and again after the migration), so a failure
in steps 8 to 12 still leaves each run's outcome; the deploy.log lines are JSON lines whose
`cutover` field holds the line. Step 8 proves the home empty by its units (the automatic
coordinator's, and any active `sluice-run-`/`sluice-test-` unit of a run in its database) and by
the processes holding its database or `coordinator.lock` open, never by a command-line pattern.

### 10.2 The converter

`sluice_store::convert::convert_home(database) -> Result<ConversionReport>` (lane B), with one
set of preconditions wherever it runs (round 2; replaces decision 22's `Restore` mode).
`sluice home migrate [--dry-run] [--json]` (lane G) runs it on `SLUICE_HOME`'s database while
holding `coordinator.lock`; `--dry-run` converts a backup-API copy in a scratch directory and
prints the report without touching the home. `restore_into_fresh_home` runs it on its private
destination when the backup is at schema 1 or 2.

**A schema-changing restore refuses a backup holding live work.** A restore keeps every
identity: run and attempt ids, and each run's recorded unit name and cgroup. Adoption in the
restored home looks those up (`Coordinator::adopt_until` reads `runs.unit_name` and
`runs.cgroup` of every nonterminal attempt; `OsAdoptionHost::reconcile` queries that unit in
the same user service manager and can stop its cgroup), so the restored home's coordinator would
reach, and could stop, the **original** home's live units: another directory is not isolation.
So conversion's zero-blocker precondition (§10.3) holds for a restore too, and such a backup is
refused before anything is written: `StoreError::LiveWorkInBackup`, public `invalid`, message
`this backup holds live work (<a> attempts, <r> runs, <l> leases, <c> calls); a schema-changing
restore needs a backup taken with nothing live`. Restoring one would need a separately specified
offline recovery path that never contacts the original units and resolves the copied work before
any adoption starts; it is not part of this cutover. A same-schema restore (a schema-3 backup
into a schema-3 binary) converts nothing and is unchanged.

- **Inputs:** the database file at schema 1 or 2. Nothing else: no fn manifest, recipe file or
  `.env` is read, because historical conversion never reinterprets old revisions with today's
  registry.
- **Output:** the same file at schema 3, converted in **one transaction** (any failure leaves it
  untouched), then `PRAGMA integrity_check` and `foreign_key_check`.
- **Report** (JSON with `--json`): `{from_schema, projects: [{project_id, name, revisions,
  steps, inputs, outputs, records_rewritten, attempt_snapshots_removed, anchor}], anchored:
  [{project_id, name, rev, folded_edits, source}], warnings: [...]}`. `anchor` is
  `{rev, folded_edits, source}` for a project anchored by §10.4.1 (`source` `snapshot` or
  `current`), else null; `anchored` lists those projects, and the cutover report (lane G)
  lists each one's anchor rev and folded edits.

### 10.3 Preconditions (every conversion, checked inside the transaction)

- `home_meta`: format 1, schema 1 or 2, `user_version` equal, `application_id` sluice's, 23
  tables, integrity and foreign keys clean.
- **Zero blockers:** no attempt with `phase <> 'terminal'`, no run with `finished_at IS NULL`,
  no lease `waiting` or `held`, no call `running`. Pending calls are not blockers (they have no
  process and run after the release). A blocker is never deleted or rewritten to pass: only the
  old release's own cancel, completion and adoption paths end live work (§10.1, §10.10).
- Every per-project check of §10.4 passes. A blocker names the project, the check and the rev.

### 10.4 Per project (every `plans` row)

1. History is complete: `plan_edits` revs are exactly `1 … plans.rev`; rev 1 has `ops` `[]`
   (its origin is `EmptyPlanInitializer`'s `{"steps":{}}`). A gap or another rev-1 origin is a
   blocker (**unknown origin**): never collapsed into a new baseline. The one exception is a
   logged history that starts above rev 1, contiguous from there to `plans.rev`: §10.4.1
   anchors it, or it is a blocker.
2. Replay: from `{"steps":{}}` (or the anchor's document from rev `k + 1`), apply each
   revision's RFC 6902 `ops` with `Plan::patch`'s
   exact semantics (`json_patch` one operation at a time, restoring the key order of every map
   above each touched path), **without** validation. A patch that does not apply is a blocker.
3. At each revision, build its rows from the replayed document (`PlanRows::from_document` with
   the previous revision's rows as base, §10.6), take the changes from the previous rows, and
   check that the rows export back to that document (§10.5). A mismatch is a blocker.
4. The final replayed document must equal `plans.doc` (§10.5); so must the final rows' export.
5. Every `steps` row's `declaration` must equal (§10.5) its document entry, and the step ids,
   input names and their sets must match the document's. A mismatch is a blocker.
6. Write: `plans` (`rev`, `root_order`, `state_epoch` 0); `inputs.declaration` exact and
   `position` from the final rows; `steps.position` from the final rows (moving rows above the
   maximum first); `plan_outputs`; `steps.unit`, `step_tags`, `plan_refs`, `plan_edges` from
   `step_index` and edge derivation; `plan_edits.changes` per revision (keeping `rev`, `seq`,
   `at`, `author`, `reason`).
7. Records: each retained `plan.edit` record's payload becomes `{kind, rev, author, reason,
   changes}` with its revision's changes (a record whose rev has no `plan_edits` row is a
   blocker; an author or reason differing from the row is a warning).
8. Attempts: for each attempt with a completion snapshot, compare `request.declaration` with
   the snapshot's `document.steps[step]` (a difference is a **warning**, not a blocker: the
   attempt is terminal and its result recorded), then remove `completion` from
   `request.provenance.runtime` and from `provenance.runtime`. Every other field stays.

Then, home-wide: every record's `payload_version` becomes 2; `plans.doc`, `plan_edits.ops`,
`projects.board_slots` and the `board_slots` view are dropped; the tables, indexes, triggers and
views become §2.2's (schema-equivalent to a fresh home); `home_meta.schema_version` and
`user_version` become 3. IDs, generations, work generations, input values, outcomes, attempts,
runs, results and their links are preserved.

### 10.4.1 Imported projects: the anchored baseline (round 3, decided)

Projects the Python home's importer created (`initialize_plan` at rev 1, no `plan_edits` row)
have a logged history that starts at rev 2: the imported document was never logged. On the
live home these are lash, figments and sluice. The Python home is deleted by owner ruling, so
nothing else can supply their origin; attempts' frozen completion snapshots
(`provenance.runtime.completion.{revision, document}`, also under `request.provenance`) can.

- **Anchor.** A project whose logged history starts at rev `f > 1` (contiguous from `f` to
  `N = plans.rev`) is anchored at the **earliest** revision `k`, `f − 1 ≤ k ≤ N`, for which an
  attempt's completion snapshot of revision `k`, replayed through the logged revisions
  `k + 1 … N` (step 2's semantics), equals the stored plan under §10.5. Candidates are tried by
  `k` ascending, and each revision's distinct documents in the order their attempts were
  created; a patch that does not apply or a final document that differs rules a candidate out.
  The snapshots are read in step 1, before step 8 removes them.
- **No snapshot.** With none that reproduces the stored plan and exactly one logged revision
  (`f = N`), the anchor is `k = N` with the stored plan as its document. Otherwise the project
  is a blocker naming it: `history starts at rev <f>: unknown origin (no completion snapshot
  replays through revs <f> to <N> to the stored plan, and more than one revision is logged)`.
  An empty logged history stays a blocker.
- **Baseline.** Revision `k`'s `plan_edits` row becomes the baseline: author `sluice`, reason
  `imported baseline: history before rev <k> was not logged`, `changes` the anchor document's
  rows from nothing (`header.put` and every put). It keeps rev `k`'s own `seq` and `at` when
  rev `k` is logged; below the first logged revision it takes the highest record sequence under
  that revision's that no record of the project holds (history orders and pages by it) and the
  project's `created_at`, and no free sequence is a blocker. Revisions after `k` convert by
  steps 2 to 5 from the anchor's rows; steps 4 and 5 hold unchanged.
- **Folded edits.** Logged revisions at or below `k` are **folded** into the baseline: their
  `plan_edits` rows are not kept (rev `k`'s is the baseline). Each such revision's retained
  `plan.edit` record is kept with its author and reason: rev `k`'s carries the baseline's
  changes, an earlier one `changes: []`; each is a warning (`plan.edit record <seq> at rev <r>
  is folded into the imported baseline at rev <k>`), not a blocker.
- **Report.** `folded_edits` counts the logged revisions at or below `k`. On a backup copy of
  the live home (2026-10-10) the converter anchors lash at rev 1 from a snapshot (0 folded:
  its whole history is recovered), figments at rev 2 from a snapshot (1 folded: rev 2's own
  edit) and sluice at rev 2 from the stored plan (1 folded).

### 10.5 Round-trip equality

Two plan documents are equal when their **compact serializations are byte-equal**, both parsed
as `JsonMap` (insertion-ordered) and written with `serde_json::to_string`. That is equal JSON
content, the same declaration spelling, omitted-versus-empty sections preserved, and the same
order of root sections, inputs, outputs, steps and every key inside a declaration.
Whitespace in the stored text does not count. **Decision:** stricter than the study's
"meaningful ordering", because rows keep every order exactly, so nothing less is needed.

### 10.6 Positions in replayed history

Legacy documents order collections by object order, and legacy patching keeps a replacement
subtree in the order it was supplied (`Plan::patch` restores only the maps above each touched
path), so replacing `{"a": …}` with `{"b": …, "a": …}` puts a new key before a surviving one
without moving any survivor. **Decision** (round 2; replaces decision 25's survivor-order
test): for each collection (`inputs`, `outputs`, `steps`) at each revision, given the previous
revision's rows and the replayed document's keys in order:

1. **Assign tentatively** by schema 3's own rules: a key that stays keeps its position; each new
   key, in document order, is appended at `max + 1` of the previous rows (0 when there were
   none), then the next; a removed key leaves a gap.
2. **Compare the complete key sequence** of the tentative rows, ordered by position, with the
   document's key sequence.
3. If they are equal, keep the tentative positions: the revision's changes are a put for each
   new key and each key whose declaration changed, and a delete for each removed key.
4. If they differ (a key inserted before or between survivors, a `move`, a section replaced
   whole in another order), **renumber** the collection `0 … n-1` in the document's order: a
   put for each new key, each changed key, and each key whose position differs from its
   previous one; a delete for each removed key.

`root_order` follows the same comparison: a revision whose present sections, in order, differ
from the previous revision's has a `header.put`. Rev 1 of a converted project is `[{"op":
"header.put", "root_order": ["steps"]}]`. Final positions may differ from schema 1's dense
`steps.position`; only the order is preserved, and §10.4 step 3 checks it at every revision.

The rule is pinned by `replay.positions`, which `tests/plan_rows.rs` runs through a reference
implementation of steps 1 to 4 kept in that test; lane B's `PlanRows::from_document` must give
the same `positions`, `puts` and `deletes` (keys only; a put for a changed declaration comes on
top). Each case's `before` is the previous rows' positions, `keys` the replayed document's
order:

```json fixture=replay.positions
[
  {
    "case": "append",
    "before": {"a": 0, "b": 1},
    "keys": ["a", "b", "c"],
    "positions": {"a": 0, "b": 1, "c": 2},
    "renumbered": false,
    "puts": ["c"],
    "deletes": []
  },
  {
    "case": "remove_leaves_gap",
    "before": {"a": 0, "b": 1, "c": 2},
    "keys": ["a", "c"],
    "positions": {"a": 0, "c": 2},
    "renumbered": false,
    "puts": [],
    "deletes": ["b"]
  },
  {
    "case": "append_after_gap",
    "before": {"a": 0, "c": 2},
    "keys": ["a", "c", "d"],
    "positions": {"a": 0, "c": 2, "d": 3},
    "renumbered": false,
    "puts": ["d"],
    "deletes": []
  },
  {
    "case": "insert_before_survivor",
    "before": {"a": 0},
    "keys": ["b", "a"],
    "positions": {"b": 0, "a": 1},
    "renumbered": true,
    "puts": ["b", "a"],
    "deletes": []
  },
  {
    "case": "insert_between_survivors",
    "before": {"a": 0, "c": 1},
    "keys": ["a", "b", "c"],
    "positions": {"a": 0, "b": 1, "c": 2},
    "renumbered": true,
    "puts": ["b", "c"],
    "deletes": []
  },
  {
    "case": "insert_into_gap",
    "before": {"a": 0, "c": 2},
    "keys": ["a", "b", "c"],
    "positions": {"a": 0, "b": 1, "c": 2},
    "renumbered": true,
    "puts": ["b"],
    "deletes": []
  },
  {
    "case": "move",
    "before": {"a": 0, "b": 1, "c": 2},
    "keys": ["c", "a", "b"],
    "positions": {"c": 0, "a": 1, "b": 2},
    "renumbered": true,
    "puts": ["c", "a", "b"],
    "deletes": []
  },
  {
    "case": "replace_whole_section",
    "before": {"a": 0, "b": 1},
    "keys": ["c", "b"],
    "positions": {"c": 0, "b": 1},
    "renumbered": true,
    "puts": ["c"],
    "deletes": ["a"]
  },
  {
    "case": "from_empty",
    "before": {},
    "keys": ["x", "y"],
    "positions": {"x": 0, "y": 1},
    "renumbered": false,
    "puts": ["x", "y"],
    "deletes": []
  }
]
```

`puts` are in position order and `deletes` in key order, as §5.1 lists changes.

### 10.7 After the cutover

- Ordinary schema-3 startup refuses a schema-1 or schema-2 home (`MigrationRequired`). Offline
  homes and old backups use the same converter, with the same preconditions; a restore of an
  old backup holding live work is refused (§10.2).
- Before production resumes (step 12), a failed cutover recovers by restoring the step-9 backup
  and the old release together. After it, selecting the old binary alone is invalid (it refuses
  schema 3): recover forward, or by a controlled restore that accounts for the changes since.

### 10.8 Compatibility checks

`scripts/compat-check` gains `--incompatible`, chosen automatically when the candidate's release
manifest `schema` (a new integer field written by `build-release`) differs from the home's
`schema_version`, and `--copy DIR`, which runs against a prepared private copy (the rehearsal
helper's, §10.10) instead of taking its own. In incompatible mode it requires that no live run
is pinned to any release in the database it checks (a pin is an error, not a skip), converts
that copy with the candidate's `sluice home migrate` (unweakened preconditions), starts the
candidate's coordinator on it and runs `log_read`, `status`, `plan_get` for every project,
`plan_history` (first page), `plan_read` and `step_context` for one step; then it runs the
selected (old) release against the converted copy, which must refuse with an unsupported-schema
error and leave the copy's bytes unchanged: its `coordinator` (the writer) and its `tool
log_read` (a local read), the two that open the database themselves. (**Lane G:** not `status`,
which goes through a coordinator and, with none running and no route to the service manager,
fails at activation before it reads anything.) Run on a copy of a home with
live work, it fails on the pins by design: the rehearsal helper is what brings a copy to zero
blockers first. Ordinary compatible deploys keep today's mode.

### 10.9 The AGENTS.md rule

The study's wording, adjusted to say why and where additive changes go (as written in
AGENTS.md):

> Incompatible storage changes require a new schema version and a drained, fenced migration
> with no live consumers of the old schema: a run's pinned `sluice` reads the database itself
> and refuses any other version. Schema 2 remains reserved for the historical interim board
> layout; normalized plans use schema 3. Additive changes use version-scoped migrations
> (`ADDED_COLUMNS` and `ADDED_VIEWS` for the current version). Never re-pin a running
> executable by changing database metadata.

with a shipping note that `scripts/ship` ships compatible changes only and a schema change goes
out through `scripts/deploy --schema-cutover --deadline <time>`.

### 10.10 The rehearsal helper (round 2)

A copy of a home with live work holds exactly the attempts, runs and leases the drain counts as
blockers, so converting it, or running `compat-check --incompatible` on it, fails before the
deadline handling is ever exercised. The rehearsal therefore plays the deadline on the copy
first, with the old release's own code, and converts only a copy that reached zero blockers.

`scripts/cutover-rehearse --candidate <release dir> [--old <release dir>] [--deadline <time>]
[--keep] [--json]` (lane G owns the script, its report and its place in `--schema-cutover` and
`--dry-run`; lane H owns the harness and the test adoption host it builds):

1. **Copy.** As `compat-check` copies today: the home's `sluice.db` by the SQLite backup API
   (the live file opened read-only), `config.json`, the fn and recipe trees, never a `.env`,
   into `/tmp/cr.*`. The copy is a scratch home: no installation selects it, nothing from it
   is registered with the user service manager, and the helper runs as a plain child process.
2. **Play the deadline with the old release.** The harness `cutover-rehearsal` (lane H) is a
   small binary outside the workspace (`tools/cutover-rehearsal/`, its own `Cargo.toml`,
   listed in the workspace's `exclude`), built against the **old** release's crates: the helper
   checks out the old release's commit (from its manifest; `--old`, default the selected
   release) into a temporary worktree and builds the harness with `path` dependencies on it.
   It opens the old coordinator in process on the copy (`Coordinator::open(copy, catalog,
   RehearsalHost)`, with the catalog the old coordinator builds for that home, so a cancel the
   catalog would refuse is refused here too, and no scheduler loop) with H's `RehearsalHost`:
   its `FnHost`, `ExecutionHost` and `AdoptionHost` never call `systemd-run` or `systemctl`,
   never signal
   a PID, never opens a cgroup or a socket outside the copy, and launches nothing: every
   reconcile reports the guardian gone with its processes proven gone. The harness then runs
   the cutover's steps 3 to 7 with the old code: `drain` (author `cutover`), `step_cancel`
   once per (project, step) with live attempts and the same reason, the intent check, and one
   adoption pass, which settles every nonterminal attempt through the old completion path
   (cancelled when intent was stored, lost otherwise; calls `process_lost`) and releases their
   leases. It re-reads the blockers: any left, or any refused cancel, fails the rehearsal and
   reports them as the cutover would (`CancelRefusal`), which is the real cutover's step-6 risk
   found early.
3. **Convert, only at zero blockers.** The candidate's `sluice home migrate --json` on the copy,
   with its unweakened preconditions, then `scripts/compat-check --incompatible --copy <copy>
   --release <candidate>` (§10.8).
4. **Report** what the play cancelled and how each run ended there (the same `CutoverReport`
   shape, `stopped` from the copy), the conversion report and the compat table; exit non-zero
   on any failure; remove the copy and the worktree unless `--keep`.

**The harness's command line** (lane G's helper calls it; lane H2 builds it): `cutover-rehearsal
rehearse --home <copy> --deadline <RFC 3339> --sha <candidate sha> [--report <file>]`, run with
`SLUICE_HOME` the copy and no route to the user service manager (`XDG_RUNTIME_DIR` and
`DBUS_SESSION_BUS_ADDRESS` point into the scratch directory). The cancel's reason is the
deadline's (`schema-3 cutover at <deadline>: stopped at the deadline; retry it after the
cutover`) and its author `cutover`. It prints the play's `CutoverReport` as JSON on stdout (or
to `--report`), with `refused` in the `cutover.refused` fixture's shape, and on stderr one line
per stopped run, refused cancel and blocker left; it exits 0 at zero blockers with no refused
cancel, 1 when the play ran but left either, and 2 on an error of its own (said on stderr).
`cutover-rehearsal oracle --home <copy> [--out <file>]` prints every plan's documents replayed
revision by revision with the old release's patch semantics, for the legacy fixtures. The helper
builds it from this checkout's `tools/cutover-rehearsal`, copied into a `git worktree` of the
old release's commit beside a copy of that commit's `Cargo.lock` (the harness is its own
workspace, `[workspace]` in its `Cargo.toml`, with `path` dependencies `../../crates/<crate>`),
with `cargo build --release` into `<prefix>/.build/rehearsal-target`; `--harness BIN` takes one
already built. It never trusts the harness's word alone: it re-reads the copy's blockers itself,
and reads each run's outcome from the copy as the cutover reads it from the home.

Production preconditions are never weakened: the converter has no switch to skip blockers, the
helper never deletes or rewrites a blocker row itself (only the old release's own cancel,
completion and adoption code ends live work on the copy), and nothing in the helper can reach a
production unit, cgroup, socket or the live database file. Lane H tests the host's isolation (a
reconcile of a unit name that exists in the user manager touches nothing) and the helper end to
end on a scratch home with a live rolling run.

## 11. Cost counters and test budgets

Lanes B and C count, lane H reads. The counters are process-wide (the writer runs on its own
thread), reset and read by the test that measures:

| Counter | Counted by | Meaning |
|---|---|---|
| `declarations_decoded` | B | step, input or output declarations parsed from SQL text |
| `declarations_written` | B | declarations written |
| `rows_written` | B | authored and index rows inserted, updated or deleted by an edit |
| `full_exports` | B | `export_plan` and `read_plan_rows` calls |
| `full_compiles` | C | `compile_rows` calls |
| `positions_renumbered` | B | rows whose position changed other than by their own put |
| `state_rows_read` | B | step, input, lease and competitor rows `read_scoped_state` read (round 2) |
| `preparations` | D | edit preparations started, by outcome (`committed`, `stale`, `contended`, `dry_run`) (round 2) |
| `writer_preparations` | D | preparations run on the writer's thread: always 0 (round 2) |

Release gates (lane H):

- The same local edit (a binding or metadata change of one step) at 198, 1,980 and 19,800
  unrelated steps decodes and writes the same counts (`declarations_decoded`,
  `declarations_written`, `rows_written` equal across the three sizes).
- No ordinary edit on a warm cache calls `export_plan`, `read_plan_rows` or `compile_rows`
  (`full_exports = full_compiles = 0`).
- Removal and prune renumber nothing (`positions_renumbered = 0`).
- After every removal (`step.remove`, `unit.remove`, a removing `step_update` chain) and every
  prune, rebuilding the indexes from the declarations (`step_index`, `output_references`, edge
  derivation) equals the stored `steps.unit`, `step_tags`, `plan_refs` and `plan_edges`
  exactly; in particular no `plan_refs` row names a removed consumer, and removing the source a
  removed reader read is then accepted (round 2).
- A compact `plan_read`, `step_get(compact)` and `unit_get(compact)`, and the competitor read
  of `read_scoped_state`, decode no declaration; lane H also checks their query plans use the
  covering indexes (`EXPLAIN QUERY PLAN`: `USING COVERING INDEX` on `steps_compact`,
  `steps_unit_compact`, `steps_status_compact` and `steps_needs`).
- A high-fanout edit (one input read by n steps) scales with n, not with the plan, and its
  `state_rows_read` is bounded by the affected set and its sources (§6.4).
- **Contention (round 2).** A barrier-driven test holds a high-fanout edit's preparation (one
  input read by 2,000 steps) at a barrier three times, and between releases commits an
  independent status write that moves `state_epoch`: the edit is refused `busy` with
  `retryable: true` and `EditContended`'s message after its third stale preparation, nothing
  is written, `writer_preparations` stays 0, and an independent small write issued while the
  preparations wait commits within the independent-write budget below.
- Cold compile and the cold recipe-matching cache are measured and reported separately, with no
  budget.
- `plan_scale`'s concurrent-write test uses a preparation barrier, not a sleep.

Proposed debug-build budgets (the study's; lane H measures before freezing them in CI): a local
metadata or binding edit with a bounded affected set ≤ 100 ms; a six-step unit addition with a
small external boundary ≤ 200 ms; an independent small write during an edit's preparation
≤ 100 ms. Today's bounds (1,000, 2,500 and 750 ms) stay until the measurement says otherwise.

## 12. Lanes

**Round 2: B, C, D, E and F are one atomic integration group.** Their cutovers cannot land one
by one: the coordinator's preparation uses the APIs C removes and B replaces
(`coordinator.rs`'s edit path: `edit::prepare_edit` over an `EditSnapshot`, then
`plans::edit_effect` with the compiled `plan.document()`), the log renders
the `Event::PlanEdit.ops` field B removes (`views/log.rs`), and the dashboard compiles with the
parser C removes (`views/board.rs`, `panel.rs`, `settings.rs`, `gallery.rs`). Making B wait on C
only exposes a cycle (C's edit path needs B's store, D needs both, E and F need D). So:

- **Each lane implements independently**, in its own worktree from this branch (`rw/pn-b` …
  `rw/pn-f`), against the pinned interfaces of this document. A lane's branch need not build
  the whole workspace: its gate is **scoped**: `cargo fmt --check`, Clippy with `-D warnings`
  and `cargo test --locked` for the crates and test targets it owns (each lane lists them),
  with the other lanes' consumers left as they are. Where a scoped test needs another lane's
  piece, it uses a hand-built value of the pinned type (B writes hand-built `PlanEditCommit`s;
  F reads hand-built rows), never a shim in production code.
- **One integration branch**, `rw/pn-cutover`, combines the group's cutover commits (in the
  order B, C, D, E, F, conflicts resolved there), then G's and H's. The workspace gate
  `scripts/check` runs only there, and must pass there before anything lands; so do H's release
  gates (§11) and G's rehearsal (§10.10) on a copy of the live home.
- **Intermediate states are never deployed or landed.** No lane branch and no partial
  combination is pushed to `main`, shipped or deployed; `scripts/ship` refuses an incompatible
  ref anyway (lane G). The integration branch lands as one push and goes live only through
  `scripts/deploy --schema-cutover` (§10.1).
- **H solely owns the test-only reference**: today's compiler, patch semantics, edit
  preparation and whole-plan simulation, moved verbatim into `crates/sluice-reference`
  (`publish = false`, a dev-dependency only; never linked into a binary), and the differential
  harness over it. C consumes it as a dev-dependency and never keeps its own copy. H delivers
  the reference first, before C deletes anything.

A **HARD** edge: the lane cannot finish (its done-when) until the named piece exists. A **SOFT**
edge: coordinate, but work against the pinned interface. Every lane: works in its own worktree
from this branch; changes the SPEC sections it implements, removing their "(schema 3; lands
with the plan-rows cutover)" marks only on the integration branch (lane G does it once the whole
cutover passes); deletes what it supersedes (no shims, no dual paths); commits as the owner
without AI attribution.

```
A (contracts) ──┬──> H1 (reference crate + harness) ──> C ──┐
                ├──> B ─────────────────────────────────────┤
                ├──> D ─────────────────────────────────────┤
                ├──> E ─────────────────────────────────────┼──> integration rw/pn-cutover
                ├──> F ─────────────────────────────────────┤      (B+C+D+E+F, then G, H2)
                ├──> G (tooling, rehearsal helper) ─────────┤      scripts/check, §11 gates,
                └──> H2 (fixtures, gates, rehearsal host) ──┘      rehearsal on a live copy
                                                                     │
                                                       one landing ──┴──> deploy --schema-cutover
```

**Critical path:** A → H1 (the reference crate) → C (incremental model, differential tests
green) → integration (B, C, D, E, F combined, `scripts/check`) → H2's gates and G's rehearsal on
the integration branch → landing and the cutover. B, D, E, F, G and H2 run in parallel with H1
and C; the longest of them that is not C usually sets the integration date.

### Lane B: storage and history

- **Scope.** `migrations/0003.sql`; `schema.rs` (version 3, 27 tables, `MigrationRequired`,
  `RECORD_PAYLOAD_VERSION` 2, no `ADDED_*`); `plans.rs` row reads and writes (§8, including
  `read_scoped_state`), `commit_plan_edit` over `PlanEditCommit`, history paging; the
  `CHECK`-held step columns and the `plan_refs` triggers (§2.2); `projects.rs` (initializer,
  `last_retirement`, delete order); `records.rs`; `events.rs` `Event::PlanEdit`; `backup.rs`
  restore conversion and its live-work refusal; `query.rs` view docs; the converter
  `sluice-store/src/convert.rs` with the legacy RFC 6902 replay confined to it (`json-patch`
  leaves every other path) and §10.6's positions; `PlanRows::{to_document, from_document}` in
  `plan_rows.rs`; the store's cost counters.
- **Scoped gate.** `sluice-store` and `sluice-model`'s `plan_rows` tests.
- **Edges.** HARD A. SOFT C (`step_index`, `output_references`, `PlanEditCommit`; B tests write
  rows and commits built by hand). Integration group.
- **Done when.** A fresh home is schema 3 with §2.2's DDL; `commit_plan_edit` applies a
  hand-built `PlanEditCommit` exactly as §4 says (no-op, stale, conflict, removal with its
  `plan_refs` gone, order.set's two-phase positions); a step-column write that disagrees with
  its declaration is refused by its `CHECK`; history pages as §5.4; `from_document` passes
  `replay.positions`; the converter converts a schema-1 and a schema-2 fixture home with
  gapped, malformed and well-formed histories (blockers named) and a copy of the live home
  (read-only backup, deleted afterwards) with round-trip equality at every revision; a restore
  of a backup with a live attempt is refused with §10.2's message and writes nothing;
  converted and fresh homes are schema-equivalent; the scoped gate green.

### Lane C: incremental model

- **Scope.** `sluice-model`: `compile_rows`, the persistent compiled `Plan`, the certified
  candidate (`CertifiedPlan`, §8.1), `step_index`, `output_references`, `preparation_reads`
  and `prepare_plan_edit` over the read set (§6.4), `PlanOp` application (§7.6) with
  `check_supplied` and the typed no-op lowering (§7.8), incremental validation (§6.1, the input
  and output rows included), cycle checks on edge deltas, unit membership and exits deltas,
  impact reconciliation and preview (§6.3), recipe expansion against the candidate; deletes
  `Plan.document`, `patch`, `PlanPatchData`, `prepare_patch`, `edit::PlanEdit`/`PreparedEdit`/
  `prepare_edit`, the whole-plan `simulate_edit` on ordinary edits (kept for `"all"`).
- **Scoped gate.** `sluice-model`.
- **Edges.** HARD A and H1 (the reference crate it tests against). Integration group.
- **Done when.** Differential property tests (generated plans and atomic op sequences) agree
  with H's reference: acceptance, rejection paths and messages (`validation.differential`
  included), ordered export after every accepted edit, nested paths, fan-in, scatter, open
  outputs, boolean gates, unit membership and exits, singleton collisions, unit-gate cycles,
  batches that cycle only together, distant readers of changed types, input and output puts,
  staleness restored when hashes match again, running-step rules, no-ops (typed and net),
  `start: false`, recipe overrides; a step added with a fn the base never used compiles into
  the candidate; preparation never consults a step outside its read set (debug assertion);
  `full_compiles` stays 0 on warm edits; the scoped gate green.

### Lane D: runtime and completion

- **Scope.** `coordinator.rs` (plan cache and §8.1's install rule, tokens, preparation outside
  the writer with §4's retry rule and `EditContended`, edit dispatch for every edit tool),
  `scheduler.rs` and `attempts.rs` (no `FrozenPlan`; completion from the attempt's frozen
  contract and identity; `admitted_rev` scalar), `execution.rs`, `resources.rs::admit_order`,
  `watch.rs`, `verify.rs` (index rebuild and compare), `naming.rs` (§8 keys), `dispatch_ext.rs`
  (`edit_extension`, input preview, `step_context`), `me.rs`, the model's `naming::name_plan`
  over rows.
- **Scoped gate.** `sluice-runtime` (its unit tests and the integration tests it owns).
- **Edges.** HARD A. SOFT B and C (it codes against their pinned APIs). Integration group.
- **Done when.** Every edit tool runs through the one pipeline; the barrier test of §11
  passes (`busy`, nothing written, `writer_preparations` 0); a committed edit installs its
  candidate only over its own base; completion succeeds with the catalog broken and with no
  snapshot in the attempt; no reader in §9 owned by D reads a document; `verify` reports a
  hand-corrupted index row; frozen completion after plan and registry changes passes; green on
  the integration branch.

### Lane E: tools and agent docs

- **Scope.** `commands.rs` (§7.10's variants; delete `PlanPatch`, `PatchOperation`, the old
  preview and result types), `cli.rs`, `mcp.rs`, `drain.rs::check_command`, the coordinator's
  command lists, `edit_label`, `compose::reply_value`, the read tools' dispatch (`plan_get`,
  `plan_read`, `step_get`, `unit_get`, `plan_history`, `plan_view`'s filters and boundary nodes),
  `docs/agent/*` (patch examples replaced; JSON plan examples kept as the export format),
  `docs/rust/schemas.json`, SPEC §12, and the onboarding text: "Read the unit or steps you need
  with `unit_get`, `step_get` or `plan_read`. Use the typed tools for single changes and
  `plan_edit` for an atomic batch. Pass `rev` when a change depends on an earlier read.
  `plan_get` exports the whole plan. A preview describes the edit's affected work; ask for a
  full dry run explicitly. An edit refused `busy` is retried as is."
- **Scoped gate.** `sluice-model`'s command tests and the `sluice` crate's tool tests.
- **Edges.** HARD A. Integration group (final behaviour needs B, C and D).
- **Done when.** Every tool in §7 decodes from MCP, HTTP and `sluice tool` with the same flat
  arguments and returns the fixtures' shapes; `plan_patch` is unknown everywhere (tools,
  helper allowlists, docs); tool-description tests pass; green on the integration branch.

### Lane F: dashboard readers

- **Scope.** The loaders of the shipped dashboard (`775d57c`): `views/board.rs`
  (`PlanCache::compile`, `load_board`, `load_step`, `plan_mermaid`), `views/panel.rs`,
  `settings.rs` (through `steps_needs`), `views/log.rs` (`changes` in sentences and the retire
  fold), `views/step.rs::load_detail` (§9.1), `views/gallery.rs::step_band_part`, `views.rs`,
  `examples/dashboard_fixture/`, the web tests' fixtures. Loaders only: rendering, templates and
  layout are not this lane's.
- **Scoped gate.** `sluice-web`.
- **Edges.** HARD A. Unblocked: the redesign it builds on is on main as `775d57c`, and no other
  lane edits these files. Integration group (final behaviour needs B and D).
- **Done when.** No dashboard reader reads a document or compiles per request; §9.1's pause
  cases pass; every page it touches passes AGENTS.md's Chromium checks (390, 1440 and 2560 px,
  light and dark, each screenshot looked at) on a scratch home built from the integration
  branch; green there.

### Lane G: deployment and the cutover

- **Scope.** `scripts/deploy --schema-cutover` (§10.1: `--deadline`, `--cancel-grace`,
  `--settle-timeout`, `--dry-run`, fence then cancel, the intent check, the refusal stop, the
  bounded zero-blocker check, the notice text, the report and `cutover-<ts>.json` as
  `CutoverReport`), `scripts/cutover-rehearse` (§10.10, with H's harness),
  `scripts/compat-check --incompatible --copy` (§10.8), `scripts/build-release` (manifest
  `schema`), `scripts/ship` (refuses an incompatible ref), `sluice home migrate` (CLI over B's
  converter), SPEC §2.2, §2.6 and §3's cutover text, AGENTS.md's shipping notes.
- **Edges.** HARD A. Its migrate CLI and end-to-end tests need B and H2; it joins the
  integration branch after the group.
- **Done when.** `--dry-run` lists what would be stopped and changes nothing; a scratch
  installation with a live rolling run (a scratch home, test-mode units) goes through notice,
  drain, fence, cancel request and verified intent, settlement, migration and release, the
  report naming the run and its actual outcome; a scratch run with settle intent reports
  `succeeded` / `none`; a scratch catalog drift makes the cancel refuse and the cutover stop
  fenced with `refused` filled and no unit stopped; `compat-check --incompatible` proves the
  old binary refuses before mutation; the rehearsal helper takes a copy of the live home with
  live work to zero blockers with the old release, converts it and passes the compat check.

### Lane H: independent verification

- **Scope.** H1, first: `crates/sluice-reference` (today's compiler, patch semantics, edit
  preparation and whole-plan simulation, test-only) and the differential harness and generators
  over it, consumed by C. H2: replay fixtures for schema 1 and 2 (absent root fields,
  normalized old input rows, trimmed records, malformed and gapped history, interrupted
  conversion, backup and restore, a backup with live work); `tools/cutover-rehearsal` and its
  `RehearsalHost` (§10.10); the counters and gates of §11; schema equivalence; `plan_scale`
  tightened; compatibility tests; Chromium evidence from F.
- **Edges.** H1: HARD A, and C's HARD dependency. H2: HARD A; final on the integration branch.
- **Done when.** H1: the reference compiles every plan today's compiler does with the same
  results, and `validation.differential` runs through it. H2: every gate of §11 passes on the
  integration branch; every historical revision of a read-only live backup converts with
  round-trip equality; the grep of §9 finds no leftover reader; the rehearsal host's isolation
  test passes.

## 13. Decisions

Round 2 (after the astra review) changed decisions 3, 4, 11, 22, 25 and 26 and added 28 to 36;
each is marked.

| # | Decision | Where |
|---|---|---|
| 1 | `root_order` constrained to its eleven values | §2.2 |
| 2 | No `base_rev` or `change_version` columns or fields | §2.2, §5.2 |
| 3 | **Round 2:** `steps.paused`, `run`, `priority` and `needs` are plain columns held equal to the declaration by `CHECK`s (were generated: SQLite never covers an index holding a generated column); `unit` maintained | §2.2 |
| 4 | Three covering indexes serve compact reads, and **round 2** `steps_needs` serves resource competitors; no `steps_run_position` | §2.2 |
| 5 | `state_epoch` moved by triggers; leases excluded | §2.2, §3 |
| 6 | `board_slots` column and view dropped | §2.1 |
| 7 | Put into an absent section adds it at its canonical place; sections never removed | §2.4 |
| 8 | New projects start with all three sections, as SPEC always said | §2.4 |
| 9 | One record payload version (2) for every kind | §5.2 |
| 10 | `recipe_generation` is a digest, not a counter | §3 |
| 11 | **Round 2, reversed:** preparation never runs in the writer; the third stale preparation is refused `busy`, `retryable: true` (`EditContended`) | §4 |
| 12 | Net effect per id decides added, changed, removed | §4 |
| 13 | `reason` required by `plan_edit`, `unit_update`, `unit_remove` | §7.1 |
| 14 | `plan_read` filters never `not_found`; default `limit` 200 | §7.3 |
| 15 | Cursor text format; binds `rev` always, `state_epoch` only with `status`, recipes only with `recipe` | §7.3 |
| 16 | Operation refusals collected as `invalid` with `ops[i]` paths before validation | §7.6 |
| 17 | `plan_edit`'s `steps` are the ids its adds added | §7.6 |
| 18 | `StepChanges` is closed; unknown keys are `bad_request`, for `step_update` too | §7.6 |
| 19 | Typed tools gain `preview_scope` and keep their own error kinds | §7.8 |
| 20 | New typed reply variants for the read tools | §7.10 |
| 21 | The compiled type keeps the name `Plan` and its query API | §8 |
| 22 | **Round 2, reversed:** one converter mode; a schema-changing restore refuses a backup holding live work (`LiveWorkInBackup`) | §10.2 |
| 23 | Snapshot mismatches in terminal attempts are warnings | §10.4 |
| 24 | Round-trip equality is compact-serialization byte equality | §10.5 |
| 25 | **Round 2:** replayed positions are assigned retain/append/gap, then the complete key sequence is compared with the document's and the collection renumbered whenever they differ | §10.6 |
| 26 | **Round 2:** fence, then one `step_cancel` per (project, step) (author `cutover`) with the intent verified before any unit stops; a refusal stops the cutover fenced; the cutover records intent only, existing completion precedence holds, and the report gives each run's actual outcome and advice; calls end `process_lost` | §10.1 |
| 27 | Release manifest gains `schema`; `compat-check --incompatible` | §10.8 |
| 28 | **Round 2:** triggers delete a removed step's or output's `plan_refs`; index-rebuild equality is required after every removal and prune | §2.2, §4, §11 |
| 29 | **Round 2:** preparation reads a read set (sources, inputs, unit exits, pause, holds, competitors) separate from the preview's affected set | §6.4 |
| 30 | **Round 2:** preparation returns a certified compiled candidate beside the store payload (`PlanEditCommit`); it is installed only after commit over its own base | §8.1 |
| 31 | **Round 2:** empty-list refusals apply to supplied requests only; typed lowering may yield no operations and commits nothing; `step_set_input` keeps its refusal | §7.6, §7.8 |
| 32 | **Round 2:** input and output puts have their own validation rows, checked differentially (`validation.differential`) | §6.1 |
| 33 | **Round 2:** the rehearsal helper plays the deadline on a copy with the old release and a test adoption host, and converts only at zero blockers | §10.10 |
| 34 | **Round 2:** B to F are one atomic integration group on `rw/pn-cutover`; H owns the test-only reference crate | §12 |
| 35 | **Round 2:** "who paused it" is the latest actual pause transition in the step's current incarnation | §9.1 |
| 36 | **Round 2:** the cutover report is `plan_rows::CutoverReport` | §10.1 |
