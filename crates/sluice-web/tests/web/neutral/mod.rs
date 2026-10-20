//! The neutral fixture: projects that share nothing with any real one, for every page to render
//! and every screenshot to show (the generic rule: the dashboard knows only sluice's concepts).
//!
//! `almanac`, a team writing a field guide, has two recipes with different steps: `article`
//! (draft → review → publish) and the one-step `scan`. Its units sit in every band: done ones
//! (whose runs its Stats page measures), two stopped at their wall-clock cap (stops sharing a
//! cause) and a cancelled one, one running longer than its stage's done runs took (it has said
//! something three times lately), a quiet one, one whose step asks the owner a question, a wait chain across units
//! (a-8 after a-6, a-9 after a-8; s-5 after s-3), a unit of no recipe (`index`: two gathers
//! fanning in to a merge), and a long run of no unit (`survey`) reporting progress fields.
//! `chores` is a tiny project of three loose steps.
#![allow(dead_code)]
#[allow(dead_code)]
#[path = "../../../../../tests/support/messages.rs"]
pub(crate) mod stored_messages;

use serde_json::{Value, json};
use sluice_model::{
    error::PublicError,
    ids::{AttemptId, ProjectId, RunId},
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
                    limits: None,
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

/// Lays a Codex session rollout for `step`'s running run under `home`, as its engine leaves one:
/// a turn as the run began, then a command at each of `acts` seconds ago, so its row's pulse
/// strip has its agent's actions to draw.
pub async fn lay_pulse(writer: &Writer, home: &Path, project: ProjectId, step: &str, acts: &[u64]) {
    let id = step.to_owned();
    let (run, started): (String, u64) = writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            Ok(tx.sql().query_row(
                "SELECT run_id,CAST(strftime('%s',started_at) AS INTEGER) FROM runs WHERE project_id=?1 AND step_id=?2 AND finished_at IS NULL",
                (project.to_string(), id),
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)),
            )?)
        })
        .await
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        * 1000;
    let started_ms = started * 1000;
    let session = format!("{}", sluice_model::ids::RunId::new());
    let run_dir = home.join("runs").join(&run);
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join("native.json"),
        json!({"engine": "codex", "session": session, "state": "Running"}).to_string(),
    )
    .unwrap();
    let sessions = home
        .join("codex-native-homes")
        .join(&session)
        .join("sessions/2026/10/15");
    std::fs::create_dir_all(&sessions).unwrap();
    let at = |ms: u64| sluice_web::views::rfc3339(ms / 1000);
    let mut lines = vec![
        json!({"timestamp": at(started_ms), "type": "session_meta", "payload": {"id": session}}),
        json!({"timestamp": at(started_ms), "type": "event_msg", "payload": {"type": "item_completed",
            "item": {"type": "UserMessage", "id": "task", "content": [{"type": "text", "text": "Your task is in task.md; read it fully, then do it."}]},
            "started_at_ms": started_ms, "completed_at_ms": started_ms}}),
    ];
    let mut acts = acts.to_vec();
    acts.sort_unstable_by(|a, b| b.cmp(a));
    for (n, ago) in acts.iter().enumerate() {
        let t = now - ago * 1000;
        lines.push(json!({"timestamp": at(t + 1500), "type": "event_msg", "payload": {"type": "item_completed",
            "item": {"type": "CommandExecution", "id": format!("exec-{n}"), "command": ["/bin/bash", "-lc", "rg -n 'range' sections/"],
                     "exit_code": 0, "aggregated_output": ""},
            "started_at_ms": t, "completed_at_ms": t + 1500}}));
    }
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(
        sessions.join(format!("rollout-2026-10-15T00-00-00-{session}.jsonl")),
        text,
    )
    .unwrap();
    // its run's own directory last written as it began, as a run whose agent keeps its
    // transcript elsewhere leaves it (so a quiet run stays quiet)
    std::fs::File::open(&run_dir)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(started))
        .unwrap();
}
/// Seconds ago, every `step`, from `from` down to `to`.
fn every(from: u64, to: u64, step: u64) -> Vec<u64> {
    (to..=from).rev().step_by(step as usize).collect()
}

