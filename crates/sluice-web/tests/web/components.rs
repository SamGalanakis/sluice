//! The dashboard's Rocket components (DESIGN.md, Components): Rust draws each host and all in
//! it, and the component only behaves. Over HTTP every page draws its components' hosts and no
//! template writes one by hand; in a real Chromium each component is defined as the server
//! declares it, behaves, keeps its state through a stream patch of its region, takes the
//! keyboard and logs nothing.
use crate::board_fixture;
use crate::chrome;
use crate::stored_messages;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::{Value, json};
use sluice_model::commands::CommandRequest;
use sluice_store::RetrySafety;
use stored_messages::{Stored, stored};

/// A long output on alpha-build, so its Outputs tab folds it.
async fn long_output(f: &Fixture) {
    let id = f.id;
    let long = "The rebase onto main conflicts in three places. ".repeat(40);
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET outputs=?2 WHERE project_id=?1 AND step_id='alpha-build'",
                (id.to_string(), json!({ "summary": long }).to_string()),
            )?;
            tx.changed(Some(id), "steps");
            Ok(())
        })
        .await
        .unwrap();
}
async fn note(f: &Fixture, to: &'static str, body: &'static str) {
    stored(
        &f.writer,
        f.id,
        Stored {
            thread: "step-alpha-build",
            from: "orchestrator",
            to: Some(to),
            body,
            ..Default::default()
        },
    )
    .await;
}

// ---- Chromium ----------------------------------------------------------------------------

