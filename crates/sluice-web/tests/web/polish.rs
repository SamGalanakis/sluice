//! The first critique's fixes, each through the pages it changed: a cancel the owner dismisses
//! stops marking its project; a project's messages page is "Messages"; a message box to a step
//! not running says when it is read; "Waits on" names steps by their titles, linked; a pending
//! step that ran says when its last run ended, once; an empty output says so; Clear board and
//! Remove act at once and can be undone. In Chromium: a step opened from
//! another opens on its Overview, a long token never
//! pushes a phone's page sideways, a step's heading steps down on a phone, and a form of filters
//! applies as it changes.
use crate::board_fixture;
use crate::chrome;
use crate::plan_html;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::json;
use sluice_model::{commands::StepDismiss, error::PublicError, ids::ProjectId};
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
    let (_, page) = f.get(&format!("/projects/id/{id}")).await;
    assert!(title(&page).contains(">⚠1 · aside"), "{}", title(&page));
    let (_, step) = f.get(&format!("/projects/id/{id}/steps/b")).await;
    assert!(
        step.contains(
            "<input type=\"hidden\" name=\"action\" value=\"dismiss\"><button class=\"quiet\">Dismiss</button>"
        ),
        "{step}"
    );
    // only a cancel can be dismissed
    assert!(dismiss(&f, id, "c", true).await.is_err());

    dismiss(&f, id, "b", true).await.unwrap();
    let (_, home) = f.get("/").await;
    assert!(!title(&home).contains("cancelled"), "{}", title(&home));
    let (_, page) = f.get(&format!("/projects/id/{id}")).await;
    assert!(!title(&page).contains('⚠'), "{}", title(&page));
    // no longer stopped: its unit is done, a line of the Done index that still reads cancelled
    let (_, index) = f.get(&format!("/projects/id/{id}?show=done")).await;
    assert!(index.contains(&format!("href=\"/projects/id/{id}/history\"")));
    let (_, unit) = f.get(&format!("/projects/id/{id}/units/u")).await;
    assert!(unit.contains("cancelled"), "{unit}");
    assert!(!plan_html::summary(&page).contains("cancelled"), "{page}");
    // settled, not set aside: Needs you holds no item and no Set aside line (its step's page
    // undoes it)
    let well = plan_html::band(&page, "plan-needs");
    assert!(
        plan_html::needs(&page).is_empty() && !well.contains("nd-gone"),
        "{well}"
    );
    let (_, step) = f.get(&format!("/projects/id/{id}/steps/b")).await;
    assert!(step.contains("cancelled"), "the step still reads cancelled");
    assert!(
        step.contains("value=\"undismiss\"><p class=\"meta\">Dismissed. <button")
            && !step.contains("no longer marks the unit"),
        "{step}"
    );

    // undone, it stands out again
    dismiss(&f, id, "b", false).await.unwrap();
    let (_, home) = f.get("/").await;
    assert!(title(&home).contains("1 cancelled"), "{}", title(&home));
}

#[tokio::test]
async fn clear_board_is_immediate_and_undo_restores_its_program() {
    let f = Fixture::new().await;
    let path = format!("/projects/id/{}/settings", f.id);
    let (_, page) = f.get(&path).await;
    assert!(page.contains("<button id=\"board-clear\">Clear board</button>"));
    assert!(!page.contains("sluice-confirm"));
    let response = post_html(
        f.router(),
        &format!("{path}/board"),
        &[("op", "clear"), ("expected_rev", "1")],
    )
    .await;
    assert!(response.contains("Board cleared"), "{response}");
    assert!(response.contains("<button>Undo</button>"), "{response}");
    assert!(
        response.contains("id=\"board-clear\" disabled"),
        "{response}"
    );
    let restored = post_html(f.router(), &format!("{path}/undo"), &[("key", "board")]).await;
    assert!(restored.contains("root = Stack("), "{restored}");
    assert!(!restored.contains("Board cleared"), "{restored}");
}

/// Board clear's direct form posted, its answer.
async fn post_html(router: axum::Router, path: &str, form: &[(&str, &str)]) -> String {
    use tower::ServiceExt;
    let response = router
        .oneshot(super::forms::post(path, "board-clear-form", form))
        .await
        .unwrap();
    super::forms::text(response).await
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

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_a_form_of_filters_applies_as_it_changes_with_no_apply_button() {
    let f = Fixture::new().await;
    let (addr, server) = serve(&f).await;
    let id = f.id;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}/log")).unwrap();
        browser.wait("document.readyState === 'complete' && document.querySelector('form.filters')").unwrap();
        assert_eq!(
            browser.eval("!!document.querySelector('form.filters button')").unwrap(),
            false,
            "the form applies itself: no Apply"
        );
        browser
            // the click navigates: it runs after this evaluation has returned
            .eval("(() => { const box = document.querySelector('form.filters input[name=kind]'); box.closest('details').open = true; setTimeout(() => box.click(), 0); return box.value; })()")
            .unwrap();
        browser.wait("new URLSearchParams(location.search).has('kind')").unwrap();
        // a text field applies when it is left with new words, no Enter needed
        browser
            .wait("document.readyState === 'complete' && document.querySelector('form.filters input[name=step]')")
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
        browser.wait("document.readyState === 'complete' && document.querySelector('form.picker')").unwrap();
        assert_eq!(browser.eval("!!document.querySelector('form.picker button')").unwrap(), false);
        // choosing applies it
        browser
            .eval("(s => { s.value = s.options[1]?.value ?? ''; setTimeout(() => s.dispatchEvent(new Event('change', {bubbles: true})), 0); })(document.querySelector('form.picker select'))")
            .unwrap();
        browser.wait("new URLSearchParams(location.search).has('project')").unwrap();
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_clear_board_acts_and_undo_restores() {
    let f = Fixture::new().await;
    let (addr, server) = serve(&f).await;
    let id = f.id;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}/settings")).unwrap();
        browser.wait("document.querySelector('#board-clear') && document.readyState === 'complete'").unwrap();
        browser.eval("document.querySelector('#board-clear').click()").unwrap();
        browser.wait("document.querySelector('#board-edit .undo-line button') && document.querySelector('#board-clear').disabled").unwrap();
        // The old document can still be complete before navigation starts.
        browser.eval("window.beforeUndoReload = true; location.reload()").unwrap();
        browser.wait("!window.beforeUndoReload && document.querySelector('#board-edit .undo-line button') && document.readyState === 'complete'").unwrap();
        browser.eval("document.querySelector('#board-edit .undo-line button').click()").unwrap();
        browser.wait("document.querySelector('#board-program').value.startsWith('root = Stack(') && !document.querySelector('#board-clear').disabled").unwrap();
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
    }).await.unwrap();
    server.abort();
}
