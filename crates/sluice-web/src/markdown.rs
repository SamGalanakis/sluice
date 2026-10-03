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
                h.level = (i16::from(h.level) + 4 - i16::from(lowest)).clamp(4, 6) as u8
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
