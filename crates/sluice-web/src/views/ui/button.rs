//! A button (DESIGN.md, Components: button): a pill. Outlined by default; `Primary` the one
//! next move, filled navy; `Danger` a delete; `Quiet` words with an underline (Dismiss, Undo,
//! Set aside); `Icon` a round glyph with no box (×, ⋯). A link that acts as one wears the same
//! classes (`.btn`).
use super::esc;
use crate::views::TrustedHtml;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Btn {
    #[default]
    Outlined,
    Primary,
    Danger,
    Quiet,
    Icon,
}
impl Btn {
    /// Its class on a `<button>` ("" for the outlined default).
    pub fn class(self) -> &'static str {
        match self {
            Btn::Outlined => "",
            Btn::Primary => "primary",
            Btn::Danger => "danger",
            Btn::Quiet => "quiet",
            Btn::Icon => "icon",
        }
    }
    /// Its class on a link (`<a class="btn …">`).
    pub fn link_class(self) -> &'static str {
        match self {
            Btn::Outlined => "btn",
            Btn::Primary => "btn primary",
            Btn::Danger => "btn danger",
            Btn::Quiet => "btn quiet",
            Btn::Icon => "btn icon",
        }
    }
}
/// A form's submit button: `name=value` posted with its form. `label` names it for a screen
/// reader (and its title) when its words are a glyph (an `Icon` button).
pub fn submit(kind: Btn, name: &str, value: &str, words: &TrustedHtml, label: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<button{} name=\"{}\" value=\"{}\"{}>{words}</button>",
        class_attr(kind.class()),
        esc(name),
        esc(value),
        label_attr(label)
    ))
}
/// A link that reads as a button.
pub fn link(kind: Btn, href: &str, words: &TrustedHtml, label: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<a class=\"{}\" href=\"{}\"{}>{words}</a>",
        kind.link_class(),
        esc(href),
        label_attr(label)
    ))
}
/// A button of `kind` (a form's submit unless `attrs` says `type="button"`): `words` HTML and
/// `attrs` more attributes, each with its leading space, already escaped.
pub fn button(kind: Btn, words: &str, attrs: &str) -> String {
    format!(
        "<button{}{attrs}>{words}</button>",
        class_attr(kind.class())
    )
}
/// A link of `kind` to `href` that reads as a button, with more attributes (`attrs`, each with
/// its leading space, already escaped).
pub fn link_with(kind: Btn, href: &str, words: &str, attrs: &str) -> String {
    format!(
        "<a class=\"{}\" href=\"{}\"{attrs}>{words}</a>",
        kind.link_class(),
        esc(href)
    )
}
fn class_attr(class: &str) -> String {
    if class.is_empty() {
        String::new()
    } else {
        format!(" class=\"{class}\"")
    }
}
fn label_attr(label: &str) -> String {
    if label.is_empty() {
        String::new()
    } else {
        format!(" aria-label=\"{}\" title=\"{}\"", esc(label), esc(label))
    }
}
