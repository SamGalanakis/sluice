//! The neutral fixture: projects that share nothing with any real one, for every page to render
//! and every screenshot to show (the generic rule: the dashboard knows only sluice's concepts).
//!
//! `almanac`, a team writing a field guide, has two recipes with different steps: `article`
//! (draft → review → publish) and the one-step `scan`. Its units sit in every band: done ones
//! (whose runs its Stats page measures), a failed and a cancelled one, one running longer than
//! its stage's done runs took, a quiet one, one whose step asks the owner a question, a wait chain across units
//! (a-8 after a-6, a-9 after a-8; s-5 after s-3), a unit of no recipe (`index`: two gathers
//! fanning in to a merge), and a long run of no unit (`survey`) reporting progress fields.
//! `chores` is a tiny project of three loose steps.
#![allow(dead_code)]
#[allow(dead_code)]
#[path = "../../../../tests/support/messages.rs"]
mod stored_messages;

use serde_json::{Value, json};
use sluice_model::{
    error::PublicError,
    ids::{AttemptId, ProjectId, RunId},
    plan::{FnSignature, Plan, SignatureProvider},
};
use sluice_store::{
    RetrySafety, Writer,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use std::path::Path;

/// The two projects and the names the next lanes read.
#[derive(Clone, Copy, Debug)]
pub struct Neutral {
    pub almanac: ProjectId,
    pub chores: ProjectId,
    /// The open question to the owner (from a-7-draft).
    pub question: i64,
}
/// The `article` recipe's stages, in order.
pub const ARTICLE: [&str; 3] = ["draft", "review", "publish"];
/// The units of `almanac`, by the band they open in.
pub const DONE: [&str; 5] = ["a-1", "a-2", "a-3", "s-1", "s-2"];
pub const STOPPED: [&str; 3] = ["a-4", "a-5", "s-4"];
pub const RUNNING: [&str; 5] = ["a-6", "a-7", "s-3", "index", "survey"];
pub const WAITING: [&str; 3] = ["a-8", "a-9", "s-5"];

struct Open;
impl SignatureProvider for Open {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        (name == "custom.open").then(|| FnSignature {
            open: true,
            ..Default::default()
        })
    }
}

/// An instant `seconds` ago, as the store writes one.
pub fn ago(seconds: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    sluice_web::views::rfc3339(now - seconds)
}

fn article_recipe() -> Value {
    json!({"name": "article", "params": {"topic": "string"}, "title": "{topic}",
        "view": "root = Stack([Param(\"topic\"), LastMessage(80)], \"row\")",
        "steps": {
            "{unit}-draft": {"run": "custom.open", "in": {"topic": {"default": "{topic}"}}},
            "{unit}-review": {"run": "custom.open", "after": ["{unit}-draft"]},
            "{unit}-publish": {"run": "custom.open", "after": ["{unit}-review"]}}})
}
fn scan_recipe() -> Value {
    json!({"name": "scan", "params": {"source": "string"}, "title": "Scan {source}",
        "view": "root = Stack([Param(\"source\"), LastMessage(80)], \"row\")",
        "steps": {"{unit}-scan": {"run": "custom.open", "in": {"source": {"default": "{source}"}}}}})
}

/// How a step of the fixture stands: its stored status, and its runs as (started ago, took)
/// seconds, the last one still running when it took `None`.
struct Step {
    id: String,
    status: &'static str,
    runs: Vec<(u64, Option<u64>)>,
    error: Option<PublicError>,
}
fn step(id: &str, status: &'static str, runs: &[(u64, Option<u64>)]) -> Step {
    Step {
        id: id.into(),
        status,
        runs: runs.to_vec(),
        error: None,
    }
}

async fn project(writer: &Writer, name: &'static str, description: &'static str) -> ProjectId {
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: name.parse().unwrap(),
                    description: description.into(),
                    icon: None,
                    resources: None,
                    author: "owner".into(),
                },
                &EmptyPlanInitializer,
                &NoResourceSettings,
            )
        })
        .await
        .unwrap()
        .project_id
}

