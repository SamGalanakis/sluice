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
          plan: box('#plan-pane'), board: box('#board-pane'), tabs: box('.view-switch'), split: box('.splitter'),
          view: document.querySelector('#project-board').dataset.view ?? null,
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
                    assert!(plan.is_object(), "{label}: {g}");
                    match (slug, width) {
                        ("board", 390) => {
                            assert!(g["tabs"].is_object() && g["board"].is_null(), "{label}: {g}");
                        }
                        ("board", _) => {
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
                            assert!(g["tabs"].is_null() && g["board"].is_null(), "{label}: {g}");
                            assert!(near(&plan["width"], g["right"].as_f64().unwrap() - g["left"].as_f64().unwrap()), "{label}: {g}");
                        }
                    }
                    shoot(&mut browser, &format!("project-{slug}-{width}-{theme}"));
                }
            }
        }
        // On a phone the switch shows one section at a time and remembers the choice.
        browser
            .navigate(&format!("{base}/projects/id/{lanes}"))
            .unwrap();
        browser.wait(ready).unwrap();
        browser.viewport(390, "light").unwrap();
        browser
            .eval("document.querySelector('[data-view-tab=board]').click()")
            .unwrap();
        let g = browser.eval(GEOMETRY).unwrap();
        check(&g, "board tab");
        assert!(g["plan"].is_null() && g["board"].is_object(), "{g}");
        assert_eq!(
            browser
                .eval("document.querySelector('[data-view-tab=board]').getAttribute('aria-pressed')")
                .unwrap(),
            "true"
        );
        for theme in ["light", "dark"] {
            browser.viewport(390, theme).unwrap();
            shoot(&mut browser, &format!("project-board-tab-390-{theme}"));
        }
        browser
            .navigate(&format!("{base}/projects/id/{lanes}"))
            .unwrap();
        browser.wait(ready).unwrap();
        browser.viewport(390, "light").unwrap();
        let g = browser.eval(GEOMETRY).unwrap();
        assert_eq!(g["view"], "board", "remembered {g}");
        assert!(g["plan"].is_null() && g["board"].is_object(), "{g}");
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

/// Each drawn edge, and whether it leaves its source's foot and ends on its dependent's entry
/// (the top of the chips on its top edge, or its own top), and whether it runs over a chip.
const EDGES: &str = r#"(() => {
  const plane = document.querySelector('.plane').getBoundingClientRect();
  const chips = [...document.querySelectorAll('.plane .xref')].map(c => c.getBoundingClientRect());
  return [...document.querySelectorAll('svg.edges .wires path:not(.head)')].map(p => {
    const a = document.querySelector(`[data-node="${p.dataset.from}"]`).getBoundingClientRect();
    const node = document.querySelector(`[data-node="${p.dataset.to}"]`), z = node.getBoundingClientRect();
    const entry = (node.parentElement.classList.contains('stack') ? node.parentElement : node).getBoundingClientRect();
    const n = p.getAttribute('d').match(/-?[\d.]+/g).map(Number);
    const [x1, y1, x2, y2] = [n[0], n[1], n.at(-2), n.at(-1)];
    let over = false;
    for (let s = 0; s < p.getTotalLength() && !over; s += 2) {
      const q = p.getPointAtLength(s), x = q.x + plane.left, y = q.y + plane.top;
      over = chips.some(r => x > r.left + 1 && x < r.right - 1 && y > r.top + 1 && y < r.bottom - 1);
    }
    return {from: p.dataset.from, to: p.dataset.to, over,
            lands: x1 >= a.left - plane.left - 1 && x1 <= a.right - plane.left + 1
              && Math.abs(y1 - (a.bottom - plane.top)) < 1.5
              && x2 >= z.left - plane.left - 1 && x2 <= z.right - plane.left + 1
              && Math.abs(y2 + 7 - (entry.top - plane.top)) < 1.5};
  });
})()"#;
const BOXES: &str = r#"[...document.querySelectorAll('.boxes.units > .box')].map(b => {
  const r = b.getBoundingClientRect(); return {id: b.id, left: r.left, top: r.top, bottom: r.bottom, width: r.width}; })"#;

/// The plan's units as a grid keyed off the plan pane's width: units of one lane side by side
/// where the pane is wide, stacked on a phone; the edges inside each land on their cards after
/// layout and after a resize, and nothing scrolls sideways.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_plan_grid_lays_units_side_by_side_and_edges_land_on_their_cards() {
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
        for width in [1440, 1300, 2560, 1440] {
            browser.viewport(width, "light").unwrap();
            browser
                .wait("document.querySelectorAll('svg.edges .wires path:not(.head)').length === 2")
                .unwrap();
            let boxes = browser.eval(BOXES).unwrap();
            let boxes = boxes.as_array().unwrap();
            assert_eq!(boxes.len(), 2, "{width}: {boxes:?}");
            let (a, b) = (&boxes[0], &boxes[1]);
            assert!(
                near(&a["top"], b["top"].as_f64().unwrap()),
                "{width}: one grid row {boxes:?}"
            );
            assert!(
                b["left"].as_f64().unwrap()
                    > a["left"].as_f64().unwrap() + a["width"].as_f64().unwrap(),
                "{width}: side by side {boxes:?}"
            );
            assert!(
                a["width"].as_f64().unwrap() >= 296.0,
                "{width}: a cell holds a card {boxes:?}"
            );
            let edges = browser.eval(EDGES).unwrap();
            assert!(
                edges
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|e| e["lands"] == true && e["over"] == false),
                "{width}: {edges}"
            );
            let scroll = browser
                .eval("document.documentElement.scrollWidth - document.documentElement.clientWidth")
                .unwrap();
            assert_eq!(scroll, 0, "{width}: sideways scroll");
        }
        browser.viewport(390, "light").unwrap();
        let boxes = browser.eval(BOXES).unwrap();
        let boxes = boxes.as_array().unwrap();
        assert!(
            boxes[1]["top"].as_f64().unwrap() >= boxes[0]["bottom"].as_f64().unwrap(),
            "phone: stacked {boxes:?}"
        );
        assert_eq!(
            browser
                .eval("document.querySelectorAll('svg.edges path').length")
                .unwrap(),
            0
        );
        assert_eq!(
            browser.eval("window.browserErrors").unwrap(),
            serde_json::json!([])
        );
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}

