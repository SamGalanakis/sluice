//! The fifth critique's fixes, each through the pages it changed. A page as drawn reads its
//! durations as the server read the clock; a retried step that has not run yet says so
//! under its header; a step is named one way on its page, its thread, its message boxes and the
//! log; the asking step's tab leads with its question and the switcher counts a project's
//! questions; an answered or closed question keeps its place and reads whole; the log folds a
//! settled unit's status changes and titles a question to the owner; and the small copy fixes:
//! the run file's title, a pause reason's period, the inbox's one section name, the guide's title
//! and the agent docs.
use crate::board_fixture;
use crate::stored_messages;
use board_fixture::Fixture;
use serde_json::json;
use sluice_model::{
    events::{Event, UnitStep},
    ids::{AttemptId, ProjectId, Revision, RunId, StepId, WorkGeneration},
};
use sluice_store::RetrySafety;
use stored_messages::{Stored, stored};

fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from} in {html}"));
    let end = html[start..].find(to).map_or(html.len(), |e| start + e);
    &html[start..end]
}

/// An instant `seconds` ago, as the store writes one.
fn ago(seconds: u64) -> &'static str {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    Box::leak(sluice_web::views::rfc3339(now - seconds).into_boxed_str())
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

/// A record appended now.
async fn record(f: &Fixture, project: ProjectId, event: Event) -> i64 {
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            Ok(tx.append_record(Some(project), event)?.seq.0)
        })
        .await
        .unwrap()
}

/// A question to the owner from `from`, asked now: someone waits on it.
async fn ask(f: &Fixture, project: ProjectId, from: &'static str, title: &'static str) -> i64 {
    let thread = if from == "orchestrator" {
        "owner".to_owned()
    } else {
        format!("step-{from}")
    };
    stored(
        &f.writer,
        project,
        Stored {
            thread: &thread,
            from,
            to: Some("owner"),
            body: "Which?\n\n- **Now:** land it.\n- **Later:** wait.",
            title: Some(title),
            question: true,
            ..Default::default()
        },
    )
    .await
    .id
    .0
}

fn status(step: &str, from: &str, to: &str) -> Event {
    Event::StepStatus {
        step: StepId::new(step).unwrap(),
        from: Some(from.parse().unwrap()),
        to: to.parse().unwrap(),
        error: None,
        run_ids: vec![],
        needs: Default::default(),
    }
}

#[tokio::test]
async fn an_answered_or_closed_question_keeps_its_place_and_reads_whole() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    // answered on a running step's Overview: whole, its list kept, the answer under it
    let q = ask(&f, id, "l1-work", "Land l1 first?").await;
    let reply = stored(
        &f.writer,
        id,
        Stored {
            thread: "step-l1-work",
            from: "owner",
            to: Some("l1-work"),
            body: "Land it now.",
            reply: Some(sluice_model::ids::MessageId(q)),
            ..Default::default()
        },
    )
    .await
    .id
    .0;
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/l1-work")).await;
    assert!(!page.contains("d-sec d-ask"), "answered: nothing waits");
    let now = between(&page, "<article class=\"mod d-sec d-now\"", "</article>");
    let item = between(
        now,
        &format!("id=\"ov-message-{q}\""),
        "<div class=\"msg-card\"",
    );
    assert!(
        item.contains("<li><strong>Now:</strong> land it.</li>"),
        "{item}"
    );
    assert!(
        !item.contains("m-text"),
        "never a run-together excerpt: {item}"
    );
    // its answer the card after it, whole too
    assert!(
        now.contains(&format!(
            "<div class=\"msg-card\" id=\"ov-message-{reply}\">"
        )),
        "{now}"
    );

    // closed on the inbox: kept in its place for ten minutes as the line that says so
    let c = ask(&f, id, "orchestrator", "Accept revision 3?").await;
    let close = stored(
        &f.writer,
        id,
        Stored {
            thread: "owner",
            from: "owner",
            to: Some("orchestrator"),
            body: "",
            reply: Some(sluice_model::ids::MessageId(c)),
            ..Default::default()
        },
    )
    .await;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE messages SET closed_at=?1 WHERE id=?2",
                (close.at.as_str(), c),
            )?;
            tx.changed(Some(id), "messages");
            Ok(())
        })
        .await
        .unwrap();
    let (_, inbox) = f.get("/inbox").await;
    let card = between(&inbox, &format!("id=\"item-{id}-{c}\""), "</article>");
    assert!(
        inbox.contains(&format!(
            "<article class=\"q-card q-done\" id=\"item-{id}-{c}\""
        )),
        "{card}"
    );
    assert!(
        card.contains(&format!(
            "<p class=\"q-answered\" tabindex=\"-1\" data-q=\"{id}-{c}\">"
        )) && card.contains(">just now</time>: <span class=\"qa-title\">Accept revision 3?</span>")
            && card.contains("<span class=\"qa-what\">Closed "),
        "{card}"
    );
    // one section name on the inbox and Questions; the inbox does not explain what Questions
    // adds (the messages' switch leads there)
    for path in ["/inbox", "/questions"] {
        let (_, html) = f.get(path).await;
        assert!(html.contains(">Questions for you<"), "{path}");
        assert!(!html.contains(">For you<span"), "{path}");
    }
    assert!(!inbox.contains("q-also") && !inbox.contains("also lists"));
}

