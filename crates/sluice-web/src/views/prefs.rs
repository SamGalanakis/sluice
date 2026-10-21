//! Display preferences (DESIGN.md, The frame): the menu behind the "…" at the sidebar's foot,
//! beside the owner. Its top level is short, one row each: Theme (Paper or Carbon, a radio pair
//! in place), Show value types (a checkbox) and Keyboard shortcuts (a "›" and its `?`), a
//! sub-menu whose panel opens inside the same popover under a back control ("‹ Keyboard
//! shortcuts", `sluice-menu`). The theme and the value types are the page's `theme` and `types`
//! signals (`data-bind`), which `<html>` wears at once; a change posts them to `/settings`,
//! which keeps them in cookies for every page drawn after.
use super::icons::{Icon, icon};
use super::ui::{esc, keys_close, keys_open};
use super::{THEMES, TrustedHtml, Viewer};

/// Which level of the menu a gallery picture shows open (the page's own opens at the top).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Top,
    Keys,
}

/// The page's own menu panel (`.menu`), drawn from the viewer's cookies: `keys` the page's `g`
/// keys (`NavView::keys`).
pub fn menu(viewer: &Viewer, keys: &[(char, String, String)]) -> TrustedHtml {
    TrustedHtml::owned(draw(&Draw {
        theme: viewer.theme,
        types: viewer.types,
        keys,
        open: Level::Top,
        page: true,
        prefix: "",
    }))
}

/// A picture of the menu at `open` for the gallery: not the page's (no form, no components,
/// its radios named apart by `prefix`), inert, with `theme` chosen.
pub fn picture(prefix: &str, theme: &'static str, open: Level) -> TrustedHtml {
    let keys = sample_keys();
    TrustedHtml::owned(draw(&Draw {
        theme: Some(theme),
        types: false,
        keys: &keys,
        open,
        page: false,
        prefix,
    }))
}

/// The keys' list with the gallery's sections, as the Keyboard shortcuts panel holds it.
pub fn keys_picture() -> TrustedHtml {
    TrustedHtml::owned(keys_list(&sample_keys()))
}
fn sample_keys() -> [(char, String, String); 5] {
    [
        ('h', "All projects".to_owned(), "#".to_owned()),
        ('p', "Plan".to_owned(), "#".to_owned()),
        ('m', "Messages".to_owned(), "#".to_owned()),
        ('l', "Log".to_owned(), "#".to_owned()),
        ('i', "Inbox".to_owned(), "#".to_owned()),
    ]
}

/// The keys' list, as the Keyboard shortcuts panel holds it.
fn keys_list(keys: &[(char, String, String)]) -> String {
    let go: String = keys
        .iter()
        .map(|(k, label, href)| {
            format!(
                "<div><dt><kbd>g</kbd> <kbd>{k}</kbd></dt><dd><a href=\"{}\" data-go=\"{k}\">{}</a></dd></div>",
                esc(href),
                esc(label)
            )
        })
        .collect();
    format!(
        "<div class=\"keys\"><dl class=\"keys-list\"><div><dt><kbd>/</kbd></dt><dd>Find</dd></div><div><dt><kbd>[</kbd> <kbd>]</kbd></dt><dd>Previous and next step</dd></div>{go}<div><dt><kbd>?</kbd></dt><dd>These keys</dd></div></dl></div>"
    )
}

struct Draw<'a> {
    theme: Option<&'static str>,
    types: bool,
    keys: &'a [(char, String, String)],
    open: Level,
    /// The page's own (a form with its components) or a gallery picture.
    page: bool,
    /// A picture's prefix for its radios' and checkbox's names.
    prefix: &'a str,
}

fn draw(d: &Draw) -> String {
    let name = |what: &str| {
        if d.page {
            what.to_owned()
        } else {
            format!("{}{:?}-{what}", d.prefix, d.open).to_lowercase()
        }
    };
    // Paper or Carbon, side by side; before the first choice neither is checked (the page
    // keeps the system's at once, `layout.html`)
    let themes: String = THEMES
        .iter()
        .map(|t| {
            format!(
                "<label><input type=\"radio\" name=\"{}\" value=\"{id}\"{checked}{bind}><span>{name}</span></label>",
                name("theme"),
                id = t.id,
                checked = if d.theme == Some(t.id) { " checked" } else { "" },
                bind = if d.page { " data-bind:theme__root" } else { "" },
                name = esc(t.name),
            )
        })
        .collect();
    let theme = format!(
        "<fieldset class=\"pref-theme\"><legend>Theme</legend><div class=\"seg\">{themes}</div></fieldset>"
    );
    let types = format!(
        "<label class=\"check\"><input type=\"checkbox\" name=\"{}\" value=\"1\"{}{}>Show value types</label>",
        name("types"),
        if d.types { " checked" } else { "" },
        if d.page { " data-bind:types__root" } else { "" },
    );
    let list = keys_list(d.keys);
    let keys = format!(
        "<details class=\"sub sub-keys\" data-preserve-attr=\"open\"{}><summary class=\"sub-row\"><span class=\"sub-name\">Keyboard shortcuts</span><kbd class=\"sub-hint\">?</kbd>{}</summary><div class=\"sub-panel\"><button type=\"button\" class=\"sub-back\">{}Keyboard shortcuts<span class=\"vh\">: back to display preferences</span></button>{}</div></details>",
        if d.open == Level::Keys { " open" } else { "" },
        icon(Icon::ChevronRight, 16, "chev"),
        icon(Icon::ArrowLeft, 16, "chev"),
        if d.page {
            format!("{}{list}{}", keys_open(), keys_close())
        } else {
            list
        }
    );
    let guide = "<p class=\"prefs-guide\"><a href=\"/_ui\">States and parts</a><a href=\"/docs\">Agent docs</a></p>";
    if d.page {
        format!(
            "<div class=\"menu prefs-menu\"><div class=\"prefs\" role=\"group\" aria-label=\"Display preferences\" data-on:change=\"@post('/settings')\">{theme}{types}</div>{keys}{guide}</div>"
        )
    } else {
        format!(
            "<div class=\"menu prefs-menu gal-prefs\" inert><div class=\"prefs\">{theme}{types}</div>{keys}{guide}</div>"
        )
    }
}
