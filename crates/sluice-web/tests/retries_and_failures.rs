//! What the plan says of a step that has run before, or stopped, before it is opened: a retry's
//! run number and how its earlier runs ended (its unit's row, the step's badge and Now), a
//! failure's kind in a word and its one sentence in its stopped module, a link to the failure's
//! own log record (the step page and the index), what a filtered empty plan hides, and a wait
//! named by its source's title and id.
mod board_fixture;
mod plan_html;
use axum::http::StatusCode;
use board_fixture::Fixture;
use serde_json::json;
use sluice_model::{
    commands::StepStatus,
    error::PublicError,
    events::Event,
    ids::{AttemptId, ProjectId, RunId, StepId},
    shown::Shown,
};
use sluice_store::RetrySafety;

fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from} in {html}"));
    let end = html[start..].find(to).map_or(html.len(), |e| start + e);
    &html[start..end]
}

/// Home's stopped row for `step`, in its project's module.
fn stopped_row<'a>(home: &'a str, step: &str) -> &'a str {
    let row = home
        .split("<li class=\"pm-row pm-stopped")
        .skip(1)
        .find(|r| {
            r.split("</li>")
                .next()
                .unwrap()
                .contains(&format!("/steps/{step}\""))
        })
        .unwrap_or_else(|| panic!("no stopped row for {step} in {home}"));
    &row[..row.find("</li>").unwrap()]
}

fn agent(kind: &str) -> PublicError {
    PublicError::AgentFailure {
        kind: kind.into(),
        message: "it stopped".into(),
        session: None,
    }
}

/// One run of `step` in its current generation, started `started`; ended with `ended` (its
/// result's status and error) or still going.
fn run(
    tx: &mut sluice_store::WriteTransaction<'_>,
    project: ProjectId,
    step: &str,
    started: &str,
    ended: Option<(&str, Option<PublicError>)>,
) -> rusqlite::Result<()> {
    let (attempt, run) = (AttemptId::new(), RunId::new());
    let phase = if ended.is_some() {
        "terminal"
    } else {
        "executing"
    };
    tx.sql().execute(
        "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?1,?2,?3,1,1,?4,'{}','hash',?5)",
        (attempt.to_string(), project.to_string(), step, phase, started),
    )?;
    let (finished, result) = match ended {
        Some((status, error)) => (
            Some(started.to_owned()),
            Some(json!({"status": status, "error": error}).to_string()),
        ),
        None => (None, None),
    };
    tx.sql().execute(
        "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at,finished_at,result) VALUES (?1,?2,?3,?4,1,1,?5,?5,?6,?7)",
        (run.to_string(), project.to_string(), attempt.to_string(), step, started, finished, result),
    )?;
    Ok(())
}

