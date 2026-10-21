//! Browser checks for the two themes (Paper and Carbon), their faces and display preferences.
use crate::board_fixture;
use crate::chrome;
use crate::neutral;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::{Value, json};
use std::collections::BTreeMap;
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
/// The page's width against the window's.
const MEASURE: &str =
    "({scroll: document.documentElement.scrollWidth, width: document.documentElement.clientWidth})";
/// Every control a phone taps on the page, in document order: its height, the height a rule
/// gives it (its min-height, or a fixed 44px; 0 for a control as tall as its words) and what it
/// is.
const TARGETS: &str = "[...document.querySelectorAll('a[href], button, summary, input:not([type=hidden]), select, textarea, [role=tab]')].filter(e => e.getClientRects().length && getComputedStyle(e).visibility !== 'hidden').map(e => { const h = e.getBoundingClientRect().height; const ruled = Math.max(parseFloat(getComputedStyle(e).minHeight) || 0, Math.abs(h - 44) < 0.5 ? 44 : 0); return [Math.round(h * 10) / 10, ruled, e.outerHTML.slice(0, 120)]; })";
/// The page's theme and its paper, as drawn.
const PAPER: &str = "[document.documentElement.dataset.theme ?? null, getComputedStyle(document.body).backgroundColor]";
const CHECKED: &str =
    "document.querySelector('details.settings input[name=theme]:checked')?.value ?? null";

/// Every page fits every width, and a pick in display preferences applies at once (the radio
/// pair in the menu's top level, bound to `<html>`), is kept by its cookie for the next page,
/// and on a phone every row of the menu is a 44px target.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_every_width_fits_and_a_theme_pick_applies() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    tokio::task::spawn_blocking(move || {
        let gallery = format!("{base}/_ui");
        let plan = format!("{base}/projects/id/{}", n.almanac);
        let home = format!("{base}/");
        let mut browser = Chrome::open(&gallery).unwrap();
        for width in [390, 1440, 2560] {
            for theme in ["light", "dark"] {
                for (name, url) in [("ui", &gallery), ("plan", &plan), ("home", &home)] {
                    browser.viewport(width, theme).unwrap();
                    browser.navigate(url).unwrap();
                    browser.wait("document.readyState === 'complete'").unwrap();
                    browser.eval("document.fonts.ready").unwrap();
                    browser.eval(FRAMES).unwrap();
                    let m: Value = browser.eval(MEASURE).unwrap();
                    assert!(
                        m["scroll"].as_f64() <= m["width"].as_f64(),
                        "{name} at {width} in {theme} scrolls sideways: {m}"
                    );
                }
            }
        }

        // ---- a pick: the pair applies at once, and the cookie keeps it
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&home).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        browser.eval("document.querySelector('input[name=theme][value=paper]').click()").unwrap();
        browser.eval(FRAMES).unwrap();
        let paper = browser.eval(PAPER).unwrap();
        assert_eq!(paper[0], json!("paper"));
        browser.eval("document.querySelector('details.settings').open = true").unwrap();
        browser.eval(FRAMES).unwrap();
        browser
            .eval("document.querySelector('input[name=theme][value=carbon]').closest('label').click()")
            .unwrap();
        browser.eval(FRAMES).unwrap();
        let carbon = browser.eval(PAPER).unwrap();
        assert_eq!(carbon[0], json!("carbon"));
        assert_ne!(carbon[1], paper[1], "Carbon draws its own paper");
        // kept: a fresh load draws it from the cookie, with the pair said, under the light
        // system preference
        wait_theme_cookie(&mut browser);
        browser.navigate(&home).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        assert_eq!(browser.eval(CHECKED).unwrap(), json!("carbon"));
        assert_eq!(browser.eval(PAPER).unwrap(), carbon);
        // on a phone every row of the menu is a 44px target: the pair, the types, the keys' row,
        // then the keys' back control
        browser.viewport(390, "carbon").unwrap();
        browser.eval("document.querySelector('details.settings').open = true").unwrap();
        browser.eval(FRAMES).unwrap();
        let targets = "(() => { const t = [...document.querySelectorAll('details.settings :is(.sub-row, .sub-back, label.check, .seg label)')].filter(e => e.checkVisibility()); return [t.length, t.filter(e => e.getBoundingClientRect().height < 44).map(e => e.textContent)]; })()";
        assert_eq!(browser.eval(targets).unwrap(), json!([4, []]));
        browser.eval("document.querySelector('details.settings .sub-keys').open = true").unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(browser.eval(targets).unwrap(), json!([1, []]));
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

