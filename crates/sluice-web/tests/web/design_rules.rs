//! The dashboard's design rules (DESIGN.md, Rules), checked over its stylesheet: a colour
//! written outside the token block, a colour token with only one theme, an undefined token, a
//! size off the scale, a shadow on something at rest, a heavy or dashed line, a filter on text,
//! a gradient as decoration, a third family or mono out of its roles, capitals by transform, an
//! accent read outside its meaning, motion off the one ease, or motion that reduced motion does
//! not stop.
//!
//! The regression it catches: a hard-coded colour or a resting shadow that looks right in the
//! theme it was written in and wrong in the other (a `#fff` card on Carbon, a `--muted` that
//! exists only in Paper). Nothing else catches that short of screenshotting every page in both
//! themes, and it is the one guard that keeps the token system from eroding a lane at a time.
//!
//! A kernel test: the stylesheet is read as plain text by a small parser that keeps each
//! declaration's selector list, its enclosing at-rules and its line, and every rule reports
//! each violation with that line.

/// One declaration and where it stands.
#[derive(Debug, Clone)]
pub struct Decl {
    pub line: usize,
    /// The rule's selector list, as written (comments gone, whitespace collapsed).
    pub selector: String,
    /// The enclosing at-rules' preludes, outermost first ("@media (max-width: 760px)").
    pub within: Vec<String>,
    pub prop: String,
    pub value: String,
}

/// What the parser reads: every declaration, the keyframes' names, the at-rules that are
/// statements.
#[derive(Debug, Default)]
pub struct Sheet {
    pub decls: Vec<Decl>,
    pub keyframes: Vec<(usize, String)>,
}

/// Comments blanked to spaces, newlines kept, so every offset keeps its line.
fn strip_comments(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            out.push(c as char);
            if c == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == b'"' || c == b'\'' {
            quote = Some(c);
            out.push(c as char);
            i += 1;
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let end = css[i + 2..]
                .find("*/")
                .map_or(bytes.len(), |e| i + 2 + e + 2);
            for &b in &bytes[i..end] {
                out.push(if b == b'\n' { '\n' } else { ' ' });
            }
            i = end;
            continue;
        }
        // keep multibyte characters whole
        let ch = css[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split `s` at `sep` outside parentheses and quotes.
pub fn split_top(s: &str, sep: char) -> Vec<String> {
    let mut out = vec![];
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut cur = String::new();
    for c in s.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
                continue;
            }
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                cur.push(c);
                continue;
            }
            None => {}
        }
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
        if c == sep && depth == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// Parse a stylesheet into its declarations.