/// Write `doc` as `project`'s plan and each step's status, runs and error.
async fn plan(writer: &Writer, project: ProjectId, doc: Value, steps: Vec<Step>) {
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let p = project.to_string();
            crate::seed::put(tx, project, doc)?;
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
        // a draft says what it is asked for (its brief) and what it hands on (its outputs)
        let mut draft = json!({"run": "custom.open", "tags": tags,
            "in": {"topic": {"default": topic},
                   "spec": {"default": format!("Draft the field guide's entries for {topic}: one entry a species, each with its range, its season and a photograph credited from the archive. Follow the regional checklist's names and dates.\n\nLeave a note for review on anything the checklist and the archive disagree about.")}},
            "outputs": {"entries": "int", "notes": "string"}});
        // the drafts run on two engines, so the plan's rows wear their badges
        draft["in"]["engine"] = json!({"default": if unit == "a-7" { "codex" } else { "claude" }});
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
    // the latest to finish says what it produced
    steps["a-3-publish"]["outputs"] = json!({"summary": "string"});
    for (unit, source) in sources {
        let mut scan = json!({"run": "custom.open", "tags": [format!("unit:{unit}")], "in": {"source": {"default": source}}});
        if unit == "s-5" {
            scan["after"] = json!(["s-3-scan"]);
        }
        steps.insert(format!("{unit}-scan"), scan);
    }
    // a unit of no recipe: two gathers fanning in to a merge; its title (its first step's) is
    // long, as an outcome sentence is
    for (id, doc, after) in [
        (
            "index-birds",
            "Gather every bird entry's names from the spring and autumn sections into one list, each with the section it appears in, its checklist code and every older name the checklist folds into it",
            json!([]),
        ),
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
        json!({"run": "custom.open", "doc": "Survey the archive's photographs against the checklist\n\nA folder at a time, for as long as the archive grows.",
            "in": {"spec": {"default": "Go through the archive's photographs a folder at a time and check each bird against the checklist: its species, where it was taken and the season. Note every photograph filed under the wrong species, and every species on the checklist with no photograph at all.\n\nWrite the mismatches to a report, one line each, with the photograph's path and what it should be filed as."}},
            "outputs": {"mismatches": "int", "report": "string"}}),
    );
    let cancelled = PublicError::Cancelled {
        message: "The editors set the owls aside for the next edition.".into(),
    };
    let capped = || PublicError::AgentFailure {
        retry_at: None,
        kind: "WallCap".into(),
        message: "The run reached its wall-clock cap.".into(),
        session: None,
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
        // stopped: a review and a scan at their wall-clock cap (one cause), a draft cancelled
        step("a-4-draft", "succeeded", &[(3 * H, Some(28 * M))]),
        Step {
            error: Some(capped()),
            ..step("a-4-review", "failed", &[(2 * H, Some(18 * M))])
        },
        Step {
            error: Some(cancelled),
            ..step("a-5-draft", "failed", &[(4 * H, Some(12 * M))])
        },
        Step {
            error: Some(capped()),
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
            from: "@lead",
            to: Some("a-6-draft"),
            body: "The tern maps moved under maps/coast/; point the entries there.",
            ..Default::default()
        },
    )
    .await;
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET outputs=?2 WHERE project_id=?1 AND step_id='a-3-publish'",
                (
                    almanac.to_string(),
                    json!({"summary": "Fourteen entries, each with its photograph credited."})
                        .to_string(),
                ),
            )?;
            // the oldest finished draft handed on what it was asked for (the latest ones left
            // nothing to say), and so did the draft before the failed review
            for (unit, entries) in [("a-1", 12), ("a-4", 11)] {
                tx.sql().execute(
                    "UPDATE steps SET outputs=?2 WHERE project_id=?1 AND step_id=?3",
                    (
                        almanac.to_string(),
                        json!({"entries": entries, "notes": "The checklist and the archive agree on every species."})
                            .to_string(),
                        format!("{unit}-draft"),
                    ),
                )?;
            }
            tx.changed(Some(almanac), "steps");
            Ok(())
        })
        .await
        .unwrap();
    // the index's gather was told something long just as it began (so it has said nothing
    // since): its row cuts it, its drilldown shows it whole
    let id = stored_messages::stored(
        writer,
        almanac,
        stored_messages::Stored {
            thread: "step-index-places",
            from: "@lead",
            to: Some("index-places"),
            body: "Use the checklist's own spellings for every name, even where the guide's sections disagree: `checklist/2024/species.csv` is the source of truth, and its `alt_names` column lists the older names to fold in.\n\nKeep the list in the order the sections first mention each bird, and end with a line naming any bird a section mentions that the checklist lacks, with the section's path (`sections/spring/coast/terns-and-skimmers.md`) so review can find it.",
            ..Default::default()
        },
    )
    .await
    .id
    .0;
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql()
                .execute("UPDATE messages SET at=?2 WHERE id=?1", (id, ago(29 * M)))?;
            tx.changed(Some(almanac), "messages");
            Ok(())
        })
        .await
        .unwrap();
    // what the agents did, as their transcripts say: a-6 busy all along, a-7 busy until it
    // asked its question, the index's places a few times an hour, the quiet scan last nearly
    // three hours ago
    let busy = [every(68 * M, 24 * M, 45), every(20 * M, 40, 30)].concat();
    lay_pulse(writer, home, almanac, "a-6-draft", &busy).await;
    lay_pulse(
        writer,
        home,
        almanac,
        "a-7-draft",
        &every(19 * M, 12 * M, 40),
    )
    .await;
    lay_pulse(
        writer,
        home,
        almanac,
        "index-places",
        &[27 * M, 21 * M, 14 * M, 9 * M, 4 * M],
    )
    .await;
    lay_pulse(
        writer,
        home,
        almanac,
        "s-3-scan",
        &every(3 * H - 2 * M, 2 * H + 50 * M, 50),
    )
    .await;
    // a-6 has said something three times lately: a tick each on its track
    for (body, at) in [
        ("Entries 1 to 20 drafted.", 60 * M),
        ("Entries 21 to 40 drafted.", 35 * M),
        (
            "Entries 41 to 58 drafted; the maps moved under maps/coast/.",
            10 * M,
        ),
    ] {
        let id = stored_messages::stored(
            writer,
            almanac,
            stored_messages::Stored {
                thread: "step-a-6-draft",
                from: "a-6-draft",
                to: Some("@lead"),
                body,
                ..Default::default()
            },
        )
        .await
        .id
        .0;
        writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql()
                    .execute("UPDATE messages SET at=?2 WHERE id=?1", (id, ago(at)))?;
                tx.changed(Some(almanac), "messages");
                Ok(())
            })
            .await
            .unwrap();
    }
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