/// The titled fixture with l1-work on its sixth run (five ended: failed, failed, cancelled,
/// failed at its wall-clock cap, failed at a usage cap; the sixth quiet since January), kit-a on
/// its second (the first failed), and l2-work failed at a usage cap.
async fn retried(f: &Fixture) -> ProjectId {
    let id = f.titled().await;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let fail = |m: &str| Some(PublicError::FnFailure { message: m.into() });
            run(
                tx,
                id,
                "l1-work",
                "2026-01-01T00:00:00Z",
                Some(("failed", fail("one"))),
            )?;
            run(
                tx,
                id,
                "l1-work",
                "2026-01-01T00:01:00Z",
                Some(("failed", fail("two"))),
            )?;
            run(
                tx,
                id,
                "l1-work",
                "2026-01-01T00:02:00Z",
                Some((
                    "failed",
                    Some(PublicError::Cancelled {
                        message: "wrong base".into(),
                    }),
                )),
            )?;
            run(
                tx,
                id,
                "l1-work",
                "2026-01-01T00:03:00Z",
                Some(("failed", Some(agent("WallCap")))),
            )?;
            run(
                tx,
                id,
                "l1-work",
                "2026-01-01T00:04:00Z",
                Some(("failed", Some(agent("QuotaExhausted")))),
            )?;
            run(tx, id, "l1-work", "2026-01-01T00:05:00Z", None)?;
            run(
                tx,
                id,
                "kit-a",
                "2026-10-01T00:00:00Z",
                Some(("failed", fail("flaky"))),
            )?;
            run(tx, id, "kit-a", "2026-10-01T00:10:00Z", None)?;
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='l2-work'",
                (
                    id.to_string(),
                    serde_json::to_string(&agent("QuotaExhausted")).unwrap(),
                ),
            )?;
            tx.changed(Some(id), "project");
            Ok(())
        })
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn a_retry_says_its_run_and_how_its_earlier_runs_ended_even_when_quiet() {
    let f = Fixture::new().await;
    let id = retried(&f).await;
    let (status, html) = f.get(&format!("/projects/id/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    // the row of a quiet run: its quiet time, then "run 6" after the last three earlier runs'
    // marks and "+2" for the two before them
    let row = plan_html::row(&html, "l1");
    assert!(
        row.contains("<b>quiet</b>") && row.contains(" · silent "),
        "{row}"
    );
    let tries = between(row, "<span class=\"tries\"", "</span></span>");
    assert!(
        tries.contains("title=\"Run 6, after 4 failed and 1 cancelled\""),
        "{tries}"
    );
    assert!(
        tries.contains("<span class=\"tries-more\">+2</span>"),
        "{tries}"
    );
    let mark = |s| sluice_web::views::ui::mark(s).as_str().to_owned();
    assert!(
        row.contains(&format!(
            "+2</span>{}{}{}</span><span aria-hidden=\"true\">run 6</span>",
            mark(Shown::Cancelled),
            mark(Shown::Failed),
            mark(Shown::Failed)
        )),
        "the last three, oldest first: {tries}"
    );
    assert!(
        row.contains("<span class=\"vh\">run 6, after 4 failed and 1 cancelled</span></span>"),
        "{row}"
    );
    // a unit of no recipe: its second run, the first failed
    let kit = plan_html::row(&html, "kit");
    assert!(kit.contains("title=\"Run 2, after 1 failed\""), "{kit}");
    assert!(kit.contains(">run 2<"), "{kit}");
    // a first run says nothing of runs
    let first = plan_html::row(&html, "probe");
    assert!(!first.contains("tries"), "{first}");

    // its page: the badge says the run, Now says how the run before ended and why, linked to
    // that run under Runs
    let (status, page) = f.get(&format!("/projects/id/{id}/steps/l1-work")).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let badges = between(&page, "<p class=\"d-badges\">", "</p>");
    assert!(badges.contains(">run 6<"), "{badges}");
    let now = between(&page, "<section class=\"d-sec d-now\">", "</section>");
    let retry = between(now, "<p class=\"meta now-retry\">", "</p>");
    assert!(
        retry.starts_with("<p class=\"meta now-retry\">Run 6 started <time")
            && retry.contains(", after <a href=\"#run-5\">run 5</a> failed: "),
        "{retry}"
    );
    assert!(retry.ends_with(": Its engine hit a usage cap."), "{retry}");
    assert!(page.contains("<li id=\"run-5\">"), "{page}");
    // a fn's failure says its message
    let (_, kit) = f.get(&format!("/projects/id/{id}/steps/kit-a")).await;
    let retry = between(&kit, "<p class=\"meta now-retry\">", "</p>");
    assert!(
        retry.contains("<a href=\"#run-1\">run 1</a> failed"),
        "{retry}"
    );
    assert!(retry.ends_with(": flaky."), "{retry}");
}

#[tokio::test]
async fn a_stopped_card_names_its_failures_kind_and_says_why_under_it() {
    let f = Fixture::new().await;
    let id = retried(&f).await;
    // a failed step outside any matrix, lost; and one whose fn failed
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            for (step, error) in [
                (
                    "plain",
                    PublicError::ProcessLost {
                        message: "gone".into(),
                    },
                ),
                (
                    "bare",
                    PublicError::FnFailure {
                        message: "boom".into(),
                    },
                ),
                // a fn stopped at its wall-clock cap reads as an agent's cap does
                (
                    "kit-b",
                    PublicError::FnFailure {
                        message: "Traceback (most recent call last):\n  File \"x\", line 1\nRuntimeError: wall-clock cap of 600 minutes exceeded".into(),
                    },
                ),
            ] {
                tx.sql().execute(
                    "UPDATE steps SET status='failed',error=?3 WHERE project_id=?1 AND step_id=?2",
                    (id.to_string(), step, serde_json::to_string(&error).unwrap()),
                )?;
            }
            tx.changed(Some(id), "project");
            Ok(())
        })
        .await
        .unwrap();
    let (_, html) = f.get(&format!("/projects/id/{id}")).await;
    // a stopped module: the glyph and its word, the failure's kind in a word, why under it;
    // never the stored JSON
    let l2 = plan_html::stopped(&html, "l2");
    assert!(
        l2.contains("<b>failed</b> <span class=\"pl-kind\">quota</span>"),
        "{l2}"
    );
    assert!(l2.contains("<p>Its engine hit a usage cap.</p>"), "{l2}");
    assert!(!l2.contains("agent_failure"), "{l2}");
    let plain = plan_html::stopped(&html, "plain");
    assert!(
        plain.contains("<span class=\"pl-kind\">lost</span>")
            && plain.contains("<p>Its process was lost.</p>"),
        "{plain}"
    );
    // the work's own failure: no kind beside "failed"
    let bare = plan_html::stopped(&html, "bare");
    assert!(
        !bare.contains("pl-kind") && bare.contains("<p>Its fn failed: boom.</p>"),
        "{bare}"
    );
    // a fn stopped at its wall-clock cap reads as an agent's cap does
    let kit = plan_html::stopped(&html, "kit");
    assert!(
        kit.contains("<span class=\"pl-kind\">cap</span>")
            && kit.contains("<p>Stopped at its wall-clock cap after 10h 0m.</p>"),
        "{kit}"
    );
    // a running unit says no why
    assert!(!plan_html::row(&html, "probe").contains("class=\"why\""));
}

