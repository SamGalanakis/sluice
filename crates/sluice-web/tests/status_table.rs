//! One status table (`sluice_model::shown`, SPEC §13): every surface names, draws and ranks a
//! step's state from it. A project holds a step in each state; each is checked on its card, its
//! step page, the board's StepStatus, Units and Count, the summary line and bar, and the index.
mod board_fixture;
mod plan_html;
use axum::http::StatusCode;
use board_fixture::Fixture;
use serde_json::json;
use sluice_model::{
    error::PublicError,
    ids::{AttemptId, ProjectId, ProjectSelector, Revision, RunId},
    shown::Shown,
};
use sluice_store::{RetrySafety, projects};

fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from} in {html}"));
    let end = html[start..].find(to).map_or(html.len(), |e| start + e);
    &html[start..end]
}

/// The step each state is drawn for.
fn step_of(state: Shown) -> String {
    format!("s-{}", state.key())
}

/// A glyph as every page draws one for `state`: its icon, named by its word.
fn glyph(state: Shown) -> String {
    format!(
        "<span class=\"g g-{}\" role=\"img\" aria-label=\"{}\">",
        state.key(),
        state.word()
    )
}

fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    sluice_web::views::rfc3339(secs)
}

/// A run of `step`, started at `started`, its attempt asked to stop when `cancel`; the step's
/// current run.
fn run(
    tx: &mut sluice_store::WriteTransaction<'_>,
    project: ProjectId,
    step: &str,
    started: &str,
    cancel: bool,
) -> rusqlite::Result<RunId> {
    let (attempt, run) = (AttemptId::new(), RunId::new());
    tx.sql().execute(
        "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at,cancel_requested) VALUES (?1,?2,?3,1,1,'executing','{}','hash',?4,?5)",
        (attempt.to_string(), project.to_string(), step, started, cancel),
    )?;
    tx.sql().execute(
        "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at) VALUES (?1,?2,?3,?4,1,1,?5,?5)",
        (run.to_string(), project.to_string(), attempt.to_string(), step, started),
    )?;
    tx.sql().execute(
        "UPDATE steps SET status='running',run_ids=?3 WHERE project_id=?1 AND step_id=?2",
        (project.to_string(), step, json!([run]).to_string()),
    )?;
    Ok(run)
}

/// Set `project`'s board program.
async fn board(f: &Fixture, project: ProjectId, program: String) {
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::board_set(
                tx,
                &ProjectSelector::Id(project),
                projects::SetBoard {
                    program: Some(program),
                    expected_rev: Some(Revision(0)),
                    reason: None,
                    author: "orch".into(),
                },
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// A project `states` with one step (its own unit) in each state of the table, and a board
/// that draws each through Units, StepStatus and Count.
async fn states(f: &Fixture) -> ProjectId {
    let mut steps = serde_json::Map::new();
    for state in Shown::ALL {
        let mut step = json!({"run": "custom.open"});
        match state {
            Shown::Blocked => step["after"] = json!([step_of(Shown::Failed)]),
            Shown::External => step["run"] = json!("core.external"),
            Shown::Paused => step["paused"] = json!(true),
            Shown::Held => step["in"] = json!({"b": {"source": "brief"}}),
            Shown::Queued => step["needs"] = json!({"lane": 1}),
            Shown::Pending => step["after"] = json!([step_of(Shown::Running)]),
            _ => {}
        }
        steps.insert(step_of(state), step);
    }
    let id = f
        .project(
            "states",
            json!({"inputs": {"brief": "string"}, "steps": steps}),
            &[
                ("s-failed", "failed"),
                ("s-cancelled", "failed"),
                ("s-stale", "stale"),
                ("s-manual", "succeeded"),
                ("s-succeeded", "succeeded"),
                ("s-skipped", "skipped"),
            ],
        )
        .await;
    let recent = now();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let p = id.to_string();
            let error = |e: PublicError| serde_json::to_string(&e).unwrap();
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='s-failed'",
                (&p, error(PublicError::BadRequest { message: "tests failed".into() })),
            )?;
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='s-cancelled'",
                (&p, error(PublicError::Cancelled { message: "not needed".into() })),
            )?;
            tx.sql().execute(
                "UPDATE steps SET manual=1 WHERE project_id=?1 AND step_id='s-manual'",
                [&p],
            )?;
            // a run that has written nothing since January: quiet
            run(tx, id, "s-quiet", "2026-01-01T00:00:00Z", false)?;
            run(tx, id, "s-stopping", &recent, true)?;
            run(tx, id, "s-running", &recent, false)?;
            let finishing = run(tx, id, "s-finishing", &recent, false)?;
            tx.sql().execute(
                "INSERT INTO submissions(run_id,project_id,step_id,outputs,at) VALUES(?1,?2,'s-finishing','{}',?3)",
                (finishing.to_string(), &p, &recent),
            )?;
            tx.changed(Some(id), "project");
            Ok(())
        })
        .await
        .unwrap();
    let mut program = String::from("root = Stack([units");
    for (i, _) in Shown::ALL.iter().enumerate() {
        program.push_str(&format!(", st{i}, n{i}"));
    }
    program.push_str("])\nunits = Units([\"running\", \"failed\", \"settled\", \"blocked\", \"queued\", \"pending\"])\n");
    for (i, state) in Shown::ALL.iter().enumerate() {
        program.push_str(&format!(
            "st{i} = StepStatus(\"{}\")\nn{i} = Count(\"{} steps\", \"{}\")\n",
            step_of(*state),
            state.key(),
            state.key()
        ));
    }
    board(f, id, program).await;
    id
}

