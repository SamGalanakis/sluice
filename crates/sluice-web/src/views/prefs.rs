//! Display preferences (DESIGN.md, The frame): the menu behind the sliders at the nav bar's right
//! end. Its top level is short, one row each: Theme (the current theme's name and swatch, a "›"),
//! Show value types (a checkbox, toggled in place) and Keyboard shortcuts (a "›" and its `?`).
//! Theme and Keyboard shortcuts are each a sub-menu, a `<details class="sub">` whose panel opens
//! inside the same popover under a back control ("‹ Theme"); with script one level shows at a time
//! (`sluice-menu`, style.css), without it a row opens its panel in place under it, so every theme
//! and the value types stay a plain form with Save.
use super::icons::{Icon, icon};
use super::ui::{esc, keys_close, keys_open, setting_close, setting_open};
use super::{THEMES, TrustedHtml, Viewer};

/// Which level of the menu a gallery picture shows open (the page's own opens at the top).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Top,
    Theme,
    Keys,
}

/// The page's own menu panel (`.menu`), drawn from the viewer's cookies: `path` is where Save
/// comes back to without script, `keys` the page's `g` keys (`NavView::keys`).
pub fn menu(viewer: &Viewer, path: &str, keys: &[(char, String, String)]) -> TrustedHtml {
    TrustedHtml::owned(draw(&Draw {
        theme: viewer.theme,
        types: viewer.types,
        path,
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
        path: "",
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
    path: &'a str,
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
        "<button type=\"button\" class=\"sub-back needs-js\">{}{name}<span class=\"vh\">: back to display preferences</span></button>",
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
                "<label><input type=\"radio\" name=\"{radio}\" value=\"{id}\"{checked}>{tick}<span>{name}</span><span class=\"swatch\" data-theme=\"{id}\" aria-hidden=\"true\"><i class=\"sw-run\"></i><i class=\"sw-ask\"></i></span></label>",
                id = t.id,
                checked = if d.theme == Some(t.id) { " checked" } else { "" },
                tick = icon(Icon::Check, 16, "tick"),
                name = esc(t.name),
            )
        })
        .collect();
    // the current theme by name, its swatch in the page's own tokens (so it is right whatever a
    // stale stream patch says); before the first choice "Sluice", in the system's scheme
    let now = format!(
        "<span class=\"sub-now\"><span class=\"sub-theme\">{}</span><span class=\"swatch\"{} aria-hidden=\"true\"><i class=\"sw-run\"></i><i class=\"sw-ask\"></i></span></span>",
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
        "<label class=\"check\"><input type=\"checkbox\" name=\"{}\" value=\"1\"{}>Show value types</label>",
        if d.page {
            "types".to_owned()
        } else {
            format!("{}{:?}-types", d.prefix, d.open).to_lowercase()
        },
        if d.types { " checked" } else { "" }
    );
    let list = keys_list(d.keys);
    let keys = format!(
        "<details class=\"sub sub-keys needs-js\" data-preserve-attr=\"open\"{}>{}<div class=\"sub-panel\">{}{}</div></details>",
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
            "<div class=\"menu prefs-menu\"><form class=\"prefs\" method=\"post\" action=\"/settings\" aria-label=\"Display preferences\"><input type=\"hidden\" name=\"next\" value=\"{}\">{}{theme}{}<input type=\"hidden\" name=\"types\" value=\"0\">{}{types}{}<button class=\"save\" type=\"submit\">Save</button></form>{keys}{guide}</div>",
            esc(d.path),
            setting_open("theme"),
            setting_close(),
            setting_open("types"),
            setting_close(),
        )
    } else {
        format!(
            "<div class=\"menu prefs-menu gal-prefs\" inert><div class=\"prefs\">{theme}{types}</div>{keys}{guide}</div>"
        )
    }
}
