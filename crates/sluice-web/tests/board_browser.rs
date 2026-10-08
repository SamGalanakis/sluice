//! Real Chromium gate for the project's board: beside the plan from 1280px across the window (to
//! 2400px, the nav on the same edges) with a splitter between them and a Plan · Both · Board
//! switch, its own section behind the Plan · Board switch below that (each remembered per
//! project), nothing at all without a board; centred, unclipped and never scrolling sideways,
//! light and dark. `SLUICE_BOARD_SCREENS=<dir>` also saves a
//! screenshot of each state there.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::Value;

const GEOMETRY: &str = r#"(() => {
  const box = s => { const e = document.querySelector(s); if (!e || !e.checkVisibility()) return null;
                     const r = e.getBoundingClientRect(); return {left: r.left, right: r.right, width: r.width, top: r.top}; };
  const nav = document.querySelector('#top-nav'), n = nav.getBoundingClientRect(), css = getComputedStyle(nav);
  const page = document.querySelector('#project-board').getBoundingClientRect();
  return {scroll: document.documentElement.scrollWidth, width: document.documentElement.clientWidth,
          left: page.left, right: page.right,
          navLeft: n.left + parseFloat(css.paddingLeft), navRight: n.right - parseFloat(css.paddingRight),
          plan: document.querySelector('#plan-pane sluice-board')?.checkVisibility() ? box('#plan-pane') : null,
          board: box('#board-pane'), sum: box('#plan-pane .sumline'),
          navFits: (l => l.scrollWidth <= l.clientWidth)(nav.querySelector('.links')), tabs: box('.view-switch'), split: box('.splitter'),
          view: document.querySelector('#project-board').dataset.view ?? null,
          h1: box('#plan-pane .p-title'),
          // the page is as tall as its main column (or the window): nothing hangs below it
          below: document.documentElement.scrollHeight - Math.max(innerHeight,
            Math.ceil(document.querySelector('main').getBoundingClientRect().bottom + scrollY)),
          clipped: [...document.querySelectorAll('#board-pane *')].filter(e => {
            const r = e.getBoundingClientRect(); return r.width > 0 && r.right > document.documentElement.clientWidth + 0.5; }).length,
          errors: window.browserErrors};
})()"#;