#[tokio::test]
async fn every_state_reads_the_same_on_every_surface() {
    let f = Fixture::new().await;
    let id = states(&f).await;
    let (status, html) = f.get(&format!("/projects/id/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let plan = between(&html, "<div id=\"plan-pane\"", "<aside id=\"board-pane\"");
    let pane = between(&html, "<aside id=\"board-pane\"", "</aside>");
    for state in Shown::ALL {
        let step = step_of(state);
        // on the plan, by its band: a stopped one its module on the sand, its glyph and word; a
        // running or waiting one its cell, its state and its word; a done one the index's line
        // with its glyph
        match state.spec().band {
            sluice_model::shown::Band::Done => {
                let line = between(
                    plan_html::band(plan, "plan-done"),
                    &format!("<code class=\"pl-did\">{step}</code>"),
                    "</li>",
                );
                let item = &plan[..plan.find(line).unwrap() + line.len()];
                let item = &item[item.rfind("<li ").unwrap()..];
                assert!(item.contains(&glyph(state)), "{state:?}: {item}");
            }
            _ if matches!(state, Shown::Failed | Shown::Cancelled | Shown::Stale) => {
                let module = plan_html::stopped(plan, &step);
                assert!(
                    module.contains(&format!("{}<b>{}</b>", mark(state), state.word())),
                    "{state:?}: {module}"
                );
            }
            _ => {
                let cell = plan_html::cell(plan, &step);
                assert!(
                    cell.contains(&format!("data-state=\"{}\"", state.key()))
                        && cell.contains(&format!(": {}</span>", state.word())),
                    "{state:?}: {cell}"
                );
            }
        }
        // the board's StepStatus: the same class, glyph and word
        let chip = between(pane, &format!("/steps/{step}\">"), "</div>");
        assert!(chip.contains(&glyph(state)), "{state:?}: {chip}");
        // the board's Units: the unit reads as its one step does, its mark the table's
        let row = between(pane, &format!("/units/{step}\">{step}</a>"), "</tr>");
        assert!(
            row.contains(&format!("<span>{}</span>", state.word())),
            "{state:?}: {row}"
        );
        assert!(
            row.contains(&format!("<span class=\"stg\">{}", glyph(state)))
                && row.contains(&format!("</span>{step}</span>")),
            "{state:?}: {row}"
        );
        // Count counts it by its key
        assert!(
            pane.contains(&format!(
                "<span class=\"metric-v\">1</span><span class=\"metric-l\">{} steps</span>",
                state.key()
            )),
            "{state:?}: {pane}"
        );
        // its own page: the badge's glyph and word are the state's, never two readings
        let (status, page) = f.get(&format!("/projects/id/{id}/steps/{step}")).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let badge = between(&page, "<p class=\"d-badges\">", "</p>");
        assert!(
            badge.contains(&format!("g g-{}", state.key())) && badge.contains(state.word()),
            "{state:?}: {badge}"
        );
        // a cancel asked for is not offered again
        if state == Shown::Stopping {
            assert!(!page.contains("<summary>Cancel</summary>"), "{page}");
        }
        if state == Shown::Running {
            assert!(page.contains("<summary>Cancel</summary>"), "{page}");
        }
    }
    // the band's summary sentence: each unit (a loose step is one) counted once by how it reads,
    // a step a failure holds up as waiting
    let sum = plan_html::summary(&html);
    for said in [
        "1 failed, 1 cancelled, 1 stale.",
        "5 units at work: s-quiet quiet for ",
        "5 waiting.",
        "3 of 16 units done",
    ] {
        assert!(sum.contains(said), "{said}: {sum}");
    }
    // the project reads as its first state: its tab says what needs a look first
    assert!(
        html.contains(
            "data-page-title=\"1 failed · 1 cancelled · 1 stale · 1 quiet · states · sluice\""
        ),
        "{html}"
    );
    // the index: the same counts from the store (the plan's blocked, held, queued and outside
    // steps there are pending), the running steps by how each reads
    let (_, home) = f.get("/").await;
    // home's module: its squares say every state that needs a look, its rows each running one
    let row = between(&home, "aria-label=\"states\"", "</article>");
    let squares = between(row, "<p class=\"pm-squares\"", ">");
    for state in [Shown::Failed, Shown::Cancelled, Shown::Stale, Shown::Quiet] {
        assert!(
            squares.contains(&format!(" {}", state.word())),
            "{state:?}: {squares}"
        );
    }
    for state in [
        Shown::Quiet,
        Shown::Stopping,
        Shown::Finishing,
        Shown::Running,
    ] {
        assert!(row.contains(&glyph(state)), "{state:?}: {row}");
    }
    assert!(
        row.contains("<span class=\"pm-word\">stopping</span> · running for"),
        "{row}"
    );
    // the tab title counts what needs attention (the fixture's lanes has a failure too)
    assert!(
        home.contains(
            "data-page-title=\"2 failed · 1 cancelled · 1 stale · 1 quiet · Projects · sluice\""
        ),
        "{home}"
    );
}

/// A unit with a failed step and a running one reads failed: on the board's Units, in the
/// band Live first draws it under (Stopped), and the project that holds it; stale and cancelled
/// rank above running on the index; the board's Live order ranks a quiet run as attention.
#[tokio::test]
async fn a_failure_ranks_before_running_work_on_every_surface() {
    let f = Fixture::new().await;
    let mixed = f
        .project(
            "mixed",
            json!({"steps":{
                "m-a":{"run":"custom.open","tags":["unit:m"]},
                "m-b":{"run":"custom.open","tags":["unit:m"]},
                "solo":{"run":"custom.open"}}}),
            &[("m-a", "failed")],
        )
        .await;
    let recent = now();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            run(tx, mixed, "m-b", &recent, false)?;
            run(tx, mixed, "solo", &recent, false)?;
            tx.changed(Some(mixed), "project");
            Ok(())
        })
        .await
        .unwrap();
    board(&f, mixed, "root = Units([\"failed\"])".into()).await;
    let (_, html) = f.get(&format!("/projects/id/{mixed}")).await;
    // under Stopped, not Running, which comes after it
    assert_eq!(plan_html::place(&html, "m"), "stopped", "{html}");
    assert_eq!(plan_html::place(&html, "solo"), "running", "{html}");
    assert!(
        html.find("<!--r:s-m-->").unwrap() < html.find("<!--r:plan-running-->").unwrap(),
        "{html}"
    );
    // the Units table names it failed, and its filter finds it as failed
    let pane = between(&html, "<aside id=\"board-pane\"", "</aside>");
    let row = between(pane, "/units/m\">m</a>", "</tr>");
    assert!(
        row.contains(&format!("{}<span>failed</span>", mark(Shown::Failed))),
        "{row}"
    );
    // the index: a stale project and a cancelled one before a running one
    let stale = f
        .project(
            "aaa-stale",
            json!({"steps":{"old":{"run":"custom.open"},"go2":{"run":"custom.open"}}}),
            &[("old", "stale")],
        )
        .await;
    let calm = f
        .project(
            "aab-calm",
            json!({"steps":{"go":{"run":"custom.open"}}}),
            &[],
        )
        .await;
    let recent = now();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            run(tx, calm, "go", &recent, false)?;
            run(tx, stale, "go2", &recent, false)?;
            tx.changed(Some(stale), "project");
            Ok(())
        })
        .await
        .unwrap();
    let (_, page) = f.get("/").await;
    // the index's rows (the nav's switcher lists them in the same order)
    let home = between(&page, "<div class=\"proj-grid\">", "</section>");
    let at = |name: &str| home.find(&format!("aria-label=\"{name}\"")).unwrap();
    assert!(at("mixed") < at("aaa-stale"), "{home}");
    assert!(at("aaa-stale") < at("aab-calm"), "{home}");
    let head = between(home, "aria-label=\"aaa-stale\"", "</p>");
    assert!(head.contains(&glyph(Shown::Stale)), "{head}");
}

