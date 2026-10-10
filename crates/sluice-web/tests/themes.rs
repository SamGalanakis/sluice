//! The frame's themes and its fluid grid: every theme's tokens read from the stylesheet hold
//! WCAG contrast (text 4.5:1, focus and status marks 3:1) and keep the question's colour apart
//! from every other role; the theme cookie, the picker and the gallery over HTTP; and in
//! Chromium, at eight widths from 320 to 3840, no page scrolls sideways, the sheet's modules
//! take the columns its width gives them, a strip head stays over its rows, the first open
//! takes Sluice Light or Dark from the system once and keeps it, and a pick applies at once
//! and is kept.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
mod neutral;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::{Value, json};
use sluice_web::views::THEMES;
use std::collections::BTreeMap;

const STYLE: &str = include_str!("../assets/style.css");

/// The 27 roles every theme maps (DESIGN.md, Themes).
const ROLES: [&str; 27] = [
    "paper",
    "paper-2",
    "paper-raised",
    "ink",
    "ink-muted",
    "navy",
    "navy-deep",
    "on-navy",
    "band",
    "band-ink",
    "band-accent",
    "band-mark",
    "run",
    "on-run",
    "run-ink",
    "focus",
    "sky",
    "sky-ink",
    "sweep",
    "sand",
    "on-sand",
    "sand-mark",
    "sand-pale",
    "sand-ink",
    "coral",
    "on-coral",
    "idle",
];
/// Text and what it sits on: 4.5:1.
const TEXT: [(&str, &str); 20] = [
    ("ink", "paper"),
    ("ink", "paper-2"),
    ("ink", "paper-raised"),
    ("ink", "sand-pale"),
    ("ink", "sky"),
    ("ink-muted", "paper"),
    ("ink-muted", "paper-2"),
    ("ink-muted", "paper-raised"),
    ("ink-muted", "sand-pale"),
    ("ink-muted", "sky"),
    ("run-ink", "paper"),
    ("run-ink", "paper-2"),
    ("sand-ink", "paper"),
    ("sand-ink", "paper-2"),
    ("band-ink", "band"),
    ("band-accent", "band"),
    ("on-run", "run"),
    ("on-sand", "sand"),
    ("on-coral", "coral"),
    ("on-navy", "navy"),
];
/// A focus ring or a status mark and what it sits on: 3:1.
const MARKS: [(&str, &str); 10] = [
    ("focus", "paper"),
    ("focus", "paper-2"),
    ("sky-ink", "paper"),
    ("sky-ink", "sky"),
    ("sand-ink", "sand-pale"),
    ("sand-mark", "sand"),
    ("idle", "paper"),
    ("band-mark", "band"),
    ("coral", "band"),
    ("band-ink", "band"),
];

