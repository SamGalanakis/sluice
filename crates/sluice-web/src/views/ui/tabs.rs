//! Tabs (DESIGN.md, Components: tabs): one bar of soft pills, a tablist of buttons over its
//! panels (`sluice-tabs`, `tabs_open` … `tabs_close`) or links between views and pages
//! (`switch`).
use super::{Host, esc};
use crate::views::TrustedHtml;

/// One tab of a tab set: its key (the `?tab=` word and its panel's id), its label and a count
/// after it ("15", "3/7"), with the count's words for a screen reader ("3 of 7 set").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub key: &'static str,
    pub label: &'static str,
    pub count: String,
    pub said: String,
}
impl Tab {
    pub fn new(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            count: String::new(),
            said: String::new(),
        }
    }
    /// With a count after its label, and the words a screen reader hears for it.
    pub fn counted(mut self, count: impl Into<String>, said: impl Into<String>) -> Self {
        self.count = count.into();
        self.said = said.into();
        self
    }
}
/// A tab set's host (`sluice-tabs`) and its bar: an ARIA tablist, the tab `selected` chosen.
/// Its panels follow (`panel_open` … `panel_close`), then `tabs_close`. `prefix` keeps two tab
/// sets on one page apart. The chosen tab is the host's `current`, which a stream patch never
/// resets: the script owns it once the page is drawn.
pub fn tabs_open(prefix: &str, label: &str, tabs: &[Tab], selected: &str) -> TrustedHtml {
    let selected = tabs
        .iter()
        .find(|t| t.key == selected)
        .or(tabs.first())
        .map_or("", |t| t.key);
    let mut bar = String::new();
    for tab in tabs {
        let on = tab.key == selected;
        // a counted tab's name is one phrase, "Activity, 3 turns": its label in words, so a
        // screen reader never hears the count apart from it
        let (count, name) = if tab.count.is_empty() {
            (String::new(), String::new())
        } else if tab.said.is_empty() {
            (
                format!("<span class=\"n\">{}</span>", esc(&tab.count)),
                String::new(),
            )
        } else {
            (
                format!(
                    "<span class=\"n\" aria-hidden=\"true\">{}</span>",
                    esc(&tab.count)
                ),
                format!(" aria-label=\"{}, {}\"", esc(tab.label), esc(&tab.said)),
            )
        };
        bar.push_str(&format!(
            "<button type=\"button\" role=\"tab\" id=\"{p}tt-{k}\" aria-controls=\"{p}tp-{k}\" aria-selected=\"{on}\" tabindex=\"{ti}\" data-tab=\"{k}\" data-preserve-attr=\"aria-selected tabindex\"{name}>{label}{count}</button>",
            p = esc(prefix),
            k = tab.key,
            ti = if on { 0 } else { -1 },
            label = esc(tab.label),
        ));
    }
    let host = Host::new("sluice-tabs")
        .attr("id", format!("{prefix}tabs"))
        .attr("current", selected)
        .keep("current");
    let id = format!("{}tab-bar", esc(prefix));
    TrustedHtml::owned(format!(
        "{}<!--r:{id}--><div class=\"tabs\" role=\"tablist\" id=\"{id}\" aria-label=\"{label}\">{bar}</div><!--/r:{id}-->",
        host.open(),
        label = esc(label),
    ))
}
pub fn tabs_close() -> TrustedHtml {
    Host::new("sluice-tabs").close()
}
/// A tab's panel: a section under its own head (at `level`), the tab's words for a screen
/// reader. With `region`, a
/// stream patches the panel on its own (`<!--r:…-->`).
pub fn panel_open(
    prefix: &str,
    key: &str,
    label: &str,
    level: u8,
    selected: bool,
    region: bool,
) -> TrustedHtml {
    let level = level.clamp(1, 6);
    let id = format!("{}tp-{}", esc(prefix), esc(key));
    TrustedHtml::owned(format!(
        "{open}<section class=\"tp\" id=\"{id}\" role=\"tabpanel\" aria-labelledby=\"{p}tt-{k}\" data-tab=\"{k}\"{on} data-preserve-attr=\"data-chosen tabindex\"><h{level} class=\"tp-h\">{label}</h{level}>",
        open = if region {
            format!("<!--r:{id}-->")
        } else {
            String::new()
        },
        p = esc(prefix),
        k = esc(key),
        on = if selected { " data-chosen" } else { "" },
        label = esc(label),
    ))
}
pub fn panel_close(prefix: &str, key: &str, region: bool) -> TrustedHtml {
    TrustedHtml::owned(if region {
        format!("</section><!--/r:{}tp-{}-->", esc(prefix), esc(key))
    } else {
        "</section>".into()
    })
}
/// Links between views or pages as the same bar of pills (a project's sections, the plan's
/// Plan · Both · Board, the log's presets): each `(href, words, current)`, the current one
/// `aria-current`. `class` adds a hook a page's script finds it by.
pub fn switch(label: &str, class: &str, links: &[(String, String, bool)]) -> TrustedHtml {
    let items: String = links
        .iter()
        .map(|(href, words, on)| {
            format!(
                "<a href=\"{}\"{}>{}</a>",
                esc(href),
                if *on { " aria-current=\"page\"" } else { "" },
                esc(words)
            )
        })
        .collect();
    TrustedHtml::owned(format!(
        "<nav class=\"tabs{}\" aria-label=\"{}\">{items}</nav>",
        if class.is_empty() {
            String::new()
        } else {
            format!(" {}", esc(class))
        },
        esc(label)
    ))
}