/// Which faces a page asked for: each `@font-face` family the stylesheet declares, with whether
/// any of its faces has been asked to load (a face that could not be fetched counts, so this
/// holds offline).
const FACES: &str = "(() => { const asked = {}; for (const f of document.fonts) { const k = f.family.replace(/[\"']/g, ''); asked[k] = (asked[k] ?? false) || f.status !== 'unloaded'; } return asked; })()";
/// Each face family the page has loaded, and every URL the page fetched that is not the
/// dashboard's own.
const LOADED: &str = "(() => { const loaded = {}; for (const f of document.fonts) { const k = f.family.replace(/[\"']/g, ''); loaded[k] = (loaded[k] ?? false) || f.status === 'loaded'; } return [loaded, performance.getEntriesByType('resource').map(e => e.name).filter(u => new URL(u).origin !== location.origin)]; })()";

/// The stylesheet declares two faces, Inter and JetBrains Mono, and both themes set every word
/// in them: a page in either loads both from the dashboard itself and asks no other host for
/// anything.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_a_page_loads_its_two_faces_from_the_dashboard() {
    let f = Fixture::new().await;
    neutral::seed(&f.writer, f._home.path()).await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    tokio::task::spawn_blocking(move || {
        let home = format!("{base}/");
        let faces = ["Inter Variable", "JetBrains Mono Variable"];
        for theme in ["paper", "carbon"] {
            let mut browser = Chrome::open("about:blank").unwrap();
            browser
                .send(
                    "Network.setCookie",
                    json!({"url": base, "name": "sluice_theme", "value": theme}),
                )
                .unwrap();
            browser.navigate(&home).unwrap();
            browser.wait("document.readyState === 'complete'").unwrap();
            browser
                .eval("document.fonts.ready.then(() => true)")
                .unwrap();
            let asked: BTreeMap<String, bool> =
                serde_json::from_value(browser.eval(FACES).unwrap()).unwrap();
            assert_eq!(
                asked.keys().map(String::as_str).collect::<Vec<_>>(),
                faces,
                "{theme} declares only the two faces"
            );
            let (loaded, elsewhere): (BTreeMap<String, bool>, Vec<String>) =
                serde_json::from_value(browser.eval(LOADED).unwrap()).unwrap();
            assert!(
                elsewhere.is_empty(),
                "{theme} asks another host: {elsewhere:?}"
            );
            for face in faces {
                assert_eq!(
                    loaded.get(face),
                    Some(&true),
                    "{theme} loads {face} from the dashboard: {loaded:?}"
                );
            }
        }
    })
    .await
    .unwrap();
    server.abort();
}

