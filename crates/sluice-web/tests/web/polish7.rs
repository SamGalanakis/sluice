//! The seventh critique's fixes, each through the pages it changed: every Details panel opens
//! inside the window; a cancel is set aside from Needs you and undone from the line it leaves,
//! in place; and who cancelled a run, and why, is read from the run once the log has trimmed
//! the record that said it.
use crate::board_fixture;
use crate::chrome;
use crate::neutral;
use axum::{Extension, Router};
use board_fixture::Fixture;
use chrome::Chrome;
use neutral::stored_messages;
use serde_json::json;
use sluice_model::{
    commands::{NeedsDismiss, StepDismiss},
    error::PublicError,
    ids::{AttemptId, ProjectId, RunId},
};
use sluice_store::{RetrySafety, Writer};
use sluice_web::views::step::{Action, CommandService, Commands, OwnerCommand};
use std::{future::Future, pin::Pin, sync::Arc};
use stored_messages::{Stored, stored};

fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from} in {html}"));
    let end = html[start..].find(to).map_or(html.len(), |e| start + e);
    &html[start..end]
}

/// The owner's marks as the coordinator takes them: Needs you's set-asides and a step's
/// Dismiss, each in the store.
struct Marks(Writer);
impl CommandService for Marks {
    fn execute(
        &self,
        command: OwnerCommand,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicError>> + Send + '_>> {
        Box::pin(async move {
            self.0
                .write(RetrySafety::Idempotent, move |tx| match command.action {
                    Action::SetAside | Action::Restore => sluice_store::messages::needs_dismiss(
                        tx,
                        NeedsDismiss {
                            project: command.project,
                            items: command.items,
                            dismissed: command.action == Action::SetAside,
                        },
                    ),
                    Action::Dismiss | Action::Undismiss => sluice_store::messages::dismiss(
                        tx,
                        StepDismiss {
                            project: command.project,
                            step: command.step.expect("a step"),
                            dismissed: command.action == Action::Dismiss,
                        },
                    ),
                    _ => Err(PublicError::BadRequest {
                        message: "only marks here".into(),
                    }
                    .into()),
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

/// In Chromium: the × on a cancel in Needs you sets it aside in place (the same document, the
/// scroll kept), for attention alone: its unit stays unsettled; Undo, which takes the focus,
/// brings it back; a × that never reaches sluice says so under the item and leaves it there.
#[tokio::test(flavor = "multi_thread")]
async fn slow_in_chromium_a_cancel_set_aside_and_undone_in_place() {
    let f = Fixture::new().await;
    let id = aside(&f).await;
    let (addr, server) = serve(router(&f)).await;
    tokio::task::spawn_blocking(move || {
        let plan = format!("http://{addr}/projects/id/{id}?view=plan");
        let mut browser = Chrome::open(&plan).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("document.querySelector('#n-stop-b button.x')?.checkVisibility()")
            .unwrap();
        browser.eval("(() => { const item = document.querySelector('#n-stop-b'); document.querySelector('#project-board').before(Object.assign(document.createElement('div'), {style:'height:700px'})); document.querySelector('#content').append(Object.assign(document.createElement('div'), {style:'height:1400px'})); window.scrollTo(0, item.getBoundingClientRect().top + scrollY - 400); window.dismissMarker = 'same document'; window.dismissNavigations = performance.getEntriesByType('navigation').length; window.dismissScroll = scrollY; })()").unwrap();
        browser
            .eval("document.querySelector('#n-stop-b button.x').click()")
            .unwrap();
        browser
            .wait("!document.querySelector('#n-stop-b') && document.activeElement?.id === 'undo-n-stop-b'")
            .unwrap();
        assert_eq!(browser.eval("[window.dismissMarker, performance.getEntriesByType('navigation').length === window.dismissNavigations, Math.abs(scrollY - window.dismissScroll) <= 3]").unwrap(), json!(["same document", true, true]));
        // the line says what was set aside and offers Undo, on one line
        let line = browser
            .eval("(() => { const b = document.querySelector('#undo-n-stop-b'), li = b.closest('li'); return [li.textContent.includes('Set aside: '), b.textContent, Math.round(li.getBoundingClientRect().height) <= 48]; })()")
            .unwrap();
        assert_eq!(line, json!([true, "Undo", true]));
        browser
            .eval("document.querySelector('#undo-n-stop-b').click()")
            .unwrap();
        browser
            .wait("document.activeElement?.id === 'n-stop-b' && !document.querySelector('#undo-n-stop-b')")
            .unwrap();
        assert_eq!(browser.eval("[window.dismissMarker, performance.getEntriesByType('navigation').length === window.dismissNavigations, Math.abs(scrollY - window.dismissScroll) <= 3]").unwrap(), json!(["same document", true, true]));
        browser.eval("window.realFetch = window.fetch; window.fetch = (...args) => /\\/(needs|actions)$/.test(String(args[0])) ? Promise.reject(new TypeError('Failed to fetch')) : realFetch(...args); document.querySelector('#n-stop-b button.x').click()").unwrap();
        browser.wait("document.querySelector('#n-stop-b .notice')?.textContent.includes('Sluice did not answer')").unwrap();
        assert_eq!(browser.eval("[window.dismissMarker, !document.querySelector('#n-stop-b button.x').disabled]").unwrap(), json!(["same document", true]));
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

/// The plan's Retry and the drawer's action forms post in place: a refusal reads under the
/// form that sent it, the page and a typed message kept.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_plan_and_drawer_action_forms_submit_in_place() {
    let f = Fixture::new().await;
    let id = aside(&f).await;
    let (addr, server) = serve(router(&f)).await;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}")).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser.wait("customElements.get('sluice-confirm') && document.querySelector('#n-stop-b form[action$=\"/actions\"] button')").unwrap();
        browser.eval("window.marker = true; window.posts = []; window.realFetch = fetch; window.fetch = (url, options) => { if (options?.method === 'POST' && /\\/(actions|needs)$/.test(String(url))) { posts.push({url:String(url), data:Object.fromEntries(options.body)}); return Promise.resolve(new Response('Fixture refusal', {status:409})); } return realFetch(url, options); }").unwrap();
        browser.eval("document.querySelector('#n-stop-b form[action$=\"/actions\"] button').click()").unwrap();
        browser.wait("posts.length === 1 && document.querySelector('#n-stop-b .notice')?.textContent.includes('Fixture refusal')").unwrap();
        assert_eq!(browser.eval("[posts[0].data.action, !!posts[0].data.revision, !!posts[0].data.seen]").unwrap(), json!(["retry", true, true]));
        browser.eval("location.hash = 'step:b'").unwrap();
        browser.wait("document.querySelector('#drawer .d-actions form > button[value=retry]')?.checkVisibility()").unwrap();
        browser.eval("document.querySelector('#drawer .d-actions form > button[value=retry]').click()").unwrap();
        browser.wait("posts.length === 2 && document.querySelector('#drawer .d-actions .notice')").unwrap();
        browser.eval("document.querySelector('#drawer .pl-fb').open = true; document.querySelector('#drawer textarea[name=message]').value = 'Drawer feedback'; document.querySelector('#drawer .pl-fb button').click()").unwrap();
        browser.wait("posts.length === 3 && !document.querySelector('#drawer .pl-fb button').disabled").unwrap();
        assert_eq!(browser.eval("[posts[2].data.action, posts[2].data.message, document.querySelector('#drawer textarea[name=message]').value]").unwrap(), json!(["retry", "Drawer feedback", "Drawer feedback"]));
        browser.eval("location.hash = 'step:c'").unwrap();
        browser.wait("document.querySelector('#drawer sluice-confirm summary')?.checkVisibility()").unwrap();
        browser.eval("document.querySelector('#drawer sluice-confirm summary').click()").unwrap();
        browser.wait("document.querySelector('#confirmation').open").unwrap();
        browser.eval("document.querySelector('#confirmation textarea').value = 'Stop this work'; document.querySelector('#confirmation button.danger').click()").unwrap();
        browser.wait("posts.length === 4 && document.querySelector('#confirmation .notice')").unwrap();
        assert_eq!(browser.eval("[window.marker, document.querySelector('#confirmation').open, posts[3].data.action, posts[3].data.message, document.querySelector('#confirmation textarea').value]").unwrap(), json!([true, true, "cancel", "Stop this work", "Stop this work"]));
        browser.eval("window.fetch = () => Promise.resolve(new Response(null, {status:204})); document.querySelector('#confirmation button.danger').click()").unwrap();
        browser.wait("!document.querySelector('#confirmation').open").unwrap();
        assert_eq!(browser.eval("[window.marker, window.browserErrors]").unwrap(), json!([true, []]));
    }).await.unwrap();
    server.abort();
}

/// Every Details panel on the plan, a step, a unit, home and the inbox opens wholly inside
/// the window, at a phone's width, a laptop's and a wide screen's (home draws none today; it
/// is checked so one added there is held too).
#[tokio::test(flavor = "multi_thread")]
async fn slow_every_details_panel_opens_inside_the_window_at_every_width() {
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

/// The ask an agent puts last in a long question to the owner: on every page built to answer
/// it, at a phone's width, the question is whole and its last line seen, never folded away.
#[tokio::test(flavor = "multi_thread")]
async fn slow_a_long_open_question_is_whole_with_its_last_line_seen_on_every_answering_page() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let mut body =
        String::from("The encoder for stored images changed its byte layout. Two options:\n\n");
    for n in 1..=12 {
        body.push_str(&format!(
            "- option detail {n}: what it costs and what it keeps\n"
        ));
    }
    body.push_str("\n```rust\npub fn encode(img: &Image) -> Vec<u8> {\n    todo!()\n}\n```\n\nWhich do you want before I land?");
    let body: &'static str = Box::leak(body.into_boxed_str());
    stored(
        &f.writer,
        id,
        Stored {
            thread: "step-l1-work",
            from: "l1-work",
            to: Some("owner"),
            body,
            title: Some("Re-record goldens or keep the old layout?"),
            question: true,
            ..Default::default()
        },
    )
    .await;
    let (addr, server) = serve(f.router()).await;
    tokio::task::spawn_blocking(move || {
        let base = format!("http://{addr}");
        let pages = [
            "/inbox".to_owned(),
            format!("/projects/id/{id}/steps/l1-work"),
            format!("/projects/id/{id}/units/l1"),
            format!("/projects/id/{id}/thread?thread=step-l1-work"),
        ];
        // the last line drawn, inside every box around it (no fold, no clip), no closed
        // disclosure over it
        const SEEN: &str = "(() => { const want = 'Which do you want before I land?'; const e = [...document.querySelectorAll('p')].find(p => p.textContent.trim() === want && p.checkVisibility()); if (!e) return 'missing'; const r = e.getBoundingClientRect(); for (let a = e.parentElement; a && a !== document.body; a = a.parentElement) { if (a.tagName === 'DETAILS' && !a.open) return 'in a closed details'; const cs = getComputedStyle(a); if (cs.overflowY !== 'visible') { const ar = a.getBoundingClientRect(); if (r.bottom > ar.bottom + 1) return 'clipped by ' + a.tagName + '.' + a.className; } } return 'seen'; })()";
        let mut browser = Chrome::open(&format!("{base}{}", pages[0])).unwrap();
        browser.viewport(390, "light").unwrap();
        for page in &pages {
            browser.navigate(&format!("{base}{page}")).unwrap();
            browser.wait("document.readyState === 'complete'").unwrap();
            browser.eval("document.fonts.ready").unwrap();
            assert_eq!(browser.eval(SEEN).unwrap(), json!("seen"), "{page}");
        }
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}
