//! The gallery's plain sheet and Details keyboard interactions.
use crate::board_fixture;
use crate::chrome;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::json;

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

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_the_plain_sheet_and_details_by_keyboard() {
    let f = Fixture::new().await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    tokio::task::spawn_blocking(move || {
        let gallery = format!("{base}/_ui");
        let mut browser = Chrome::open(&gallery).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&gallery).unwrap();
        browser.wait("document.readyState === 'complete' && !!customElements.get('sluice-menu')").unwrap();
        browser.eval(FRAMES).unwrap();

        // ---- the sheet is a plain element: no grid to show, nothing drawn over it, nothing kept
        let plain = browser.eval("[customElements.get('sluice-grid') === undefined, document.querySelector('#l-grid').localName, document.querySelectorAll('.grid-ovl, .gc, button.grid-toggle').length, [...document.querySelectorAll('button, a')].some(b => /show grid/i.test(b.textContent)), (() => { try { return localStorage.getItem('sluice.grid'); } catch { return null; } })()]").unwrap();
        assert_eq!(plain, json!([true, "div", 0, false, null]));

        // ---- Details: the "⋯" opens by keyboard, says it is open, Escape closes it and gives
        // the focus back; each id is whole with its copy button
        let dm = "document.querySelector('.gal-sec .dm')";
        browser.eval(&format!("{dm}.querySelector('summary').focus()")).unwrap();
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13, "text": "\r"})).unwrap();
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13})).unwrap();
        browser.eval(FRAMES).unwrap();
        let open = browser.eval(&format!("(d => [d.open, d.querySelector('summary').getAttribute('aria-expanded'), d.querySelector('.dm-p').checkVisibility(), d.querySelectorAll('.dm-l sluice-copy, .dm-l button.copy').length > 0])({dm})")).unwrap();
        assert_eq!(open, json!([true, "true", true, true]));
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27})).unwrap();
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27})).unwrap();
        browser.eval(FRAMES).unwrap();
        let shut = browser.eval(&format!("(d => [d.open, d.querySelector('summary').getAttribute('aria-expanded'), document.activeElement === d.querySelector('summary')])({dm})")).unwrap();
        assert_eq!(shut, json!([false, "false", true]));

        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}
