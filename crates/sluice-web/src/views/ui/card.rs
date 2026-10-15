//! Surfaces (DESIGN.md, Components: card, well, message card): a card is a hairline round its
//! content, a well the raised second paper where what needs the owner stands, and a message
//! card one message: who said it, when, its words clamped and the way to read it whole.
use super::esc;
use crate::views::TrustedHtml;

/// A well: the raised second paper, the place what needs the owner stands, as `tag`. `tone` ""
/// (plain) or "look" (on the pale sand: what needs a look); `padded` for one block of
/// content rather than a list of items.
pub fn well_open(tag: &str, class: &str, tone: &str, padded: bool) -> TrustedHtml {
    let mut classes = vec!["well"];
    if !tone.is_empty() {
        classes.push(tone);
    }
    if padded {
        classes.push("padded");
    }
    if !class.is_empty() {
        classes.push(class);
    }
    TrustedHtml::owned(format!("<{tag} class=\"{}\">", esc(&classes.join(" "))))
}
pub fn well_close(tag: &str) -> TrustedHtml {
    TrustedHtml::owned(format!("</{tag}>"))
}
/// One message as a card: `id` its anchor ("" for none), `who` (already drawn: its sender,
/// and to whom), `when` (a time already drawn), its words (`body`, already drawn, cut when
/// long) and `whole`, where it reads whole when it was cut.
pub fn message_card(
    id: &str,
    who: &TrustedHtml,
    when: &TrustedHtml,
    body: &TrustedHtml,
    whole: &str,
) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"msg-card\"{}><p class=\"mc-h\">{who}{}</p><div class=\"mc-b\">{body}</div>{}</div>",
        if id.is_empty() {
            String::new()
        } else {
            format!(" id=\"{}\"", esc(id))
        },
        if when.as_str().is_empty() {
            String::new()
        } else {
            format!("<span>{when}</span>")
        },
        if whole.is_empty() {
            String::new()
        } else {
            format!(
                "<p class=\"mc-more\"><a href=\"{}\">Read it whole</a></p>",
                esc(whole)
            )
        }
    ))
}
