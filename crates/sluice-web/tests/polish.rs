//! The first critique's fixes, each through the pages it changed: a cancel the owner dismisses
//! stops marking its project; a project's messages page is "Messages"; a message box to a step
//! not running says when it is read; "Waits on" names steps by their titles, linked; a pending
//! step that ran says when its last run ended, once; an empty output says so; Clear board and
//! Remove ask first, a cleared board's program kept in the box. In Chromium: a step opened from
//! another opens on its Overview, a long token never
//! pushes a phone's page sideways, a step's heading steps down on a phone, and a form of filters
//! applies as it changes.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
mod plan_html;
mod seed;
#[allow(dead_code)]
#[path = "../../../tests/support/messages.rs"]
mod stored_messages;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::json;
use sluice_model::{
    commands::StepDismiss,
    error::PublicError,
    ids::{AttemptId, ProjectId, RunId},
};
use sluice_store::RetrySafety;

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
                message: "pivot: audit by inspection (Sam)".into(),
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
async fn dismiss(
    f: &Fixture,
    project: ProjectId,
    step: &str,
    dismissed: bool,
) -> Result<(), PublicError> {
    let step = step.parse().unwrap();
    f.writer
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
}

#[tokio::test]
async fn a_cancel_the_owner_dismisses_stays_on_its_unit_but_no_longer_marks_its_project() {
    let f = Fixture::new().await;
    let id = aside(&f).await;
    let (_, home) = f.get("/").await;
    assert!(title(&home).contains("1 cancelled"), "{}", title(&home));
    let row = between(
        &home,
        "<li class=\"pm-row pm-stopped sr-cancelled\">",
        "</li>",
    );
    assert!(
        row.contains("<form class=\"sr-dismiss\" method=\"post\" action=\"/projects/id/"),
        "{row}"
    );
    assert!(
        row.contains("<input type=\"hidden\" name=\"next\" value=\"/\">"),
        "{row}"
    );
    let (_, page) = f.get(&format!("/projects/id/{id}")).await;
    assert!(title(&page).contains("1 cancelled"), "{}", title(&page));
    let (_, step) = f.get(&format!("/projects/id/{id}/steps/b")).await;
    assert!(
        step.contains(
            "<input type=\"hidden\" name=\"action\" value=\"dismiss\"><button class=\"quiet-act\" title=\"It stays on its unit, but no longer marks the unit or its project\">Dismiss</button>"
        ),
        "{step}"
    );
    // only a cancel can be dismissed
    assert!(dismiss(&f, id, "c", true).await.is_err());

    dismiss(&f, id, "b", true).await.unwrap();
    let (_, home) = f.get("/").await;
    assert!(!title(&home).contains("cancelled"), "{}", title(&home));
    assert!(!home.contains("sr-dismiss"), "the index lists it no more");
    let (_, page) = f.get(&format!("/projects/id/{id}")).await;
    assert!(!title(&page).contains("cancelled"), "{}", title(&page));
    // no longer stopped: its unit is done, a line of the Done index that still reads cancelled
    assert_eq!(plan_html::place(&page, "u"), "done", "{page}");
    let done = plan_html::band(&page, "plan-done");
    let line = between(done, "<li data-said=\"a succeeded|b cancelled\">", "</li>");
    assert!(
        line.contains("g-cancelled") && line.contains(">b cancelled and dismissed</span>"),
        "{line}"
    );
    assert!(!plan_html::summary(&page).contains("cancelled"), "{page}");
    // Stopped holds no card now, only the line that offers its Undo, for ten minutes
    let stopped = plan_html::band(&page, "plan-stopped");
    assert!(
        !stopped.contains("pl-item pl-stop")
            && stopped.contains("<div class=\"pl-dismissed\" role=\"status\">"),
        "{stopped}"
    );
    let (_, step) = f.get(&format!("/projects/id/{id}/steps/b")).await;
    assert!(step.contains("cancelled"), "the step still reads cancelled");
    assert!(
        step.contains("value=\"undismiss\"><p class=\"meta\">Dismissed:"),
        "{step}"
    );

    // undone, it stands out again
    dismiss(&f, id, "b", false).await.unwrap();
    let (_, home) = f.get("/").await;
    assert!(title(&home).contains("1 cancelled"), "{}", title(&home));
}

