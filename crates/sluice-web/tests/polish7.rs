//! The seventh critique's fixes, each through the pages it changed: every Details panel opens
//! inside the window; a cancel is dismissed from its card in Stopped and undone from the line
//! Stopped then says, with or without script; and who cancelled a run, and why, is read from
//! the run once the log has trimmed the record that said it.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
mod plan_html;
#[allow(dead_code)]
#[path = "../../../tests/support/messages.rs"]
mod stored_messages;
use axum::{
    Extension, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::json;
use sluice_model::{
    commands::StepDismiss,
    error::PublicError,
    ids::{AttemptId, ProjectId, RunId},
};
use sluice_store::{RetrySafety, Writer};
use sluice_web::views::step::{Action, CommandService, Commands, OwnerCommand};
use std::{future::Future, pin::Pin, sync::Arc};
use stored_messages::{Stored, stored};
use tower::ServiceExt;

fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from} in {html}"));
    let end = html[start..].find(to).map_or(html.len(), |e| start + e);
    &html[start..end]
}
fn title(html: &str) -> &str {
    between(html, "<title>", "</title>")
}

/// Dismiss and Undismiss as the coordinator takes them: the owner's mark in the store.
struct Marks(Writer);
impl CommandService for Marks {
    fn execute(
        &self,
        command: OwnerCommand,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicError>> + Send + '_>> {
        Box::pin(async move {
            let dismissed = match command.action {
                Action::Dismiss => true,
                Action::Undismiss => false,
                _ => {
                    return Err(PublicError::BadRequest {
                        message: "only marks here".into(),
                    });
                }
            };
            let (project, step) = (command.project, command.step.expect("a step"));
            self.0
                .write(RetrySafety::Idempotent, move |tx| {
                    sluice_store::messages::dismiss(
                        tx,
                        StepDismiss {
                            project,
                            step,
                            dismissed,
                        },
                    )
                })
                .await
        })
    }
}
fn router(f: &Fixture) -> Router {
    f.router()
        .layer(Extension(Commands(Arc::new(Marks(f.writer.clone())))))
}
async fn serve(router: Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (
        addr,
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    )
}
async fn get(router: &Router, path: &str) -> String {
    let response = router
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(body.to_vec()).unwrap()
}
/// A browser's form post (no script): where it is sent on to.
async fn post(router: &Router, path: &str, body: &str) -> String {
    let response = router
        .clone()
        .oneshot(
            Request::post(path)
                .header("content-type", "application/x-www-form-urlencoded")
                .header("accept", "text/html")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER, "{path} {body}");
    response.headers()["location"].to_str().unwrap().to_owned()
}

/// A project `aside`: unit `u` of `a` (succeeded) and `b` (cancelled by the owner), and `c`
/// running on its own.
async fn aside(f: &Fixture) -> ProjectId {
    let id = f
        .project(
            "aside",
            json!({"steps":{
                "a":{"run":"custom.open","tags":["unit:u"]},
                "b":{"run":"custom.open","tags":["unit:u"],"after":["a"]},
                "c":{"run":"custom.open"}}}),
            &[("a", "succeeded"), ("b", "failed"), ("c", "running")],
        )
        .await;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let cancel = PublicError::Cancelled {
                message: "pivot".into(),
            };
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='b'",
                (id.to_string(), serde_json::to_string(&cancel).unwrap()),
            )?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    id
}

