//! A pill (DESIGN.md, Components: pill): a small fact set apart, its state, a count. Round,
//! on the second paper; its tone a palette role: `Run` the blue (running), `Look` and `Quiet`
//! the sand (needs a look), `Done` the sky, `Muted` a hairline. A question to the owner is no
//! pill: its coral dot (`ask_link`, `ui::QuestionCard`).
use super::esc;
use crate::views::TrustedHtml;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Plain,
    Run,
    Look,
    Quiet,
    Done,
    Muted,
}
impl Tone {
    pub fn class(self) -> &'static str {
        match self {
            Tone::Plain => "pill",
            Tone::Run => "pill run",
            Tone::Look => "pill look",
            Tone::Quiet => "pill quiet",
            Tone::Done => "pill done",
            Tone::Muted => "pill muted",
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
/// The way to an open question to the owner, where it is answered: its coral dot and "Waiting
/// on your answer". Its question is its title.
pub fn ask_link(href: &str, question: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<a class=\"q-wait\" href=\"{}\" title=\"{}\" aria-label=\"Waiting on your answer: {}\"><span class=\"nk-ask\" aria-hidden=\"true\"></span>Waiting on your answer</a>",
        esc(href),
        esc(question),
        esc(question)
    ))
}
