//! The step view (DESIGN.md, The step view) on the plan, in a real Chromium: a row at work
//! opens its steps in place under it (Enter or Space on the row), its rows in the unit's order
//! with their states, a click on a row opens the step in the drawer, the arrows move between
//! rows, the view stays open through a stream patch that moves its unit from running to
//! waiting, and nothing clips or scrolls sideways at 390, 1440 and 2560 in Sluice Light, Sluice
//! Dark and Interstate Light.
use crate::{board_fixture, chrome, neutral};
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::{Value, json};
use sluice_store::RetrySafety;

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
/// A key pressed and let go; Enter and Space carry their text, so a summary acts on them.
fn press(browser: &mut Chrome, key: &str, code: u32) {
    let text = match key {
        "Enter" => "\r",
        " " => " ",
        _ => "",
    };
    for kind in ["keyDown", "keyUp"] {
        let mut event = json!({"type": kind, "key": key, "code": if key == " " { "Space" } else { key }, "windowsVirtualKeyCode": code});
        if kind == "keyDown" && !text.is_empty() {
            event["text"] = json!(text);
        }
        browser.send("Input.dispatchKeyEvent", event).unwrap();
    }
}
fn click(browser: &mut Chrome, selector: &str) {
    let at = browser
        .eval(&format!(
            "(e => {{ window.scrollBy({{top: e.getBoundingClientRect().top - innerHeight / 2, behavior: 'instant'}}); const r = e.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; }})(document.querySelector({}))",
            json!(selector)
        ))
        .unwrap();
    let (x, y) = (at[0].as_f64().unwrap(), at[1].as_f64().unwrap());
    for kind in ["mousePressed", "mouseReleased"] {
        browser
            .send(
                "Input.dispatchMouseEvent",
                json!({"type": kind, "x": x, "y": y, "button": "left", "clickCount": 1}),
            )
            .unwrap();
    }
}
const FRAMES: &str = "new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))";
/// The open step view of l1 as the page reads it: each row's step and state word, and whether
/// the row is selected.
const ROWS: &str = "[...document.querySelectorAll('#u-l1[open] li.sv-row')].map(r => [r.dataset.sv, r.querySelector('.st-w').textContent, r.classList.contains('is-sel')])";

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_the_drawers_strip_and_step_view_say_quiet_like_the_plan() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let id = n.almanac;
    // a-6's draft has run three hours and said nothing for as long: quiet
    f.writer.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute("UPDATE runs SET started_at=strftime('%Y-%m-%dT%H:%M:%SZ','now','-3 hours'),created_at=strftime('%Y-%m-%dT%H:%M:%SZ','now','-3 hours') WHERE project_id=?1 AND step_id='a-6-draft'", [id.to_string()])?;
        tx.sql().execute("UPDATE messages SET at=strftime('%Y-%m-%dT%H:%M:%SZ','now','-3 hours') WHERE project_id=?1 AND \"from\"='a-6-draft'", [id.to_string()])?;
        tx.changed(Some(id), "runs");
        Ok(())
    }).await.unwrap();
    let (addr, server) = serve(&f);
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}#step:a-6-draft")).unwrap();
        browser.wait("document.querySelector('#step-detail[data-step=\"a-6-draft\"] .sh-lane')").unwrap();
        assert_eq!(browser.eval("document.querySelector('#u-a-6 .pill.quiet') !== null").unwrap(), json!(true));
        assert_eq!(browser.eval("document.querySelector('#step-detail .sh-lane .sc[data-state=\"quiet\"] .sc-word')?.textContent").unwrap(), json!("quiet"));
        assert_eq!(browser.eval("document.querySelector('#step-detail li[data-sv=\"a-6-draft\"] .st-w')?.textContent").unwrap(), json!("quiet"));
    }).await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_a_row_opens_its_step_view_in_place_through_a_patch() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let (addr, server) = serve(&f);
    let writer = f.writer.clone();
    let rt = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let page = format!("http://{addr}/projects/id/{id}");
        let mut browser = Chrome::open(&page).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("customElements.get('sluice-steps') && document.querySelector('#u-l1 > summary')")
            .unwrap();
        // closed at first: the view is drawn but out of sight
        assert_eq!(
            browser.eval("[document.querySelector('#u-l1').open, document.querySelector('#u-l1 sluice-steps').checkVisibility()]").unwrap(),
            json!([false, false])
        );
        // Enter on the row opens it
        browser.eval("document.querySelector('#u-l1 > summary').focus()").unwrap();
        press(&mut browser, "Enter", 13);
        browser.wait("document.querySelector('#u-l1').open").unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(browser.eval("document.querySelector('#u-l1 sluice-steps').checkVisibility()").unwrap(), json!(true));
        // its rows: the unit's steps in its recipe's order, each with its state's word
        assert_eq!(
            browser.eval(ROWS).unwrap(),
            json!([["l1-fork", "succeeded", false], ["l1-work", "running", false], ["l1-land", "pending", false]])
        );
        // the view sits under the row
        let geometry = browser
            .eval("(() => { const row = document.querySelector('#u-l1 > summary').getBoundingClientRect(), view = document.querySelector('#u-l1 sluice-steps').getBoundingClientRect(); return [view.top >= row.bottom, view.left >= row.left - 0.5, view.right <= row.right + 0.5]; })()")
            .unwrap();
        assert_eq!(geometry, json!([true, true, true]));
        // the arrows move between its rows' links, the row in focus selected
        browser.eval("document.querySelector('#u-l1 li.sv-row a.sv-a').focus()").unwrap();
        press(&mut browser, "ArrowDown", 40);
        assert_eq!(
            browser.eval("[document.activeElement.closest('li.sv-row')?.dataset.sv, document.querySelector('#u-l1 sluice-steps').getAttribute('selected')]").unwrap(),
            json!(["l1-work", "l1-work"])
        );
        press(&mut browser, "End", 35);
        assert_eq!(browser.eval("document.activeElement.closest('li.sv-row')?.dataset.sv").unwrap(), json!("l1-land"));
        press(&mut browser, "Home", 36);
        assert_eq!(browser.eval("document.activeElement.closest('li.sv-row')?.dataset.sv").unwrap(), json!("l1-fork"));
        // a click on a step's row (not on its link) opens the step in the drawer, the row ringed
        click(&mut browser, "#u-l1 li.sv-row[data-sv='l1-work'] .sv-st");
        browser.wait("location.hash === '#step:l1-work' && !document.getElementById('drawer').hidden").unwrap();
        browser.wait("document.querySelector('#step-detail[data-step=\"l1-work\"] #d-title')").unwrap();
        assert_eq!(
            browser.eval("document.querySelector('#u-l1 li.sv-row.is-sel')?.dataset.sv").unwrap(),
            json!("l1-work")
        );
        press(&mut browser, "Escape", 27);
        browser.wait("location.hash === '' && document.getElementById('drawer').hidden").unwrap();
        // Space on another row opens it too
        browser.eval("window.other = [...document.querySelectorAll('details.ro-u')].find(d => d.id !== 'u-l1').id; document.querySelector(`#${other} > summary`).focus()").unwrap();
        press(&mut browser, " ", 32);
        browser.wait("document.getElementById(other).open").unwrap();
        // a stream patch: l1's work finishes, so l1 waits on its next step, drawn anew; its
        // view stays open and says the new state
        assert_eq!(browser.eval("document.querySelector('#u-l1').classList.contains('is-wait')").unwrap(), json!(false));
        rt.block_on(async {
            writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    tx.sql().execute(
                        "UPDATE steps SET status='succeeded' WHERE project_id=?1 AND step_id='l1-work'",
                        [id.to_string()],
                    )?;
                    tx.changed(Some(id), "steps");
                    Ok(())
                })
                .await
                .unwrap()
        });
        browser
            .wait("document.querySelector('#u-l1')?.classList.contains('is-wait')")
            .unwrap();
        browser.wait("document.querySelector('#u-l1').open").unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(
            browser.eval(ROWS).unwrap(),
            json!([["l1-fork", "succeeded", false], ["l1-work", "succeeded", false], ["l1-land", "pending", false]])
        );
        assert_eq!(browser.eval("document.getElementById(other).open").unwrap(), json!(true));

        // at every width and in the three themes: nothing scrolls sideways, nothing in a row
        // spills, every rail runs its row's height and the row is a whole target
        for theme in ["light", "dark", "interstate-light"] {
            for width in [390, 1440, 2560] {
                browser.viewport(width, theme).unwrap();
                browser.eval(FRAMES).unwrap();
                let g: Value = browser
                    .eval("(() => { const view = document.querySelector('#u-l1 sluice-steps'); const rows = [...view.querySelectorAll('li.sv-row')]; const out = view.getBoundingClientRect(); const sum = document.querySelector('#u-l1 > summary').getBoundingClientRect(); return {fits: document.documentElement.scrollWidth <= document.documentElement.clientWidth, inside: rows.every(r => { const b = r.getBoundingClientRect(); return b.left >= out.left - 0.5 && b.right <= out.right + 0.5; }), spills: rows.flatMap(r => [...r.querySelectorAll('.sv-name, .sv-st, .sv-pr, .sv-sp, .sv-tr')].filter(e => e.scrollWidth > e.clientWidth + 1).map(e => r.dataset.sv + ' ' + e.className)), rails: rows.every(r => { const a = r.getBoundingClientRect(), s = r.querySelector('.sv-rail > svg').getBoundingClientRect(); return Math.abs(a.top - s.top) < 1 && Math.abs(a.bottom - s.bottom) < 1; }), target: [Math.round(sum.width), Math.round(sum.height)], errors: window.browserErrors}; })()")
                    .unwrap();
                let label = format!("{width} {theme}");
                assert_eq!(g["fits"], true, "{label}: {g}");
                assert_eq!(g["inside"], true, "{label}: {g}");
                assert_eq!(g["spills"], json!([]), "{label}: {g}");
                assert_eq!(g["rails"], true, "{label}: {g}");
                let least = if width == 390 { 44 } else { 34 };
                assert!(g["target"][0].as_i64().unwrap() >= least && g["target"][1].as_i64().unwrap() >= least, "{label}: {g}");
                assert_eq!(g["errors"], json!([]), "{label}: {g}");
            }
        }
    })
    .await
    .unwrap();
    server.abort();
}
