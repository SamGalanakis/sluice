//! Browser checks for theme layout, font requests and display preferences.
use crate::board_fixture;
use crate::chrome;
use crate::neutral;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::{Value, json};
use sluice_web::views::THEMES;
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
/// The page's width against the window's, and the light sheet's tracks and its modules' widths.
const MEASURE: &str = "(() => {
  const d = document.documentElement, g = document.querySelector('#l-grid');
  const r = {scroll: d.scrollWidth, width: d.clientWidth};
  if (g) {
    const css = getComputedStyle(g), tracks = css.gridTemplateColumns.split(' ').map(parseFloat);
    r.sheet = g.getBoundingClientRect().width;
    r.tracks = tracks.length;
    r.track = tracks[0];
    r.gap = parseFloat(css.columnGap);
    r.widths = [...g.querySelectorAll(':scope > .mod')].map(m => m.getBoundingClientRect().width);
  }
  return r; })()";
/// Every control a phone taps on the page, in document order: its height, the height a rule
/// gives it (its min-height, a fixed 44px, or a stage cell's that its link fills; 0 for a
/// control as tall as its words) and what it is.
const TARGETS: &str = "[...document.querySelectorAll('a[href], button, summary, input:not([type=hidden]), select, textarea, [role=tab]')].filter(e => e.getClientRects().length && getComputedStyle(e).visibility !== 'hidden').map(e => { const h = e.getBoundingClientRect().height; const ruled = Math.max(parseFloat(getComputedStyle(e).minHeight) || 0, Math.abs(h - 44) < 0.5 ? 44 : 0, e.matches('.sc-a') ? parseFloat(getComputedStyle(e.parentElement).minHeight) || 0 : 0); return [Math.round(h * 10) / 10, ruled, e.outerHTML.slice(0, 120)]; })";
/// Every element whose own words are set in its theme's display face (where that is not also
/// its text face) under 19px: its size, its face and what it is.
const UNDER_FLOOR: &str = "(() => { const norm = s => s.replace(/[\"'\\s]/g, ''); return [...document.querySelectorAll('body *')].filter(e => [...e.childNodes].some(n => n.nodeType === 3 && n.textContent.trim()) && e.getClientRects().length).map(e => { const cs = getComputedStyle(e); const display = norm(cs.getPropertyValue('--font-display')), text = norm(cs.getPropertyValue('--font-text')); return [parseFloat(cs.fontSize), norm(cs.fontFamily) === display && display !== text, e.outerHTML.slice(0, 100)]; }).filter(([size, display]) => display && size < 18.95); })()";
/// Every stage cell's state word drawn on the page that clips, wraps or leaves its cell: its
/// text and its cell's state and width. A word has its own line under the cell's glyph and name.
const WORDS: &str = "[...document.querySelectorAll('.sc-word')].filter(w => w.getClientRects().length && getComputedStyle(w).display !== 'none').map(w => { const r = w.getBoundingClientRect(), c = w.closest('.sc').getBoundingClientRect(); return [w.textContent, w.closest('.sc').dataset.state, Math.round(c.width), w.scrollWidth > w.clientWidth + 0.5 || r.height > 1.5 * parseFloat(getComputedStyle(w).lineHeight) || r.right > c.right + 0.5 || r.bottom > c.bottom + 0.5]; }).filter(w => w[3])";
/// Every drawn cell that carries a glyph (done, running, a look, a cell waiting in its own way)
/// whose two lines are not whole: its glyph missing, squeezed or outside the cell, its second
/// line (the word, else the time) not under the first or its ink (less words for a screen reader) outside the cell, or a running cell's
/// sweep not along its foot.
const LINES: &str = "[...document.querySelectorAll('.sc-run, .sc-done, .sc-look, .sc-empty:has(.sc-word)')].filter(c => c.getClientRects().length && c.getBoundingClientRect().width > 0).map(c => { const r = c.getBoundingClientRect(), top = c.querySelector('.sc-top'), g = top && top.querySelector(':scope > .g'); const shown = e => e && e.getClientRects().length && getComputedStyle(e).display !== 'none'; const words = c.querySelector('.sc-word'), second = shown(words) ? words : c.querySelector('.sc-time'); const bad = []; if (!g || !shown(g)) bad.push('no glyph'); else { const b = g.getBoundingClientRect(); if (b.width < 12.5 || b.left < r.left - 0.5 || b.right > r.right + 0.5) bad.push('glyph ' + Math.round(b.width) + 'px at ' + Math.round(b.left - r.left)); } if (second && shown(second)) { const s = second.getBoundingClientRect(); if (s.top < top.getBoundingClientRect().bottom - 0.5) bad.push('line 2 beside line 1'); const k = []; const walk = document.createTreeWalker(second, NodeFilter.SHOW_TEXT); while (walk.nextNode()) { const n = walk.currentNode; if (!n.textContent.trim() || n.parentElement.closest('.vh')) continue; const ink = document.createRange(); ink.selectNodeContents(n); k.push(ink.getBoundingClientRect()); } if (k.some(b => b.right > r.right + 0.5 || b.bottom > r.bottom + 0.5)) bad.push('line 2 cut'); } if (c.matches('.sc-run')) { const w = c.querySelector('.sweep'); const sb = w && w.getBoundingClientRect(); if (!sb || Math.abs(sb.bottom - r.bottom) > 1 || Math.abs(sb.width - r.width) > 1) bad.push('sweep'); } return [c.dataset.state, Math.round(r.width), bad]; }).filter(c => c[2].length)";
/// The page's paper and its theme, as drawn.
const PAPER: &str = "[document.documentElement.dataset.theme ?? null, getComputedStyle(document.body).backgroundColor]";

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_every_width_fits_its_columns_and_a_theme_pick_applies() {
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
            for (name, url) in [("ui", &gallery), ("plan", &plan), ("home", &home)] {
                browser.viewport(width, "light").unwrap();
                browser.navigate(url).unwrap();
                browser.wait("document.readyState === 'complete'").unwrap();
                browser.eval("document.fonts.ready").unwrap();
                browser.eval(FRAMES).unwrap();
                let m: Value = browser.eval(MEASURE).unwrap();
                assert!(m["scroll"].as_f64() <= m["width"].as_f64(), "{name} at {width} scrolls sideways: {m}");
                // a cell's state word is whole on its own line, in the narrowest cell too, and
                // every cell that carries a glyph draws it before its name, its word or time under
                assert_eq!(browser.eval(WORDS).unwrap(), json!([]), "{name} at {width}");
                assert_eq!(browser.eval(LINES).unwrap(), json!([]), "{name} at {width}");
                if name == "ui" {
                    // the gallery's held strip: "⊖ review", then "blocked" under it
                    let blocked = browser.eval("(() => { const c = document.querySelector('ol.strip[aria-label=\"Stages of a-15\"] .sc-empty[data-state=blocked]'); const top = c.querySelector('.sc-top'), w = c.querySelector('.sc-word'); return [top.firstElementChild.matches('.g-blocked'), top.textContent, w.textContent, w.getBoundingClientRect().top >= top.getBoundingClientRect().bottom - 0.5, w.getClientRects().length > 0]; })()").unwrap();
                    assert_eq!(blocked, json!([true, "review", "blocked", true, true]), "{width}");
                    // and a running cell: its turning glyph before "review", its time under them
                    let running = browser.eval("(() => { const c = document.querySelector('ol.strip[aria-label=\"Stages of a-12\"] .sc-run[data-state=running]'); const top = c.querySelector('.sc-top'); return [top.firstElementChild.matches('.g-running'), top.textContent, c.querySelector('.sc-time').getBoundingClientRect().top >= top.getBoundingClientRect().bottom - 0.5]; })()").unwrap();
                    assert_eq!(running, json!([true, "review", true]), "{width}");
                }
                if name != "ui" {
                    continue;
                }
                // the sheet's modules take the columns its own width gives them: every one
                // across a narrow sheet, halves and wholes on a medium one, a share of twelve
                // from there (the frame keeps the sheet to 1440px)
                let sheet = m["sheet"].as_f64().unwrap();
                let (cols, takes) = if sheet < 640.0 {
                    (6.0, [6.0, 6.0, 6.0, 6.0])
                } else if sheet < 1200.0 {
                    (12.0, [6.0, 6.0, 6.0, 12.0])
                } else {
                    (12.0, [6.0, 4.0, 2.0, 12.0])
                };
                let (tracks, track, gap) = (
                    m["tracks"].as_f64().unwrap(),
                    m["track"].as_f64().unwrap(),
                    m["gap"].as_f64().unwrap(),
                );
                let widths = m["widths"].as_array().unwrap();
                assert_eq!(widths.len(), takes.len(), "{width}: {m}");
                for (take, w) in takes.iter().zip(widths) {
                    let spans = take * tracks / cols;
                    let want = if sheet < 640.0 { sheet } else { spans * track + (spans - 1.0) * gap };
                    assert!((w.as_f64().unwrap() - want).abs() <= 2.0, "{width}: {take} of {cols} is {w}, not {want}: {m}");
                }
            }
        }

        // ---- a sweep of none is the still line, as reduced motion draws it: on the gallery's
        // theme cards, Williamsburg's sweeps rest still and every other one moves
        browser.navigate(&gallery).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        browser.viewport(1440, "light").unwrap();
        browser.eval(FRAMES).unwrap();
        let sweeps: Vec<(String, String, String, String)> = serde_json::from_value(browser.eval("[...document.querySelectorAll('.sweep')].map(s => { const a = getComputedStyle(s, '::after'); return [s.closest('[data-theme]').dataset.theme, a.animationName, a.transform, a.opacity]; })").unwrap()).unwrap();
        assert!(sweeps.iter().any(|s| s.0.starts_with("williamsburg-")), "{sweeps:?}");
        for (theme, name, transform, opacity) in &sweeps {
            if theme.starts_with("williamsburg-") {
                assert_eq!((name.as_str(), transform.as_str(), opacity.as_str()), ("none", "none", "0.6"), "{theme}");
            } else {
                assert_ne!(name, "none", "{theme}");
            }
        }

        // ---- the picker at 30 themes (display preferences' Theme): one list, two columns from
        // 900px (a family's light and dark side by side), one under it; no name wraps; the
        // panel scrolls inside itself
        for (width, columns) in [(390, 1), (1440, 2)] {
            browser.navigate(&home).unwrap();
            browser.wait("document.readyState === 'complete' && !!customElements.get('sluice-menu')").unwrap();
            browser.viewport(width, "light").unwrap();
            browser.eval("document.querySelector('details.settings > summary').click()").unwrap();
            browser.eval(FRAMES).unwrap();
            browser.eval("document.querySelector('details.settings .sub-themes > summary').click()").unwrap();
            browser.eval(FRAMES).unwrap();
            let p: Value = browser.eval("(() => { const m = document.querySelector('details.settings .menu'); const rows = [...m.querySelectorAll('.themes label')]; const tops = new Set(rows.map(r => Math.round(r.getBoundingClientRect().top))); return {rows: rows.length, lines: tops.size, wraps: rows.filter(r => { const n = r.querySelector('span'); return n.getClientRects().length > 1 || n.scrollWidth > n.clientWidth + 0.5 || n.getBoundingClientRect().height > 30; }).map(r => r.textContent), scrolls: getComputedStyle(m).overflowY, fits: m.getBoundingClientRect().bottom <= innerHeight, right: m.getBoundingClientRect().right <= innerWidth, left: m.getBoundingClientRect().left >= 0}; })()").unwrap();
            assert_eq!(p["rows"], json!(THEMES.len()), "{width}: {p}");
            assert_eq!(p["lines"], json!(THEMES.len() / columns), "{width}: {p}");
            assert_eq!(p["wraps"], json!([]), "{width}: {p}");
            assert_eq!(p["scrolls"], json!("auto"), "{width}: {p}");
            assert!(p["fits"] == json!(true) && p["right"] == json!(true) && p["left"] == json!(true), "{width}: {p}");
        }

        // ---- a pick: the picker applies at once, and the cookie keeps it
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&home).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        let sluice = browser.eval(PAPER).unwrap();
        browser.eval("document.querySelector('details.settings').open = true").unwrap();
        browser.eval("document.querySelector('details.settings .sub-themes').open = true").unwrap();
        browser.eval(FRAMES).unwrap();
        browser.eval("document.querySelector('input[name=theme][value=nord-light]').click()").unwrap();
        browser.eval(FRAMES).unwrap();
        // Nord Light's Snow Storm paper, nord6
        assert_eq!(browser.eval(PAPER).unwrap(), json!(["nord-light", "rgb(236, 239, 244)"]));
        assert_ne!(browser.eval(PAPER).unwrap(), sluice);
        browser.eval("document.querySelector('input[name=theme][value=nord-dark]').click()").unwrap();
        browser.eval(FRAMES).unwrap();
        // Nord's Polar Night paper, nord0, under the light system preference
        assert_eq!(browser.eval(PAPER).unwrap(), json!(["nord-dark", "rgb(46, 52, 64)"]));
        // kept: a fresh load draws it from the cookie, with the picker said
        wait_theme_cookie(&mut browser);
        browser.navigate(&home).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        assert_eq!(
            browser.eval("document.querySelector('input[name=theme][value=nord-dark]').checked").unwrap(),
            true
        );
        assert_eq!(browser.eval(PAPER).unwrap(), json!(["nord-dark", "rgb(46, 52, 64)"]));
        // on a phone every row is a 44px target: the top level's, then the theme rows and the
        // back control over them
        browser.viewport(390, "nord-dark").unwrap();
        browser.eval("document.querySelector('details.settings').open = true").unwrap();
        browser.eval(FRAMES).unwrap();
        let targets = "(() => { const t = [...document.querySelectorAll('details.settings :is(.sub-row, .sub-back, label.check, .themes label)')].filter(e => e.checkVisibility()); return [t.length, t.every(e => e.getBoundingClientRect().height >= 44)]; })()";
        assert_eq!(browser.eval(targets).unwrap(), json!([3, true]));
        browser.eval("document.querySelector('details.settings .sub-themes').open = true").unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(browser.eval(targets).unwrap(), json!([THEMES.len() + 1, true]));
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
        if let Some(dir) = std::env::var_os("SLUICE_FRAME_SCREENS") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            for (name, url) in [("ui", &gallery), ("plan", &plan), ("home", &home)] {
                for width in [390, 1440, 2560, 3840] {
                    for theme in ["light", "dark"] {
                        browser.navigate(url).unwrap();
                        browser.wait("document.readyState === 'complete'").unwrap();
                        browser.viewport(width, theme).unwrap();
                        browser.eval(FRAMES).unwrap();
                        browser.screenshot(&dir.join(format!("{name}-{width}-{theme}.png"))).unwrap();
                    }
                }
            }
            for t in THEMES {
                for (name, url) in [("plan", &plan), ("home", &home)] {
                    browser.navigate(url).unwrap();
                    browser.wait("document.readyState === 'complete'").unwrap();
                    browser.viewport(1440, t.id).unwrap();
                    browser.eval(FRAMES).unwrap();
                    browser.screenshot(&dir.join(format!("theme-{}-{name}-1440.png", t.id))).unwrap();
                }
            }
        }
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

