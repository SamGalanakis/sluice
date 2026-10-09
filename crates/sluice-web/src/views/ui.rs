//! The dashboard's shared components: every page draws a status, a time, a duration, a count,
//! a section head, a field row, a step reference and a tag through these, so they read one way
//! everywhere (DESIGN.md, Components). Each returns escaped HTML (or plain text for words).
use super::TrustedHtml;
use super::icons::{Icon, solid};

mod rocket;
pub use rocket::*;

/// Text escaped for HTML content and attribute values.
pub fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

// ---- status ----------------------------------------------------------------------------------

pub use sluice_model::shown::{Shown, Tally};
/// The stored status a done count counts (`Tally::stored`), for a template.
pub const SUCCEEDED: &sluice_model::commands::StepStatus =
    &sluice_model::commands::StepStatus::Succeeded;

/// A state's glyph (the Shape Carries It Rule): its Lucide icon from the status table, named for
/// a screen reader by the state's word.
pub fn glyph(shown: Shown) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<span class=\"g g-{}\" role=\"img\" aria-label=\"{}\">{}</span>",
        shown.key(),
        esc(shown.word()),
        shape(shown)
    ))
}
/// A state's glyph beside words that already say it: hidden from a screen reader.
pub fn mark(shown: Shown) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<span class=\"g g-{}\" aria-hidden=\"true\">{}</span>",
        shown.key(),
        shape(shown)
    ))
}
fn shape(shown: Shown) -> TrustedHtml {
    let spec = shown.spec();
    let icon = Icon::named(spec.icon).expect("every state's icon is in the sprite");
    solid(icon, 16, if spec.turns { "spin" } else { "" })
}
/// A state's word ("set by hand", "outside").
pub fn word(shown: Shown) -> &'static str {
    shown.word()
}
/// A state as its glyph and its word (the word hidden from a screen reader, which hears the
/// glyph's name).
pub fn status(shown: Shown) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<span class=\"st st-{}\">{}<span class=\"st-w\" aria-hidden=\"true\">{}</span></span>",
        shown.key(),
        glyph(shown),
        esc(shown.word())
    ))
}
/// Each state counted and its word, in priority order, the states with none left out
/// ("1 failed · 4 running · 1 paused" once joined by `tally`).
pub fn states(tally: &Tally) -> Vec<(usize, &'static str)> {
    tally.iter().map(|(s, n)| (n, s.word())).collect()
}
/// "1 running · 4 pending": every state counted, in priority order.
pub fn states_words(tally: &Tally) -> String {
    states(tally)
        .iter()
        .map(|(n, w)| format!("{n} {w}"))
        .collect::<Vec<_>>()
        .join(" · ")
}
/// Each state counted as a summary reads them: the done states first (the work so far), then
/// the rest in priority order.
pub fn progress(tally: &Tally) -> Vec<(Shown, usize)> {
    let mut order: Vec<(Shown, usize)> = tally.iter().collect();
    order.sort_by_key(|(s, _)| (s.spec().band != sluice_model::shown::Band::Done, s.rank()));
    order
}
/// Steps counted as a bar, one segment a state, in `progress` order; named for a screen reader
/// by every count.
pub fn bar(tally: &Tally, class: &str) -> TrustedHtml {
    let order = progress(tally);
    let said = order
        .iter()
        .map(|(s, n)| format!("{n} {}", s.word()))
        .collect::<Vec<_>>()
        .join(", ");
    let segments: String = order
        .iter()
        .map(|(s, n)| format!("<i class=\"b-{}\" style=\"flex:{n}\"></i>", s.key()))
        .collect();
    TrustedHtml::owned(format!(
        "<span class=\"bar{}\" role=\"img\" aria-label=\"{}: {said}\">{segments}</span>",
        if class.is_empty() {
            String::new()
        } else {
            format!(" {class}")
        },
        esc(&count(tally.total(), "step", "steps"))
    ))
}

// ---- time and duration -----------------------------------------------------------------------