/// A colour's linear sRGB and its alpha.
#[derive(Clone, Copy, Debug)]
struct Rgb([f64; 3], f64);
fn to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
fn to_srgb(c: f64) -> f64 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}
fn parse(value: &str) -> Rgb {
    let v = value.trim();
    if let Some(hex) = v.strip_prefix('#') {
        assert_eq!(hex.len(), 6, "{v}");
        let at =
            |i: usize| to_linear(u8::from_str_radix(&hex[i..i + 2], 16).unwrap() as f64 / 255.0);
        return Rgb([at(0), at(2), at(4)], 1.0);
    }
    let inner = v
        .strip_prefix("oklch(")
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or_else(|| panic!("a colour: {v}"));
    let (lch, alpha) = inner.split_once('/').unwrap_or((inner, "100%"));
    let n: Vec<f64> = lch.split_whitespace().map(|x| x.parse().unwrap()).collect();
    let alpha = alpha.trim().trim_end_matches('%').parse::<f64>().unwrap() / 100.0;
    let (l, c, h) = (n[0], n[1], n[2].to_radians());
    let (a, b) = (c * h.cos(), c * h.sin());
    let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_ = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    let rgb = [
        4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
        -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
        -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_,
    ];
    // clipped to sRGB, as the page draws it
    Rgb(rgb.map(|x| to_linear(to_srgb(x))), alpha)
}
fn over(fg: Rgb, bg: Rgb) -> Rgb {
    let mix = |i: usize| to_linear(to_srgb(fg.0[i]) * fg.1 + to_srgb(bg.0[i]) * (1.0 - fg.1));
    Rgb([mix(0), mix(1), mix(2)], 1.0)
}
fn luminance(c: Rgb) -> f64 {
    0.2126 * c.0[0] + 0.7152 * c.0[1] + 0.0722 * c.0[2]
}
fn contrast(fg: Rgb, bg: Rgb) -> f64 {
    let (a, b) = (luminance(over(fg, bg)), luminance(bg));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
/// The distance between two colours in Oklab.
fn apart(x: Rgb, y: Rgb) -> f64 {
    let lab = |c: Rgb| {
        let [r, g, b] = c.0;
        let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
        let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
        let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
        [
            0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
            1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
            0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
        ]
    };
    let (a, b) = (lab(x), lab(y));
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}
/// The declarations of the block whose selector is exactly `selector`.
fn block(selector: &str) -> BTreeMap<String, String> {
    let at = STYLE
        .find(&format!("\n{selector} {{"))
        .unwrap_or_else(|| panic!("no block {selector}"));
    let body = &STYLE[at + selector.len() + 3..];
    let body = &body[..body.find("\n}").unwrap()];
    body.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (name, value) = line.strip_prefix("--")?.split_once(':')?;
            Some((
                name.to_owned(),
                value.trim().trim_end_matches(';').to_owned(),
            ))
        })
        .collect()
}
/// A theme's value of every role in its own scheme: its family's block over Sluice's, the
/// light or the dark side of each `light-dark()` by its id's suffix.
fn theme(id: &str) -> BTreeMap<String, Rgb> {
    let mut tokens = block(":root, [data-theme]");
    let (family, scheme) = id.rsplit_once('-').unwrap();
    assert!(matches!(scheme, "light" | "dark"), "{id}: a scheme");
    if family != "sluice" {
        let own = block(&format!("[data-theme^=\"{family}-\"]"));
        for role in ROLES {
            assert!(own.contains_key(role), "{id} maps every role: {role}");
        }
        tokens.extend(own);
    }
    ROLES
        .iter()
        .map(|role| {
            let value = &tokens[*role];
            let pair = match value.strip_prefix("light-dark(") {
                Some(rest) => {
                    let rest = rest.strip_suffix(')').unwrap();
                    // the split between the two colours: the comma outside any parentheses
                    let mut depth = 0;
                    let cut = rest
                        .char_indices()
                        .find(|(_, c)| {
                            match c {
                                '(' => depth += 1,
                                ')' => depth -= 1,
                                ',' if depth == 0 => return true,
                                _ => {}
                            }
                            false
                        })
                        .unwrap()
                        .0;
                    (parse(&rest[..cut]), parse(&rest[cut + 1..]))
                }
                None => (parse(value), parse(value)),
            };
            (
                role.to_string(),
                if scheme == "dark" { pair.1 } else { pair.0 },
            )
        })
        .collect()
}

