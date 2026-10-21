//! A question to the owner (DESIGN.md, Components: Question), drawn one way everywhere in two
//! densities: a line in a list (Needs you, For you: `ui::need_line` with `Mark::Ask`, its
//! Answer the way to the card) and a card where it is answered (a step, Questions, a thread, a
//! unit). Teal is its dot alone, never a stripe or a box; one filled Answer, one quiet Close
//! question; its answer form stays folded until Answer.
use super::{Btn, esc};
use crate::views::TrustedHtml;

/// A question as a card. Every part but the title is already drawn.
pub struct QuestionCard<'a> {
    /// Its element's id ("" for none).
    pub id: &'a str,
    /// The heading level of its title (3 under a section's head).
    pub level: u8,
    pub title: &'a str,
    /// Whether its title is drawn over its words: a question with no title of its own is
    /// said once, by its words (its title, their first line, names it to a screen reader).
    pub titled: bool,
    /// Where it comes from, muted after "Question for you": its project, who asked, when.
    pub context: TrustedHtml,
    /// Its ⋯ (what identifies it), "" for none.
    pub details: TrustedHtml,
    /// Its words, whole.
    pub body: TrustedHtml,
    /// Its answer area: `answer.html`'s Answer, Close question and folded form, or
    /// `answer_link`'s where the form lives elsewhere.
    pub answer: TrustedHtml,
    /// The way on (its step or its thread), "" for none.
    pub way_on: TrustedHtml,
}
impl QuestionCard<'_> {
    pub fn html(&self) -> TrustedHtml {
        TrustedHtml::owned(format!(
            "<article class=\"q-card\"{id} aria-label=\"Question for you: {label}\"><div class=\"q-k\"><span class=\"nk-ask\" aria-hidden=\"true\"></span><b>Question for you</b><span class=\"q-from\">{context}</span>{details}</div>{title}<div class=\"md q-body\">{body}</div>{answer}{way_on}</article>",
            id = if self.id.is_empty() {
                String::new()
            } else {
                format!(" id=\"{}\"", esc(self.id))
            },
            label = esc(self.title),
            title = if self.titled {
                format!(
                    "<h{l} class=\"q-t\">{}</h{l}>",
                    esc(self.title),
                    l = self.level.clamp(2, 4)
                )
            } else {
                String::new()
            },
            context = self.context,
            details = self.details,
            body = self.body,
            answer = self.answer,
            way_on = self.way_on,
        ))
    }
}
/// The answer area of a card whose form lives on its step: Answer, filled, the way there, and
/// Close question, quiet, posted to `reply_url`.
pub fn answer_link(answer_href: &str, reply_url: &str, title: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"q-actions\">{}{}</div>",
        super::link(
            Btn::Primary,
            answer_href,
            &TrustedHtml::owned("Answer".into()),
            ""
        ),
        close_form("", reply_url, title, ""),
    ))
}
/// A question's Close question, behind the page's confirmation: it is posted as its owner's
/// reply with the action `close`, which reaches whoever asked at once and cannot be taken
/// back (the store has no reopen), so it is confirmed rather than undone. `slot` tells apart
/// the cards one question has on a page (a step's Overview and Messages); `area` is the id of
/// the answer area the line that says so takes the place of ("" where the stream redraws it).
pub fn close_form(slot: &str, reply_url: &str, title: &str, area: &str) -> String {
    super::Confirm {
        opener: "Close question".into(),
        title: format!("Close “{}”?", super::cut(title, 80)),
        action: reply_url.into(),
        form_id: super::form_id(&format!("close {slot}{reply_url}")),
        hidden: vec![("action", "close".into())],
        copy: "Whoever asked is told you closed it, without an answer. It cannot be reopened."
            .into(),
        confirm: "Close question".into(),
        keep: "Keep it open",
        form_class: "q-close",
        form_attrs: if area.is_empty() {
            String::new()
        } else {
            format!(" data-closes=\"{}\"", esc(area))
        },
        ..Default::default()
    }
    .html()
    .0
}
/// The way on under a card: "Open its step" or "Open its thread".
pub fn way_on(href: &str, words: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<p class=\"q-on\"><a href=\"{}\">{}</a></p>",
        esc(href),
        esc(words)
    ))
}