/// Every chip on the board: whether it sits in its card's stack before the card, on the card's
/// top edge (at most 6px above it), and its look: hairline, fill and ink (a focused chip's ring
/// is a state of its own, not its look).
const CHIPS: &str = r#"[...document.querySelectorAll('.plane .xref')].map(c => {
  const stack = c.closest('.stack'), card = stack?.lastElementChild, s = getComputedStyle(c);
  const r = c.getBoundingClientRect(), k = card?.getBoundingClientRect();
  return {from: c.dataset.from, to: c.dataset.to, text: c.textContent,
          first: !!stack && stack.firstElementChild.contains(c) && card.matches('[data-node]')
            && card.dataset.node === c.dataset.to
            && !!(c.compareDocumentPosition(card) & Node.DOCUMENT_POSITION_FOLLOWING),
          gap: k ? k.top - r.bottom : null,
          look: [s.borderTopColor, s.borderTopStyle, s.backgroundColor, s.color, s.boxShadow].join(' | ')};
})"#;
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

/// A card's inputs from other units are chips on its top edge, read before it, each saying its
/// kind ("after alpha-build"); edges arriving at the card land on them and none runs over a
/// chip. Every chip at rest looks the same, and stays so after a chip opened the drawer and
/// the drawer closed: the focus it gives back (to the source card, or the chip) traces nothing
/// once the pointer has left, so no chip is left lit with nothing held.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_chips_sit_on_their_cards_top_edge_and_rest_alike() {
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
            .wait("document.querySelectorAll('svg.edges .wires path:not(.head)').length === 2")
            .unwrap();
        let chips = browser.eval(CHIPS).unwrap();
        let chips = chips.as_array().unwrap();
        let texts: Vec<_> = chips.iter().map(|c| c["text"].as_str().unwrap()).collect();
        assert_eq!(texts, ["after alpha-build", "after alpha-review"], "{chips:?}");
        for chip in chips {
            assert_eq!(chip["first"], true, "a chip before its card: {chip}");
            let gap = chip["gap"].as_f64().unwrap();
            assert!((0.0..=6.0).contains(&gap), "on its card's top edge: {chip}");
        }
        let rest = chips[0]["look"].clone();
        assert!(chips.iter().all(|c| c["look"] == rest), "one look at rest: {chips:?}");
        // beta-review's own edge, from beta-build, lands on its chip, not across it
        let edges = browser.eval(EDGES).unwrap();
        assert!(
            edges
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["lands"] == true && e["over"] == false),
            "{edges}"
        );
        // a chip opens its source in the drawer; Escape closes it and gives the focus back (to
        // the source card, or the chip); the pointer passes over a card and leaves the board
        let chip = r#".xref[data-from="s:alpha-review"]"#;
        let (x, y) = hover(&mut browser, chip);
        pointer(&mut browser, "mousePressed", x, y);
        pointer(&mut browser, "mouseReleased", x, y);
        browser
            .wait("location.hash === '#step:alpha-review'")
            .unwrap();
        browser
            .send(
                "Input.dispatchKeyEvent",
                serde_json::json!({"type": "keyDown", "key": "Escape", "code": "Escape",
                                   "windowsVirtualKeyCode": 27}),
            )
            .unwrap();
        browser
            .wait("location.hash === '' && document.getElementById('drawer').hidden && document.activeElement?.matches('#n-alpha-review, .xref')")
            .unwrap();
        settle(&mut browser);  // the page reflows as the drawer leaves
        hover(&mut browser, "#n-beta-build");
        browser
            .wait("document.querySelector('.plane').classList.contains('tracing')")
            .unwrap();
        for step in [40.0, 20.0, 4.0] {
            pointer(&mut browser, "mouseMoved", step, step);
        }
        settle(&mut browser);  // the chips' and cards' transitions run out
        let lit = browser
            .eval("[document.querySelector('.plane').classList.contains('tracing'), document.querySelectorAll('.plane .on').length]")
            .unwrap();
        assert_eq!(lit, serde_json::json!([false, 0]), "a trace left on");
        let after = browser.eval(CHIPS).unwrap();
        assert!(
            after.as_array().unwrap().iter().all(|c| c["look"] == rest),
            "one look at rest after the drawer: {after}"
        );
        assert_eq!(
            browser.eval("window.browserErrors").unwrap(),
            serde_json::json!([])
        );
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}