fn serve(f: &Fixture) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let router = f.router();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    (
        addr,
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    )
}
const FRAMES: &str = "new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))";
/// Press one key (with `modifiers`: 2 Ctrl, 8 Shift) on what has the focus.
fn press(browser: &mut Chrome, key: &str, code: u32, modifiers: u32) {
    for kind in ["keyDown", "keyUp"] {
        browser
            .send(
                "Input.dispatchKeyEvent",
                json!({"type": kind, "key": key, "code": key, "windowsVirtualKeyCode": code, "modifiers": modifiers}),
            )
            .unwrap();
    }
}
/// What the page logged as an error or a warning, and what it threw.
fn logged(browser: &mut Chrome) -> Vec<String> {
    browser.eval("1").unwrap(); // read what is pending
    let mut out: Vec<String> = browser
        .events
        .iter()
        .filter_map(|e| match e["method"].as_str() {
            Some("Runtime.consoleAPICalled")
                if matches!(
                    e["params"]["type"].as_str(),
                    Some("error" | "warning" | "assert")
                ) =>
            {
                Some(e["params"]["args"].to_string())
            }
            Some("Runtime.exceptionThrown") => Some(e["params"]["exceptionDetails"].to_string()),
            _ => None,
        })
        .collect();
    if let Ok(Value::Array(errors)) = browser.eval("window.browserErrors ?? []") {
        out.extend(errors.iter().map(Value::to_string));
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_the_step_page_components_keep_their_state_through_a_patch_and_take_the_keyboard()
 {
    let f = std::sync::Arc::new(Fixture::new().await);
    long_output(&f).await;
    note(&f, "alpha-build", "Build the crates.").await;
    let (addr, server) = serve(&f);
    let page = format!("http://{addr}/projects/id/{}/steps/alpha-build", f.id);
    let rt = tokio::runtime::Handle::current();
    let checks = f.clone();
    tokio::task::spawn_blocking(move || {
        let f = checks;
        let mut browser = Chrome::open(&format!("{page}?tab=outputs")).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("document.readyState === 'complete' && customElements.get('sluice-fold') && document.querySelector('#tp-outputs sluice-fold')")
            .unwrap();
        browser.eval(FRAMES).unwrap();
        // the long summary is cut: its Show all is there; open it
        let fold = "document.querySelector('#tp-outputs sluice-fold')";
        assert_eq!(
            browser.eval(&format!("[{fold}.hasAttribute('fits'), {fold}.querySelector('.fold-toggle').checkVisibility()]")).unwrap(),
            json!([false, true])
        );
        browser.eval(&format!("{fold}.querySelector('.fold-toggle > summary').click()")).unwrap();
        // the Types switch: every one on the page follows, and <html> shows types
        browser.eval("[...document.querySelectorAll('.types-toggle')].find(b => b.checkVisibility()).click()").unwrap();
        assert_eq!(
            browser.eval("[document.documentElement.classList.contains('show-types'), [...document.querySelectorAll('.types-toggle')].map(b => b.getAttribute('aria-pressed'))]").unwrap(),
            json!([true, ["true"]])
        );
        // the message box keeps what is typed, the Types switch its state and the fold its
        // opening through a patch of their panels
        browser.eval("document.querySelector('#tt-messages').click()").unwrap();
        browser.eval("document.querySelector('#tp-messages textarea').value = 'Half a thought'").unwrap();
        rt.block_on(note(&f, "alpha-build", "One more thing: tag the release."));
        rt.block_on(long_output(&f));
        browser.wait("document.querySelector('#tt-messages .n')?.textContent === '2'").unwrap();
        assert_eq!(
            browser.eval(&format!("[document.querySelector('#tp-messages textarea').value, {fold}.querySelector('.fold-toggle').open, document.querySelector('#tp-outputs .types-toggle').getAttribute('aria-pressed'), document.querySelector('sluice-tabs').current]")).unwrap(),
            json!(["Half a thought", true, "true", "messages"])
        );
        // Ctrl+Enter sends the box's text; its status says so
        browser.eval("document.querySelector('#tp-messages textarea').focus()").unwrap();
        press(&mut browser, "Enter", 13, 2);
        browser.wait("document.querySelector('#tp-messages .form-said').textContent === 'Sent.'").unwrap();
        let sent: Vec<_> = f
            .commands
            .0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                CommandRequest::Say(say) => Some((say.to.clone(), say.body.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(sent, [("alpha-build".to_owned(), "Half a thought".to_owned())]);
        // the Types switch back off, kept for the next page
        browser.eval("document.querySelector('#tt-outputs').click()").unwrap();
        browser.eval("document.querySelector('#tp-outputs .types-toggle').click()").unwrap();
        assert_eq!(browser.eval("document.documentElement.classList.contains('show-types')").unwrap(), false);
        // Show less folds it, its head brought back into view
        browser.eval(&format!("{fold}.querySelector('.fold-toggle > summary').click()")).unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(browser.eval(&format!("{fold}.querySelector('.fold-toggle').open")).unwrap(), false);
        // a short text that fits its first lines offers no Show all
        browser.eval(&format!("{fold}.querySelector('.clip').replaceChildren('Short.')")).unwrap();
        browser.wait(&format!("{fold}.hasAttribute('fits') && !{fold}.querySelector('.fold-toggle').checkVisibility()")).unwrap();
        // the display menu: ArrowDown goes into it, Escape closes it back on its summary
        browser.eval("document.querySelector('details.settings > summary').focus()").unwrap();
        press(&mut browser, "ArrowDown", 40, 0);
        browser.wait("document.querySelector('details.settings').open && document.activeElement.closest('details.settings .menu')").unwrap();
        press(&mut browser, "Escape", 27, 0);
        assert_eq!(
            browser.eval("[document.querySelector('details.settings').open, document.activeElement.matches('details.settings > summary')]").unwrap(),
            json!([false, true])
        );
        assert_eq!(logged(&mut browser), Vec::<String>::new());
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_the_project_page_components_keep_their_state_through_a_patch() {
    let f = std::sync::Arc::new(Fixture::new().await);
    let (addr, server) = serve(&f);
    let page = format!("http://{addr}/projects/id/{}", f.id);
    let rt = tokio::runtime::Handle::current();
    let checks = f.clone();
    tokio::task::spawn_blocking(move || {
        let f = checks;
        let mut browser = Chrome::open(&page).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&page).unwrap();
        let ready = "document.readyState === 'complete' && customElements.get('sluice-splitter') && document.querySelector('#project-board').dataset.view === 'both'";
        browser.wait(ready).unwrap();
        browser.eval(FRAMES).unwrap();
        // the splitter: arrows widen and narrow the board, Home and End to its bounds
        let width = "Math.round(document.querySelector('#board-pane').getBoundingClientRect().width)";
        let before = browser.eval(width).unwrap().as_i64().unwrap();
        browser.eval("document.querySelector('sluice-splitter').focus()").unwrap();
        press(&mut browser, "ArrowLeft", 37, 8);
        browser.eval(FRAMES).unwrap();
        let wider = browser.eval(width).unwrap().as_i64().unwrap();
        assert_eq!(wider, before + 64);
        assert_eq!(
            browser.eval("document.querySelector('sluice-splitter').getAttribute('aria-valuenow')").unwrap(),
            json!(wider.to_string())
        );
        press(&mut browser, "Home", 36, 0);
        browser.eval(FRAMES).unwrap();
        assert_eq!(browser.eval(width).unwrap(), json!(320));
        press(&mut browser, "ArrowLeft", 37, 0);
        browser.eval(FRAMES).unwrap();
        // the head's Details open: a patch keeps it and the board's width
        browser.eval("document.querySelector('#plan-band details.dm > summary').click()").unwrap();
        let version = browser.eval("document.documentElement.dataset.signals").unwrap();
        rt.block_on(note(&f, "alpha-build", "The board moves."));
        rt.block_on(async {
            let id = f.id;
            f.writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    tx.sql().execute(
                        "UPDATE steps SET status='running' WHERE project_id=?1 AND step_id='alpha-review'",
                        [id.to_string()],
                    )?;
                    tx.changed(Some(id), "steps");
                    Ok(())
                })
                .await
                .unwrap()
        });
        browser.wait("document.querySelector('[data-said*=\"alpha-review running\"]')").unwrap();
        assert_ne!(browser.eval("document.documentElement.dataset.signals").unwrap(), json!(null), "{version}");
        assert_eq!(
            browser.eval(&format!("[{width}, document.querySelector('#plan-band details.dm').open]")).unwrap(),
            json!([336, true])
        );
        // opening another menu closes it: one menu open at a time
        browser.eval("document.querySelector('details.switcher > summary').click()").unwrap();
        assert_eq!(
            browser.eval("[document.querySelector('details.switcher').open, document.querySelector('#plan-band details.dm').open]").unwrap(),
            json!([true, false])
        );
        // the menu: ArrowDown moves into it, Escape closes it back on its summary
        browser.eval("document.querySelector('details.switcher > summary').focus()").unwrap();
        press(&mut browser, "ArrowDown", 40, 0);
        browser.wait("document.activeElement.closest('details.switcher .menu')").unwrap();
        press(&mut browser, "Escape", 27, 0);
        assert_eq!(
            browser.eval("[document.querySelector('details.switcher').open, document.activeElement.matches('details.switcher > summary')]").unwrap(),
            json!([false, true])
        );
        // the board as wide after a reload
        browser.navigate(&page).unwrap();
        browser.wait(ready).unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(browser.eval(width).unwrap(), json!(336));
        // the find: the stream reopens on the page's `q` as one types and draws the plan as
        // found, the banner staying hidden (the old stream ends on purpose); Escape clears it
        browser.eval("document.querySelector('form.board-tools input[name=q]').focus()").unwrap();
        browser.send("Input.insertText", json!({"text": "beta"})).unwrap();
        browser.wait("!document.querySelector('#plan-pane [data-unit=alpha]')").unwrap();
        assert_eq!(browser.eval("document.querySelector('#stream-state').hidden").unwrap(), true);
        press(&mut browser, "Escape", 27, 0);
        browser.wait("document.querySelector('#plan-pane [data-unit=alpha]')").unwrap();
        assert_eq!(browser.eval("[location.search, document.querySelector('#stream-state').hidden]").unwrap(), json!(["", true]));
        // the drawer: a step opens beside the board, on the tab chosen in it; closing leaves
        // the address as it was
        browser.navigate(&format!("{page}#step:alpha-build")).unwrap();
        browser.wait("document.querySelector('#step-detail sluice-tabs #tt-outputs')").unwrap();
        browser.eval("document.querySelector('#step-detail #tt-outputs').click()").unwrap();
        browser.wait("document.querySelector('#step-detail sluice-tabs')?.current === 'outputs'").unwrap();
        assert_eq!(
            browser.eval("[...document.querySelectorAll('#step-detail .tp')].filter(p => p.checkVisibility()).map(p => p.dataset.tab)").unwrap(),
            json!(["outputs"])
        );
        press(&mut browser, "Escape", 27, 0);
        browser.wait("document.querySelector('#drawer').hidden").unwrap();
        assert_eq!(browser.eval("location.search + location.hash").unwrap(), json!(""));
        assert_eq!(logged(&mut browser), Vec::<String>::new());
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_conversations_answers_copies_and_the_dialog_take_the_keyboard() {
    let f = Fixture::new().await;
    for body in ["Build the crates.", "Then the docs.", "Then the review."] {
        note(&f, "alpha-build", body).await;
    }
    stored(
        &f.writer,
        f.id,
        Stored {
            thread: "step-alpha-build",
            from: "alpha-build",
            to: Some("owner"),
            body: "Ship it now?",
            question: true,
            ..Default::default()
        },
    )
    .await;
    let (addr, server) = serve(&f);
    let thread = format!(
        "http://{addr}/projects/id/{}/thread?thread=step-alpha-build",
        f.id
    );
    let (inbox, gallery) = (format!("http://{addr}/inbox"), format!("http://{addr}/_ui"));
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&thread).unwrap();
        browser.viewport(390, "dark").unwrap();
        browser.navigate(&thread).unwrap();
        browser.wait("document.readyState === 'complete' && customElements.get('sluice-conversation')").unwrap();
        browser.eval(FRAMES).unwrap();
        // a thread's page opens at its end, its message box in view
        assert_eq!(
            browser.eval("(r => r.bottom <= innerHeight + 1 && r.bottom > 0)(document.querySelector('.composer').getBoundingClientRect())").unwrap(),
            true
        );
        // Jump to latest gives the newest message the focus
        browser.eval("scrollTo(0, 0); document.querySelector('a.jump').click()").unwrap();
        assert_eq!(
            browser.eval("document.activeElement.id === [...document.querySelectorAll('.convo-list [id^=message-]')].at(-1).id").unwrap(),
            true
        );
        // the thread's name copies from its head's Details, the button saying so for a moment
        browser
            .send("Browser.grantPermissions", json!({"permissions": ["clipboardReadWrite", "clipboardSanitizedWrite"]}))
            .unwrap();
        browser.eval("document.querySelector('#messages-band details.dm > summary').click()").unwrap();
        browser.eval("document.querySelector('#messages-band .dm-l sluice-copy button.copy').click()").unwrap();
        browser.wait("document.querySelector('#messages-band .dm-l sluice-copy button.copy').hasAttribute('data-copied')").unwrap();
        assert_eq!(browser.eval("navigator.clipboard.readText()").unwrap(), json!("step-alpha-build"));
        assert_eq!(browser.eval("document.querySelector('#messages-band .dm-l sluice-copy .copy-said').textContent").unwrap(), json!("Copied"));
        // a question's Answer opens its box and puts the focus in it; again closes it
        browser.navigate(&inbox).unwrap();
        browser.wait("document.readyState === 'complete' && customElements.get('sluice-answer') && document.querySelector('button.q-toggle')").unwrap();
        browser.eval("document.querySelector('button.q-toggle').click()").unwrap();
        assert_eq!(
            browser.eval("[document.querySelector('button.q-toggle').getAttribute('aria-expanded'), document.querySelector('.q-box').hasAttribute('data-open'), !!document.activeElement.closest('.q-box')]").unwrap(),
            json!(["true", true, true])
        );
        // the dialog: focus starts on keep, Tab stays in it, Escape closes it back on its opener
        browser.navigate(&gallery).unwrap();
        browser.wait("document.readyState === 'complete' && customElements.get('sluice-confirm')").unwrap();
        browser.eval("document.querySelector('.gal-th sluice-confirm:not(.dm-acts > *) summary:not([aria-disabled=true])').click()").unwrap();
        assert_eq!(
            browser.eval("[document.querySelector('#confirmation').open, document.activeElement.matches('[data-keep]')]").unwrap(),
            json!([true, true])
        );
        press(&mut browser, "Tab", 9, 0);
        assert_eq!(browser.eval("document.querySelector('#confirmation').contains(document.activeElement)").unwrap(), true);
        press(&mut browser, "Escape", 27, 0);
        browser
            .wait("!document.querySelector('#confirmation').open && document.activeElement.matches('.gal-th sluice-confirm summary') && !!document.querySelector('.gal-th sluice-confirm details > form')")
            .unwrap();
        // a disabled one does nothing
        browser.eval("document.querySelector('.gal-th sluice-confirm summary[aria-disabled=true]').click()").unwrap();
        assert_eq!(browser.eval("document.querySelector('#confirmation').open").unwrap(), false);
        assert_eq!(logged(&mut browser), Vec::<String>::new());
    })
    .await
    .unwrap();
    server.abort();
}