/// Whole seconds of a duration, rounded to the millisecond first: durations come from julian-day
/// differences, so 38 minutes reads 2279.99998 seconds and must not floor to 37m.
fn whole_seconds(seconds: f64) -> u64 {
    ((seconds.max(0.0) * 1000.0).round() / 1000.0).floor() as u64
}
/// A stored time as every page draws it before its script reads it: "2026-10-07 20:47 UTC".
pub fn at_text(at: &str) -> String {
    match (at.get(..10), at.get(10..11), at.get(11..16)) {
        (Some(day), Some("T"), Some(time)) => format!("{day} {time} UTC"),
        _ => at.to_owned(),
    }
}
/// A past time: "12m ago", "3d 12h ago" once the page's script reads it; the UTC day and minute
/// before (and in its title, always).
pub fn ago(at: &str) -> TrustedHtml {
    time(at, "data-ago")
}
/// How long since a time, ticking: "45s", "12m", "2h 14m", "3d 12h".
pub fn since(at: &str) -> TrustedHtml {
    time(at, "data-since")
}
/// A time as itself, the UTC day and minute, never read relative.
pub fn at(at: &str) -> TrustedHtml {
    let shown = esc(&at_text(at));
    TrustedHtml::owned(format!(
        "<time datetime=\"{a}\">{shown}</time>",
        a = esc(at)
    ))
}
fn time(at: &str, mode: &str) -> TrustedHtml {
    let shown = esc(&at_text(at));
    TrustedHtml::owned(format!(
        "<time {mode}=\"{a}\" datetime=\"{a}\" title=\"{shown}\">{shown}</time>",
        a = esc(at)
    ))
}
/// A duration in the board's two largest units: "<1s", "45s", "12m", "2h 14m", "1d 3h"
/// (`sluice.js` ticks a running one in the same words).
pub fn duration_text(seconds: f64) -> String {
    let s = whole_seconds(seconds);
    let (d, h, m) = (s / 86_400, s % 86_400 / 3_600, s % 3_600 / 60);
    match s {
        0 => "<1s".into(),
        1..60 => format!("{s}s"),
        60..3_600 => format!("{m}m"),
        3_600..86_400 => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}
/// The same duration in words, for a screen reader: "under a second", "45 seconds",
/// "2 hours 14 minutes", "1 day".
pub fn duration_words(seconds: f64) -> String {
    let s = whole_seconds(seconds);
    let (d, h, m) = (s / 86_400, s % 86_400 / 3_600, s % 3_600 / 60);
    let parts = match s {
        0 => return "under a second".into(),
        1..60 => [count(s as usize, "second", "seconds"), String::new()],
        60..3_600 => [count(m as usize, "minute", "minutes"), String::new()],
        3_600..86_400 => [
            count(h as usize, "hour", "hours"),
            count(m as usize, "minute", "minutes"),
        ],
        _ => [
            count(d as usize, "day", "days"),
            count(h as usize, "hour", "hours"),
        ],
    };
    parts
        .into_iter()
        .filter(|p| !p.is_empty() && !p.starts_with("0 "))
        .collect::<Vec<_>>()
        .join(" ")
}
/// A duration drawn: its short form, its words for a screen reader.
pub fn duration(seconds: f64) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<span class=\"dur-v\"><span aria-hidden=\"true\">{}</span><span class=\"vh\">{}</span></span>",
        duration_text(seconds),
        duration_words(seconds)
    ))
}

// ---- counts ----------------------------------------------------------------------------------

/// "1 step", "3 steps", "0 steps".
pub fn count(n: impl Whole, one: &str, many: &str) -> String {
    let n = n.whole();
    format!("{n} {}", if n == 1 { one } else { many })
}
/// A count a page says: any whole number, or a reference to one.
pub trait Whole {
    fn whole(&self) -> u64;
}
macro_rules! whole {
    ($($t:ty),*) => {$(impl Whole for $t { fn whole(&self) -> u64 { *self as u64 } })*};
}
whole!(usize, u64, u32, i64, i32);
impl<T: Whole> Whole for &T {
    fn whole(&self) -> u64 {
        (*self).whole()
    }
}
/// Counts joined as a summary line counts: "6 steps · 1 running · 4 pending", each part that
/// is zero left out but the first.
pub fn tally(parts: &[(usize, &str)]) -> String {
    parts
        .iter()
        .enumerate()
        .filter(|(i, (n, _))| *i == 0 || *n > 0)
        .map(|(_, (n, word))| format!("{n} {word}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Words cut to `chars` at a word, an ellipsis after them when cut.
pub fn cut(text: &str, chars: usize) -> String {
    sluice_model::naming::cut(text, chars)
}
/// A type in a few words: a JSON schema reads as its `type` ("object"), not as JSON; a type's
/// name as itself.
pub fn type_words(ty: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(ty) {
        Ok(serde_json::Value::Object(schema)) => schema
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("object")
            .to_owned(),
        _ => ty.to_owned(),
    }
}

// ---- structure -------------------------------------------------------------------------------

/// A section head (every page's one style): its words, then its count in muted ink, left out at
/// zero (the empty line under the head says it). `id` names it for `aria-labelledby`.
pub fn head(level: u8, text: &str, count: Option<usize>, id: &str) -> TrustedHtml {
    let level = level.clamp(1, 6);
    let id = if id.is_empty() {
        String::new()
    } else {
        format!(" id=\"{}\"", esc(id))
    };
    let n = match count {
        Some(n) if n > 0 => format!(" <span class=\"n\">{n}</span>"),
        _ => String::new(),
    };
    TrustedHtml::owned(format!("<h{level}{id}>{}{n}</h{level}>", esc(text)))
}
/// One row of a facts list: its name, then its value.
pub fn field(name: &str, value: &TrustedHtml) -> TrustedHtml {
    TrustedHtml::owned(format!("{}{value}{}", field_start(name), field_end()))
}
/// A facts row whose value a template draws between these two.
pub fn field_start(name: &str) -> TrustedHtml {
    TrustedHtml::owned(format!("<div><dt>{}</dt><dd>", esc(name)))
}
pub fn field_end() -> TrustedHtml {
    TrustedHtml::owned("</dd></div>".into())
}
/// A small fact set apart: `tone` "" (plain), "attn" (gold), "muted", "failed" or "live",
/// with a glyph before its words when given.
pub fn tag(text: &str, tone: &str, mark: Option<TrustedHtml>) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<span class=\"tag{}\">{}{}</span>",
        tone_class(tone),
        mark.map(|m| m.0).unwrap_or_default(),
        esc(text)
    ))
}
/// A tag that leads somewhere.
pub fn tag_link(href: &str, text: &str, tone: &str, mark: Option<TrustedHtml>) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<a class=\"tag{}\" href=\"{}\">{}{}</a>",
        tone_class(tone),
        esc(href),
        mark.map(|m| m.0).unwrap_or_default(),
        esc(text)
    ))
}
fn tone_class(tone: &str) -> String {
    match tone {
        "" => String::new(),
        "failed" => " t-failed".into(),
        other => format!(" {}", esc(other)),
    }
}

