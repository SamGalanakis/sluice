//! The Synthesis kit (DESIGN.md, The grid, The band, Modules, Status presentation): the parts
//! every page composes on the module grid. The module grid and its show-grid switch, a module
//! and its swell, a band section's head, a unit's stage strip, the summary sentence, the
//! "Recently finished" strip, the margin module for a long run's progress, and the trace rail
//! with its chain sentence. Each returns escaped HTML; the grid and the trace are Rocket
//! components (`sluice-grid`, `sluice-trace`) that only behave: what they show is drawn here.
//!
//! Generic by rule: a stage is whatever a unit's recipe names it, a count is a status table
//! count, and a progress field is whatever a step reported. Nothing here knows a project.
use super::{Host, Shown, Tally, count, duration, duration_text, esc, mark, since, word};
use crate::views::TrustedHtml;
use crate::views::icons::{Icon, icon};
use sluice_model::shown::Band;
use std::collections::{BTreeMap, BTreeSet};

// ---- the module grid ---------------------------------------------------------------------------

/// The module grid's host (`sluice-grid`): a sheet of `cols` columns (12; a phone's 6) that
/// modules span (`module_open`), with the construction grid drawn under them, hidden until the
/// show-grid switch (`grid_toggle`, anywhere on the page) shows it: each column tinted and
/// numbered, each module's span named in its corner. `id` names it for its switch.
pub fn grid_open(id: &str, cols: u8) -> TrustedHtml {
    let cols = cols.clamp(1, 12);
    let columns: String = (1..=cols)
        .map(|n| format!("<span class=\"gc\"><span class=\"gc-n\">{n}</span></span>"))
        .collect();
    TrustedHtml::owned(format!(
        "{}<div class=\"grid-ovl\" aria-hidden=\"true\">{columns}</div>",
        Host::new("sluice-grid")
            .some("id", id)
            .attr("class", "sheet")
            .attr("style", format!("--cols:{cols}"))
            .keep("showing")
            .open()
    ))
}
pub fn grid_close() -> TrustedHtml {
    Host::new("sluice-grid").close()
}
/// The show-grid switch for the grid `id`: a pressed button while the construction grid shows,
/// there only with script (the grid is a way of looking, not content).
pub fn grid_toggle(id: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<button type=\"button\" class=\"grid-toggle needs-js\" aria-controls=\"{}\" aria-pressed=\"false\" data-preserve-attr=\"aria-pressed\">{}Show grid</button>",
        esc(id),
        icon(Icon::Grid3x3, 16, "")
    ))
}

// ---- a module --------------------------------------------------------------------------------

/// How much a module asks for: the one that needs the owner swells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Swell {
    /// A module like any other: a hairline over it.
    Plain,
    /// It needs the owner: a question to them. The coral rule over it, its title a size up.
    Ask,
    /// It needs a look (stopped: failed, cancelled, stale): on the sand, a navy rule over it.
    Look,
    /// A long run's margin module: on the sky, a heavy rule over it.
    Margin,
}
impl Swell {
    fn class(self) -> &'static str {
        match self {
            Swell::Plain => "mod",
            Swell::Ask => "mod swell-ask",
            Swell::Look => "mod swell-look",
            Swell::Margin => "mod swell-margin",
        }
    }
}
/// A module on the grid: an `article` spanning `span` of its sheet's columns (all of them on a
/// phone), named `label` for a screen reader. Its parts go between this and `module_close`:
/// `.mod-k` (its kind's line: a glyph, a word, who and when), `.mod-t` (its title),
/// `.mod-meta` (its id and facts), a stage strip, `.mod-body`, `.mod-actions`.
pub fn module_open(span: u8, swell: Swell, label: &str) -> TrustedHtml {
    let span = span.clamp(1, 12);
    TrustedHtml::owned(format!(
        "<article class=\"{}\" style=\"--span:{span}\" data-span=\"{span}\" aria-label=\"{}\">",
        swell.class(),
        esc(label)
    ))
}
pub fn module_close() -> TrustedHtml {
    TrustedHtml::owned("</article>".into())
}
/// A column of the sheet that holds modules stacked (`span` columns wide): a band's modules
/// under its head.
pub fn column_open(span: u8) -> TrustedHtml {
    let span = span.clamp(1, 12);
    TrustedHtml::owned(format!(
        "<div class=\"mod-col\" style=\"--span:{span}\" data-span=\"{span}\">"
    ))
}
pub fn column_close() -> TrustedHtml {
    TrustedHtml::owned("</div>".into())
}