/// The first open takes Paper or Carbon from the system's scheme, once: the page keeps it as
/// the choice, so a later change of the system's scheme changes nothing; a pick of the other
/// replaces it and survives the scheme changing back.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_the_first_open_follows_the_system_once_and_then_keeps_its_theme() {
    let f = Fixture::new().await;
    neutral::seed(&f.writer, f._home.path()).await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    tokio::task::spawn_blocking(move || {
        let home = format!("{base}/");
        let scheme = |browser: &mut Chrome, scheme: &str| {
            browser
                .send(
                    "Emulation.setEmulatedMedia",
                    json!({"features": [{"name": "prefers-color-scheme", "value": scheme}]}),
                )
                .unwrap();
        };
        let load = |browser: &mut Chrome| {
            browser.navigate(&home).unwrap();
            browser.wait("document.readyState === 'complete'").unwrap();
            browser.eval(FRAMES).unwrap();
        };
        for (system, other, first, pick) in [
            ("dark", "light", "carbon", "paper"),
            ("light", "dark", "paper", "carbon"),
        ] {
            // a browser that has never chosen: a fresh profile, no cookie
            let mut browser = Chrome::open("about:blank").unwrap();
            scheme(&mut browser, system);
            load(&mut browser);
            assert_eq!(browser.eval("document.documentElement.dataset.theme").unwrap(), first, "{system}");
            assert_eq!(browser.eval(CHECKED).unwrap(), first, "{system}");
            let paper = browser.eval(PAPER).unwrap();
            // kept: the server draws it from now on, whatever the system says
            wait_theme_cookie(&mut browser);
            scheme(&mut browser, other);
            load(&mut browser);
            assert_eq!(browser.eval(PAPER).unwrap(), paper, "{system} then {other}");
            assert_eq!(
                browser.eval("document.documentElement.outerHTML.startsWith('<html lang=\"en\" data-theme=')").unwrap(),
                true
            );
            // a pick replaces it, and survives the system's scheme changing back
            browser.eval("document.querySelector('details.settings').open = true").unwrap();
            browser
                .eval(&format!("document.querySelector('input[name=theme][value={pick}]').click()"))
                .unwrap();
            wait_theme_cookie(&mut browser);
            scheme(&mut browser, system);
            load(&mut browser);
            assert_eq!(browser.eval("document.documentElement.dataset.theme").unwrap(), pick);
            assert_eq!(browser.eval(CHECKED).unwrap(), pick);
            assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
        }
    })
    .await
    .unwrap();
    server.abort();
}

/// A key pressed as the keyboard sends it (Enter and Space with their text, so a summary or a
/// radio acts on them as it would).
fn press(browser: &mut Chrome, key: &str) {
    let (code, text) = match key {
        "Enter" => (13, "\r"),
        " " => (32, " "),
        "Escape" => (27, ""),
        "Backspace" => (8, ""),
        "ArrowLeft" => (37, ""),
        "ArrowUp" => (38, ""),
        "ArrowRight" => (39, ""),
        "ArrowDown" => (40, ""),
        other => panic!("no code for {other}"),
    };
    let name = if key == " " { "Space" } else { key };
    for kind in ["keyDown", "keyUp"] {
        let mut event =
            json!({"type": kind, "key": key, "code": name, "windowsVirtualKeyCode": code});
        if kind == "keyDown" && !text.is_empty() {
            event["text"] = json!(text);
        }
        browser.send("Input.dispatchKeyEvent", event).unwrap();
    }
    browser.eval(FRAMES).unwrap();
}

/// Display preferences, one level at a time: what shows, what has the focus, and the theme.
const LEVEL: &str = "(() => { const m = document.querySelector('details.settings'); const seen = (s) => [...m.querySelectorAll(s)].filter(e => e.checkVisibility()); const a = document.activeElement; return {open: m.open, rows: seen('.pref-theme legend, .seg label, .sub-row, label.check').map(e => e.innerText.trim().replace(/\\s+/g, ' ')), back: seen('.sub-back').map(e => e.textContent), keys: seen('a[data-go]').length, focus: a.matches('.sub-row') ? 'row ' + a.querySelector('.sub-name').textContent : a.matches('input[type=radio]') ? 'radio ' + a.value : a.matches('a[data-go]') ? 'go ' + a.dataset.go : a.matches('details.settings > summary') ? 'menu' : a.matches('.sub-back') ? 'back' : a.matches('input[type=checkbox]') ? 'types' : a.localName, theme: document.documentElement.dataset.theme, expanded: [...m.querySelectorAll('summary')].map(s => s.getAttribute('aria-expanded')), fits: m.querySelector('.menu').getBoundingClientRect().bottom <= innerHeight}; })()";

