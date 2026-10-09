//! Real Chromium gate for the message pages and the page's times: a note is marked read only
//! when the owner asks ("Mark read" on its card or its thread, or "Mark all read"), never by
//! being on screen or by its thread being opened; once marked it is kept under "Read today",
//! which the server draws from the stored read marks, so it survives a reload. A time a stream
//! patch writes back in the server's words reads in the page's words again at once.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
#[allow(dead_code)]
#[path = "../../../tests/support/messages.rs"]
mod stored_messages;
use board_fixture::Fixture;
use chrome::Chrome;
use sluice_model::commands::CommandRequest;
use sluice_store::RetrySafety;
use std::time::Duration;

fn read_marks(f: &Fixture) -> Vec<sluice_model::commands::MarkRead> {
    f.commands
        .0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|r| match r {
            CommandRequest::MarkRead(read) => Some(read.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_a_note_is_read_only_when_the_owner_asks_and_times_survive_a_patch() {
    let f = Fixture::new().await;
    for (thread, body) in [
        ("notes", "The lane landed on main."),
        ("later", "A second note, on its own thread."),
    ] {
        stored_messages::stored(
            &f.writer,
            f.id,
            stored_messages::Stored {
                thread,
                from: "worker",
                to: Some("owner"),
                body,
                ..Default::default()
            },
        )
        .await;
    }
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let lanes = f.id;
    let f = std::sync::Arc::new(f);
    let checks = f.clone();
    let rt = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let f = checks;
        let base = format!("http://{addr}");
        let mut browser = Chrome::open(&format!("{base}/inbox")).unwrap();
        browser.viewport(390, "light").unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelectorAll('article.note').length === 2")
            .unwrap();
        // on screen and scrolled past for well over the old two seconds: still unread
        browser.eval("window.scrollTo(0, document.body.scrollHeight)").unwrap();
        std::thread::sleep(Duration::from_millis(2600));
        assert!(read_marks(&f).is_empty(), "seeing a note never marks it read");
        // its card's Mark read marks that thread, through what the page drew of it
        browser
            .wait("[...document.querySelectorAll('article.note button.mark-read')].every(b => b.checkVisibility())")
            .unwrap();
        browser
            .eval("[...document.querySelectorAll('article.note')].find(a => a.textContent.includes('The lane landed')).querySelector('button.mark-read').click()")
            .unwrap();
        browser
            .wait("[...document.querySelectorAll('button.mark-read')].some(b => b.textContent === 'Marked read')")
            .unwrap();
        let marks = read_marks(&f);
        assert_eq!(marks.len(), 1, "{marks:?}");
        assert_eq!((marks[0].identity.as_str(), marks[0].thread.as_str()), ("owner", "notes"));
        // the coordinator takes the mark (the fixture records commands; the store applies it)
        let read = marks[0].clone();
        let writer = f.writer.clone();
        rt.block_on(writer.write(RetrySafety::Idempotent, move |tx| {
            sluice_store::messages::mark_read(tx, read)
        }))
        .unwrap();
        // the inbox now keeps it under "Read today", drawn from the stored marks: a reload
        // (twice) still shows it there, and the other note still unread
        for _ in 0..2 {
            browser.navigate(&format!("{base}/inbox")).unwrap();
            browser
                .wait("document.readyState === 'complete' && !!document.querySelector('details.read-today')")
                .unwrap();
            let state = browser
                .eval("[document.querySelector('details.read-today').textContent.includes('The lane landed on main.'), document.querySelector('section.notes').textContent.includes('The lane landed'), document.querySelector('section.notes').textContent.includes('A second note'), document.querySelector('details.read-today .n').textContent]")
                .unwrap();
            assert_eq!(state, serde_json::json!([true, false, true, "1"]), "{state}");
        }
        // opening a thread does not mark it read: it offers to
        browser
            .navigate(&format!("{base}/projects/id/{lanes}/thread?thread=later"))
            .unwrap();
        browser
            .wait("document.readyState === 'complete' && !!document.querySelector('.thread-reply')")
            .unwrap();
        std::thread::sleep(Duration::from_millis(2600));
        assert_eq!(read_marks(&f).len(), 1, "opening a thread marks nothing");
        assert_eq!(
            browser.eval("(b => b && b.checkVisibility() ? b.textContent : null)(document.querySelector('.thread-of button.mark-read'))").unwrap(),
            "Mark 1 note read"
        );
        // a stream patch puts the server's text back into a <time>; it reads in the page's
        // words again before the next frame (a characterData change, not a new node)
        browser
            .navigate(&format!("{base}/projects/id/{lanes}/steps/beta-build"))
            .unwrap();
        browser.wait("document.readyState === 'complete' && !!document.querySelector('#step-detail')").unwrap();
        let text = browser
            .eval("(async () => { const t = document.createElement('time'); const at = new Date(Date.now() - 3 * 86400e3 - 5 * 3600e3).toISOString(); t.dataset.ago = at; t.setAttribute('datetime', at); t.textContent = '2026-10-07 20:47 UTC'; document.querySelector('#step-detail').append(t); await new Promise(r => requestAnimationFrame(r)); const first = t.textContent; t.firstChild.nodeValue = '2026-10-07 20:47 UTC'; await Promise.resolve(); await new Promise(r => setTimeout(r, 0)); return [first, t.textContent]; })()")
            .unwrap();
        assert_eq!(text, serde_json::json!(["3d 5h ago", "3d 5h ago"]), "{text}");
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), serde_json::json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

/// A question to the owner is answered where it is read, the step's Overview among them, and
/// the answer is confirmed there: the buttons and box give way to "Answered: sent to … · Read
/// the thread", spoken; on the inbox the card keeps its place, the nav's count drops.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_an_answer_is_confirmed_where_it_was_given() {
    let f = Fixture::new().await;
    for (thread, from) in [("step-beta-build", "beta-build"), ("owner", "orchestrator")] {
        stored_messages::stored(
            &f.writer,
            f.id,
            stored_messages::Stored {
                thread,
                from,
                to: Some("owner"),
                body: "Land it before the rename?\n\n- **Now:** rebase after.\n- **Later:** wait.",
                title: Some("Land it before the rename?"),
                question: true,
                ..Default::default()
            },
        )
        .await;
    }
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let lanes = f.id;
    let f = std::sync::Arc::new(f);
    let checks = f.clone();
    tokio::task::spawn_blocking(move || {
        let f = checks;
        let base = format!("http://{addr}");
        let mut browser =
            Chrome::open(&format!("{base}/projects/id/{lanes}/steps/beta-build")).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("document.readyState === 'complete' && !!document.querySelector('#tp-overview sluice-answer .answer[data-drawn]')")
            .unwrap();
        // the Overview's copy: whole, its list kept, Answer opens its own box
        assert_eq!(
            browser.eval("document.querySelector('#tp-overview li.m-yours .m-body li strong')?.textContent").unwrap(),
            "Now:"
        );
        browser
            .eval("document.querySelector('#tp-overview sluice-answer button.q-toggle').click()")
            .unwrap();
        browser
            .wait("document.activeElement?.id?.startsWith('ov-reply-')")
            .unwrap();
        browser
            .eval("(t => { t.value = 'Land it now.'; t.form.requestSubmit(); })(document.activeElement)")
            .unwrap();
        browser
            .wait("!!document.querySelector('#tp-overview .q-answered[role=status] a')")
            .unwrap();
        // the server's one sentence, from the box's template, and the keyboard's focus with it
        let said = browser.eval("document.querySelector('#tp-overview .q-answered').textContent").unwrap();
        let said = said.as_str().unwrap();
        assert!(
            said.starts_with("Answered just now: Land it before the rename? sent to ")
                && said.contains("beta-build")
                && said.ends_with(" · Read the thread"),
            "{said}"
        );
        browser
            .wait("document.activeElement?.matches('#tp-overview .q-answered[role=status][aria-live=polite]')")
            .unwrap();

        assert!(
            browser
                .eval("document.querySelector('#tp-overview .q-answered .qa-to > a').getAttribute('href')")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("thread=step-beta-build#message-"),
        );
        // a patch that replaces the exchange hands the focus to the question as now drawn
        let question = browser
            .eval("document.querySelector('#tp-overview .q-answered').closest('li.m').id")
            .unwrap();
        browser
            .eval("(li => { const fresh = li.cloneNode(false); fresh.innerHTML = '<div class=\"md m-body\">Land it before the rename?</div><ol class=\"m-replies\"><li class=\"m-reply\" id=\"ov-message-999\">Land it now.</li></ol>'; li.replaceWith(fresh); })(document.querySelector('#tp-overview .q-answered').closest('li.m'))")
            .unwrap();
        browser.wait("document.activeElement?.id === 'ov-message-999'").unwrap();
        assert!(question.as_str().unwrap().starts_with("ov-message-"), "{question}");
        let answers: Vec<_> = f
            .commands
            .0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|r| match r {
                CommandRequest::Reply(reply) => Some((reply.body.clone(), reply.owner)),
                _ => None,
            })
            .collect();
        assert_eq!(answers, vec![("Land it now.".to_owned(), true)]);
        // on the inbox the card stays where it was, as the line that says so
        browser.navigate(&format!("{base}/inbox")).unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelectorAll('#inbox-items article.item.q').length === 2 && [...document.querySelectorAll('#inbox-items .answer')].every(a => a.dataset.drawn)")
            .unwrap();
        let badge = browser.eval("document.querySelector('#nav-inbox .badge').textContent").unwrap();
        browser
            .eval("(a => { a.querySelector('button.q-toggle').click(); const t = a.querySelector('textarea'); t.value = 'Accept it.'; t.form.requestSubmit(); })([...document.querySelectorAll('#inbox-items article.item.q')].at(-1))")
            .unwrap();
        browser
            .wait("!!document.querySelector('#inbox-items article.item.q.q-done .q-answered a')")
            .unwrap();
        browser
            .wait("document.activeElement?.matches('#inbox-items article.item.q-done .q-answered')")
            .unwrap();
        assert_eq!(
            browser.eval("document.querySelectorAll('#inbox-items article.item.q').length").unwrap(),
            2
        );
        assert_eq!(
            browser.eval("document.querySelector('#nav-inbox .badge')?.textContent ?? '0'").unwrap(),
            (badge.as_str().unwrap().parse::<u32>().unwrap() - 1).to_string()
        );
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), serde_json::json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

/// Close question confirms where it was pressed, as an answer does: the buttons give way to the
/// server's "Closed just now: …" line, a polite status that takes the keyboard's focus, so the
/// next Tab goes on from the question; the nav's count drops.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_closing_a_question_confirms_it_in_place_and_keeps_the_focus() {
    let f = Fixture::new().await;
    for (thread, from, title) in [
        (
            "step-beta-build",
            "beta-build",
            "Land it before the rename?",
        ),
        ("owner", "orchestrator", "Accept revision 3?"),
    ] {
        stored_messages::stored(
            &f.writer,
            f.id,
            stored_messages::Stored {
                thread,
                from,
                to: Some("owner"),
                body: "Say which.",
                title: Some(title),
                question: true,
                ..Default::default()
            },
        )
        .await;
    }
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let lanes = f.id;
    let f = std::sync::Arc::new(f);
    let checks = f.clone();
    tokio::task::spawn_blocking(move || {
        let f = checks;
        let base = format!("http://{addr}");
        let closes = |f: &Fixture| {
            f.commands
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|r| {
                    matches!(r, CommandRequest::Reply(reply)
                        if reply.owner && reply.answer.as_ref().is_some_and(|a| a.action == "close"))
                })
                .count()
        };
        // on the step's Overview, by keyboard: the focus lands on the line that says so
        let mut browser =
            Chrome::open(&format!("{base}/projects/id/{lanes}/steps/beta-build")).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("document.readyState === 'complete' && !!document.querySelector('#tp-overview sluice-answer .answer[data-drawn]') && customElements.get('sluice-answer')")
            .unwrap();
        browser
            .eval("(b => { b.focus(); b.click(); })(document.querySelector('#tp-overview sluice-answer form.q-close button'))")
            .unwrap();
        browser
            .wait("document.activeElement?.matches('#tp-overview .q-answered[role=status][aria-live=polite]')")
            .unwrap();
        let said = browser.eval("document.activeElement.textContent").unwrap();
        assert_eq!(said, "Closed just now: Land it before the rename? Read the thread");
        assert_eq!(closes(&f), 1);
        // on the inbox the same: its line in the card's place, focused, the count down by one
        browser.navigate(&format!("{base}/inbox")).unwrap();
        browser
            .wait("document.readyState === 'complete' && customElements.get('sluice-answer') && [...document.querySelectorAll('#inbox-items .answer')].every(a => a.dataset.drawn)")
            .unwrap();
        let badge = browser.eval("document.querySelector('#nav-inbox .badge').textContent").unwrap();
        browser
            .eval("[...document.querySelectorAll('#inbox-items article.item.q')].at(-1).querySelector('form.q-close button').click()")
            .unwrap();
        browser
            .wait("document.activeElement?.matches('#inbox-items article.item.q-done .q-answered')")
            .unwrap();
        let said = browser.eval("document.activeElement.textContent").unwrap();
        assert!(said.as_str().unwrap().starts_with("Closed just now: "), "{said}");
        assert_eq!(
            browser.eval("document.querySelector('#nav-inbox .badge')?.textContent ?? '0'").unwrap(),
            (badge.as_str().unwrap().parse::<u32>().unwrap() - 1).to_string()
        );
        assert_eq!(closes(&f), 2);
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), serde_json::json!([]));
    })
    .await
    .unwrap();
    server.abort();
}
