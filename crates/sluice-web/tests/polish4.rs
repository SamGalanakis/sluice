//! The fourth critique's fixes, each through the pages it changed. An open question to the
//! owner is impossible to miss: it leads the index's and a project's tab titles, the coral
//! "Awaiting your reply" marks its step's row on the index and its unit on the plan, and the
//! step's Overview draws it whole with Answer; an answer is confirmed where it was given, with or
//! without script. A rerun says what started it on Runs; a pending step names what holds its
//! chain and a stopped runner; the log keeps sluice's bookkeeping quiet; and the small fixes:
//! a lane string's overrun, a live run file's end, the timeline's unreached stages, readable
//! types, the owner's thread by name.
mod board_fixture;
#[allow(dead_code)]
#[path = "../../../tests/support/messages.rs"]
mod stored_messages;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use board_fixture::Fixture;
use serde_json::json;
use sluice_model::{
    events::Event,
    ids::{AttemptId, ProjectId, Revision, RunId, StepId, WorkGeneration},
};
use sluice_store::RetrySafety;
use stored_messages::{Stored, stored};
use tower::ServiceExt;

fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from} in {html}"));
    let end = html[start..].find(to).map_or(html.len(), |e| start + e);
    &html[start..end]
}

/// A question to the owner from `from` (a step on its own thread, or the orchestrator on the
/// owner's), asked now, nobody's run attached: someone waits on it.
async fn ask(
    f: &Fixture,
    project: ProjectId,
    from: &'static str,
    title: &'static str,
    body: &'static str,
) -> i64 {
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
            body,
            title: Some(title),
            question: true,
            ..Default::default()
        },
    )
    .await
    .id
    .0
}