fn mark(state: Shown) -> String {
    sluice_web::views::ui::mark(state).as_str().to_owned()
}

/// A cancelled step is named cancelled wherever a step waiting on it says why: its gate's
/// glyph and the gates' count on the step page, a handoff's wait, the card's description and
/// the board's StepStatus; a blocked step's caption says a cancel blocks it too.
#[tokio::test]
async fn a_cancel_reads_cancelled_in_every_wait_and_gate() {
    let f = Fixture::new().await;
    let mut steps = serde_json::Map::new();
    steps.insert(
        "up".into(),
        json!({"run":"custom.open","outputs":{"summary":"string"}}),
    );
    for i in 0..4 {
        steps.insert(format!("ok{i}"), json!({"run":"custom.open"}));
    }
    steps.insert(
        "gated".into(),
        json!({"run":"custom.open","after":["up","ok0","ok1","ok2","ok3"]}),
    );
    steps.insert(
        "reads".into(),
        json!({"run":"custom.open","in":{"s":{"source":"up/summary"}}}),
    );
    let id = f
        .project(
            "cancels",
            json!({ "steps": steps }),
            &[
                ("up", "failed"),
                ("ok0", "succeeded"),
                ("ok1", "succeeded"),
                ("ok2", "succeeded"),
                ("ok3", "succeeded"),
            ],
        )
        .await;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='up'",
                (
                    id.to_string(),
                    serde_json::to_string(&PublicError::Cancelled {
                        message: "pivot".into(),
                    })
                    .unwrap(),
                ),
            )?;
            tx.changed(Some(id), "project");
            Ok(())
        })
        .await
        .unwrap();
    board(
        &f,
        id,
        "root = Stack([a, b])\na = StepStatus(\"gated\")\nb = StepStatus(\"reads\")".into(),
    )
    .await;
    let (_, html) = f.get(&format!("/projects/id/{id}")).await;
    // each row says what it waits on and how that reads: cancelled, never failed
    let gated = plan_html::row(&html, "gated");
    assert!(
        gated.contains("<code>up</code></a> (cancelled)."),
        "{gated}"
    );
    assert!(!gated.contains("(failed)"), "{gated}");
    let reads = plan_html::row(&html, "reads");
    assert!(
        reads.contains("<code>up</code></a> (cancelled)."),
        "{reads}"
    );
    // a blocked step's cell says it is blocked
    let cell = plan_html::cell(&html, "gated");
    assert!(
        cell.contains("data-state=\"blocked\"") && cell.contains("blocked</span>"),
        "{cell}"
    );
    let pane = between(&html, "<aside id=\"board-pane\"", "</aside>");
    assert!(
        pane.contains("<p class=\"meta board-why\">step up is cancelled</p>"),
        "{pane}"
    );
    assert!(!pane.contains("failed"), "{pane}");
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/gated")).await;
    assert!(
        page.contains("<span>5 steps: 4 done, 1 cancelled</span>"),
        "{page}"
    );
    assert!(
        page.contains(&format!("<span class=\"gate\">{}", glyph(Shown::Cancelled))),
        "{page}"
    );
    // its page names the step it takes from by name and link, how it reads after it
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/reads")).await;
    let wait = between(&page, "<p class=\"gate wait-step\">", "</p>");
    assert!(wait.contains("aria-label=\"cancelled\""), "{wait}");
    assert!(wait.contains("/steps/up\"><code>up</code></a>"), "{wait}");
    assert!(
        wait.ends_with(" <span class=\"meta\">(cancelled)</span>"),
        "{wait}"
    );
    assert!(
        !page.contains("<p>step up is cancelled</p>"),
        "said once: {page}"
    );
}