fn near(a: &Value, b: f64) -> bool {
    (a.as_f64().unwrap() - b).abs() < 1.0
}
fn check(g: &Value, label: &str) {
    let (width, left, right) = (
        g["width"].as_f64().unwrap(),
        g["left"].as_f64().unwrap(),
        g["right"].as_f64().unwrap(),
    );
    assert!(
        g["scroll"].as_f64().unwrap() <= width,
        "{label}: sideways scroll {g}"
    );
    assert!(
        near(&g["navLeft"], left) && near(&g["navRight"], right),
        "{label}: nav edges {g}"
    );
    assert!(
        near(&g["left"], (width - (right - left)) / 2.0),
        "{label}: not centred {g}"
    );
    assert_eq!(g["clipped"], 0, "{label}: clipped {g}");
    assert!(
        g["below"].as_f64().unwrap() <= 1.0,
        "{label}: the page runs on past main {g}"
    );
    assert_eq!(
        g["navFits"], true,
        "{label}: a section hides in the nav {g}"
    );
    assert_eq!(g["errors"], serde_json::json!([]), "{label}: {g}");
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_board_beside_the_plan_and_behind_a_switch_on_a_phone() {
    let f = Fixture::new().await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    assert_ne!(addr.port(), 3065);
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (lanes, plain) = (f.id, f.plain);
    tokio::task::spawn_blocking(move || {
        let screens = std::env::var_os("SLUICE_BOARD_SCREENS").map(std::path::PathBuf::from);
        let base = format!("http://{addr}");
        let mut browser = Chrome::open(&format!("{base}/projects/id/{lanes}")).unwrap();
        let ready = "document.querySelector('#project-board') && document.readyState === 'complete' && (!document.querySelector('.has-panel') || document.querySelector('#project-board').dataset.view)";
        browser.wait(ready).unwrap();
        let listener_counts = |browser: &mut Chrome| {
            browser.send("Runtime.evaluate", serde_json::json!({
                "expression": "Object.fromEntries(['pointerover','pointerout','focusin','focusout','keydown','toggle'].map(t=>[t,(getEventListeners(document.querySelector('sluice-board'))[t]||[]).length]))",
                "includeCommandLineAPI": true,
                "returnByValue": true
            })).unwrap()["result"]["value"].clone()
        };
        let expected = listener_counts(&mut browser);
        assert!(expected.as_object().unwrap().values().all(|n| n == &serde_json::json!(1)), "{expected}");
        let scroll_count = |browser: &mut Chrome| {
            browser.send("Runtime.evaluate", serde_json::json!({
                "expression": "(getEventListeners(document.querySelector('#drawer')).scroll||[]).length",
                "includeCommandLineAPI": true,
                "returnByValue": true
            })).unwrap()["result"]["value"].clone()
        };
        let scroll = scroll_count(&mut browser);
        assert_eq!(scroll, 1);
        for _ in 0..3 {
            browser.eval("(()=>{window.controller=sluiceStream();const board=document.querySelector('sluice-board'),parent=board.parentNode,next=board.nextSibling;board.remove();parent.insertBefore(board,next);window.host=document.querySelector('sluice-drawer');window.drawerParent=host.parentNode;window.after=host.nextSibling;host.remove()})()").unwrap();
            assert_eq!(browser.eval("controller.signal.aborted").unwrap(), true);
            browser.eval("drawerParent.insertBefore(host,after)").unwrap();
            assert_eq!(listener_counts(&mut browser), expected);
            assert_eq!(scroll_count(&mut browser), scroll);
        }
        browser.eval("document.getElementById('n-alpha-build').focus()").unwrap();
        browser.send("Input.dispatchKeyEvent", serde_json::json!({"type":"keyDown","key":"ArrowDown","code":"ArrowDown"})).unwrap();
        assert_ne!(browser.eval("document.activeElement.id").unwrap(), "n-alpha-build");
        browser.eval("window.late=0;document.querySelector('#drawer').focus=()=>late++;history.replaceState(null,'','#step:beta-build');dispatchEvent(new HashChangeEvent('hashchange'));host.remove()").unwrap();
        browser.eval("new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))").unwrap();
        assert_eq!(browser.eval("late").unwrap(), 0);
        assert_eq!(browser.eval("document.documentElement.classList.contains('drawer-open')").unwrap(), false);
        browser.eval("drawerParent.insertBefore(host,after);history.replaceState(null,'',location.pathname)").unwrap();
        browser.navigate(&format!("{base}/projects/id/{lanes}")).unwrap();
        browser.wait(ready).unwrap();
        let shoot = |browser: &mut Chrome, name: &str| {
            if let Some(dir) = &screens {
                browser.screenshot(&dir.join(format!("{name}.png"))).unwrap();
            }
        };
        for (project, slug) in [(lanes, "board"), (plain, "no-board")] {
            browser
                .navigate(&format!("{base}/projects/id/{project}"))
                .unwrap();
            browser.wait(ready).unwrap();
            for width in [390, 1440, 2560] {
                for theme in ["light", "dark"] {
                    browser.viewport(width, theme).unwrap();
                    let g = browser.eval(GEOMETRY).unwrap();
                    let label = format!("{slug} {width} {theme}");
                    check(&g, &label);
                    let (vw, left, right) = (
                        g["width"].as_f64().unwrap(),
                        g["left"].as_f64().unwrap(),
                        g["right"].as_f64().unwrap(),
                    );
                    let plan = &g["plan"];
                    match (slug, width) {
                        // a phone opens on the board, under the summary line and its switch
                        ("board", 390) => {
                            assert!(g["tabs"].is_object() && g["board"].is_object() && plan.is_null(), "{label}: {g}");
                            assert_eq!(g["view"], "board", "{label}: {g}");
                            assert!(g["h1"].is_object(), "{label}: the project's title stays over its board {g}");
                            assert!(g["tabs"]["top"].as_f64().unwrap() > g["sum"]["top"].as_f64().unwrap(), "{label}: the switch after the summary {g}");
                        }
                        ("board", _) => {
                            assert!(plan.is_object(), "{label}: {g}");
                            // From 1280px the page takes the window (to 2400px): the plan, the
                            // splitter, the board; the switch offers Plan · Both · Board.
                            let board = &g["board"];
                            assert!(g["tabs"].is_object() && g["split"].is_object(), "{label}: {g}");
                            assert_eq!(g["view"], "both", "{label}: {g}");
                            assert!(
                                board["left"].as_f64().unwrap()
                                    >= plan["right"].as_f64().unwrap() + 24.0,
                                "{label}: side by side {g}"
                            );
                            assert!(near(&g["split"]["left"], plan["right"].as_f64().unwrap()), "{label}: {g}");
                            assert!(near(&board["right"], g["right"].as_f64().unwrap()), "{label}: {g}");
                            let page = right - left;
                            assert!((page - (vw - 64.0).min(2400.0)).abs() < 1.0, "{label}: {g}");
                            assert!(plan["width"].as_f64().unwrap() >= 560.0, "{label}: {g}");
                        }
                        _ => {
                            assert!(plan.is_object(), "{label}: {g}");
                            assert!(g["tabs"].is_null() && g["board"].is_null(), "{label}: {g}");
                            assert!(near(&plan["width"], g["right"].as_f64().unwrap() - g["left"].as_f64().unwrap()), "{label}: {g}");
                        }
                    }
                    shoot(&mut browser, &format!("project-{slug}-{width}-{theme}"));
                }
            }
        }
        // Across 1280px the page keeps its edges and the switch its place: at the summary
        // line's right end, on its row, as wide as its words.
        browser
            .navigate(&format!("{base}/projects/id/{lanes}"))
            .unwrap();
        browser.wait(ready).unwrap();
        for width in [1279, 1280] {
            browser.viewport(width, "light").unwrap();
            let g = browser.eval(GEOMETRY).unwrap();
            let label = format!("cliff {width}");
            check(&g, &label);
            assert!(near(&g["left"], 32.0) && near(&g["right"], g["width"].as_f64().unwrap() - 32.0), "{label}: {g}");
            let (tabs, sum, plan) = (&g["tabs"], &g["sum"], &g["plan"]);
            assert!(near(&tabs["right"], plan["right"].as_f64().unwrap()), "{label}: {g}");
            assert!(tabs["top"].as_f64().unwrap() >= sum["top"].as_f64().unwrap() - 1.0, "{label}: {g}");
        }
        // Every page's times are nav.js's, in one vocabulary: a running time is a two-unit
        // duration, never "… ago".
        let since = browser
            .eval("(() => { const t = document.createElement('time'); t.id = 't-since'; const at = new Date(Date.now() - 3600e3).toISOString(); t.dataset.since = at; t.setAttribute('datetime', at); document.querySelector('main').append(t); return new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => { document.dispatchEvent(new CustomEvent('datastar-signal-patch', {detail: {}})); r(t.textContent); }))); })()")
            .unwrap();
        assert_eq!(since, "1h 0m", "{since}");
        assert_eq!(browser.eval("document.querySelector('#t-since').textContent").unwrap(), "1h 0m");
        browser.eval("document.querySelector('#t-since').remove()").unwrap();
        // On a phone the switch shows one section at a time (the board until one is picked)
        // and remembers the choice.
        browser
            .navigate(&format!("{base}/projects/id/{lanes}"))
            .unwrap();
        browser.wait(ready).unwrap();
        browser.viewport(390, "light").unwrap();
        browser
            .eval("document.querySelector('[data-view-tab=plan]').click()")
            .unwrap();
        let g = browser.eval(GEOMETRY).unwrap();
        check(&g, "plan tab");
        assert!(g["plan"].is_object() && g["board"].is_null(), "{g}");
        assert_eq!(
            browser
                .eval("document.querySelector('[data-view-tab=plan]').getAttribute('aria-pressed')")
                .unwrap(),
            "true"
        );
        for theme in ["light", "dark"] {
            browser.viewport(390, theme).unwrap();
            shoot(&mut browser, &format!("project-plan-tab-390-{theme}"));
        }
        browser
            .navigate(&format!("{base}/projects/id/{lanes}"))
            .unwrap();
        browser.wait(ready).unwrap();
        browser.viewport(390, "light").unwrap();
        let g = browser.eval(GEOMETRY).unwrap();
        assert_eq!(g["view"], "plan", "remembered {g}");
        assert!(g["plan"].is_object() && g["board"].is_null(), "{g}");
        // an address that asks for a search, a Show or an Order opens on the plan, whatever
        // view was picked last
        browser
            .eval("document.querySelector('[data-view-tab=board]').click()")
            .unwrap();
        for query in ["q=alpha", "show=attention", "order=plan"] {
            browser
                .navigate(&format!("{base}/projects/id/{lanes}?{query}"))
                .unwrap();
            browser.wait(ready).unwrap();
            let g = browser.eval(GEOMETRY).unwrap();
            assert_eq!(g["view"], "plan", "{query}: {g}");
        }
        browser
            .navigate(&format!("{base}/projects/id/{lanes}"))
            .unwrap();
        browser.wait(ready).unwrap();
        assert_eq!(browser.eval(GEOMETRY).unwrap()["view"], "board", "the pick is kept");
        browser
            .eval("document.querySelector('[data-view-tab=plan]').click()")
            .unwrap();
        // The splitter: End and Home take the board to its bounds, the width is remembered,
        // a double-click forgets it; Board shows the board alone.
        browser.viewport(1440, "light").unwrap();
        let width = |browser: &mut Chrome| {
            browser
                .eval("Math.round(document.querySelector('#board-pane').getBoundingClientRect().width)")
                .unwrap()
                .as_f64()
                .unwrap()
        };
        let default = width(&mut browser);
        let key = |key: &str| format!("document.querySelector('.splitter').dispatchEvent(new KeyboardEvent('keydown', {{key: '{key}', bubbles: true}}))");
        browser.eval(&key("End")).unwrap();
        let max = browser
            .eval("Number(document.querySelector('.splitter').getAttribute('aria-valuemax'))")
            .unwrap();
        assert!(near(&max, width(&mut browser)) && max.as_f64().unwrap() > default, "{max} {default}");
        browser.eval(&key("Home")).unwrap();
        assert_eq!(width(&mut browser), 320.0);
        browser.eval(&key("ArrowLeft")).unwrap();
        assert_eq!(width(&mut browser), 336.0);
        browser
            .navigate(&format!("{base}/projects/id/{lanes}"))
            .unwrap();
        browser.wait(ready).unwrap();
        assert_eq!(width(&mut browser), 336.0, "remembered");
        let g = browser.eval(GEOMETRY).unwrap();
        check(&g, "narrow board");
        browser
            .eval("document.querySelector('.splitter').dispatchEvent(new MouseEvent('dblclick', {bubbles: true}))")
            .unwrap();
        assert_eq!(width(&mut browser), default, "reset");
        browser
            .eval("document.querySelector('[data-view-tab=board]').click()")
            .unwrap();
        let g = browser.eval(GEOMETRY).unwrap();
        check(&g, "board alone");
        assert!(g["plan"].is_null() && near(&g["board"]["width"], g["right"].as_f64().unwrap() - g["left"].as_f64().unwrap()), "{g}");
        browser
            .eval("document.querySelector('[data-view-tab=both]').click()")
            .unwrap();
        // A button press on the page sends the say and says so under the board.
        browser
            .eval("document.querySelector('#board-pane input[name=\"field-0\"]').value='gamma'; document.querySelector('#board-pane input[name=\"field-0\"]').closest('form').querySelector('[name=\"field-1\"]').value='go now'; document.querySelector('#board-pane button[name=button][value=\"0\"]').click()")
            .unwrap();
        browser
            .wait("document.querySelector('#board-pane .board-status').textContent === 'Sent to the orchestrator.'")
            .unwrap();
        // The settings page's Board section, with its live preview.
        browser
            .navigate(&format!("{base}/projects/id/{lanes}/settings"))
            .unwrap();
        browser
            .wait("document.querySelector('#board-preview') && !document.querySelector('#board-preview').hidden && document.querySelector('#board-preview .metric-v')")
            .unwrap();
        for width in [390, 1440, 2560] {
            for theme in ["light", "dark"] {
                browser.viewport(width, theme).unwrap();
                let g = browser
                    .eval("(() => { const r = document.querySelector('#board-settings').getBoundingClientRect(); return {scroll: document.documentElement.scrollWidth, width: document.documentElement.clientWidth, left: r.left, right: r.right, errors: window.browserErrors}; })()")
                    .unwrap();
                assert!(g["scroll"].as_f64().unwrap() <= g["width"].as_f64().unwrap(), "settings {width}: {g}");
                assert_eq!(g["errors"], serde_json::json!([]), "{g}");
                browser
                    .eval("document.querySelector('#board-settings').scrollIntoView()")
                    .unwrap();
                shoot(&mut browser, &format!("settings-{width}-{theme}"));
            }
        }
    })
    .await
    .unwrap();
    assert!(!f.commands.0.lock().unwrap().is_empty());
    server.abort();
    let _ = server.await;
}

/// Each drawn line: its two ends, whether it leaves its source's foot and its arrowhead meets
/// its dependent, and whether it runs down the page (its head below its source's foot).
const EDGES: &str = r#"(() => {
  const plane = document.querySelector('.plane').getBoundingClientRect();
  return [...document.querySelectorAll('svg.edges .wires path:not(.head)')].map(p => {
    const a = document.querySelector(`[data-node="${p.dataset.from}"]`).getBoundingClientRect();
    const z = document.querySelector(`[data-node="${p.dataset.to}"]`).getBoundingClientRect();
    const head = p.nextElementSibling.getBoundingClientRect();
    const n = p.getAttribute('d').match(/-?[\d.]+/g).map(Number);
    const [x1, y1] = [n[0], n[1]];
    const meets = (h, r, d) => h.right > r.left - d && h.left < r.right + d && h.bottom > r.top - d && h.top < r.bottom + d;
    return {from: p.dataset.from, to: p.dataset.to, down: head.top > a.bottom,
            lands: x1 >= a.left - plane.left - 1 && x1 <= a.right - plane.left + 1
              && Math.abs(y1 - (a.bottom - plane.top)) < 1.5 && meets(head, z, 1.5)};
  });
})()"#;
/// Each unit laid out on the board: its id and box.
const TILES: &str = r#"Object.fromEntries([...document.querySelectorAll('.plane .layer > .box')].map(b => {
  const r = b.getBoundingClientRect(); return [b.id, {left: r.left, top: r.top, bottom: r.bottom, width: r.width}]; }))"#;