/// The owner's answer to `question`, sent now to `to` on its thread.
async fn answer(f: &Fixture, project: ProjectId, question: i64, to: &'static str) {
    stored(
        &f.writer,
        project,
        Stored {
            thread: &format!("step-{to}"),
            from: "owner",
            to: Some(to),
            body: "Land it now.",
            reply: Some(sluice_model::ids::MessageId(question)),
            ..Default::default()
        },
    )
    .await;
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

/// A record appended now, then dated `at` (when given).
async fn record(f: &Fixture, project: ProjectId, event: Event, at: Option<&'static str>) -> i64 {
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let record = tx.append_record(Some(project), event)?;
            if let Some(at) = at {
                tx.sql()
                    .execute("UPDATE records SET at=?1 WHERE seq=?2", (at, record.seq.0))?;
            }
            Ok(record.seq.0)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn an_open_question_to_the_owner_leads_the_titles_and_marks_its_step_in_coral() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let lane = ask(
        &f,
        id,
        "l1-work",
        "Land l1 first?",
        "It touches **two** files.",
    )
    .await;
    let boxed = ask(&f, id, "kit-a", "Keep kit-a's cache?", "Yes or no.").await;
    let orchestrator = ask(
        &f,
        id,
        "orchestrator",
        "Accept revision 3?",
        "It drops `span`.",
    )
    .await;

    // the tab titles, as a hidden tab's poll reads them, lead with the questions
    let (_, home) = f.get("/title").await;
    assert!(home.starts_with("3 questions · "), "{home}");
    let (_, project) = f.get(&format!("/title?project={id}")).await;
    assert!(project.starts_with("3 questions · "), "{project}");
    assert!(project.ends_with("titled · sluice"), "{project}");
    let (_, page) = f.get("/").await;
    assert!(page.contains("<title>3 questions · "), "{page}");

    // the index: each question leads its project's card, a row of its own with the coral tag:
    // a step's names the step and leads to its Overview, the orchestrator's to the inbox; the
    // asking step's running row no longer repeats the tag
    let others = between(&page, "<ul class=\"ask-rows\"", "</ul>");
    assert!(
        others.contains(&format!(
            "<a class=\"tag ask\" href=\"/projects/id/{id}/steps/l1-work#ov-message-{lane}\" title=\"Land l1 first?\" aria-label=\"Awaiting your reply: Land l1 first?\">Awaiting your reply</a>"
        )) && others.contains("<span class=\"ar-from meta\">from <span class=\"sref\">"),
        "{others}"
    );
    assert!(
        others.contains(&format!(
            "<a class=\"ar-title\" href=\"/projects/id/{id}/inbox#item-{id}-{orchestrator}\">Accept revision 3?</a>"
        )),
        "{others}"
    );
    assert!(
        page.find("<ul class=\"ask-rows\"").unwrap() < page.find("<ul class=\"now\"").unwrap(),
        "the questions lead the card"
    );
    let row = between(&page, "#step:l1-work\"", "</li>");
    assert!(!row.contains("tag ask"), "{row}");

    // the plan: its summary counts them in coral, the matrix row and the box's card say so
    let (_, plan) = f.get(&format!("/projects/id/{id}")).await;
    assert!(
        plan.contains(&format!(
            "<a class=\"tag ask\" href=\"/projects/id/{id}/inbox\">3 questions for you</a>"
        )),
        "{plan}"
    );
    let l1 = between(&plan, "<tr id=\"unit-l1\"", "</tr>");
    assert!(
        l1.contains(&format!(
            "<p class=\"card-ask\"><a class=\"tag ask\" href=\"/projects/id/{id}/steps/l1-work#ov-message-{lane}\""
        )),
        "{l1}"
    );
    let l2 = between(&plan, "<tr id=\"unit-l2\"", "</tr>");
    assert!(!l2.contains("card-ask"), "{l2}");
    let kit = between(&plan, "<section id=\"unit-kit\"", "</section>");
    assert!(
        kit.contains(&format!("steps/kit-a#ov-message-{boxed}\"")),
        "{kit}"
    );
    // coral is spent on nothing else on the plan
    assert_eq!(plan.matches("class=\"tag ask\"").count(), 3, "{plan}");
}

#[tokio::test]
async fn a_steps_overview_draws_its_open_question_whole_to_answer_and_its_header_leads_there() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let q = ask(
        &f,
        id,
        "l1-work",
        "Land l1 first?",
        "Land l1 first?\n\nThe rename touches `exec.rs`.\n\n- **Land now:** l2 rebases.\n- **Wait:** l1 rebases.",
    )
    .await;
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/l1-work")).await;
    // the header's chip is the coral one, to the question on Overview
    let actions = between(&page, "<div class=\"d-actions\">", "</header>");
    assert!(
        actions.contains(&format!(
            "<a class=\"tag ask\" href=\"#ov-message-{q}\" title=\"Land l1 first?\""
        )),
        "{actions}"
    );
    assert!(!actions.contains("awaiting reply"), "{actions}");
    // Overview: first, above Now, whole, its list kept, with Answer and Close in the shared
    // component
    let asked = between(&page, "<section class=\"d-sec d-ask\">", "</section>");
    assert!(asked.contains("<h3>Its question for you</h3>"), "{asked}");
    assert!(
        page.find("d-sec d-ask").unwrap() < page.find("d-sec d-now").unwrap(),
        "the question comes before Now"
    );
    let now = between(&page, "<section class=\"d-sec d-now\">", "</section>");
    assert!(!now.contains(&format!("ov-message-{q}")), "{now}");
    let item = between(asked, &format!("id=\"ov-message-{q}\""), "</sluice-answer>");
    assert!(
        item.contains("<li><strong>Land now:</strong> l2 rebases.</li>"),
        "{item}"
    );
    assert!(!item.contains("Read it whole"), "{item}");
    assert!(item.contains("<sluice-answer"), "{item}");
    assert!(
        item.contains(&format!("aria-controls=\"ov-qbox-{id}-{q}\"")),
        "{item}"
    );
    assert!(item.contains(">Answer</button>"), "{item}");
    assert!(item.contains(">Close question</button>"), "{item}");
    // the line it becomes once answered, the server's one sentence, waits in a template
    assert!(
        item.contains(&format!(
            "<template data-done=\"answer\"><p class=\"q-answered\" tabindex=\"-1\" data-q=\"{id}-{q}\">"
        )) && item.contains("Answered just now: <span class=\"qa-title\">Land l1 first?</span>"),
        "{item}"
    );
    // the same question on Messages keeps its own ids: none is drawn twice
    for prefix in ["qbox-", "answer-", "reply-"] {
        assert_eq!(
            page.matches(&format!("id=\"{prefix}{id}-{q}\"")).count(),
            1,
            "{prefix}"
        );
        assert_eq!(
            page.matches(&format!("id=\"ov-{prefix}{id}-{q}\"")).count(),
            1,
            "{prefix}"
        );
    }
    assert_eq!(page.matches(&format!("id=\"message-{q}\"")).count(), 1);
    assert_eq!(page.matches(&format!("id=\"ov-message-{q}\"")).count(), 1);
}

