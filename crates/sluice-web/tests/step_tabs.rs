//! A step's page and drawer in tabs (Overview, Activity, Thread, Inputs, Outputs, Runs, each only
//! when it has something to show), the one conversation every message page draws, and the
//! kit's gallery: over HTTP, then in a real Chromium (the chosen tab through a stream patch, the
//! keyboard, deep links, the fallback without script, the message box) with the gallery's
//! screenshots at every width in both themes.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
#[allow(dead_code)]
#[path = "../../../tests/support/messages.rs"]
mod stored_messages;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::json;
use sluice_model::{commands::CommandRequest, ids::MessageId};
use sluice_store::RetrySafety;
use stored_messages::{Stored, stored};

fn between<'a>(html: &'a str, start: &str, end: &str) -> &'a str {
    let from = html
        .find(start)
        .unwrap_or_else(|| panic!("{start} in {html}"));
    let rest = &html[from..];
    &rest[..rest.find(end).map_or(rest.len(), |e| e + end.len())]
}
/// The tabs a page draws, in order, each its key and its count ("" for none).
fn tabs(html: &str) -> Vec<(String, String)> {
    let bar = between(html, "<div class=\"tablist\"", "</div>");
    bar.split("role=\"tab\"")
        .skip(1)
        .map(|tab| {
            let key = tab.split_once("data-tab=\"").unwrap().1;
            let key = key[..key.find('"').unwrap()].to_owned();
            let count = tab
                .split_once("class=\"n\"")
                .map(|(_, rest)| {
                    let rest = &rest[rest.find('>').unwrap() + 1..];
                    rest[..rest.find('<').unwrap()].to_owned()
                })
                .unwrap_or_default();
            (key, count)
        })
        .collect()
}
fn tab_list(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, n)| ((*k).to_owned(), (*n).to_owned()))
        .collect()
}
/// alpha-build's conversation: the orchestrator's note to it (a day earlier), a question it put
/// to alpha-review and the reply, and its two notes to the owner, the second unread.
async fn converse(f: &Fixture) -> (MessageId, MessageId, MessageId) {
    let first = stored(
        &f.writer,
        f.id,
        Stored {
            thread: "step-alpha-build",
            from: "orchestrator",
            to: Some("alpha-build"),
            body: "Build the crates; the review waits on your summary.",
            ..Default::default()
        },
    )
    .await;
    let ask = stored(
        &f.writer,
        f.id,
        Stored {
            thread: "step-alpha-build",
            from: "alpha-build",
            to: Some("alpha-review"),
            body: "Do you need the crate list in the summary?",
            question: true,
            ..Default::default()
        },
    )
    .await;
    let reply = stored(
        &f.writer,
        f.id,
        Stored {
            thread: "step-alpha-build",
            from: "alpha-review",
            to: Some("alpha-build"),
            body: "Yes, one per line.",
            reply: Some(ask.id),
            ..Default::default()
        },
    )
    .await;
    let read = stored(
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
    let unread = stored(
        &f.writer,
        f.id,
        Stored {
            thread: "step-alpha-build",
            from: "alpha-build",
            to: Some("owner"),
            body: "The summary is ready.",
            ..Default::default()
        },
    )
    .await;
    let (id, first_id, read_id) = (f.id, first.id.0, read.id.0);
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            // the first a day before the rest; the owner has read through the first note
            tx.sql().execute(
                "UPDATE messages SET at='2026-10-07T09:00:00Z' WHERE project_id=?1 AND id=?2",
                (id.to_string(), first_id),
            )?;
            tx.sql().execute(
                "INSERT INTO readers(project_id,identity,stream,thread,cursor) VALUES (?1,'owner','owner','step-alpha-build',?2)",
                (id.to_string(), read_id),
            )?;
            tx.changed(Some(id), "messages");
            Ok(())
        })
        .await
        .unwrap();
    (ask.id, reply.id, unread.id)
}