/// Display preferences are a short menu: Theme's pair in place (the arrows choose, applied at
/// once), Show value types, and Keyboard shortcuts, whose keys open in the same menu under a
/// back control; Escape closes it all. By the keyboard (ArrowDown from the opener comes in on
/// the chosen theme; Enter or → opens the keys, ← or Backspace comes back, the focus following)
/// and by the pointer, at a phone's width and a desktop's.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_display_preferences_choose_in_place_open_shortcuts_and_escape() {
    let f = Fixture::new().await;
    neutral::seed(&f.writer, f._home.path()).await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    tokio::task::spawn_blocking(move || {
        let home = format!("{base}/");
        let mut browser = Chrome::open("about:blank").unwrap();
        browser
            .send(
                "Network.setCookie",
                json!({"url": base, "name": "sluice_theme", "value": "carbon"}),
            )
            .unwrap();
        let level = |browser: &mut Chrome| -> Value { browser.eval(LEVEL).unwrap() };
        let top = json!(["Theme", "Paper", "Carbon", "Show value types", "Keyboard shortcuts ?"]);
        for width in [390, 1440] {
            browser.navigate(&home).unwrap();
            browser
                .wait("document.readyState === 'complete' && !!customElements.get('sluice-menu')")
                .unwrap();
            browser.viewport(width, "carbon").unwrap();
            if width == 390 {
                // a phone's sidebar is the drawer the bar's menu button opens
                browser.eval("document.querySelector('[data-nav-open]').click()").unwrap();
                browser.eval(FRAMES).unwrap();
            }
            // ---- the keyboard: ArrowDown from the opener opens the top level, on Carbon
            browser.eval("document.querySelector('details.settings > summary').focus()").unwrap();
            press(&mut browser, "ArrowDown");
            let l = level(&mut browser);
            assert_eq!(l["open"], json!(true), "{width}: {l}");
            assert_eq!(l["rows"], top, "{width}: {l}");
            assert_eq!((l["keys"].clone(), l["back"].clone()), (json!(0), json!([])), "{width}: {l}");
            assert_eq!(l["focus"], json!("radio carbon"), "{width}: {l}");
            assert_eq!(l["expanded"], json!(["true", "false"]), "{width}: {l}");
            assert_eq!(l["fits"], json!(true), "{width}: {l}");
            // the arrows choose in place, at once, and the menu stays open on the pair
            press(&mut browser, "ArrowUp");
            let l = level(&mut browser);
            assert_eq!((l["theme"].clone(), l["focus"].clone(), l["open"].clone()), (json!("paper"), json!("radio paper"), json!(true)), "{width}: {l}");
            press(&mut browser, "ArrowDown");
            let l = level(&mut browser);
            assert_eq!(l["theme"], json!("carbon"), "{width}: {l}");
            // from the types, ArrowDown reaches Keyboard shortcuts' row and → opens it, on its
            // first key, the top level out of sight
            browser.eval("document.querySelector('details.settings input[type=checkbox]').focus()").unwrap();
            press(&mut browser, "ArrowDown");
            assert_eq!(level(&mut browser)["focus"], json!("row Keyboard shortcuts"), "{width}");
            press(&mut browser, "ArrowRight");
            let l = level(&mut browser);
            assert_eq!((l["rows"].clone(), l["back"].clone(), l["focus"].clone()), (json!([]), json!(["Keyboard shortcuts: back to display preferences"]), json!("go h")), "{width}: {l}");
            assert!(l["keys"].as_u64().unwrap() >= 2, "{width}: {l}");
            assert_eq!(l["expanded"], json!(["true", "true"]), "{width}: {l}");
            // Backspace comes back to its row; Enter opens it again; Escape closes the menu,
            // the focus on its opener, and the next open starts at the top level
            press(&mut browser, "Backspace");
            let l = level(&mut browser);
            assert_eq!((l["rows"].clone(), l["focus"].clone()), (top.clone(), json!("row Keyboard shortcuts")), "{width}: {l}");
            press(&mut browser, "Enter");
            assert_eq!(level(&mut browser)["focus"], json!("go h"), "{width}");
            press(&mut browser, "Escape");
            let l = level(&mut browser);
            assert_eq!((l["open"].clone(), l["focus"].clone()), (json!(false), json!("menu")), "{width}: {l}");
            press(&mut browser, "Enter");
            let l = level(&mut browser);
            assert_eq!((l["open"].clone(), l["rows"].clone()), (json!(true), top.clone()), "{width}: {l}");
            press(&mut browser, "Escape");

            // ---- the pointer: the opener, Paper, the keys' row, back
            browser.eval("document.querySelector('details.settings > summary').click()").unwrap();
            browser.eval(FRAMES).unwrap();
            browser.eval("document.querySelector('input[name=theme][value=paper]').closest('label').click()").unwrap();
            browser.eval(FRAMES).unwrap();
            let l = level(&mut browser);
            assert_eq!((l["theme"].clone(), l["open"].clone()), (json!("paper"), json!(true)), "{width}: {l}");
            browser.eval("document.querySelector('.sub-keys > summary').click()").unwrap();
            browser.eval(FRAMES).unwrap();
            assert_eq!(level(&mut browser)["rows"], json!([]), "{width}");
            browser.eval("document.querySelector('.sub-keys .sub-back').click()").unwrap();
            browser.eval(FRAMES).unwrap();
            let l = level(&mut browser);
            assert_eq!((l["rows"].clone(), l["focus"].clone()), (top.clone(), json!("row Keyboard shortcuts")), "{width}: {l}");
            // a click elsewhere closes it
            browser.eval("document.querySelector('main').click()").unwrap();
            browser.eval(FRAMES).unwrap();
            assert_eq!(level(&mut browser)["open"], json!(false), "{width}");
            assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
            // the pick is kept for the next page; the next width starts from Carbon again
            wait_theme_cookie(&mut browser);
            browser.eval("document.querySelector('input[name=theme][value=carbon]').click()").unwrap();
            wait_theme_cookie(&mut browser);
        }
    })
    .await
    .unwrap();
    server.abort();
}