// ---- a step ----------------------------------------------------------------------------------

/// How a page names a step: its title (its stage before it, muted: "land ·") and its id after
/// it in data mono, muted; just its id when it has no title.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepRef {
    pub id: String,
    pub title: String,
    pub stage: String,
}
impl StepRef {
    pub fn new(id: &str, name: Option<&sluice_model::naming::StepName>) -> Self {
        Self {
            id: id.to_owned(),
            title: name.map(|n| n.title.clone()).unwrap_or_default(),
            stage: name.map(|n| n.stage.clone()).unwrap_or_default(),
        }
    }
    /// It has a title of its own, apart from its id.
    pub fn titled(&self) -> bool {
        !self.title.is_empty() && self.title != self.id
    }
    /// Its title as plain text, its stage before it ("land · Ship the cron fix"), cut to
    /// `chars`; its id when it has no title.
    pub fn text(&self, chars: usize) -> String {
        if !self.titled() {
            return self.id.clone();
        }
        let title = sluice_model::naming::cut(&self.title, chars);
        if self.stage.is_empty() {
            title
        } else {
            format!("{} · {title}", self.stage)
        }
    }
    /// A link's accessible name: its title cut to `chars` (its stage before it), then its id,
    /// so the name starts with the words the link shows; its id alone without a title.
    pub fn link_name(&self, chars: usize) -> String {
        if self.titled() {
            format!("{} {}", self.text(chars), self.id)
        } else {
            self.id.clone()
        }
    }
    /// Its words, inline: title (with its stage) then its id, or the id alone.
    pub fn html(&self, chars: usize) -> TrustedHtml {
        if !self.titled() {
            return TrustedHtml::owned(format!(
                "<span class=\"sref\"><span class=\"sref-t\">{}</span></span>",
                esc(&self.id)
            ));
        }
        TrustedHtml::owned(format!(
            "<span class=\"sref\">{}<span class=\"sref-t\">{}</span> <code class=\"sref-id\">{}</code></span>",
            self.stage_html(),
            esc(&sluice_model::naming::cut(&self.title, chars)),
            esc(&self.id)
        ))
    }
    fn stage_html(&self) -> String {
        if self.stage.is_empty() {
            String::new()
        } else {
            format!("<span class=\"sref-stage\">{} ·</span> ", esc(&self.stage))
        }
    }
    /// A link to it: its words as `html` draws them; `opens` names the step the drawer opens.
    pub fn link(&self, href: &str, chars: usize, opens: bool) -> TrustedHtml {
        TrustedHtml::owned(format!(
            "<a class=\"sref-a\" href=\"{}\"{}{}>{}</a>",
            esc(href),
            if opens {
                format!(" data-opens=\"{}\"", esc(&self.id))
            } else {
                String::new()
            },
            if self.titled() {
                format!(" title=\"{}\"", esc(&self.title))
            } else {
                String::new()
            },
            self.html(chars)
        ))
    }
}

