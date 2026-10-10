//! The Synthesis kit (DESIGN.md, The grid, The page head, Modules, Details, Status
//! presentation): the parts every page composes on the module grid. The module grid, a module
//! and its swell, a band section's head, a unit's stage strip, the summary sentence, the page's
//! head, what finished last, an item's Details behind its "⋯", the margin module for a long
//! run's progress, and the trace rail with its chain sentence. Each returns escaped HTML; the
//! trace is a Rocket component (`sluice-trace`) that only behaves: what it shows is drawn here.
//!
//! Generic by rule: a stage is whatever a unit's recipe names it, a count is a status table
//! count, and a progress field is whatever a step reported. Nothing here knows a project.
use super::{Host, Shown, Tally, count, duration, duration_text, esc, mark, since, word};
use crate::views::TrustedHtml;
use crate::views::icons::{Icon, icon};
use sluice_model::shown::Band;
use std::collections::{BTreeMap, BTreeSet};

// ---- the module grid ---------------------------------------------------------------------------

/// The module grid (`ui::grid_open`): a sheet of `cols` columns (12) that modules span
/// (`module_open`). Layout only: it has no behaviour, so it is a plain element.
///
/// The sheet is fluid and lays out by its own width (DESIGN.md, The grid): half its columns
/// under 640px with every module across it, its columns from 640px (a module half the sheet or
/// whole under 1200px), and twice its columns from 2000px.
pub fn grid_open(id: &str, cols: u8) -> TrustedHtml {
    let cols = cols.clamp(1, 12);
    TrustedHtml::owned(format!(
        "<div{} class=\"sheet\" style=\"--cols:{cols}\">",
        if id.is_empty() {
            String::new()
        } else {
            format!(" id=\"{}\"", esc(id))
        }
    ))
}
pub fn grid_close() -> TrustedHtml {
    TrustedHtml::owned("</div>".into())
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
/// What a module does on a wide sheet (2000px and over, where the sheet has twice its
/// columns): keep its fraction of the sheet, or halve it so twice as many stand in a row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Wide {
    /// Twice its span of the doubled columns: the same fraction of the sheet.
    #[default]
    Keep,
    /// Its span of the doubled columns: half the fraction, so two whole-width modules stand
    /// side by side.
    Halve,
}
impl Wide {
    fn attr(self) -> &'static str {
        match self {
            Wide::Keep => "",
            Wide::Halve => " data-wide=\"halve\"",
        }
    }
}
/// A module on the grid: an `article` spanning `span` of its sheet's columns (all of them on a
/// narrow sheet, half or all on a medium one, twice as many of a wide sheet's), named `label`
/// for a screen reader. Its parts go between this and `module_close`: `.mod-k` (its kind's
/// line: a glyph, a word, who and when), `.mod-t` (its title), `.mod-meta` (its id and facts),
/// a stage strip, `.mod-body`, `.mod-actions`.
pub fn module_open(span: u8, swell: Swell, label: &str) -> TrustedHtml {
    module_open_wide(span, swell, label, Wide::Keep)
}
/// `module_open`, saying what it does on a wide sheet (`Wide::Halve`: two whole-width modules
/// side by side from 2000px).
pub fn module_open_wide(span: u8, swell: Swell, label: &str, wide: Wide) -> TrustedHtml {
    let span = span.clamp(1, 12);
    TrustedHtml::owned(format!(
        "<article class=\"{}\" style=\"--span:{span}\" data-span=\"{span}\"{} aria-label=\"{}\">",
        swell.class(),
        wide.attr(),
        esc(label)
    ))
}
pub fn module_close() -> TrustedHtml {
    TrustedHtml::owned("</article>".into())
}
/// A module as `module_open` draws it, named by its own heading (`labelled`, an element's id)
/// rather than a label, and carrying `class` after its swell's: a page's own kind of module (a
/// step's Now, its waits) that its stream and its tests find by name.
pub fn module_open_as(span: u8, swell: Swell, labelled: &str, class: &str) -> TrustedHtml {
    module_open_with("", span, swell, labelled, class)
}
/// The same with its own `id` (a link's target, what a patch keeps in place).
pub fn module_open_with(
    id: &str,
    span: u8,
    swell: Swell,
    labelled: &str,
    class: &str,
) -> TrustedHtml {
    let span = span.clamp(1, 12);
    TrustedHtml::owned(format!(
        "<article{} class=\"{} {}\" style=\"--span:{span}\" data-span=\"{span}\"{}>",
        if id.is_empty() {
            String::new()
        } else {
            format!(" id=\"{}\"", esc(id))
        },
        swell.class(),
        esc(class),
        if labelled.is_empty() {
            String::new()
        } else {
            format!(" aria-labelledby=\"{}\"", esc(labelled))
        }
    ))
}
/// A column of the sheet that holds modules stacked (`span` columns wide): a band's modules
/// under its head.
pub fn column_open(span: u8) -> TrustedHtml {
    column_open_wide(span, Wide::Keep)
}
/// `column_open`, saying what it does on a wide sheet.
pub fn column_open_wide(span: u8, wide: Wide) -> TrustedHtml {
    let span = span.clamp(1, 12);
    TrustedHtml::owned(format!(
        "<div class=\"mod-col\" style=\"--span:{span}\" data-span=\"{span}\"{}>",
        wide.attr()
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
/// A band's head over a table of strips: its name and line, then each stage's name over its
/// column, the columns as wide as a row's cells (`strip_row_open`), so each name stands over its
/// cell at every width.
pub fn strip_head(id: &str, name: &str, line: &str, stages: &[&str]) -> TrustedHtml {
    let names: String = stages
        .iter()
        .map(|s| format!("<span class=\"sh-stage\">{}</span>", esc(s)))
        .collect();
    TrustedHtml::owned(format!(
        "<div class=\"sec-h sec-strip\"{} style=\"--n:{n}\"><div class=\"sh-lead\"><h2>{}</h2>{}</div><div class=\"sh-stages\" aria-hidden=\"true\">{names}</div><span class=\"sh-end\" aria-hidden=\"true\"></span></div>",
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
        n = stages.len().max(1),
    ))
}

/// A row under a `strip_head` of `n` stages: its head (the unit's status and title) taking what
/// the cells leave, then its `stage_strip`, each cell as wide as every other row's, so a cell
/// stands under its stage's name; then the row's end (its Details). Head and cells stack when
/// their container is narrow.
pub fn strip_row_open(n: usize) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"strip-row\" style=\"--n:{}\">",
        n.max(1)
    ))
}
pub fn strip_row_close() -> TrustedHtml {
    TrustedHtml::owned("</div>".into())
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
    /// Its step's id: its link opens the step in a plan's drawer (`data-step`) and carries the
    /// id `n-<step>` the drawer marks open and gives the focus back to.
    pub step: String,
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
    /// Its step's id (`step`): the cell's link opens it in the drawer.
    pub fn step(mut self, id: impl Into<String>) -> Self {
        self.step = id.into();
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
/// How many times its usual time a run has taken, the one rule every page says it by (a stage's
/// cell, the overrun chip, a step's band and timer, the summary sentence): "2.1×", one decimal
/// below ten ("5×" when it is whole) and a whole number from ten, each rounded down, so no two
/// places disagree and none says more than it has run.
pub fn ratio_text(ratio: f64) -> String {
    let r = if ratio >= 10.0 {
        ratio.floor()
    } else {
        (ratio * 10.0).floor() / 10.0
    };
    // a whole number without its ".0": "5×"
    if r.fract() == 0.0 {
        format!("{r:.0}×")
    } else {
        format!("{r:.1}×")
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
                // a run still going ticks, a quiet one too: its text is the clock's
                _ if !stage.since.is_empty() => {
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
                "<a class=\"sc-a\" href=\"{}\"{}>{inner}</a>",
                esc(&stage.href),
                if stage.step.is_empty() {
                    String::new()
                } else {
                    format!(" id=\"n-{s}\" data-step=\"{s}\"", s = esc(&stage.step))
                }
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
/// The strip as `stage_strip` draws it, with the cell at `at` marked as the page's own stage
/// (`aria-current="step"`, ringed): a step's band shows where it stands in its unit.
pub fn stage_strip_at(label: &str, stages: &[Stage], at: usize) -> TrustedHtml {
    let strip = stage_strip(label, stages).0;
    let mut out = String::with_capacity(strip.len() + 32);
    let mut rest = strip.as_str();
    let mut n = 0;
    while let Some(i) = rest.find("<li class=\"sc") {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if n == at {
            out.push_str("<li aria-current=\"step\" class=\"sc-here sc");
            rest = &rest["<li class=\"sc".len()..];
            out.push_str(rest);
            return TrustedHtml::owned(out);
        }
        out.push_str("<li class=\"sc");
        rest = &rest["<li class=\"sc".len()..];
        n += 1;
    }
    out.push_str(rest);
    TrustedHtml::owned(out)
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
    /// Its name (its unit's, or a loose step's id): what a question to the owner is matched to
    /// (home's squares), never said in the sentence.
    pub name: String,
    /// The recipe it came from, "" for none.
    pub recipe: String,
    /// The unit's state: its steps' first in the status table's order.
    pub shown: Option<Shown>,
    /// Its running step against its stage's usual time, when past it.
    pub over: Option<f64>,
    /// How long its quiet run has written nothing, seconds.
    pub quiet: Option<f64>,
    /// When its quiet run last wrote (RFC 3339), so "quiet for 53m" ticks on the page and a
    /// stream's version leaves it out; "" to say `quiet` as it stands.
    pub quiet_since: String,
}
/// What the summary sentence says (`summary_sentence`): built from the status table's
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
/// The summary sentence under the page's name: "1 question for you. 1 failed, 1 cancelled. 2
/// article units and 1 scan unit at work: 1 quiet for 53m, 1 at 2.1× its usual time. 4
/// waiting. 11 of 19 units done; the last finished 57m ago." It counts and names no unit (the
/// rows do). Each part left out when it has nothing to say; "Nothing has started yet." for a
/// plan with no work at all.
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
        // how many: which recipe made them is the Running section's groups, not the sentence's
        let who = count(running.len(), one, many);
        // the quiet ones and those past their usual time, counted, the longest quiet and the
        // furthest over said: the units are named on their rows, not here
        let mut quiet: Vec<(&UnitFact, f64)> = running
            .iter()
            .filter_map(|u| u.quiet.map(|q| (*u, q)))
            .collect();
        quiet.sort_by(|a, b| b.1.total_cmp(&a.1));
        let over: Vec<f64> = running
            .iter()
            .filter(|u| u.quiet.is_none())
            .filter_map(|u| u.over)
            .collect();
        let furthest = over.iter().copied().fold(0.0_f64, f64::max);
        // since it went quiet, ticking, when the page knows when that was
        let quiet_for = |unit: &UnitFact, secs: f64| {
            if unit.quiet_since.is_empty() {
                esc(&duration_text(secs))
            } else {
                since(&unit.quiet_since).0
            }
        };
        let mut notes: Vec<String> = vec![];
        match quiet.as_slice() {
            [] => {}
            [(u, q)] => notes.push(format!("1 quiet for {}", quiet_for(u, *q))),
            [(u, q), ..] => notes.push(format!(
                "{} quiet, the longest for {}",
                quiet.len(),
                quiet_for(u, *q)
            )),
        }
        match over.len() {
            0 => {}
            1 => notes.push(format!("1 at {} its usual time", ratio_text(furthest))),
            n => notes.push(format!(
                "{n} past their usual time, the furthest at {}",
                ratio_text(furthest)
            )),
        }
        let lead = if notes.is_empty() { "" } else { ": " };
        parts.push(format!("{} at work{lead}{}.", esc(&who), notes.join(", ")));
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
/// What units of no recipe are called where the plan's Running groups them: "1 unit without
/// a recipe", "3 units without a recipe".
pub fn no_recipe(n: usize, noun: (&str, &str)) -> String {
    count(
        n,
        &format!("{} without a recipe", noun.0),
        &format!("{} without a recipe", noun.1),
    )
}
/// "a", "a and b", "a, b and c".
fn join(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

// ---- the page's head and what finished last ---------------------------------------------------

/// The page's head on the paper (`Frame::head`), under the slim nav bar: the page's name as its
/// heading at a reading size (the project's name on its plan, the page's elsewhere), and on the
/// plan and home only the summary sentence under it, one line of large body text.
pub fn page_head(name: &str, line: &TrustedHtml) -> TrustedHtml {
    page_head_html(&TrustedHtml::owned(esc(name)), line)
}
/// The page's head with its heading already drawn, the summary sentence under it.
pub fn page_head_html(name: &TrustedHtml, line: &TrustedHtml) -> TrustedHtml {
    page_head_with(
        &TrustedHtml::default(),
        name,
        &TrustedHtml::default(),
        &if line.as_str().is_empty() {
            TrustedHtml::default()
        } else {
            TrustedHtml::owned(format!("<p class=\"page-line\">{line}</p>"))
        },
    )
}
/// The page's head with a muted note under its name instead of a summary sentence: what a
/// page shows ("42 records on this page"), or why it cannot be drawn.
pub fn page_head_note(name: &str, note: &TrustedHtml) -> TrustedHtml {
    page_head_with(
        &TrustedHtml::default(),
        &TrustedHtml::owned(esc(name)),
        &TrustedHtml::default(),
        &if note.as_str().is_empty() {
            TrustedHtml::default()
        } else {
            TrustedHtml::owned(format!("<p class=\"page-note\">{note}</p>"))
        },
    )
}
/// The page's head whole: its way back (`crumbs`, a breadcrumb nav) over its name, what stands
/// beside the name (`beside`: its Details' "⋯"), and what goes under it (`under`: its lines,
/// its strip, its actions).
pub fn page_head_with(
    crumbs: &TrustedHtml,
    name: &TrustedHtml,
    beside: &TrustedHtml,
    under: &TrustedHtml,
) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"page-head\">{crumbs}<div class=\"ph-name\"><h1>{name}</h1>{beside}</div>{under}</div>"
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
    /// Where it belongs, when the list spans several (home's project name), said muted first.
    pub place: String,
}
/// How many of what finished last a list shows.
pub const LATEST: usize = 4;
/// What finished last, newest first (the plan's Done head, home's Today): a line each, its clock
/// time, its title (its name when it has none) and how long it took, linking to it. The name
/// is not shown: it identifies, and its page has it. Nothing when nothing has finished.
pub fn latest_list(label: &str, items: &[Finished]) -> TrustedHtml {
    if items.is_empty() {
        return TrustedHtml::default();
    }
    let rows: String = items
        .iter()
        .take(LATEST)
        .map(|f| {
            let took = format!(
                "took {}{}",
                duration_text(f.took),
                if f.runs > 1 {
                    format!(", {} runs", f.runs)
                } else {
                    String::new()
                }
            );
            let ended = match f.shown {
                Some(s) if s.spec().band != Band::Done => {
                    format!("<span class=\"lt-how\">{}{}</span> ", mark(s), esc(word(s)))
                }
                _ => String::new(),
            };
            format!(
                "<li><a href=\"{}\"><span class=\"lt-at\">{}</span><span class=\"lt-t\">{}{}</span><span class=\"lt-took\">{ended}{}</span></a></li>",
                esc(&f.href),
                clock(&f.at),
                if f.place.is_empty() {
                    String::new()
                } else {
                    format!("<span class=\"lt-place\">{}</span> ", esc(&f.place))
                },
                esc(if f.title.is_empty() { &f.name } else { &f.title }),
                esc(&took)
            )
        })
        .collect();
    TrustedHtml::owned(format!(
        "<ol class=\"latest\" aria-label=\"{}\">{rows}</ol>",
        esc(label)
    ))
}

// ---- details: what identifies an item, behind its "⋯" ------------------------------------------

/// What identifies an item rather than explains it (its ids, run, fn, engine, tags, recipe,
/// hashes and paths), kept behind one "⋯" control at the item's end (`Details::menu`): a
/// `<details>` that opens without script, inside a `sluice-menu` (Escape and a click elsewhere
/// close it, the focus back on its button). Each id is shown whole with a copy button.
#[derive(Clone, Debug, Default)]
pub struct Details {
    rows: Vec<(String, String)>,
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
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    /// The "⋯" control for the item `about` ("Details of a-12"): a 44px button on a phone that
    /// opens the Details panel, nothing when there is nothing to hold.
    pub fn menu(&self, about: &str) -> TrustedHtml {
        if self.rows.is_empty() {
            return TrustedHtml::default();
        }
        let rows: String = self
            .rows
            .iter()
            .map(|(name, value)| format!("<div><dt>{}</dt><dd>{value}</dd></div>", esc(name)))
            .collect();
        TrustedHtml::owned(format!(
            "{}<details class=\"dm\" data-preserve-attr=\"open\"><summary class=\"dm-b\" aria-label=\"Details of {a}\" title=\"Details\" data-preserve-attr=\"aria-expanded\">{}</summary><div class=\"menu dm-p\" role=\"group\" aria-label=\"Details of {a}\" data-preserve-attr=\"style\"><p class=\"dm-h\">Details</p><dl class=\"dm-l\">{rows}</dl></div></details>{}",
            super::menu_open(),
            icon(Icon::Ellipsis, 18, ""),
            super::menu_close(),
            a = esc(about),
        ))
    }
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
/// How a progress value or an output's preview is set, decided from its JSON type and shape
/// alone, never its name (DESIGN.md, Details).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueSet {
    /// A number, a boolean, nothing, or a few short words: shown as it is (the first of them
    /// that is a number or a word of at most `SHORT_VALUE` characters at display size).
    Show,
    /// What identifies rather than explains: a hash, an id, a path, a URL, a long unbroken
    /// token, a list or an object. Kept in the item's Details.
    Detail,
    /// Words: shown clamped to two lines, the whole text in the item's Details.
    Prose,
}
/// The longest string a value may be to be set at display size.
pub const SHORT_VALUE: usize = 8;
/// The longest a run of words may be to be shown whole rather than clamped as prose.
pub const SHORT_WORDS: usize = 32;
/// The longest unbroken token that still reads as a word.
pub const LONG_TOKEN: usize = 24;
impl ValueSet {
    pub fn of(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::String(s) => Self::of_text(s),
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => ValueSet::Detail,
            _ => ValueSet::Show,
        }
    }
    /// A string's shape: a URL, a path, a hash, an id or a long token identifies; a few words
    /// show; more words are prose.
    pub fn of_text(text: &str) -> Self {
        let s = text.trim();
        if s.contains("://") {
            return ValueSet::Detail;
        }
        if s.split_whitespace().nth(1).is_none() {
            return if token_identifies(s) {
                ValueSet::Detail
            } else {
                ValueSet::Show
            };
        }
        // words: a path or a hash among them makes them prose to clamp, never shown whole
        let any_token = s.split_whitespace().any(token_identifies);
        if s.chars().count() <= SHORT_WORDS && !any_token {
            ValueSet::Show
        } else {
            ValueSet::Prose
        }
    }
    /// Whether a value is a number or a word short enough for display size.
    pub fn display(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Number(_) | serde_json::Value::Bool(_) => true,
            serde_json::Value::String(s) => {
                ValueSet::of_text(s) == ValueSet::Show && s.trim().chars().count() <= SHORT_VALUE
            }
            _ => false,
        }
    }
}
/// One unbroken token that identifies rather than says: a path (a slash with a letter), a hash
/// (seven or more hex digits), an id (letters and digits joined by a separator, six or more
/// characters), or anything longer than `LONG_TOKEN`.
fn token_identifies(t: &str) -> bool {
    let t = t.trim_matches(|c: char| matches!(c, '(' | ')' | ',' | '.' | ';' | ':' | '"' | '\''));
    let n = t.chars().count();
    let letters = t.chars().any(char::is_alphabetic);
    let digits = t.chars().any(|c| c.is_ascii_digit());
    if n > LONG_TOKEN {
        return true;
    }
    if (t.contains('/') || t.contains('\\')) && letters {
        return true;
    }
    if n >= 7 && t.chars().all(|c| c.is_ascii_hexdigit()) && digits {
        return true;
    }
    n >= 6 && letters && digits && t.contains(['-', '_', ':', '#', '@'])
}
/// A value as words: a string as it is, nothing as "none", anything else as its JSON.
fn value_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "none".into(),
        other => other.to_string(),
    }
}
/// The margin module (`Swell::Margin`, two columns at the sheet's right): its title and words,
/// its running time on the blue, then each progress field in the order reported as its shape
/// says (`ValueSet`: the first number or short word at display size, other short values as they
/// are, prose clamped to two lines), when it last reported, and its "⋯" Details holding its
/// step, its run and every value whole, the hashes, ids and paths among them.
pub fn margin_module(run: &LongRun) -> TrustedHtml {
    let mut fields = String::new();
    let lead = run.fields.iter().position(|(_, v)| ValueSet::display(v));
    let mut details = Details::new().id("Step", &run.name).text(
        "Run",
        &if run.run > 0 {
            run.run.to_string()
        } else {
            String::new()
        },
    );
    for (i, (key, v)) in run.fields.iter().enumerate() {
        let text = value_text(v);
        match ValueSet::of(v) {
            ValueSet::Detail => {
                details = details.id(key, &text);
            }
            ValueSet::Prose => {
                details = details.text(key, &text);
                fields.push_str(&format!(
                    "<div class=\"mm-f mm-prose\"><dt>{}</dt><dd>{}</dd></div>",
                    esc(key),
                    esc(&text)
                ));
            }
            ValueSet::Show => fields.push_str(&format!(
                "<div class=\"{}\"><dt>{}</dt><dd>{}</dd></div>",
                if Some(i) == lead {
                    "mm-f mm-lead"
                } else {
                    "mm-f"
                },
                esc(key),
                esc(&text)
            )),
        }
    }
    let shown_title = if run.title.is_empty() {
        &run.name
    } else {
        &run.title
    };
    TrustedHtml::owned(format!(
        "{open}<div class=\"mm-head\"><h3 class=\"mm-t\"><a href=\"{href}\">{title}</a></h3>{menu}</div>{doc}<div class=\"mm-run\"><span class=\"mm-k\">{run_mark}running</span><span class=\"mm-time\">{since}</span><span class=\"sweep\" aria-hidden=\"true\"></span></div>{fields}{at}{close}",
        open = module_open(2, Swell::Margin, &format!("{shown_title}, running")).as_str(),
        href = esc(&run.href),
        title = esc(shown_title),
        menu = details.menu(shown_title),
        doc = if run.doc.is_empty() {
            String::new()
        } else {
            format!("<p class=\"mm-doc\">{}</p>", esc(&run.doc))
        },
        run_mark = mark(Shown::Running),
        since = since(&run.since),
        fields = if fields.is_empty() {
            if run.fields.is_empty() {
                "<p class=\"mm-none\">It has reported no progress.</p>".to_owned()
            } else {
                String::new()
            }
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
/// The button that selects a unit to trace: its head (its kind's line, its id and its title,
/// phrasing content only; its strip and the rest go after it), and the part of it that opens
/// in place while it is traced (`trace_more_open`).
/// Its name says what pressing does, "Trace <name>", so the card's state, its strip's image
/// and its title are not read as one run-together label; `described` names the element that
/// says how it stands (its kind's line), read as its description ("" for none).
pub fn trace_button_open(name: &str, described: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<button type=\"button\" class=\"trace-pick\" data-trace-pick aria-pressed=\"false\" aria-expanded=\"false\" data-preserve-attr=\"aria-pressed aria-expanded\" aria-label=\"Trace {}\"{}>",
        esc(name),
        if described.is_empty() {
            String::new()
        } else {
            format!(" aria-describedby=\"{}\"", esc(described))
        }
    ))
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
        let unit = |recipe: &str, shown, over, quiet| UnitFact {
            recipe: recipe.into(),
            shown: Some(shown),
            over,
            quiet,
            ..UnitFact::default()
        };
        let units = [
            unit("article", Shown::Failed, None, None),
            unit("article", Shown::Cancelled, None, None),
            unit("article", Shown::Running, Some(2.14), None),
            unit("scan", Shown::Quiet, None, Some(3180.0)),
            unit("article", Shown::Pending, None, None),
            unit("article", Shown::Succeeded, None, None),
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
            "<a class=\"ask\" href=\"#for-you\">1 question for you</a>. 1 failed, 1 cancelled. 2 units at work: 1 quiet for 53m, 1 at 2.1× its usual time. 1 waiting. 1 of 6 units done."
        );
        assert_eq!(
            summary_sentence(&Summary::default()).as_str(),
            "Nothing has started yet."
        );
    }

    #[test]
    fn one_rule_says_how_far_past_its_usual_time_a_run_is() {
        // one decimal below ten, a whole number from ten, each rounded down: a cell, a chip
        // and a step's head never say "20×" beside "19×"
        assert_eq!(ratio_text(2.19), "2.1×");
        assert_eq!(ratio_text(4.5), "4.5×");
        assert_eq!(ratio_text(9.99), "9.9×");
        assert_eq!(ratio_text(5.0), "5×");
        assert_eq!(ratio_text(10.0), "10×");
        assert_eq!(ratio_text(19.7), "19×");
        assert!(overrun(19.7).as_str().contains("<span>19× usual</span>"));
    }

    #[test]
    fn a_busy_summary_counts_its_overruns_and_quiet_runs_and_names_no_unit() {
        let run = |recipe: &str, over: Option<f64>, quiet: Option<f64>| UnitFact {
            recipe: recipe.into(),
            shown: Some(if quiet.is_some() {
                Shown::Quiet
            } else {
                Shown::Running
            }),
            over,
            quiet,
            ..UnitFact::default()
        };
        let ratios = [5.7, 2.2, 9.8, 3.1, 1.6, 4.0, 2.5, 1.2];
        let mut units: Vec<UnitFact> = ratios.iter().map(|r| run("lane", Some(*r), None)).collect();
        units.push(run("", Some(1.1), None));
        let says = |units: &[UnitFact]| {
            summary_sentence(&Summary {
                units,
                noun: ("unit", "units"),
                ..Summary::default()
            })
            .as_str()
            .to_owned()
        };
        assert_eq!(
            says(&units),
            "9 units at work: 9 past their usual time, the furthest at 9.8×."
        );
        units.extend([
            run("lane", None, Some(600.0)),
            run("lane", None, Some(4000.0)),
            run("lane", None, Some(120.0)),
        ]);
        assert_eq!(
            says(&units),
            "12 units at work: 3 quiet, the longest for 1h 6m, 9 past their usual time, the furthest at 9.8×."
        );
        assert_eq!(says(&[run("", None, None)]), "1 unit at work.");
    }

    #[test]
    fn a_value_is_shown_or_kept_in_details_by_its_shape_alone() {
        use serde_json::json;
        let of = |v: serde_json::Value| ValueSet::of(&v);
        // numbers, booleans and short words show
        assert_eq!(of(json!(2)), ValueSet::Show);
        assert_eq!(of(json!(true)), ValueSet::Show);
        assert_eq!(of(json!("green")), ValueSet::Show);
        assert_eq!(of(json!("3 of 7 done")), ValueSet::Show);
        assert_eq!(of(json!("12/40")), ValueSet::Show);
        // hashes, ids, paths, URLs and long tokens go to Details
        assert_eq!(
            of(json!("168272b5c838681da58f6457eda242e413485bd6")),
            ValueSet::Detail
        );
        assert_eq!(of(json!("168272b5c8")), ValueSet::Detail);
        assert_eq!(of(json!("a-12-review")), ValueSet::Detail);
        assert_eq!(of(json!("/srv/almanac/report.json")), ValueSet::Detail);
        assert_eq!(of(json!("https://example.org/a")), ValueSet::Detail);
        assert_eq!(
            of(json!("abcdefghijklmnopqrstuvwxyzabcdef")),
            ValueSet::Detail
        );
        assert_eq!(of(json!(["a", "b"])), ValueSet::Detail);
        // prose stays, clamped, whole in Details; a path inside it never makes it a token
        assert_eq!(
            of(json!(
                "Checked every entry against the field notes; two disagree."
            )),
            ValueSet::Prose
        );
        assert_eq!(
            of(json!("2 red of 641 at /srv/almanac/report.json")),
            ValueSet::Prose
        );
        // the first number or short word is set at display size
        assert!(ValueSet::display(&json!(2)));
        assert!(ValueSet::display(&json!("green")));
        assert!(!ValueSet::display(&json!("3 of 7 done and more")));
        assert!(!ValueSet::display(&json!("168272b5c8")));
    }

    #[test]
    fn the_margin_keeps_what_identifies_in_its_details() {
        use serde_json::json;
        let html = margin_module(&LongRun {
            name: "survey-watch".into(),
            title: "Watches the survey".into(),
            run: 4,
            since: "2026-10-09T08:00:00Z".into(),
            fields: vec![
                (
                    "head".into(),
                    json!("168272b5c838681da58f6457eda242e413485bd6"),
                ),
                ("open".into(), json!(2)),
                (
                    "note".into(),
                    json!("Checked every entry against the field notes; two disagree."),
                ),
            ],
            ..LongRun::default()
        })
        .0;
        let visible = html
            .split("<details class=\"dm\"")
            .next()
            .unwrap()
            .to_owned()
            + html.split("</details>").nth(1).unwrap_or("");
        assert!(!visible.contains("168272b5"), "{visible}");
        assert!(!visible.contains("survey-watch"), "{visible}");
        assert!(
            visible.contains("<div class=\"mm-f mm-lead\"><dt>open</dt><dd>2</dd></div>"),
            "{visible}"
        );
        assert!(visible.contains("mm-prose"), "{visible}");
        let details = html.split("<details class=\"dm\"").nth(1).unwrap();
        assert!(
            details.contains("168272b5c838681da58f6457eda242e413485bd6"),
            "{details}"
        );
        assert!(details.contains("survey-watch"), "{details}");
    }
}
