//! Surfaces (DESIGN.md, Components: message card): one message, who said it, when, its words
//! clamped and the way to read it whole, between hairlines.
use super::esc;
use crate::views::TrustedHtml;

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