/// The plan as a graph keyed off nothing but the plan: under Live first the running units side
/// by side in one layer, the waiting in layers under them by depth, and a line for each wait
/// between units, leaving its source's foot and landing on the card that waits, running down
/// the page; a satisfied wait has no line. That holds after a resize. On a phone the units
/// stack, no line is drawn, and a card says its waits in words. Nothing scrolls sideways.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_plan_graph_draws_each_wait_between_units_as_a_line_down_to_its_card() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "graph",
            serde_json::json!({"steps":{
                "d1":{"run":"custom.open","tags":["unit:d1"]},
                "r1":{"run":"custom.open","after":["d1"],"tags":["unit:r1"]},
                "r2":{"run":"custom.open","tags":["unit:r2"]},
                "w1":{"run":"custom.open","after":["r1"],"tags":["unit:w1"]},
                "w2":{"run":"custom.open","after":["r1","r2"],"tags":["unit:w2"]},
                "w3":{"run":"custom.open","after":["w1","w2","r1"],"tags":["unit:w3"]}}}),
            &[("d1", "succeeded"), ("r1", "running"), ("r2", "running")],
        )
        .await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}")).unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelector('sluice-board')")
            .unwrap();
        let want: Vec<(String, String)> = [("r1", "w1"), ("r1", "w2"), ("r2", "w2"), ("w1", "w3"), ("w2", "w3")]
            .iter()
            .map(|(a, b)| (format!("s:{a}"), format!("s:{b}")))
            .collect();
        for width in [1440, 1300, 2560, 1440] {
            browser.viewport(width, "light").unwrap();
            browser
                .wait("document.querySelectorAll('svg.edges .wires path:not(.head)').length === 5")
                .unwrap();
            let edges = browser.eval(EDGES).unwrap();
            let mut drawn: Vec<(String, String)> = edges
                .as_array()
                .unwrap()
                .iter()
                .map(|e| (e["from"].as_str().unwrap().into(), e["to"].as_str().unwrap().into()))
                .collect();
            drawn.sort();
            assert_eq!(drawn, want, "{width}: r1→w3 is implied, d1→r1 is satisfied");
            assert!(
                edges.as_array().unwrap().iter().all(|e| e["lands"] == true && e["down"] == true),
                "{width}: {edges}"
            );
            let t = browser.eval(TILES).unwrap();
            let (top, bottom) = (|u: &str| t[u]["top"].as_f64().unwrap(), |u: &str| t[u]["bottom"].as_f64().unwrap());
            assert!(near(&t["unit-r1"]["top"], top("unit-r2")), "{width}: one layer {t}");
            assert!(near(&t["unit-w1"]["top"], top("unit-w2")), "{width}: one layer {t}");
            assert!(top("unit-w1") > bottom("unit-r1") + 30.0, "{width}: waiting under running {t}");
            assert!(top("unit-w3") > bottom("unit-w2") + 30.0, "{width}: by depth {t}");
            let scroll = browser
                .eval("document.documentElement.scrollWidth - document.documentElement.clientWidth")
                .unwrap();
            assert_eq!(scroll, 0, "{width}: sideways scroll");
        }
        browser.viewport(390, "light").unwrap();
        let t = browser.eval(TILES).unwrap();
        assert!(t["unit-w1"]["top"].as_f64().unwrap() >= t["unit-r2"]["bottom"].as_f64().unwrap(), "phone: stacked {t}");
        assert_eq!(browser.eval("document.querySelectorAll('svg.edges path').length").unwrap(), 0);
        let words = browser
            .eval("(w => w && w.checkVisibility() ? w.textContent : null)(document.querySelector('#unit-w2 .waits'))")
            .unwrap();
        assert_eq!(words, "Waits for r1 (running) and r2 (running)");
        assert_eq!(browser.eval("window.browserErrors").unwrap(), serde_json::json!([]));
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}