/// `atlas`, a plan at scale for the scratch dashboard's screenshots: 250 units over three
/// recipes. `entry` (one step, 120 units) is healthy, so its group folds; `article` (80) has
/// two failures at their wall-clock cap and `scan` (50) a quiet run, so theirs stay open.
pub async fn seed_big(writer: &Writer, home: &Path) -> ProjectId {
    const M: u64 = 60;
    const H: u64 = 3600;
    const BIRDS: [&str; 12] = [
        "Dunlin",
        "Sanderling",
        "Knot",
        "Turnstone",
        "Curlew",
        "Godwit",
        "Redshank",
        "Greenshank",
        "Oystercatcher",
        "Avocet",
        "Plover",
        "Whimbrel",
    ];
    let atlas = project(
        writer,
        "atlas",
        "The coastal atlas: an entry for every species, a section for every coast, and the scans that check them.",
    )
    .await;
    let mut steps = serde_json::Map::new();
    let mut states: Vec<Step> = vec![];
    // entry: 95 done over the last eleven hours, 25 running, each heard from lately
    for n in 1..=120u64 {
        let unit = format!("e-{n:03}");
        let topic = format!("{} entry {n}", BIRDS[n as usize % BIRDS.len()]);
        steps.insert(
            format!("{unit}-check"),
            json!({"run": "custom.open", "tags": [format!("unit:{unit}")], "in": {"species": {"default": topic}}}),
        );
        states.push(if n <= 95 {
            step(
                &format!("{unit}-check"),
                "succeeded",
                &[((n * 7 + 20) * M, Some(6 * M))],
            )
        } else {
            step(
                &format!("{unit}-check"),
                "running",
                &[((n % 30 + 4) * M, None)],
            )
        });
    }
    // article: 55 done, 22 drafting, 2 failed at review, 1 reviewing
    for n in 1..=80u64 {
        let unit = format!("b-{n:02}");
        let tags = json!([format!("unit:{unit}")]);
        let topic = format!("{}: the coast section {n}", BIRDS[n as usize % BIRDS.len()]);
        steps.insert(
            format!("{unit}-draft"),
            json!({"run": "custom.open", "tags": tags, "in": {"topic": {"default": topic}}}),
        );
        steps.insert(
            format!("{unit}-review"),
            json!({"run": "custom.open", "tags": tags, "after": [format!("{unit}-draft")]}),
        );
        steps.insert(
            format!("{unit}-publish"),
            json!({"run": "custom.open", "tags": tags, "after": [format!("{unit}-review")]}),
        );
        let at = (n * 11 + 30) * M;
        match n {
            1..=55 => states.extend([
                step(
                    &format!("{unit}-draft"),
                    "succeeded",
                    &[(at + 50 * M, Some(30 * M))],
                ),
                step(
                    &format!("{unit}-review"),
                    "succeeded",
                    &[(at + 15 * M, Some(10 * M))],
                ),
                step(
                    &format!("{unit}-publish"),
                    "succeeded",
                    &[(at, Some(2 * M))],
                ),
            ]),
            56..=77 => states.push(step(
                &format!("{unit}-draft"),
                "running",
                &[((n % 25 + 3) * M, None)],
            )),
            78 | 79 => states.extend([
                step(
                    &format!("{unit}-draft"),
                    "succeeded",
                    &[(3 * H, Some(40 * M))],
                ),
                Step {
                    error: Some(PublicError::AgentFailure {
                        retry_at: None,
                        kind: "WallCap".into(),
                        message: "The run reached its wall-clock cap.".into(),
                        session: None,
                    }),
                    ..step(
                        &format!("{unit}-review"),
                        "failed",
                        &[(2 * H, Some(70 * M))],
                    )
                },
            ]),
            _ => states.extend([
                step(&format!("{unit}-draft"), "succeeded", &[(H, Some(35 * M))]),
                step(&format!("{unit}-review"), "running", &[(20 * M, None)]),
            ]),
        }
    }
    // scan: 42 done, 7 running, 1 quiet for three hours
    for n in 1..=50u64 {
        let unit = format!("c-{n:02}");
        let source = format!(
            "the {} coast photographs, roll {n}",
            BIRDS[n as usize % BIRDS.len()].to_lowercase()
        );
        steps.insert(
            format!("{unit}-scan"),
            json!({"run": "custom.open", "tags": [format!("unit:{unit}")], "in": {"source": {"default": source}}}),
        );
        states.push(match n {
            1..=42 => step(
                &format!("{unit}-scan"),
                "succeeded",
                &[((n * 13 + 25) * M, Some(9 * M))],
            ),
            43..=49 => step(
                &format!("{unit}-scan"),
                "running",
                &[((n % 20 + 2) * M, None)],
            ),
            _ => step(&format!("{unit}-scan"), "running", &[(3 * H, None)]),
        });
    }
    plan(writer, atlas, json!({"steps": steps}), states).await;
    // every running agent's actions, some busy and some slow, as their transcripts say
    for n in 96..=120u64 {
        let gap = [25, 40, 90, 240][n as usize % 4];
        lay_pulse(
            writer,
            home,
            atlas,
            &format!("e-{n:03}-check"),
            &every((n % 30 + 3) * M, (n % 7) * 20 + 10, gap),
        )
        .await;
    }
    for n in 56..=77u64 {
        let gap = [35, 70, 180][n as usize % 3];
        lay_pulse(
            writer,
            home,
            atlas,
            &format!("b-{n:02}-draft"),
            &every((n % 25 + 2) * M, (n % 5) * 60 + 20, gap),
        )
        .await;
    }
    // the latest entries to finish each said what they did
    for n in 1..=4u64 {
        let step: &'static str = Box::leak(format!("e-{n:03}-check").into_boxed_str());
        let thread: &'static str = Box::leak(format!("step-{step}").into_boxed_str());
        stored_messages::stored(
            writer,
            atlas,
            stored_messages::Stored {
                thread,
                from: step,
                to: Some("@lead"),
                body: "Checked against the regional list: range, season and photograph credit all match.",
                ..Default::default()
            },
        )
        .await;
    }
    let recipes = home
        .join("projects")
        .join(atlas.to_string())
        .join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    let entry = json!({"name": "entry", "params": {"species": "string"}, "title": "{species}",
        "steps": {"{unit}-check": {"run": "custom.open", "in": {"species": {"default": "{species}"}}}}});
    for (name, doc) in [
        ("entry", entry),
        ("article", article_recipe()),
        ("scan", scan_recipe()),
    ] {
        std::fs::write(
            recipes.join(format!("{name}.json")),
            serde_json::to_vec(&doc).unwrap(),
        )
        .unwrap();
    }
    atlas
}
