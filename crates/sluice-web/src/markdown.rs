//! Markdown is the only untrusted rich-text input accepted by the shared layout.
use crate::views::TrustedHtml;
use comrak::{Arena, Options, nodes::NodeValue, parse_document};

/// Decode percent spellings before checking the scheme. Comrak has already
/// decoded HTML entities in AST destinations. Reject ambiguous controls rather
/// than relying on a browser's scheme normalization.
pub fn allowed_url(url: &str) -> bool {
    let mut decoded = url.to_owned();
    for _ in 0..8 {
        let bytes = decoded.as_bytes();
        let mut next = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%'
                && i + 2 < bytes.len()
                && let Ok(byte) =
                    u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                next.push(byte);
                i += 3;
            } else {
                next.push(bytes[i]);
                i += 1;
            }
        }
        let Ok(next) = String::from_utf8(next) else {
            return false;
        };
        if next == decoded {
            break;
        }
        decoded = next;
    }
    if decoded
        .as_bytes()
        .windows(3)
        .any(|w| w[0] == b'%' && w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit())
        || decoded.chars().any(char::is_control)
        || decoded.contains('\\')
    {
        return false;
    }
    let decoded = decoded.trim();
    if decoded.starts_with("//") {
        return false;
    }
    let head = decoded.split(['/', '?', '#']).next().unwrap_or_default();
    match head.split_once(':') {
        None => true,
        Some((scheme, _)) => ["http", "https", "mailto"]
            .iter()
            .any(|s| scheme.eq_ignore_ascii_case(s)),
    }
}

pub fn render(text: &str) -> TrustedHtml {
    render_from(text, 4)
}

/// `render`, its highest heading at level `top` (3 to 6) and the rest under it in turn, so
/// the text nests under the heading it is drawn beneath without skipping a level.
pub fn render_from(text: &str, top: u8) -> TrustedHtml {
    let top = i16::from(top.clamp(1, 6));
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.table = true;
    options.render.escape = true;
    options.render.r#unsafe = false;
    let root = parse_document(&arena, text, &options);
    let lowest = root
        .descendants()
        .filter_map(|n| match n.data.borrow().value {
            NodeValue::Heading(h) => Some(h.level),
            _ => None,
        })
        .min()
        .unwrap_or(4);
    for node in root.descendants() {
        let mut data = node.data.borrow_mut();
        match &mut data.value {
            NodeValue::Heading(h) => {
                h.level = (i16::from(h.level) + top - i16::from(lowest)).clamp(top, 6) as u8
            }
            NodeValue::Link(link) | NodeValue::Image(link) if !allowed_url(&link.url) => {
                link.url.clear()
            }
            _ => {}
        }
    }
    let mut html = String::new();
    comrak::format_html(root, &options, &mut html).expect("writing HTML to String is infallible");
    TrustedHtml::owned(html)
}

/// `text` as its lead and the rest: the lead is its first block (a heading takes the block
/// after it too), the rest everything after, or `None` when nothing follows. Each part is
/// rendered as `render` renders a whole text.
pub fn render_folded(text: &str) -> (TrustedHtml, Option<TrustedHtml>) {
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.table = true;
    let root = parse_document(&arena, text, &options);
    let mut blocks = root.children();
    let lead_end = match blocks.next() {
        None => return (render(text), None),
        Some(first) => {
            let heading = matches!(first.data.borrow().value, NodeValue::Heading(_));
            let last = if heading {
                blocks.next().unwrap_or(first)
            } else {
                first
            };
            last.data.borrow().sourcepos.end.line
        }
    };
    if blocks.next().is_none() {
        return (render(text), None);
    }
    let lines: Vec<&str> = text.lines().collect();
    let cut = lead_end.min(lines.len());
    let rest = lines[cut..].join("\n");
    if rest.trim().is_empty() {
        return (render(text), None);
    }
    (render(&lines[..cut].join("\n")), Some(render(&rest)))
}