// ---- the kit: tabs, panels, an empty state, a confirmation ------------------------------------
// Server-drawn parts with one look each (DESIGN.md, Components; the gallery at `/_ui` shows every
// one in every state). Their behaviour is a Rocket component around them (`ui::rocket`): a page
// works without it, each part falling back to plain HTML (tabs to their panels stacked under
// their heads, a fold to its details, a confirmation to its form inline).

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
/// sets on one page apart; `url` mirrors the chosen tab into the address's `?tab=`. The chosen
/// tab is the host's `current`, which a stream patch never resets: the script owns it once the
/// page is drawn.
pub fn tabs_open(
    prefix: &str,
    label: &str,
    tabs: &[Tab],
    selected: &str,
    url: bool,
) -> TrustedHtml {
    let selected = tabs
        .iter()
        .find(|t| t.key == selected)
        .or(tabs.first())
        .map_or("", |t| t.key);
    let mut bar = String::new();
    for tab in tabs {
        let on = tab.key == selected;
        let count = if tab.count.is_empty() {
            String::new()
        } else if tab.said.is_empty() {
            format!(" <span class=\"n\">{}</span>", esc(&tab.count))
        } else {
            format!(
                " <span class=\"n\" aria-hidden=\"true\">{}</span><span class=\"vh\">, {}</span>",
                esc(&tab.count),
                esc(&tab.said)
            )
        };
        bar.push_str(&format!(
            "<button type=\"button\" role=\"tab\" id=\"{p}tt-{k}\" aria-controls=\"{p}tp-{k}\" aria-selected=\"{on}\" tabindex=\"{ti}\" data-tab=\"{k}\" data-preserve-attr=\"aria-selected tabindex\">{label}{count}</button>",
            p = esc(prefix),
            k = tab.key,
            ti = if on { 0 } else { -1 },
            label = esc(tab.label),
        ));
    }
    let host = Host::new("sluice-tabs")
        .attr("id", format!("{prefix}tabs"))
        .attr("class", "tabs")
        .attr("current", selected)
        .flag("url", url)
        .keep("current");
    TrustedHtml::owned(format!(
        "{}<div class=\"tabbar\"><div class=\"tablist\" role=\"tablist\" aria-label=\"{label}\">{bar}</div></div>",
        host.open(),
        label = esc(label),
    ))
}
pub fn tabs_close() -> TrustedHtml {
    Host::new("sluice-tabs").close()
}
/// A tab's panel: a section under its own head (at `level`), which stands alone without script
/// (every panel stacked) and is the tab's words for a screen reader with it. With `region`, a
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
/// A fold around HTML that may run long: its first lines, faded, and "Show all" under them
/// (`sluice-fold`; said only when the text is cut).
pub fn fold(class: &str, html: &TrustedHtml) -> TrustedHtml {
    TrustedHtml::owned(format!("{}{html}{}", fold_open(class), fold_close()))
}
/// What a place says when it has nothing to show: one plain sentence.
pub fn empty(text: &str) -> TrustedHtml {
    empty_with(text, &TrustedHtml::owned(String::new()))
}
/// The same with a way on after it (a link).
pub fn empty_with(text: &str, more: &TrustedHtml) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"empty-state\"><p>{}</p>{}</div>",
        esc(text),
        if more.as_str().is_empty() {
            String::new()
        } else {
            format!("<p class=\"es-more\">{more}</p>")
        }
    ))
}
/// A confirmation (Cancel, Close all, Delete project), drawn by `html` (`ui::rocket`): its
/// opener's words, the dialog's title with an `id` after it in data mono, the form it confirms
/// and its words.
#[derive(Clone, Debug, Default)]
pub struct Confirm {
    pub opener: String,
    /// The opener's own id, for a page that names it (`delete-button`).
    pub opener_id: String,
    pub title: String,
    pub id: String,
    pub action: String,
    pub form_id: String,
    pub hidden: Vec<(&'static str, String)>,
    pub copy: String,
    /// A reason field's placeholder: none when the confirmation takes no reason.
    pub reason: Option<&'static str>,
    pub confirm: String,
    pub keep: &'static str,
    /// The confirm button in the ink-filled danger tone (Delete), else primary.
    pub danger: bool,
    /// It cannot be confirmed now: its opener is dimmed and does nothing.
    pub disabled: bool,
    /// What the form draws after its buttons (a status line its page's script writes).
    pub after: TrustedHtml,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_from_julian_days_does_not_lose_a_minute() {
        // 22:17 to 22:55 as julian days: (to - from) * 86400 is a hair under 2280
        let from = 2_461_322.428_472_222_3_f64;
        let to = from + 38.0 / 1_440.0;
        let seconds = (to - from) * 86_400.0;
        assert_eq!(duration_text(seconds), "38m");
        assert_eq!(duration_text(2_279.999_98), "38m");
        assert_eq!(duration_words(2_279.999_98), "38 minutes");
        assert_eq!(duration_text(59.4), "59s");
    }
}
