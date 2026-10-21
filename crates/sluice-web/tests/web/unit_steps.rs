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
/// The page's pulse popover as it stands: open, still asking, its minute (its time's datetime)
/// and its words.
const POP: &str = "(() => { const p = document.getElementById('pulse-pop'); return {open: p.matches(':popover-open'), busy: p.getAttribute('aria-busy'), at: p.querySelector('time[data-clock]')?.getAttribute('datetime') ?? null, text: p.innerText, link: p.querySelector('.pp-more a')?.getAttribute('href') ?? null}; })()";
/// The pointer moved to `x`, `y`.
fn point(browser: &mut Chrome, x: f64, y: f64) {
    browser
        .send(
            "Input.dispatchMouseEvent",
            json!({"type": "mouseMoved", "x": x, "y": y}),
        )
        .unwrap();
}
/// Where a strip (`host`, a selector) is, and its element `mark` (a selector inside it, the
/// last that matches): the mark's middle, its minute (as the popover dates it), and the strip's
/// left edge and middle.
fn aim(browser: &mut Chrome, host: &str, mark: &str) -> (f64, f64, String, f64) {
    let at = browser
        .eval(&format!(
            "(() => {{ const h = document.querySelector({}); h.scrollIntoView({{block: 'center'}}); const b = h.getBoundingClientRect(); const m = [...h.querySelectorAll({})].at(-1); const r = m.getBoundingClientRect(); const minute = Math.floor(Number(m.dataset.a) / 60) * 60; return [r.left + r.width / 2, b.top + b.height / 2, new Date(minute * 1000).toISOString().replace('.000Z', 'Z'), b.left]; }})()",
            json!(host),
            json!(mark)
        ))
        .unwrap();
    (
        at[0].as_f64().unwrap(),
        at[1].as_f64().unwrap(),
        at[2].as_str().unwrap().to_owned(),
        at[3].as_f64().unwrap(),
    )
}
/// The popover once it has its answer.
fn answered(browser: &mut Chrome) -> Value {
    browser
        .wait("(() => { const p = document.getElementById('pulse-pop'); return p.matches(':popover-open') && !p.hasAttribute('aria-busy') && p.innerText.trim() !== ''; })()")
        .unwrap();
    browser.eval(POP).unwrap()
}