/// "Blocked" is one thing, a step behind a failure; a pause (the step's or its project's) reads
/// "paused" and ready work outside sluice "outside", in the board's Units and in the lane
/// marks too.
#[tokio::test]
async fn a_project_pause_and_outside_work_read_as_themselves() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let outside = f
        .project(
            "outside",
            json!({"steps":{"x-wait":{"run":"core.external","tags":["unit:x"]},
                "x-after":{"run":"custom.open","tags":["unit:x"],"after":["x-wait"]}}}),
            &[],
        )
        .await;
    board(&f, outside, "root = Units()".into()).await;
    let (_, html) = f.get(&format!("/projects/id/{outside}")).await;
    let pane = between(&html, "<aside id=\"board-pane\"", "</aside>");
    let row = between(pane, "/units/x\">x</a>", "</tr>");
    assert!(row.contains("<span>outside</span>"), "{row}");
    assert!(
        row.contains(&format!("<span class=\"stg\">{}", glyph(Shown::External)))
            && row.contains("</span>wait</span>"),
        "{row}"
    );
    let key = between(pane, "u-key", "</p>");
    assert!(
        key.contains("g-external") && key.contains("</span>outside</span>"),
        "the key: {key}"
    );
    // the titled project paused: a matrix's lane string marks each pending stage paused
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE projects SET paused=1 WHERE project_id=?1",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "project");
            Ok(())
        })
        .await
        .unwrap();
    board(&f, id, "root = Units()".into()).await;
    let (_, html) = f.get(&format!("/projects/id/{id}")).await;
    // the plan: a waiting unit's first stage says it is paused, in its cell
    let cell = plan_html::cell(&html, "l3-fork");
    assert!(
        cell.contains("data-state=\"paused\"")
            && cell.contains(&format!("{}paused</span>", mark(Shown::Paused))),
        "{cell}"
    );
    let pane = between(&html, "<aside id=\"board-pane\"", "</aside>");
    let row = between(pane, "/units/l3\">l3</a>", "</tr>");
    assert!(
        row.contains(&format!("{}<span>paused</span>", mark(Shown::Paused))),
        "{row}"
    );
    assert!(
        row.contains(&format!("<span class=\"stg\">{}", glyph(Shown::Paused)))
            && row.contains("</span>fork</span>"),
        "{row}"
    );
}

