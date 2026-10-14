//! Gallery trace rails and Details keyboard interactions.
use crate::board_fixture;
use crate::chrome;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::json;
use sluice_web::views::ui::{self};

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
async fn slow_chromium_the_plain_sheet_the_trace_details_and_the_head_at_every_width() {
    let f = Fixture::new().await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    tokio::task::spawn_blocking(move || {
        let gallery = format!("{base}/_ui");
        let mut browser = Chrome::open(&gallery).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&gallery).unwrap();
        browser.wait("document.readyState === 'complete' && !!customElements.get('sluice-trace')").unwrap();
        browser.eval(FRAMES).unwrap();

        // ---- the sheet is a plain element: no grid to show, nothing drawn over it, nothing kept
        let plain = browser.eval("[customElements.get('sluice-grid') === undefined, document.querySelector('#l-grid').localName, document.querySelectorAll('.grid-ovl, .gc, button.grid-toggle').length, [...document.querySelectorAll('button, a')].some(b => /show grid/i.test(b.textContent)), (() => { try { return localStorage.getItem('sluice.grid'); } catch { return null; } })()]").unwrap();
        assert_eq!(plain, json!([true, "div", 0, false, null]));

        // ---- the trace: select, the chain both ways, the fades, the rail and the sentence
        let host = "document.querySelector('#l-trace')";
        let roles = format!("Object.fromEntries([...{host}.querySelectorAll('[data-unit]')].map(u => [u.dataset.unit, [u.dataset.trace ?? '', getComputedStyle(u).opacity]]))");
        let said = format!("{host}.querySelector('.trace-words').textContent");
        assert_eq!(browser.eval(&said).unwrap(), ui::TRACE_IDLE);
        assert_eq!(browser.eval(&format!("{host}.querySelector('.trace-clear').checkVisibility()")).unwrap(), false);
        browser.eval(&format!("{host}.querySelector('[data-unit=\"a-13\"] [data-trace-pick]').click()")).unwrap();
        browser.wait(&format!("{host}.querySelector('[data-unit=\"a-15\"]').dataset.trace === 'faded' && getComputedStyle({host}.querySelector('[data-unit=\"a-15\"]')).opacity === '0.25'")).unwrap();
        assert_eq!(browser.eval(&said).unwrap(), "Tracing a-13. It waits for a-12 and s-3. a-14 waits on it.");
        let r = browser.eval(&roles).unwrap();
        assert_eq!(r["a-13"][0], "selected", "{r}");
        assert_eq!(r["a-12"][0], "up", "{r}");
        assert_eq!(r["s-3"][0], "up", "{r}");
        assert_eq!(r["a-14"][0], "down", "{r}");
        assert_eq!(r["a-15"], json!(["faded", "0.25"]), "{r}");
        assert_eq!(r["a-12"][1], "1", "{r}");
        let opened = browser.eval(&format!("(u => [u.querySelector('[data-trace-pick]').getAttribute('aria-pressed'), u.querySelector('[data-trace-pick]').getAttribute('aria-expanded'), u.querySelector('[data-trace-more]').hidden, getComputedStyle(u.querySelector('.trace-role'), '::before').content])({host}.querySelector('[data-unit=\"a-13\"]'))")).unwrap();
        assert_eq!(opened, json!(["true", "true", false, "\"Selected\""]));
        // the rail runs from s-3 (the first lit) down to a-14 (the last), through the head between
        let rail = browser.eval(&format!("[...{host}.querySelectorAll('.rail-slot')].map(s => (s.dataset.unit ?? 'head') + ':' + (s.querySelector(':scope > .rail').dataset.rail ?? ''))")).unwrap();
        assert_eq!(rail, json!(["head:", "s-3:bottom node", "a-12:top bottom node", "head:top bottom", "a-13:top bottom chosen", "a-14:top node", "a-15:"]));
        assert_eq!(browser.eval(&format!("{host}.querySelector('.trace-clear').checkVisibility()")).unwrap(), true);
        // the arrows move between the units' buttons; Escape clears and keeps the focus there
        browser.eval(&format!("{host}.querySelector('[data-unit=\"a-13\"] [data-trace-pick]').focus()")).unwrap();
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "ArrowDown", "code": "ArrowDown", "windowsVirtualKeyCode": 40})).unwrap();
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": "ArrowDown", "code": "ArrowDown", "windowsVirtualKeyCode": 40})).unwrap();
        assert_eq!(browser.eval("document.activeElement.closest('[data-unit]').dataset.unit").unwrap(), "a-14");
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27})).unwrap();
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27})).unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(browser.eval(&said).unwrap(), ui::TRACE_IDLE);
        assert_eq!(browser.eval(&format!("{host}.hasAttribute('selected') || {host}.hasAttribute('tracing')")).unwrap(), false);
        assert_eq!(browser.eval(&format!("[...{host}.querySelectorAll('[data-trace]')].length")).unwrap(), 0);
        assert_eq!(browser.eval("document.activeElement.closest('[data-unit]').dataset.unit").unwrap(), "a-13");
        // selecting again clears; Clear trace clears and gives the focus back
        browser.eval(&format!("{host}.querySelector('[data-unit=\"a-12\"] [data-trace-pick]').click()")).unwrap();
        assert_eq!(browser.eval(&format!("{host}.getAttribute('selected')")).unwrap(), "a-12");
        browser.eval(&format!("{host}.querySelector('[data-unit=\"a-12\"] [data-trace-pick]').click()")).unwrap();
        assert_eq!(browser.eval(&format!("{host}.hasAttribute('selected')")).unwrap(), false);
        browser.eval(&format!("{host}.querySelector('[data-unit=\"s-3\"] [data-trace-pick]').click()")).unwrap();
        browser.eval(&format!("{host}.querySelector('.trace-clear').click()")).unwrap();
        assert_eq!(browser.eval(&format!("[{host}.hasAttribute('selected'), document.activeElement.closest('[data-unit]')?.dataset.unit]")).unwrap(), json!([false, "s-3"]));

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
