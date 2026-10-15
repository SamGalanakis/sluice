//! Real Chromium gate for the project page: the board beside the plan from 1280px with a
//! splitter between them and a Plan · Both · Board switch, its own section behind the Plan ·
//! Board switch below that (each remembered per project), nothing at all without a board;
//! centred, unclipped and never scrolling sideways, light and dark. The drawer giving
//! the focus back to a step row on the plan, and a step's new state said to a screen reader.
//! `SLUICE_BOARD_SCREENS=<dir>` also saves a screenshot of each state there.
use crate::board_fixture;
use crate::chrome;
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
          board: box('#board-pane'), sum: box('.page-line'),
          navFits: (l => l.scrollWidth <= l.clientWidth)(document.querySelector('.subnav nav.tabs.lead')), tabs: box('.view-switch'), split: box('.splitter'),
          view: (() => { const p = document.querySelector('#plan-pane')?.checkVisibility(), b = document.querySelector('#board-pane')?.checkVisibility(); return p && b ? 'both' : b ? 'board' : 'plan'; })(),
          h1: box('.page-head h1'),
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
async fn slow_chromium_board_beside_the_plan_and_behind_a_switch_on_a_phone() {
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
        let ready = "document.querySelector('#project-board') && document.readyState === 'complete' && !!customElements.get('sluice-board')";
        browser.wait(ready).unwrap();
        let listener_counts = |browser: &mut Chrome| {
            browser.send("Runtime.evaluate", serde_json::json!({
                "expression": "Object.fromEntries(['pointerdown','keydown'].map(t=>[t,(getEventListeners(document.querySelector('sluice-splitter'))[t]||[]).length]))",
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
        // with a step open, taking the drawer out ends its step's stream (Datastar's
        // requestCancellation 'cleanup', its binding gone)
        browser.eval("window.ended=new Set();document.addEventListener('datastar-fetch',e=>{if(e.detail.type==='finished')ended.add(e.detail.el)});location.hash='#step:beta-build'").unwrap();
        browser.wait("!!document.querySelector('#drawer-stream > span')").unwrap();
        for _ in 0..3 {
            browser.wait("!!document.querySelector('#drawer-stream > span')").unwrap();
            browser.eval("(()=>{window.binding=document.querySelector('#drawer-stream > span');const board=document.querySelector('sluice-splitter'),parent=board.parentNode,next=board.nextSibling;board.remove();parent.insertBefore(board,next);window.host=document.querySelector('sluice-drawer');window.drawerParent=host.parentNode;window.after=host.nextSibling;host.remove()})()").unwrap();
            browser.wait("ended.has(binding)").unwrap();
            browser.eval("drawerParent.insertBefore(host,after)").unwrap();
            assert_eq!(listener_counts(&mut browser), expected);
            assert_eq!(scroll_count(&mut browser), scroll);
        }
        browser.eval("history.replaceState(null,'',location.pathname+location.search);dispatchEvent(new HashChangeEvent('hashchange'))").unwrap();
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
                        // failed), the switch in the row over the project's title
                        ("board", 390) => {
                            assert!(g["tabs"].is_object() && g["board"].is_null() && plan.is_object(), "{label}: {g}");
                            assert_eq!(g["view"], "plan", "{label}: {g}");
                            assert!(g["h1"].is_object(), "{label}: the project's title stays over its board {g}");
                            assert!(g["tabs"]["top"].as_f64().unwrap() < g["h1"]["top"].as_f64().unwrap(), "{label}: the switch in the row, over the title {g}");
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
                            // the frame's centred column, at most 1440px, the band's content on
                            // the same edges
                            let column = g["navRight"].as_f64().unwrap() - g["navLeft"].as_f64().unwrap();
                            assert!(
                                (page - column).abs() < 1.0
                                    && page > (vw - 140.0).min(1439.0)
                                    && page < 1441.0
                                    && (left - (vw - right)).abs() < 24.0,
                                "{label}: {g}"
                            );
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
            // the switch in the row, over the title
            assert!(g["tabs"]["top"].as_f64().unwrap() < g["h1"]["top"].as_f64().unwrap(), "{label}: {g}");
        }
        // Every page's times are nav.js's, in one vocabulary: a running time is a two-unit
        // duration, never "… ago".
        let since = browser
            .eval("(() => { const t = document.createElement('time'); t.id = 't-since'; const at = new Date(Date.now() - 3600e3).toISOString(); t.dataset.since = at; t.setAttribute('datetime', at); document.querySelector('main').append(t); return new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => { document.dispatchEvent(new CustomEvent('datastar-signal-patch', {detail: {}})); r(t.textContent); }))); })()")
            .unwrap();
        assert_eq!(since, "1h 0m", "{since}");
        assert_eq!(browser.eval("document.querySelector('#t-since').textContent").unwrap(), "1h 0m");
        browser.eval("document.querySelector('#t-since').remove()").unwrap();
        // On a phone the switch shows one section at a time and the server keeps the choice:
        // it is a link, answered with the page in that view and the project's cookie.
        browser
            .navigate(&format!("{base}/projects/id/{lanes}"))
            .unwrap();
        browser.wait(ready).unwrap();
        browser.viewport(390, "light").unwrap();
        pick(&mut browser, "plan", ready);
        let g = browser.eval(GEOMETRY).unwrap();
        check(&g, "plan tab");
        assert!(g["plan"].is_object() && g["board"].is_null(), "{g}");
        assert_eq!(
            browser
                .eval("document.querySelector('[data-view-tab=plan]').getAttribute('aria-current')")
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
        // an address that asks the plan something (a find, a Show) opens on the plan, whatever
        // view was picked last
        pick(&mut browser, "board", ready);
        for query in ["q=alpha", "show=attention"] {
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
        // At 1440px each choice of the switch is drawn by the server, says it is the current
        // one (filled), and is kept: a fresh load draws it again from the cookie.
        browser.viewport(1440, "light").unwrap();
        let shown = "(() => { const v = s => document.querySelector(s)?.checkVisibility() ?? false; return [document.querySelector('#project-board').dataset.view, v('#plan-pane'), v('#board-pane'), [...document.querySelectorAll('[data-view-tab]')].filter(a => a.getAttribute('aria-current') === 'true').map(a => a.dataset.viewTab).join(), getComputedStyle(document.querySelector('[data-view-tab][aria-current=true]'), '::before').backgroundColor !== getComputedStyle(document.querySelector('[data-view-tab][aria-current=false]'), '::before').backgroundColor]; })()";
        for (choice, plan, board) in [("plan", true, false), ("board", false, true), ("both", true, true)] {
            pick(&mut browser, choice, ready);
            assert_eq!(browser.eval(shown).unwrap(), serde_json::json!([choice, plan, board, choice, true]), "{choice}");
            browser
                .navigate(&format!("{base}/projects/id/{lanes}"))
                .unwrap();
            browser.wait(ready).unwrap();
            assert_eq!(browser.eval(shown).unwrap(), serde_json::json!([choice, plan, board, choice, true]), "{choice} kept");
            assert_eq!(
                browser.eval("document.documentElement.outerHTML.includes('data-view=')").unwrap(),
                true
            );
        }
        browser
            .navigate(&format!("{base}/projects/id/{lanes}"))
            .unwrap();
        browser.wait(ready).unwrap();
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
        pick(&mut browser, "board", ready);
        let g = browser.eval(GEOMETRY).unwrap();
        check(&g, "board alone");
        assert!(g["plan"].is_null() && near(&g["board"]["width"], g["right"].as_f64().unwrap() - g["left"].as_f64().unwrap()), "{g}");
        pick(&mut browser, "both", ready);
        // A button press on the page sends the say and says so under the board.
        browser
            .eval("document.querySelector('#board-pane input[name=\"field-0\"]').value='gamma'; document.querySelector('#board-pane input[name=\"field-0\"]').closest('form').querySelector('[name=\"field-1\"]').value='go now'; document.querySelector('#board-pane button[name=button][value=\"0\"]').click()")
            .unwrap();
        browser
            .wait("document.querySelector('#board-form-said').textContent === 'Sent to @lash-f4.'")
            .unwrap();
        // The settings page's Board section, with its live preview.
        browser
            .navigate(&format!("{base}/projects/id/{lanes}/settings"))
            .unwrap();
        // the preview is drawn when its fold is opened
        browser
            .wait("document.readyState === 'complete' && !!document.querySelector('#board-preview')")
            .unwrap();
        browser
            .eval("document.querySelector('#board-preview').closest('details').open = true")
            .unwrap();
        browser
            .wait("document.querySelector('#board-preview .metric-v')")
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

/// The drawer gives the focus back to the step row it opened from, in a unit's steps under its
/// row on the plan: that row's link wears the keyboard's ring.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_the_step_the_drawer_gives_focus_back_wears_the_ring() {
    let f = Fixture::new().await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let lanes = f.id;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{lanes}")).unwrap();
        const LINK: &str = "#u-alpha a.sv-a[data-step=alpha-review]";
        browser
            .wait("document.readyState === 'complete' && document.querySelector('#u-alpha > summary')")
            .unwrap();
        browser.viewport(1440, "light").unwrap();
        // no line is drawn between units
        assert_eq!(browser.eval("document.querySelectorAll('svg.edges').length").unwrap(), 0);
        let (x, y) = hover(&mut browser, "#u-alpha > summary .ro-t");
        pointer(&mut browser, "mousePressed", x, y);
        pointer(&mut browser, "mouseReleased", x, y);
        browser.wait("document.querySelector('#u-alpha').open").unwrap();
        settle(&mut browser);
        let (x, y) = hover(&mut browser, LINK);
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
            .wait(&format!("location.hash === '' && document.getElementById('drawer').hidden && document.activeElement === document.querySelector('{LINK}')"))
            .unwrap();
        settle(&mut browser);
        let ring = browser
            .eval("(c => { const s = getComputedStyle(c); return [c.matches(':focus-visible'), s.outlineStyle !== 'none']; })(document.activeElement)")
            .unwrap();
        assert_eq!(ring, serde_json::json!([true, true]), "the keyboard's ring, the drawer shut");
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
async fn slow_chromium_a_phone_opens_on_the_plan_while_something_needs_attention() {
    let f = Fixture::new().await;
    let lanes = f.id;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let url = format!("http://{addr}/projects/id/{lanes}");
    const READY: &str = "document.readyState === 'complete' && !!document.querySelector('#project-board') && !!customElements.get('sluice-board')";
    const SHOWN: &str = "[document.querySelector('#plan-pane').checkVisibility() ? 'plan' : 'board', document.querySelector('#project-board').hasAttribute('data-plan-first'), document.querySelector('#project-board').dataset.view ?? null]";
    let first = url.clone();
    let view = tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&first).unwrap();
        browser.viewport(390, "light").unwrap();
        browser.navigate(&first).unwrap();
        browser.wait(READY).unwrap();
        let view = browser.eval(SHOWN).unwrap();
        assert_eq!(
            browser.eval("window.browserErrors").unwrap(),
            serde_json::json!([])
        );
        view
    })
    .await
    .unwrap();
    assert_eq!(
        view,
        serde_json::json!(["plan", true, null]),
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
        browser.eval(SHOWN).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        view,
        serde_json::json!(["board", false, null]),
        "nothing needs attention: the board"
    );
    server.abort();
    let _ = server.await;
}

/// A step whose state the live plan moves on is said once to a screen reader, politely: its id
/// and the state's word, also when the change moves its unit elsewhere on the plan (here into
/// Needs you).
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_says_a_steps_new_state_to_a_screen_reader() {
    let f = Fixture::new().await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let lanes = f.id;
    let mut browser = tokio::task::spawn_blocking(move || {
        let mut browser = open_live(&format!("http://{addr}/projects/id/{lanes}"));
        browser
            .wait("document.readyState === 'complete' && document.querySelector('[data-said*=\"alpha-review pending\"]')")
            .unwrap();
        assert_eq!(
            browser
                .eval("document.getElementById('announce').getAttribute('aria-live')")
                .unwrap(),
            "polite"
        );
        // let the page's stream open before the change
        browser.wait("window.sluiceStreamReady === true").unwrap();
        browser
    })
    .await
    .unwrap();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='failed' WHERE project_id=?1 AND step_id='alpha-review'",
                [lanes.to_string()],
            )?;
            tx.changed(Some(lanes), "status");
            Ok(())
        })
        .await
        .unwrap();
    let said = tokio::task::spawn_blocking(move || {
        let said = browser
            .wait("!document.querySelector('[data-said*=\"alpha-review pending\"]') && document.getElementById('announce').textContent")
            .unwrap();
        // the stream's patches redraw the nav and keep its live line
        assert_eq!(
            browser.eval("document.querySelectorAll('#top-nav .live').length").unwrap(),
            1
        );
        said
    })
    .await
    .unwrap();
    assert_eq!(said, "alpha-review failed");
    server.abort();
}

fn open_live(url: &str) -> Chrome {
    let mut browser = Chrome::open("about:blank").unwrap();
    browser.send("Page.addScriptToEvaluateOnNewDocument", serde_json::json!({"source": "document.addEventListener('datastar-fetch', e => { if (e.detail.type === 'datastar-patch-signals' && JSON.parse(e.detail.argsRaw?.signals ?? '{}').rel) window.sluiceStreamReady = true; });"})).unwrap();
    browser.navigate(url).unwrap();
    browser.wait("window.sluiceStreamReady === true").unwrap();
    browser
}

/// Follow the view switch's link to `view`, as a click would: the server draws the page in it
/// and keeps it in the project's cookie.
fn pick(browser: &mut Chrome, view: &str, ready: &str) {
    let href = browser
        .eval(&format!(
            "document.querySelector('[data-view-tab={view}]').href"
        ))
        .unwrap();
    browser.navigate(href.as_str().unwrap()).unwrap();
    browser.wait(ready).unwrap();
}