/// The plan counts each unit once, under how it reads: a unit with a failed step and a quiet one
/// is one failed module under Stopped, never also a quiet row under Running, and the band's
/// sentence counts it failed.
#[tokio::test]
async fn a_unit_with_a_failed_and_a_quiet_step_counts_once() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    // l2 has a failed work; its land runs quiet (so l3, after it, only waits)
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            run(tx, id, "l2-land", "2026-01-01T00:00:00Z", false)?;
            tx.changed(Some(id), "project");
            Ok(())
        })
        .await
        .unwrap();
    let (status, html) = f.get(&format!("/projects/id/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert_eq!(plan_html::place(&html, "l2"), "stopped", "{html}");
    assert!(
        !plan_html::band(&html, "plan-running").contains("data-unit=\"l2\""),
        "{html}"
    );
    let head = between(
        plan_html::band(&html, "plan-stopped"),
        "<p class=\"sec-n\">",
        "</p>",
    );
    assert_eq!(head, "<p class=\"sec-n\">1 failed");
    // its stages' marks still say the quiet one needs a look
    let module = plan_html::stopped(&html, "l2");
    assert!(module.contains("land quiet"), "{module}");
    assert!(plan_html::summary(&html).contains("1 failed."), "{html}");
}

/// A done unit reads as its steps do (all skipped: skipped), and a unit's own page always
/// draws its glyph, done or not.
#[tokio::test]
async fn a_unit_reads_by_its_steps_on_the_shelf_and_its_page() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "shelf",
            json!({"steps":{
                "k-a":{"run":"custom.open","tags":["unit:k"]},
                "k-b":{"run":"custom.open","tags":["unit:k"]},
                "w-a":{"run":"custom.open","tags":["unit:w"]},
                "w-b":{"run":"custom.open","tags":["unit:w"],"after":["w-a"]}}}),
            &[("k-a", "skipped"), ("k-b", "skipped")],
        )
        .await;
    let (_, html) = f.get(&format!("/projects/id/{id}")).await;
    let done = plan_html::band(&html, "plan-done");
    let line = between(done, "<li ", "<code class=\"pl-did\">k</code>");
    let line = &line[line.rfind("<li ").unwrap()..];
    assert!(line.contains(&glyph(Shown::Skipped)), "{line}");
    let (_, page) = f.get(&format!("/projects/id/{id}/units/w")).await;
    assert!(
        between(&page, "<h1 class=\"unit-h\">", "</h1>").contains(&glyph(Shown::Pending)),
        "{page}"
    );
    let (_, page) = f.get(&format!("/projects/id/{id}/units/k")).await;
    assert!(
        between(&page, "<h1 class=\"unit-h\">", "</h1>").contains(&glyph(Shown::Skipped)),
        "{page}"
    );
}