const CENTRE: &str = r#"(sel => { const e = document.querySelector(sel); e.scrollIntoView({block: 'center'});
  const r = e.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; })"#;

fn pointer(browser: &mut Chrome, kind: &str, x: f64, y: f64) {
    let mut event = serde_json::json!({"type": kind, "x": x, "y": y});
    if kind != "mouseMoved" {
        event["button"] = "left".into();
        event["clickCount"] = 1.into();
    }
    browser.send("Input.dispatchMouseEvent", event).unwrap();
}
/// Waits two frames and for every transition to run out.
fn settle(browser: &mut Chrome) {
    browser
        .wait("new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r(true))))")
        .unwrap();
    browser
        .wait("document.getAnimations().length === 0")
        .unwrap();
}
/// Moves the pointer onto the element `selector` names (scrolled into view), in a few steps.
fn hover(browser: &mut Chrome, selector: &str) -> (f64, f64) {
    let at = browser
        .eval(&format!("{CENTRE}({})", serde_json::json!(selector)))
        .unwrap();
    let (x, y) = (at[0].as_f64().unwrap(), at[1].as_f64().unwrap());
    for step in [8.0, 4.0, 0.0] {
        pointer(browser, "mouseMoved", x - step, y - step);
    }
    (x, y)
}

/// The drawer gives the focus back to the card it closes on: that card wears the keyboard's
/// ring in ink on the pill itself, never running's blue or the open step's ring, and once the
/// pointer has passed over a card and left the board nothing stays traced.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_the_card_the_drawer_gives_focus_back_wears_an_ink_ring_and_traces_nothing() {
    let f = Fixture::new().await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let lanes = f.id;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{lanes}")).unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelector('sluice-board')")
            .unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("document.querySelectorAll('svg.edges .wires path:not(.head)').length === 3")
            .unwrap();
        let edges = browser.eval(EDGES).unwrap();
        assert!(edges.as_array().unwrap().iter().all(|e| e["lands"] == true), "{edges}");
        let (x, y) = hover(&mut browser, "#n-alpha-review");
        pointer(&mut browser, "mousePressed", x, y);
        pointer(&mut browser, "mouseReleased", x, y);
        browser.wait("location.hash === '#step:alpha-review'").unwrap();
        browser
            .send(
                "Input.dispatchKeyEvent",
                serde_json::json!({"type": "keyDown", "key": "Escape", "code": "Escape",
                                   "windowsVirtualKeyCode": 27}),
            )
            .unwrap();
        browser
            .wait("location.hash === '' && document.getElementById('drawer').hidden && document.activeElement?.id === 'n-alpha-review'")
            .unwrap();
        settle(&mut browser);
        let ring = browser
            .eval("(c => { const s = getComputedStyle(c); return [c.matches(':focus-visible'), s.outlineColor === getComputedStyle(document.body).color, s.outlineOffset, s.boxShadow]; })(document.activeElement)")
            .unwrap();
        assert_eq!(ring, serde_json::json!([true, true, "0px", "none"]), "an ink ring, not the open ring");
        hover(&mut browser, "#n-beta-build");
        browser
            .wait("document.querySelector('.plane').classList.contains('tracing')")
            .unwrap();
        for step in [40.0, 20.0, 4.0] {
            pointer(&mut browser, "mouseMoved", step, step);
        }
        settle(&mut browser);
        let lit = browser
            .eval("[document.querySelector('.plane').classList.contains('tracing'), document.querySelectorAll('.plane .on').length]")
            .unwrap();
        assert_eq!(lit, serde_json::json!([false, 0]), "a trace left on");
        assert_eq!(browser.eval("window.browserErrors").unwrap(), serde_json::json!([]));
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}

