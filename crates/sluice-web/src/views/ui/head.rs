//! The page head and the status line (DESIGN.md, Components: page head, status line): a page's
//! name in the title style (the text face, 22px at 650), the one sentence under it, what stands
//! beside it (its "⋯") and under it; and one muted sentence about a thing's state and time,
//! its mark first.
use super::esc;
use crate::views::TrustedHtml;

/// The page's head on the paper (`Frame::head`), under the slim nav bar: the page's name as its
/// heading at a reading size (the project's name on its plan, the page's elsewhere), and on the
/// plan and home only the summary sentence under it, one line of large body text.
pub fn page_head(name: &str, line: &TrustedHtml) -> TrustedHtml {
    page_head_html(&TrustedHtml::owned(esc(name)), line)
}
/// The page's head with its heading already drawn, the summary sentence under it.
pub fn page_head_html(name: &TrustedHtml, line: &TrustedHtml) -> TrustedHtml {
    page_head_with(
        &TrustedHtml::default(),
        name,
        &TrustedHtml::default(),
        &if line.as_str().is_empty() {
            TrustedHtml::default()
        } else {
            TrustedHtml::owned(format!("<p class=\"page-line\">{line}</p>"))
        },
    )
}
/// The page's head with a muted note under its name instead of a summary sentence: what a
/// page shows ("42 records on this page"), or why it cannot be drawn.
pub fn page_head_note(name: &str, note: &TrustedHtml) -> TrustedHtml {
    page_head_with(
        &TrustedHtml::default(),
        &TrustedHtml::owned(esc(name)),
        &TrustedHtml::default(),
        &if note.as_str().is_empty() {
            TrustedHtml::default()
        } else {
            TrustedHtml::owned(format!("<p class=\"page-note\">{note}</p>"))
        },
    )
}
/// The page's head whole: its way back (`crumbs`, a breadcrumb nav) over its name, what stands
/// beside the name (`beside`: its Details' "⋯"), and what goes under it (`under`: its lines,
/// its strip, its actions).
pub fn page_head_with(
    crumbs: &TrustedHtml,
    name: &TrustedHtml,
    beside: &TrustedHtml,
    under: &TrustedHtml,
) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"page-head\">{crumbs}<div class=\"ph-name\"><h1>{name}</h1>{beside}</div>{under}</div>"
    ))
}

/// A status line's mark: its state's glyph, as the status table draws it everywhere, or the
/// coral dot when it asks the owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dot {
    None,
    Ask,
    State(super::Shown),
}
impl Dot {
    fn html(self) -> String {
        match self {
            Dot::None => String::new(),
            Dot::Ask => "<span class=\"nk-ask\" aria-hidden=\"true\"></span>".into(),
            // the line's first part says the state in words
            Dot::State(shown) => super::mark(shown).as_str().to_owned(),
        }
    }
}
/// One muted sentence about a thing's state and time ("Running 6h 34m · heard 1h 18m ago ·
/// run 1"): its mark, then its parts (each already drawn) apart by middle dots.
pub fn status_line(dot: Dot, parts: &[TrustedHtml]) -> TrustedHtml {
    let parts: Vec<&str> = parts
        .iter()
        .map(|p| p.as_str())
        .filter(|p| !p.is_empty())
        .collect();
    TrustedHtml::owned(format!(
        "<p class=\"status-line\">{}{}</p>",
        dot.html(),
        parts
            .iter()
            .map(|p| format!("<span class=\"sl-p\">{p}</span>"))
            .collect::<Vec<_>>()
            .join(""),
    ))
}
