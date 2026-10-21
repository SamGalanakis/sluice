//! The Synthesis kit (DESIGN.md, The grid, The page head, Modules, Details, Status
//! presentation): the parts every page composes on the module grid. The module grid, a module
//! and its swell, a band section's head, a unit's stage strip, the summary sentence, the page's
//! head, what finished last, an item's Details behind its "⋯", and a value set by its shape.
//! Each returns escaped HTML.
//!
//! Generic by rule: a stage is whatever a unit's recipe names it, a count is a status table
//! count, and a progress field is whatever a step reported. Nothing here knows a project.
use super::{Shown, Tally, count, duration, duration_text, esc, since, word};
use crate::views::TrustedHtml;
use sluice_model::shown::Band;

// ---- the module grid ---------------------------------------------------------------------------

/// The module grid (`ui::grid_open`): a sheet of `cols` columns (12) that modules span
/// (`module_open`). Layout only: it has no behaviour, so it is a plain element.
///
/// The sheet is fluid and lays out by its own width (DESIGN.md, The grid): half its columns
/// under 640px with every module across it, its columns from 640px (a module half the sheet or
/// whole under 1200px); the frame keeps it to 1440px.
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
    /// It needs a look (stopped: failed, cancelled, stale): a well on the pale ochre. A
    /// question to the owner is no module but `ui::QuestionCard`.
    Look,
}
impl Swell {
    fn class(self) -> &'static str {
        match self {
            Swell::Plain => "mod",
            Swell::Look => "mod swell-look",
        }
    }
}
/// A module on the grid: an `article` spanning `span` of its sheet's columns (all of them on a
/// narrow sheet, half or all on a medium one), named `label`
/// for a screen reader. Its parts go between this and `module_close`: `.mod-k` (its kind's
/// line: a glyph, a word, who and when), `.mod-t` (its title), `.mod-meta` (its id and facts),
/// a stage strip, `.mod-body`, `.mod-actions`.
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
    let span = span.clamp(1, 12);
    TrustedHtml::owned(format!(
        "<div class=\"mod-col\" style=\"--span:{span}\" data-span=\"{span}\">"
    ))
}
pub fn column_close() -> TrustedHtml {
    TrustedHtml::owned("</div>".into())
}

// ---- the stage track ------------------------------------------------------------------------

