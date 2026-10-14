//! A pill (DESIGN.md, Components: pill): a small fact set apart, its state, a count. Round,
//! on the second paper; its tone a palette role: `Run` the blue (running), `Look` and `Quiet`
//! the sand (needs a look), `Ask` the coral (a question to the owner, coral's one place besides
//! the logo), `Done` the sky, `Muted` a hairline.
use super::esc;
use crate::views::TrustedHtml;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Plain,
    Run,
    Look,
    Quiet,
    Ask,
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
            Tone::Ask => "pill ask",
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
/// The coral "Awaiting your reply" that leads to an open question to the owner, where it is
/// answered. Its question is its title.
pub fn ask_link(href: &str, question: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<a class=\"pill ask\" href=\"{}\" title=\"{}\" aria-label=\"Awaiting your reply: {}\">Awaiting your reply</a>",
        esc(href),
        esc(question),
        esc(question)
    ))
}