/// A cancelled card in Stopped carries a quiet Dismiss, a plain form; posted, the card leaves
/// Stopped, which says "Dismissed: u · Undo" in a polite status line, the summary sentence and
/// the title no longer count it, and Done's index reads it cancelled and dismissed; Undo
/// brings the card back. A failed card has no Dismiss.
#[tokio::test]
async fn a_cancel_is_dismissed_from_its_card_and_undone_from_the_line_stopped_then_says() {
    let f = Fixture::new().await;
    let id = aside(&f).await;
    let app = router(&f);
    let plan = format!("/projects/id/{id}");
    let page = get(&app, &plan).await;
    assert_eq!(plan_html::place(&page, "u"), "stopped", "{page}");
    assert!(title(&page).contains("1 cancelled"), "{}", title(&page));
    let card = plan_html::band(&page, "s-u");
    let form = between(card, "<form class=\"pl-dismiss\"", "</form>");
    assert!(
        form.contains(&format!("action=\"/projects/id/{id}/steps/b/actions\""))
            && form.contains("<input type=\"hidden\" name=\"action\" value=\"dismiss\">")
            && form.contains(&format!(
                "<input type=\"hidden\" name=\"next\" value=\"{plan}\">"
            ))
            && form.contains("<button class=\"quiet-act\" aria-label=\"Dismiss u\""),
        "{form}"
    );
    // no confirm: it is undone in a click
    assert!(!card.contains("confirm-flow"), "{card}");
    // the step's band offers it too
    let step = get(&app, &format!("{plan}/steps/b")).await;
    let band = between(&step, "<header id=\"step-band\"", "</header>");
    assert!(band.contains("<form class=\"d-dismiss-b\""), "{band}");

    // without script: the form posts, and comes back to the plan
    let back = post(
        &app,
        &format!("{plan}/steps/b/actions"),
        &format!("action=dismiss&next={plan}"),
    )
    .await;
    assert_eq!(back, plan);
    let page = get(&app, &plan).await;
    assert_eq!(plan_html::place(&page, "u"), "done", "{page}");
    assert!(!title(&page).contains("cancelled"), "{}", title(&page));
    assert!(!plan_html::summary(&page).contains("cancelled"), "{page}");
    let stopped = plan_html::band(&page, "plan-stopped");
    assert!(
        !stopped.contains("pl-item pl-stop"),
        "no card is left: {stopped}"
    );
    let line = between(
        stopped,
        "<div class=\"pl-dismissed\" role=\"status\">",
        "</div>",
    );
    assert!(
        line.contains("Dismissed: <a href=")
            && line.contains(">u (b)</a>")
            && line.contains("<input type=\"hidden\" name=\"action\" value=\"undismiss\">")
            && line.contains(">Undo</button>"),
        "{line}"
    );
    let done = plan_html::band(&page, "plan-done");
    assert!(done.contains(">b cancelled and dismissed</span>"), "{done}");
    // its page says so, with its Undo
    let step = get(&app, &format!("{plan}/steps/b")).await;
    assert!(
        step.contains("value=\"undismiss\"><p class=\"meta\">Dismissed:"),
        "{step}"
    );
    assert!(!step.contains("d-dismiss-b"), "{step}");

    // Undo: the card is back, the line gone
    let back = post(
        &app,
        &format!("{plan}/steps/b/actions"),
        &format!("action=undismiss&next={plan}"),
    )
    .await;
    assert_eq!(back, plan);
    let page = get(&app, &plan).await;
    assert_eq!(plan_html::place(&page, "u"), "stopped", "{page}");
    assert!(!page.contains("pl-dismissed"), "{page}");
    assert!(title(&page).contains("1 cancelled"), "{}", title(&page));

    // a failure is never dismissed: its card has no Dismiss
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let error = PublicError::BadRequest {
                message: "tests failed".into(),
            };
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='b'",
                (id.to_string(), serde_json::to_string(&error).unwrap()),
            )?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    let page = get(&app, &plan).await;
    let card = plan_html::band(&page, "s-u");
    assert!(!card.contains("pl-dismiss"), "{card}");
}