// ---- a band section's head -------------------------------------------------------------------

/// A band's head on the paper (For you, Stopped, Running, Waiting, Done): a heavy rule over
/// it, its name big at the left, its count line at the right ("1 failed · 1 cancelled"), the
/// count line left out when empty. `id` names it for a link ("#for-you").
pub fn section_head(id: &str, name: &str, line: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"sec-h\"{}><h2>{}</h2>{}</div>",
        if id.is_empty() {
            String::new()
        } else {
            format!(" id=\"{}\"", esc(id))
        },
        esc(name),
        if line.is_empty() {
            String::new()
        } else {
            format!("<p class=\"sec-n\">{}</p>", esc(line))
        }
    ))
}
/// A band's head over a table of strips: its name and line in the first `lead` columns, then
/// each stage's name over its column.
pub fn strip_head(id: &str, name: &str, line: &str, lead: u8, stages: &[&str]) -> TrustedHtml {
    let names: String = stages
        .iter()
        .map(|s| format!("<span class=\"sh-stage\">{}</span>", esc(s)))
        .collect();
    TrustedHtml::owned(format!(
        "<div class=\"sec-h sec-strip\"{} style=\"--lead:{lead};--rest:{rest};--n:{n}\"><div class=\"sh-lead\"><h2>{}</h2>{}</div><div class=\"sh-stages\" aria-hidden=\"true\">{names}</div></div>",
        if id.is_empty() {
            String::new()
        } else {
            format!(" id=\"{}\"", esc(id))
        },
        esc(name),
        if line.is_empty() {
            String::new()
        } else {
            format!("<p class=\"sec-n\">{}</p>", esc(line))
        },
        lead = lead.clamp(1, 11),
        rest = 12 - lead.clamp(1, 11),
        n = stages.len().max(1),
    ))
}

// ---- the stage strip -------------------------------------------------------------------------