#[tokio::test]
async fn a_failure_links_its_own_log_record_which_the_log_marks() {
    let f = Fixture::new().await;
    let id = retried(&f).await;
    // no record kept: the sentence links the step's records on the log, as every reason does
    let (_, home) = f.get("/").await;
    let row = stopped_row(&home, "l2-work");
    assert!(
        row.contains(&format!("<a class=\"sr-why\" href=\"/projects/id/{id}/log?step=l2-work\" title=\"Its engine hit a usage cap.\">")),
        "{row}"
    );
    let seq = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let record = tx.append_record(
                Some(id),
                Event::StepStatus {
                    step: StepId::new("l2-work").unwrap(),
                    from: Some(StepStatus::Running),
                    to: StepStatus::Failed,
                    error: Some(agent("QuotaExhausted")),
                    run_ids: vec![],
                    needs: Default::default(),
                },
            )?;
            // a later record of the step: the link pages the log to the failure
            tx.append_record(
                Some(id),
                Event::StepStatus {
                    step: StepId::new("l2-work").unwrap(),
                    from: Some(StepStatus::Failed),
                    to: StepStatus::Pending,
                    error: None,
                    run_ids: vec![],
                    needs: Default::default(),
                },
            )?;
            Ok(record.seq.0)
        })
        .await
        .unwrap();
    let href = format!(
        "/projects/id/{id}/log?step=l2-work&#38;before={}#r{seq}",
        seq + 1
    );
    // "Why it failed" on its page
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/l2-work")).await;
    let why = between(&page, "<section class=\"d-sec d-failure\">", "</section>");
    assert!(
        why.contains(&format!(
            "<p class=\"meta err-log\"><a href=\"{href}\">Its log record {seq}</a></p>"
        )),
        "{why}"
    );
    // the index's stopped row
    let (_, home) = f.get("/").await;
    let row = stopped_row(&home, "l2-work");
    assert!(
        row.contains(&format!(
            "<a class=\"sr-why\" href=\"{}\"",
            href.replace("&#38;", "&amp;")
        )),
        "{row}"
    );
    // the page it opens: the failure first, its row the target
    let log = sluice_web::views::log::load(
        &f.dashboard.reads,
        Some(id),
        sluice_web::views::log::LogQuery::parse(&format!("step=l2-work&before={}", seq + 1))
            .unwrap(),
    )
    .await
    .unwrap()
    .body()
    .unwrap()
    .as_str()
    .to_owned();
    let rows = between(&log, "<tbody id=\"log-rows\">", "</tbody>");
    assert!(
        rows.starts_with(&format!("<tbody id=\"log-rows\"><tr id=\"r{seq}\">")),
        "{rows}"
    );
}

#[tokio::test]
async fn a_filtered_empty_board_says_how_many_units_its_show_hides() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "calm",
            json!({"steps": {"a": {"run": "custom.open"}, "b": {"run": "custom.open"}, "c": {"run": "custom.open", "tags": ["unit:u"]}}}),
            &[("a", "succeeded"), ("b", "running"), ("c", "pending")],
        )
        .await;
    let (_, html) = f
        .get(&format!("/projects/id/{id}?order=plan&show=attention"))
        .await;
    let empty = between(&html, "<p class=\"empty\">", "</p>");
    assert_eq!(
        empty,
        format!(
            "<p class=\"empty\">Nothing needs attention. 3 units hidden by Show: Attention. <a href=\"/projects/id/{id}?show=all\">Show all</a>"
        )
    );
    let (_, html) = f.get(&format!("/projects/id/{id}?show=done")).await;
    assert!(
        !html.contains("<p class=\"empty\">"),
        "a done unit shows: {html}"
    );
    // nothing hidden: nothing said of it
    let empty = f.project("none", json!({"steps": {}}), &[]).await;
    let (_, html) = f.get(&format!("/projects/id/{empty}?show=attention")).await;
    assert!(!html.contains("hidden by Show"), "{html}");
}

#[tokio::test]
async fn a_rows_wait_names_its_source_by_title_and_id_linked() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let (_, html) = f.get(&format!("/projects/id/{id}")).await;
    let row = plan_html::row(&html, "l3");
    let waits = between(row, "<p class=\"pl-sub pl-waits\">", "</p>");
    assert!(
        waits.contains(&format!("fork waits for <a href=\"/projects/id/{id}/steps/l2-land\" data-opens=\"l2-land\">FIG-2: Stop the parser leak <code>l2-land</code></a> (blocked)")),
        "{waits}"
    );
    assert!(
        waits.contains("Probes the parser under load <code>probe</code></a> (running)"),
        "{waits}"
    );
}
