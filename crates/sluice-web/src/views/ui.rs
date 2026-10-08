//! The dashboard's shared components: every page draws a status, a time, a duration, a count,
//! a section head, a field row, a step reference and a tag through these, so they read one way
//! everywhere (DESIGN.md, Components). Each returns escaped HTML (or plain text for words).
use super::TrustedHtml;
use super::icons::{Icon, icon, solid};

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

/// A status's glyph (the Shape Carries It Rule): its Lucide icon, named for a screen reader by
/// the status word.
pub fn glyph(status: &str) -> TrustedHtml {
    let (status, shape, class) = match status {
        "running" => ("running", Icon::LoaderCircle, "spin"),
        "succeeded" => ("succeeded", Icon::CircleCheck, ""),
        "manual" => ("manual", Icon::CircleDot, ""),
        "stale" => ("stale", Icon::RotateCw, ""),
        "failed" => ("failed", Icon::CircleX, ""),
        "cancelled" => ("cancelled", Icon::CircleStop, ""),
        "paused" => ("paused", Icon::CirclePause, ""),
        "skipped" => ("skipped", Icon::CircleSlash, ""),
        "external" => ("external", Icon::SquareArrowOutUpRight, ""),
        _ => ("pending", Icon::CircleDashed, ""),
    };
    TrustedHtml::owned(format!(
        "<span class=\"g g-{status}\" role=\"img\" aria-label=\"{status}\">{}</span>",
        solid(shape, 16, class)
    ))
}
/// A quiet run's mark: an hourglass in the attention colour, beside the status glyphs.
pub fn quiet_glyph() -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<span class=\"g g-quiet\" aria-hidden=\"true\">{}</span>",
        icon(Icon::Hourglass, 16, "")
    ))
}
/// A status's word: the mark as the board says it ("set by hand", "outside").
pub fn status_word(mark: &str) -> &str {
    match mark {
        "manual" => "set by hand",
        "external" => "outside",
        other => other,
    }
}
/// A status as a mark and its word: the glyph, then the word (hidden from a screen reader,
/// which hears the glyph's name).
pub fn status(mark: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<span class=\"st st-{m}\">{}<span class=\"st-w\" aria-hidden=\"true\">{}</span></span>",
        glyph(mark),
        esc(status_word(mark)),
        m = esc(mark)
    ))
}

// ---- time and duration -----------------------------------------------------------------------

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
    let s = seconds.max(0.0).floor() as u64;
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
    let s = seconds.max(0.0).floor() as u64;
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