/// One cell of a unit's stage strip: a stage of its recipe (or a step of a unit with no
/// recipe), in recipe order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stage {
    /// The stage's own name, as its recipe names it.
    pub name: String,
    /// Its step's state; `None` when it has no step yet (not reached).
    pub shown: Option<Shown>,
    /// Seconds: how long its last run took (done, a look) or has run (running).
    pub seconds: Option<f64>,
    /// When its running run started, so the page ticks its elapsed time.
    pub since: String,
    /// Its run against its stage's usual time, when past it ("2.1×").
    pub over: Option<f64>,
    /// Its step's page.
    pub href: String,
    /// A note across this and the cells after it ("waits for review"): `cols` cells wide.
    pub gap: Option<(String, u8)>,
}
impl Stage {
    pub fn new(name: impl Into<String>, shown: Option<Shown>) -> Self {
        Self {
            name: name.into(),
            shown,
            ..Default::default()
        }
    }
    /// A note spanning `cols` cells where no stage has reached ("waits for review").
    pub fn gap(text: impl Into<String>, cols: u8) -> Self {
        Self {
            gap: Some((text.into(), cols.max(1))),
            ..Default::default()
        }
    }
    pub fn took(mut self, seconds: f64) -> Self {
        self.seconds = Some(seconds);
        self
    }
    pub fn running_since(mut self, at: impl Into<String>, seconds: f64) -> Self {
        self.since = at.into();
        self.seconds = Some(seconds);
        self
    }
    pub fn over(mut self, ratio: f64) -> Self {
        self.over = Some(ratio);
        self
    }
    pub fn href(mut self, href: impl Into<String>) -> Self {
        self.href = href.into();
        self
    }
}
/// How a cell is drawn, from its state's row in the status table alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    /// Not reached: an outline (a waiting state that says itself, held, queued, paused,
    /// blocked, writes its word in it).
    Empty,
    /// Done: sky, its glyph and how long it took.
    Done,
    /// Running: the channel's blue, its elapsed time big, a sweep along its foot.
    Run,
    /// Needs a look: sand, its glyph and its word (failed, cancelled, stale; quiet keeps
    /// running's blue edge).
    Look,
}
impl Cell {
    /// The cell a state draws: done's band is Done; a state that wants attention is a Look; the
    /// rest of the running band is Run; everything else is not reached.
    pub fn of(shown: Option<Shown>) -> Cell {
        let Some(shown) = shown else {
            return Cell::Empty;
        };
        let spec = shown.spec();
        if spec.band == Band::Done {
            Cell::Done
        } else if spec.attention {
            Cell::Look
        } else if spec.band == Band::Running {
            Cell::Run
        } else {
            Cell::Empty
        }
    }
    /// Its name in a class: `sc-<key>` a cell, `mk-<key>` a mark.
    pub fn key(self) -> &'static str {
        match self {
            Cell::Empty => "empty",
            Cell::Done => "done",
            Cell::Run => "run",
            Cell::Look => "look",
        }
    }
    fn class(self) -> String {
        format!("sc sc-{}", self.key())
    }
}
/// "2.1×" (one decimal, a whole number past 10).
pub fn ratio_text(ratio: f64) -> String {
    if ratio >= 10.0 {
        format!("{}×", ratio.round() as i64)
    } else {
        format!("{:.1}×", (ratio * 10.0).floor() / 10.0)
    }
}
/// The overrun chip: a running step past its stage's usual time, "2.1× usual", on the sand
/// with the timer, said whole for a screen reader.
pub fn overrun(ratio: f64) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<span class=\"overrun\" title=\"{r} its usual time\">{}<span>{r} usual</span></span>",
        icon(Icon::Timer, 12, ""),
        r = ratio_text(ratio)
    ))
}
/// A unit's stages as a strip of cells, one a stage in recipe order (DESIGN.md, Status
/// presentation): never colour alone, each cell carries its glyph or its words, and the strip
/// is a list named `label` ("Stages of a-12"). A cell with a page links to it.
pub fn stage_strip(label: &str, stages: &[Stage]) -> TrustedHtml {
    let n: usize = stages
        .iter()
        .map(|s| s.gap.as_ref().map_or(1, |(_, c)| *c as usize))
        .sum();
    let mut cells = String::new();
    for stage in stages {
        if let Some((text, cols)) = &stage.gap {
            cells.push_str(&format!(
                "<li class=\"sc sc-gap\" style=\"--gap:{cols}\"><span>{}</span></li>",
                esc(text)
            ));
            continue;
        }
        let cell = Cell::of(stage.shown);
        let state = stage.shown.map_or("not reached", word);
        let time = |s: f64| -> String {
            match cell {
                Cell::Run if !stage.since.is_empty() => {
                    format!("<span class=\"sc-time\">{}</span>", since(&stage.since))
                }
                _ => format!("<span class=\"sc-time\">{}</span>", duration(s)),
            }
        };
        let inner = match cell {
            Cell::Empty => format!(
                "<span class=\"sc-name\">{}</span>{}<span class=\"vh\">: {}</span>",
                esc(&stage.name),
                match stage.shown {
                    Some(s) if s.spec().caption || s == Shown::Paused => format!(
                        "<span class=\"sc-word\" aria-hidden=\"true\">{}{}</span>",
                        mark(s),
                        esc(word(s))
                    ),
                    _ => String::new(),
                },
                esc(state)
            ),
            Cell::Done => format!(
                "<span class=\"sc-top\">{}<span class=\"sc-name\">{}</span></span>{}<span class=\"vh\">: {}</span>",
                mark(stage.shown.unwrap_or(Shown::Succeeded)),
                esc(&stage.name),
                stage.seconds.map(time).unwrap_or_default(),
                esc(state)
            ),
            Cell::Run => format!(
                "<span class=\"sc-top\"><span class=\"sc-name\">{}</span>{}</span>{}<span class=\"vh\">: {}{}</span><span class=\"sweep\" aria-hidden=\"true\"></span>",
                esc(&stage.name),
                stage
                    .over
                    .map(|r| format!(
                        "<span class=\"sc-over\" aria-hidden=\"true\">{}</span>",
                        ratio_text(r)
                    ))
                    .unwrap_or_default(),
                stage.seconds.map(time).unwrap_or_default(),
                esc(state),
                stage
                    .over
                    .map(|r| format!(", {} its usual time", ratio_text(r)))
                    .unwrap_or_default(),
            ),
            Cell::Look => {
                let shown = stage.shown.expect("a look has a state");
                format!(
                    "<span class=\"sc-top\">{}<span class=\"sc-name\">{}</span><span class=\"sc-word\" aria-hidden=\"true\">{}</span></span>{}<span class=\"vh\">: {}</span>",
                    mark(shown),
                    esc(&stage.name),
                    esc(word(shown)),
                    stage.seconds.map(time).unwrap_or_default(),
                    esc(state)
                )
            }
        };
        let quiet = stage.shown == Some(Shown::Quiet);
        let body = if stage.href.is_empty() {
            inner
        } else {
            format!(
                "<a class=\"sc-a\" href=\"{}\">{inner}</a>",
                esc(&stage.href)
            )
        };
        cells.push_str(&format!(
            "<li class=\"{}{}\"{}>{body}</li>",
            cell.class(),
            if quiet { " sc-quiet" } else { "" },
            stage
                .shown
                .map(|s| format!(" data-state=\"{}\"", s.key()))
                .unwrap_or_default()
        ));
    }
    TrustedHtml::owned(format!(
        "<ol class=\"strip\" style=\"--n:{}\" aria-label=\"{}\">{cells}</ol>",
        n.max(1),
        esc(label)
    ))
}
/// The same strip small, for a line of words (a stopped module's meta): a square a stage,
/// its state's fill, the strip's words for a screen reader.
pub fn stage_marks(label: &str, stages: &[Stage]) -> TrustedHtml {
    let said = stages
        .iter()
        .filter(|s| s.gap.is_none())
        .map(|s| format!("{} {}", s.name, s.shown.map_or("not reached", word)))
        .collect::<Vec<_>>()
        .join(", ");
    let marks: String = stages
        .iter()
        .filter(|s| s.gap.is_none())
        .map(|s| format!("<i class=\"mk mk-{}\"></i>", Cell::of(s.shown).key()))
        .collect();
    TrustedHtml::owned(format!(
        "<span class=\"marks\" role=\"img\" aria-label=\"{}: {}\">{marks}</span>",
        esc(label),
        esc(&said)
    ))
}

