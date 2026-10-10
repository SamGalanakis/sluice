//! The dashboard's Rocket components (DESIGN.md, Components): Rust draws each host and all in
//! it, and the component only behaves. Over HTTP every page draws its components' hosts and no
//! template writes one by hand; in a real Chromium each component is defined as the server
//! declares it, behaves, keeps its state through a stream patch of its region, takes the
//! keyboard and logs nothing, and every page still reads with script disabled.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
#[allow(dead_code)]
#[path = "../../../tests/support/messages.rs"]
mod stored_messages;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::{Value, json};
use sluice_model::commands::CommandRequest;
use sluice_store::RetrySafety;
use sluice_web::views::ui::COMPONENTS;
use stored_messages::{Stored, stored};

/// The component tags a page's HTML draws, each once, in order of first use.
fn hosts(html: &str) -> Vec<String> {
    let mut seen: Vec<String> = vec![];
    for part in html.split("<sluice-").skip(1) {
        let tag = format!(
            "sluice-{}",
            &part[..part
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .unwrap()]
        );
        if !seen.contains(&tag) {
            seen.push(tag);
        }
    }
    seen
}
fn has(html: &str, tags: &[&str], page: &str) {
    let drawn = hosts(html);
    for tag in tags {
        assert!(drawn.iter().any(|t| t == tag), "{page}: {tag} in {drawn:?}");
    }
}
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