pub fn parse(css: &str) -> Sheet {
    let text = strip_comments(css);
    let mut sheet = Sheet::default();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let line_of = |offset: usize| text[..offset].matches('\n').count() + 1;
    // the stack of open blocks: what each one is
    enum Block {
        At(String),
        Rule(String),
        Keyframes,
        Opaque,
    }
    let mut stack: Vec<Block> = vec![];
    let mut buf = String::new();
    let mut buf_start = 0usize;
    let mut depth_paren = 0i32;
    let mut quote: Option<char> = None;
    let flush_decls =
        |body: &str, start: usize, selector: &str, within: &[String], sheet: &mut Sheet| {
            let mut offset = start;
            for part in split_top(body, ';') {
                let lead = part.len() - part.trim_start().len();
                let here = offset + lead;
                offset += part.len() + 1;
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let Some((prop, value)) = part.split_once(':') else {
                    continue;
                };
                sheet.decls.push(Decl {
                    line: line_of(here),
                    selector: selector.to_owned(),
                    within: within.to_vec(),
                    prop: prop.trim().to_ascii_lowercase(),
                    value: collapse(value),
                });
            }
        };
    let within = |stack: &[Block]| -> Vec<String> {
        stack
            .iter()
            .filter_map(|b| match b {
                Block::At(p) => Some(p.clone()),
                _ => None,
            })
            .collect()
    };
    for &(i, c) in &chars {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            buf.push(c);
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                buf.push(c);
            }
            '(' => {
                depth_paren += 1;
                buf.push(c);
            }
            ')' => {
                depth_paren -= 1;
                buf.push(c);
            }
            '{' if depth_paren == 0 => {
                let prelude = collapse(&buf);
                let in_rule = matches!(stack.last(), Some(Block::Rule(_)));
                if in_rule {
                    // a nested rule inside a rule: not written here; read its body as its own
                    stack.push(Block::Opaque);
                } else if prelude.starts_with("@keyframes")
                    || prelude.starts_with("@-webkit-keyframes")
                {
                    let name = prelude.split_whitespace().nth(1).unwrap_or("").to_owned();
                    sheet.keyframes.push((line_of(buf_start), name));
                    stack.push(Block::Keyframes);
                } else if prelude.starts_with("@media")
                    || prelude.starts_with("@supports")
                    || prelude.starts_with("@container")
                    || prelude.starts_with("@layer")
                {
                    stack.push(Block::At(prelude));
                } else if prelude.starts_with('@') {
                    // @font-face, @property: their declarations under the at-rule's name
                    stack.push(Block::Rule(prelude));
                } else if matches!(stack.last(), Some(Block::Keyframes)) {
                    stack.push(Block::Rule(format!("@keyframes {prelude}")));
                } else {
                    stack.push(Block::Rule(prelude));
                }
                buf.clear();
                buf_start = i + 1;
            }
            '}' if depth_paren == 0 => {
                if let Some(Block::Rule(selector)) = stack.last() {
                    let selector = selector.clone();
                    let w = within(&stack);
                    flush_decls(&buf, buf_start, &selector, &w, &mut sheet);
                }
                stack.pop();
                buf.clear();
                buf_start = i + 1;
            }
            ';' if depth_paren == 0 && !matches!(stack.last(), Some(Block::Rule(_))) => {
                // a statement at-rule (@import, @charset)
                buf.clear();
                buf_start = i + 1;
            }
            _ => {
                if buf.is_empty() {
                    buf_start = i;
                }
                buf.push(c);
            }
        }
    }
    sheet
}

// ---- the rules -----------------------------------------------------------------------------

/// A rule whose selectors are all the token block's: `:root`, `:root[data-theme…]`,
/// `[data-theme…]`.
fn token_block(selector: &str) -> bool {
    split_top(selector, ',').iter().all(|s| {
        let s = s.trim();
        s == ":root"
            || s.starts_with(":root[data-theme")
            || s.starts_with(":root:not([data-theme")
            || s.starts_with("[data-theme")
    })
}

const NAMED: &[&str] = &[
    "white", "black", "red", "green", "blue", "yellow", "orange", "purple", "gray", "grey", "navy",
    "teal", "silver", "maroon", "olive", "lime", "aqua", "fuchsia", "pink", "brown", "gold",
];

