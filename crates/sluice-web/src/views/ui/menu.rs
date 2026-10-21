//! The menu (DESIGN.md, Components: menu, Details): one "⋯" at an item's end, a `<details>`
//! inside a `sluice-menu`, holding what identifies the item and what
//! can be done to it that is no next move.
use super::esc;
use crate::views::TrustedHtml;
use crate::views::icons::{Icon, icon};

// ---- details: what identifies an item, behind its "⋯" ------------------------------------------

/// What identifies an item rather than explains it (its ids, run, fn, engine, tags, recipe,
/// hashes and paths), kept behind one "⋯" control at the item's end (`Details::menu`): a
/// `<details>` inside a `sluice-menu` (Escape and a click elsewhere
/// close it, the focus back on its button). Each id is shown whole with a copy button.
#[derive(Clone, Debug, Default)]
pub struct Details {
    rows: Vec<(String, String)>,
    /// What can be done to the item that is no next move (a failure's Cancel, set aside):
    /// drawn under its rows, under a head of their own (`acts`).
    actions: Vec<String>,
    acts: &'static str,
}
impl Details {
    pub fn new() -> Self {
        Self::default()
    }
    /// An id, hash or path: whole, in data mono, with a copy button. Left out when empty.
    pub fn id(mut self, name: &str, value: &str) -> Self {
        if !value.is_empty() {
            self.rows.push((
                name.to_owned(),
                super::copy(value, &format!("Copy {}", name.to_lowercase())).0,
            ));
        }
        self
    }
    /// Words or a short value. Left out when empty.
    pub fn text(mut self, name: &str, value: &str) -> Self {
        if !value.is_empty() {
            self.rows
                .push((name.to_owned(), format!("<span>{}</span>", esc(value))));
        }
        self
    }
    /// A value in data mono with no copy (a fn's name, a tag). Left out when empty.
    pub fn code(mut self, name: &str, value: &str) -> Self {
        if !value.is_empty() {
            self.rows
                .push((name.to_owned(), format!("<code>{}</code>", esc(value))));
        }
        self
    }
    /// Values already drawn. Left out when empty.
    pub fn html(mut self, name: &str, value: &TrustedHtml) -> Self {
        if !value.as_str().is_empty() {
            self.rows.push((name.to_owned(), value.0.clone()));
        }
        self
    }
    /// A link onward.
    pub fn link(mut self, name: &str, href: &str, text: &str) -> Self {
        if !href.is_empty() {
            self.rows.push((
                name.to_owned(),
                format!("<a href=\"{}\">{}</a>", esc(href), esc(text)),
            ));
        }
        self
    }
    /// Actions under the rows, headed `head` ("Set it aside"): each already drawn as
    /// a form or link. Left out when there are none.
    pub fn actions(mut self, head: &'static str, actions: Vec<TrustedHtml>) -> Self {
        self.acts = head;
        self.actions = actions.into_iter().map(|a| a.0).collect();
        self
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.actions.is_empty()
    }
    /// Its rows in place, under a quiet fold named by what they are (`summary`, "2 ids"),
    /// opening in place: what identifies a list of values, kept out of the way but named.
    pub fn fold(&self, summary: &str) -> TrustedHtml {
        if self.rows.is_empty() {
            return TrustedHtml::default();
        }
        let rows: String = self
            .rows
            .iter()
            .map(|(name, value)| format!("<div><dt>{}</dt><dd>{value}</dd></div>", esc(name)))
            .collect();
        TrustedHtml::owned(format!(
            "<details class=\"qf held\" data-preserve-attr=\"open\"><summary>{}</summary><dl class=\"dm-l\">{rows}</dl></details>",
            esc(summary)
        ))
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    /// The "⋯" control for the item `about` ("Details of a-12"): a 44px button on a phone that
    /// opens the Details panel, nothing when there is nothing to hold.
    pub fn menu(&self, about: &str) -> TrustedHtml {
        if self.is_empty() {
            return TrustedHtml::default();
        }
        let rows: String = self
            .rows
            .iter()
            .map(|(name, value)| format!("<div><dt>{}</dt><dd>{value}</dd></div>", esc(name)))
            .collect();
        let rows = if self.actions.is_empty() {
            format!("<dl class=\"dm-l\">{rows}</dl>")
        } else {
            format!(
                "<dl class=\"dm-l\">{rows}</dl><div class=\"dm-acts\" role=\"group\" aria-label=\"{h}\"><p class=\"dm-h\">{h}</p>{}</div>",
                self.actions.concat(),
                h = esc(self.acts),
            )
        };
        TrustedHtml::owned(format!(
            "{}<details class=\"dm\" data-preserve-attr=\"open\"><summary class=\"dm-b\" aria-label=\"Details of {a}\" title=\"Details\" data-preserve-attr=\"aria-expanded\">{}</summary><div class=\"menu dm-p\" role=\"group\" aria-label=\"Details of {a}\" data-preserve-attr=\"style\"><p class=\"dm-h\">Details</p>{rows}</div></details>{}",
            super::menu_open(),
            icon(Icon::Ellipsis, 18, ""),
            super::menu_close(),
            a = esc(about),
        ))
    }
}
