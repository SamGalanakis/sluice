//! Display preferences (DESIGN.md, The frame): the menu behind the sliders at the nav bar's right
//! end. Its top level is short, one row each: Theme (the current theme's name and swatch, a "›"),
//! Show value types (a checkbox, toggled in place) and Keyboard shortcuts (a "›" and its `?`).
//! Theme and Keyboard shortcuts are each a sub-menu, a `<details class="sub">` whose panel opens
//! inside the same popover under a back control ("‹ Theme"), one level at a time (`sluice-menu`,
//! style.css). The theme and the value types are the page's `theme` and `types` signals
//! (`data-bind`), which `<html>` wears at once; a change posts them to `/settings`, which keeps
//! them in cookies for every page drawn after.
use super::icons::{Icon, icon};
use super::ui::{esc, keys_close, keys_open};
use super::{THEMES, TrustedHtml, Viewer};

/// Which level of the menu a gallery picture shows open (the page's own opens at the top).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Top,
    Theme,
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
/// its radios named apart by `prefix`), inert, in the theme `theme`.
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

/// A sub-menu's row and the back control at the head of its panel.
fn row(name: &str, rest: &str) -> String {
    format!(
        "<summary class=\"sub-row\"><span class=\"sub-name\">{name}</span>{rest}{}</summary>",
        icon(Icon::ChevronDown, 16, "chev sub-chev")
    )
}
fn back(name: &str) -> String {
    format!(
        "<button type=\"button\" class=\"sub-back\">{}{name}<span class=\"vh\">: back to display preferences</span></button>",
        icon(Icon::ChevronDown, 16, "chev sub-chev")
    )
}

fn draw(d: &Draw) -> String {
    let open = |level: Level| if d.open == level { " open" } else { "" };
    let radio = if d.page {
        "theme".to_owned()
    } else {
        format!("{}{:?}-theme", d.prefix, d.open).to_lowercase()
    };
    let chosen = d.theme.and_then(|id| THEMES.iter().find(|t| t.id == id));
    let themes: String = THEMES
        .iter()
        .map(|t| {
            format!(
                "<label><input type=\"radio\" name=\"{radio}\" value=\"{id}\"{checked}{bind}>{tick}<span>{name}</span><span class=\"swatch\" data-theme=\"{id}\" aria-hidden=\"true\"><i class=\"sw-run\"></i><i class=\"sw-ask\"></i></span></label>",
                id = t.id,
                checked = if d.theme == Some(t.id) { " checked" } else { "" },
                bind = if d.page { " data-bind:theme__root" } else { "" },
                tick = icon(Icon::Check, 16, "tick"),
                name = esc(t.name),
            )
        })
        .collect();
    // the current theme by name, its swatch in the page's own tokens (so it is right whatever a
    // stale stream patch says); before the first choice "Sluice", in the system's scheme
    let now = format!(
        "<span class=\"sub-now\"><span class=\"sub-theme\"{}>{}</span><span class=\"swatch\"{} aria-hidden=\"true\"><i class=\"sw-run\"></i><i class=\"sw-ask\"></i></span></span>",
        // the chosen theme's name, as the signal says it
        if d.page {
            let names: serde_json::Map<String, serde_json::Value> = THEMES
                .iter()
                .map(|t| (t.id.to_owned(), t.name.into()))
                .collect();
            format!(
                " data-text=\"{}\"",
                esc(&format!(
                    "({})[$theme] ?? 'Sluice'",
                    serde_json::Value::Object(names)
                ))
            )
        } else {
            String::new()
        },
        esc(chosen.map_or("Sluice", |t| t.name)),
        if d.page {
            String::new()
        } else {
            format!(" data-theme=\"{}\"", d.theme.unwrap_or_default())
        }
    );
    let theme = format!(
        "<details class=\"sub sub-themes\" data-preserve-attr=\"open\"{}>{}<div class=\"sub-panel\">{}<fieldset class=\"themes\"><legend class=\"vh\">Theme</legend>\n{themes}\n</fieldset></div></details>",
        open(Level::Theme),
        row("Theme", &now),
        back("Theme")
    );
    let types = format!(
        "<label class=\"check\"><input type=\"checkbox\" name=\"{}\" value=\"1\"{}{}>Show value types</label>",
        if d.page {
            "types".to_owned()
        } else {
            format!("{}{:?}-types", d.prefix, d.open).to_lowercase()
        },
        if d.types { " checked" } else { "" },
        if d.page { " data-bind:types__root" } else { "" },
    );
    let list = keys_list(d.keys);
    let keys = format!(
        "<details class=\"sub sub-keys\" data-preserve-attr=\"open\"{}>{}<div class=\"sub-panel\">{}{}</div></details>",
        open(Level::Keys),
        row("Keyboard shortcuts", "<kbd class=\"sub-hint\">?</kbd>"),
        back("Keyboard shortcuts"),
        if d.page {
            format!("{}{list}{}", keys_open(), keys_close())
        } else {
            list
        }
    );
    let guide = "<p class=\"prefs-guide\"><a href=\"/_ui#part-1\">States and parts</a><a href=\"/docs\">Agent docs</a></p>";
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