#[tokio::test]
async fn an_answer_is_confirmed_in_its_place_with_or_without_script() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let answered = ask(&f, id, "l1-work", "Land l1 first?", "Yes or no.").await;
    let waiting = ask(
        &f,
        id,
        "orchestrator",
        "Accept revision 3?",
        "It drops `span`.",
    )
    .await;
    answer(&f, id, answered, "l1-work").await;
    let (_, inbox) = f.get("/inbox").await;
    // the answered question keeps its place as one line; the open one is still a card
    let line = between(
        &inbox,
        &format!("<article class=\"item q q-done\" id=\"item-{id}-{answered}\">"),
        "</article>",
    );
    assert!(
        line.contains("<span class=\"qa-what\">Answered <time"),
        "{line}"
    );
    assert!(
        line.contains("<span class=\"qa-title\">Land l1 first?</span>"),
        "{line}"
    );
    assert!(
        line.contains("sent to <span class=\"who who-step\">"),
        "{line}"
    );
    assert!(
        line.contains(&format!(
            "thread?thread=step-l1-work#message-{answered}\">Read the thread</a>"
        )),
        "{line}"
    );
    assert!(inbox.contains(&format!(
        "<article class=\"item q\" id=\"item-{id}-{waiting}\">"
    )));
    // the count says what waits, the answered one not among them
    assert!(
        inbox.contains("<title>1 question · Inbox · sluice</title>"),
        "{inbox}"
    );
    // the card's first line is its project, not its thread's internal name
    let card = between(&inbox, &format!("id=\"item-{id}-{waiting}\""), "</article>");
    assert!(card.contains(&format!("<p class=\"meta q-where\"><a href=\"/projects/id/{id}/thread?thread=owner\">titled</a></p>")), "{card}");

    // without script the words-only form goes back where it was sent from, to its place
    for (referer, to) in [
        (
            "http://127.0.0.1:1/inbox".to_owned(),
            format!("/inbox#item-{id}-{waiting}"),
        ),
        (
            format!("http://127.0.0.1:1/projects/id/{id}/steps/l1-work?tab=messages#x"),
            format!("/projects/id/{id}/steps/l1-work?tab=messages"),
        ),
    ] {
        let response = f
            .router()
            .oneshot(
                Request::post(format!("/projects/id/{id}/messages/{waiting}/reply"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("referer", referer)
                    .body(Body::from("body=Accept+it"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 303);
        assert_eq!(response.headers()["location"], to.as_str());
        let _ = to_bytes(response.into_body(), usize::MAX).await;
    }
}

#[tokio::test]
async fn every_rerun_says_on_runs_what_started_it() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "reruns",
            json!({"steps":{"w":{"run":"custom.open"},"s":{"run":"custom.open"}}}),
            &[("w", "succeeded"), ("s", "succeeded")],
        )
        .await;
    let ok = || Some(json!({"status":"succeeded"}));
    run(
        &f,
        id,
        "w",
        "2026-10-01T00:00:00Z",
        Some("2026-10-01T02:00:00Z"),
        ok(),
    )
    .await;
    let retry = |author: &str, reason: &str| Event::StepRetry {
        rev: Revision(1),
        author: author.into(),
        reason: reason.into(),
        step: StepId::new("w").unwrap(),
        work: WorkGeneration(2),
    };
    record(
        &f,
        id,
        retry("w-land", "conditional Rejected retry"),
        Some("2026-10-01T02:00:20Z"),
    )
    .await;
    run(
        &f,
        id,
        "w",
        "2026-10-01T02:00:30Z",
        Some("2026-10-01T02:30:00Z"),
        ok(),
    )
    .await;
    record(
        &f,
        id,
        retry("owner", "rebase onto ce4fc38"),
        Some("2026-10-01T03:00:00Z"),
    )
    .await;
    run(
        &f,
        id,
        "w",
        "2026-10-01T03:00:10Z",
        Some("2026-10-01T03:20:00Z"),
        ok(),
    )
    .await;
    // s: ran again after an input changed
    run(
        &f,
        id,
        "s",
        "2026-10-01T00:00:00Z",
        Some("2026-10-01T00:10:00Z"),
        ok(),
    )
    .await;
    record(
        &f,
        id,
        Event::StepStatus {
            step: StepId::new("s").unwrap(),
            from: Some(sluice_model::commands::StepStatus::Stale),
            to: sluice_model::commands::StepStatus::Pending,
            error: None,
            run_ids: vec![],
            needs: Default::default(),
        },
        Some("2026-10-01T01:00:00Z"),
    )
    .await;
    run(
        &f,
        id,
        "s",
        "2026-10-01T01:00:05Z",
        Some("2026-10-01T01:05:00Z"),
        ok(),
    )
    .await;

    let (_, page) = f.get(&format!("/projects/id/{id}/steps/w?tab=runs")).await;
    let first = between(&page, "<li id=\"run-1\"", "</li>");
    assert!(!first.contains("a-why"), "{first}");
    let second = between(&page, "<li id=\"run-2\"", "</li>");
    assert!(
        second.contains("<p class=\"a-why\"><code>w-land</code> sent it back.</p>"),
        "{second}"
    );
    let third = between(&page, "<li id=\"run-3\"", "</li>");
    assert!(
        third.contains("<p class=\"a-why\">You retried it: rebase onto ce4fc38.</p>"),
        "{third}"
    );
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/s?tab=runs")).await;
    let second = between(&page, "<li id=\"run-2\"", "</li>");
    assert!(
        second.contains("<p class=\"a-why\">An input it reads changed, so it ran again.</p>"),
        "{second}"
    );
}

#[tokio::test]
async fn a_pending_step_names_what_holds_its_chain_and_a_stopped_runner() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "chain",
            json!({"steps":{
                "a":{"run":"custom.open","doc":"Lands the rename"},
                "e":{"run":"custom.open"},
                "b":{"run":"custom.open","after":["e","a"]},
                "c":{"run":"custom.open","after":["b"]},
                "d":{"run":"custom.open","after":["a"]}}}),
            // e is done: it holds nothing, so the chain is named through a, not e
            &[("a", "running"), ("e", "succeeded")],
        )
        .await;
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/c")).await;
    let waits = between(&page, "<dl class=\"facts\">", "</dl>");
    let root = between(waits, "<p class=\"gate wait-root\">", "</p>");
    assert!(
        root.contains("<span class=\"wr-lead\">Held up by</span>"),
        "{root}"
    );
    assert!(
        root.contains(&format!("href=\"/projects/id/{id}/steps/a\"")),
        "{root}"
    );
    assert!(root.contains("(running), before <a href="), "{root}");
    assert!(root.contains("<code>b</code>"), "{root}");
    // the fixture's home has no runner: a pending step says nothing starts
    assert!(
        waits
            .contains("The runner is stopped: nothing starts until <code>sluice loop</code> runs."),
        "{waits}"
    );
    // a step whose own wait is running needs no more: its wait says it
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/b")).await;
    assert!(!page.contains("wait-root"), "{page}");
    // nor does a running step say the runner is stopped
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/a")).await;
    assert!(!page.contains("wait-runner"));
}

