//! The frame's themes and its fluid grid: every theme's tokens read from the stylesheet hold
//! WCAG contrast (text 4.5:1, focus and status marks 3:1) and keep the question's colour apart
//! from every other role; the theme and appearance cookies, the picker and the gallery over
//! HTTP; and in Chromium, at eight widths from 320 to 3840, no page scrolls sideways, the
//! sheet has the columns its width gives it (its overlay drawing them), a strip head stays
//! over its rows, and a theme and an appearance switch apply at once and are kept.
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
/// A theme's light and dark value of every role: its own block over Americana's.
fn theme(id: &str) -> BTreeMap<String, (Rgb, Rgb)> {
    let mut tokens = block(":root, [data-theme]");
    if id != "americana" {
        let own = block(&format!("[data-theme=\"{id}\"]"));
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
            (role.to_string(), pair)
        })
        .collect()
}

#[test]
fn every_theme_maps_every_role_and_holds_its_contrast() {
    // the themes the picker offers are the stylesheet's, and no other theme block is there
    let blocks = STYLE.matches("\n[data-theme=\"").count();
    assert_eq!(
        blocks,
        THEMES.len() - 1,
        "a block for each theme but the default"
    );
    let mut failures = vec![];
    for t in THEMES {
        let tokens = theme(t.id);
        for (appearance, pick) in [("light", 0), ("dark", 1)] {
            let get = |role: &str| {
                let (light, dark) = tokens[role];
                if pick == 0 { light } else { dark }
            };
            for (fg, bg) in TEXT {
                let ratio = contrast(get(fg), get(bg));
                if ratio < 4.5 {
                    failures.push(format!(
                        "{} {appearance}: {fg} on {bg} {ratio:.2} < 4.5",
                        t.id
                    ));
                }
            }
            // the band's muted words: its ink at 74%
            let muted = Rgb(get("band-ink").0, 0.74);
            let ratio = contrast(muted, get("band"));
            if ratio < 4.5 {
                failures.push(format!(
                    "{} {appearance}: band-muted on band {ratio:.2} < 4.5",
                    t.id
                ));
            }
            for (fg, bg) in MARKS {
                let ratio = contrast(get(fg), get(bg));
                if ratio < 3.0 {
                    failures.push(format!(
                        "{} {appearance}: {fg} on {bg} {ratio:.2} < 3",
                        t.id
                    ));
                }
            }
            // the question's colour is its own: apart from every other role a page colours with
            for other in [
                "sand", "run", "sky", "ink", "paper", "sky-ink", "sand-ink", "run-ink",
            ] {
                let d = apart(get("coral"), get(other));
                if d < 0.1 {
                    failures.push(format!(
                        "{} {appearance}: coral too near {other} ({d:.3})",
                        t.id
                    ));
                }
            }
            if apart(get("run"), get("sky")) < 0.1 {
                failures.push(format!("{} {appearance}: run too near sky", t.id));
            }
            // the band is the deepest surface: darker than the paper in the light, no lighter
            // than it in the dark
            assert!(
                luminance(get("band")) <= luminance(get("paper")) + 1e-9,
                "{} {appearance}: the band is the deepest surface",
                t.id
            );
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[tokio::test]
async fn the_theme_and_appearance_are_cookies_the_picker_and_the_gallery_show() {
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
    let r = post("theme=nord&appearance=dark&next=%2Finbox").await;
    assert_eq!(r.status(), 303);
    assert_eq!(r.headers()["location"], "/inbox");
    assert_eq!(cookies(&r), ["sluice_theme=nord", "sluice_appearance=dark"]);
    // the default theme and Match system are no cookie at all
    let r = post("theme=americana&appearance=").await;
    assert_eq!(cookies(&r), ["sluice_theme=", "sluice_appearance="]);
    assert!(
        r.headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .all(|v| v.to_str().unwrap().contains("Max-Age=0")),
        "{:?}",
        r.headers()
    );
    // nothing else is a theme or an appearance (the old light and dark themes neither)
    for body in [
        "theme=dark",
        "theme=solarized-dark",
        "appearance=dusk",
        "appearance=nord",
    ] {
        assert_eq!(post(body).await.status(), 400, "{body}");
    }

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
    let html = get("sluice_theme=rose-pine; sluice_appearance=light").await;
    assert!(
        html.contains("<html lang=\"en\" data-theme=\"rose-pine\" data-appearance=\"light\">"),
        "{html}"
    );
    assert!(
        html.contains("<input type=\"radio\" name=\"theme\" value=\"rose-pine\" checked>"),
        "{html}"
    );
    assert!(
        html.contains("<input type=\"radio\" name=\"appearance\" value=\"light\" checked>"),
        "{html}"
    );
    // no cookie: Americana, the system's appearance, both said checked in the picker
    let html = get("").await;
    assert!(html.contains("<html lang=\"en\">"), "{html}");
    assert!(
        html.contains("<input type=\"radio\" name=\"theme\" value=\"americana\" checked>"),
        "{html}"
    );
    assert!(
        html.contains("<input type=\"radio\" name=\"appearance\" value=\"\" checked>"),
        "{html}"
    );
    // a stale cookie (the old light and dark themes, the default named) draws nothing stale
    let html = get("sluice_theme=dark; sluice_appearance=sepia").await;
    assert!(html.contains("<html lang=\"en\">"), "{html}");
    let html = get("sluice_theme=americana").await;
    assert!(html.contains("<html lang=\"en\">"), "{html}");
    // each theme in the picker with its swatch (band, paper, running, question)
    for t in THEMES {
        assert!(html.contains(&format!("<span>{}</span><span class=\"swatch\" data-theme=\"{}\" aria-hidden=\"true\"><i class=\"sw-run\"></i><i class=\"sw-ask\"></i></span>", t.name, t.id)), "{}", t.id);
        // and in the gallery, in light and in dark
        assert_eq!(
            html.matches(&format!(
                "<figure class=\"gal-themecard\" data-theme=\"{}\">",
                t.id
            ))
            .count(),
            2,
            "{}",
            t.id
        );
    }
    assert!(html.contains("<div class=\"gal-th\" data-appearance=\"light\"><p class=\"gal-theme\">Light</p><div class=\"gal-themes\">"), "{html}");
    assert!(html.contains("<div class=\"gal-th\" data-appearance=\"dark\"><p class=\"gal-theme\">Dark</p><div class=\"gal-themes\">"), "{html}");
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
/// The page's width against the window's, the light sheet's mode and its overlay, and the
/// strip head's stage columns against its rows' cells.
const MEASURE: &str = "(() => {
  const d = document.documentElement, g = document.querySelector('#l-grid');
  const r = {scroll: d.scrollWidth, width: d.clientWidth};
  if (g) {
    const mods = [...g.querySelectorAll(':scope > .mod')];
    r.sheet = g.getBoundingClientRect().width;
    r.cells = [...g.querySelectorAll('.gc')].filter(c => c.checkVisibility()).length;
    r.cellTops = new Set([...g.querySelectorAll('.gc')].filter(c => c.checkVisibility()).map(c => Math.round(c.getBoundingClientRect().top))).size;
    r.takes = mods.map(m => getComputedStyle(m).counterReset.replace('take ', ''));
    r.widths = mods.map(m => m.getBoundingClientRect().width);
    r.gap = parseFloat(getComputedStyle(g).columnGap);
    r.col = g.querySelector('.gc').getBoundingClientRect().width;
    const s = document.querySelector('.gal-strips');
    r.heads = [...s.querySelectorAll('.sh-stage')].map(e => Math.round(e.getBoundingClientRect().left));
    r.rows = [...s.querySelectorAll('.strip-row > .strip')].map(st => [...st.children].map(e => Math.round(e.getBoundingClientRect().left)));
  }
  return r; })()";

#[tokio::test(flavor = "multi_thread")]
async fn chromium_every_width_fits_its_columns_and_a_theme_switch_applies() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    tokio::task::spawn_blocking(move || {
        let gallery = format!("{base}/_ui");
        let plan = format!("{base}/projects/id/{}", n.almanac);
        let home = format!("{base}/");
        let mut browser = Chrome::open(&gallery).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser.eval("try { localStorage.setItem('sluice.grid', '1') } catch {}").unwrap();
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
                browser.wait("!!customElements.get('sluice-grid') && document.querySelector('#l-grid').hasAttribute('showing')").unwrap();
                let m: Value = browser.eval(MEASURE).unwrap();
                // the sheet's columns follow its own width; the overlay draws them in one row
                let sheet = m["sheet"].as_f64().unwrap();
                let (cols, takes) = if sheet < 640.0 {
                    (6, ["6", "6", "6", "6", "6", "6"])
                } else if sheet < 1200.0 {
                    (12, ["6", "6", "6", "12", "12", "12"])
                } else if sheet < 2000.0 {
                    (12, ["6", "4", "2", "12", "12", "12"])
                } else {
                    (24, ["12", "8", "4", "24", "12", "12"])
                };
                assert_eq!(m["cells"], cols, "{width}: {m}");
                assert_eq!(m["cellTops"], 1, "{width}: {m}");
                assert_eq!(m["takes"], json!(takes), "{width}: {m}");
                // each module is its columns and the gaps between them, on the overlay's lines
                let (col, gap) = (m["col"].as_f64().unwrap(), m["gap"].as_f64().unwrap());
                for (take, w) in takes.iter().zip(m["widths"].as_array().unwrap()) {
                    let take: f64 = take.parse().unwrap();
                    let want = if sheet < 640.0 { sheet } else { take * col + (take - 1.0) * gap };
                    assert!((w.as_f64().unwrap() - want).abs() <= 2.0, "{width}: {take} col is {w}, not {want}: {m}");
                }
                // the strip head's stages stand over its rows' cells at every width
                for row in m["rows"].as_array().unwrap() {
                    assert_eq!(row, &m["heads"], "{width}: {m}");
                }
            }
        }
        browser.eval("try { localStorage.removeItem('sluice.grid') } catch {}").unwrap();

        // ---- a theme switch: the picker applies at once, and the cookie keeps it
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&home).unwrap();
        browser.wait("document.readyState === 'complete' && !!customElements.get('sluice-toggle')").unwrap();
        let paper = "getComputedStyle(document.body).backgroundColor";
        let americana = browser.eval(paper).unwrap();
        browser.eval("document.querySelector('details.settings').open = true").unwrap();
        browser.eval("document.querySelector('input[name=theme][value=nord]').click()").unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(browser.eval("document.documentElement.dataset.theme").unwrap(), "nord");
        // Nord's Snow Storm paper, nord6
        assert_eq!(browser.eval(paper).unwrap(), "rgb(236, 239, 244)");
        assert_ne!(browser.eval(paper).unwrap(), americana);
        browser.eval("document.querySelector('input[name=appearance][value=dark]').click()").unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(browser.eval("document.documentElement.dataset.appearance").unwrap(), "dark");
        // Nord's Polar Night paper, nord0, under the light system preference
        assert_eq!(browser.eval(paper).unwrap(), "rgb(46, 52, 64)");
        // kept: a fresh load draws it from the cookies, with no script involved
        browser.wait("new Promise(r => setTimeout(() => r(true), 300))").unwrap();
        browser.navigate(&home).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        assert_eq!(
            browser.eval("[document.documentElement.dataset.theme, document.documentElement.dataset.appearance, document.querySelector('input[name=theme][value=nord]').checked]").unwrap(),
            json!(["nord", "dark", true])
        );
        assert_eq!(browser.eval(paper).unwrap(), "rgb(46, 52, 64)");
        // back to the default and the system: no attribute left behind
        browser.eval("document.querySelector('details.settings').open = true").unwrap();
        browser.eval("document.querySelector('input[name=theme][value=americana]').click()").unwrap();
        browser.eval("document.querySelector('input[name=appearance][value=\"\"]').click()").unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(
            browser.eval("[document.documentElement.hasAttribute('data-theme'), document.documentElement.hasAttribute('data-appearance')]").unwrap(),
            json!([false, false])
        );
        assert_eq!(browser.eval(paper).unwrap(), americana);
        // the appearance and theme rows are 44px targets on a phone
        browser.viewport(390, "light").unwrap();
        browser.eval("document.querySelector('details.settings').open = true").unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(
            browser.eval("[...document.querySelectorAll('.prefs .themes label, .prefs .seg label')].every(l => l.getBoundingClientRect().height >= 44)").unwrap(),
            true
        );
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
        if let Some(dir) = std::env::var_os("SLUICE_FRAME_SCREENS") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            for (name, url) in [("ui", &gallery), ("plan", &plan), ("home", &home)] {
                for width in [390, 1440, 2560, 3840] {
                    for appearance in ["light", "dark"] {
                        browser.viewport(width, appearance).unwrap();
                        browser.navigate(url).unwrap();
                        browser.wait("document.readyState === 'complete'").unwrap();
                        browser.eval("document.fonts.ready").unwrap();
                        browser.eval(FRAMES).unwrap();
                        browser.screenshot(&dir.join(format!("{name}-{width}-{appearance}.png"))).unwrap();
                    }
                }
            }
            for t in THEMES {
                for appearance in ["light", "dark"] {
                    for (name, url) in [("plan", &plan), ("home", &home)] {
                        browser.viewport(1440, appearance).unwrap();
                        browser.navigate(url).unwrap();
                        browser.wait("document.readyState === 'complete'").unwrap();
                        browser.eval(&format!("document.documentElement.dataset.theme = '{}'", t.id)).unwrap();
                        browser.eval("document.fonts.ready").unwrap();
                        browser.eval(FRAMES).unwrap();
                        browser.screenshot(&dir.join(format!("theme-{}-{name}-1440-{appearance}.png", t.id))).unwrap();
                    }
                }
            }
        }
    })
    .await
    .unwrap();
    server.abort();
}
