//! The drawn states (DESIGN.md, Empty states): what a place says when it has nothing to show, a
//! pen sketch in cenote's house hand over its one line and a muted sentence. Each stroke is a
//! path with its own window on the pen's clock (`--t0`, `--d`): the sketch draws on once when it
//! is shown, and stands finished under reduced motion. The SVGs are cenote's
//! (`assets/sketches/`, from its pen model), embedded and inlined.
use super::esc;
use crate::views::TrustedHtml;

/// A sketch, by what it stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sketch {
    /// An empty sheet: nothing needs the owner.
    Sheet,
    /// Ruled lines: nothing new has finished.
    Lines,
    /// A spiral at rest: nothing is at work.
    Spiral,
    /// A lens: a find with nothing found.
    Lens,
    /// A speech bubble: no messages.
    Bubble,
}
impl Sketch {
    fn svg(self) -> &'static str {
        match self {
            Sketch::Sheet => include_str!("../../../assets/sketches/empty-sheet.svg"),
            Sketch::Lines => include_str!("../../../assets/sketches/lines.svg"),
            Sketch::Spiral => include_str!("../../../assets/sketches/spiral.svg"),
            Sketch::Lens => include_str!("../../../assets/sketches/lens.svg"),
            Sketch::Bubble => include_str!("../../../assets/sketches/bubble.svg"),
        }
    }
}

/// A drawn state: `sketch` over `title` (its one line) and `body` (HTML, already escaped: a
/// muted sentence and its way on), `small` for a state that sits in a narrow place (a rail, a
/// line of a list).
pub fn state(sketch: Sketch, title: &str, body: &str, small: bool) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"state{}\"><div class=\"art\" aria-hidden=\"true\">{}</div><h3>{}</h3>{}</div>",
        if small { " small" } else { "" },
        sketch.svg().trim(),
        esc(title),
        if body.is_empty() {
            String::new()
        } else {
            format!("<p>{body}</p>")
        }
    ))
}
