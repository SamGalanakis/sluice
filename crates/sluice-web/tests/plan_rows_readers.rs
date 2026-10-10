//! The dashboard's plan readers over hand-built schema-3 rows (plan-rows §9, §9.1): who paused a
//! step is read from `plan_edits.changes`, and the compiled-plan cache is keyed by revision and
//! signatures, never by a document.
use rusqlite::Connection;
use serde_json::json;
use sluice_model::{
    ids::{ProjectId, Revision, StepId},
    plan::{FnSignature, SignatureProvider, compile_rows},
    plan_rows::{PlanChange, PlanHeader, PlanRows, RootSection, StateEpoch, StepRow},
};
use sluice_web::views::{board::PlanCache, step::who_paused};
use std::cell::Cell;

/// `plan_edits` as schema 3 declares it (plan-rows §2.2), alone.
fn edits() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE plan_edits (
          project_id TEXT NOT NULL,
          rev INTEGER NOT NULL CHECK (rev >= 1), seq INTEGER NOT NULL CHECK (seq > 0),
          at TEXT NOT NULL, author TEXT NOT NULL, reason TEXT NOT NULL,
          changes TEXT NOT NULL CHECK (json_type(changes) = 'array'),
          PRIMARY KEY (project_id, rev)
        ) STRICT;",
    )
    .unwrap();
    c
}

/// A hand-built history: each edit's revision, author and changes, as the writer logs them.
struct History {
    c: Connection,
    project: ProjectId,
    rev: u64,
}
impl History {
    fn new(c: Connection, project: ProjectId) -> Self {
        let mut history = Self { c, project, rev: 0 };
        history.edit(
            "cli",
            vec![PlanChange::HeaderPut {
                root_order: vec![
                    RootSection::Inputs,
                    RootSection::Outputs,
                    RootSection::Steps,
                ],
            }],
        );
        history
    }
    fn edit(&mut self, author: &str, changes: Vec<PlanChange>) {
        self.rev += 1;
        self.c
            .execute(
                "INSERT INTO plan_edits(project_id,rev,seq,at,author,reason,changes) VALUES (?1,?2,?3,?4,?5,'',?6)",
                rusqlite::params![
                    self.project.to_string(),
                    self.rev as i64,
                    1000 + self.rev as i64,
                    format!("2026-10-10T10:{:02}:00Z", self.rev),
                    author,
                    serde_json::to_string(&changes).unwrap(),
                ],
            )
            .unwrap();
    }
    /// One edit putting `step` with `declaration`, beside another step's put (which must never
    /// be credited for this one).
    fn put(&mut self, author: &str, step: &str, declaration: serde_json::Value) {
        self.edit(
            author,
            vec![
                put(
                    "other",
                    json!({"run": "custom.open", "paused": "not this one"}),
                ),
                put(step, declaration),
            ],
        );
    }
    fn who(&self, step: &str) -> Option<(String, String)> {
        who_paused(&self.c, self.project, &step.parse().unwrap()).unwrap()
    }
    fn at(rev: u64) -> String {
        format!("2026-10-10T10:{rev:02}:00Z")
    }
}
fn put(step: &str, declaration: serde_json::Value) -> PlanChange {
    PlanChange::StepPut {
        step: step.parse().unwrap(),
        position: 0,
        declaration: serde_json::from_value(declaration).unwrap(),
    }
}

/// Plan-rows §9.1: a pause, then a tag edit, then a doc edit leave the pause's author; a reason
/// change, and a reason turned into `true`, credit their own; a remove and re-add count only the
/// new incarnation; a step that is not paused names nobody.
#[test]
fn who_paused_a_step_is_its_latest_pause_transition_in_its_current_incarnation() {
    let project = ProjectId::new();
    let mut h = History::new(edits(), project);
    h.put("orch", "work", json!({"run": "custom.open"}));
    assert_eq!(h.who("work"), None, "never paused");
    // rev 3: alice pauses it; rev 4 and 5 carry the pause along
    h.put(
        "alice",
        "work",
        json!({"run": "custom.open", "paused": "after the release"}),
    );
    h.put(
        "bob",
        "work",
        json!({"run": "custom.open", "paused": "after the release", "tags": ["unit:ship"]}),
    );
    h.put(
        "carol",
        "work",
        json!({"run": "custom.open", "paused": "after the release", "tags": ["unit:ship"], "doc": "Ship it"}),
    );
    assert_eq!(h.who("work"), Some(("alice".into(), History::at(3))));
    // rev 6: another reason is another pause
    h.put(
        "dave",
        "work",
        json!({"run": "custom.open", "paused": "until review", "tags": ["unit:ship"], "doc": "Ship it"}),
    );
    assert_eq!(h.who("work"), Some(("dave".into(), History::at(6))));
    // rev 7: a reason turned into a plain pause is a transition too
    h.put(
        "erin",
        "work",
        json!({"run": "custom.open", "paused": true, "tags": ["unit:ship"], "doc": "Ship it"}),
    );
    assert_eq!(h.who("work"), Some(("erin".into(), History::at(7))));
    // rev 8 removes it, rev 9 adds it again paused: the new incarnation's add is credited, never
    // erin's pause of the step that was removed, though both pauses read `true`
    h.edit(
        "frank",
        vec![PlanChange::StepDelete {
            step: "work".parse().unwrap(),
        }],
    );
    h.put(
        "gina",
        "work",
        json!({"run": "custom.open", "paused": true}),
    );
    assert_eq!(h.who("work"), Some(("gina".into(), History::at(9))));
    // rev 10 resumes it: nobody holds it now
    h.put(
        "hank",
        "work",
        json!({"run": "custom.open", "paused": false}),
    );
    assert_eq!(h.who("work"), None, "resumed");
    // rev 11 pauses it again, rev 12 edits its doc
    h.put("ivy", "work", json!({"run": "custom.open", "paused": true}));
    h.put(
        "jack",
        "work",
        json!({"run": "custom.open", "paused": true, "doc": "Again"}),
    );
    assert_eq!(h.who("work"), Some(("ivy".into(), History::at(11))));
    // the other step, paused when it was added at rev 2 and carried along since, is the first
    // put's
    assert_eq!(h.who("other"), Some(("orch".into(), History::at(2))));
    // another project's history is its own
    assert_eq!(
        who_paused(&h.c, ProjectId::new(), &"work".parse().unwrap()).unwrap(),
        None
    );
}