#[test]
fn every_theme_maps_every_role_and_holds_its_contrast() {
    // the themes the picker offers are the stylesheet's: a block for each family but Sluice's,
    // each family a light and a dark entry, and no other theme block is there
    let families: std::collections::BTreeSet<&str> = THEMES
        .iter()
        .map(|t| t.id.rsplit_once('-').unwrap().0)
        .collect();
    assert_eq!(THEMES.len(), 14);
    assert_eq!(families.len() * 2, THEMES.len(), "{families:?}");
    assert_eq!(
        STYLE.matches("\n[data-theme^=\"").count(),
        families.len() - 1,
        "a block for each family but Sluice's"
    );
    let mut failures = vec![];
    for t in THEMES {
        assert_eq!(t.dark(), t.id.ends_with("-dark"));
        let tokens = theme(t.id);
        let get = |role: &str| tokens[role];
        for (fg, bg) in TEXT {
            let ratio = contrast(get(fg), get(bg));
            if ratio < 4.5 {
                failures.push(format!("{}: {fg} on {bg} {ratio:.2} < 4.5", t.id));
            }
        }
        // the band's muted words: its ink at 74%
        let muted = Rgb(get("band-ink").0, 0.74);
        let ratio = contrast(muted, get("band"));
        if ratio < 4.5 {
            failures.push(format!("{}: band-muted on band {ratio:.2} < 4.5", t.id));
        }
        for (fg, bg) in MARKS {
            let ratio = contrast(get(fg), get(bg));
            if ratio < 3.0 {
                failures.push(format!("{}: {fg} on {bg} {ratio:.2} < 3", t.id));
            }
        }
        // the question's colour is its own: apart from every other role a page colours with
        for other in [
            "sand", "run", "sky", "ink", "paper", "sky-ink", "sand-ink", "run-ink",
        ] {
            let d = apart(get("coral"), get(other));
            if d < 0.1 {
                failures.push(format!("{}: coral too near {other} ({d:.3})", t.id));
            }
        }
        if apart(get("run"), get("sky")) < 0.1 {
            failures.push(format!("{}: run too near sky", t.id));
        }
        // the band is the deepest surface: darker than the paper in the light, no lighter
        // than it in the dark
        assert!(
            luminance(get("band")) <= luminance(get("paper")) + 1e-9,
            "{}: the band is the deepest surface",
            t.id
        );
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[tokio::test]
async fn the_theme_is_a_cookie_the_picker_and_the_gallery_show() {
    let f = Fixture::new().await;
    let router = f.router();
    use axum::body::Body;
    use axum::http::{Request, header};
    use tower::ServiceExt;
    let post = |body: &'static str| {
        let router = router.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/settings")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };
    let cookies = |r: &axum::response::Response| -> Vec<String> {
        r.headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
            .collect()
    };
    let r = post("theme=nord-dark&next=%2Finbox").await;
    assert_eq!(r.status(), 303);
    assert_eq!(r.headers()["location"], "/inbox");
    assert_eq!(cookies(&r), ["sluice_theme=nord-dark"]);
    // Sluice's own are choices like any other, kept for a year
    let r = post("theme=sluice-light").await;
    assert_eq!(cookies(&r), ["sluice_theme=sluice-light"]);
    assert!(
        r.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=34560000"),
        "{:?}",
        r.headers()
    );
    // nothing else is a theme: not a family, not a scheme, not the old default
    for body in ["theme=dark", "theme=nord", "theme=americana", "theme="] {
        assert_eq!(post(body).await.status(), 400, "{body}");
    }
    // an appearance is no longer a setting: it sets nothing
    assert!(cookies(&post("appearance=dark").await).is_empty());

    let get = |cookie: &'static str| {
        let router = router.clone();
        async move {
            let response = router
                .oneshot(
                    Request::builder()
                        .uri("/_ui")
                        .header(header::COOKIE, cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            String::from_utf8(body.to_vec()).unwrap()
        }
    };
    let html = get("sluice_theme=rose-pine-light").await;
    assert!(
        html.contains("<html lang=\"en\" data-theme=\"rose-pine-light\">"),
        "{html}"
    );
    assert!(
        html.contains("<input type=\"radio\" name=\"theme\" value=\"rose-pine-light\" checked>"),
        "{html}"
    );
    assert!(!html.contains("appearance"), "{html}");
    // no cookie (the first open): no theme drawn, so the system's scheme picks Sluice Light or
    // Dark in the stylesheet, and none checked until the page's script keeps that choice
    let html = get("").await;
    assert!(html.contains("<html lang=\"en\">"), "{html}");
    assert!(
        !html.contains("name=\"theme\" value=\"sluice-light\" checked"),
        "{html}"
    );
    assert!(
        !html.contains("name=\"theme\" value=\"sluice-dark\" checked"),
        "{html}"
    );
    // a stale cookie (a family alone, the old default) draws nothing stale
    for stale in [
        "sluice_theme=nord; sluice_appearance=dark",
        "sluice_theme=americana",
    ] {
        let html = get(stale).await;
        assert!(html.contains("<html lang=\"en\">"), "{stale}: {html}");
    }
    // each theme in the picker with its swatch (band, paper, running, question), and in the
    // gallery once
    for t in THEMES {
        assert!(html.contains(&format!("<span>{}</span><span class=\"swatch\" data-theme=\"{}\" aria-hidden=\"true\"><i class=\"sw-run\"></i><i class=\"sw-ask\"></i></span>", t.name, t.id)), "{}", t.id);
        assert_eq!(
            html.matches(&format!(
                "<figure class=\"gal-themecard\" data-theme=\"{}\">",
                t.id
            ))
            .count(),
            1,
            "{}",
            t.id
        );
    }
}

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
/// The page's width against the window's, the light sheet's tracks and its modules' widths, and
/// the strip head's stage columns against its rows' cells.
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
    const s = document.querySelector('.gal-strips');
    r.heads = [...s.querySelectorAll('.sh-stage')].map(e => Math.round(e.getBoundingClientRect().left));
    r.rows = [...s.querySelectorAll('.strip-row > .strip')].map(st => [...st.children].map(e => Math.round(e.getBoundingClientRect().left)));
  }
  return r; })()";