/// Write `doc` as `project`'s plan and each step's status, runs and error.
async fn plan(writer: &Writer, project: ProjectId, doc: Value, steps: Vec<Step>) {
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let plan = Plan::parse_json(&serde_json::to_vec(&doc).unwrap(), &Open).unwrap();
            let p = project.to_string();
            tx.sql()
                .execute("DELETE FROM plans WHERE project_id=?1", [&p])?;
            sluice_store::plans::initialize_plan(tx, project, &plan)?;
            for s in &steps {
                let mut ids = vec![];
                for (started, took) in &s.runs {
                    let (attempt, run) = (AttemptId::new(), RunId::new());
                    let start = ago(*started);
                    let end = took.map(|t| ago(started.saturating_sub(t)));
                    // a failed run's result carries its error, as the store writes it
                    let result = took.map(|_| match (s.status, &s.error) {
                        ("failed", Some(e)) => json!({"status": "failed", "error": e}).to_string(),
                        ("failed", None) => json!({"status": "failed"}).to_string(),
                        _ => json!({"status": "succeeded"}).to_string(),
                    });
                    tx.sql().execute(
                        "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at,finished_at) VALUES (?1,?2,?3,1,1,?4,'{}','hash',?5,?6)",
                        (attempt.to_string(), &p, &s.id, if end.is_some() { "terminal" } else { "executing" }, &start, &end),
                    )?;
                    tx.sql().execute(
                        "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at,finished_at,result) VALUES (?1,?2,?3,?4,1,1,?5,?5,?6,?7)",
                        (run.to_string(), &p, attempt.to_string(), &s.id, &start, &end, result),
                    )?;
                    ids.push(run);
                }
                tx.sql().execute(
                    "UPDATE steps SET status=?3,run_ids=?4,error=?5 WHERE project_id=?1 AND step_id=?2",
                    (
                        &p,
                        &s.id,
                        s.status,
                        json!(ids).to_string(),
                        s.error.as_ref().map(|e| serde_json::to_string(e).unwrap()),
                    ),
                )?;
            }
            tx.changed(Some(project), "project");
            Ok(())
        })
        .await
        .unwrap();
}