/// A step removed and added again unpaused, then paused: the pause is the new incarnation's.
#[test]
fn a_removed_steps_pause_is_never_credited_to_the_step_added_in_its_place() {
    let mut h = History::new(edits(), ProjectId::new());
    h.put(
        "alice",
        "work",
        json!({"run": "custom.open", "paused": true}),
    );
    h.edit(
        "bob",
        vec![PlanChange::StepDelete {
            step: "work".parse().unwrap(),
        }],
    );
    h.put("carol", "work", json!({"run": "custom.open"}));
    assert_eq!(h.who("work"), None);
    h.put(
        "dave",
        "work",
        json!({"run": "custom.open", "paused": true}),
    );
    assert_eq!(h.who("work"), Some(("dave".into(), History::at(5))));
}

struct Open;
impl SignatureProvider for Open {
    fn signature(&self, _: &str) -> Option<FnSignature> {
        Some(FnSignature {
            open: true,
            ..Default::default()
        })
    }
}
/// One project's rows at `rev`: its steps, in position order.
fn rows(rev: u64, steps: &[&str]) -> PlanRows {
    PlanRows {
        header: PlanHeader {
            rev: Revision(rev),
            root_order: vec![RootSection::Steps],
            state_epoch: StateEpoch(0),
        },
        inputs: vec![],
        outputs: vec![],
        steps: steps
            .iter()
            .zip(0..)
            .map(|(step, position)| StepRow {
                step: step.parse().unwrap(),
                position,
                declaration: serde_json::from_value(json!({"run": "custom.open"})).unwrap(),
            })
            .collect(),
    }
}

/// The dashboard compiles a project's rows once per revision and signatures: a page at the same
/// revision reuses the plan, a new revision or new signatures compile again, and a plan that
/// does not compile is never kept.
#[test]
fn the_plan_cache_compiles_rows_once_per_revision_and_signatures() {
    let cache = PlanCache::default();
    let project = ProjectId::new();
    let compiles = Cell::new(0);
    let compile = |rows: PlanRows| {
        compiles.set(compiles.get() + 1);
        Ok(compile_rows(&rows, &Open).unwrap())
    };
    let first = cache
        .compiled(project, Revision(4), "catalog:a", || {
            compile(rows(4, &["a"]))
        })
        .unwrap();
    let again = cache
        .compiled(project, Revision(4), "catalog:a", || {
            compile(rows(4, &["a"]))
        })
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &again));
    assert_eq!(compiles.get(), 1);
    let id = |step: &str| step.parse::<StepId>().unwrap();
    // a new revision reads its rows again
    let edited = cache
        .compiled(project, Revision(5), "catalog:a", || {
            compile(rows(5, &["a", "b"]))
        })
        .unwrap();
    assert_eq!(compiles.get(), 2);
    assert!(edited.steps().contains_key(&id("b")));
    // new signatures compile the same revision again
    cache
        .compiled(project, Revision(5), "registry:b", || {
            compile(rows(5, &["a", "b"]))
        })
        .unwrap();
    assert_eq!(compiles.get(), 3);
    // another project has its own plan
    let other = cache
        .compiled(ProjectId::new(), Revision(5), "registry:b", || {
            compile(rows(5, &["z"]))
        })
        .unwrap();
    assert!(other.steps().contains_key(&id("z")));
    assert_eq!(compiles.get(), 4);
    // a plan that does not compile is an error and leaves the kept one in place
    let broken = cache.compiled(project, Revision(6), "registry:b", || {
        Err(sluice_model::error::PublicError::Invalid {
            message: "stored plan cannot be compiled".into(),
            errors: vec!["steps.b.run: unknown fn".into()],
        }
        .into())
    });
    assert!(broken.is_err());
    let kept = cache
        .compiled(project, Revision(5), "registry:b", || compile(rows(5, &[])))
        .unwrap();
    assert!(kept.steps().contains_key(&id("b")));
    assert_eq!(compiles.get(), 4);
}