/// The log's Errors filter reads a cancel by the same rule as every page: the SQL it runs
/// agrees with `is_cancel` on each stored form, a bare message from an older release too.
#[test]
fn the_logs_cancel_filter_and_the_pages_agree() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    let sql = format!(
        "SELECT {} FROM (SELECT ?1 AS payload)",
        sluice_model::shown::cancel_sql("payload", "$.error")
    );
    for stored in [
        r#"{"error":"cancelled","message":"cancel requested"}"#,
        r#"{"error":"agent_failure","kind":"Cancelled","message":"x"}"#,
        r#"{"error":"agent_failure","kind":"WallCap","message":"x"}"#,
        r#"{"error":"agent_failure","message":"x"}"#,
        r#"{"error":"fn_failure","message":"cancelled: not needed"}"#,
        r#"{"error":"fn_failure","message":"cancelled"}"#,
        r#"{"error":"fn_failure","message":"Cancelled: shouting"}"#,
        r#"{"error":"fn_failure","message":"cancelledx"}"#,
        r#"{"error":"bad_request","message":"cancelled"}"#,
        r#""cancelled""#,
        r#""cancelled: by hand""#,
        r#""CANCELLED: by hand""#,
        r#""tests failed""#,
        "null",
    ] {
        let payload = format!(r#"{{"error":{stored}}}"#);
        let sql_says: bool = c.query_row(&sql, [&payload], |r| r.get(0)).unwrap();
        let rust_says = match serde_json::from_str::<serde_json::Value>(stored).unwrap() {
            serde_json::Value::String(bare) => sluice_model::shown::stored_is_cancel(&bare),
            serde_json::Value::Null => false,
            object => sluice_model::shown::stored_is_cancel(&object.to_string()),
        };
        assert_eq!(sql_says, rust_says, "{stored}");
    }
}

/// Every state the table names is styled and documented: its glyph's colour, its bar segment
/// and card class in style.css, its icon in the sprite, and its word in DESIGN.md's ramp.
#[test]
fn every_state_is_styled_drawn_and_documented() {
    const CSS: &str = include_str!("../assets/style.css");
    const DESIGN: &str = include_str!("../../../DESIGN.md");
    let ramp = &DESIGN[DESIGN.find("### Status ramp").unwrap()..];
    let ramp = &ramp[..ramp.find("### Named Rules").unwrap()];
    for state in Shown::ALL {
        let key = state.key();
        for selector in [format!(".g-{key}"), format!(".b-{key}")] {
            assert!(
                CSS.split(|c: char| c == ',' || c == '{' || c.is_whitespace())
                    .any(|s| s == selector),
                "{selector} in style.css"
            );
        }
        assert!(
            CSS.contains(&format!(".node.is-{key}")) || CSS.contains(&format!(".is-{key} ")),
            ".node.is-{key} in style.css"
        );
        let drawn = sluice_web::views::ui::glyph(state);
        assert!(
            drawn
                .as_str()
                .contains(&format!("#i-{}", state.spec().icon)),
            "{state:?}: {}",
            drawn.as_str()
        );
        assert!(
            ramp.contains(&format!("**{}**", capital(state.word()))),
            "{state:?}'s word in DESIGN.md's status ramp"
        );
    }
}

fn capital(word: &str) -> String {
    word[..1].to_uppercase() + &word[1..]
}
