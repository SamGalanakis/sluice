//! The Needs-you list (DESIGN.md, Components: Needs you): an item a line, its mark, its
//! sentence (and what is folded under it), its one action and its ×. The plan's Needs you and
//! home's For you draw it; a question to the owner is its line density (`ui::question`).
use super::{Shown, esc, glyph};

/// How an item is marked: a coral dot for a question to the owner, else its state's glyph from
/// the status table (failed, cancelled, stale, quiet), so a mark reads the same everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Ask,
    State(Shown),
}
impl Mark {
    /// The state it stands for (none for a question).
    pub fn shown(self) -> Option<Shown> {
        match self {
            Mark::Ask => None,
            Mark::State(shown) => Some(shown),
        }
    }
    pub fn html(self) -> String {
        match self {
            Mark::Ask => {
                "<span class=\"nk nk-ask\" role=\"img\" aria-label=\"Question\"></span>".into()
            }
            Mark::State(shown) => format!("<span class=\"nk\">{}</span>", glyph(shown)),
        }
    }
}
/// One item: `attrs` added to its `<li>` (already escaped), `sentence` and `more` HTML, `act`
/// its one action and `x` its set-aside control, both HTML.
pub fn need_line(
    mark: Mark,
    id: &str,
    attrs: &str,
    sentence: &str,
    more: &str,
    act: &str,
    x: &str,
) -> String {
    format!(
        "<li id=\"{id}\" class=\"nd-i\"{attrs} tabindex=\"-1\">{mk}<div class=\"nd-say\"><p>{sentence}</p>{more}</div><span class=\"nd-acts\">{act}{x}</span></li>",
        id = esc(id),
        mk = mark.html(),
    )
}
/// An item settled where it stood (a question answered or closed in the last ten minutes): its
/// one line across the whole row in the item's place, so the list keeps its order and the line
/// its width.
pub fn need_said(id: &str, line: &str) -> String {
    format!(
        "<li id=\"{id}\" class=\"nd-i nd-said\">{line}</li>",
        id = esc(id)
    )
}
