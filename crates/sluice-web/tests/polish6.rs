//! The sixth critique's fixes, each through the pages it changed. A pending step nothing holds
//! says it is ready and when it starts, never "Waits on" the runner alone; a failure that
//! repeats its run before says so and opens the retry's feedback; a lane row whose step asks
//! the owner says its question, and a note sent to several rests on the rows with nothing of
//! their own, is one row on the log and one message on the unit page; a question to the
//! orchestrator shows its answer under it on Overview; the questions nobody waits on say why in
//! the owner's words; the agent docs lead with the overview. In Chromium: a markdown code block
//! is one box keeping its lines, `/` on a step's page finds on its plan, `[` and `]` move the
//! drawer through the board, the board alone keeps the summary's tags, and a wide screen sets a
//! step's Overview two to a row.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
#[allow(dead_code)]
#[path = "../../../tests/support/messages.rs"]
mod stored_messages;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::json;
use sluice_model::{
    error::PublicError,
    ids::{AttemptId, MessageId, ProjectId, RunId},
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

/// One run of `step`, from `started` to `finished`, with `result`.
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

/// A fn's failure, as a run's result and a failed step keep it.
fn fn_failure(message: &str) -> serde_json::Value {
    serde_json::to_value(PublicError::FnFailure {
        message: message.into(),
    })
    .unwrap()
}

/// A step's stored error, as a failed step keeps it.
async fn fail(f: &Fixture, project: ProjectId, step: &'static str, message: &'static str) {
    let message = fn_failure(message).to_string();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='failed',error=?3 WHERE project_id=?1 AND step_id=?2",
                (project.to_string(), step, message),
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
}

async fn say(
    f: &Fixture,
    project: ProjectId,
    thread: &'static str,
    from: &'static str,
    to: &'static str,
    body: &'static str,
) -> i64 {
    stored(
        &f.writer,
        project,
        Stored {
            thread,
            from,
            to: Some(to),
            body,
            ..Default::default()
        },
    )
    .await
    .id
    .0
}

async fn serve(f: &Fixture) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (
        addr,
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    )
}

fn press(browser: &mut Chrome, key: &str, code: &str) {
    for kind in ["keyDown", "keyUp"] {
        browser
            .send(
                "Input.dispatchKeyEvent",
                json!({"type": kind, "key": key, "code": code, "text": if kind == "keyDown" { key } else { "" }}),
            )
            .unwrap();
    }
}

#[tokio::test]
async fn a_ready_step_says_so_and_when_it_starts_never_waits_on_the_runner_alone() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "ready",
            json!({"steps":{
                "a":{"run":"custom.open"},
                "b":{"run":"custom.open","after":["a"]}}}),
            &[("a", "pending"), ("b", "pending")],
        )
        .await;
    // the fixture's home has no runner: a pending step nothing holds starts when one runs
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/a")).await;
    let ready = between(&page, "<p class=\"wait-runner d-ready\">", "</p>");
    assert!(
        ready.contains(
            "Ready, but the runner is stopped: it starts when <code>sluice loop</code> runs."
        ),
        "{ready}"
    );
    // not under a "Waits on" of the runner alone, and no empty state's "Waits for …" either
    assert!(!page.contains("<dl class=\"facts\">"), "{page}");
    assert!(!page.contains("empty-state"), "{page}");
    // a step that waits says what holds it, with the runner line under its waits, not ready
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/b")).await;
    assert!(!page.contains("d-ready"), "{page}");
    let waits = between(&page, "<dl class=\"facts\">", "</dl>");
    assert!(
        waits
            .contains("The runner is stopped: nothing starts until <code>sluice loop</code> runs."),
        "{waits}"
    );
}