/// The bug this catches: a strip's marks say nothing of what happened, or the wrong minute's
/// (a pick answered by an earlier one, the minute misread from the pointer), or its details
/// leak into the page and its stream. Pointing at a row's tall mark shows that minute's message
/// on the reader's clock; the arrows move the popover to the minute before, Escape closes it;
/// a minute of calls says them with a link into its step's Activity, and the strip's faded left
/// edge says when its run began.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_a_strips_marks_say_what_its_agent_did_that_minute() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let id = n.almanac;
    // Isolate the command minutes: the dense showcase fixture can put adjacent marks
    // less than a pixel apart on the row's strip.
    neutral::lay_pulse(
        &f.writer,
        f._home.path(),
        id,
        "a-6-draft",
        &[15 * 60, 5 * 60, 2 * 60],
    )
    .await;
    let (addr, server) = serve(&f);
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}")).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser.wait("document.querySelector('#u-a-6 sluice-pulse[data-rocket-host]')").unwrap();
        // the page carries the strip's marks, not what they were
        assert_eq!(browser.eval("document.getElementById('pulse-pop-body').innerHTML").unwrap(), json!(""));
        // a-6's tall mark: the minute it said its entries were drafted
        let row = "#u-a-6 > summary sluice-pulse";
        let (x, y, minute, _) = aim(&mut browser, row, "i.pk-m");
        point(&mut browser, x + 1.0, y + 8.0);
        let pop = answered(&mut browser);
        assert_eq!(pop["at"], json!(minute), "{pop}");
        assert!(
            pop["text"].as_str().unwrap().contains("Entries 41 to 58 drafted; the maps moved under maps/coast/."),
            "{pop}"
        );
        // the strip is one stop: the arrow goes to the minute before, Escape closes it
        browser.eval(&format!("document.querySelector({}).focus()", json!(row))).unwrap();
        press(&mut browser, "ArrowLeft", 37);
        browser
            .wait(&format!("(() => {{ const p = document.getElementById('pulse-pop'); const t = p.querySelector('time[data-clock]'); return !p.hasAttribute('aria-busy') && t && t.getAttribute('datetime') < {}; }})()", json!(minute)))
            .unwrap();
        press(&mut browser, "Escape", 27);
        browser.wait("!document.getElementById('pulse-pop').matches(':popover-open')").unwrap();
        // pointing never opened the row
        assert_eq!(browser.eval("document.querySelector('#u-a-6').open").unwrap(), json!(false));
        // a minute of calls: what was run, and where its step's Activity has them
        let (x, y, minute, _) = aim(&mut browser, row, "i.pk:not(.pk-m):not(.pk-start)");
        point(&mut browser, x, y);
        let pop = answered(&mut browser);
        assert_eq!(pop["at"], json!(minute), "{pop}");
        assert!(pop["text"].as_str().unwrap().contains("rg -n 'range' sections/"), "{pop}");
        let link = pop["link"].as_str().unwrap();
        assert!(
            link.starts_with(&format!("/projects/id/{id}/steps/a-6-draft?activity=all&tab=activity#act-r1-t1-c")),
            "{pop}"
        );
        // its run began before the hour: its left edge fades and says when
        assert_eq!(
            browser.eval(&format!("document.querySelector({}).hasAttribute('data-before')", json!(row))).unwrap(),
            json!(true)
        );
        let (_, y, _, left) = aim(&mut browser, row, "i.pk");
        point(&mut browser, left + 3.0, y);
        browser.wait("document.getElementById('pulse-pop').innerText.startsWith('Started 1h')").unwrap();
        let pop = answered(&mut browser);
        assert!(pop["text"].as_str().unwrap().contains(", at "), "{pop}");
        assert!(pop["at"].is_string(), "{pop}");
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_the_drawers_strip_and_step_view_say_quiet_like_the_plan() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let id = n.almanac;
    // a-6's draft has run three hours and said or written nothing for as long: quiet
    let run: String = f.writer.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute("UPDATE runs SET started_at=strftime('%Y-%m-%dT%H:%M:%SZ','now','-3 hours'),created_at=strftime('%Y-%m-%dT%H:%M:%SZ','now','-3 hours') WHERE project_id=?1 AND step_id='a-6-draft'", [id.to_string()])?;
        tx.sql().execute("UPDATE messages SET at=strftime('%Y-%m-%dT%H:%M:%SZ','now','-3 hours') WHERE project_id=?1 AND \"from\"='a-6-draft'", [id.to_string()])?;
        tx.changed(Some(id), "runs");
        Ok(tx.sql().query_row("SELECT run_id FROM runs WHERE project_id=?1 AND step_id='a-6-draft' AND finished_at IS NULL", [id.to_string()], |r| r.get(0))?)
    }).await.unwrap();
    std::fs::File::open(f._home.path().join("runs").join(run))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 3600))
        .unwrap();
    let (addr, server) = serve(&f);
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}#step:a-6-draft")).unwrap();
        browser.wait("document.querySelector('#step-detail[data-step=\"a-6-draft\"] .sh-lane')").unwrap();
        // the row's track and the drawer's say the stage is quiet, in the same words
        assert_eq!(browser.eval("document.querySelector('#u-a-6 .track .current-stage[data-state=\"quiet\"]')?.textContent.startsWith('draft quiet ')").unwrap(), json!(true));
        assert_eq!(browser.eval("document.querySelector('#step-detail .sh-lane li.st[data-state=\"quiet\"] .st-w')?.textContent.startsWith('quiet ')").unwrap(), json!(true));
    }).await.unwrap();
    server.abort();
}

