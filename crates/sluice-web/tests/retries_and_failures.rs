//! What the board says of a step that has run before, or stopped, before it is opened: a retry's
//! run number and how its earlier runs ended (card, matrix pill, phone lane string, the step's
//! badge and Now), a failure's kind in its caption and its one sentence under its card or matrix
//! row, a link to the failure's own log record (the step page and the index), what each stage
//! column of a lane matrix has that needs a look, what a filtered empty board hides, and a wait
//! named by its source's title.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
use axum::http::StatusCode;
use board_fixture::Fixture;
use chrome::Chrome;
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
    // the matrix pill of a quiet run: its quiet time, then "run 6" after the last three earlier
    // runs' marks and "+2" for the two before them
    let pill = between(&html, "<a id=\"n-l1-work\"", "</a>");
    assert!(pill.contains("is-quiet"), "{pill}");
    assert!(pill.contains("title=\"Nothing written since"), "{pill}");
    let tries = between(pill, "<span class=\"tries\"", "</span></span>");
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
        pill.contains(&format!(
            "+2</span>{}{}{}</span>run 6",
            mark(Shown::Cancelled),
            mark(Shown::Failed),
            mark(Shown::Failed)
        )),
        "the last three, oldest first: {tries}"
    );
    assert!(
        pill.contains("run 6<span class=\"vh\">, after 4 failed and 1 cancelled</span></span>"),
        "{pill}"
    );
    // the phone's lane string says it too
    let row = between(&html, "<tr id=\"unit-l1\"", "</tr>");
    let lane = between(
        row,
        "<p class=\"mx-lane fb-lane\" aria-label=\"Stages\">",
        "</p>",
    );
    assert!(
        lane.contains("aria-label=\"l1-work quiet, run 6\"><span class=\"stg\"><span class=\"g g-quiet\" aria-hidden=\"true\">")
            && lane.contains("</span>work <span class=\"lm-run\">(run 6)</span></span></a>"),
        "{lane}"
    );
    // a card in a box: its second run, the first failed
    let card = between(&html, "<a id=\"n-kit-a\"", "</a>");
    assert!(card.contains("title=\"Run 2, after 1 failed\""), "{card}");
    assert!(card.contains(">run 2<"), "{card}");
    // a first run says nothing of runs
    let first = between(&html, "<a id=\"n-probe\"", "</a>");
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
            && retry.contains(". <a href=\"#run-5\">Run 5</a> failed <time"),
        "{retry}"
    );
    assert!(retry.ends_with(": Its engine hit a usage cap."), "{retry}");
    assert!(page.contains("<li id=\"run-5\">"), "{page}");
    // a fn's failure says its message
    let (_, kit) = f.get(&format!("/projects/id/{id}/steps/kit-a")).await;
    let retry = between(&kit, "<p class=\"meta now-retry\">", "</p>");
    assert!(
        retry.contains("<a href=\"#run-1\">Run 1</a> failed"),
        "{retry}"
    );
    assert!(retry.ends_with(": Its fn failed: flaky."), "{retry}");
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
    // the matrix pill: the ink pill and glyph stay, its caption the kind, its title the sentence
    let pill = between(&html, "<a id=\"n-l2-work\"", "</a>");
    assert!(pill.contains("is-failed"), "{pill}");
    assert!(
        pill.contains("<span class=\"dur\" title=\"Its engine hit a usage cap.\">quota</span>"),
        "{pill}"
    );
    assert!(
        pill.contains("aria-description=\"Its engine hit a usage cap.\""),
        "{pill}"
    );
    assert!(
        !pill.contains("agent_failure"),
        "never the stored JSON: {pill}"
    );
    // its row says why, a link to the step
    let row = between(&html, "<tr id=\"unit-l2\"", "</tr>");
    assert!(
        row.contains("<p class=\"why\"><a href=\"/projects/id/{id}/steps/l2-work\" data-opens=\"l2-work\">Its engine hit a usage cap.</a></p>".replace("{id}", &id.to_string()).as_str()),
        "{row}"
    );
    // a stopped card outside a matrix: its kind, and its sentence under it
    let unit = between(&html, "<section id=\"unit-plain\"", "</section>");
    assert!(
        unit.contains("<span class=\"dur\" title=\"Its process was lost.\">lost</span>"),
        "{unit}"
    );
    assert!(unit.contains(">Its process was lost.</a></p>"), "{unit}");
    let unit = between(&html, "<section id=\"unit-bare\"", "</section>");
    assert!(
        unit.contains(">failed</span>"),
        "the work's own failure: {unit}"
    );
    assert!(unit.contains(">Its fn failed: boom.</a></p>"), "{unit}");
    let card = between(&html, "<a id=\"n-kit-b\"", "</a>");
    assert!(
        card.contains(
            "<span class=\"dur\" title=\"Stopped at its wall-clock cap after 10h 0m.\">cap</span>"
        ),
        "{card}"
    );
    // a running card says no why
    let unit = between(&html, "<section id=\"unit-probe\"", "</section>");
    assert!(!unit.contains("class=\"why\""), "{unit}");
}

