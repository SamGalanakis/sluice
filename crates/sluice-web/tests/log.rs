#[allow(dead_code)]
#[path = "../../../tests/support/messages.rs"]
mod stored_messages;
use axum::{body::Body, http::Request};
use sluice_model::{commands::MessageAnswer, events::Event, ids::Revision};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use sluice_web::views::{
    DashboardState, EmptyCatalog, Viewer,
    log::{self, LogQuery},
};
use std::sync::Arc;
use tower::ServiceExt;
#[test]
fn p605_query_keeps_repeated_filters_and_rejects_invalid_or_ambiguous_cursors() {
    let query =
        LogQuery::parse("kind=message,step&kind=step&thread=a,b&thread=a&before=10").unwrap();
    assert_eq!(query.kinds, ["message", "step"]);
    assert_eq!(query.threads, ["a", "b"]);
    assert_eq!(
        LogQuery::parse(&query.query(Some(10), None)).unwrap(),
        query
    );
    for raw in [
        "before=-1",
        "after=abc",
        "before=1&after=2",
        "before=9223372036854775808",
    ] {
        assert!(LogQuery::parse(raw).is_err(), "{raw}");
    }
}
#[tokio::test]
async fn p605_log_keyset_pages_preserve_filters_hide_successful_message_calls_and_keep_failures() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let project = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "log".parse().unwrap(),
                    description: String::new(),
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
        .project_id;
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            for n in 0..115 {
                tx.append_record(
                    Some(project),
                    Event::PlanEdit {
                        rev: Revision(n + 1),
                        author: "owner".into(),
                        reason: format!("change {n} <script>"),
                        ops: vec![],
                    },
                )?;
            }
            for status in [
                sluice_model::commands::StepStatus::Succeeded,
                sluice_model::commands::StepStatus::Failed,
            ] {
                tx.append_record(
                    Some(project),
                    Event::Call {
                        call: sluice_model::ids::RunId::new(),
                        name: "message.post".into(),
                        status,
                        inputs: None,
                        outputs: None,
                        error: None,
                        direct: false,
                        author: Some("owner".into()),
                    },
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let newest = log::load(&reads, Some(project), LogQuery::default())
        .await
        .unwrap();
    assert_eq!(newest.rows.len(), 50);
    assert_eq!(newest.rows.iter().filter(|r| r.kind == "call").count(), 1);
    assert!(newest.rows[0].summary.contains("failed"));
    let first_seq = newest.rows.last().unwrap().seq;
    let older = log::load(
        &reads,
        Some(project),
        LogQuery {
            before: Some(first_seq),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(!older.rows.is_empty());
    assert!(older.rows.iter().all(|r| r.seq < first_seq));
    assert!(!older.older.is_empty());
    assert!(!older.newer.is_empty());
    let returned = log::load(
        &reads,
        Some(project),
        LogQuery {
            after: Some(older.rows[0].seq),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        returned.rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
        newest.rows.iter().map(|r| r.seq).collect::<Vec<_>>()
    );
    let calls = log::load(
        &reads,
        Some(project),
        LogQuery {
            kinds: vec!["call".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(calls.rows.len(), 2);
    let html = newest.render(&Viewer::default()).unwrap();
    assert!(!html.as_str().contains("change 1 <script>"));
    assert!(html.as_str().contains("data-preserve-attr=\"open\""));
    assert!(
        !older
            .render(&Viewer::default())
            .unwrap()
            .as_str()
            .contains("data-init=")
    );
    let router = log::router(DashboardState::new(reads, Arc::new(EmptyCatalog)));
    for (query, status) in [
        ("kind=inbox", 400),
        ("before=1&after=2", 400),
        ("kind=plan", 200),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/projects/id/{project}/log?{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
}
#[tokio::test]
async fn p605_thread_filter_uses_message_records_and_log_retention_cannot_erase_conversations() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let project = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "messages".parse().unwrap(),
                    description: String::new(),
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
        .project_id;
    for thread in ["a", "b"] {
        stored_messages::stored(
            &writer,
            project,
            stored_messages::Stored {
                thread,
                from: "worker",
                to: Some("owner"),
                body: thread,
                ..Default::default()
            },
        )
        .await;
    }
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let page = log::load(
        &reads,
        Some(project),
        LogQuery {
            threads: vec!["a".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert!(page.rows[0].summary.contains("a from worker"));
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            sluice_store::records::trim_to(tx, Some(project), 1, 1)?;
            Ok(())
        })
        .await
        .unwrap();
    let history = sluice_web::views::threads::load(
        &reads,
        Some(project),
        sluice_model::commands::MessageView::History,
        None,
    )
    .await
    .unwrap();
    assert_eq!(history.threads.len(), 2);
    let _type_contract = MessageAnswer {
        action: "close".into(),
        params: None,
        values: None,
    };
}
/// The log leads with signal: a capacity fn's successful calls stay out unless the calls are
/// asked for; presets pick steps, runs, messages or errors; a Step field narrows to one step.
/// The table scrolls in a named, focusable region and every time reads in the page's words.
#[tokio::test]
async fn log_hides_call_noise_and_offers_presets_and_a_step_field() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let project = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "noisy".parse().unwrap(),
                    description: String::new(),
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
        .project_id;
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            use sluice_model::commands::StepStatus;
            for step in ["build", "work"] {
                tx.append_record(
                    Some(project),
                    Event::StepStatus {
                        step: step.parse().unwrap(),
                        from: Some(StepStatus::Running),
                        to: if step == "work" {
                            StepStatus::Failed
                        } else {
                            StepStatus::Succeeded
                        },
                        error: None,
                        run_ids: vec![],
                        needs: Default::default(),
                    },
                )?;
            }
            for n in 0..60 {
                tx.append_record(
                    Some(project),
                    Event::Call {
                        call: sluice_model::ids::RunId::new(),
                        name: "lash.lane_capacity".into(),
                        status: if n == 0 {
                            StepStatus::Failed
                        } else {
                            StepStatus::Succeeded
                        },
                        inputs: None,
                        outputs: None,
                        error: None,
                        direct: true,
                        author: Some("capacity".into()),
                    },
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let all = log::load(&reads, Some(project), LogQuery::default())
        .await
        .unwrap();
    assert_eq!(all.query.preset(), "all");
    assert_eq!(
        all.rows.iter().filter(|r| r.kind == "call").count(),
        1,
        "only the failed call"
    );
    assert_eq!(
        all.rows.iter().filter(|r| r.kind == "step.status").count(),
        2
    );
    let calls = log::load(&reads, Some(project), LogQuery::parse("kind=call").unwrap())
        .await
        .unwrap();
    assert_eq!(
        calls.rows.iter().map(|r| r.count).sum::<usize>(),
        50,
        "every call when the calls are asked for"
    );
    // calls in a row that say the same are one line, counted
    assert!(calls.rows.len() < 50, "{}", calls.rows.len());
    assert!(calls.rows.iter().any(|r| r.count > 1));
    let errors = log::load(&reads, Some(project), LogQuery::parse("errors=1").unwrap())
        .await
        .unwrap();
    assert_eq!(errors.query.preset(), "errors");
    let kinds: Vec<_> = errors
        .rows
        .iter()
        .map(|r| (r.kind.as_str(), r.summary.as_str()))
        .collect();
    assert_eq!(kinds.len(), 2, "{kinds:?}");
    assert!(
        kinds
            .iter()
            .any(|(k, s)| *k == "step.status" && s.contains("work")),
        "{kinds:?}"
    );
    assert!(kinds.iter().any(|(k, _)| *k == "call"), "{kinds:?}");
    let step = log::load(
        &reads,
        Some(project),
        LogQuery::parse("step=build").unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(step.rows.len(), 1);
    assert!(step.rows[0].summary.starts_with("build "));
    assert_eq!(LogQuery::parse("kind=step").unwrap().preset(), "step");
    assert_eq!(LogQuery::parse("kind=step&kind=run").unwrap().preset(), "");
    // a preset keeps the step and threads
    assert_eq!(step.query.preset_query("errors"), "step=build&errors=1");
    let html = step.body().unwrap();
    let html = html.as_str();
    assert!(
        html.contains(
            r#"<div class="scroll" tabindex="0" role="region" aria-label="Log records">"#
        )
    );
    assert!(
        html.contains(r#"aria-current="page">All</a>"#),
        "the preset shown is marked"
    );
    assert!(html.contains(r#"name="step" value="build""#));
    assert!(
        html.contains(r#"<details class="kinds""#)
            && !html.contains(r#"<details class="kinds" open"#),
        "the kinds fold"
    );
    assert!(html.contains("<time data-ago="), "{html}");
    // no raw RFC 3339 text outside a record's JSON: a time reads "2026-10-07 20:47 UTC" until
    // the script reads it
    let shown: String = html
        .split("<pre>")
        .map(|part| part.split_once("</pre>").map_or(part, |(_, after)| after))
        .collect();
    let text = shown
        .split('>')
        .filter_map(|s| s.split('<').next())
        .collect::<String>();
    assert!(!regex_like_rfc3339(&text), "{text}");
    assert!(text.contains(" UTC"), "{text}");
    writer.shutdown().await.unwrap();
}
/// Some "2026-10-07T20:47:08" in text.
fn regex_like_rfc3339(text: &str) -> bool {
    text.as_bytes().windows(19).any(|w| {
        w[4] == b'-'
            && w[7] == b'-'
            && w[10] == b'T'
            && w[13] == b':'
            && w[16] == b':'
            && w[..4].iter().all(u8::is_ascii_digit)
    })
}

#[tokio::test]
async fn the_log_says_each_record_in_a_sentence_and_the_global_log_spans_projects() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let project = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "said".parse().unwrap(),
                    description: String::new(),
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
        .project_id;
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            use sluice_model::{commands::StepStatus, error::PublicError};
            let status = |step: &str, from, to, error| Event::StepStatus {
                step: step.parse().unwrap(),
                from: Some(from),
                to,
                error,
                run_ids: vec![],
                needs: Default::default(),
            };
            tx.append_record(
                Some(project),
                status(
                    "harness",
                    StepStatus::Running,
                    StepStatus::Failed,
                    Some(PublicError::Cancelled {
                        message: "cancel requested".into(),
                    }),
                ),
            )?;
            tx.append_record(
                Some(project),
                status(
                    "bump",
                    StepStatus::Running,
                    StepStatus::Failed,
                    Some(PublicError::FnFailure {
                        message: "exit code 1\nTraceback (most recent call last):\n  File \"m.py\"\nRuntimeError: codex ran past the wall-clock cap of 600 min (SLUICE_AGENT_MAX_MIN)\nsession: s".into(),
                    }),
                ),
            )?;
            // a running step restarted under a new run: no change to show
            tx.append_record(
                Some(project),
                status("watch", StepStatus::Running, StepStatus::Running, None),
            )?;
            tx.append_record(
                Some(project),
                Event::ProjectUpdate {
                    fields: vec!["board_doc".into(), "prune_done_after".into()],
                    reason: Some("standup refresh".into()),
                    author: "cli".into(),
                },
            )?;
            tx.append_record(
                Some(project),
                Event::ProjectPause {
                    paused: true,
                    reason: None,
                    author: "owner".into(),
                },
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let page = log::load(&reads, Some(project), LogQuery::default())
        .await
        .unwrap();
    let said: Vec<&str> = page.rows.iter().map(|r| r.summary.as_str()).collect();
    assert!(
        said.contains(&"harness: Cancelled while it ran."),
        "{said:?}"
    );
    assert!(
        said.contains(&"bump: Stopped at its wall-clock cap after 10h 0m."),
        "{said:?}"
    );
    assert!(
        said.contains(
            &"The board's document and when done units retire changed by cli: standup refresh"
        ),
        "{said:?}"
    );
    assert!(said.contains(&"Project paused by owner"), "{said:?}");
    assert!(
        said.iter().all(|s| !s.contains('{')),
        "never JSON: {said:?}"
    );
    assert!(!said.iter().any(|s| s.starts_with("watch")), "{said:?}");
    // each sentence links the step it names to its page
    let bump = page
        .rows
        .iter()
        .find(|r| r.summary.starts_with("bump:"))
        .unwrap();
    assert_eq!(
        bump.html.as_str(),
        format!(
            "<a href=\"/projects/id/{project}/steps/bump\">bump</a>: Stopped at its wall-clock cap after 10h 0m."
        )
    );
    // Errors: the failure, not the owner's cancel
    let errors = log::load(&reads, Some(project), LogQuery::parse("errors=1").unwrap())
        .await
        .unwrap();
    let errs: Vec<&str> = errors.rows.iter().map(|r| r.summary.as_str()).collect();
    assert_eq!(
        errs,
        ["bump: Stopped at its wall-clock cap after 10h 0m."],
        "{errs:?}"
    );
    // the page: the sentence outside the JSON's toggle, whose name is short
    let html = page.body().unwrap();
    let html = html.as_str();
    assert!(html.contains("<p class=\"log-what\"><a href=\""), "{html}");
    assert!(html.contains("<summary aria-label=\"Record "), "{html}");
    // asked for, the status records keep the restart too
    let statuses = log::load(
        &reads,
        Some(project),
        LogQuery::parse("kind=step.status").unwrap(),
    )
    .await
    .unwrap();
    assert!(
        statuses
            .rows
            .iter()
            .any(|r| r.summary == "watch running → running")
    );
    // the global log holds every project's records, each under its project's name
    let global = log::load(&reads, None, LogQuery::default()).await.unwrap();
    assert!(global.rows.len() >= 4, "{:?}", global.rows);
    assert!(
        global
            .rows
            .iter()
            .any(|r| r.place.0 == "said" && r.summary == "Project paused by owner")
    );
}