#[tokio::test]
async fn elapsed_time_retry_feedback_and_settled_log() {
    let f = Fixture::new().await;
    {
        let id = f
            .project(
                "clock",
                json!({"steps":{"s":{"run":"custom.open"}}}),
                &[("s", "running")],
            )
            .await;
        let started = ago(85 * 60 + 10);
        run(&f, id, "s", started, None, None).await;
        let (_, page) = f.get(&format!("/projects/id/{id}/steps/s")).await;
        let badge = between(&page, "<p class=\"status-line\">", "</p>");
        assert!(
            badge.contains(&format!(
                "<b>Running</b> <time data-since=\"{started}\" datetime=\"{started}\" title=\""
            )) && badge.contains(" UTC\">1h 25m</time>"),
            "{badge}"
        );
        assert!(
            !badge.contains("UTC</time>"),
            "no date where a duration goes: {badge}"
        );
    }
    {
        let id = f
            .project(
                "retried",
                json!({"steps":{"w":{"run":"custom.open"}}}),
                &[("w", "pending")],
            )
            .await;
        run(
            &f,
            id,
            "w",
            ago(3 * 86_400),
            Some(ago(3 * 86_400 - 600)),
            Some(json!({"status":"failed","error":{"kind":"fn","message":"boom"}})),
        )
        .await;
        record(
            &f,
            id,
            Event::StepRetry {
                rev: Revision(1),
                author: "owner".into(),
                reason: String::new(),
                step: StepId::new("w").unwrap(),
                work: WorkGeneration(2),
            },
        )
        .await;
        let feedback = stored(
            &f.writer,
            id,
            Stored {
                thread: "step-w",
                from: "owner",
                to: Some("w"),
                body: "try with a longer cap",
                ..Default::default()
            },
        )
        .await
        .id
        .0;
        let (_, page) = f.get(&format!("/projects/id/{id}/steps/w")).await;
        let said = between(&page, "<p class=\"sh-note\">", "</p>");
        assert!(
            said.contains("You retried it <time data-ago=\"") && said.contains(">just now</time>"),
            "{said}"
        );
        assert!(
        said.contains(&format!(
            ", <a href=\"#message-{feedback}\">with feedback</a>: <q class=\"fb-q\">try with a longer cap</q>."
        )),
        "{said}"
    );
        // when it starts is Overview's to say (`polish6.rs`), not the header's
        assert!(!said.contains("It starts when"), "{said}");
        // it sits under the head's status line, which says when its last run ended
        assert!(
            page.find("its last run ended").unwrap() < page.find("You retried it").unwrap(),
            "{page}"
        );
        // once it runs again the header no longer says it: Runs and Now do
        let (_, other) = f
            .get(&format!("/projects/id/{}/steps/beta-build", f.id))
            .await;
        assert!(!other.contains("<p class=\"sh-note\">"), "{other}");
    }
    {
        let id = f
            .project(
                "settle",
                json!({"steps":{
                "u-a":{"run":"custom.open","tags":["unit:u"]},
                "u-b":{"run":"custom.open","tags":["unit:u"],"after":["u-a"]},
                "other":{"run":"custom.open"}}}),
                &[("u-a", "succeeded"), ("u-b", "succeeded")],
            )
            .await;
        for (step, from, to) in [
            ("u-a", "pending", "running"),
            ("u-a", "running", "succeeded"),
            ("other", "pending", "running"),
            ("u-b", "pending", "running"),
            ("u-b", "running", "succeeded"),
        ] {
            record(&f, id, status(step, from, to)).await;
        }
        let step = |s: &str| UnitStep {
            id: StepId::new(s).unwrap(),
            status: sluice_model::commands::StepStatus::Succeeded,
            held: false,
            outputs: None,
            omitted: vec![],
        };
        record(
            &f,
            id,
            Event::UnitSettled {
                unit: "u".parse().unwrap(),
                work: WorkGeneration(1),
                steps: vec![step("u-a"), step("u-b")],
            },
        )
        .await;
        ask(&f, id, "other", "Rebase first?").await;
        let (_, log) = f.get(&format!("/projects/id/{id}/log")).await;
        let rows = between(&log, "<table class=\"log\">", "</table>");
        // what each row says, its JSON left out
        let rows: String = rows
            .split("<p class=\"log-what\">")
            .skip(1)
            .map(|what| &what[..what.find("</p>").unwrap()])
            .collect::<Vec<_>>()
            .join("\n");
        let rows = rows.as_str();
        assert!(
            rows.contains(&format!(
                " · <a href=\"/projects/id/{id}/log?unit=u\">4 status changes</a>"
            )),
            "{rows}"
        );
        assert!(rows.contains("Unit <a"), "{rows}");
        assert!(!rows.contains("u-a</code></span></a> pending"), "{rows}");
        assert!(!rows.contains(">u-a</a> pending"), "{rows}");
        // another step's change is its own row still
        assert!(rows.contains("other</a> pending → running"), "{rows}");
        // a question to the owner by its title
        assert!(
            rows.contains(&format!(
                "<a href=\"/projects/id/{id}/steps/other\">other</a> asked you: Rebase first?"
            )),
            "{rows}"
        );
        assert!(!rows.contains("Which?"), "{rows}");
        // the unit's own log lists each change
        let (_, unit) = f.get(&format!("/projects/id/{id}/log?unit=u")).await;
        assert!(!unit.contains("status changes</a>"), "{unit}");
        assert!(unit.contains("running → succeeded"), "{unit}");
    }
}