#[tokio::test]
async fn every_page_draws_its_components_hosts_and_none_writes_one_by_hand() {
    let f = Fixture::new().await;
    long_output(&f).await;
    note(&f, "alpha-build", "Build the crates.").await;
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
    stored(
        &f.writer,
        f.id,
        Stored {
            thread: "step-alpha-build",
            from: "alpha-build",
            to: Some("owner"),
            body: "Twelve crates built.",
            ..Default::default()
        },
    )
    .await;
    let p = format!("/projects/id/{}", f.id);
    let layout = ["sluice-menu", "sluice-toggle", "sluice-banner"];
    for (path, tags) in [
        ("/".to_owned(), &[][..]),
        (
            p.clone(),
            &[
                "sluice-search",
                "sluice-trace",
                "sluice-grid",
                "sluice-splitter",
                "sluice-drawer",
                "sluice-fold",
            ][..],
        ),
        (
            format!("{p}/steps/alpha-build"),
            &[
                "sluice-tabs",
                "sluice-fold",
                "sluice-conversation",
                "sluice-composer",
                "sluice-answer",
                "sluice-toggle",
            ][..],
        ),
        (
            format!("{p}/units/alpha"),
            &["sluice-grid", "sluice-conversation"][..],
        ),
        (
            format!("{p}/thread?thread=step-alpha-build"),
            &[
                "sluice-conversation",
                "sluice-composer",
                "sluice-copy",
                "sluice-answer",
            ][..],
        ),
        (
            "/inbox".to_owned(),
            &["sluice-conversation", "sluice-answer"][..],
        ),
        ("/fns".to_owned(), &["sluice-search"][..]),
        (
            format!("{p}/settings"),
            &["sluice-confirm", "sluice-fold"][..],
        ),
        (format!("{p}/log"), &[][..]),
        (format!("{p}/history"), &[][..]),
    ] {
        let (status, html) = f.get(&path).await;
        assert!(status.is_success(), "{path}: {status} {html}");
        has(&html, &layout, &path);
        has(&html, tags, &path);
    }
    // the gallery draws every component the page kit has (the board and the drawer are a
    // project page's), and lists them all
    let (_, gallery) = f.get("/_ui").await;
    for c in COMPONENTS {
        if c.script == "components.js" {
            has(&gallery, &[c.tag], "/_ui");
        }
        assert!(
            gallery.contains(&format!("<code>{}</code>", c.tag)),
            "{} listed",
            c.tag
        );
    }
    // a host's attributes are its props: the thread page opens at its end, the board's search
    // points the stream under the project, the settings' Delete is the shared confirmation
    let (_, thread) = f.get(&format!("{p}/thread?thread=step-alpha-build")).await;
    assert!(
        thread.contains("<sluice-conversation start=\"end\">"),
        "{thread}"
    );
    let (_, board) = f.get(&p).await;
    assert!(
        board.contains(&format!("<sluice-search mode=\"stream\" base=\"{p}\">")),
        "{board}"
    );
    assert!(board.contains(&format!("<sluice-drawer id=\"step-drawer\" base=\"{p}\">")));
    assert!(board.contains(&format!("store=\"sluice.boardw.{}\"", f.id)));
    let (_, settings) = f.get(&format!("{p}/settings")).await;
    assert!(
        settings.contains("<sluice-confirm heading=\"Delete lanes?\""),
        "{settings}"
    );
    assert!(
        settings.contains("<summary id=\"delete-button\""),
        "{settings}"
    );
    // no template or view writes a host itself: `views::ui::rocket` is the one place
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = vec![];
    for dir in ["templates", "src", "src/views"] {
        for entry in std::fs::read_dir(root.join(dir)).unwrap().flatten() {
            let path = entry.path();
            if path.is_file() {
                sources.push(path);
            }
        }
    }
    for path in sources {
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("<sluice-") && !text.contains("</sluice-"),
            "{} writes a component host by hand",
            path.display()
        );
    }
    // every component is defined by its script, and every script's component is listed
    let script = |name: &str| std::fs::read_to_string(root.join("assets").join(name)).unwrap();
    for c in COMPONENTS {
        let js = script(c.script);
        assert!(
            js.contains(&format!("define(\"{}\"", c.tag))
                || js.contains(&format!("rocket(\"{}\"", c.tag)),
            "{} defined in {}",
            c.tag,
            c.script
        );
    }
    for name in ["components.js", "sluice.js"] {
        let js = script(name);
        for call in ["define(\"sluice-", "rocket(\"sluice-"] {
            for part in js.split(call).skip(1) {
                let tag = format!("sluice-{}", &part[..part.find('"').unwrap()]);
                assert!(
                    COMPONENTS.iter().any(|c| c.tag == tag && c.script == name),
                    "{tag} listed"
                );
            }
        }
    }
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
async fn chromium_each_component_is_defined_as_the_server_declares_it() {
    let f = Fixture::new().await;
    let (addr, server) = serve(&f);
    let (gallery, board) = (
        format!("http://{addr}/_ui"),
        format!("http://{addr}/projects/id/{}", f.id),
    );
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&gallery).unwrap();
        for (page, script) in [(&gallery, "components.js"), (&board, "sluice.js")] {
            browser.navigate(page).unwrap();
            browser.wait("document.readyState === 'complete'").unwrap();
            for c in COMPONENTS.iter().filter(|c| c.script == script) {
                let declared = browser
                    .wait(&format!(
                        "(k => k && (m => ({{props: m.props.map(p => [p.attribute, p.type]), events: m.events.map(e => e.name), slots: m.slots.map(s => s.name)}}))(k.manifest()))(customElements.get('{}'))",
                        c.tag
                    ))
                    .unwrap();
                let props: Vec<Value> = c.props.iter().map(|(n, t)| json!([n, t])).collect();
                assert_eq!(declared["props"], json!(props), "{} props", c.tag);
                assert_eq!(declared["events"], json!(c.events), "{} events", c.tag);
                assert_eq!(declared["slots"], json!(c.slots), "{} slots", c.tag);
            }
        }
        // each instance keeps its state in its own scope, named by its host's id
        browser.navigate(&gallery).unwrap();
        browser.wait("document.readyState === 'complete' && customElements.get('sluice-tabs')").unwrap();
        assert_eq!(
            browser.eval("[...document.querySelectorAll('sluice-tabs')].map(t => t.rocketSignalPath)").unwrap(),
            json!(["_rocket.sluice_tabs.l_tabs_tabs", "_rocket.sluice_tabs.d_tabs_tabs"])
        );
        assert_eq!(logged(&mut browser), Vec::<String>::new());
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_the_step_page_components_keep_their_state_through_a_patch_and_take_the_keyboard()
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
        browser.wait("document.querySelector('#tp-messages .ou-status').textContent === 'Sent.'").unwrap();
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
async fn chromium_the_project_page_components_keep_their_state_through_a_patch() {
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
        // the plan's More menu open, the description's More open: a patch keeps them and the
        // board's width
        browser.eval("document.querySelector('details.about-more > summary').click()").unwrap();
        browser.eval("document.querySelector('details.tool-more > summary').click()").unwrap();
        let version = browser.eval("document.body.dataset.signals").unwrap();
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
        browser.wait("document.querySelector('li.sc[data-state=running] > #n-alpha-review')").unwrap();
        assert_ne!(browser.eval("document.body.dataset.signals").unwrap(), json!(null), "{version}");
        assert_eq!(
            browser.eval(&format!("[{width}, document.querySelector('details.tool-more').open, document.querySelector('details.about-more').open]")).unwrap(),
            json!([336, true, true])
        );
        // the menu: ArrowDown moves into it, Escape closes it back on its summary
        browser.eval("document.querySelector('details.tool-more > summary').focus()").unwrap();
        press(&mut browser, "ArrowDown", 40, 0);
        browser.wait("document.activeElement.closest('details.tool-more .menu')").unwrap();
        press(&mut browser, "Escape", 27, 0);
        assert_eq!(
            browser.eval("[document.querySelector('details.tool-more').open, document.activeElement.matches('details.tool-more > summary')]").unwrap(),
            json!([false, true])
        );
        // the description's More is kept for this project: open again after a reload, and the
        // board as wide
        browser.navigate(&page).unwrap();
        browser.wait(ready).unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(
            browser.eval(&format!("[{width}, document.querySelector('details.about-more').open]")).unwrap(),
            json!([336, true])
        );
        // the search: the address and the stream follow as one types, the banner stays hidden
        // (the old stream ends on purpose), Escape clears
        browser.eval("document.querySelector('form.board-tools input[name=q]').focus()").unwrap();
        browser.send("Input.insertText", json!({"text": "beta"})).unwrap();
        browser.wait("location.search === '?q=beta'").unwrap();
        browser.wait("document.querySelector('main').getAttribute('data-init').includes('/stream?q=beta')").unwrap();
        browser.wait("!document.querySelector('#n-alpha-build')").unwrap();
        assert_eq!(browser.eval("document.querySelector('#stream-state').hidden").unwrap(), true);
        press(&mut browser, "Escape", 27, 0);
        browser.wait("location.search === '' && document.querySelector('#n-alpha-build')").unwrap();
        // the drawer: a step opens beside the board, its tab goes into the address and a
        // reload opens it there; closing takes the tab out
        browser.navigate(&format!("{page}#step:alpha-build")).unwrap();
        browser.wait("document.querySelector('#step-detail sluice-tabs #tt-outputs')").unwrap();
        browser.eval("document.querySelector('#step-detail #tt-outputs').click()").unwrap();
        assert_eq!(browser.eval("location.search + location.hash").unwrap(), json!("?tab=outputs#step:alpha-build"));
        browser.navigate(&format!("{page}?tab=outputs#step:alpha-build")).unwrap();
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
async fn chromium_conversations_answers_copies_and_the_dialog_take_the_keyboard() {
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
        // the thread's name copies, the button saying so for a moment
        browser
            .send("Browser.grantPermissions", json!({"permissions": ["clipboardReadWrite", "clipboardSanitizedWrite"]}))
            .unwrap();
        browser.eval("document.querySelector('.thread-of sluice-copy button.copy').click()").unwrap();
        browser.wait("document.querySelector('.thread-of sluice-copy button.copy').hasAttribute('data-copied')").unwrap();
        assert_eq!(browser.eval("navigator.clipboard.readText()").unwrap(), json!("step-alpha-build"));
        assert_eq!(browser.eval("document.querySelector('.thread-of sluice-copy .copy-said').textContent").unwrap(), json!("Copied"));
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
        browser.eval("document.querySelector('.gal-th sluice-confirm:not([disabled]) summary').click()").unwrap();
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
        browser.eval("document.querySelector('.gal-th sluice-confirm[disabled] summary').click()").unwrap();
        assert_eq!(browser.eval("document.querySelector('#confirmation').open").unwrap(), false);
        assert_eq!(logged(&mut browser), Vec::<String>::new());
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_without_script_every_page_still_reads() {
    let f = Fixture::new().await;
    long_output(&f).await;
    note(&f, "alpha-build", "Build the crates.").await;
    stored(
        &f.writer,
        f.id,
        Stored {
            thread: "step-alpha-build",
            from: "alpha-build",
            to: Some("owner"),
            body: "Twelve crates built.",
            ..Default::default()
        },
    )
    .await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    let p = format!("/projects/id/{}", f.id);
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("{base}/")).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .send("Emulation.setScriptExecutionDisabled", json!({"value": true}))
            .unwrap();
        for (path, reads) in [
            (format!("{p}/steps/alpha-build"), "The rebase onto main conflicts"),
            (p.clone(), "alpha-build"),
            (format!("{p}/thread?thread=step-alpha-build"), "Build the crates."),
            ("/inbox".to_owned(), "Twelve crates built."),
            ("/fns".to_owned(), "Functions"),
            (format!("{p}/settings"), "Delete project"),
            ("/_ui".to_owned(), "States and parts"),
        ] {
            browser.navigate(&format!("{base}{path}")).unwrap();
            browser
                .wait(&format!(
                    "location.pathname + location.search === {} && document.readyState === 'complete'",
                    json!(path)
                ))
                .unwrap();
            let shown = browser
                .eval(&format!(
                    "({{reads: document.body.innerText.includes({}), scripted: [...document.querySelectorAll('.needs-js, .types-toggle, .tabbar')].filter(e => e.checkVisibility()).map(e => e.className), defined: [...document.querySelectorAll('*')].some(e => e.localName.startsWith('sluice-') && e.matches(':defined')), sideways: document.documentElement.scrollWidth > document.documentElement.clientWidth, text: document.body.innerText.slice(0, 300)}})",
                    json!(reads)
                ))
                .unwrap();
            let text = shown["text"].clone();
            let mut shown = shown;
            shown.as_object_mut().unwrap().remove("text");
            assert_eq!(
                shown,
                json!({"reads": true, "scripted": [], "defined": false, "sideways": false}),
                "{path}: {text}"
            );
        }
        // every tab's panel stands stacked, the display preferences keep their Save, and a
        // confirmation's form opens inline
        browser.navigate(&format!("{base}{p}/steps/alpha-build")).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        assert_eq!(
            browser.eval("[...document.querySelectorAll('.tp')].filter(p => p.checkVisibility()).map(p => p.dataset.tab).join(' ')").unwrap(),
            json!("overview messages outputs runs")
        );
        browser.eval("document.querySelector('details.settings').open = true").unwrap();
        assert_eq!(browser.eval("document.querySelector('form.prefs .save').checkVisibility()").unwrap(), true);
        browser.navigate(&format!("{base}/_ui")).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        browser.eval("document.querySelector('.gal-th sluice-confirm:not([disabled]) summary').click()").unwrap();
        assert_eq!(
            browser.eval("document.querySelector('.gal-th sluice-confirm:not([disabled]) form').checkVisibility()").unwrap(),
            true
        );
    })
    .await
    .unwrap();
    server.abort();
}