#[tokio::test]
async fn a_failure_that_repeats_its_run_before_says_so_and_opens_the_feedback() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "repeats",
            json!({"steps":{"same":{"run":"custom.open"},"other":{"run":"custom.open"}}}),
            &[],
        )
        .await;
    let flagged = "This content was flagged for possible risk; try rephrasing your request.";
    for step in ["same", "other"] {
        run(
            &f,
            id,
            step,
            "2026-10-06T09:00:00Z",
            Some("2026-10-06T09:04:00Z"),
            Some(json!({"status":"failed","error": fn_failure(flagged)})),
        )
        .await;
    }
    run(
        &f,
        id,
        "same",
        "2026-10-06T10:00:00Z",
        Some("2026-10-06T10:04:00Z"),
        Some(json!({"status":"failed","error": fn_failure(flagged)})),
    )
    .await;
    run(
        &f,
        id,
        "other",
        "2026-10-06T10:00:00Z",
        Some("2026-10-06T10:04:00Z"),
        Some(json!({"status":"failed","error": fn_failure("The disk is full.")})),
    )
    .await;
    fail(&f, id, "same", flagged).await;
    fail(&f, id, "other", "The disk is full.").await;
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/same")).await;
    let next = between(&page, "<p class=\"meta err-next\">", "</p>");
    assert!(
        next.contains("Run 1 failed the same way, so a bare Retry would most likely fail again. Retry with feedback, or change its inputs."),
        "{next}"
    );
    // its feedback box is open, to be written in
    assert!(
        page.contains(
            "<details data-preserve-attr=\"open\" open><summary>Feedback for retry</summary>"
        ),
        "{page}"
    );
    // a failure unlike the run before keeps its kind's advice and its box closed
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/other")).await;
    assert!(!page.contains("failed the same way"), "{page}");
    assert!(
        page.contains("<details data-preserve-attr=\"open\"><summary>Feedback for retry</summary>"),
        "{page}"
    );
}

#[tokio::test]
async fn a_lane_row_says_its_question_and_a_note_to_many_is_drawn_once() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    // l2's own note first, then l1's question to the owner and one note from l1 to l2 and l3
    say(
        &f,
        id,
        "step-l2-work",
        "l2-work",
        "orchestrator",
        "Parser leak found in the lexer.",
    )
    .await;
    stored(
        &f.writer,
        id,
        Stored {
            thread: "step-l1-work",
            from: "l1-work",
            to: Some("owner"),
            body: "Which way?",
            title: Some("Land the cron fix now?"),
            question: true,
            ..Default::default()
        },
    )
    .await;
    for to in ["l2-work", "l3-work"] {
        say(
            &f,
            id,
            "step-l1-work",
            "l1-work",
            to,
            "Heads up: main moved; rebase first.",
        )
        .await;
    }
    let (_, html) = f.get(&format!("/projects/id/{id}")).await;
    // the asking row's summary is its question, not its last note
    let l1 = between(&html, "<tr id=\"unit-l1\"", "</tr>");
    let sum = between(l1, "<td class=\"mx-sum\">", "</td>");
    assert!(
        sum.contains("<span class=\"uv-from\">Question for you</span>: Land the cron fix now?"),
        "{sum}"
    );
    assert!(!sum.contains("Heads up"), "{sum}");
    // a row with a message of its own says that one, not the note sent to it
    let l2 = between(&html, "<tr id=\"unit-l2\"", "</tr>");
    let sum = between(l2, "<td class=\"mx-sum\">", "</td>");
    assert!(sum.contains("Parser leak found"), "{sum}");
    // a row with nothing of its own says the note and who sent it
    let l3 = between(&html, "<tr id=\"unit-l3\"", "</tr>");
    let sum = between(l3, "<td class=\"mx-sum\">", "</td>");
    assert!(
        sum.contains("<span class=\"uv-from\">Note from l1-work</span>")
            && sum.contains("Heads up"),
        "{sum}"
    );
    // the log: the note is one row, its sender named as pages name a step and linked
    let (_, log) = f.get(&format!("/projects/id/{id}/log")).await;
    let rows: Vec<&str> = log
        .split("<tr id=\"r")
        .filter(|r| r.contains("Heads up"))
        .collect();
    assert_eq!(rows.len(), 1, "{log}");
    let row = rows[0];
    assert!(
        row.contains(&format!("<a href=\"/projects/id/{id}/steps/l1-work\""))
            && row.contains("<code class=\"sref-id\">l1-work</code></span></a> to 2 steps: <a href=\"/projects/id/"),
        "{row}"
    );
    // not "step-l1-work from l1-work to …": the thread is the link, not words
    assert!(!row.contains("step-l1-work from"), "{row}");
    let own = log
        .split("<tr id=\"r")
        .find(|r| r.contains("Parser leak"))
        .unwrap();
    assert!(own.contains("</a> to the orchestrator: <a href="), "{own}");
    // the unit page draws the note as its thread does: once, to 2 steps
    let (_, unit) = f.get(&format!("/projects/id/{id}/units/l3")).await;
    let last = between(&unit, "<section class=\"d-sec unit-last\"", "</section>");
    assert!(last.contains("<summary>to 2 steps</summary>"), "{last}");
    assert_eq!(last.matches("Heads up").count(), 1, "{last}");
    // the sender's Messages counts each copy and says the note is drawn once
    let (_, page) = f
        .get(&format!("/projects/id/{id}/steps/l1-work?tab=messages"))
        .await;
    assert!(
        page.contains("3 messages · the note to 2 steps shown once"),
        "{page}"
    );
}