/// A page fetches only its own theme's faces: Sluice's two in Sluice, Empire Builder's in
/// Empire Builder, never another family's; opening the picker (whose swatches carry no text)
/// fetches none; and every face loads from the dashboard itself, the page asking no other host
/// for anything.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_a_page_fetches_only_its_themes_faces() {
    let f = Fixture::new().await;
    neutral::seed(&f.writer, f._home.path()).await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    tokio::task::spawn_blocking(move || {
        let home = format!("{base}/");
        let asked = |browser: &mut Chrome| -> BTreeMap<String, bool> {
            serde_json::from_value(browser.eval(FACES).unwrap()).unwrap()
        };
        for (theme, own, others) in [
            (
                "sluice-light",
                ["Schibsted Grotesk Variable", "JetBrains Mono Variable"],
                ["Libre Franklin Variable", "Courier Prime"],
            ),
            (
                "empire-dark",
                ["Libre Franklin Variable", "Courier Prime"],
                ["Schibsted Grotesk Variable", "JetBrains Mono Variable"],
            ),
        ] {
            {
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
                browser
                    .eval("document.querySelector('details.settings').open = true; document.querySelector('details.settings .sub-themes').open = true")
                    .unwrap();
                browser.eval(FRAMES).unwrap();
                browser
                    .eval("document.fonts.ready.then(() => true)")
                    .unwrap();
                let faces = asked(&mut browser);
                let (loaded, elsewhere): (BTreeMap<String, bool>, Vec<String>) =
                    serde_json::from_value(browser.eval(LOADED).unwrap()).unwrap();
                assert!(
                    elsewhere.is_empty(),
                    "{theme} asks another host: {elsewhere:?}"
                );
                for face in own {
                    assert_eq!(
                        loaded.get(face),
                        Some(&true),
                        "{theme} loads {face} from the dashboard: {loaded:?}"
                    );
                }
                for face in own {
                    assert_eq!(
                        faces.get(face),
                        Some(&true),
                        "{theme} asks for {face}: {faces:?}"
                    );
                }
                for face in others {
                    assert_eq!(
                        faces.get(face),
                        Some(&false),
                        "{theme} never asks for {face}: {faces:?}"
                    );
                }
            }
        }
    })
    .await
    .unwrap();
    server.abort();
}

