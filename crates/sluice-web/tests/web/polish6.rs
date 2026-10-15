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
use crate::board_fixture;
use crate::chrome;
use crate::plan_html;
use crate::stored_messages;
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

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_a_code_block_keeps_its_lines_and_the_keys_find_and_move() {
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

        // `]` and `[` move the drawer through the plan's steps, in its order as drawn
        browser.navigate(&format!("{base}#step:l1-work")).unwrap();
        browser
            .wait("customElements.get('sluice-drawer') && document.querySelector('#step-detail[data-step=\"l1-work\"] #d-title')")
            .unwrap();
        let order = browser
            .eval("[...new Set([...document.querySelectorAll('#plan-pane a[data-step]')].map(a => a.dataset.step))]")
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
        // back, and drawn: a field put in before its detail streams in would be replaced
        browser
            .wait("location.hash === '#step:l1-work' && document.querySelector('#step-detail[data-step=\"l1-work\"] #d-title')")
            .unwrap();
        // typing in a field is the field's
        browser
            .eval("(() => { const t = document.createElement('textarea'); t.id = 'probe'; document.querySelector('#step-detail').append(t); t.focus(); })()")
            .unwrap();
        press(&mut browser, "]", "BracketRight");
        browser
            .wait("new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r(true))))")
            .unwrap();
        assert_eq!(browser.eval("location.hash").unwrap(), json!("#step:l1-work"));
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test]
async fn ready_work_repeated_failure_and_answered_questions() {
    let f = Fixture::new().await;
    {
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
            ready.contains("Ready. Runner stopped: it starts when <code>sluice loop</code> runs."),
            "{ready}"
        );
        // not under a "Waits on" of the runner alone, and no empty state's "Waits for …" either
        assert!(
            !page.contains("<div class=\"wait-list\">") && !page.contains(">Waits on</h"),
            "{page}"
        );
        assert!(!page.contains("empty-state"), "{page}");
        // a step that waits says what holds it, with the runner line under its waits, not ready
        let (_, page) = f.get(&format!("/projects/id/{id}/steps/b")).await;
        assert!(!page.contains("d-ready"), "{page}");
        let waits = between(&page, "<div class=\"wait-list\">", "</article>");
        assert!(
            waits.contains("Runner stopped. No step starts until <code>sluice loop</code> runs."),
            "{waits}"
        );
    }
    {
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
        let next = between(&page, "<p class=\"err-next\">", "</p>");
        assert!(
            next.contains("Run 1 failed the same way. Retry with feedback, or change its inputs."),
            "{next}"
        );
        // its feedback box stands open under the advice, in Why it failed, to be written in after
        // reading why; the band keeps a plain Retry and no box of its own
        let why = between(&page, "<p class=\"err-next\">", "</form>");
        assert!(
        why.contains("class=\"err-retry\" method=\"post\"")
            && why.contains("<label for=\"why-feedback\">Feedback for its next run</label>")
            && why.contains("<button name=\"action\" value=\"retry\" class=\"primary\">Retry with this feedback</button>"),
        "{why}"
    );
        let band = between(&page, "<header id=\"step-band\"", "</header>");
        assert!(
            !band.contains("sh-fb") && band.contains(">Retry</button>"),
            "{band}"
        );
        // a failure unlike the run before keeps its kind's advice, and its box folded in the band
        // under the plan's own "Retry with feedback"
        let (_, page) = f.get(&format!("/projects/id/{id}/steps/other")).await;
        assert!(!page.contains("failed the same way"), "{page}");
        assert!(!page.contains("err-retry"), "{page}");
        assert!(
            page.contains(
                "<details class=\"sh-fb\" data-preserve-attr=\"open\"><summary class=\"btn primary\">"
            ) && page.contains("Retry with feedback</summary>"),
            "{page}"
        );
    }
    {
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
        // the asking unit is a question for you; the note it sent is not the question
        assert_eq!(plan_html::place(&html, "l1"), "asks");
        let q = plan_html::needs(&html)[0];
        assert!(
            q.contains("Land the cron fix now?") && !q.contains("Heads up"),
            "{q}"
        );
        // a stopped unit says why it stopped, not the note sent to it
        let l2 = plan_html::stopped(&html, "l2");
        assert!(
            l2.contains("</a></b> failed") && !l2.contains("Heads up"),
            "{l2}"
        );
        // l3 has not begun: the plan counts it, its page says the note
        assert_eq!(plan_html::place(&html, "l3"), "");
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
            && row.contains("<code class=\"sref-id\">l1-work</code></span></a> to 2 steps: <span class=\"lg-said\">Heads up"),
        "{row}"
    );
        // not "step-l1-work from l1-work to …": the thread is one small link after its words
        assert!(!row.contains("step-l1-work from"), "{row}");
        let own = log
            .split("<tr id=\"r")
            .find(|r| r.contains("Parser leak"))
            .unwrap();
        assert!(
            own.contains(
                "</a> to <span class=\"who-old\">orchestrator</span>: <span class=\"lg-said\">"
            ) && own.contains("</span> <a class=\"lg-thread\" href=\""),
            "{own}"
        );
        // the unit page says the note once, in its step view's foot, by its sender's stage
        let (_, unit) = f.get(&format!("/projects/id/{id}/units/l3")).await;
        let last = between(&unit, "<p class=\"sv-foot\">", "</p>");
        assert!(last.contains("<b>work</b>"), "{last}");
        assert_eq!(unit.matches("Heads up").count(), 1, "{unit}");
        // the sender's Messages counts each copy and does not explain that the note is drawn once
        let (_, page) = f
            .get(&format!("/projects/id/{id}/steps/l1-work?tab=messages"))
            .await;
        assert!(
            page.contains("3 messages") && !page.contains("shown once"),
            "{page}"
        );
    }
    {
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
        let last = between(&page, "<article class=\"mod d-sec d-last\"", "</article>");
        let replies = between(
            last,
            "<span class=\"mc-to\"> replied</span>",
            "</div></div>",
        );
        assert!(
            replies.contains("Option (b) is live: re-measure."),
            "{replies}"
        );
        // an excerpt's reply carries no id: the Messages tab's copy has it
        assert!(!replies.contains("id=\"ov-message-"), "{replies}");
    }
}