#[tokio::test]
async fn a_question_to_the_orchestrator_shows_its_answer_under_it_on_overview() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "asked",
            json!({"steps":{"s":{"run":"custom.open"}}}),
            &[("s", "succeeded")],
        )
        .await;
    let asked = stored(
        &f.writer,
        id,
        Stored {
            thread: "step-s",
            from: "s",
            to: Some("orchestrator"),
            body: "Should Kiln change the route, or should I?",
            question: true,
            ..Default::default()
        },
    )
    .await
    .id;
    stored(
        &f.writer,
        id,
        Stored {
            thread: "step-s",
            from: "orchestrator",
            to: Some("s"),
            body: "Option (b) is live: re-measure.",
            reply: Some(MessageId(asked.0)),
            ..Default::default()
        },
    )
    .await;
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/s")).await;
    let last = between(&page, "<section class=\"d-sec d-last\">", "</section>");
    let replies = between(last, "<ol class=\"m-replies\">", "</ol>");
    assert!(
        replies.contains("Option (b) is live: re-measure."),
        "{replies}"
    );
    // an excerpt's reply carries no id: the Messages tab's copy has it
    assert!(!replies.contains("id=\"ov-message-"), "{replies}");
}

#[tokio::test]
async fn the_questions_nobody_waits_on_say_why_in_the_owners_words() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "stopped",
            json!({"steps":{"done":{"run":"custom.open"}}}),
            &[("done", "succeeded")],
        )
        .await;
    let asker = run(
        &f,
        id,
        "done",
        "2026-10-06T09:00:00Z",
        Some("2026-10-06T09:04:00Z"),
        Some(json!({"status":"succeeded"})),
    )
    .await;
    let asked = stored(
        &f.writer,
        id,
        Stored {
            thread: "step-done",
            from: "done",
            to: Some("owner"),
            body: "Ship it?",
            question: true,
            ..Default::default()
        },
    )
    .await
    .id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE messages SET run_id=?2 WHERE id=?1",
                (asked.0, asker.to_string()),
            )?;
            tx.changed(Some(id), "questions");
            Ok(())
        })
        .await
        .unwrap();
    let (_, page) = f.get("/questions").await;
    let stopped = between(&page, "<ul class=\"stopped-list\">", "</ul>");
    assert!(stopped.contains("stopped · done has finished"), "{stopped}");
    assert!(!stopped.contains("is succeeded"), "{stopped}");
}