#[tokio::test]
async fn a_failure_links_its_own_log_record_which_the_log_marks() {
    let f = Fixture::new().await;
    let id = retried(&f).await;
    // no record kept: the sentence links the step's records on the log, as every reason does
    let (_, home) = f.get("/").await;
    let row = between(&home, "#step:l2-work", "</li>");
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
    let row = between(&home, "#step:l2-work", "</li>");
    assert!(
        row.contains(&format!("<a class=\"sr-why\" href=\"{href}\"")),
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
async fn a_lane_matrix_heads_each_stage_with_what_needs_a_look_in_it() {
    let f = Fixture::new().await;
    let id = retried(&f).await;
    let (_, html) = f.get(&format!("/projects/id/{id}")).await;
    // each band draws its own matrix: the failed lane's under Stopped, the quiet one's under
    // Running
    let stopped = between(&html, "<tr id=\"unit-l2\"", "</table>");
    let head = &html[..html.find("<tr id=\"unit-l2\"").unwrap()];
    let head = &head[head.rfind("<thead>").unwrap()..];
    assert!(
        head.contains(
            "<th scope=\"col\" class=\"mx-stage\">work<span class=\"mx-hc\"> · 1 failed</span></th>"
        ),
        "{head}"
    );
    assert!(!stopped.contains("unit-l1"), "{stopped}");
    let running = &html[..html.find("<tr id=\"unit-l1\"").unwrap()];
    let running = &running[running.rfind("<thead>").unwrap()..];
    assert!(
        running.contains(
            "<th scope=\"col\" class=\"mx-stage\">work<span class=\"mx-hc\"> · 1 quiet</span></th>"
        ),
        "{running}"
    );
    assert!(
        head.contains("<th scope=\"col\" class=\"mx-stage\">fork</th>"),
        "{head}"
    );
    assert!(
        head.contains("<th scope=\"col\" class=\"mx-stage\">land</th>"),
        "{head}"
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
            "<p class=\"empty\">Nothing needs attention. 3 units hidden by Show: Attention. <a href=\"/projects/id/{id}?order=plan&#38;show=all\">Show all</a>"
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
async fn a_matrix_rows_wait_names_its_source_by_id_its_title_on_hover() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let (_, html) = f.get(&format!("/projects/id/{id}")).await;
    let row = between(&html, "<tr id=\"unit-l3\"", "</tr>");
    let waits = between(row, "<p class=\"waits", "</p>");
    // in a lane matrix's row a wait is its source's id, its title on hover
    assert!(
        waits.contains("data-from=\"s:l2-land\" data-to=\"s:l3-fork\"><span class=\"sref\" title=\"FIG-2: Stop the parser leak\"><code class=\"sref-id\">l2-land</code></span></a>"),
        "{waits}"
    );
    assert!(
        waits.contains("<span class=\"sref\" title=\"Probes the parser under load\"><code class=\"sref-id\">probe</code></span>"),
        "{waits}"
    );
}

/// A tall lane matrix's stage heads stay at the page's top while its rows scroll under them,
/// and the matrix never scrolls sideways or clips.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_a_matrix_keeps_its_stage_heads_in_view() {
    let f = Fixture::new().await;
    let id = retried(&f).await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}")).unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelector('.mx thead')")
            .unwrap();
        for (width, theme) in [(1440, "light"), (2560, "dark")] {
            browser.viewport(width, theme).unwrap();
            let g = browser
                .eval(
                    r#"(() => {
  const table = document.querySelector('.mx'), head = table.querySelector('thead th.mx-stage');
  const rows = table.querySelectorAll('tbody tr');
  // the table's head scrolled just above the window's top, a row still under it
  scrollTo(0, scrollY + head.getBoundingClientRect().top + 30);
  const top = head.getBoundingClientRect().top;
  const last = rows[rows.length - 1].getBoundingClientRect();
  const wrap = document.querySelector('.mx-wrap');
  return {top, under: last.bottom > top, overflow: getComputedStyle(wrap).overflowX,
          scroll: document.documentElement.scrollWidth - document.documentElement.clientWidth,
          clipped: wrap.scrollWidth > wrap.clientWidth + 1, errors: window.browserErrors};
})()"#,
                )
                .unwrap();
            assert!(g["top"].as_f64().unwrap().abs() < 1.0, "{width}: {g}");
            assert_eq!(g["under"], true, "{width}: {g}");
            assert_eq!(g["overflow"], "clip", "{width}: {g}");
            assert_eq!(g["scroll"], 0, "{width}: {g}");
            assert_eq!(g["clipped"], false, "{width}: {g}");
            assert_eq!(g["errors"], json!([]), "{width}: {g}");
            browser.eval("scrollTo(0, 0)").unwrap();
        }
    })
    .await
    .unwrap();
    server.abort();
}
