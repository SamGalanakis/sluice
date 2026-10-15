//! A question to the owner (DESIGN.md, Components: Question), drawn one way everywhere in two
//! densities: a line in a list (Needs you, For you: `ui::need_line` with `Mark::Ask`, its
//! Answer the way to the card) and a card where it is answered (a step, Questions, a thread, a
//! unit). Coral is its dot alone, never a stripe or a box; one filled Answer, one quiet Close
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
            "<article class=\"q-card\"{id} aria-label=\"Question for you: {label}\"><p class=\"q-k\"><span class=\"nk-ask\" aria-hidden=\"true\"></span><b>Question for you</b><span class=\"q-from\">{context}</span>{details}</p>{title}<div class=\"md q-body\">{body}</div>{answer}{way_on}</article>",
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
        "<div class=\"q-actions\">{}<form class=\"q-close\" method=\"post\" action=\"{}\"><input type=\"hidden\" name=\"action\" value=\"close\"><button class=\"{}\" aria-label=\"Close question: {}\">Close question</button></form></div>",
        super::link(
            Btn::Primary,
            answer_href,
            &TrustedHtml::owned("Answer".into()),
            ""
        ),
        esc(reply_url),
        Btn::Quiet.class(),
        esc(title),
    ))
}
/// The way on under a card: "Open its step" or "Open its thread".
pub fn way_on(href: &str, words: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<p class=\"q-on\"><a href=\"{}\">{}</a></p>",
        esc(href),
        esc(words)
    ))
}