// ---- the summary sentence ---------------------------------------------------------------------

/// One unit as the summary counts it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UnitFact {
    /// How the summary names it (its unit name, or a loose step's id).
    pub name: String,
    /// The recipe it came from, "" for none.
    pub recipe: String,
    /// The unit's state: its steps' first in the status table's order.
    pub shown: Option<Shown>,
    /// Its running step against its stage's usual time, when past it.
    pub over: Option<f64>,
    /// How long its quiet run has written nothing, seconds.
    pub quiet: Option<f64>,
}
/// What the band's summary sentence says (`summary_sentence`): built from the status table's
/// counts, overruns, quiet runs and open questions alone, naming recipes by their own names.
#[derive(Clone, Debug, Default)]
pub struct Summary<'a> {
    /// Open questions to the owner that someone waits on, and where they are answered.
    pub asks: usize,
    pub ask_href: &'a str,
    pub units: &'a [UnitFact],
    /// When the last unit to finish finished.
    pub last_done: &'a str,
    /// What a unit is called in the count ("unit", "units").
    pub noun: (&'a str, &'a str),
}
/// The band's summary sentence: "1 question for you. 1 failed, 1 cancelled. 2 article and 1
/// scan at work: a-14 at 2.1× its usual time, s-3 quiet for 53m. 4 waiting. 11 of 19 units
/// done; the last finished 57m ago." Each part left out when it has nothing to say; "Nothing
/// has started yet." for a plan with no work at all.
pub fn summary_sentence(s: &Summary<'_>) -> TrustedHtml {
    let (one, many) = if s.noun.0.is_empty() {
        ("unit", "units")
    } else {
        s.noun
    };
    let mut parts: Vec<String> = vec![];
    if s.asks > 0 {
        parts.push(format!(
            "<a class=\"ask\" href=\"{}\">{} for you</a>.",
            esc(s.ask_href),
            count(s.asks, "question", "questions")
        ));
    }
    let mut stopped: Tally = Tally::default();
    let mut running: Vec<&UnitFact> = vec![];
    let (mut waiting, mut done) = (0usize, 0usize);
    for unit in s.units {
        let Some(shown) = unit.shown else { continue };
        match shown.spec().band {
            Band::Stopped => stopped.add(shown),
            Band::Running => running.push(unit),
            Band::Waiting => waiting += 1,
            Band::Done => done += 1,
        }
    }
    let stops: Vec<String> = stopped
        .iter()
        .map(|(shown, n)| format!("{n} {}", word(shown)))
        .collect();
    if !stops.is_empty() {
        parts.push(format!("{}.", esc(&stops.join(", "))));
    }
    if !running.is_empty() {
        // how many of each recipe, in the order a recipe first appears; a unit of no recipe
        // is "other"
        let mut by: Vec<(&str, usize)> = vec![];
        for unit in &running {
            let name = if unit.recipe.is_empty() {
                ""
            } else {
                unit.recipe.as_str()
            };
            match by.iter_mut().find(|(r, _)| *r == name) {
                Some((_, n)) => *n += 1,
                None => by.push((name, 1)),
            }
        }
        let named = by.iter().any(|(r, _)| !r.is_empty());
        let who = if named {
            join(
                &by.iter()
                    .map(|(r, n)| {
                        if r.is_empty() {
                            format!("{n} other")
                        } else {
                            format!("{n} {r}")
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        } else {
            running.len().to_string()
        };
        let mut notes: Vec<(f64, String)> = vec![];
        for unit in &running {
            if let Some(secs) = unit.quiet {
                notes.push((
                    f64::MAX,
                    format!("{} quiet for {}", unit.name, duration_text(secs)),
                ));
            } else if let Some(r) = unit.over {
                notes.push((
                    r,
                    format!("{} at {} its usual time", unit.name, ratio_text(r)),
                ));
            }
        }
        // quiet first, then the furthest past its usual time
        notes.sort_by(|a, b| b.0.total_cmp(&a.0));
        let notes: Vec<String> = notes.into_iter().map(|(_, n)| n).collect();
        parts.push(format!(
            "{} at work{}.",
            esc(&who),
            if notes.is_empty() {
                String::new()
            } else {
                format!(": {}", esc(&notes.join(", ")))
            }
        ));
    }
    if waiting > 0 {
        parts.push(format!("{waiting} waiting."));
    }
    let total = s.units.iter().filter(|u| u.shown.is_some()).count();
    if done > 0 {
        let of = if done == total {
            format!("All {} done", count(total, one, many))
        } else {
            format!("{done} of {} done", count(total, one, many))
        };
        parts.push(if s.last_done.is_empty() {
            format!("{}.", esc(&of))
        } else {
            format!(
                "{}; the last finished {}.",
                esc(&of),
                super::ago(s.last_done)
            )
        });
    }
    if parts.is_empty() {
        parts.push("Nothing has started yet.".into());
    }
    TrustedHtml::owned(parts.join(" "))
}
/// "a", "a and b", "a, b and c".
fn join(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

// ---- the band's head and its strip -------------------------------------------------------------

/// The band's head (`Frame::head`): the page's name huge at the left on the grid, its words and
/// its summary sentence beside it. A long name takes a size down and the full width
/// (`long` from 7 characters, `longer` from 13, `longest` from 21), so it never runs off.
pub fn band_head(name: &str, lead: &TrustedHtml, summary: &TrustedHtml) -> TrustedHtml {
    let chars = name.chars().count();
    let size = match chars {
        0..=6 => "",
        7..=12 => " class=\"long\"",
        13..=20 => " class=\"longer\"",
        _ => " class=\"longest\"",
    };
    let lead = if lead.as_str().is_empty() {
        String::new()
    } else {
        format!("<div class=\"band-lead\">{lead}</div>")
    };
    let summary = if summary.as_str().is_empty() {
        String::new()
    } else {
        format!("<p class=\"band-summary\">{summary}</p>")
    };
    TrustedHtml::owned(format!(
        "<div class=\"band-head\"><h1{size}>{}</h1>{}</div>",
        esc(name),
        if lead.is_empty() && summary.is_empty() {
            String::new()
        } else {
            format!("<div class=\"band-sub\">{lead}{summary}</div>")
        }
    ))
}
/// One unit (or a step of no unit) whose run ended: when, its name and title, how long.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Finished {
    pub name: String,
    pub title: String,
    pub href: String,
    /// When its last run ended.
    pub at: String,
    /// How long its work took, seconds, and in how many runs.
    pub took: f64,
    pub runs: usize,
    /// How it ended: done, or a state that needs a look.
    pub shown: Option<Shown>,
}
/// The band's strip of what finished most recently (`Frame::head`, after `band_head`), its
/// heading's id `id`: its head and line in the first two columns, then up to five, newest first, each its clock time
/// big, its name, its title and how long it took. Nothing when nothing has finished.
pub fn recent_strip(id: &str, line: &str, items: &[Finished]) -> TrustedHtml {
    if items.is_empty() {
        return TrustedHtml::default();
    }
    let cells: String = items
        .iter()
        .take(5)
        .map(|f| {
            let took = format!(
                "took {}{}",
                duration_text(f.took),
                if f.runs > 1 {
                    format!(" · {} runs", f.runs)
                } else {
                    String::new()
                }
            );
            let ended = match f.shown {
                Some(s) if s.spec().band != Band::Done => {
                    format!("<span class=\"rf-how\">{}{}</span>", mark(s), esc(word(s)))
                }
                _ => String::new(),
            };
            format!(
                "<li><a href=\"{}\"><span class=\"rf-at\">{}</span><span class=\"rf-name\">{}</span><span class=\"rf-title\">{}</span><span class=\"rf-took\">{ended}{}</span></a></li>",
                esc(&f.href),
                clock(&f.at),
                esc(&f.name),
                esc(&f.title),
                esc(&took)
            )
        })
        .collect();
    let id = esc(id);
    TrustedHtml::owned(format!(
        "<section class=\"band-strip\" aria-labelledby=\"{id}\"><div class=\"rf-head\"><h2 id=\"{id}\">Recently finished</h2>{}</div><ol class=\"rf-list\">{cells}</ol></section>",
        if line.is_empty() {
            String::new()
        } else {
            format!("<p>{}</p>", esc(line))
        }
    ))
}
/// A time of day: "17:49" as UTC on the server, the reader's own clock once the page's script
/// reads it (`data-clock`); the day and minute in UTC in its title.
pub fn clock(at: &str) -> TrustedHtml {
    let shown = at.get(11..16).unwrap_or(at);
    TrustedHtml::owned(format!(
        "<time data-clock datetime=\"{a}\" title=\"{t}\">{}</time>",
        esc(shown),
        a = esc(at),
        t = esc(&super::at_text(at))
    ))
}

// ---- the margin module: a long run's progress ------------------------------------------------

/// A step whose run has gone on far past any usual time, or reports progress (`step_progress`):
/// its fields as it reported them, never read or named by sluice.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LongRun {
    pub name: String,
    pub title: String,
    pub doc: String,
    pub href: String,
    /// Its run's number and when it started.
    pub run: usize,
    pub since: String,
    /// Its progress fields in the order it reported them, and when it last did.
    pub fields: Vec<(String, serde_json::Value)>,
    pub at: String,
}
/// The margin module (`Swell::Margin`, two columns at the sheet's right): its name and words,
/// its running time on the blue, then each progress field (the first one large), then when it
/// last reported and a link to its page.
pub fn margin_module(run: &LongRun) -> TrustedHtml {
    let value = |v: &serde_json::Value| match v {
        serde_json::Value::String(s) => esc(s),
        serde_json::Value::Null => "none".into(),
        other => esc(&other.to_string()),
    };
    let mut fields = String::new();
    for (i, (key, v)) in run.fields.iter().enumerate() {
        fields.push_str(&format!(
            "<div class=\"{}\"><dt>{}</dt><dd>{}</dd></div>",
            if i == 0 { "mm-f mm-lead" } else { "mm-f" },
            esc(key),
            value(v)
        ));
    }
    TrustedHtml::owned(format!(
        "{open}<h3 class=\"mm-t\"><a href=\"{href}\">{title}</a></h3>{doc}<div class=\"mm-run\"><span class=\"mm-k\">{run_mark}running{n}</span><span class=\"mm-time\">{since}</span><span class=\"sweep\" aria-hidden=\"true\"></span></div>{fields}{at}{close}",
        open = module_open(2, Swell::Margin, &format!("{}, running", run.name)).as_str(),
        href = esc(&run.href),
        title = esc(if run.title.is_empty() {
            &run.name
        } else {
            &run.title
        }),
        doc = if run.doc.is_empty() {
            String::new()
        } else {
            format!("<p class=\"mm-doc\">{}</p>", esc(&run.doc))
        },
        run_mark = mark(Shown::Running),
        n = if run.run > 1 {
            format!(" · run {}", run.run)
        } else {
            String::new()
        },
        since = since(&run.since),
        fields = if fields.is_empty() {
            "<p class=\"mm-none\">It has reported no progress.</p>".to_owned()
        } else {
            format!("<dl class=\"mm-fields\">{fields}</dl>")
        },
        at = if run.at.is_empty() {
            String::new()
        } else {
            format!("<p class=\"mm-at\">Reported {}</p>", super::ago(&run.at))
        },
        close = module_close(),
    ))
}

// ---- select to trace --------------------------------------------------------------------------

/// What waits on what, unit to unit, and each unit's whole chain both ways: drawn into the
/// page's markup (`attrs`) so the trace (`sluice-trace`) reads it there and fetches nothing.
#[derive(Clone, Debug, Default)]
pub struct Trace {
    up: BTreeMap<String, BTreeSet<String>>,
    down: BTreeMap<String, BTreeSet<String>>,
}
impl Trace {
    /// From each wait: `unit` waits for `on`.
    pub fn new<'a>(waits: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut trace = Trace::default();
        for (unit, on) in waits {
            if unit == on {
                continue;
            }
            trace
                .up
                .entry(unit.to_owned())
                .or_default()
                .insert(on.to_owned());
            trace
                .down
                .entry(on.to_owned())
                .or_default()
                .insert(unit.to_owned());
        }
        trace
    }
    fn reach(edges: &BTreeMap<String, BTreeSet<String>>, start: &str) -> Vec<String> {
        let mut seen: Vec<String> = vec![];
        let mut frontier = vec![start.to_owned()];
        while let Some(at) = frontier.pop() {
            for next in edges.get(&at).into_iter().flatten() {
                if next != start && !seen.contains(next) {
                    seen.push(next.clone());
                    frontier.push(next.clone());
                }
            }
        }
        seen
    }
    /// Every unit `unit` waits for, nearest first.
    pub fn upstream(&self, unit: &str) -> Vec<String> {
        Self::reach(&self.up, unit)
    }
    /// Every unit that waits on `unit`, nearest first.
    pub fn downstream(&self, unit: &str) -> Vec<String> {
        Self::reach(&self.down, unit)
    }
    /// The chain in one sentence: "Tracing a-12. It waits for a-11 and s-4. a-13 waits on it."
    pub fn sentence(&self, unit: &str) -> String {
        let (up, down) = (self.upstream(unit), self.downstream(unit));
        format!(
            "Tracing {unit}. {} {}",
            if up.is_empty() {
                "It waits for nothing on this plan.".to_owned()
            } else {
                format!("It waits for {}.", join(&up))
            },
            match down.len() {
                0 => "Nothing waits on it.".to_owned(),
                1 => format!("{} waits on it.", down[0]),
                _ => format!("{} wait on it.", join(&down)),
            }
        )
    }
    /// A traceable unit's attributes, for the element that holds it (`.rail-slot`): its name,
    /// its whole chain up and down, and its sentence.
    pub fn attrs(&self, unit: &str) -> TrustedHtml {
        TrustedHtml::owned(format!(
            " data-unit=\"{}\" data-up=\"{}\" data-down=\"{}\" data-chain=\"{}\"",
            esc(unit),
            esc(&self.upstream(unit).join(" ")),
            esc(&self.downstream(unit).join(" ")),
            esc(&self.sentence(unit))
        ))
    }
}
/// The words the trace line says while nothing is traced.
pub const TRACE_IDLE: &str = "Select any unit to trace what it waits for and what waits on it.";
/// The trace's host (`sluice-trace`) around the units it traces: its line first, the sentence
/// the chain reads in (polite, and its Clear trace there only while tracing).
pub fn trace_open(id: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "{}<div class=\"trace-line needs-js\"><p class=\"trace-said\" aria-live=\"polite\">{}<span class=\"trace-words\">{}</span></p><button type=\"button\" class=\"trace-clear\" hidden>{}Clear trace</button></div>",
        Host::new("sluice-trace")
            .attr("id", id)
            .attr("class", "trace")
            .keep("selected")
            .open(),
        icon(Icon::Workflow, 18, ""),
        esc(TRACE_IDLE),
        icon(Icon::X, 14, "")
    ))
}
pub fn trace_close() -> TrustedHtml {
    Host::new("sluice-trace").close()
}
/// A unit's place on the trace's rail, in the margin at its left (inside its `.rail-slot`).
pub fn rail() -> TrustedHtml {
    TrustedHtml::owned("<span class=\"rail\" aria-hidden=\"true\"></span>".into())
}
/// The button that selects a unit to trace (its whole head), and the part of it that opens in
/// place while it is traced (`trace_more_open`).
pub fn trace_button_open() -> TrustedHtml {
    TrustedHtml::owned(
        "<button type=\"button\" class=\"trace-pick\" data-trace-pick aria-pressed=\"false\" aria-expanded=\"false\" data-preserve-attr=\"aria-pressed aria-expanded\">"
            .into(),
    )
}
pub fn trace_button_close() -> TrustedHtml {
    TrustedHtml::owned("</button>".into())
}
/// Where a traced unit says how it stands to the one selected: "Selected", "Upstream" or
/// "Downstream" in a navy chip while tracing, nothing otherwise (the line says it in words).
pub fn trace_role() -> TrustedHtml {
    TrustedHtml::owned("<span class=\"trace-role\" aria-hidden=\"true\"></span>".into())
}
/// What a traced unit opens to in place, hidden until then (and without script).
pub fn trace_more_open() -> TrustedHtml {
    TrustedHtml::owned(
        "<div class=\"trace-more\" data-trace-more hidden data-preserve-attr=\"hidden\">".into(),
    )
}
pub fn trace_more_close() -> TrustedHtml {
    TrustedHtml::owned("</div>".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_follows_the_status_table() {
        assert_eq!(Cell::of(None), Cell::Empty);
        assert_eq!(Cell::of(Some(Shown::Succeeded)), Cell::Done);
        assert_eq!(Cell::of(Some(Shown::Skipped)), Cell::Done);
        assert_eq!(Cell::of(Some(Shown::Running)), Cell::Run);
        assert_eq!(Cell::of(Some(Shown::Stopping)), Cell::Run);
        assert_eq!(Cell::of(Some(Shown::Quiet)), Cell::Look);
        assert_eq!(Cell::of(Some(Shown::Failed)), Cell::Look);
        assert_eq!(Cell::of(Some(Shown::Blocked)), Cell::Empty);
        assert_eq!(Cell::of(Some(Shown::Pending)), Cell::Empty);
    }

    #[test]
    fn a_trace_reads_the_whole_chain_both_ways() {
        let t = Trace::new([("b", "a"), ("c", "b"), ("d", "b"), ("e", "x")]);
        assert_eq!(t.upstream("c"), ["b", "a"]);
        let mut down = t.downstream("a");
        down.sort();
        assert_eq!(down, ["b", "c", "d"]);
        assert_eq!(
            t.sentence("b"),
            "Tracing b. It waits for a. c and d wait on it."
        );
        assert_eq!(
            t.sentence("x"),
            "Tracing x. It waits for nothing on this plan. e waits on it."
        );
    }

    #[test]
    fn the_summary_says_only_what_the_counts_say() {
        let unit = |name: &str, recipe: &str, shown, over, quiet| UnitFact {
            name: name.into(),
            recipe: recipe.into(),
            shown: Some(shown),
            over,
            quiet,
        };
        let units = [
            unit("a-1", "article", Shown::Failed, None, None),
            unit("a-2", "article", Shown::Cancelled, None, None),
            unit("a-3", "article", Shown::Running, Some(2.14), None),
            unit("s-1", "scan", Shown::Quiet, None, Some(3180.0)),
            unit("a-4", "article", Shown::Pending, None, None),
            unit("a-5", "article", Shown::Succeeded, None, None),
        ];
        let html = summary_sentence(&Summary {
            asks: 1,
            ask_href: "#for-you",
            units: &units,
            last_done: "",
            noun: ("unit", "units"),
        });
        assert_eq!(
            html.as_str(),
            "<a class=\"ask\" href=\"#for-you\">1 question for you</a>. 1 failed, 1 cancelled. 1 article and 1 scan at work: s-1 quiet for 53m, a-3 at 2.1× its usual time. 1 waiting. 1 of 6 units done."
        );
        assert_eq!(
            summary_sentence(&Summary::default()).as_str(),
            "Nothing has started yet."
        );
    }
}
