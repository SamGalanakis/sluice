//! Real Chromium gate for the project page: the board beside the plan from 1280px with a
//! splitter between them and a Plan · Both · Board switch, its own section behind the Plan ·
//! Board switch below that (each remembered per project), nothing at all without a board;
//! centred, unclipped and never scrolling sideways, light and dark. Select-to-trace on the
//! plan, the drawer giving the focus back, and a step's new state said to a screen reader.
//! `SLUICE_BOARD_SCREENS=<dir>` also saves a screenshot of each state there.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::Value;
use sluice_store::RetrySafety;

const GEOMETRY: &str = r#"(() => {
  const box = s => { const e = document.querySelector(s); if (!e || !e.checkVisibility()) return null;
                     const r = e.getBoundingClientRect(); return {left: r.left, right: r.right, width: r.width, top: r.top}; };
  const nav = document.querySelector('#top-nav'), n = nav.getBoundingClientRect(), css = getComputedStyle(nav);
  const page = document.querySelector('#project-board').getBoundingClientRect();
  return {scroll: document.documentElement.scrollWidth, width: document.documentElement.clientWidth,
          left: page.left, right: page.right,
          navLeft: n.left + parseFloat(css.paddingLeft), navRight: n.right - parseFloat(css.paddingRight),
          plan: box('#plan-pane'),
          board: box('#board-pane'), sum: box('.band-summary'),
          navFits: (l => l.scrollWidth <= l.clientWidth)(document.querySelector('.subnav nav.links')), tabs: box('.view-switch'), split: box('.splitter'),
          view: document.querySelector('#project-board').dataset.view ?? null,
          h1: box('.site-head h1'),
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
                "expression": "Object.fromEntries(['click','keydown'].map(t=>[t,(getEventListeners(document.querySelector('sluice-trace'))[t]||[]).length]))",
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
            browser.eval("(()=>{window.controller=sluiceStream();const board=document.querySelector('sluice-trace'),parent=board.parentNode,next=board.nextSibling;board.remove();parent.insertBefore(board,next);window.host=document.querySelector('sluice-drawer');window.drawerParent=host.parentNode;window.after=host.nextSibling;host.remove()})()").unwrap();
            assert_eq!(browser.eval("controller.signal.aborted").unwrap(), true);
            browser.eval("drawerParent.insertBefore(host,after)").unwrap();
            assert_eq!(listener_counts(&mut browser), expected);
            assert_eq!(scroll_count(&mut browser), scroll);
        }
        // the arrows move between the units' trace buttons
        browser.eval("window.first=document.querySelector('[data-trace-pick]');first.focus()").unwrap();
        browser.send("Input.dispatchKeyEvent", serde_json::json!({"type":"keyDown","key":"ArrowDown","code":"ArrowDown"})).unwrap();
        assert_eq!(browser.eval("document.activeElement !== first && document.activeElement.matches('[data-trace-pick]')").unwrap(), true);
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
                        // a phone opens on the plan while something needs attention (beta-build
                        // failed), the switch under the summary line
                        ("board", 390) => {
                            assert!(g["tabs"].is_object() && g["board"].is_null() && plan.is_object(), "{label}: {g}");
                            assert_eq!(g["view"], "plan", "{label}: {g}");
                            assert!(g["h1"].is_object(), "{label}: the project's title stays over its board {g}");
                            assert!(g["tabs"]["top"].as_f64().unwrap() > g["sum"]["top"].as_f64().unwrap(), "{label}: the switch after the summary {g}");
                            assert!(g["board"].is_null(), "{label}: {g}");
                        }
                        ("board", _) => {
                            assert!(plan.is_object(), "{label}: {g}");
                            // From 1280px the page takes the frame's column: the plan, the
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
                            // the frame's fluid column, the band's content on the same edges
                            let column = g["navRight"].as_f64().unwrap() - g["navLeft"].as_f64().unwrap();
                            assert!((page - column).abs() < 1.0 && page > vw - 140.0, "{label}: {g}");
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
        // Across 1280px the page keeps its edges and the switch its place, in the tools' row
        // under the summary.
        browser
            .navigate(&format!("{base}/projects/id/{lanes}"))
            .unwrap();
        browser.wait(ready).unwrap();
        for width in [1279, 1280] {
            browser.viewport(width, "light").unwrap();
            let g = browser.eval(GEOMETRY).unwrap();
            let label = format!("cliff {width}");
            check(&g, &label);
            // the frame's fluid gutter each side, the band's edges
            let gutter = (10.0 + 0.015 * width as f64).clamp(16.0, 64.0);
            assert!(near(&g["left"], gutter) && near(&g["right"], g["width"].as_f64().unwrap() - gutter), "{label}: {g}");
            assert!(near(&g["left"], g["navLeft"].as_f64().unwrap()) && near(&g["right"], g["navRight"].as_f64().unwrap()), "{label}: {g}");
            let (tabs, sum) = (&g["tabs"], &g["sum"]);
            assert!(tabs["top"].as_f64().unwrap() > sum["top"].as_f64().unwrap(), "{label}: {g}");
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

/// Each unit's trace role on the plan, the host's selection and its sentence.
const TRACED: &str = r#"(() => { const host = document.querySelector('sluice-trace');
  return {selected: host.getAttribute('selected'), tracing: host.hasAttribute('tracing'),
          words: host.querySelector('.trace-words').textContent,
          roles: Object.fromEntries([...host.querySelectorAll('[data-unit]')].map(u => [u.dataset.unit, u.dataset.trace ?? ''])),
          faded: [...host.querySelectorAll('[data-trace=faded]')].filter(u => Number(getComputedStyle(u).opacity) < 1).length,
          more: [...host.querySelectorAll('[data-trace-more]')].filter(m => !m.hidden).length,
          errors: window.browserErrors}; })()"#;

