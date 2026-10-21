//! A badge (DESIGN.md, Components: Badge): a small fact set apart, its state, a count; a 20px
//! pill. Its tone is a meaning: `Run` green (running), `Look` ochre (needs a look), `Fail` red,
//! `Done` settled, `Muted` a hairline, `Plain` the active overlay. A state's badge is
//! `status_pill`, its tone the status table's for it. A question to the owner is no badge: its
//! teal dot (`ask_link`, `ui::QuestionCard`).
use super::esc;
use crate::views::TrustedHtml;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Plain,
    Run,
    Look,
    Fail,
    Done,
    Muted,
}
impl Tone {
    pub fn class(self) -> &'static str {
        match self {
            Tone::Plain => "badge",
            Tone::Run => "badge running",
            Tone::Look => "badge look",
            Tone::Fail => "badge fail",
            Tone::Done => "badge done",
            Tone::Muted => "badge muted",
        }
    }
}
/// A pill: a glyph before its words when given.
pub fn pill(text: &str, tone: Tone, mark: Option<TrustedHtml>) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<span class=\"{}\">{}{}</span>",
        tone.class(),
        mark.map(|m| m.0).unwrap_or_default(),
        esc(text)
    ))
}
/// A state as a pill, one way on every page (DESIGN.md, Status presentation): its glyph, then
/// `words` (HTML, already escaped: its word, its stage, its time), in its meaning's tone: green
/// running, red failed, ochre needing a look, settled done, plain waiting.
pub fn status_pill(shown: super::Shown, words: &str) -> TrustedHtml {
    let tone = match super::Cell::of(Some(shown)) {
        super::Cell::Run => Tone::Run,
        super::Cell::Look if shown == super::Shown::Failed => Tone::Fail,
        super::Cell::Look if shown == super::Shown::Cancelled => Tone::Muted,
        super::Cell::Look => Tone::Look,
        super::Cell::Done => Tone::Done,
        super::Cell::Empty => Tone::Plain,
    };
    TrustedHtml::owned(format!(
        "<span class=\"{}\">{}{words}</span>",
        tone.class(),
        super::mark(shown)
    ))
}
/// A pill that leads somewhere.
pub fn pill_link(href: &str, text: &str, tone: Tone, mark: Option<TrustedHtml>) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<a class=\"{}\" href=\"{}\">{}{}</a>",
        tone.class(),
        esc(href),
        mark.map(|m| m.0).unwrap_or_default(),
        esc(text)
    ))
}
/// The way to an open question to the owner, where it is answered: its teal dot and "Waiting
/// on your answer". Its question is its title.
pub fn ask_link(href: &str, question: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<a class=\"q-wait\" href=\"{}\" title=\"{}\" aria-label=\"Waiting on your answer: {}\"><span class=\"nk-ask\" aria-hidden=\"true\"></span>Waiting on your answer</a>",
        esc(href),
        esc(question),
        esc(question)
    ))
}
