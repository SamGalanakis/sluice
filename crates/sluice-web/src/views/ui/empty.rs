//! An empty state and a notice (DESIGN.md, Components): what a place says when it has nothing
//! to show, and what a page says at its top after something asked for was not done.
use super::esc;
use crate::views::TrustedHtml;

/// What a place says when it has nothing to show: one plain sentence.
pub fn empty(text: &str) -> TrustedHtml {
    empty_with(text, &TrustedHtml::owned(String::new()))
}
/// The same with a way on after it (a link).
pub fn empty_with(text: &str, more: &TrustedHtml) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"empty-state\"><p>{}</p>{}</div>",
        esc(text),
        if more.as_str().is_empty() {
            String::new()
        } else {
            format!("<p class=\"es-more\">{more}</p>")
        }
    ))
}
/// What a page says at its top after something the owner asked for was not done: one calm box
/// with the attention mark, read out as it appears.
pub fn notice(words: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"notice\" role=\"alert\">{}<p>{}</p></div>",
        crate::views::icons::icon(crate::views::icons::Icon::TriangleAlert, 16, "notice-i"),
        esc(words)
    ))
}