#[tokio::test]
async fn a_step_draws_only_the_tabs_it_has_something_for_with_their_counts() {
    let f = Fixture::new().await;
    let step = |s: &str| format!("/projects/id/{}/steps/{s}", f.id);
    // a failed step with nothing else: Overview, and Runs saying none is kept
    let (_, html) = f.get(&step("beta-build")).await;
    assert_eq!(
        tabs(&html),
        tab_list(&[("overview", ""), ("runs", "")]),
        "{html}"
    );
    // a pending step that reads an input and has no outputs, runs or messages
    let (_, html) = f.get(&step("alpha-review")).await;
    assert_eq!(tabs(&html), tab_list(&[("overview", ""), ("inputs", "1")]));
    // a succeeded step with its output, and then a conversation
    let (_, html) = f.get(&step("alpha-build")).await;
    assert_eq!(
        tabs(&html),
        tab_list(&[("overview", ""), ("outputs", "1"), ("runs", "")])
    );
    converse(&f).await;
    let (_, html) = f.get(&step("alpha-build")).await;
    assert_eq!(
        tabs(&html),
        tab_list(&[
            ("overview", ""),
            ("messages", "5"),
            ("outputs", "1"),
            ("runs", "")
        ])
    );
    // the header no longer links the thread page once the Thread tab holds its messages
    assert!(!html.contains("class=\"d-thread\""), "{html}");
    // Overview is chosen; its panel leads, every panel drawn (stacked without script)
    assert!(
        html.contains("<sluice-tabs id=\"tabs\" class=\"tabs\" current=\"overview\" url data-preserve-attr=\"current\">"),
        "{html}"
    );
    assert!(html.contains(
        "id=\"tt-overview\" aria-controls=\"tp-overview\" aria-selected=\"true\" tabindex=\"0\""
    ));
    assert!(html.contains("<!--r:tp-overview--><section class=\"tp\" id=\"tp-overview\" role=\"tabpanel\" aria-labelledby=\"tt-overview\" data-tab=\"overview\" data-chosen"));
    for key in ["messages", "outputs", "runs"] {
        assert!(
            html.contains(&format!("<section class=\"tp\" id=\"tp-{key}\" role=\"tabpanel\" aria-labelledby=\"tt-{key}\" data-tab=\"{key}\" data-preserve-attr")),
            "{key}: {html}"
        );
    }
    assert!(
        !html.contains(" hidden data-tab"),
        "no panel is hidden by the server"
    );
    // its key output leads Overview; its last message after it
    let overview = between(&html, "id=\"tp-overview\"", "<!--/r:tp-overview-->");
    // its only output: no way to "all" of one
    assert!(
        overview.contains("<h3 id=\"ov-key-h\">Its output</h3></div>"),
        "{overview}"
    );
    assert!(overview.contains("Built &#60;12&#62; crates"), "{overview}");
    assert!(
        overview.contains("<h3 id=\"ov-last-h\">Its last message</h3>")
            && overview.contains("<p class=\"m-text\">The summary is ready.</p>"),
        "{overview}"
    );
}

#[tokio::test]
async fn the_tab_asked_for_opens_and_an_unknown_one_falls_back_to_overview() {
    let f = Fixture::new().await;
    let page = format!("/projects/id/{}/steps/alpha-build", f.id);
    let (_, html) = f.get(&format!("{page}?tab=outputs")).await;
    assert!(html.contains("current=\"outputs\""), "{html}");
    assert!(html.contains("data-tab=\"outputs\" data-chosen"), "{html}");
    assert!(
        !html.contains("data-tab=\"overview\" data-chosen"),
        "{html}"
    );
    assert!(
        html.contains("data-signals__ifmissing=\"{sver: '', tab: 'outputs'}\""),
        "{html}"
    );
    // a tab it has no content for (no messages: no Thread tab) opens Overview
    let (_, html) = f.get(&format!("{page}?tab=thread")).await;
    assert!(html.contains("current=\"overview\""), "{html}");
    let (_, html) = f.get(&format!("{page}?tab=nonsense")).await;
    assert!(html.contains("data-tab=\"overview\" data-chosen"), "{html}");
}