/// One path a pair of cards, named with every relation between them (a handoff and an after
/// on the same pair are one line). A line whose order a longer path already gives is not
/// drawn, a value passed along it too: the step's page lists its inputs. Every line is drawn
/// alike, its kind in its name; plain order has none but "after" in its title. Arrowheads
/// land on their cards.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_one_path_a_pair_and_no_after_that_a_path_already_gives() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "edges",
            serde_json::json!({"steps":{
                "lane-fork":{"run":"custom.open","outputs":{"path":"string"},"tags":["unit:lane"]},
                "lane-work":{"run":"custom.open","in":{"cwd":{"source":"lane-fork/path"}},"after":["lane-fork"],"tags":["unit:lane"]},
                "lane-land":{"run":"custom.open","in":{"fork":{"source":"lane-fork/path"}},"after":["lane-work"],"tags":["unit:lane"]},
                "lane-rm":{"run":"custom.open","after":["lane-fork","lane-land"],"tags":["unit:lane"]}}}),
            &[("lane-fork", "succeeded")],
        )
        .await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}")).unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelector('sluice-board')")
            .unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("document.querySelectorAll('svg.edges .wires path:not(.head)').length > 0")
            .unwrap();
        let paths = browser
            .eval("[...document.querySelectorAll('svg.edges .wires path:not(.head)')].map(p => [p.dataset.from, p.dataset.to, p.dataset.kind, p.querySelector('title')?.textContent ?? ''])")
            .unwrap();
        let mut paths: Vec<Vec<String>> = serde_json::from_value(paths).unwrap();
        paths.sort();
        let want: Vec<Vec<String>> = [
            ["s:lane-fork", "s:lane-work", "handoff", "path → cwd"],
            ["s:lane-land", "s:lane-rm", "ordering", "after"],
            ["s:lane-work", "s:lane-land", "ordering", "after"],
        ]
        .iter()
        .map(|p| p.iter().map(|s| s.to_string()).collect())
        .collect();
        assert_eq!(paths, want, "fork→land and fork→rm are implied by fork→work→land→rm");
        let dash = browser
            .eval("[...document.querySelectorAll('svg.edges .wires path:not(.head)')].map(p => getComputedStyle(p).strokeDasharray)")
            .unwrap();
        let dash: Vec<String> = serde_json::from_value(dash).unwrap();
        assert!(dash.iter().all(|d| d == "none"), "every line alike: {dash:?}");
        let edges = browser.eval(EDGES).unwrap();
        assert!(
            edges.as_array().unwrap().iter().all(|e| e["lands"] == true),
            "{edges}"
        );
        assert_eq!(browser.eval("window.browserErrors").unwrap(), serde_json::json!([]));
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}