#[tokio::test]
async fn the_log_keeps_bookkeeping_quiet_and_a_steps_quick_changes_on_one_row() {
    use sluice_model::commands::StepStatus;
    let f = Fixture::new().await;
    let id = f
        .project(
            "quiet",
            json!({"steps":{"s":{"run":"custom.open"},"t":{"run":"custom.open"}}}),
            &[],
        )
        .await;
    let status = |step: &str, from: StepStatus, to: StepStatus| Event::StepStatus {
        step: StepId::new(step).unwrap(),
        from: Some(from),
        to,
        error: None,
        run_ids: vec![],
        needs: Default::default(),
    };
    let chore = |rev: u64| Event::PlanEdit {
        rev: Revision(rev),
        author: "sluice".into(),
        reason: "retire done units older than 6h".into(),
        ops: vec![],
    };
    record(&f, id, chore(7), None).await;
    record(
        &f,
        id,
        status("s", StepStatus::Pending, StepStatus::Running),
        None,
    )
    .await;
    record(
        &f,
        id,
        status("t", StepStatus::Pending, StepStatus::Running),
        None,
    )
    .await;
    record(
        &f,
        id,
        status("s", StepStatus::Running, StepStatus::Succeeded),
        None,
    )
    .await;
    record(&f, id, chore(8), None).await;
    record(
        &f,
        id,
        Event::ProjectNotify {
            message: sluice_model::ids::MessageId(5),
            outcome: serde_json::from_value(json!("reserved")).unwrap(),
            error: None,
        },
        None,
    )
    .await;
    record(&f, id, chore(9), None).await;
    let (_, log) = f.get(&format!("/projects/id/{id}/log")).await;
    let rows = between(&log, "<tbody id=\"log-rows\">", "</tbody>");
    assert!(!rows.contains("Notification for message"), "{rows}");
    // the chores either side of other records are one row
    assert_eq!(
        rows.matches("class=\"log-what chore\"").count(),
        1,
        "{rows}"
    );
    assert!(
        rows.contains("sluice retired done units 3 times: plan revs 7 to 9"),
        "{rows}"
    );
    // s went pending → running → succeeded within a minute, t's change between: one row
    assert!(rows.contains("pending → running → succeeded"), "{rows}");
    assert_eq!(rows.matches("/steps/s\">").count(), 1, "{rows}");
    assert_eq!(rows.matches("pending → running").count(), 2, "{rows}");
    // asked for by name, the notification is listed
    let (_, log) = f
        .get(&format!("/projects/id/{id}/log?kinds=project.notify"))
        .await;
    assert!(log.contains("Notification for message 5"), "{log}");
}