/// The page's paper and its theme, as drawn.
const PAPER: &str = "[document.documentElement.dataset.theme ?? null, getComputedStyle(document.body).backgroundColor]";

#[tokio::test(flavor = "multi_thread")]
async fn chromium_every_width_fits_its_columns_and_a_theme_pick_applies() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    tokio::task::spawn_blocking(move || {
        let gallery = format!("{base}/_ui");
        let plan = format!("{base}/projects/id/{}", n.almanac);
        let home = format!("{base}/");
        let mut browser = Chrome::open(&gallery).unwrap();
        for width in [320, 390, 768, 1024, 1440, 1920, 2560, 3840] {
            for (name, url) in [("ui", &gallery), ("plan", &plan), ("home", &home)] {
                browser.viewport(width, "light").unwrap();
                browser.navigate(url).unwrap();
                browser.wait("document.readyState === 'complete'").unwrap();
                browser.eval("document.fonts.ready").unwrap();
                browser.eval(FRAMES).unwrap();
                let m: Value = browser.eval(MEASURE).unwrap();
                assert!(m["scroll"].as_f64() <= m["width"].as_f64(), "{name} at {width} scrolls sideways: {m}");
                if name != "ui" {
                    continue;
                }
                // the sheet's modules take the columns its own width gives them: every one
                // across a narrow sheet, halves and wholes on a medium one, a share of twelve
                // to 2000px and of twenty-four from there
                let sheet = m["sheet"].as_f64().unwrap();
                let (cols, takes) = if sheet < 640.0 {
                    (6.0, [6.0, 6.0, 6.0, 6.0, 6.0, 6.0])
                } else if sheet < 1200.0 {
                    (12.0, [6.0, 6.0, 6.0, 12.0, 12.0, 12.0])
                } else if sheet < 2000.0 {
                    (12.0, [6.0, 4.0, 2.0, 12.0, 12.0, 12.0])
                } else {
                    (24.0, [12.0, 8.0, 4.0, 24.0, 12.0, 12.0])
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
                // the strip head's stages stand over its rows' cells wherever the sheet sets
                // them beside their lead; under 640px the names give way and each row's cells
                // share its width
                for row in m["rows"].as_array().unwrap() {
                    if sheet < 640.0 {
                        assert_eq!(m["heads"], json!([0, 0, 0]), "{width}: {m}");
                    } else {
                        assert_eq!(row, &m["heads"], "{width}: {m}");
                    }
                }
            }
        }

        // ---- a pick: the picker applies at once, and the cookie keeps it
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&home).unwrap();
        browser.wait("document.readyState === 'complete' && !!customElements.get('sluice-toggle')").unwrap();
        let sluice = browser.eval(PAPER).unwrap();
        browser.eval("document.querySelector('details.settings').open = true").unwrap();
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
        browser.wait("new Promise(r => setTimeout(() => r(true), 300))").unwrap();
        browser.navigate(&home).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        assert_eq!(
            browser.eval("document.querySelector('input[name=theme][value=nord-dark]').checked").unwrap(),
            true
        );
        assert_eq!(browser.eval(PAPER).unwrap(), json!(["nord-dark", "rgb(46, 52, 64)"]));
        // the theme rows are 44px targets on a phone
        browser.viewport(390, "nord-dark").unwrap();
        browser.eval("document.querySelector('details.settings').open = true").unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(
            browser.eval("[...document.querySelectorAll('.prefs .themes label, .prefs label.check')].every(l => l.getBoundingClientRect().height >= 44)").unwrap(),
            true
        );
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

/// The first open takes Sluice Light or Sluice Dark from the system's scheme, once: the page's
/// script keeps it as the choice, so a later change of the system's scheme changes nothing;
/// without script the stylesheet follows the system until a theme is picked.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_the_first_open_follows_the_system_once_and_then_keeps_its_theme() {
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
        // without script no frame callback runs: the page is read once it has loaded
        let load = |browser: &mut Chrome, script: bool| {
            browser.navigate(&home).unwrap();
            browser.wait("document.readyState === 'complete'").unwrap();
            if script {
                browser.eval(FRAMES).unwrap();
            }
        };
        let checked = "document.querySelector('input[name=theme]:checked')?.value ?? null";
        for (system, other, first) in [("dark", "light", "sluice-dark"), ("light", "dark", "sluice-light")] {
            // a browser that has never chosen: a fresh profile, no cookie
            let mut browser = Chrome::open("about:blank").unwrap();
            scheme(&mut browser, system);
            load(&mut browser, true);
            assert_eq!(browser.eval("document.documentElement.dataset.theme").unwrap(), first, "{system}");
            assert_eq!(browser.eval(checked).unwrap(), first, "{system}");
            let paper = browser.eval(PAPER).unwrap();
            // kept: the server draws it from now on, whatever the system says
            browser.wait("new Promise(r => setTimeout(() => r(true), 300))").unwrap();
            scheme(&mut browser, other);
            load(&mut browser, true);
            assert_eq!(browser.eval(PAPER).unwrap(), paper, "{system} then {other}");
            assert_eq!(
                browser.eval("document.documentElement.outerHTML.startsWith('<html lang=\"en\" data-theme=')").unwrap(),
                true
            );
            // a pick replaces it, and survives the system's scheme changing back
            browser.eval("document.querySelector('details.settings').open = true").unwrap();
            browser.eval("document.querySelector('input[name=theme][value=gruvbox-light]').click()").unwrap();
            browser.wait("new Promise(r => setTimeout(() => r(true), 300))").unwrap();
            scheme(&mut browser, system);
            load(&mut browser, true);
            assert_eq!(browser.eval("document.documentElement.dataset.theme").unwrap(), "gruvbox-light");
            assert_eq!(browser.eval(checked).unwrap(), "gruvbox-light");
            assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
        }
        // without script nothing is kept: the stylesheet draws Sluice by the system's scheme
        let mut browser = Chrome::open("about:blank").unwrap();
        browser.send("Emulation.setScriptExecutionDisabled", json!({"value": true})).unwrap();
        let mut papers = vec![];
        for system in ["light", "dark"] {
            scheme(&mut browser, system);
            load(&mut browser, false);
            let drawn = browser.eval(PAPER).unwrap();
            assert_eq!(drawn[0], Value::Null, "{system}: {drawn}");
            papers.push(drawn[1].clone());
        }
        assert_ne!(papers[0], papers[1], "{papers:?}");
    })
    .await
    .unwrap();
    server.abort();
}