/// Paper and Carbon draw the same page: the same controls at a phone's width, each as tall,
/// and neither scrolls sideways at any width.
async fn theme_geometry(name: &'static str) {
    let f = Fixture::new().await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    let url = match name {
        "ui" => format!("{base}/_ui"),
        "plan" => format!("{base}/projects/id/{}", f.id),
        _ => format!("{base}/"),
    };
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open("about:blank").unwrap();
        browser.navigate(&url).unwrap();
        // Module imports can still be upgrading folds and hiding inactive tabs when
        // readyState is complete. Compare both themes after the same enhancement.
        browser
            .wait("document.readyState === 'complete' && !!customElements.get('sluice-pulse')")
            .unwrap();
        browser.eval("document.fonts.ready").unwrap();
        let mut drawn = Vec::new();
        for theme in ["paper", "carbon"] {
            for width in [390, 1440, 2560] {
                browser.viewport(width, theme).unwrap();
                let m: Value = browser.eval(MEASURE).unwrap();
                assert!(
                    m["scroll"].as_f64() <= m["width"].as_f64(),
                    "{name} in {theme} at {width} scrolls sideways: {m}"
                );
            }
            browser.viewport(390, theme).unwrap();
            let targets: Vec<(f64, f64, String)> =
                serde_json::from_value(browser.eval(TARGETS).unwrap()).unwrap();
            drawn.push(targets);
        }
        let (paper, carbon) = (&drawn[0], &drawn[1]);
        assert_eq!(paper.len(), carbon.len(), "{name}: the same targets");
        for ((h, ruled, what), (base, _, _)) in carbon.iter().zip(paper) {
            assert!(
                (h - base).abs() < 0.5,
                "{name} at 390: {what} is {h}px in Carbon, {base}px in Paper"
            );
            assert!(
                *ruled < 44.0 || *h >= 43.5,
                "{name} at 390: {what} is {h}px"
            );
        }
        assert_eq!(
            browser.eval("window.browserErrors ?? []").unwrap(),
            json!([])
        );
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}
#[tokio::test(flavor = "multi_thread")]
async fn slow_theme_geometry_gallery() {
    theme_geometry("ui").await;
}
#[tokio::test(flavor = "multi_thread")]
async fn slow_theme_geometry_plan() {
    theme_geometry("plan").await;
}
#[tokio::test(flavor = "multi_thread")]
async fn slow_theme_geometry_home() {
    theme_geometry("home").await;
}

fn wait_theme_cookie(browser: &mut Chrome) {
    let expected = browser
        .eval("document.documentElement.dataset.theme")
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .expect("the selected theme is stored");
        let cookies = browser
            .send_timeout("Network.getCookies", json!({}), remaining)
            .unwrap();
        if cookies["cookies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|cookie| cookie["name"] == "sluice_theme" && cookie["value"] == expected)
        {
            break;
        }
    }
}