/// The first open takes Sluice Light or Sluice Dark from the system's scheme, once: the page
/// keeps it as the choice, so a later change of the system's scheme changes nothing.
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
        let checked = "document.querySelector('input[name=theme]:checked')?.value ?? null";
        for (system, other, first) in [("dark", "light", "sluice-dark"), ("light", "dark", "sluice-light")] {
            // a browser that has never chosen: a fresh profile, no cookie
            let mut browser = Chrome::open("about:blank").unwrap();
            scheme(&mut browser, system);
            load(&mut browser);
            assert_eq!(browser.eval("document.documentElement.dataset.theme").unwrap(), first, "{system}");
            assert_eq!(browser.eval(checked).unwrap(), first, "{system}");
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
            browser.eval("document.querySelector('details.settings').open = true; document.querySelector('details.settings .sub-themes').open = true").unwrap();
            browser.eval("document.querySelector('input[name=theme][value=gruvbox-light]').click()").unwrap();
            wait_theme_cookie(&mut browser);
            scheme(&mut browser, system);
            load(&mut browser);
            assert_eq!(browser.eval("document.documentElement.dataset.theme").unwrap(), "gruvbox-light");
            assert_eq!(browser.eval(checked).unwrap(), "gruvbox-light");
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
const LEVEL: &str = "(() => { const m = document.querySelector('details.settings'); const seen = (s) => [...m.querySelectorAll(s)].filter(e => e.checkVisibility()); const a = document.activeElement; return {open: m.open, rows: seen('.sub-row, label.check').map(e => e.innerText.trim().replace(/\\s+/g, ' ')), back: seen('.sub-back').map(e => e.textContent), themes: seen('.themes label').length, keys: seen('a[data-go]').length, focus: a.matches('.sub-row') ? 'row ' + a.querySelector('.sub-name').textContent : a.matches('input[type=radio]') ? 'radio ' + a.value : a.matches('a[data-go]') ? 'go ' + a.dataset.go : a.matches('details.settings > summary') ? 'menu' : a.matches('.sub-back') ? 'back' : a.matches('input[type=checkbox]') ? 'types' : a.localName, theme: document.documentElement.dataset.theme, named: m.querySelector('.sub-theme').textContent, expanded: [...m.querySelectorAll('summary')].map(s => s.getAttribute('aria-expanded')), fits: m.querySelector('.menu').getBoundingClientRect().bottom <= innerHeight, scrolls: m.querySelector('.menu').scrollHeight > m.querySelector('.menu').clientHeight, header: seen('.sub-back').map(b => { const r = b.getBoundingClientRect(), p = m.querySelector('.menu').getBoundingClientRect(); return [Math.round(r.top - p.top), document.elementFromPoint(r.left + 20, r.top + r.height / 2)?.closest('.sub-back') === b]; })}; })()";

/// Display preferences are a short menu of rows: Theme opens the thirty themes in the same
/// menu, a pick applies at once and stays there, back returns to Theme's row; Keyboard
/// shortcuts opens the keys; Escape closes it all. By the keyboard (Enter or → opens a row, ← or
/// Backspace comes back, the focus following) and by the pointer, at a phone's width and a
/// desktop's.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_display_preferences_open_theme_pick_back_shortcuts_and_escape() {
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
                json!({"url": base, "name": "sluice_theme", "value": "nord-light"}),
            )
            .unwrap();
        let level = |browser: &mut Chrome| -> Value { browser.eval(LEVEL).unwrap() };
        let top = json!(["Theme Nord Light", "Show value types", "Keyboard shortcuts ?"]);
        for width in [390, 1440] {
            browser.navigate(&home).unwrap();
            browser
                .wait("document.readyState === 'complete' && !!customElements.get('sluice-menu')")
                .unwrap();
            browser.viewport(width, "nord-light").unwrap();
            // ---- the keyboard: ArrowDown from the sliders opens the top level, on Theme's row
            browser.eval("document.querySelector('details.settings > summary').focus()").unwrap();
            press(&mut browser, "ArrowDown");
            let l = level(&mut browser);
            assert_eq!(l["open"], json!(true), "{width}: {l}");
            assert_eq!(l["rows"], top, "{width}: {l}");
            assert_eq!((l["themes"].clone(), l["keys"].clone(), l["back"].clone()), (json!(0), json!(0), json!([])), "{width}: {l}");
            assert_eq!(l["focus"], json!("row Theme"), "{width}: {l}");
            assert_eq!(l["expanded"], json!(["true", "false", "false"]), "{width}: {l}");
            // Enter opens Theme in the same menu: its back control and the thirty themes alone,
            // the focus on the chosen one
            press(&mut browser, "Enter");
            let l = level(&mut browser);
            assert_eq!((l["rows"].clone(), l["back"].clone(), l["themes"].clone()), (json!([]), json!(["Theme: back to display preferences"]), json!(THEMES.len())), "{width}: {l}");
            assert_eq!(l["focus"], json!("radio nord-light"), "{width}: {l}");
            assert_eq!(l["expanded"], json!(["true", "true", "false"]), "{width}: {l}");
            // taller than the window on a phone, it scrolls inside itself; never past the window
            assert_eq!(l["fits"], json!(true), "{width}: {l}");
            assert_eq!(l["scrolls"], json!(width == 390), "{width}: {l}");
            // its back control heads it, right under the menu's 3px edge, in sight however far
            // the list scrolls
            assert_eq!(l["header"], json!([[3, true]]), "{width}: {l}");
            browser.eval("document.querySelector('details.settings .menu').scrollTo(0, 99999)").unwrap();
            browser.eval(FRAMES).unwrap();
            assert_eq!(level(&mut browser)["header"], json!([[3, true]]), "{width}");
            // ArrowDown picks the next theme at once, and the menu stays on the themes
            press(&mut browser, "ArrowDown");
            let l = level(&mut browser);
            assert_eq!((l["theme"].clone(), l["focus"].clone(), l["themes"].clone()), (json!("nord-dark"), json!("radio nord-dark"), json!(THEMES.len())), "{width}: {l}");
            assert_eq!(browser.eval(PAPER).unwrap(), json!(["nord-dark", "rgb(46, 52, 64)"]));
            // → is not a level deeper here, and moves nothing
            press(&mut browser, "ArrowRight");
            assert_eq!(level(&mut browser)["theme"], json!("nord-dark"), "{width}");
            // ← comes back to the top level, on Theme's row, which names the pick
            press(&mut browser, "ArrowLeft");
            let l = level(&mut browser);
            assert_eq!(l["rows"], json!(["Theme Nord", "Show value types", "Keyboard shortcuts ?"]), "{width}: {l}");
            assert_eq!((l["focus"].clone(), l["named"].clone(), l["themes"].clone()), (json!("row Theme"), json!("Nord"), json!(0)), "{width}: {l}");
            // down the rows to Keyboard shortcuts; → opens it, on its first key
            press(&mut browser, "ArrowDown");
            assert_eq!(level(&mut browser)["focus"], json!("types"), "{width}");
            press(&mut browser, "ArrowDown");
            assert_eq!(level(&mut browser)["focus"], json!("row Keyboard shortcuts"), "{width}");
            press(&mut browser, "ArrowRight");
            let l = level(&mut browser);
            assert_eq!((l["rows"].clone(), l["back"].clone(), l["themes"].clone(), l["focus"].clone()), (json!([]), json!(["Keyboard shortcuts: back to display preferences"]), json!(0), json!("go h")), "{width}: {l}");
            assert!(l["keys"].as_u64().unwrap() >= 2, "{width}: {l}");
            // Backspace comes back to its row; Enter opens it again; Escape closes the menu,
            // the focus on the sliders, and the next open starts at the top level
            press(&mut browser, "Backspace");
            let l = level(&mut browser);
            assert_eq!((l["rows"].clone(), l["focus"].clone()), (json!(["Theme Nord", "Show value types", "Keyboard shortcuts ?"]), json!("row Keyboard shortcuts")), "{width}: {l}");
            press(&mut browser, "Enter");
            assert_eq!(level(&mut browser)["focus"], json!("go h"), "{width}");
            press(&mut browser, "Escape");
            let l = level(&mut browser);
            assert_eq!((l["open"].clone(), l["focus"].clone()), (json!(false), json!("menu")), "{width}: {l}");
            press(&mut browser, "Enter");
            let l = level(&mut browser);
            assert_eq!((l["open"].clone(), l["rows"].clone()), (json!(true), json!(["Theme Nord", "Show value types", "Keyboard shortcuts ?"])), "{width}: {l}");
            press(&mut browser, "Escape");

            // ---- the pointer: the sliders, Theme's row, a theme, back
            browser.eval("document.querySelector('details.settings > summary').click()").unwrap();
            browser.eval(FRAMES).unwrap();
            browser.eval("document.querySelector('.sub-themes > summary').click()").unwrap();
            browser.eval(FRAMES).unwrap();
            browser.eval("document.querySelector('input[name=theme][value=nord-light]').closest('label').click()").unwrap();
            browser.eval(FRAMES).unwrap();
            let l = level(&mut browser);
            assert_eq!((l["theme"].clone(), l["themes"].clone()), (json!("nord-light"), json!(THEMES.len())), "{width}: {l}");
            browser.eval("document.querySelector('.sub-themes .sub-back').click()").unwrap();
            browser.eval(FRAMES).unwrap();
            let l = level(&mut browser);
            assert_eq!((l["rows"].clone(), l["focus"].clone()), (top.clone(), json!("row Theme")), "{width}: {l}");
            // a click elsewhere closes it
            browser.eval("document.querySelector('main').click()").unwrap();
            browser.eval(FRAMES).unwrap();
            assert_eq!(level(&mut browser)["open"], json!(false), "{width}");
            assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
            // the pick is kept for the next page
            wait_theme_cookie(&mut browser);
        }
    })
    .await
    .unwrap();
    server.abort();
}

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
        browser.wait("document.readyState === 'complete'").unwrap();
        browser.viewport(390, "light").unwrap();
        let sluice: Vec<(f64, f64, String)> =
            serde_json::from_value(browser.eval(TARGETS).unwrap()).unwrap();
        for t in THEMES.iter().filter(|t| t.id != "sluice-light") {
            for width in [390, 1440, 2560] {
                browser.viewport(width, t.id).unwrap();
                let m: Value = browser.eval(MEASURE).unwrap();
                assert!(
                    m["scroll"].as_f64() <= m["width"].as_f64(),
                    "{name} in {} at {width} scrolls sideways: {m}",
                    t.id
                );
                if width != 320 {
                    browser.eval("document.fonts.ready").unwrap();
                    assert_eq!(
                        browser.eval(WORDS).unwrap(),
                        json!([]),
                        "{name} in {} at {width}: a cell's word clips",
                        t.id
                    );
                    assert_eq!(
                        browser.eval(LINES).unwrap(),
                        json!([]),
                        "{name} in {} at {width}: a cell's lines",
                        t.id
                    );
                }
                if matches!(width, 390 | 1440) {
                    // a display face never sets words under 19px: the head takes the text face
                    browser.eval("document.fonts.ready").unwrap();
                    assert_eq!(
                        browser.eval(UNDER_FLOOR).unwrap(),
                        json!([]),
                        "{name} in {} at {width}",
                        t.id
                    );
                }
                if width == 390 {
                    let heights: Vec<(f64, f64, String)> =
                        serde_json::from_value(browser.eval(TARGETS).unwrap()).unwrap();
                    assert_eq!(
                        heights.len(),
                        sluice.len(),
                        "{name} in {}: the same targets",
                        t.id
                    );
                    for ((h, _, what), (base, ruled, _)) in heights.iter().zip(&sluice) {
                        assert!(
                            *ruled < 44.0 || *h >= 43.5,
                            "{name} in {} at 390: {what} is {h}px (Sluice's {base}px)",
                            t.id
                        );
                    }
                }
            }
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