#[tokio::test]
async fn outputs_say_a_repeated_value_once_and_inputs_where_theirs_came_from() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "dupes",
            json!({"steps":{
                "w":{"run":"custom.open","outputs":{"summary":"string","final":"string","ready":"boolean","done":"boolean"}},
                "r":{"run":"custom.open","in":{"s":{"source":"w/summary"},"mode":{"default":"fast"}}}}}),
            &[("w", "succeeded")],
        )
        .await;
    let text = "Landed the parser fix on main; every fixture passes and the docs follow.";
    let outputs = json!({"summary": text, "final": text, "ready": true, "done": true}).to_string();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET outputs=?2 WHERE project_id=?1 AND step_id='w'",
                (id.to_string(), outputs),
            )?;
            tx.changed(Some(id), "steps");
            Ok(())
        })
        .await
        .unwrap();
    let (_, html) = f
        .get(&format!("/projects/id/{id}/steps/w?tab=outputs"))
        .await;
    let outputs = between(&html, "id=\"tp-outputs\"", "<!--/r:tp-outputs-->");
    // the later field equal to an earlier one says so instead of repeating it; a short value
    // (true twice) is said each time
    assert_eq!(outputs.matches(text).count(), 1, "{outputs}");
    assert!(
        outputs.contains("<div class=\"f\" id=\"out-final\"><dt class=\"f-k\"><span class=\"f-name\">final</span>")
            && outputs.contains("<span class=\"v quiet f-same\">Same as <a href=\"#out-summary\">summary</a></span>"),
        "{outputs}"
    );
    assert_eq!(outputs.matches("f-same").count(), 1, "{outputs}");
    assert!(html.contains("<button type=\"button\" role=\"tab\" id=\"tt-outputs\" aria-controls=\"tp-outputs\" aria-selected=\"true\" tabindex=\"0\" data-tab=\"outputs\" data-preserve-attr=\"aria-selected tabindex\" aria-label=\"Outputs, all set\">Outputs<span class=\"n\" aria-hidden=\"true\">4</span></button>"), "{html}");
    // an input says where its value came from: another step's output (a link to it) or its
    // default
    let (_, html) = f
        .get(&format!("/projects/id/{id}/steps/r?tab=inputs"))
        .await;
    let inputs = between(&html, "id=\"tp-inputs\"", "<!--/r:tp-inputs-->");
    assert!(
        inputs.contains(&format!("<span class=\"f-src\">From <a href=\"/projects/id/{id}/steps/w\" data-opens=\"w\"><code>w</code></a> · summary</span>")),
        "{inputs}"
    );
    assert!(
        inputs.contains("<span class=\"f-src\">Default</span>"),
        "{inputs}"
    );
    // a pending step's outputs not set yet: "0/…" on its tab, named on one line
    let (_, html) = f
        .get(&format!("/projects/id/{}/steps/alpha-review", f.id))
        .await;
    assert!(!html.contains("tt-outputs"), "{html}");
}

