//! A step's page and drawer in tabs (Overview, Activity, Thread, Inputs, Outputs, Runs, each only
//! when it has something to show), the one conversation every message page draws, and the
//! kit's gallery: over HTTP, then in a real Chromium (the chosen tab through a stream patch, the
//! keyboard, deep links, the message box) with the gallery's
//! screenshots at every width in both themes.
use crate::board_fixture;
use crate::chrome;
use crate::stored_messages;
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
    let bar = between(html, "<div class=\"tabs\" role=\"tablist\"", "</div>");
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
async fn slow_chromium_tabs_keep_their_choice_through_a_patch_move_by_keyboard_and_open_on_a_link()
{
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
        // a click chooses Outputs: its panel alone shows
        browser.eval("document.querySelector('#tt-outputs').click()").unwrap();
        assert_eq!(browser.eval(CHOSEN).unwrap(), json!(["outputs", "outputs", "outputs"]));
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
            .wait("document.querySelector('#tp-messages .form-said').textContent === 'Sent.'")
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
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test]
async fn tabs_values_and_owner_conversations() {
    let f = Fixture::new().await;
    let conversation;
    {
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
    {
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
        conversation = converse(&f).await;
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
        // Overview is chosen; its panel leads, every panel drawn
        assert!(
            html.contains(
                "<sluice-tabs id=\"tabs\" current=\"overview\" data-preserve-attr=\"data-rocket-host current\">"
            ),
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
                && overview.contains("<div class=\"mc-b\"><p>The summary is ready.</p></div>"),
            "{overview}"
        );
    }
    {
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
        let outputs =
            json!({"summary": text, "final": text, "ready": true, "done": true}).to_string();
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
    {
        let (ask, reply, unread) = conversation;
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
            assert!(html.contains("<li class=\"mg mg-in\"><p class=\"mg-head\"><span class=\"who who-old\">orchestrator</span>"), "{place}: {html}");
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
                question.contains("<span class=\"badge muted\">Answered</span>"),
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
                        "<dt>Message</dt><dd><sluice-copy value=\"#{}\" data-preserve-attr=\"data-rocket-host\">",
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
                html.contains("class=\"composer thread-reply\" method=\"post\""),
                "{html}"
            );
            assert!(
                html.contains("<input type=\"hidden\" name=\"to\" value=\"alpha-build\">"),
                "{html}"
            );
            assert!(
                html.contains(
                    "<input type=\"checkbox\" name=\"ask\" value=\"true\" data-bind__root=\"_ask"
                ),
                "{html}"
            );
            assert!(
                html.contains("> Ask a question that needs a reply"),
                "{html}"
            );
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
            notes.contains("<p class=\"mg-head\"><span class=\"who who-this\">This step</span>"),
            "{notes}"
        );
    }
}