#[tokio::test]
async fn the_small_fixes_read_as_they_should() {
    let f = Fixture::new().await;
    // a live run's files open at their end, and a long file has a way there
    let id = f
        .project(
            "files",
            json!({"steps":{"s":{"run":"custom.open"}}}),
            &[("s", "running")],
        )
        .await;
    let live = run(&f, id, "s", "2026-10-01T00:00:00Z", None, None).await;
    let dir = f._home.path().join("runs").join(live.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("stderr.log"), "line\n".repeat(120)).unwrap();
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/s?tab=runs")).await;
    assert!(
        page.contains(&format!(
            "href=\"/projects/id/{id}/runs/{live}/files/stderr.log#file-end\""
        )),
        "{page}"
    );
    let (_, file) = f
        .get(&format!("/projects/id/{id}/runs/{live}/files/stderr.log"))
        .await;
    assert!(
        file.contains("<a class=\"jump\" href=\"#file-end\">"),
        "{file}"
    );
    assert!(
        file.contains(
            "<p class=\"meta run-file-end\" id=\"file-end\">End of the file as read.</p>"
        ),
        "{file}"
    );

    // readable types: a union of null and a record by its fields; a list of strings as itself
    use sluice_web::views::ui::type_words;
    assert_eq!(
        type_words(
            r#"["null",{"type":"record","fields":{"head_before":"string","commits":"int"}}]"#
        ),
        "record? (head_before, commits)"
    );
    assert_eq!(type_words("string[]"), "string[]");
    assert_eq!(
        type_words(r#"{"type":"enum","symbols":["low","high"]}"#),
        "enum (low | high)"
    );
    assert_eq!(type_words(r#"{"type":"object"}"#), "object");

    // the owner's thread is named for who talks on it, its open question first on History
    let q = ask(
        &f,
        id,
        "orchestrator",
        "Accept revision 3?",
        "It drops `span`.",
    )
    .await;
    let (_, history) = f.get("/history").await;
    let card = between(&history, "You and the orchestrator", "</article>");
    assert!(card.contains("<p class=\"t-ask\"><span class=\"t-ask-title\">Accept revision 3?</span><a class=\"tag ask\""), "{card}");
    assert!(card.contains(&format!("#message-{q}\"")), "{card}");
}

#[tokio::test]
async fn a_units_unreached_stages_are_one_line_of_its_timeline() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    run(
        &f,
        id,
        "l1-fork",
        "2026-10-01T00:00:00Z",
        Some("2026-10-01T00:01:00Z"),
        Some(json!({"status":"succeeded"})),
    )
    .await;
    run(
        &f,
        id,
        "l3-fork",
        "2026-10-01T00:00:00Z",
        Some("2026-10-01T00:01:00Z"),
        Some(json!({"status":"succeeded"})),
    )
    .await;
    // l3: its fork ran, its work and land have not: one line, each a link
    let (_, page) = f.get(&format!("/projects/id/{id}/units/l3")).await;
    let rest = between(&page, "<li class=\"tl-row tl-rest\">", "</li>");
    assert!(
        rest.contains(&format!(
            "<a href=\"/projects/id/{id}/steps/l3-work\">work</a>, <a href=\"/projects/id/{id}/steps/l3-land\">land</a>: no run yet."
        )),
        "{rest}"
    );
    assert!(!page.contains("No run yet."), "{page}");
}