/// In Chromium: Dismiss takes the card out of Stopped and puts the line with its Undo there;
/// Undo brings the card back.
#[tokio::test(flavor = "multi_thread")]
async fn in_chromium_dismiss_and_undo_move_the_card_out_of_stopped_and_back() {
    let f = Fixture::new().await;
    let id = aside(&f).await;
    let (addr, server) = serve(router(&f)).await;
    tokio::task::spawn_blocking(move || {
        let plan = format!("http://{addr}/projects/id/{id}");
        let mut browser = Chrome::open(&plan).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("document.querySelector('#s-u .pl-dismiss button')?.checkVisibility()")
            .unwrap();
        browser
            .eval("document.querySelector('#s-u .pl-dismiss button').click()")
            .unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelector('.pl-dismissed') && !document.querySelector('#s-u.pl-stop')")
            .unwrap();
        let line = browser
            .eval("document.querySelector('.pl-dismissed').textContent.replace(/\\s+/g, ' ').trim()")
            .unwrap();
        assert_eq!(line, json!("Dismissed: u (b) · Undo"));
        browser
            .eval("document.querySelector('.pl-dismissed button').click()")
            .unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelector('#s-u.pl-stop') && !document.querySelector('.pl-dismissed')")
            .unwrap();
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

/// Every Details panel on the plan, a step, a unit, home and the inbox opens wholly inside
/// the window, at a phone's width, a laptop's and a wide screen's (home draws none today; it
/// is checked so one added there is held too).
#[tokio::test(flavor = "multi_thread")]
async fn every_details_panel_opens_inside_the_window_at_every_width() {
    let f = Fixture::new().await;
    let id = f.id;
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
    let (addr, server) = serve(f.router()).await;
    tokio::task::spawn_blocking(move || {
        let base = format!("http://{addr}");
        let pages = [
            format!("/projects/id/{id}"),
            format!("/projects/id/{id}/steps/beta-build"),
            format!("/projects/id/{id}/units/alpha"),
            "/".to_owned(),
            "/inbox".to_owned(),
        ];
        // each visible "⋯" in turn: open it, measure its panel against the window, close it
        const OPEN_EACH: &str = "(async () => { const frame = () => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))); const out = []; const all = [...document.querySelectorAll('details.dm')].filter(d => d.querySelector(':scope > summary').checkVisibility()); for (const d of all) { const s = d.querySelector(':scope > summary'); s.scrollIntoView({block: 'center'}); s.click(); await frame(); const p = d.querySelector(':scope > .dm-p').getBoundingClientRect(), w = document.documentElement.clientWidth; if (!d.open || p.left < 0 || p.right > w + 0.5 || p.width === 0) out.push([s.getAttribute('aria-label'), Math.round(p.left), Math.round(p.right), w]); if (d.open) s.click(); await frame(); } return [all.length, out, document.documentElement.scrollWidth <= innerWidth]; })()";
        let mut browser = Chrome::open(&format!("{base}{}", pages[0])).unwrap();
        for width in [390, 1440, 2560] {
            browser.viewport(width, "light").unwrap();
            for page in &pages {
                browser.navigate(&format!("{base}{page}")).unwrap();
                browser.wait("document.readyState === 'complete'").unwrap();
                browser.eval("document.fonts.ready").unwrap();
                let got = browser.eval(OPEN_EACH).unwrap();
                // home draws none of its own; every other page has some
                assert!(
                    page == "/" || got[0].as_u64().unwrap() > 0,
                    "{page} at {width}: no Details drawn"
                );
                assert_eq!(got[1], json!([]), "{page} at {width}: {got}");
                assert_eq!(got[2], json!(true), "{page} at {width} scrolls sideways");
            }
        }
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

/// Who cancelled a run, and why, is kept on the run: once the log has trimmed the record
/// that said it, its Runs still says "Cancelled after 1h 18m by you: switch to the new brief."
#[tokio::test]
async fn who_cancelled_a_run_and_why_is_read_from_the_run_once_the_log_trimmed_it() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "kept",
            json!({"steps":{"w":{"run":"custom.open"}}}),
            &[("w", "running")],
        )
        .await;
    let run = RunId::new();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let attempt = AttemptId::new();
            tx.sql().execute(
                "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?1,?2,'w',1,1,'terminal','{}','hash','2026-10-01T00:00:00Z')",
                (attempt.to_string(), id.to_string()),
            )?;
            tx.sql().execute(
                "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at,finished_at,result,stopped) VALUES (?1,?2,?3,'w',1,1,'2026-10-01T00:00:00Z','2026-10-01T00:00:00Z','2026-10-01T01:18:00Z',?4,?5)",
                (
                    run.to_string(),
                    id.to_string(),
                    attempt.to_string(),
                    json!({"status":"failed","error":{"error":"cancelled","message":"cancel requested"}}).to_string(),
                    json!({"cancel":{"author":"owner","reason":"switch to the new brief","at":"2026-10-01T01:17:58Z"}}).to_string(),
                ),
            )?;
            // the log keeps no step.cancel for it
            tx.sql().execute(
                "DELETE FROM records WHERE project_id=?1 AND kind='step.cancel'",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/w?tab=runs")).await;
    let first = between(&page, "<li id=\"run-1\"", "</li>");
    assert!(
        first.contains(
            "<p class=\"a-err\">Cancelled after 1h 18m by you: switch to the new brief.</p>"
        ),
        "{first}"
    );
}