/// Select to trace on the plan: a unit's button selects it, every unit up its chain and down
/// it is marked, the rest fade, the sentence says the chain and the selected unit's more opens
/// in place. The arrows move between the buttons, Enter selects, Escape clears and gives the
/// focus back. A stream patch keeps the choice and draws it again. Nothing scrolls sideways.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_select_to_trace_marks_the_chain_by_keyboard_and_through_a_patch() {
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
    let mut browser = tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}")).unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelector('sluice-trace [data-trace-pick]')")
            .unwrap();
        for width in [390, 1440, 2560] {
            browser.viewport(width, "light").unwrap();
            let g = browser.eval(TRACED).unwrap();
            assert_eq!(g["tracing"], false, "{width}: {g}");
            assert_eq!(
                browser.eval("document.documentElement.scrollWidth - document.documentElement.clientWidth").unwrap(),
                0,
                "{width}: sideways scroll"
            );
        }
        // by keyboard: w2's button, Enter
        browser.eval("document.querySelector('[data-unit=w2] [data-trace-pick]').focus()").unwrap();
        let key = |browser: &mut Chrome, key: &str, code: u32| {
            let mut down = serde_json::json!({"type": "keyDown", "key": key, "code": key, "windowsVirtualKeyCode": code});
            if key == "Enter" {
                down["text"] = "\r".into();
            }
            browser.send("Input.dispatchKeyEvent", down).unwrap();
            browser
                .send("Input.dispatchKeyEvent", serde_json::json!({"type": "keyUp", "key": key, "code": key, "windowsVirtualKeyCode": code}))
                .unwrap();
        };
        key(&mut browser, "Enter", 13);
        browser.wait("document.querySelector('sluice-trace').getAttribute('selected') === 'w2'").unwrap();
        settle(&mut browser);
        let g = browser.eval(TRACED).unwrap();
        assert_eq!(
            g["roles"],
            serde_json::json!({"r1": "up", "r2": "up", "w1": "faded", "w2": "selected", "w3": "down"}),
            "{g}"
        );
        assert_eq!(g["words"], "Tracing w2. It waits for r1 and r2. w3 waits on it.", "{g}");
        assert_eq!(g["faded"], 1, "the rest fades {g}");
        assert_eq!(g["more"], 1, "{g}");
        assert_eq!(
            browser.eval("document.querySelector('[data-unit=w2] [data-trace-pick]').getAttribute('aria-pressed')").unwrap(),
            "true"
        );
        // the arrows move between the buttons
        key(&mut browser, "ArrowDown", 40);
        assert_eq!(
            browser.eval("document.activeElement.closest('[data-unit]').dataset.unit").unwrap(),
            "w3"
        );
        key(&mut browser, "ArrowUp", 38);
        // let the page's stream open before the change
        std::thread::sleep(std::time::Duration::from_millis(1500));
        browser
    })
    .await
    .unwrap();
    // r1 succeeds: the patch moves it to Done and w1 to its turn; the choice stays
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='succeeded' WHERE project_id=?1 AND step_id='r1'",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    tokio::task::spawn_blocking(move || {
        browser
            .wait("!document.querySelector('[data-unit=r1] [data-trace-pick]')")
            .unwrap();
        settle(&mut browser);
        let g = browser.eval(TRACED).unwrap();
        assert_eq!(g["selected"], "w2", "{g}");
        assert_eq!(g["roles"]["w2"], "selected", "{g}");
        assert_eq!(g["roles"]["r2"], "up", "{g}");
        assert_eq!(g["roles"]["w3"], "down", "{g}");
        assert_eq!(g["more"], 1, "{g}");
        // Escape clears and leaves the focus on the selected unit's button
        browser.eval("document.querySelector('[data-unit=w2] [data-trace-pick]').focus()").unwrap();
        browser
            .send("Input.dispatchKeyEvent", serde_json::json!({"type": "keyDown", "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27}))
            .unwrap();
        settle(&mut browser);
        let g = browser.eval(TRACED).unwrap();
        assert_eq!(g["tracing"], false, "{g}");
        assert_eq!(g["more"], 0, "{g}");
        assert_eq!(
            browser.eval("document.activeElement.closest('[data-unit]')?.dataset.unit").unwrap(),
            "w2"
        );
        assert_eq!(g["errors"], serde_json::json!([]), "{g}");
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
/// Waits two frames and for every transition to run out (a running cell's sweep never does).
fn settle(browser: &mut Chrome) {
    browser
        .wait("new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r(true))))")
        .unwrap();
    browser
        .wait("document.getAnimations().every(a => a.playState !== 'running' || a.effect?.getTiming().iterations === Infinity)")
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

/// The drawer gives the focus back to the stage cell it opened from: that cell wears the
/// keyboard's ring, and the pointer passing over the plan traces nothing (a trace is chosen).
#[tokio::test(flavor = "multi_thread")]
async fn chromium_the_cell_the_drawer_gives_focus_back_wears_the_ring_and_hover_traces_nothing() {
    let f = Fixture::new().await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let lanes = f.id;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{lanes}")).unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelector('#n-alpha-review')")
            .unwrap();
        browser.viewport(1440, "light").unwrap();
        // no line is drawn between units
        assert_eq!(browser.eval("document.querySelectorAll('svg.edges').length").unwrap(), 0);
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
            .eval("(c => { const s = getComputedStyle(c); return [c.matches(':focus-visible'), s.outlineStyle !== 'none', c.classList.contains('open')]; })(document.activeElement)")
            .unwrap();
        assert_eq!(ring, serde_json::json!([true, true, false]), "the keyboard's ring, the drawer shut");
        hover(&mut browser, "#u-alpha .pl-t");
        for step in [40.0, 20.0, 4.0] {
            pointer(&mut browser, "mouseMoved", step, step);
        }
        settle(&mut browser);
        let lit = browser
            .eval("[document.querySelector('sluice-trace').hasAttribute('tracing'), document.querySelectorAll('[data-trace]').length]")
            .unwrap();
        assert_eq!(lit, serde_json::json!([false, 0]), "hover traced");
        assert_eq!(browser.eval("window.browserErrors").unwrap(), serde_json::json!([]));
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}

/// A phone opens on the board, its quick check, while nothing needs attention; once a step has
/// failed (or is cancelled, quiet or paused) it opens on the plan, which leads with what stopped.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_a_phone_opens_on_the_plan_while_something_needs_attention() {
    let f = Fixture::new().await;
    let lanes = f.id;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let url = format!("http://{addr}/projects/id/{lanes}");
    const READY: &str = "document.readyState === 'complete' && document.querySelector('#project-board')?.dataset.view";
    let first = url.clone();
    let view = tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&first).unwrap();
        browser.viewport(390, "light").unwrap();
        browser.navigate(&first).unwrap();
        browser.wait(READY).unwrap();
        let view = browser
            .eval("[document.querySelector('#project-board').dataset.view, document.querySelector('#project-board').hasAttribute('data-attention')]")
            .unwrap();
        assert_eq!(browser.eval("window.browserErrors").unwrap(), serde_json::json!([]));
        view
    })
    .await
    .unwrap();
    assert_eq!(
        view,
        serde_json::json!(["plan", true]),
        "beta-build failed: the plan"
    );
    // the failure is put right: nothing needs attention, and a phone opens on the board
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='succeeded',error=NULL WHERE project_id=?1 AND step_id='beta-build'",
                [lanes.to_string()],
            )?;
            tx.changed(Some(lanes), "status");
            Ok(())
        })
        .await
        .unwrap();
    let view = tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&url).unwrap();
        browser.viewport(390, "light").unwrap();
        browser.navigate(&url).unwrap();
        browser.wait(READY).unwrap();
        browser
            .eval("[document.querySelector('#project-board').dataset.view, document.querySelector('#project-board').hasAttribute('data-attention')]")
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        view,
        serde_json::json!(["board", false]),
        "nothing needs attention: the board"
    );
    server.abort();
    let _ = server.await;
}

/// A step whose state the live plan moves on is said once to a screen reader, politely: its id
/// and the state's word, also when the change moves its unit to another band (here to Done).
#[tokio::test(flavor = "multi_thread")]
async fn chromium_says_a_steps_new_state_to_a_screen_reader() {
    let f = Fixture::new().await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let lanes = f.id;
    let mut browser = tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{lanes}")).unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelector('li.sc[data-state=pending] > #n-alpha-review')")
            .unwrap();
        assert_eq!(
            browser
                .eval("document.getElementById('announce').getAttribute('aria-live')")
                .unwrap(),
            "polite"
        );
        // let the page's stream open before the change
        std::thread::sleep(std::time::Duration::from_millis(1500));
        browser
    })
    .await
    .unwrap();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='skipped' WHERE project_id=?1 AND step_id='alpha-review'",
                [lanes.to_string()],
            )?;
            tx.changed(Some(lanes), "status");
            Ok(())
        })
        .await
        .unwrap();
    let said = tokio::task::spawn_blocking(move || {
        browser
            .wait("!document.querySelector('#n-alpha-review') && document.getElementById('announce').textContent")
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(said, "alpha-review skipped");
    server.abort();
}