#[tokio::test]
async fn a_projects_messages_page_is_titled_messages_and_the_tray_stays_inbox() {
    let f = Fixture::new().await;
    let (_, project) = f.get(&format!("/projects/id/{}/inbox", f.id)).await;
    // its name heads it; its way back is the tab row's Plan
    assert!(
        project.contains("<div class=\"ph-name\"><h1>Messages</h1></div>"),
        "{project}"
    );
    assert!(
        project.contains(">For you</a>"),
        "its seg names the view for you"
    );
    let (_, tray) = f.get("/inbox").await;
    assert!(tray.contains("<h1>Inbox</h1>"), "{tray}");
    let (_, history) = f.get(&format!("/projects/id/{}/history", f.id)).await;
    // History wears the messages' head as For you and Questions do: its name, in the frame's
    // head before the page; the threads' count is its section's
    let band = history
        .split("<div id=\"messages-band\" class=\"messages-band\">")
        .nth(1)
        .and_then(|b| b.split("<main").next())
        .unwrap_or_else(|| panic!("no band: {history}"));
    assert!(band.contains("<h1>History</h1>"), "{band}");
    assert_eq!(history.matches("<h1>History</h1>").count(), 1, "{history}");
    assert!(
        history.contains("/history\" aria-current=\"page\">History</a>"),
        "its switch names History, the third view, in the tab row: {history}"
    );
}

#[tokio::test]
async fn a_message_box_to_a_step_not_running_says_when_it_is_read() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    for thread in ["step-l2-work", "step-l1-work"] {
        stored_messages::stored(
            &f.writer,
            id,
            stored_messages::Stored {
                thread,
                from: "orchestrator",
                to: Some(&thread[5..]),
                body: "Rebase first.",
                ..Default::default()
            },
        )
        .await;
    }
    let (_, failed) = f
        .get(&format!("/projects/id/{id}/steps/l2-work?tab=thread"))
        .await;
    let composer = between(&failed, "<form class=\"composer thread-reply\"", "</form>");
    assert!(
        composer.contains("<p class=\"meta composer-note\" id=\"composer-note-l2-work\">It is not running: it reads this only if it runs again. To tell it something now, Retry with feedback.</p>"),
        "{composer}"
    );
    assert!(
        composer.contains("aria-describedby=\"composer-note-l2-work\""),
        "{composer}"
    );
    let (_, thread) = f
        .get(&format!("/projects/id/{id}/thread?thread=step-l2-work"))
        .await;
    assert!(
        thread.contains("composer-note"),
        "the thread's page says it too"
    );
    // the head says the step's title; its crumb leads back to the step, its id in Details
    assert!(
        thread.contains("<div class=\"ph-name\"><h1><span class=\"d-stage\">")
            && thread.contains("/steps/l2-work\">FIG-2: Stop the parser leak</a></nav>")
            && thread.contains("<dt>Step</dt>"),
        "{thread}"
    );
    let (_, running) = f
        .get(&format!("/projects/id/{id}/steps/l1-work?tab=thread"))
        .await;
    assert!(
        !running.contains("composer-note"),
        "a running step reads it now"
    );
}

#[tokio::test]
async fn waits_on_names_each_step_it_takes_from_by_its_title_linked() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "takes",
            json!({"steps":{
                "probe":{"run":"custom.open","doc":"Probes the parser under load","outputs":{"x":"string"}},
                "reader":{"run":"custom.open","in":{"x":{"source":"probe/x"}}}}}),
            &[("probe", "running")],
        )
        .await;
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/reader")).await;
    let waits = between(&page, "<p class=\"gate wait-step\">", "</p>");
    assert!(waits.contains(&format!("<a class=\"sref-a\" href=\"/projects/id/{id}/steps/probe\" title=\"Probes the parser under load\">")), "{waits}");
    assert!(!waits.contains("sref-id"), "the title alone: {waits}");
    assert!(
        waits.ends_with(" <span class=\"meta\">(running)</span>"),
        "{waits}"
    );
    assert!(
        !page.contains("step probe is running"),
        "said once, as a link"
    );
}