/// Seed both projects into the home at `home` through `writer`.
pub async fn seed(writer: &Writer, home: &Path) -> Neutral {
    const M: u64 = 60;
    const H: u64 = 3600;
    let almanac = project(
        writer,
        "almanac",
        "The field guide's spring and autumn editions, written a section at a time and checked against the regional checklist.",
    )
    .await;
    let topics = [
        ("a-1", "Gulls: the winter guide's entries"),
        ("a-2", "Herons: the autumn guide's entries"),
        ("a-3", "Plovers: the spring guide's entries"),
        ("a-4", "Waders: the autumn guide's entries"),
        ("a-5", "Owls: a section the editors set aside"),
        ("a-6", "Terns: the spring guide's entries"),
        ("a-7", "Shorebirds: the spring guide's entries"),
        ("a-8", "Skuas: the spring guide's entries"),
        ("a-9", "Auks: the spring guide's entries"),
    ];
    let sources = [
        ("s-1", "the checklist for renamed species"),
        ("s-2", "the photograph archive for credits"),
        ("s-3", "the coast photographs for duplicates"),
        ("s-4", "the range maps for broken links"),
        ("s-5", "the index for missing entries"),
    ];
    let mut steps = serde_json::Map::new();
    for (unit, topic) in topics {
        let tags = json!([format!("unit:{unit}")]);
        let mut draft =
            json!({"run": "custom.open", "tags": tags, "in": {"topic": {"default": topic}}});
        // the wait chain: a-8 after a-6, a-9 after a-8
        match unit {
            "a-8" => draft["after"] = json!(["a-6-publish"]),
            "a-9" => draft["after"] = json!(["a-8-publish"]),
            _ => {}
        }
        steps.insert(format!("{unit}-draft"), draft);
        steps.insert(
            format!("{unit}-review"),
            json!({"run": "custom.open", "tags": tags, "after": [format!("{unit}-draft")]}),
        );
        steps.insert(
            format!("{unit}-publish"),
            json!({"run": "custom.open", "tags": tags, "after": [format!("{unit}-review")]}),
        );
    }
    for (unit, source) in sources {
        let mut scan = json!({"run": "custom.open", "tags": [format!("unit:{unit}")], "in": {"source": {"default": source}}});
        if unit == "s-5" {
            scan["after"] = json!(["s-3-scan"]);
        }
        steps.insert(format!("{unit}-scan"), scan);
    }
    // a unit of no recipe: two gathers fanning in to a merge
    for (id, doc, after) in [
        ("index-birds", "Gather every bird entry's names", json!([])),
        (
            "index-places",
            "Gather every place the entries name",
            json!([]),
        ),
        (
            "index-merge",
            "Merge both into the guide's index",
            json!(["index-birds", "index-places"]),
        ),
    ] {
        steps.insert(
            id.into(),
            json!({"run": "custom.open", "doc": doc, "tags": ["unit:index"], "after": after}),
        );
    }
    // a long run of no unit, reporting progress
    steps.insert(
        "survey".into(),
        json!({"run": "custom.open", "doc": "Survey the archive's photographs against the checklist\n\nA folder at a time, for as long as the archive grows."}),
    );
    let cancelled = PublicError::Cancelled {
        message: "The editors set the owls aside for the next edition.".into(),
    };
    let failed = PublicError::FnFailure {
        message: "The style check found two entries without a photograph credit.".into(),
    };
    let broken = PublicError::FnFailure {
        message: "Three range maps link to a folder that no longer exists.".into(),
    };
    let done = |unit: &str, at: u64, draft: u64, review: u64| {
        vec![
            step(
                &format!("{unit}-draft"),
                "succeeded",
                &[(at + draft + review + 4 * M, Some(draft))],
            ),
            step(
                &format!("{unit}-review"),
                "succeeded",
                &[(at + review + 3 * M, Some(review))],
            ),
            step(
                &format!("{unit}-publish"),
                "succeeded",
                &[(at + 2 * M, Some(2 * M))],
            ),
        ]
    };
    let mut states: Vec<Step> = vec![];
    states.extend(done("a-1", 9 * H, 25 * M, 40 * M));
    states.extend(done("a-2", 5 * H, 30 * M, 35 * M));
    states.extend(done("a-3", 57 * M, 35 * M, 50 * M));
    states.extend([
        step("s-1-scan", "succeeded", &[(3 * H, Some(4 * M))]),
        step("s-2-scan", "succeeded", &[(80 * M, Some(6 * M))]),
        // stopped: a review failed, a draft cancelled, a scan failed
        step("a-4-draft", "succeeded", &[(3 * H, Some(28 * M))]),
        Step {
            error: Some(failed),
            ..step("a-4-review", "failed", &[(2 * H, Some(18 * M))])
        },
        Step {
            error: Some(cancelled),
            ..step("a-5-draft", "failed", &[(4 * H, Some(12 * M))])
        },
        Step {
            error: Some(broken),
            ..step("s-4-scan", "failed", &[(40 * M, Some(3 * M))])
        },
        // running: a-6 longer than its stage's done runs took, a-7 asking the owner, s-3 quiet
        step("a-6-draft", "running", &[(70 * M, None)]),
        step("a-7-draft", "running", &[(20 * M, None)]),
        step("s-3-scan", "running", &[(3 * H, None)]),
        step("index-birds", "succeeded", &[(50 * M, Some(14 * M))]),
        step("index-places", "running", &[(30 * M, None)]),
        step(
            "survey",
            "running",
            &[(4 * 86_400, Some(30 * H)), (2 * 86_400 + 4 * H, None)],
        ),
    ]);
    plan(writer, almanac, json!({"steps": steps}), states).await;
    // the long run's progress, reported four minutes ago
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET progress=?2,progress_at=?3 WHERE project_id=?1 AND step_id='survey'",
                (
                    almanac.to_string(),
                    json!({"checked": 1240, "remaining": 310, "folder": "coast/2019-05", "last mismatch": "A sanderling filed as a dunlin."}).to_string(),
                    ago(4 * M),
                ),
            )?;
            tx.changed(Some(almanac), "status");
            Ok(())
        })
        .await
        .unwrap();
    let recipes = home
        .join("projects")
        .join(almanac.to_string())
        .join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    std::fs::write(
        recipes.join("article.json"),
        serde_json::to_vec(&article_recipe()).unwrap(),
    )
    .unwrap();
    std::fs::write(
        recipes.join("scan.json"),
        serde_json::to_vec(&scan_recipe()).unwrap(),
    )
    .unwrap();
    let question = stored_messages::stored(
        writer,
        almanac,
        stored_messages::Stored {
            thread: "step-a-7-draft",
            from: "a-7-draft",
            to: Some("owner"),
            title: Some("Use the checklist's new spring dates in this draft?"),
            body: "The regional checklist moved the spring dates a week earlier.\n\n- **Now:** use the new dates and say so in each entry.\n- **Later:** wait for s-5 to finish the index first.",
            question: true,
            ..Default::default()
        },
    )
    .await
    .id
    .0;
    stored_messages::stored(
        writer,
        almanac,
        stored_messages::Stored {
            thread: "step-a-6-draft",
            from: "orchestrator",
            to: Some("a-6-draft"),
            body: "The tern maps moved under maps/coast/; point the entries there.",
            ..Default::default()
        },
    )
    .await;
    let chores = project(
        writer,
        "chores",
        "Three loose things to do around the office.",
    )
    .await;
    plan(
        writer,
        chores,
        json!({"steps": {
            "water-plants": {"run": "custom.open", "doc": "Water the plants by the window"},
            "sort-mail": {"run": "custom.open", "doc": "Sort the week's mail"},
            "file-receipts": {"run": "custom.open", "doc": "File the receipts", "after": ["sort-mail"]}}}),
        vec![
            step("water-plants", "succeeded", &[(2 * H, Some(5 * M))]),
            step("sort-mail", "running", &[(12 * M, None)]),
        ],
    )
    .await;
    Neutral {
        almanac,
        chores,
        question,
    }
}
