//! The frame's themes and its fluid grid: every theme's tokens, read from the stylesheet the
//! pages get (style.css and every family's file under assets/themes, joined), hold WCAG contrast
//! (text 4.5:1, focus, status marks, chart colours and edges 3:1, also on a textured paper, a
//! look cell's border and where the band's edge meets its marks) and keep the question's colour
//! apart from every other role and out of every non-colour token; every deep token a family
//! declares is a known one, in its allowed range; a primitive (`--p-…`) never leaves its family's
//! block; the theme cookie, the picker and the gallery over HTTP; and in Chromium, at eight
//! widths from 320 to 3840 (four in every other theme), no page scrolls sideways, the sheet's
//! modules take the columns its width gives them, a strip head stays over its rows, a phone's
//! 44px targets stay 44px in every theme, a page fetches only its own theme's faces (the picker
//! none, a page without script the cookie's), the first open takes Sluice Light or Dark from the
//! system once and keeps it, and a pick applies at once and is kept.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
mod neutral;
mod seed;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::{Value, json};
use sluice_web::views::THEMES;
use std::collections::{BTreeMap, BTreeSet};

/// The stylesheet as the pages get it: style.css with every family's file joined after it.
const STYLE: &str = sluice_web::views::STYLESHEET;

/// Every theme family, in the picker's order: Sluice's in style.css, each other one's in
/// `assets/themes/<family>.css`. A new family is appended here.
const FAMILIES: &[&str] = &[
    "sluice",
    "solarized",
    "nord",
    "gruvbox",
    "catppuccin",
    "rose-pine",
    "flexoki",
    "empire",
    "kodachrome",
    "fiesta",
    "atlas",
];

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
/// The tokens every theme works out from its roles: never a family's to set.
const DERIVED: [&str; 7] = [
    "rule",
    "rule-faint",
    "line",
    "band-muted",
    "band-rule",
    "band-hover",
    "scrim",
];
/// Text and what it sits on: 4.5:1.
const TEXT: [(&str, &str); 21] = [
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
    ("on-selection", "selection"),
];
/// A focus ring, a status mark or a chart's colour and what it sits on: 3:1.
const MARKS: [(&str, &str); 18] = [
    ("focus", "paper"),
    ("focus", "paper-2"),
    ("focus", "sky"),
    ("focus", "sand-pale"),
    ("sky-ink", "paper"),
    ("sky-ink", "sky"),
    ("sand-ink", "sand-pale"),
    ("sand-mark", "sand"),
    ("idle", "paper"),
    ("band-mark", "band"),
    ("coral", "band"),
    ("band-ink", "band"),
    ("chart-ink", "paper"),
    ("chart-ink", "paper-2"),
    ("chart-busiest", "paper"),
    ("chart-busiest", "paper-2"),
    ("chart-series", "paper"),
    ("chart-series", "paper-2"),
];
/// The colour tokens of the deep set, read like roles.
const DEEP_COLOURS: [&str; 6] = [
    "selection",
    "on-selection",
    "chart-ink",
    "chart-busiest",
    "chart-series",
    "chart-range",
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

/// `text` cut at each `sep` outside any parentheses.
fn split_top(text: &str, sep: char) -> Vec<&str> {
    let (mut depth, mut from, mut out) = (0, 0, vec![]);
    for (i, c) in text.char_indices() {
        if c == sep && depth == 0 {
            out.push(&text[from..i]);
            from = i + c.len_utf8();
            continue;
        }
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
    }
    out.push(&text[from..]);
    out
}
/// A block's custom properties, each value on one line, its comments gone.
fn declarations(body: &str) -> BTreeMap<String, String> {
    let mut plain = String::new();
    let mut rest = body;
    while let Some(at) = rest.find("/*") {
        plain.push_str(&rest[..at]);
        rest = &rest[at + rest[at..].find("*/").unwrap() + 2..];
    }
    plain.push_str(rest);
    split_top(&plain, ';')
        .into_iter()
        .filter_map(|d| {
            let (name, value) = d.trim().strip_prefix("--")?.split_once(':')?;
            Some((
                name.trim().to_owned(),
                value.split_whitespace().collect::<Vec<_>>().join(" "),
            ))
        })
        .collect()
}
/// Where each block whose selector is exactly `selector` starts and ends in the stylesheet.
fn spans(selector: &str) -> Vec<(usize, usize)> {
    let needle = format!("\n{selector} {{");
    let mut out = vec![];
    let mut from = 0;
    while let Some(at) = STYLE[from..].find(&needle) {
        let start = from + at + needle.len();
        let end = start + STYLE[start..].find("\n}").unwrap();
        out.push((start, end));
        from = end;
    }
    out
}
/// The declarations of every block whose selector is exactly `selector`, in order.
fn block(selector: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (start, end) in spans(selector) {
        out.extend(declarations(&STYLE[start..end]));
    }
    out
}
fn family_selector(family: &str) -> String {
    format!("[data-theme^=\"{family}-\"]")
}
/// What a family's own blocks declare: its block, then its rule for one scheme.
fn own(family: &str, scheme: &str) -> BTreeMap<String, String> {
    let mut own = block(&family_selector(family));
    own.extend(block(&format!("[data-theme=\"{family}-{scheme}\"]")));
    own
}
/// A theme's tokens as written: Sluice's, then its family's over them.
fn written(id: &str) -> BTreeMap<String, String> {
    let (family, scheme) = id.rsplit_once('-').unwrap();
    assert!(matches!(scheme, "light" | "dark"), "{id}: a scheme");
    let mut tokens = block(":root, [data-theme]");
    if family != "sluice" {
        tokens.extend(own(family, scheme));
    }
    tokens
}
/// `value` with every `var(--name)` replaced by that token's value, its `light-dark()`s by
/// the side of its scheme.
fn resolve(tokens: &BTreeMap<String, String>, value: &str, dark: bool) -> String {
    let mut v = value.to_owned();
    for _ in 0..8 {
        let Some(at) = v.find("var(--") else { break };
        let inner = &v[at + 4..];
        let end = at + 4 + split_top(inner, ')')[0].len();
        let name = split_top(&v[at + 6..end], ',')[0].trim().to_owned();
        let with = tokens
            .get(&name)
            .unwrap_or_else(|| panic!("no token --{name} for {value}"))
            .clone();
        v.replace_range(at..=end, &with);
    }
    while let Some(at) = v.find("light-dark(") {
        let inner = &v[at + 11..];
        let body = split_top(inner, ')')[0];
        let sides = split_top(body, ',');
        let pick = sides[if dark { 1 } else { 0 }].trim().to_owned();
        v.replace_range(at..at + 11 + body.len() + 1, &pick);
    }
    v
}
/// A token's colour in a theme.
fn colour(tokens: &BTreeMap<String, String>, name: &str, dark: bool) -> Rgb {
    parse(&resolve(tokens, &tokens[name], dark))
}
/// The colours a value draws with, by the tokens it names (`var(--x)`), in a theme.
fn colours_named(tokens: &BTreeMap<String, String>, value: &str, dark: bool) -> Vec<(String, Rgb)> {
    let mut out = vec![];
    let mut rest = value;
    while let Some(at) = rest.find("var(--") {
        let tail = &rest[at + 6..];
        let name = &tail[..tail.find([')', ',']).unwrap()];
        if ROLES.contains(&name) || DEEP_COLOURS.contains(&name) || name.starts_with("p-") {
            out.push((name.to_owned(), colour(tokens, name, dark)));
        }
        rest = tail;
    }
    out
}
/// A length in px (or a bare number), from "3px", "0", "0.5px".
fn px(value: &str) -> Option<f64> {
    value.trim().trim_end_matches("px").parse().ok()
}
/// The colours drawn in the band edge's top row, where it meets the nav and its marks: each
/// layer that covers y = 0, down to the first plain colour.
fn edge_top(tokens: &BTreeMap<String, String>, dark: bool) -> Vec<(String, Rgb)> {
    let edge = &tokens["band-edge"];
    let mut out = vec![];
    if edge == "none" {
        return out;
    }
    for layer in split_top(edge, ',') {
        let layer = layer.trim();
        if !layer.contains("gradient(") {
            out.extend(colours_named(tokens, layer, dark));
            break;
        }
        let close = layer.rfind(')').unwrap();
        let (place, size) = layer[close + 1..]
            .split_once('/')
            .unwrap_or((&layer[close + 1..], ""));
        let y = place.split_whitespace().nth(1).and_then(px).unwrap_or(0.0);
        let h = size
            .split_whitespace()
            .nth(1)
            .and_then(px)
            .unwrap_or(f64::INFINITY);
        if y <= 0.0 && y + h > 0.0 {
            out.extend(colours_named(tokens, &layer[..=close], dark));
        }
    }
    out
}

#[test]
fn every_theme_maps_every_role_and_holds_its_contrast() {
    // the themes the picker offers are the stylesheet's: a block for each family but Sluice's,
    // each family a light and a dark entry, and no other theme block is there
    let families: Vec<&str> = THEMES
        .iter()
        .map(|t| t.id.rsplit_once('-').unwrap().0)
        .collect::<Vec<_>>()
        .chunks(2)
        .map(|pair| {
            assert_eq!(pair[0], pair[1], "each family light then dark");
            pair[0]
        })
        .collect();
    assert_eq!(families, FAMILIES, "the picker's families, in order");
    assert_eq!(
        STYLE.matches("\n[data-theme^=\"").count(),
        families.len() - 1,
        "a block for each family but Sluice's"
    );
    let mut failures = vec![];
    for t in THEMES {
        assert_eq!(t.dark(), t.id.ends_with("-dark"));
        let dark = t.dark();
        let tokens = written(t.id);
        let (family, scheme) = t.id.rsplit_once('-').unwrap();
        if family != "sluice" {
            let own = own(family, scheme);
            for role in ROLES {
                assert!(own.contains_key(role), "{} maps every role: {role}", t.id);
            }
        }
        let get = |name: &str| colour(&tokens, name, dark);
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
        // text on the paper with its texture at its darkest: its ink (or navy) at the
        // texture's alpha over the paper
        let alpha = tokens["paper-texture-alpha"]
            .trim_end_matches('%')
            .parse::<f64>()
            .unwrap()
            / 100.0;
        for grain in ["ink", "navy"] {
            let textured = over(Rgb(get(grain).0, alpha), get("paper"));
            for fg in ["ink", "ink-muted", "run-ink", "sand-ink"] {
                let ratio = contrast(get(fg), textured);
                if ratio < 4.5 {
                    failures.push(format!(
                        "{}: {fg} on the textured paper ({grain}) {ratio:.2} < 4.5",
                        t.id
                    ));
                }
            }
        }
        // where the band's edge meets the band's ink and its marks, they read against it
        for (name, c) in edge_top(&tokens, dark) {
            for fg in ["band-ink", "band-mark"] {
                let ratio = contrast(get(fg), c);
                if ratio < 3.0 {
                    failures.push(format!(
                        "{}: {fg} against the band edge's {name} {ratio:.2} < 3",
                        t.id
                    ));
                }
            }
        }
        // a look cell's border reads on its sand, an outlined done cell's on the paper
        for (edge, on) in [("cell-look-edge", "sand"), ("cell-done-edge", "paper")] {
            for (name, c) in colours_named(&tokens, &tokens[edge], dark) {
                let ratio = contrast(c, get(on));
                if ratio < 3.0 {
                    failures.push(format!("{}: {edge}'s {name} on {on} {ratio:.2} < 3", t.id));
                }
            }
        }
        // the question's colour is its own: apart from every other role a page colours with,
        // and from every chart colour
        for other in [
            "sand",
            "run",
            "sky",
            "ink",
            "paper",
            "sky-ink",
            "sand-ink",
            "run-ink",
            "chart-ink",
            "chart-busiest",
            "chart-series",
            "chart-range",
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

/// Is `value` in its token's allowed set (DESIGN.md, Themes: the deep tokens)? `None` when it is,
/// else why not.
fn allowed(name: &str, value: &str, head_case: &str) -> Option<String> {
    let within = |v: Option<f64>, lo: f64, hi: f64| v.is_some_and(|v| (lo..=hi).contains(&v));
    let em = |v: &str| v.trim().trim_end_matches("em").parse::<f64>().ok();
    let number = |v: &str| v.trim().parse::<f64>().ok();
    let ok = match name {
        "font-display" | "font-text" | "font-mono" => {
            let last = value.rsplit(',').next().unwrap().trim();
            [
                "serif",
                "sans-serif",
                "monospace",
                "cursive",
                "fantasy",
                "system-ui",
                "ui-serif",
                "ui-sans-serif",
                "ui-monospace",
                "ui-rounded",
            ]
            .contains(&last)
        }
        "display-weight" | "head-weight" => within(number(value), 100.0, 900.0),
        "display-tracking" => within(em(value), -0.05, 0.1),
        "head-tracking" => {
            within(em(value), -0.05, 0.2)
                // tracked capitals need room between them
                && (head_case != "uppercase" || em(value).unwrap() >= 0.05)
        }
        "head-case" => ["none", "uppercase"].contains(&value),
        "head-style" | "kind-style" => ["normal", "italic"].contains(&value),
        "head-scale" => within(number(value), 0.7, 1.1),
        "prose-numerals" => ["normal", "oldstyle-nums", "lining-nums"].contains(&value),
        "radius-module" | "radius-cell" | "radius-control" | "radius-chip" | "band-radius"
        | "chart-radius" => within(px(value), 0.0, 14.0),
        "section-rule-height" => within(px(value), 1.0, 10.0),
        "ask-rule" => within(px(value), 4.0, 8.0),
        "look-rule" => {
            let parts: Vec<&str> = value.split_whitespace().collect();
            within(px(parts[0]), 1.0, 3.0) && ["solid", "dashed"].contains(&parts[1])
        }
        "band-edge-height" => within(px(value), 0.0, 12.0),
        "band-margin" => within(px(value), 0.0, 8.0),
        "band-frame" | "cell-done-edge" | "cell-look-edge" | "cell-empty-edge" => {
            value == "none"
                || split_top(value, ',')
                    .iter()
                    .all(|s| s.trim().starts_with("inset "))
        }
        "paper-texture-alpha" => within(value.trim_end_matches('%').parse().ok(), 0.0, 6.0),
        "section-rule" | "band-edge" | "paper-texture" | "chart-grid" | "sweep-fill" => {
            // gradients and colours only, never an image
            !value.contains("url(")
        }
        "density" => within(number(value), 0.85, 1.1),
        "cell-height" => within(px(value), 46.0, 56.0),
        "cell-empty-outline" => {
            let parts: Vec<&str> = value.split_whitespace().collect();
            value == "none"
                || (within(px(parts[0]), 1.0, 2.0)
                    && ["solid", "dashed", "dotted"].contains(&parts[1]))
        }
        "chart-column-width" => within(number(value), 0.3, 1.0),
        "focus-width" => ["2px", "3px"].contains(&value),
        "focus-offset" => within(px(value), 1.0, 4.0),
        "link-thickness" => within(px(value), 1.0, 3.0),
        "link-offset" => within(px(value), 2.0, 6.0),
        "link-style" => ["solid", "dotted", "dashed"].contains(&value),
        "sweep-animation" => {
            value == "none" || {
                let parts: Vec<&str> = value.split_whitespace().collect();
                let secs = parts.iter().find_map(|p| {
                    p.strip_suffix("ms")
                        .and_then(|n| n.parse::<f64>().ok().map(|n| n / 1000.0))
                        .or_else(|| p.strip_suffix('s').and_then(|n| n.parse::<f64>().ok()))
                });
                ["tide", "lane"].contains(&parts[0]) && secs.is_some_and(|s| s >= 1.0)
            }
        }
        "glyph-stroke" => within(number(value), 1.75, 2.5),
        // a colour (the chart's, a done cell's fill, the rule's), shadows, or a lift: read by
        // the contrast test or free
        _ => true,
    };
    (!ok).then(|| format!("--{name}: {value} is outside its allowed set"))
}

#[test]
fn every_deep_token_a_family_sets_is_known_and_in_its_range() {
    let defaults = block(":root, [data-theme]");
    let deep: BTreeSet<&str> = defaults
        .keys()
        .map(String::as_str)
        .filter(|k| !ROLES.contains(k) && !DERIVED.contains(k))
        .collect();
    let mut failures = vec![];
    for family in FAMILIES {
        for scheme in ["light", "dark"] {
            let declared = if *family == "sluice" {
                defaults.clone()
            } else {
                own(family, scheme)
            };
            let head_case = written(&format!("{family}-{scheme}"))["head-case"].clone();
            for (name, value) in &declared {
                if ROLES.contains(&name.as_str()) || DERIVED.contains(&name.as_str()) {
                    if *family != "sluice" && DERIVED.contains(&name.as_str()) {
                        failures.push(format!("{family}: --{name} is worked out, never set"));
                    }
                    continue;
                }
                if name.starts_with("p-") {
                    continue;
                }
                if !deep.contains(name.as_str()) {
                    failures.push(format!("{family}: --{name} is no token"));
                    continue;
                }
                // the question's colour is the question's alone: no rule, edge, cell, chart,
                // link or sweep takes it
                if value.contains("var(--coral)") || value.contains("var(--on-coral)") {
                    failures.push(format!(
                        "{family}: --{name} draws with the question's colour"
                    ));
                }
                if let Some(why) = allowed(name, value, &head_case) {
                    failures.push(format!("{family}-{scheme}: {why}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn a_primitive_never_leaves_its_family_block() {
    // every family's block, where its primitives (`--p-…`) may be declared and read
    let blocks: Vec<(String, usize, usize)> = FAMILIES
        .iter()
        .skip(1)
        .flat_map(|family| {
            spans(&family_selector(family))
                .into_iter()
                .map(move |(start, end)| (family.to_string(), start, end))
        })
        .collect();
    let mut failures = vec![];
    for (at, _) in STYLE.match_indices("--p-") {
        let line = STYLE[..at].matches('\n').count() + 1;
        let Some((family, start, end)) = blocks.iter().find(|(_, s, e)| (*s..*e).contains(&at))
        else {
            failures.push(format!("a primitive outside a family's block, line {line}"));
            continue;
        };
        let name: String = STYLE[at + 2..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let declared = declarations(&STYLE[*start..*end]);
        if !declared.contains_key(&name) {
            failures.push(format!(
                "{family}: --{name} is read, never declared, in its block"
            ));
        }
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
/// Every control a phone taps on the page, in document order: its height, the height a rule
/// gives it (its min-height, a fixed 44px, or a stage cell's that its link fills; 0 for a
/// control as tall as its words) and what it is.
const TARGETS: &str = "[...document.querySelectorAll('a[href], button, summary, input:not([type=hidden]), select, textarea, [role=tab]')].filter(e => e.getClientRects().length && getComputedStyle(e).visibility !== 'hidden').map(e => { const h = e.getBoundingClientRect().height; const ruled = Math.max(parseFloat(getComputedStyle(e).minHeight) || 0, Math.abs(h - 44) < 0.5 ? 44 : 0, e.matches('.sc-a') ? parseFloat(getComputedStyle(e.parentElement).minHeight) || 0 : 0); return [Math.round(h * 10) / 10, ruled, e.outerHTML.slice(0, 120)]; })";
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

        // ---- every other theme, set in place on each page: no page scrolls sideways at a
        // phone's, a laptop's or a wide screen's width, and every phone target Sluice draws at
        // 44px or more stays so (a theme's rhythm and faces never shrink one)
        for (name, url) in [("ui", &gallery), ("plan", &plan), ("home", &home)] {
            browser.navigate(url).unwrap();
            browser.wait("document.readyState === 'complete'").unwrap();
            browser.viewport(390, "light").unwrap();
            let sluice: Vec<(f64, f64, String)> = serde_json::from_value(browser.eval(TARGETS).unwrap()).unwrap();
            for t in THEMES.iter().filter(|t| t.id != "sluice-light") {
                for width in [320, 390, 1440, 2560] {
                    browser.viewport(width, t.id).unwrap();
                    let m: Value = browser.eval(MEASURE).unwrap();
                    assert!(m["scroll"].as_f64() <= m["width"].as_f64(), "{name} in {} at {width} scrolls sideways: {m}", t.id);
                    if width == 390 {
                        let heights: Vec<(f64, f64, String)> = serde_json::from_value(browser.eval(TARGETS).unwrap()).unwrap();
                        assert_eq!(heights.len(), sluice.len(), "{name} in {}: the same targets", t.id);
                        for ((h, _, what), (base, ruled, _)) in heights.iter().zip(&sluice) {
                            assert!(*ruled < 44.0 || *h >= 43.5, "{name} in {} at 390: {what} is {h}px (Sluice's {base}px)", t.id);
                        }
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

/// Which faces a page asked for: each `@font-face` family the stylesheet declares, with whether
/// any of its faces has been asked to load (with script or without; a face that could not be
/// fetched counts, so this holds offline).
const FACES: &str = "(() => { const asked = {}; for (const f of document.fonts) { const k = f.family.replace(/[\"']/g, ''); asked[k] = (asked[k] ?? false) || f.status !== 'unloaded'; } return asked; })()";

/// A page fetches only its own theme's faces: Sluice's two in Sluice, Empire Builder's in
/// Empire Builder, never another family's; opening the picker (whose swatches carry no text)
/// fetches none; and a page without script draws the cookie's theme in its own faces.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_a_page_fetches_only_its_themes_faces() {
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
            for script in [true, false] {
                let mut browser = Chrome::open("about:blank").unwrap();
                if !script {
                    browser
                        .send(
                            "Emulation.setScriptExecutionDisabled",
                            json!({"value": true}),
                        )
                        .unwrap();
                }
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
                if script {
                    browser
                        .eval("document.querySelector('details.settings').open = true")
                        .unwrap();
                    browser.eval(FRAMES).unwrap();
                    browser
                        .eval("document.fonts.ready.then(() => true)")
                        .unwrap();
                }
                let faces = asked(&mut browser);
                for face in own {
                    assert_eq!(
                        faces.get(face),
                        Some(&true),
                        "{theme} (script {script}) asks for {face}: {faces:?}"
                    );
                }
                for face in others {
                    assert_eq!(
                        faces.get(face),
                        Some(&false),
                        "{theme} (script {script}) never asks for {face}: {faces:?}"
                    );
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