/// A piece of markdown's words: plain text, or a code span's literal.
enum Piece {
    Text(String),
    Code(String),
}
/// Markdown as the words a reader sees, in order: what the parser takes for marks (emphasis,
/// headings, list bullets, link brackets) is gone, an in-word `_` or `*` stays, and a code
/// span keeps its literal. Whitespace collapses to single spaces.
fn pieces(text: &str) -> Vec<Piece> {
    fn push(out: &mut Vec<Piece>, piece: Piece) {
        let squeeze = |s: &str| {
            let lead = s.starts_with(char::is_whitespace);
            let trail = s.ends_with(char::is_whitespace);
            let mid = s.split_whitespace().collect::<Vec<_>>().join(" ");
            match (mid.is_empty(), lead, trail) {
                (true, _, _) if !s.is_empty() => " ".to_owned(),
                (true, _, _) => String::new(),
                _ => format!(
                    "{}{mid}{}",
                    if lead { " " } else { "" },
                    if trail { " " } else { "" }
                ),
            }
        };
        match piece {
            Piece::Text(t) => {
                let t = squeeze(&t);
                if t.is_empty() {
                    return;
                }
                if let Some(Piece::Text(last)) = out.last_mut() {
                    if !(last.ends_with(' ') && t.starts_with(' ')) {
                        last.push_str(&t);
                    } else {
                        last.push_str(&t[1..]);
                    }
                } else {
                    out.push(Piece::Text(t));
                }
            }
            Piece::Code(c) => {
                let c = c.split_whitespace().collect::<Vec<_>>().join(" ");
                if !c.is_empty() {
                    out.push(Piece::Code(c));
                }
            }
        }
    }
    fn walk<'a>(node: &'a comrak::nodes::AstNode<'a>, out: &mut Vec<Piece>) {
        let value = node.data.borrow().value.clone();
        match &value {
            NodeValue::Text(t) => push(out, Piece::Text(t.to_string())),
            NodeValue::Code(c) => push(out, Piece::Code(c.literal.clone())),
            NodeValue::SoftBreak | NodeValue::LineBreak => push(out, Piece::Text(" ".into())),
            NodeValue::HtmlInline(h) => push(out, Piece::Text(h.clone())),
            NodeValue::HtmlBlock(h) => push(out, Piece::Text(h.literal.clone())),
            NodeValue::CodeBlock(b) => push(out, Piece::Code(b.literal.clone())),
            _ => {}
        }
        for child in node.children() {
            walk(child, out);
        }
        if value.block() {
            push(out, Piece::Text(" ".into()));
        }
    }
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.table = true;
    let root = parse_document(&arena, text, &options);
    let mut out = Vec::new();
    walk(root, &mut out);
    // no space at either end
    if let Some(Piece::Text(first)) = out.first_mut() {
        *first = first.trim_start().to_owned();
    }
    if let Some(Piece::Text(last)) = out.last_mut() {
        *last = last.trim_end().to_owned();
    }
    out.retain(|p| !matches!(p, Piece::Text(t) if t.is_empty()));
    out
}
/// Markdown read as one line of plain words: its marks and line breaks dropped, its
/// identifiers (`a__b`, `snake_case`, a code span's literal) kept whole.
pub fn plain(text: &str) -> String {
    pieces(text)
        .into_iter()
        .map(|p| match p {
            Piece::Text(t) | Piece::Code(t) => t,
        })
        .collect()
}
/// Markdown as one line of inline HTML at most `most` characters long, cut at a word with an
/// ellipsis: its words as `plain` reads them, its code spans still code. The `bool` says
/// whether it was cut.
pub fn excerpt(text: &str, most: usize) -> (TrustedHtml, bool) {
    let pieces = pieces(text);
    let whole: String = pieces
        .iter()
        .map(|p| match p {
            Piece::Text(t) | Piece::Code(t) => t.as_str(),
        })
        .collect();
    let short = cut(&whole, most);
    let was_cut = short != whole;
    let mut budget = short.trim_end_matches('…').chars().count();
    let mut html = String::new();
    for piece in &pieces {
        if budget == 0 {
            break;
        }
        let (t, code) = match piece {
            Piece::Text(t) => (t, false),
            Piece::Code(t) => (t, true),
        };
        let take: String = t.chars().take(budget).collect();
        budget -= take.chars().count();
        let escaped = escape(&take);
        if code {
            html.push_str("<code>");
            html.push_str(&escaped);
            html.push_str("</code>");
        } else {
            html.push_str(&escaped);
        }
    }
    if was_cut {
        html.push('…');
    }
    (TrustedHtml::owned(html), was_cut)
}
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}
/// At most `most` characters, cut at a word, with an ellipsis when cut.
pub fn cut(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_owned();
    }
    let head: String = text.chars().take(most).collect();
    let at = head
        .rfind(' ')
        .filter(|&i| i > most / 2)
        .unwrap_or(head.len());
    format!(
        "{}…",
        head[..at].trim_end_matches([',', '.', ';', ':', ' '])
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_words_keep_identifiers_and_drop_only_marks() {
        assert_eq!(
            plain(
                "Red: `//crates/lash-core-store:lash-core-store__unit_test` and intent_hash_golden_vector **now**"
            ),
            "Red: //crates/lash-core-store:lash-core-store__unit_test and intent_hash_golden_vector now"
        );
        assert_eq!(
            plain("# Title\n\n- one *two*\n- a_b\n\n> said"),
            "Title one two a_b said"
        );
        let (html, cut) = excerpt(
            "Use `a<b>` then _see_ [docs](http://x) for snake_case.",
            200,
        );
        assert!(!cut);
        assert_eq!(
            html.to_string(),
            "Use <code>a&lt;b&gt;</code> then see docs for snake_case."
        );
        let (html, cut) = excerpt("word `code_span_here` more words after it", 12);
        assert!(cut);
        assert_eq!(html.to_string(), "word <code>code_sp</code>…");
    }
}