/// A row of At work opens in place to its conversation and its ways on (no step table), asked
/// for as it opens; it stays open through the stream's patches, and its new stamp asks again, so
/// its Open step follows the unit to its next step. The bug this catches: a patch that closes an
/// open row, or leaves its body saying a step it has moved past.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_a_row_opens_its_conversation_in_place_through_a_patch() {
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
            .wait("customElements.get('sluice-board') && document.querySelector('#u-l1 > summary')")
            .unwrap();
        // closed at first, its body not drawn: the page has only the rows
        assert_eq!(
            browser.eval("[document.querySelector('#u-l1').open, document.querySelector('#u-l1 .ro-foot')]").unwrap(),
            json!([false, null])
        );
        // Enter on the row opens it, asking for its body
        browser.eval("document.querySelector('#u-l1 > summary').focus()").unwrap();
        press(&mut browser, "Enter", 13);
        browser.wait("document.querySelector('#u-l1').open && document.querySelector('#u-l1 .ro-foot')").unwrap();
        browser.eval(FRAMES).unwrap();
        // no step table; its ways on, Open step at its current step
        assert_eq!(
            browser.eval("[!!document.querySelector('#u-l1 sluice-steps'), [...document.querySelectorAll('#u-l1 .ro-foot a')].map(a => a.textContent), document.querySelector('#u-l1 .ro-foot a.btn').dataset.step]").unwrap(),
            json!([false, ["Message", "Unit page", "Open step"], "l1-work"])
        );
        // the body sits under the row, on its edges
        let geometry = browser
            .eval("(() => { const row = document.querySelector('#u-l1 > summary').getBoundingClientRect(), body = document.querySelector('#u-l1 .ro-drill').getBoundingClientRect(); return [body.top >= row.bottom - 0.5, Math.abs(body.left - row.left) < 0.5, Math.abs(body.right - row.right) < 0.5]; })()")
            .unwrap();
        assert_eq!(geometry, json!([true, true, true]));
        // Open step opens the step in the drawer
        click(&mut browser, "#u-l1 .ro-foot a.btn");
        browser.wait("location.hash === '#step:l1-work' && !document.getElementById('drawer').hidden").unwrap();
        browser.wait("document.querySelector('#step-detail[data-step=\"l1-work\"] #d-title')").unwrap();
        press(&mut browser, "Escape", 27);
        browser.wait("location.hash === '' && document.getElementById('drawer').hidden").unwrap();
        // Space on another row opens it too
        browser.eval("window.other = [...document.querySelectorAll('details.ro-u')].find(d => d.id !== 'u-l1').id; document.querySelector(`#${other} > summary`).focus()").unwrap();
        press(&mut browser, " ", 32);
        browser.wait("document.getElementById(other).open && document.getElementById(other).querySelector('.ro-foot')").unwrap();
        // a stream patch: l1's work finishes, so l1 waits on its next step, drawn anew; it
        // stays open and, asked for again by the row's new stamp, leads to the new step
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
        browser
            .wait("document.querySelector('#u-l1 .ro-foot a.btn')?.dataset.step === 'l1-land'")
            .unwrap();
        assert_eq!(browser.eval("document.getElementById(other).open").unwrap(), json!(true));

        // at every width, in both stocks: nothing scrolls sideways, the body keeps its row's edges,
        // and the row is a whole target
        for theme in ["light", "dark"] {
            for width in [390, 1440, 2560] {
                browser.viewport(width, theme).unwrap();
                browser.eval(FRAMES).unwrap();
                let g: Value = browser
                    .eval("(() => { const body = document.querySelector('#u-l1 .ro-drill').getBoundingClientRect(); const sum = document.querySelector('#u-l1 > summary').getBoundingClientRect(); return {fits: document.documentElement.scrollWidth <= document.documentElement.clientWidth, inside: Math.abs(body.left - sum.left) < 0.5 && Math.abs(body.right - sum.right) < 0.5, spills: [...document.querySelectorAll('#u-l1 .ro-drill .msg, #u-l1 .ro-foot')].filter(e => e.scrollWidth > e.clientWidth + 1).length, target: [Math.round(sum.width), Math.round(sum.height)], errors: window.browserErrors}; })()")
                    .unwrap();
                let label = format!("{width} {theme}");
                assert_eq!(g["fits"], true, "{label}: {g}");
                assert_eq!(g["inside"], true, "{label}: {g}");
                assert_eq!(g["spills"], 0, "{label}: {g}");
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
