//! The third critique's fixes, each through the pages it changed: a cancelled run says who
//! stopped it in one line, and a rerun says how the run before ended and who retried it in
//! plain words; a unit's stages still to come are drawn empty under its recipe's columns, and a
//! unit that has not started says what holds it; sluice's housekeeping on the log is one quiet
//! row; a run's file is a page with its terminal codes out, the file itself a link away.
mod board_fixture;
mod plan_html;
mod seed;
use board_fixture::Fixture;
use serde_json::json;
use sluice_model::{
    events::Event,
    ids::{AttemptId, ProjectId, Revision, RunId, StepId, WorkGeneration},
};
use sluice_store::RetrySafety;

fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from} in {html}"));
    let end = html[start..].find(to).map_or(html.len(), |e| start + e);
    &html[start..end]
}

/// One run of `step` from `started`, ended `finished` with `result` (both none while it runs).
async fn run(
    f: &Fixture,
    project: ProjectId,
    step: &'static str,
    started: &'static str,
    finished: Option<&'static str>,
    result: Option<serde_json::Value>,
) -> RunId {
    let run = RunId::new();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let attempt = AttemptId::new();
            tx.sql().execute(
                "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?1,?2,?3,1,1,'terminal','{}','hash',?4)",
                (attempt.to_string(), project.to_string(), step, started),
            )?;
            tx.sql().execute(
                "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at,finished_at,result) VALUES (?1,?2,?3,?4,1,1,?5,?5,?6,?7)",
                (run.to_string(), project.to_string(), attempt.to_string(), step, started, finished, result.map(|r| r.to_string())),
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    run
}

/// A retry of `step` by `author`, recorded `at`.
async fn retried(
    f: &Fixture,
    project: ProjectId,
    step: &'static str,
    author: &'static str,
    reason: &'static str,
    at: &'static str,
) {
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let record = tx.append_record(
                Some(project),
                Event::StepRetry {
                    rev: Revision(1),
                    author: author.into(),
                    reason: reason.into(),
                    step: StepId::new(step).unwrap(),
                    work: WorkGeneration(2),
                },
            )?;
            tx.sql()
                .execute("UPDATE records SET at=?1 WHERE seq=?2", (at, record.seq.0))?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_cancelled_run_says_who_stopped_it_and_a_rerun_says_how_and_who_in_plain_words() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "reruns",
            json!({"steps":{
                "w":{"run":"custom.open"},
                "watch":{"run":"custom.open"},
                "land":{"run":"custom.open"}}}),
            &[("w", "running"), ("watch", "running"), ("land", "running")],
        )
        .await;
    // w: cli retried it while it ran, which cancelled its first run
    run(
        &f,
        id,
        "w",
        "2026-10-01T00:00:00Z",
        Some("2026-10-01T01:18:00Z"),
        Some(json!({"status":"failed","error":{"error":"cancelled","message":"cancel requested"}})),
    )
    .await;
    retried(
        &f,
        id,
        "w",
        "cli",
        "restart the watcher",
        "2026-10-01T01:18:00Z",
    )
    .await;
    run(&f, id, "w", "2026-10-01T01:19:00Z", None, None).await;
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/w?tab=runs")).await;
    let first = between(&page, "<li id=\"run-1\"", "</li>");
    assert!(
        first.contains(
            "<p class=\"a-err\">Cancelled after 1h 18m by cli, which retried it: restart the watcher.</p>"
        ),
        "{first}"
    );
    // the kind said again under it is gone
    assert!(!first.contains("<dt>Kind</dt>"), "{first}");
    let now = between(&page, "<p class=\"meta now-retry\">", "</p>");
    assert!(
        now.contains(
            ", after <a href=\"#run-1\">run 1</a> cancelled. cli retried it: restart the watcher."
        ),
        "{now}"
    );
    // watch: cli cancelled it with a reason, then retried it: the rerun says both, each once
    run(
        &f,
        id,
        "watch",
        "2026-10-01T00:00:00Z",
        Some("2026-10-01T00:30:00Z"),
        Some(json!({"status":"failed","error":{"error":"cancelled","message":"cancel requested"}})),
    )
    .await;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let record = tx.append_record(
                Some(id),
                Event::StepCancel {
                    step: StepId::new("watch").unwrap(),
                    author: "cli".into(),
                    reason: "switch to main_tests".into(),
                },
            )?;
            tx.sql().execute(
                "UPDATE records SET at='2026-10-01T00:30:00Z' WHERE seq=?1",
                [record.seq.0],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    retried(&f, id, "watch", "cli", "", "2026-10-01T00:31:00Z").await;
    run(&f, id, "watch", "2026-10-01T00:32:00Z", None, None).await;
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/watch")).await;
    let now = between(&page, "<p class=\"meta now-retry\">", "</p>");
    assert!(
        now.contains(", after <a href=\"#run-1\">run 1</a> cancelled by cli: switch to main_tests. cli retried it."),
        "{now}"
    );
    // land: its own completion action sent it round again after its fn failed; said plainly,
    // never sluice's own wording for it
    run(
        &f,
        id,
        "land",
        "2026-10-01T00:00:00Z",
        Some("2026-10-01T00:05:00Z"),
        Some(json!({"status":"failed","error":{"error":"fn_failure","message":"kiln clippy failed after the rebase"}})),
    )
    .await;
    retried(
        &f,
        id,
        "land",
        "land",
        "conditional Rejected retry",
        "2026-10-01T00:05:00Z",
    )
    .await;
    run(&f, id, "land", "2026-10-01T00:06:00Z", None, None).await;
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/land")).await;
    let now = between(&page, "<p class=\"meta now-retry\">", "</p>");
    assert!(
        now.contains(
            ", after <a href=\"#run-1\">run 1</a> failed: kiln clippy failed after the rebase. Sluice retried it on its own."
        ),
        "{now}"
    );
    assert!(!now.contains("conditional"), "{now}");
}

#[tokio::test]
async fn a_lanes_stages_still_to_come_are_one_note_and_its_summary_shares_the_id_line() {
    let f = Fixture::new().await;
    let specs = f._home.path().join("lane-specs");
    std::fs::create_dir_all(&specs).unwrap();
    let mut steps = serde_json::Map::new();
    for unit in ["b1", "b2"] {
        let spec = specs.join(format!("{unit}.md"));
        std::fs::write(&spec, format!("# Lane {unit}\n")).unwrap();
        let tags = json!([format!("unit:{unit}")]);
        steps.insert(
            format!("{unit}-fork"),
            json!({"run":"custom.open","tags":tags}),
        );
        steps.insert(
            format!("{unit}-work"),
            json!({"run":"custom.open","tags":tags,"after":[format!("{unit}-fork")],
                "in":{"ticket":{"default":format!("T-{unit}")},"spec":{"file":spec.to_str().unwrap()}}}),
        );
        steps.insert(
            format!("{unit}-land"),
            json!({"run":"custom.open","tags":tags,"after":[format!("{unit}-work")]}),
        );
    }
    let id = f
        .project(
            "tolanes",
            json!({"steps": steps}),
            &[("b1-fork", "running")],
        )
        .await;
    let recipes = f
        ._home
        .path()
        .join("projects")
        .join(id.to_string())
        .join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    std::fs::write(
        recipes.join("lane.json"),
        serde_json::to_vec(&board_fixture::lane_recipe()).unwrap(),
    )
    .unwrap();
    let (_, page) = f.get(&format!("/projects/id/{id}")).await;
    // b1 runs its fork: a row of Running, a cell a stage under the recipe's columns, the ones
    // to come drawn but empty
    assert_eq!(plan_html::place(&page, "b1"), "running");
    assert!(plan_html::cell(&page, "b1-fork").contains("data-state=\"running\""));
    for step in ["b1-work", "b1-land"] {
        let cell = plan_html::cell(&page, step);
        assert!(
            cell.starts_with("<li class=\"sc sc-empty\""),
            "{step}: {cell}"
        );
    }
    let head = plan_html::band(&page, "plan-running");
    for stage in ["fork", "work", "land"] {
        assert!(head.contains(&format!(">{stage}</")), "{stage}: {head}");
    }
    // b2 has not started: a row of Waiting, three empty cells and what holds it in words
    assert_eq!(plan_html::place(&page, "b2"), "waiting");
    let b2 = plan_html::row(&page, "b2");
    assert_eq!(b2.matches("<li class=\"sc sc-empty\"").count(), 3, "{b2}");
    assert!(b2.contains("<p class=\"pl-sub pl-waits\">"), "{b2}");
}

#[tokio::test]
async fn sluices_housekeeping_in_a_row_is_one_quiet_line_on_the_log() {
    let f = Fixture::new().await;
    let id = f
        .project("chores", json!({"steps":{"s":{"run":"custom.open"}}}), &[])
        .await;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            // each retirement logs a step.delete per step it retired: 2, then 1, then 1
            for (rev, retired) in [
                (7, &["u-1-work", "u-1-land"][..]),
                (8, &["u-2"]),
                (9, &["u-3"]),
            ] {
                tx.append_record(
                    Some(id),
                    Event::PlanEdit {
                        rev: Revision(rev),
                        author: "sluice".into(),
                        reason: "retire done units older than 6h".into(),
                        changes: retired
                            .iter()
                            .map(|step| sluice_model::plan_rows::PlanChange::StepDelete {
                                step: step.parse().unwrap(),
                            })
                            .collect(),
                    },
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let (_, log) = f.get(&format!("/projects/id/{id}/log")).await;
    let rows = between(&log, "<tbody id=\"log-rows\">", "</tbody>");
    assert_eq!(
        rows.matches("class=\"log-what chore\"").count(),
        1,
        "{rows}"
    );
    assert!(!rows.contains("older than 6h</p>"), "{rows}");
    assert!(
        rows.contains(
            "<p class=\"log-what chore\">sluice retired done units 3 times: plan revs 7 to 9, 4 changes"
        ),
        "{rows}"
    );
}

#[tokio::test]
async fn a_runs_file_is_a_page_with_its_terminal_codes_out_and_the_file_a_link_away() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "files",
            json!({"steps":{"s":{"run":"custom.open"}}}),
            &[("s", "failed")],
        )
        .await;
    let run = run(
        &f,
        id,
        "s",
        "2026-10-01T00:00:00Z",
        Some("2026-10-01T00:01:00Z"),
        Some(json!({"status":"failed"})),
    )
    .await;
    let dir = f._home.path().join("runs").join(run.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("stderr.log"),
        "\u{1b}[38;5;9mFail\u{1b}[0m: test <a> broke\n\u{1b}]0;title\u{7}done\n",
    )
    .unwrap();
    let href = format!("/projects/id/{id}/runs/{run}/files/stderr.log");
    let (status, page) = f.get(&href).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{page}");
    assert!(page.starts_with("<!doctype html>"), "{page}");
    assert!(
        page.contains(&format!("<a href=\"/projects/id/{id}/steps/s?tab=runs\">"))
            && page.contains("<h1 class=\"title-long\">stderr.log</h1>"),
        "{page}"
    );
    assert!(
        page.contains("<pre class=\"run-file\">Fail: test &lt;a&gt; broke\ndone\n</pre>"),
        "{page}"
    );
    assert!(
        page.contains("<a href=\"?raw=1\">The file as it is</a>"),
        "{page}"
    );
    let (_, raw) = f.get(&format!("{href}?raw=1")).await;
    assert!(raw.starts_with("\u{1b}[38;5;9mFail"), "{raw}");
}