/// One finished run of `step`, ended `at`.
async fn ran(f: &Fixture, project: ProjectId, step: &'static str, at: &'static str) {
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let (attempt, run) = (AttemptId::new(), RunId::new());
            tx.sql().execute(
                "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?1,?2,?3,1,1,'terminal','{}','hash',?4)",
                (attempt.to_string(), project.to_string(), step, at),
            )?;
            tx.sql().execute(
                "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at,finished_at,result) VALUES (?1,?2,?3,?4,1,1,?5,?5,?5,'{\"status\":\"failed\"}')",
                (run.to_string(), project.to_string(), attempt.to_string(), step, at),
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_pending_step_that_ran_says_when_its_last_run_ended_and_an_empty_output_says_empty() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "again",
            json!({"steps":{
                "redo":{"run":"custom.open"},
                "said":{"run":"custom.open","outputs":{"note":"string"}}}}),
            &[("said", "succeeded")],
        )
        .await;
    ran(&f, id, "redo", "2026-10-01T00:00:00Z").await;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET outputs='{\"note\":\"\"}' WHERE project_id=?1 AND step_id='said'",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/redo")).await;
    let when = between(&page, "<span class=\"meta d-when\">", "</p>");
    let when = &when[..when.rfind("</span>").unwrap()];
    assert!(
        when.starts_with("<span class=\"meta d-when\">Last run ended <time"),
        "{when}"
    );
    assert!(when.ends_with(" (took &lt;1s)."), "{when}");
    assert!(!page.contains(">Ended "), "{page}");
    let (_, said) = f
        .get(&format!("/projects/id/{id}/steps/said?tab=outputs"))
        .await;
    assert!(
        said.contains("<dd class=\"f-v\"><span class=\"v quiet\">empty</span></dd>"),
        "{said}"
    );
}

#[tokio::test]
async fn clear_board_and_remove_ask_first_and_a_cleared_board_keeps_its_program_in_the_box() {
    let f = Fixture::new().await;
    let path = format!("/projects/id/{}/settings", f.id);
    let (_, page) = f.get(&path).await;
    let clear = between(&page, "<div class=\"board-clear\">", "</sluice-confirm>");
    assert!(
        clear.contains("<sluice-confirm heading=\"Clear the board?\""),
        "{clear}"
    );
    assert!(
        clear.contains("<summary id=\"board-clear\">Clear board</summary>"),
        "{clear}"
    );
    assert!(
        clear.contains("<input type=\"hidden\" name=\"op\" value=\"clear\">"),
        "{clear}"
    );
    assert!(
        !page.contains("<button type=\"submit\" name=\"op\" value=\"clear\""),
        "no one-click Clear"
    );
    // cleared (as the confirmation's form sends it, without script), the box keeps the program
    let response = post_html(
        f.router(),
        &format!("{path}/board"),
        &[
            ("op", "clear"),
            ("expected_rev", "1"),
            ("program", board_fixture::BOARD),
        ],
    )
    .await;
    assert!(
        response.contains("Its program is still in the box: Save board to restore it."),
        "{response}"
    );
    let program = between(&response, "<textarea id=\"board-program\"", "</textarea>");
    assert!(program.contains("root = Stack("), "{program}");
    assert!(
        response.contains("<summary id=\"board-clear\" aria-disabled=\"true\">"),
        "nothing left to clear"
    );
}