/// One stage of a unit's track: a stage of its recipe (or a step of a unit with no recipe), in
/// recipe order.
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
    /// Its step's page.
    pub href: String,
    /// A note across this and the cells after it ("waits for review"): `cols` cells wide.
    pub gap: Option<(String, u8)>,
    /// Its step's id: its link opens the step in a plan's drawer (`data-step`) and carries the
    /// id `n-<step>` the drawer marks open and gives the focus back to.
    pub step: String,
    /// Its state's word when its step says it otherwise ("bypassed" for set by hand).
    pub word: Option<&'static str>,
    /// A small tag a stage not reached carries, and what it says ("bypass", "bypass by o").
    pub tag: Option<(String, String)>,
    /// Its step waits on the owner's answer: it reads "asks you", in teal.
    pub ask: bool,
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
    pub fn href(mut self, href: impl Into<String>) -> Self {
        self.href = href.into();
        self
    }
    /// Its step's id (`step`): the cell's link opens it in the drawer.
    pub fn step(mut self, id: impl Into<String>) -> Self {
        self.step = id.into();
        self
    }
    pub fn word(mut self, word: &'static str) -> Self {
        self.word = Some(word);
        self
    }
    pub fn asks(mut self, ask: bool) -> Self {
        self.ask = ask;
        self
    }
    pub fn tag(mut self, tag: impl Into<String>, said: impl Into<String>) -> Self {
        self.tag = Some((tag.into(), said.into()));
        self
    }
}
/// How a state stands, from its row in the status table alone: done, at work, needs a look, or
/// not reached (the day line's bars and a state's badge read it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    /// Not reached: a waiting state, held, queued, paused, blocked.
    Empty,
    /// Done.
    Done,
    /// Running.
    Run,
    /// Needs a look: failed, cancelled, stale, quiet.
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
        if shown == Shown::Parked {
            Cell::Empty
        } else if spec.band == Band::Done {
            Cell::Done
        } else if spec.attention {
            Cell::Look
        } else if spec.band == Band::Running {
            Cell::Run
        } else {
            Cell::Empty
        }
    }
    /// Its name in a class (`sv-c-<key>`, `sv-b-<key>`).
    pub fn key(self) -> &'static str {
        match self {
            Cell::Empty => "empty",
            Cell::Done => "done",
            Cell::Run => "run",
            Cell::Look => "look",
        }
    }
}
/// How a stage reads on its track, from its state's row in the status table: its class, the
/// glyph before its name and the word after it ("" for none).
fn stage_look(stage: &Stage, current: bool, next: bool) -> (&'static str, String, String) {
    if stage.ask {
        return ("ask", "<i></i>".into(), "asks you".into());
    }
    // not reached and tagged (a bypass and who set it): the tag says more than "next"
    if let Some((tag, _)) = &stage.tag
        && stage.shown.is_none_or(|s| s.spec().band != Band::Done)
        && !current
    {
        return (if next { "next" } else { "pend" }, String::new(), esc(tag));
    }
    let Some(shown) = stage.shown else {
        return if next {
            ("next", String::new(), "next".into())
        } else {
            (
                "pend",
                String::new(),
                match &stage.tag {
                    Some((tag, _)) => tag.clone(),
                    None => String::new(),
                },
            )
        };
    };
    let spec = shown.spec();
    let said = stage.word.unwrap_or_else(|| word(shown));
    let time = |s: f64| {
        if stage.since.is_empty() {
            duration(s).0
        } else {
            since(&stage.since).0
        }
    };
    let took = stage
        .seconds
        .map(|s| format!(" <em>{}</em>", time(s)))
        .unwrap_or_default();
    match shown {
        Shown::Succeeded | Shown::Manual => (
            "ok",
            "<i aria-hidden=\"true\">✓</i>".into(),
            if current {
                format!("{}{took}", esc(said))
            } else {
                String::new()
            },
        ),
        Shown::Running | Shown::Finishing | Shown::External => {
            ("run", "<i></i>".into(), took.trim_start().to_owned())
        }
        Shown::Quiet => (
            "quiet",
            "<i aria-hidden=\"true\">●</i>".into(),
            format!("{}{took}", esc(said)),
        ),
        Shown::Failed => ("fail", "<i aria-hidden=\"true\">✕</i>".into(), esc(said)),
        Shown::Stale => ("quiet", String::new(), esc(said)),
        Shown::Paused | Shown::Held | Shown::Queued | Shown::Blocked => {
            ("held", "<i aria-hidden=\"true\">‖</i>".into(), esc(said))
        }
        _ if spec.band == Band::Done => ("ok", String::new(), esc(said)),
        _ if next => ("next", String::new(), "next".into()),
        _ => (
            "pend",
            String::new(),
            if spec.caption {
                esc(said)
            } else {
                String::new()
            },
        ),
    }
}
/// The stage a unit stands at: its first not done (a stop, a run, a wait), else its last.
pub fn current_stage(stages: &[Stage]) -> usize {
    stages
        .iter()
        .position(|s| s.ask || s.shown.is_none_or(|shown| shown.spec().band != Band::Done))
        .unwrap_or(stages.len().saturating_sub(1))
}
/// A unit's stages as its track (DESIGN.md, The stage track): the stage before the one it
/// stands at, that one (green while it runs, with how long; teal when it asks the owner; its
/// state's word when it stopped) and the next, a hairline between; the stages before and after
/// those folded into a count that names them. One stage is drawn alone. `at` is the stage the
/// page is about (a step's own page), which the window then stands around, marked current;
/// else the unit's own place. A list named `label` ("Stages of a-12"); each stage with a page
/// links to it. On a phone only the stage it stands at shows, with its place ("2/6").
pub fn stage_track(label: &str, stages: &[Stage], at: Option<usize>) -> TrustedHtml {
    let stages: Vec<&Stage> = stages.iter().filter(|s| s.gap.is_none()).collect();
    if stages.is_empty() {
        return TrustedHtml::default();
    }
    let owned: Vec<Stage> = stages.iter().map(|s| (*s).clone()).collect();
    let cur = at
        .unwrap_or_else(|| current_stage(&owned))
        .min(stages.len() - 1);
    let from = cur.saturating_sub(1);
    let to = (cur + 1).min(stages.len() - 1);
    let names = |range: &[&Stage]| {
        range
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut items = String::new();
    if from > 0 {
        items.push_str(&format!(
            "<li class=\"st fold-n\" title=\"{}\"><span class=\"vh\">{} earlier: </span>+{}</li>",
            esc(&names(&stages[..from])),
            from,
            from
        ));
    }
    for (i, stage) in stages.iter().enumerate().take(to + 1).skip(from) {
        let (class, glyph, said) = stage_look(stage, i == cur, i == cur + 1);
        let name = esc(&stage.name);
        // what the stage is called and where it stands, a screen reader's words for it
        let words = format!(
            "{glyph}<span class=\"st-n\">{name}</span>{}",
            if said.is_empty() {
                String::new()
            } else {
                format!(" <span class=\"st-w\">{said}</span>")
            }
        );
        let body = if stage.href.is_empty() {
            words
        } else {
            format!(
                "<a href=\"{}\"{}>{words}</a>",
                esc(&stage.href),
                if stage.step.is_empty() {
                    String::new()
                } else {
                    format!(" data-step=\"{}\"", esc(&stage.step))
                }
            )
        };
        items.push_str(&format!(
            "<li class=\"st {class}{}\"{}{}>{body}</li>",
            if i == cur { " cur" } else { "" },
            if at == Some(i) {
                " aria-current=\"step\""
            } else {
                ""
            },
            stage
                .shown
                .map(|s| format!(" data-state=\"{}\"", s.key()))
                .unwrap_or_default(),
        ));
    }
    if to + 1 < stages.len() {
        let later = stages.len() - to - 1;
        items.push_str(&format!(
            "<li class=\"st fold-n\" title=\"{}\"><span class=\"vh\">{} later: </span>+{}</li>",
            esc(&names(&stages[to + 1..])),
            later,
            later
        ));
    }
    if stages.len() > 1 {
        items.push_str(&format!(
            "<li class=\"of\" aria-hidden=\"true\">{}/{}</li>",
            cur + 1,
            stages.len()
        ));
    }
    TrustedHtml::owned(format!(
        "<ol class=\"stages\" aria-label=\"{}\">{items}</ol>",
        esc(label)
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
    /// How long its quiet run has written nothing, seconds.
    pub quiet: Option<f64>,
    /// When its quiet run last wrote (RFC 3339), so "quiet for 53m" ticks on the page and a
    /// stream's version leaves it out; "" to say `quiet` as it stands.
    pub quiet_since: String,
}
/// What the summary sentence says (`summary_sentence`): built from the status table's
/// counts, quiet runs and open questions alone, naming recipes by their own names.
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
/// The summary sentence under the page's name: "1 question for you. 1 failed, 1 cancelled. 3
/// units at work: 1 quiet for 53m. 4 waiting. 11 of 19 units done; the last finished 57m ago." It counts and names no unit (the
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
        // the quiet ones counted and the longest quiet said: the units are named on their
        // rows, not here
        let mut quiet: Vec<(&UnitFact, f64)> = running
            .iter()
            .filter_map(|u| u.quiet.map(|q| (*u, q)))
            .collect();
        quiet.sort_by(|a, b| b.1.total_cmp(&a.1));
        // since it went quiet, ticking, when the page knows when that was
        let quiet_for = |unit: &UnitFact, secs: f64| {
            if unit.quiet_since.is_empty() {
                esc(&duration_text(secs))
            } else {
                since(&unit.quiet_since).0
            }
        };
        let note = match quiet.as_slice() {
            [] => String::new(),
            [(u, q)] => format!(": 1 quiet for {}", quiet_for(u, *q)),
            [(u, q), ..] => format!(
                ": {} quiet, the longest for {}",
                quiet.len(),
                quiet_for(u, *q)
            ),
        };
        parts.push(format!("{} at work{note}.", esc(&who)));
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

/// How many units Finished lists when no look is kept (the plan's "Finished lately").
pub const LATEST: usize = 4;

/// A time of day: "17:49 UTC" on the server, so it says its zone until the page's clock reads
/// it; the reader's own clock then (`data-clock`); the day and minute in UTC in its
/// title.
pub fn clock(at: &str) -> TrustedHtml {
    let shown = match at.get(11..16) {
        Some(hm) => format!("{hm} UTC"),
        None => at.to_owned(),
    };
    TrustedHtml::owned(format!(
        "<time data-clock datetime=\"{a}\" title=\"{t}\">{}</time>",
        esc(&shown),
        a = esc(at),
        t = esc(&super::at_text(at))
    ))
}

// ---- a value by its shape --------------------------------------------------------------------

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