#[tokio::test]
async fn one_conversation_on_the_steps_thread_tab_its_thread_page_and_the_inbox() {
    let f = Fixture::new().await;
    let (ask, reply, unread) = converse(&f).await;
    let (_, step) = f
        .get(&format!(
            "/projects/id/{}/steps/alpha-build?tab=thread",
            f.id
        ))
        .await;
    let tab = between(&step, "id=\"tp-messages\"", "<!--/r:tp-messages-->");
    let (_, page) = f
        .get(&format!(
            "/projects/id/{}/thread?thread=step-alpha-build",
            f.id
        ))
        .await;
    let thread = between(&page, "<div class=\"convo\">", "</form>");
    for (place, html) in [("tab", tab), ("thread page", thread)] {
        // a line at each day's start: the first message a day before the rest
        assert_eq!(
            html.matches("class=\"m-day\"").count(),
            2,
            "{place}: {html}"
        );
        assert!(
            html.contains(
                "<li class=\"m-day\" role=\"separator\"><span>Wed 7 Oct 2026</span></li>"
            ),
            "{place}: {html}"
        );
        // a group per run of messages from one sender, named by who sent it to whom
        assert!(html.contains("<li class=\"mg mg-in\"><p class=\"mg-head\"><span class=\"who\">Orchestrator</span>"), "{place}: {html}");
        assert!(
            html.contains("<span class=\"who who-this\">This step</span>"),
            "{place}: {html}"
        );
        // the question, then its reply directly under it, inside its card
        let question = between(html, &format!("id=\"message-{}\"", ask.0), "</ol>");
        assert!(
            question.contains("<span class=\"m-q\">"),
            "{place}: {question}"
        );
        assert!(
            question.contains("<span class=\"tag muted\">Answered</span>"),
            "{place}: {question}"
        );
        assert!(
            question.contains(&format!(
                "<ol class=\"m-replies\"><li class=\"m-reply\" id=\"message-{}\"",
                reply.0
            )),
            "{place}: {question}"
        );
        assert!(
            !html.contains("reply to <a"),
            "a reply here sits under its question: {html}"
        );
        // its two notes to the owner, one group; "New" before the one not read yet
        let new = html.find("class=\"m-new\"").expect("a New line");
        assert!(
            new < html.find(&format!("id=\"message-{}\"", unread.0)).unwrap(),
            "{place}: {html}"
        );
        assert!(
            new > html.find("Twelve crates built.").unwrap(),
            "{place}: {html}"
        );
        assert_eq!(
            html.matches("class=\"m-new\"").count(),
            1,
            "{place}: {html}"
        );
        // ids in each message's Details, times relative with the absolute one in their title
        assert!(
            !html.contains("class=\"m-id\"")
                && html.contains(&format!(
                    "<dt>Message</dt><dd><sluice-copy value=\"#{}\">",
                    unread.0
                )),
            "{place}: {html}"
        );
        assert!(html.contains("<time data-ago=\"2026-10-07T09:00:00Z\" datetime=\"2026-10-07T09:00:00Z\" title=\"2026-10-07 09:00 UTC\">"), "{place}: {html}");
        assert!(html.contains("5 messages"), "{place}: {html}");
    }
    // each with its message box to the step, pinned at its end
    for html in [&step, &page] {
        assert!(
            html.contains(
                "<form class=\"composer thread-reply\" data-ignore-morph method=\"post\""
            ),
            "{html}"
        );
        assert!(
            html.contains("<input type=\"hidden\" name=\"to\" value=\"alpha-build\">"),
            "{html}"
        );
        assert!(html.contains("<input type=\"checkbox\" name=\"ask\" value=\"true\"> Ask a question that needs a reply"), "{html}");
    }
    // the inbox draws its unread notes the same way, flat in their card, no New line
    let (_, inbox) = f.get("/inbox").await;
    let notes = between(&inbox, "<section class=\"notes\"", "</section>");
    assert!(notes.contains("<div class=\"convo\">"), "{notes}");
    assert!(
        notes.contains("The summary is ready.") && !notes.contains("m-new"),
        "{notes}"
    );
    assert!(
        notes.contains("<p class=\"mg-head\"><span class=\"who who-step\">"),
        "{notes}"
    );
}

#[tokio::test]
async fn an_open_question_to_the_owner_is_the_one_coral_tag() {
    let f = Fixture::new().await;
    stored(
        &f.writer,
        f.id,
        Stored {
            thread: "step-beta-build",
            from: "beta-build",
            to: Some("owner"),
            body: "Retry with the flaky test skipped?",
            question: true,
            ..Default::default()
        },
    )
    .await;
    let (_, step) = f
        .get(&format!(
            "/projects/id/{}/steps/beta-build?tab=thread",
            f.id
        ))
        .await;
    assert!(
        step.contains("<span class=\"tag ask\">Awaiting your reply</span>"),
        "{step}"
    );
    // the header says its question waits on you, in coral, and leads to it on Overview, where
    // it is drawn whole with Answer
    assert!(
        step.contains("<a class=\"tag ask\" href=\"#ov-message-"),
        "{step}"
    );
    assert!(!step.contains("1 awaiting reply"), "{step}");
    let at =
        step.find("<a class=\"tag ask\" href=\"#").unwrap() + "<a class=\"tag ask\" href=\"#".len();
    let anchor = &step[at..at + step[at..].find('"').unwrap()];
    assert!(
        step.contains(&format!("id=\"{anchor}\"")),
        "{anchor}: {step}"
    );
    let (_, inbox) = f.get("/inbox").await;
    // the questions list draws each question as the Thread tab's card: a one-message
    // conversation under who asked whom
    let card = between(&inbox, "class=\"mod swell-ask item q\"", "</article>");
    assert!(card.contains("<li class=\"m m-ask m-yours\""), "{card}");
    assert!(
        card.contains("<span class=\"tag ask\">Awaiting your reply</span>"),
        "{card}"
    );
    assert!(card.contains("<p class=\"mg-head\">"), "{card}");
    assert!(card.contains("<span class=\"who who-step\">"), "{card}");
}

