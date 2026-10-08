//! Real Chromium gate for the message pages and the page's times: a note is marked read only
//! after it has been on screen for about two seconds, and then kept under "Read just now"; a
//! time a stream patch writes back in the server's words reads in the page's words again at once.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
#[allow(dead_code)]
#[path = "../../../tests/support/messages.rs"]
mod stored_messages;
use board_fixture::Fixture;
use chrome::Chrome;
use sluice_model::commands::CommandRequest;
use std::time::{Duration, Instant};

fn read_marks(f: &Fixture) -> usize {
    f.commands
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|r| matches!(r, CommandRequest::MarkRead(_)))
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_a_note_is_read_only_once_seen_and_times_survive_a_patch() {
    let f = Fixture::new().await;
    stored_messages::stored(
        &f.writer,
        f.id,
        stored_messages::Stored {
            thread: "notes",
            from: "worker",
            to: Some("owner"),
            body: "The lane landed on main.",
            ..Default::default()
        },
    )
    .await;
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
        let mut browser = Chrome::open(&format!("{base}/inbox")).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("document.readyState === 'complete' && !!document.querySelector('article.note')")
            .unwrap();
        let shown = Instant::now();
        // on screen, but not yet for two seconds: not read
        std::thread::sleep(Duration::from_millis(900));
        assert_eq!(read_marks(&f), 0, "a note is not read the moment it renders");
        let deadline = Instant::now() + Duration::from_secs(10);
        while read_marks(&f) == 0 {
            assert!(Instant::now() < deadline, "a note on screen is marked read");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(shown.elapsed() >= Duration::from_millis(1800), "{:?}", shown.elapsed());
        assert_eq!(read_marks(&f), 1, "marked once");
        // what was read stays on the page under "Read just now"
        browser
            .wait("!document.querySelector('#read-now').hidden && document.querySelectorAll('#read-now article').length === 1")
            .unwrap();
        assert_eq!(
            browser.eval("document.querySelector('#read-now article').textContent.includes('The lane landed on main.')").unwrap(),
            true
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
