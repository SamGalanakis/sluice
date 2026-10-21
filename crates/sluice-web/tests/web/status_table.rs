//! One status table (`sluice_model::shown`, SPEC §13): every surface names, draws and ranks a
//! step's state from it. A project holds a step in each state; each is checked on its card, its
//! step page, the board's StepStatus, Units and Count, the summary line and bar, and the index.
use crate::board_fixture;
use crate::plan_html;
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
            let parked = run(tx, id, "s-parked", &recent, false)?;
            tx.sql().execute("UPDATE attempts SET request=json_object('park',json(?2)) WHERE attempt_id=(SELECT attempt_id FROM runs WHERE run_id=?1)", (parked.to_string(), json!({"condition":{"kind":"file","path":"/tmp/ruling","marker":null},"timeout_seconds":3600,"lease":null,"phase":"parked","requested_at":recent,"parked_at":recent}).to_string()))?;
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
    program.push_str("])\nunits = Units([\"parked\", \"running\", \"failed\", \"settled\", \"blocked\", \"queued\", \"pending\"])\n");
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

fn mark(state: Shown) -> String {
    sluice_web::views::ui::mark(state).as_str().to_owned()
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
    let html = f.opened(&format!("/projects/id/{id}")).await;
    // the plan: a waiting unit held by the pause is a row whose stage says so
    let row = plan_html::row(&html, "l3");
    let stage = plan_html::stage(row, "l3-fork");
    assert!(
        stage.contains("data-state=\"paused\"")
            && stage.contains("<span class=\"st-w\">paused</span>"),
        "{stage}"
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
/// is one stop in Needs you (its quiet step a silence of its own there), never a row at work,
/// and the head's sentence counts it.
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
    assert!(!plan_html::has_region(&html, "u-l2"), "{html}");
    let stops: Vec<&str> = plan_html::needs(&html)
        .into_iter()
        .filter(|i| i.contains("data-unit=\"l2\""))
        .collect();
    assert_eq!(stops.len(), 2, "{stops:?}");
    assert!(
        plan_html::kind(stops[0]) == "fail" && plan_html::kind(stops[1]) == "quiet",
        "{stops:?}"
    );
    assert!(
        plan_html::summary(&html).contains("1 failed, 1 quiet."),
        "{html}"
    );
}

#[tokio::test]
async fn states_priorities_cancellation_and_skipped_units() {
    let f = Fixture::new().await;
    {
        let id = states(&f).await;
        let html = f.opened(&format!("/projects/id/{id}")).await;
        let plan = between(&html, "<div id=\"plan-pane\"", "<aside id=\"board-pane\"");
        let (_, index) = f.get(&format!("/projects/id/{id}?show=done")).await;
        let pane = between(&html, "<aside id=\"board-pane\"", "</aside>");
        for state in Shown::ALL {
            let step = step_of(state);
            // on the plan: a stopped one its item in Needs you, its word; one at work or begun
            // its row, its mark and word; a done one the index's line with its glyph; each one's
            // steps its mark and word
            match state.spec().band {
                sluice_model::shown::Band::Done => {
                    let line = between(
                        plan_html::band(&index, "plan-done"),
                        &format!("/{step}\"><span class=\"pl-at\">"),
                        "</li>",
                    );
                    let item = &index[..index.find(line).unwrap() + line.len()];
                    let item = &item[item.rfind("<li ").unwrap()..];
                    assert!(item.contains(&glyph(state)), "{state:?}: {item}");
                }
                _ if matches!(state, Shown::Failed | Shown::Cancelled | Shown::Stale) => {
                    let module = plan_html::stopped(plan, &step);
                    assert!(
                        plan_html::kind(module) != "quiet"
                            && module.contains(&format!("<span class=\"nd-s\">{}", state.word())),
                        "{state:?}: {module}"
                    );
                }
                _ => {
                    if plan_html::has_region(plan, &format!("u-{step}")) {
                        // its row's stage says its state, in its own words where the stage
                        // has them
                        let row = plan_html::row(plan, &step);
                        let stage = plan_html::stage(row, &step);
                        assert!(
                            stage.contains(&format!("data-state=\"{}\"", state.key())),
                            "{state:?}: {stage}"
                        );
                    }
                    let (_, unit) = f.get(&format!("/projects/id/{id}/units/{step}")).await;
                    let cell = plan_html::cell(&unit, &step);
                    assert!(
                        cell.contains(&format!("aria-label=\"{}\"", state.word())),
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
            // its own page: the status line's word is the state's, never two readings
            let (status, page) = f.get(&format!("/projects/id/{id}/steps/{step}")).await;
            assert_eq!(status, StatusCode::OK, "{page}");
            let line = between(&page, "<p class=\"status-line\">", "</p>");
            // quiet is a running step's: its sand pill says so after "Running"
            let word = match state {
                Shown::Quiet => "running",
                _ => state.word(),
            };
            assert!(
                line.contains(&format!(
                    "<b>{}{}</b>",
                    word[..1].to_uppercase(),
                    &word[1..]
                )) && (state != Shown::Quiet || line.contains("<span class=\"badge look\">")),
                "{state:?}: {line}"
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
            "1 failed, 1 cancelled, 1 stale, 1 quiet.",
            // a loose step that has run for days is watched, not at work
            "5 at work and 1 long run watched;",
            "3 of 17 done · 17 steps.",
        ] {
            assert!(sum.contains(said), "{said}: {sum}");
        }
        // its tab counts what needs a look, its icon the sand square
        assert!(
            html.contains(
                "data-page-title=\"⚠4 · states · sluice\" data-favicon=\"/static/favicon-look.svg"
            ),
            "{html}"
        );
        // the index: the same counts from the store (the plan's blocked, held, queued and outside
        // steps there are pending), the running steps by how each reads
        let (_, home) = f.get("/").await;
        // home's card: its plan's sentence says every state that needs a look, its groups
        // what runs, the quiet one named
        let card = between(&home, "aria-label=\"states\"", "</article>");
        assert!(
            card.contains(
                "<p class=\"pm-summary\">1 failed, 1 cancelled, 1 stale, 1 quiet. 5 at work and 1 long run watched;"
            ) && card.contains("<li><b>Watching</b> · 1 quiet<span class=\"pm-g-said\"><a href=")
                && card.contains("/steps/s-quiet\">s-quiet</a>"),
            "{card}"
        );
        // the tab title counts what needs attention (the fixture's lanes has a failure too)
        assert!(
            home.contains(
                "data-page-title=\"2 failed · 1 cancelled · 1 stale · 1 quiet · Projects · sluice\""
            ),
            "{home}"
        );
    }
    {
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
        // in Needs you, not at work
        assert_eq!(plan_html::place(&html, "m"), "stopped", "{html}");
        assert_eq!(plan_html::place(&html, "solo"), "running", "{html}");
        assert!(!plan_html::has_region(&html, "u-m"), "{html}");
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
    {
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
        // blocked and never begun, the plan counts them; found, each row says what it waits on
        // and how that reads: cancelled, never failed
        assert_eq!(plan_html::place(&html, "gated"), "", "{html}");
        let found = f.opened(&format!("/projects/id/{id}?q=gated")).await;
        let gated = plan_html::row(&found, "gated");
        assert!(gated.contains(">up</a> (cancelled)</span>"), "{gated}");
        assert!(!gated.contains("(failed)"), "{gated}");
        let (_, found) = f.get(&format!("/projects/id/{id}?q=reads")).await;
        let reads = plan_html::row(&found, "reads");
        assert!(reads.contains(">up</a> (cancelled)</span>"), "{reads}");
        // a blocked step's stage on its row says it is blocked
        let stage = plan_html::stage(gated, "gated");
        assert!(
            stage.contains("data-state=\"blocked\"")
                && stage.contains("<span class=\"st-w\">blocked</span>"),
            "{stage}"
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
    {
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
        let (_, html) = f.get(&format!("/projects/id/{id}?show=done")).await;
        let done = plan_html::band(&html, "plan-done");
        let at = done.find("/units/k\"><span class=\"pl-at\">").unwrap();
        let line = &done[done[..at].rfind("<li ").unwrap()..];
        let line = &line[..line.find("</li>").unwrap()];
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
}

#[test]
fn stored_cancel_spellings_have_literal_error_filter_verdicts() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    let sql = format!(
        "SELECT {} FROM (SELECT ?1 AS payload)",
        sluice_model::shown::cancel_sql("payload", "$.error")
    );
    for (stored, expected) in [
        (
            r#"{"error":"cancelled","message":"cancel requested"}"#,
            true,
        ),
        (
            r#"{"error":"agent_failure","kind":"Cancelled","message":"x"}"#,
            true,
        ),
        (
            r#"{"error":"agent_failure","kind":"WallCap","message":"x"}"#,
            false,
        ),
        (r#"{"error":"agent_failure","message":"x"}"#, false),
        (
            r#"{"error":"fn_failure","message":"cancelled: not needed"}"#,
            true,
        ),
        (r#"{"error":"fn_failure","message":"cancelled"}"#, true),
        (
            r#"{"error":"fn_failure","message":"Cancelled: shouting"}"#,
            false,
        ),
        (r#"{"error":"fn_failure","message":"cancelledx"}"#, false),
        (r#"{"error":"bad_request","message":"cancelled"}"#, false),
        (r#""cancelled""#, true),
        (r#""cancelled: by hand""#, true),
        (r#""CANCELLED: by hand""#, false),
        (r#""tests failed""#, false),
        ("null", false),
    ] {
        let payload = format!(r#"{{"error":{stored}}}"#);
        let sql_says: bool = c.query_row(&sql, [&payload], |r| r.get(0)).unwrap();
        assert_eq!(sql_says, expected, "SQL: {stored}");
        let value: serde_json::Value = serde_json::from_str(stored).unwrap();
        let rust_says = match value {
            serde_json::Value::String(bare) => sluice_model::shown::stored_is_cancel(&bare),
            serde_json::Value::Null => false,
            object => sluice_model::shown::stored_is_cancel(&object.to_string()),
        };
        assert_eq!(rust_says, expected, "stored decoder: {stored}");
    }
}
