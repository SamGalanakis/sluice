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