#[tokio::test]
async fn the_agent_docs_lead_with_the_overview() {
    let f = Fixture::new().await;
    let (_, page) = f.get("/docs").await;
    let index = between(&page, "<ul class=\"docs-index\">", "</ul>");
    let order: Vec<&str> = index
        .split("<li><a href=\"/docs/")
        .skip(1)
        .map(|li| li.split('"').next().unwrap())
        .collect();
    assert_eq!(
        order,
        [
            "instructions",
            "plans",
            "types",
            "fns",
            "composing",
            "examples",
            "threads",
            "inbox",
            "board"
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_a_code_block_keeps_its_lines_and_the_keys_find_and_move() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    say(
        &f,
        id,
        "step-l1-work",
        "l1-work",
        "orchestrator",
        "The plan:\n\n```json\n{\"inputs\": {\"repo\": \"string\"},\n  \"steps\": {\"work\": {\"run\": \"agent.run\", \"in\": {\"engine\": {\"default\": \"devin\"}, \"cwd\": {\"source\": \"repo\"}, \"spec\": {\"source\": \"tasks\"}}}}}\n```",
    )
    .await;
    let (addr, server) = serve(&f).await;
    tokio::task::spawn_blocking(move || {
        let base = format!("http://{addr}/projects/id/{id}");
        let mut browser = Chrome::open(&format!("{base}/steps/l1-work?tab=messages")).unwrap();
        browser.viewport(390, "light").unwrap();
        browser.wait("document.querySelector('.m-body pre > code')").unwrap();
        // one box: the code in it is no chip, its lines and indentation kept, scrolling inside
        let code = browser
            .eval("(() => { const c = document.querySelector('.m-body pre > code'), p = c.parentElement, cs = getComputedStyle(c), ps = getComputedStyle(p); return [cs.borderTopWidth, cs.backgroundColor, ps.whiteSpace, p.scrollWidth > p.clientWidth, document.documentElement.scrollWidth <= innerWidth]; })()")
            .unwrap();
        assert_eq!(code, json!(["0px", "rgba(0, 0, 0, 0)", "pre", true, true]));

        // `/` on a step's page finds on its plan
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&format!("{base}/steps/l1-work")).unwrap();
        browser.wait("customElements.get('sluice-keys') && document.querySelector('a[data-find-at]')").unwrap();
        press(&mut browser, "/", "Slash");
        browser
            .wait(&format!("location.pathname === '/projects/id/{id}' && document.activeElement?.matches('input[data-find]')"))
            .unwrap();
        assert_eq!(browser.eval("location.hash").unwrap(), json!(""));

        // `]` and `[` move the drawer through the board, in its order as drawn
        browser.navigate(&format!("{base}#step:l1-work")).unwrap();
        browser
            .wait("customElements.get('sluice-drawer') && document.querySelector('#step-detail[data-step=\"l1-work\"] #d-title')")
            .unwrap();
        let order = browser
            .eval("[...new Set([...document.querySelectorAll('sluice-board a[data-step], sluice-board .mx-lane a[data-opens]')].filter(a => a.checkVisibility()).map(a => a.dataset.step || a.dataset.opens))]")
            .unwrap();
        let order: Vec<String> = serde_json::from_value(order).unwrap();
        let at = order.iter().position(|s| s == "l1-work").unwrap();
        let next = order[at + 1].clone();
        browser.eval("document.querySelector('#drawer').focus()").unwrap();
        press(&mut browser, "]", "BracketRight");
        browser
            .wait(&format!("location.hash === '#step:{next}' && document.querySelector('#step-detail[data-step=\"{next}\"] #d-title')"))
            .unwrap();
        press(&mut browser, "[", "BracketLeft");
        browser.wait("location.hash === '#step:l1-work'").unwrap();
        // typing in a field is the field's
        browser
            .eval("(() => { const t = document.createElement('textarea'); t.id = 'probe'; document.querySelector('#step-detail').append(t); t.focus(); })()")
            .unwrap();
        press(&mut browser, "]", "BracketRight");
        browser
            .wait("new Promise(r => setTimeout(() => r(true), 300))")
            .unwrap();
        assert_eq!(browser.eval("location.hash").unwrap(), json!("#step:l1-work"));
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_the_board_alone_keeps_its_tags_and_a_wide_screen_sets_overview_two_to_a_row() {
    let f = Fixture::new().await;
    // the fixture's own project has a board; `titled` a running step that asks
    let id = f.id;
    let wide = f.titled().await;
    stored(
        &f.writer,
        wide,
        Stored {
            thread: "step-l1-work",
            from: "l1-work",
            to: Some("owner"),
            body: "Which way?",
            title: Some("Land the cron fix now?"),
            question: true,
            ..Default::default()
        },
    )
    .await;
    stored(
        &f.writer,
        id,
        Stored {
            thread: "step-alpha-review",
            from: "alpha-review",
            to: Some("owner"),
            body: "Which way?",
            title: Some("Land the cron fix now?"),
            question: true,
            ..Default::default()
        },
    )
    .await;
    let (addr, server) = serve(&f).await;
    tokio::task::spawn_blocking(move || {
        let base = format!("http://{addr}/projects/id/{id}");
        let mut browser = Chrome::open(&base).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("document.querySelector('[data-view-tab=\"board\"]')?.checkVisibility()")
            .unwrap();
        browser
            .eval("document.querySelector('[data-view-tab=\"board\"]').click()")
            .unwrap();
        browser
            .wait("document.querySelector('.project-page[data-view=\"board\"]')")
            .unwrap();
        // the board alone keeps the counts and the coral question tag, not the bar
        let kept = browser
            .eval("[document.querySelector('#p-sum .sum-tags').checkVisibility(), document.querySelector('#p-sum .sum-tags').textContent.includes('1 question for you'), document.querySelector('#p-sum .bar')?.checkVisibility() ?? false]")
            .unwrap();
        assert_eq!(kept, json!([true, true, false]));
        browser
            .eval("document.querySelector('[data-view-tab=\"both\"]').click()")
            .unwrap();

        // a wide screen: the asking step's question and Now side by side, in one row
        browser.viewport(2400, "dark").unwrap();
        browser.navigate(&format!("http://{addr}/projects/id/{wide}/steps/l1-work")).unwrap();
        browser.wait("document.querySelector('#tp-overview .d-ask')").unwrap();
        let row = browser
            .eval("(() => { const o = document.querySelector('#tp-overview'), a = o.querySelector('.d-ask'), n = o.querySelector('.d-now'); return [getComputedStyle(o).display, Math.round(a.getBoundingClientRect().top) === Math.round(n.getBoundingClientRect().top), a.getBoundingClientRect().right < n.getBoundingClientRect().left, document.documentElement.scrollWidth <= innerWidth]; })()")
            .unwrap();
        assert_eq!(row, json!(["grid", true, true, true]));
        // the column is the frame's, fluid (the window less its gutters), the band's content on
        // the same edges
        let edges = browser
            .eval("(() => { const h = document.querySelector('#step-detail').getBoundingClientRect(), b = document.querySelector('.band-in'), cs = getComputedStyle(b); return [Math.round(h.width), Math.round(b.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight))]; })()")
            .unwrap();
        assert_eq!(edges[0], edges[1], "{edges}");
        assert!(edges[0].as_f64().unwrap() > 2200.0, "{edges}");
        // a narrow window keeps one column
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&format!("http://{addr}/projects/id/{wide}/steps/l1-work")).unwrap();
        browser.wait("document.querySelector('#tp-overview .d-ask')").unwrap();
        assert_eq!(
            browser
                .eval("getComputedStyle(document.querySelector('#tp-overview')).display")
                .unwrap(),
            json!("block")
        );
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}