#[tokio::test]
async fn the_kit_gallery_draws_every_part_in_both_themes() {
    let f = Fixture::new().await;
    let (status, html) = f.get("/_ui").await;
    assert_eq!(status, axum::http::StatusCode::OK);
    for part in [
        "Status",
        "Page head",
        "Step head",
        "Details",
        "Finished last",
        "Summary sentence",
        "Module grid",
        "Modules",
        "Section heads",
        "Stage strip",
        "Margin module",
        "Trace",
        "Tags",
        "Buttons",
        "Tabs",
        "Sections and cards",
        "Fields",
        "Folds",
        "Conversation",
        "Empty states",
        "Confirmation",
        "Menus",
        "Copy",
        "Settings",
        "Search",
        "Banners",
        "Notice",
        "Keys",
        "Splitter",
        "Themes",
        "Components",
    ] {
        assert!(html.contains(&format!("-h\">{part}</h2>")), "{part}");
    }
    assert_eq!(
        html.matches("<div class=\"gal-th\" data-theme=\"sluice-light\">")
            .count(),
        29
    );
    assert_eq!(
        html.matches("<div class=\"gal-th\" data-theme=\"sluice-dark\">")
            .count(),
        29
    );
    // its two copies keep their ids apart
    let mut ids: Vec<&str> = html
        .split(" id=\"")
        .skip(1)
        .map(|s| &s[..s.find('"').unwrap()])
        .collect();
    let all = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), all, "every id once");
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
const CHOSEN: &str = "[document.querySelector('sluice-tabs').current, document.querySelector('[role=tab][aria-selected=true]').dataset.tab, [...document.querySelectorAll('.tp')].filter(p => p.checkVisibility()).map(p => p.dataset.tab).join(' ')]";