/// A lane matrix's rows are joined by their lines through its gutter: a line leaves its row at
/// the row's left edge and enters a row at its left edge, its arrowhead pointing into the row;
/// a line from a row to a unit outside the matrix leaves the gutter at the matrix's foot and
/// lands on the card that waits. Inside a row no line is drawn: the columns say the order. On a
/// phone a row is a block, its stages a lane string, and there are no lines.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_lines_reach_lane_matrix_rows_through_its_gutter() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}")).unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelector('sluice-board')")
            .unwrap();
        const ENDS: &str = r#"(() => {
  const r = (e) => e.getBoundingClientRect();
  const plane = r(document.querySelector('.plane'));
  const line = (from, to) => {
    const p = document.querySelector(`svg.edges .wires path:not(.head)[data-from="${from}"][data-to="${to}"]`);
    const h = document.querySelector(`svg.edges .wires path.head[data-from="${from}"][data-to="${to}"]`);
    if (!p || !h) return null;
    const a = p.getPointAtLength(0), z = p.getPointAtLength(p.getTotalLength()), hb = h.getBBox();
    return {x1: a.x + plane.left, y1: a.y + plane.top, x2: z.x + plane.left, y2: z.y + plane.top,
            head: {left: hb.x + plane.left, right: hb.x + hb.width + plane.left, top: hb.y + plane.top,
                   bottom: hb.y + hb.height + plane.top}};
  };
  const row = (u) => { const b = r(document.getElementById('unit-' + u)); return {left: b.left, top: b.top, bottom: b.bottom}; };
  const card = (s) => { const b = r(document.getElementById('n-' + s)); return {left: b.left, right: b.right, top: b.top}; };
  return {rows: line('s:l2-land', 's:l3-fork'), out: line('s:l1-land', 's:report'),
          inside: document.querySelectorAll('svg.edges path[data-from="s:l1-fork"][data-to="s:l1-work"]').length,
          l2: row('l2'), l3: row('l3'), l1: row('l1'), report: card('report'),
          wrap: r(document.querySelector('[data-matrix="lane"] .mx-wrap')).bottom,
          over: (() => { const p = document.querySelector('svg.edges .wires path:not(.head)[data-to="s:report"]');
                         const rows = r(document.querySelector('[data-matrix="rough"] tbody'));
                         let n = 0; for (let l = 0; l <= p.getTotalLength(); l += 4) { const q = p.getPointAtLength(l);
                           const x = q.x + plane.left, y = q.y + plane.top;
                           if (x > rows.left && x < rows.right && y > rows.top && y < rows.bottom) n++; } return n; })()};
})()"#;
        for width in [1440, 2560] {
            browser.viewport(width, "light").unwrap();
            browser
                .wait("document.querySelector('svg.edges .wires path[data-from=\"s:l2-land\"]') && document.querySelector('svg.edges .wires path[data-to=\"s:report\"]')")
                .unwrap();
            let g = browser.eval(ENDS).unwrap();
            let n = |v: &Value| v.as_f64().unwrap();
            let (rows, out) = (&g["rows"], &g["out"]);
            // between two rows: from l2's left edge, in its height, into l3's left edge
            assert!((n(&rows["x1"]) - n(&g["l2"]["left"])).abs() < 1.5, "{width}: {g}");
            assert!(n(&rows["y1"]) > n(&g["l2"]["top"]) && n(&rows["y1"]) < n(&g["l2"]["bottom"]), "{width}: {g}");
            assert!(n(&rows["y2"]) > n(&g["l3"]["top"]) && n(&rows["y2"]) < n(&g["l3"]["bottom"]), "{width}: {g}");
            assert!((n(&rows["head"]["right"]) - n(&g["l3"]["left"])).abs() < 1.5, "{width}: the head points into the row {g}");
            // in the gutter: left of the rows
            assert!(n(&rows["x2"]) < n(&g["l3"]["left"]), "{width}: {g}");
            // out of the matrix: from l1's row, past the matrix's foot, onto report's card
            assert!((n(&out["x1"]) - n(&g["l1"]["left"])).abs() < 1.5, "{width}: {g}");
            assert!(n(&out["y1"]) > n(&g["l1"]["top"]) && n(&out["y1"]) < n(&g["l1"]["bottom"]), "{width}: {g}");
            let tip = n(&out["head"]["bottom"]);
            assert!((tip - n(&g["report"]["top"])).abs() < 2.0, "{width}: lands on report {g}");
            assert!(n(&out["head"]["left"]) >= n(&g["report"]["left"]) && n(&out["head"]["right"]) <= n(&g["report"]["right"]), "{width}: {g}");
            assert!(tip > n(&g["wrap"]), "{width}: below the matrix {g}");
            // past the other matrix it runs down that one's gutter, never over its rows
            assert_eq!(g["over"], 0, "{width}: a line over a matrix's rows {g}");
            assert_eq!(g["inside"], 0, "{width}: no line inside a row");
            if let Some(dir) = std::env::var_os("SLUICE_BOARD_SCREENS") {
                let dir = std::path::PathBuf::from(dir);
                std::fs::create_dir_all(&dir).unwrap();
                browser.screenshot(&dir.join(format!("matrix-{width}.png"))).unwrap();
            }
            let scroll = browser
                .eval("document.documentElement.scrollWidth - document.documentElement.clientWidth")
                .unwrap();
            assert_eq!(scroll, 0, "{width}: sideways scroll");
        }
        browser.viewport(390, "light").unwrap();
        browser.wait("document.querySelectorAll('svg.edges path').length === 0").unwrap();
        let phone = browser
            .eval(r#"(() => { const row = document.getElementById('unit-l1');
              return {lane: row.querySelector('.mx-lane').checkVisibility() ? row.querySelector('.mx-lane').textContent : null,
                      cells: [...row.querySelectorAll('td')].filter(t => t.checkVisibility()).length,
                      scroll: document.documentElement.scrollWidth - document.documentElement.clientWidth}; })()"#)
            .unwrap();
        assert_eq!(phone["lane"], "fork✓ work▶ land·", "{phone}");
        assert_eq!(phone["cells"], 0, "{phone}");
        assert_eq!(phone["scroll"], 0, "{phone}");
        assert_eq!(browser.eval("window.browserErrors").unwrap(), serde_json::json!([]));
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}
