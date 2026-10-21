//! The Needs-you list (DESIGN.md, Needs you): an item a hairline row, its mark, its line, what
//! it says, its form in place, its actions and its ×. The plan's Needs you and home's For you
//! draw it; a question to the owner is its line density (`ui::question`).
use super::{Shown, esc};

/// How an item is marked: a teal dot for a question to the owner, else its state (failed,
/// cancelled, stale, quiet), so a mark reads the same everywhere.
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
}
/// One item of a Needs-you list, drawn by `need_line` (HTML where it says so).
#[derive(Clone, Debug, Default)]
pub struct NeedLine<'a> {
    pub id: &'a str,
    /// Added to its `<li>`, already escaped (`data-unit`, `data-said`).
    pub attrs: &'a str,
    /// HTML: its title, the way to what it is about (`.nd-t`).
    pub title: &'a str,
    /// HTML: how it stands after its title ("asks you", "failed at land after 12m").
    pub state: &'a str,
    /// HTML: who and when, faint after that; "" for none.
    pub who: &'a str,
    /// HTML: what it says under its line; "" for none.
    pub body: &'a str,
    /// Its body is a question, asked by this id (its avatar beside it).
    pub asker: Option<&'a str>,
    /// HTML: what is folded under it (a group's units).
    pub more: &'a str,
    /// HTML: its form in place (an answer, a failure's feedback).
    pub form: &'a str,
    /// HTML: its actions, then its × (`x`).
    pub act: &'a str,
    pub x: &'a str,
}
/// One item, a hairline row (DESIGN.md, Needs you): its mark in a column of its own (a teal dot
/// a question, a red square a failure, ochre a stale input or a silence, muted a cancel), its
/// title, how it stands and who and when on one line, its actions and × at the right, what it
/// says under the line, then its form in place.
pub fn need_line(mark: Mark, line: &NeedLine<'_>) -> String {
    let class = match mark {
        Mark::Ask => "ask",
        Mark::State(Shown::Failed) => "fail",
        Mark::State(Shown::Cancelled) => "mute",
        Mark::State(Shown::Stale) => "stale",
        Mark::State(_) => "quiet",
    };
    let body = match (line.asker, line.body.is_empty()) {
        (_, true) => String::new(),
        (Some(from), false) => format!(
            "<div class=\"nd-q\">{}<div class=\"nd-qb\">{}</div></div>",
            super::avatar(from, super::AvSize::Md, false),
            line.body
        ),
        (None, false) => format!("<div class=\"nd-body\">{}</div>", line.body),
    };
    format!(
        "<li id=\"{id}\" class=\"nd-i {class}{fb}\"{attrs} tabindex=\"-1\" data-preserve-attr=\"class\"><span class=\"nd-m\"><i></i><span class=\"vh\">{word}</span></span><div class=\"nd-l\">{title} <span class=\"nd-s\">{state}</span>{who}</div><div class=\"nd-acts\">{act}{x}</div>{body}{more}{form}</li>",
        id = esc(line.id),
        fb = if line.form.contains("answer-in fb open") {
            " fb-open"
        } else {
            ""
        },
        attrs = line.attrs,
        word = match mark {
            Mark::Ask => "Question",
            Mark::State(s) => s.word(),
        },
        title = line.title,
        state = line.state,
        who = if line.who.is_empty() {
            String::new()
        } else {
            format!(" <span class=\"nd-who\">· {}</span>", line.who)
        },
        act = line.act,
        x = line.x,
        more = line.more,
        form = line.form,
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