#[tokio::test(flavor = "multi_thread")]
async fn chromium_tabs_keep_their_choice_through_a_patch_move_by_keyboard_and_open_on_a_link() {
    let f = std::sync::Arc::new(Fixture::new().await);
    let (ask, _, _) = converse(&f).await;
    let (addr, server) = serve(&f);
    let page = format!("http://{addr}/projects/id/{}/steps/alpha-build", f.id);
    let (writer, project) = (f.writer.clone(), f.id);
    let rt = tokio::runtime::Handle::current();
    let checks = f.clone();
    tokio::task::spawn_blocking(move || {
        let f = checks;
        let mut browser = Chrome::open(&page).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("document.readyState === 'complete' && !!customElements.get('sluice-tabs') && !!document.querySelector('sluice-tabs')")
            .unwrap();
        assert_eq!(browser.eval(CHOSEN).unwrap(), json!(["overview", "overview", "overview"]));
        // a click chooses Outputs: its panel alone shows, the address says so
        browser.eval("document.querySelector('#tt-outputs').click()").unwrap();
        assert_eq!(browser.eval(CHOSEN).unwrap(), json!(["outputs", "outputs", "outputs"]));
        assert_eq!(browser.eval("location.search").unwrap(), "?tab=outputs");
        // a message arrives: the stream patches the page (the Thread tab counts 6), the choice
        // stays
        rt.block_on(stored(
            &writer,
            project,
            Stored {
                thread: "step-alpha-build",
                from: "orchestrator",
                to: Some("alpha-build"),
                body: "One more thing: tag the release.",
                ..Default::default()
            },
        ));
        browser
            .wait("document.querySelector('#tt-messages .n')?.textContent === '6'")
            .unwrap();
        assert_eq!(browser.eval(CHOSEN).unwrap(), json!(["outputs", "outputs", "outputs"]));
        // the keyboard: arrows move along the bar (wrapping), Home and End to its ends
        browser.eval("document.querySelector('#tt-outputs').focus()").unwrap();
        let key = |browser: &mut Chrome, key: &str, code: u32| {
            for kind in ["keyDown", "keyUp"] {
                browser
                    .send(
                        "Input.dispatchKeyEvent",
                        json!({"type": kind, "key": key, "code": key, "windowsVirtualKeyCode": code}),
                    )
                    .unwrap();
            }
            browser.eval("[document.activeElement.dataset.tab, document.querySelector('sluice-tabs').current]").unwrap()
        };
        assert_eq!(key(&mut browser, "ArrowRight", 39), json!(["runs", "runs"]));
        assert_eq!(key(&mut browser, "ArrowRight", 39), json!(["overview", "overview"]));
        assert_eq!(key(&mut browser, "ArrowLeft", 37), json!(["runs", "runs"]));
        assert_eq!(key(&mut browser, "Home", 36), json!(["overview", "overview"]));
        assert_eq!(key(&mut browser, "End", 35), json!(["runs", "runs"]));
        assert_eq!(browser.eval("location.search").unwrap(), "?tab=runs");
        // a deep link to a message opens the Thread tab on it
        browser
            .navigate(&format!("{page}#message-{}", ask.0))
            .unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(browser.eval(CHOSEN).unwrap(), json!(["messages", "messages", "messages"]));
        let shown = browser
            .eval(&format!("(r => r.top >= 0 && r.bottom <= innerHeight)(document.getElementById('message-{}').getBoundingClientRect())", ask.0))
            .unwrap();
        assert_eq!(shown, true);
        // the message box sends a note to the step, and keeps the page
        browser.eval("document.querySelector('#tp-messages textarea').value = 'Ship it after the tag.'").unwrap();
        browser.eval("document.querySelector('#tp-messages form.composer button').click()").unwrap();
        browser
            .wait("document.querySelector('#tp-messages .ou-status').textContent === 'Sent.'")
            .unwrap();
        let sent: Vec<_> = f
            .commands
            .0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                CommandRequest::Say(say) => Some((say.to.clone(), say.body.clone(), say.owner)),
                _ => None,
            })
            .collect();
        assert_eq!(sent, [("alpha-build".to_owned(), "Ship it after the tag.".to_owned(), true)]);
        // without script every panel stands stacked under its head, and the bar is gone
        browser
            .send("Emulation.setScriptExecutionDisabled", json!({"value": true}))
            .unwrap();
        browser.navigate(&format!("{page}?tab=runs")).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        let stacked = browser
            .eval("[[...document.querySelectorAll('.tp')].filter(p => p.checkVisibility()).map(p => p.dataset.tab).join(' '), document.querySelector('.tabbar').checkVisibility(), [...document.querySelectorAll('.tp-h')].every(h => h.getBoundingClientRect().height > 10)]")
            .unwrap();
        assert_eq!(stacked, json!(["overview messages outputs runs", false, true]));
        browser
            .send("Emulation.setScriptExecutionDisabled", json!({"value": false}))
            .unwrap();
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_the_kit_gallery_at_every_width_and_theme() {
    let f = Fixture::new().await;
    let (addr, server) = serve(&f);
    let page = format!("http://{addr}/_ui");
    tokio::task::spawn_blocking(move || {
        let screens = std::env::var_os("SLUICE_UI_SCREENS").map(std::path::PathBuf::from);
        let mut browser = Chrome::open(&page).unwrap();
        for width in [390, 1440, 2560] {
            for theme in ["light", "dark"] {
                browser.viewport(width, theme).unwrap();
                browser.navigate(&page).unwrap();
                browser.wait("document.readyState === 'complete' && !!customElements.get('sluice-splitter')").unwrap();
                browser.eval(FRAMES).unwrap();
                let g = browser
                    .eval("({scroll: document.documentElement.scrollWidth, width: document.documentElement.clientWidth, pairs: [...document.querySelectorAll('.gal-pair:not(.gal-wide)')].map(p => getComputedStyle(p).gridTemplateColumns.split(' ').length), tabs: document.querySelectorAll('sluice-tabs .tp[data-chosen]').length})")
                    .unwrap();
                let label = format!("{width} {theme}");
                assert!(g["scroll"].as_f64() <= g["width"].as_f64(), "{label}: sideways {g}");
                // light and dark side by side once the column is wide enough, stacked on a phone
                // (a part that needs the grid's room stacks them at every width)
                let columns = if width == 390 { 1 } else { 2 };
                assert!(g["pairs"].as_array().unwrap().iter().all(|n| n == columns), "{label}: {g}");
                assert_eq!(g["tabs"], 2, "{label}: one chosen panel in each tab set");
                if let Some(dir) = &screens {
                    browser.screenshot(&dir.join(format!("ui-{width}-{theme}.png"))).unwrap();
                }
            }
        }
        // the gallery's confirmation opens the shared dialog
        browser.eval("document.querySelector('.gal-th sluice-confirm:not([disabled]) details.confirm-flow > summary').click()").unwrap();
        assert_eq!(browser.eval("document.querySelector('#confirmation').open").unwrap(), true);
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}
