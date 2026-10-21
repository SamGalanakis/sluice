//! A section head (DESIGN.md, Components: section head): a section's name at 18px, 600, its
//! count muted after it, a note, and its action at the right end. No rule over it.
use super::esc;
use crate::views::TrustedHtml;

/// A section's head: `name` (its `<h2>`, `id` its target and what the section is labelled
/// by), then its count, a note and its action, each left out when empty.
#[derive(Clone, Debug, Default)]
pub struct SectionHead {
    id: String,
    name: String,
    level: u8,
    /// Its name is HTML already (a part of it hidden on a narrow rail).
    raw: bool,
    count: String,
    note: String,
    action: String,
}
impl SectionHead {
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            level: 2,
            ..Self::default()
        }
    }
    /// A head whose name is HTML, already escaped ("Finished<span class=\"lk\"> since you
    /// looked</span>").
    pub fn new_html(id: &str, html: &str) -> Self {
        Self {
            raw: true,
            ..Self::new(id, html)
        }
    }
    /// Its heading's level (2 by default).
    pub fn level(mut self, level: u8) -> Self {
        self.level = level.clamp(2, 6);
        self
    }
    /// How many it holds, muted after its name; nothing at zero.
    pub fn count(mut self, n: usize) -> Self {
        self.count = if n > 0 { n.to_string() } else { String::new() };
        self
    }
    /// A short note after it ("since 14:02", "56 units").
    pub fn note(mut self, html: &TrustedHtml) -> Self {
        self.note = html.0.clone();
        self
    }
    /// Its one action, at the right end (a quiet button or link).
    pub fn action(mut self, html: &TrustedHtml) -> Self {
        self.action = html.0.clone();
        self
    }
    pub fn html(&self) -> TrustedHtml {
        let id = if self.id.is_empty() {
            String::new()
        } else {
            format!(" id=\"{}\"", esc(&self.id))
        };
        TrustedHtml::owned(format!(
            "<div class=\"sec-h\"><h{l}{id}>{}</h{l}>{}{}{}</div>",
            if self.raw {
                self.name.clone()
            } else {
                esc(&self.name)
            },
            if self.count.is_empty() {
                String::new()
            } else {
                format!("<span class=\"n\">{}</span>", self.count)
            },
            if self.note.is_empty() {
                String::new()
            } else {
                format!("<span class=\"sec-note\">{}</span>", self.note)
            },
            if self.action.is_empty() {
                String::new()
            } else {
                format!("<span class=\"grow\"></span>{}", self.action)
            },
            l = self.level,
        ))
    }
}
/// A section's head with a note after its name ("1 failed · 1 cancelled"), `id` on its heading.
pub fn section_head(id: &str, name: &str, line: &str) -> TrustedHtml {
    SectionHead::new(id, name)
        .note(&TrustedHtml::owned(esc(line)))
        .html()
}