/// Remove each `name(…)` call from `value`, balanced.
fn without_calls(value: &str, name: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(at) = rest.find(&format!("{name}(")) {
        out.push_str(&rest[..at]);
        let open = at + name.len();
        let mut depth = 0;
        let mut end = rest.len();
        for (j, c) in rest[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + j + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// The colour literals in a value (`url(…)` and strings left out).
fn colour_literals(value: &str) -> Vec<String> {
    let value = without_calls(&without_calls(value, "url"), "var");
    let mut found = vec![];
    let bytes = value.as_bytes();
    for (i, _) in value.match_indices('#') {
        let hex: String = value[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        if matches!(hex.len(), 3 | 4 | 6 | 8) {
            found.push(format!("#{hex}"));
        }
    }
    for f in [
        "rgb(",
        "rgba(",
        "hsl(",
        "hsla(",
        "oklch(",
        "oklab(",
        "lab(",
        "lch(",
        "color-mix(",
        "color(",
    ] {
        for (i, _) in value.match_indices(f) {
            let before = if i == 0 { b' ' } else { bytes[i - 1] };
            if !(before.is_ascii_alphanumeric() || before == b'-') {
                found.push(f.trim_end_matches('(').to_owned());
            }
        }
    }
    for word in value.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        if NAMED.contains(&word.to_ascii_lowercase().as_str()) {
            found.push(word.to_owned());
        }
    }
    found
}

/// Whether any selector of the list names one of `roles` (a class, element or attribute
/// written in it).
fn names_any(selector: &str, roles: &[&str]) -> bool {
    roles.iter().any(|role| {
        selector.match_indices(role).any(|(i, _)| {
            let after = selector[i + role.len()..].chars().next();
            let before = selector[..i].chars().last();
            // a role ending in "-" names a family of classes ("sv-" every step-view part)
            let bounded_after = role.ends_with('-')
                || after.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'));
            // an element name must not be the tail of a class or a custom element's name
            let bounded_before = role.starts_with(['.', '[', ':', '#'])
                || before.is_none_or(|c| {
                    !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
                });
            bounded_after && bounded_before
        })
    })
}
/// Whether every selector of the list names one of `roles`.
fn each_names(selector: &str, roles: &[&str]) -> bool {
    split_top(selector, ',').iter().all(|s| names_any(s, roles))
}

/// Colour tokens that are the same on both stocks (the brand's teal and its ink).
const INVARIANT: &[&str] = &["--human", "--human-hover", "--on-human"];

/// Floating things: the only selectors a blurred shadow may stand on.
const FLOATS: &[&str] = &[
    ".menu",
    ".dm-p",
    ".tip",
    ".hcard",
    ".drawer",
    ".toast",
    "sluice-banner",
    "[popover]",
    "dialog",
    ".sidebar",
    ".skip",
    ".pulse-pop",
    ".confirmation",
    ".kinds",
];
/// Mono by role: ids, stages, step names, fns, times, code and figures in a table of them.
pub const MONO_ROLES: &[&str] = &[
    "code",
    "pre",
    "kbd",
    "samp",
    "time",
    ".mono",
    ".sha",
    ".track",
    ".stages",
    ".num",
    ".call",
    ".step-id",
    ".sref-id",
    ".pl-at",
    ".log",
    ".fn-",
    ".uv-out",
    ".lg-",
    ".sv-",
    ".dl-",
    ".st-",
    ".pp-",
    ".tt-",
    ".ou-id",
    ".u-steps",
    ".run-id",
    ".ids",
    ".id",
    ".f-type",
    ".type",
    ".run-file",
    ".a-files",
    ".log-json",
    ".run-files",
    ".run-file-empty",
    ".ou-error-box",
    ".act-tool",
    ".act-key",
    ".act-args",
    ".board-program",
    ".mono-field",
    ".ou-error",
];
/// Where each accent may be read: its meaning's roles.
const MEANINGS: &[(&str, &[&str])] = &[
    (
        "--agent",
        &[
            ".engine",
            ".pk-m",
            ".av",
            ".agent",
            ".msg .by b.agent",
            ".badge.agent",
            ".engine-",
            ".by-agent",
        ],
    ),
    (
        "--danger",
        &[
            ".fail",
            ".danger",
            ".err",
            ".result.red",
            ".w-hist i.r",
            ".g-failed",
            ".b-failed",
            ".is-failed",
            ".act-failed",
            ".st-bad",
            ".setting-error",
            ".form-said",
            ".refused",
            ".bad",
            ".red",
            ".broken",
            ".x-",
            "[aria-invalid",
        ],
    ),
    (
        "--running",
        &[
            ".run",
            ".running",
            ".live",
            ".result.green",
            ".w-hist i.g",
            ".g-running",
            ".g-finishing",
            ".g-external",
            ".b-running",
            ".b-finishing",
            ".b-external",
            ".is-running",
            ".is-finishing",
            ".is-external",
            ".dot",
            ".copy",
            ".green",
            ".up",
            ".ok",
            ".sv-now",
            ".sv-nowlabel",
            ".is-cur",
            ".sv-n-run",
            ".sv-c-run",
            ".sv-b-run",
            ".dl-run",
            ".dl-bar",
            ".dl-now",
            ".tt-current",
            ".tt-nowrule",
        ],
    ),
    (
        "--overlap",
        &[
            ".quiet",
            ".stream",
            ".stream-state",
            ".notice-i",
            ".overlap",
            ".look",
            ".attn",
            ".notice",
            ".page-alert",
            ".g-stale",
            ".g-quiet",
            ".g-paused",
            ".b-stale",
            ".b-quiet",
            ".b-paused",
            ".is-quiet",
            ".is-stale",
            ".is-paused",
            ".stale",
            ".paused",
            ".is-quiet",
            ".warn",
            ".board-stale",
            ".ou-error",
            ".pk-tail",
            ".wait",
            ".held",
            ".stopped",
            ".swell-look",
            ".sv-c-look",
            ".sv-n-look",
            ".sv-b-look",
            ".dl-look",
            ".attn-note",
            ".gone",
            ".eb-icon",
            ".wait-runner",
            ".uv-broken",
            ".ou-warning",
        ],
    ),
    (
        "--human",
        &[
            "a",
            ".primary",
            ".ask",
            ".count",
            ".unread",
            ":focus",
            ".flash",
            ".nk-ask",
            ".pi",
            "::selection",
            "input",
            ".seg",
            ".brand",
            ".sys",
            ".nd-i",
            ".q-",
            ".answer",
            ".btn",
            "button",
            ".m-new",
            ".mk-ask",
            ".settings-switch",
        ],
    ),
    (
        "--accent",
        &[
            "a",
            ".primary",
            ".ask",
            ".count",
            ".unread",
            ":focus",
            ".flash",
            ".nk-ask",
            ".pi",
            "::selection",
            ".nd-i",
            ".q-",
            ".menu",
            ".link",
            ".badge.ask",
            ".page-line",
            ".lead",
            ".mark",
        ],
    ),
];

pub fn check(css: &str, sources: &str) -> Vec<String> {
    let sheet = parse(css);
    let mut errors = vec![];
    let mut defined: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for d in &sheet.decls {
        if d.prop.starts_with("--") {
            defined.insert(d.prop.clone());
        }
    }
    // tokens set inline by the views and scripts (`style="--l:…"`, `setProperty('--x', …)`)
    for (i, _) in sources.match_indices("--") {
        let name: String = sources[i..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        let after = &sources[i + name.len()..];
        if name.len() > 2
            && (after.starts_with(':') || after.starts_with("',") || after.starts_with("\","))
        {
            defined.insert(name);
        }
    }
    let mut reduced_motion = false;
    for d in &sheet.decls {
        let at = format!(
            "style.css:{} `{}` {}: {}",
            d.line, d.selector, d.prop, d.value
        );
        let custom = d.prop.starts_with("--");
        let tokens = token_block(&d.selector)
            && d.within.iter().all(|w| {
                w.contains("prefers-color-scheme")
                    || w.contains("max-width")
                    || w.contains("min-width")
            });
        let font_face = d.selector.starts_with("@font-face");
        let property = d.selector.starts_with("@property");
        if property {
            continue;
        }
        let value = d.value.as_str();
        let lower = value.to_ascii_lowercase();
        // 1. colour literals only in the token block's custom properties (a mask's alpha aside)
        let mask = d.prop.starts_with("mask") || d.prop.starts_with("-webkit-mask");
        if !mask && !font_face && !(custom && tokens) {
            for lit in colour_literals(value) {
                errors.push(format!(
                    "{at}: colour literal {lit} outside the token block (rule 1)"
                ));
            }
        }
        // 2. every colour token has both themes
        if custom
            && tokens
            && d.selector.trim() == ":root"
            && d.within.is_empty()
            && !INVARIANT.contains(&d.prop.as_str())
        {
            let rest = without_calls(value, "light-dark");
            if !colour_literals(&rest).is_empty() {
                errors.push(format!(
                    "{at}: a colour token not written as light-dark() (rule 2)"
                ));
            }
        }
        // 3. no undefined token
        let mut rest = value;
        while let Some(i) = rest.find("var(") {
            let inner = &rest[i + 4..];
            let name: String = inner
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            let fallback = inner.trim_start()[name.len()..]
                .trim_start()
                .starts_with(',');
            if !fallback && !defined.contains(&name) {
                errors.push(format!("{at}: var({name}) is defined nowhere (rule 3)"));
            }
            rest = &inner[name.len().min(inner.len())..];
        }
        // 4. sizes from the scale
        let font_size = |v: &str| {
            v.starts_with("var(--text-")
                || v.ends_with("em")
                || v == "inherit"
                || v == "0"
                || v.ends_with('%')
        };
        if d.prop == "font-size" && !font_face && !font_size(value) {
            errors.push(format!("{at}: font-size off the scale (rule 4)"));
        }
        if d.prop == "font" && !font_face {
            // [style] [weight] size[/line-height] family
            let size = value
                .split_whitespace()
                .find(|w| {
                    w.chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit() || c == 'v')
                        && (w.contains("px") || w.starts_with("var(--text") || w.contains('/'))
                })
                .map(|w| w.split('/').next().unwrap_or(""));
            if let Some(size) = size
                && !(font_size(size) || size.chars().all(|c| c.is_ascii_digit()))
            {
                errors.push(format!("{at}: font size {size} off the scale (rule 4)"));
            }
        }
        if d.prop.ends_with("radius") {
            for part in value.split(['/', ' ']).filter(|p| !p.is_empty()) {
                let small = part
                    .strip_suffix("px")
                    .and_then(|n| n.parse::<f64>().ok())
                    .is_some_and(|n| n <= 2.0);
                let ok = part.starts_with("var(--radius-")
                    || part == "var(--pill)"
                    || part == "50%"
                    || part == "0"
                    || part == "inherit"
                    || small;
                if !ok {
                    errors.push(format!("{at}: radius {part} off the scale (rule 4)"));
                }
            }
        }
        if d.prop == "z-index"
            && !(value.starts_with("var(--z-") || matches!(value, "0" | "1" | "-1" | "auto"))
        {
            errors.push(format!("{at}: z-index off the scale (rule 4)"));
        }
        // 5. no shadow at rest
        if d.prop == "box-shadow" && value != "none" && !custom {
            let float = names_any(&d.selector, FLOATS);
            for layer in split_top(value, ',') {
                let layer = layer.trim();
                let ring = layer.starts_with("inset") || {
                    let w: Vec<&str> = layer.split_whitespace().collect();
                    w.len() >= 4 && w[0] == "0" && w[1] == "0" && w[2] == "0"
                };
                let shadow_token = layer == "var(--shadow-float)" || layer == "var(--shadow-sheet)";
                if shadow_token && !float {
                    errors.push(format!("{at}: a shadow on something at rest (rule 5)"));
                } else if !ring && !shadow_token {
                    errors.push(format!(
                        "{at}: a shadow neither a ring nor the float's token (rule 5)"
                    ));
                }
            }
        }
        // 6. lines are hairlines
        if d.prop.starts_with("border")
            && !d.prop.contains("radius")
            && !d.prop.contains("collapse")
            && !d.prop.contains("spacing")
            && !d.prop.contains("color")
        {
            let tail = d.selector.contains("pk-tail");
            if lower.contains("dashed") {
                errors.push(format!("{at}: a dashed line (rule 6)"));
            }
            if lower.contains("dotted") && !tail {
                errors.push(format!("{at}: a dotted line off the pulse strip (rule 6)"));
            }
            for w in value.split_whitespace() {
                if let Some(n) = w.strip_suffix("px").and_then(|n| n.parse::<f64>().ok())
                    && n != 0.0
                    && n != 1.0
                    && !(tail && n == 2.0)
                {
                    errors.push(format!("{at}: a {w} line, not a hairline (rule 6)"));
                }
            }
        }
        // 7. no glass, no filters or blends on text
        if d.prop == "backdrop-filter" || d.prop == "-webkit-backdrop-filter" {
            errors.push(format!("{at}: backdrop-filter (rule 7)"));
        }
        if (d.prop == "filter" || d.prop == "mix-blend-mode")
            && value != "none"
            && !each_names(&d.selector, &["img", "svg", ".av", ".icon"])
        {
            errors.push(format!("{at}: {} on text (rule 7)", d.prop));
        }
        // 8. gradients only in masks or the allowed places
        if lower.contains("gradient(") && !mask && !each_names(&d.selector, &[".av"]) {
            errors.push(format!("{at}: a gradient as decoration (rule 8)"));
        }
        // 9. two families, mono by role
        let family_ok =
            |v: &str| v == "var(--font-sans)" || v == "var(--font-mono)" || v == "inherit";
        if d.prop == "font-family" && !font_face && !family_ok(value) {
            errors.push(format!("{at}: a family not from the tokens (rule 9)"));
        }
        if d.prop == "font" && !font_face && !(value == "inherit") {
            let family = value.split_whitespace().last().unwrap_or("");
            if !family_ok(family) {
                errors.push(format!("{at}: a family not from the tokens (rule 9)"));
            }
        }
        if value.contains("var(--font-mono)") && !custom && !each_names(&d.selector, MONO_ROLES) {
            errors.push(format!("{at}: mono outside its roles (rule 9)"));
        }
        // 10. sentence case; tracking from the tokens
        if d.prop == "text-transform"
            && (lower.contains("uppercase") || lower.contains("capitalize"))
        {
            errors.push(format!("{at}: capitals by transform (rule 10)"));
        }
        if d.prop == "letter-spacing"
            && !(value.starts_with("var(--tracking-") || value == "0" || value == "normal")
        {
            errors.push(format!("{at}: letter-spacing off the tokens (rule 10)"));
        }
        // 11. one accent per meaning
        if !(custom && tokens) {
            for (token, roles) in MEANINGS {
                let used = value.match_indices(&format!("var({token}")).any(|(i, _)| {
                    let rest = &value[i + 4 + token.len()..];
                    // --human vs --human-hover are one meaning; --accent vs --accent-tint too
                    rest.starts_with(')') || rest.starts_with('-') || rest.starts_with(',')
                });
                if used && !each_names(&d.selector, roles) && !d.selector.starts_with("@keyframes")
                {
                    errors.push(format!("{at}: {token} read outside its meaning (rule 11)"));
                }
            }
        }
        // 12. one ease; durations from the tokens
        if d.prop.starts_with("transition") || d.prop.starts_with("animation") {
            let words = without_calls(value, "var");
            for w in words.split([' ', ',']).filter(|w| !w.is_empty()) {
                let time = w.ends_with("ms")
                    || (w.ends_with('s') && w[..w.len() - 1].parse::<f64>().is_ok());
                if time && w != "0s" {
                    errors.push(format!("{at}: a literal duration {w} (rule 12)"));
                }
                if matches!(
                    w,
                    "ease" | "ease-in" | "ease-out" | "ease-in-out" | "step-start" | "step-end"
                ) || w.starts_with("cubic-bezier")
                    || w.starts_with("steps")
                {
                    errors.push(format!("{at}: an easing {w} off the one ease (rule 12)"));
                }
            }
        }
        // 13. reduced motion stops everything
        if d.within
            .iter()
            .any(|w| w.contains("prefers-reduced-motion: reduce"))
            && d.selector
                .split(',')
                .any(|s| s.trim() == "*" || s.trim().starts_with("*,") || s.trim() == "*::before")
            && (d.prop == "animation" || d.prop == "transition")
            && (lower.starts_with("none") || lower.starts_with("0s"))
        {
            reduced_motion = true;
        }
    }
    if !reduced_motion {
        errors.push("style.css: no `@media (prefers-reduced-motion: reduce)` rule on `*` that stops transitions and animations (rule 13)".into());
    }
    errors
}

/// Every source a token can be set from inline: the views, the templates, the scripts and the
/// sketches (each stroke's `--t0` and `--d`).
fn sources() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = String::new();
    let mut dirs = vec![
        root.join("src"),
        root.join("templates"),
        root.join("assets"),
    ];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("rs" | "html" | "js" | "svg")
            ) && !path.to_string_lossy().contains("datastar")
            {
                out.push_str(&std::fs::read_to_string(&path).unwrap());
            }
        }
    }
    out
}

/// The stylesheet keeps every rule, and the checker sees each kind of break it is for: one
/// stray colour, resting shadow or one-theme token planted in a copy of the sheet is reported.
#[test]
fn the_stylesheet_keeps_the_design_rules() {
    let css = include_str!("../../assets/style.css");
    let sources = sources();
    let errors = check(css, &sources);
    assert!(
        errors.is_empty(),
        "{} design-rule violations:\n{}",
        errors.len(),
        errors.join("\n")
    );
    for (planted, rule) in [
        (".nd-i { background: #fff; }", "rule 1"),
        (".fin-r { box-shadow: 0 1px 2px var(--line); }", "rule 5"),
        (":root { --planted: #123456; }", "rule 2"),
    ] {
        let errors = check(&format!("{css}\n{planted}\n"), &sources);
        assert!(
            errors.iter().any(|e| e.contains(rule)),
            "planting `{planted}` was not reported under {rule}: {errors:?}"
        );
    }
}