/// A form posted as a page without script posts it, the page that comes back.
async fn post_html(router: axum::Router, path: &str, form: &[(&str, &str)]) -> String {
    use tower::ServiceExt;
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form)
        .finish();
    let response = router
        .oneshot(
            axum::http::Request::post(path)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(axum::body::Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(body.to_vec()).unwrap()
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
/// Two frames after a short pause: a resize or a patch has been laid out and fitted.
fn settle(browser: &mut Chrome) {
    browser
        .wait("new Promise(r => setTimeout(() => requestAnimationFrame(() => requestAnimationFrame(() => r(true))), 300))")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_a_step_opened_from_another_opens_on_its_overview() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let (addr, server) = serve(&f).await;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}#step:l1-work")).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("document.querySelector('#drawer:not([hidden]) #step-detail[data-step=\"l1-work\"] [role=tab][data-tab=\"inputs\"]')")
            .unwrap();
        browser
            .eval("document.querySelector('#drawer [role=tab][data-tab=\"inputs\"]').click()")
            .unwrap();
        browser.wait("new URLSearchParams(location.search).get('tab') === 'inputs'").unwrap();
        // another step from this one: its Overview, the address's tab gone
        browser.eval("location.hash = '#step:l2-work'").unwrap();
        browser
            .wait("document.querySelector('#drawer #step-detail[data-step=\"l2-work\"] [role=tab][aria-selected=true]')")
            .unwrap();
        let g = browser
            .eval("({tab: document.querySelector('#drawer [role=tab][aria-selected=true]').dataset.tab, query: location.search, errors: window.browserErrors})")
            .unwrap();
        assert_eq!(g["tab"], "overview", "{g}");
        assert_eq!(g["query"], "", "{g}");
        assert_eq!(g["errors"], json!([]), "{g}");
        // opened afresh with a tab in its address, it opens on that tab
        browser
            .navigate(&format!("http://{addr}/projects/id/{id}?tab=inputs#step:l2-work"))
            .unwrap();
        browser
            .wait("document.querySelector('#drawer #step-detail[data-step=\"l2-work\"] [role=tab][aria-selected=true]')?.dataset.tab === 'inputs'")
            .unwrap();
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_a_long_token_never_pushes_a_phones_page_sideways() {
    let f = Fixture::new().await;
    let token = "SessionObservationEventPayload::LanguageExecution(LanguageExecutionObservationWithAVeryLongName)";
    let body = format!("Should {token} carry the settled time?");
    for (to, question) in [
        (Some("owner"), true),
        (Some("beta-build"), true),
        (Some("owner"), false),
    ] {
        stored_messages::stored(
            &f.writer,
            f.id,
            stored_messages::Stored {
                thread: "step-beta-build",
                from: "beta-build",
                to,
                body: &body,
                question,
                ..Default::default()
            },
        )
        .await;
    }
    let (addr, server) = serve(&f).await;
    let id = f.id;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/questions")).unwrap();
        for path in [
            "/questions".to_owned(),
            "/inbox".to_owned(),
            format!("/projects/id/{id}/questions"),
            format!("/projects/id/{id}/thread?thread=step-beta-build"),
        ] {
            browser.navigate(&format!("http://{addr}{path}")).unwrap();
            browser.wait("document.readyState === 'complete'").unwrap();
            for theme in ["light", "dark"] {
                browser.viewport(390, theme).unwrap();
                settle(&mut browser);
                let g = browser
                    .eval("({scroll: document.documentElement.scrollWidth - document.documentElement.clientWidth, has: document.body.textContent.includes('LanguageExecutionObservationWithAVeryLongName'), errors: window.browserErrors})")
                    .unwrap();
                assert_eq!(g["has"], true, "{path}: {g}");
                assert_eq!(g["scroll"], 0, "{path} {theme}: sideways {g}");
                assert_eq!(g["errors"], json!([]), "{path}: {g}");
            }
        }
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_a_steps_heading_steps_down_on_a_phone_and_a_units_glyph_sits_by_its_first_line() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let (addr, server) = serve(&f).await;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}/steps/l1-work")).unwrap();
        browser.wait("document.readyState === 'complete' && document.querySelector('h1')").unwrap();
        const SIZE: &str = "(() => { const h = document.querySelector('h1'); const s = getComputedStyle(h); return {size: s.fontSize, line: s.lineHeight}; })()";
        // the step's name, its stage and its title, at a reading size: 28px, a phone's 23px, so
        // its state, its stages and its tabs stay on the first screen
        browser.viewport(390, "light").unwrap();
        assert_eq!(browser.eval(SIZE).unwrap(), json!({"size": "23px", "line": "28px"}));
        browser.viewport(1440, "light").unwrap();
        assert_eq!(browser.eval(SIZE).unwrap(), json!({"size": "34px", "line": "38px"}));
        browser.navigate(&format!("http://{addr}/projects/id/{id}/units/l1")).unwrap();
        browser.wait("document.querySelector('h1.unit-h')").unwrap();
        browser.viewport(390, "light").unwrap();
        let g = browser
            .eval("(() => { const h = document.querySelector('h1.unit-h'), g = h.querySelector('.g'); return {top: g.getBoundingClientRect().top - h.getBoundingClientRect().top, size: getComputedStyle(h).fontSize}; })()")
            .unwrap();
        assert!(g["top"].as_f64().unwrap() < 8.0, "the glyph by the first line: {g}");
        assert_eq!(g["size"], "28px", "{g}");
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_a_form_of_filters_applies_as_it_changes_with_no_apply_button() {
    let f = Fixture::new().await;
    let (addr, server) = serve(&f).await;
    let id = f.id;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}/log")).unwrap();
        browser.wait("customElements.get('sluice-search') && document.querySelector('form.filters')").unwrap();
        let shown = browser
            .eval("document.querySelector('form.filters .apply').checkVisibility()")
            .unwrap();
        assert_eq!(shown, false, "Apply hides while the form applies itself");
        browser
            // the click navigates: it runs after this evaluation has returned
            .eval("(() => { const box = document.querySelector('form.filters input[name=kind]'); box.closest('details').open = true; setTimeout(() => box.click(), 0); return box.value; })()")
            .unwrap();
        browser.wait("new URLSearchParams(location.search).has('kind')").unwrap();
        // a text field applies when it is left with new words, no Enter needed
        browser
            .wait("customElements.get('sluice-search') && document.querySelector('form.filters input[name=step]')")
            .unwrap();
        browser
            .eval("document.querySelector('form.filters input[name=step]').focus()")
            .unwrap();
        browser
            .send("Input.insertText", json!({"text": "beta-build"}))
            .unwrap();
        browser
            // leaving it navigates: it runs after this evaluation has returned
            .eval("setTimeout(() => document.activeElement.blur(), 0)")
            .unwrap();
        browser
            .wait("new URLSearchParams(location.search).get('step') === 'beta-build' && new URLSearchParams(location.search).has('kind')")
            .unwrap();
        browser.navigate(&format!("http://{addr}/fns")).unwrap();
        browser.wait("customElements.get('sluice-search') && document.querySelector('form.picker')").unwrap();
        assert_eq!(
            browser.eval("document.querySelector('form.picker .apply').checkVisibility()").unwrap(),
            false
        );
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_clear_board_asks_first_and_keeps_the_program_to_restore() {
    let f = Fixture::new().await;
    let (addr, server) = serve(&f).await;
    let id = f.id;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}/settings")).unwrap();
        browser
            .wait("customElements.get('sluice-confirm') && document.querySelector('#board-clear')")
            .unwrap();
        browser.eval("document.querySelector('#board-clear').click()").unwrap();
        browser.wait("document.querySelector('#confirmation[open]')").unwrap();
        let heading = browser
            .eval("document.querySelector('#confirmation-title').textContent")
            .unwrap();
        assert_eq!(heading, "Clear the board?");
        // Keep: nothing cleared
        browser.eval("document.querySelector('#confirmation [data-keep]').click()").unwrap();
        // closed, its form back in its details (the dialog's close event puts it there)
        browser
            .wait("!document.querySelector('#confirmation[open]') && document.querySelector('#board-clear').parentElement.querySelector(':scope > form')")
            .unwrap();
        browser.eval("document.querySelector('#board-clear').click()").unwrap();
        browser.wait("document.querySelector('#confirmation[open]')").unwrap();
        browser
            .eval("document.querySelector('#confirmation button.danger').click()")
            .unwrap();
        browser
            .wait("document.querySelector('#board-feedback').textContent.includes('Save board to restore it')")
            .unwrap();
        let g = browser
            .eval("({open: Boolean(document.querySelector('#confirmation[open]')), kept: document.querySelector('#board-program').value.startsWith('root = Stack('), off: document.querySelector('#board-clear').getAttribute('aria-disabled'), errors: window.browserErrors})")
            .unwrap();
        assert_eq!(g, json!({"open": false, "kept": true, "off": "true", "errors": []}));
    })
    .await
    .unwrap();
    server.abort();
}
