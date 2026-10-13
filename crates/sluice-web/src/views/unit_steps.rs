//! The step view (DESIGN.md, The step view): a unit drawn step by step, a row a step in graph
//! order. A rail at the left draws its gates (a node a step: filled done, ringed not reached,
//! turning while it runs; a join or a branch with arrows; a condition named at its row), then
//! each step's name, its state (glyph and word from the status table), its progress, its runs
//! as bars on one time axis shared by every row, and how long its run took. Its foot is the
//! unit's last message. The plan's rows open to it in place, the unit page is it, and a step's
//! Runs tab draws its unit's with the step ringed.
//!
//! **The time axis** runs from the unit's first start to now while it is live (rounded up to a
//! step of a tenth of the time drawn or more, so the drawing moves once a step, never with the
//! clock), else to its last end. It is linear, with two exceptions that keep every step in
//! sight: a run shorter than the axis can show is drawn at a least width (3px), and a stretch
//! where nothing ran that is a tenth or more of the time drawn to scale collapses into a marked
//! break of a fixed share of the width, named for how long it lasted. The axis is in percent,
//! so the page's clock moves the "now" line and a running bar's end between patches from the
//! last stretch's start, place and rate (`data-now`) without the server drawing again.
use super::TrustedHtml;
use super::board::{Endpoint, Relation, RelationKind, UnitView};
use super::step::StepView;
use super::ui::{self, Cell, Shown, esc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write;

/// One run of a step (a scatter's round of item runs is one), from the step's current
/// generation: when it started and ended and how it ended.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunSpan {
    /// RFC 3339 UTC.
    pub started: String,
    pub finished: Option<String>,
    /// How it reads in the status table (`sluice_model::shown`): running (not ended), else as its
    /// result says (succeeded, failed, cancelled, …); none when no result is recorded.
    pub outcome: Option<Shown>,
    /// A scatter round's item runs; 0 for a plain run.
    pub items: usize,
    /// Its start and end in seconds since the epoch (no end while it runs).
    pub from: f64,
    pub to: Option<f64>,
}
impl RunSpan {
    fn seconds(&self) -> Option<f64> {
        self.to.map(|to| (to - self.from).max(0.0))
    }
}
/// Seconds since the epoch of a julian day, as SQLite's `julianday` gives it.
pub fn from_julian(day: f64) -> f64 {
    (day - 2_440_587.5) * 86_400.0
}

/// What a step reported of how far it has got: a part of a whole ("12/40", a bar under it),
/// a short value ("draft 2"), or nothing ("—").
#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    None,
    Text(String),
    Part { done: f64, total: f64, text: String },
}
impl Progress {
    /// A step's: a scatter's items done of all, else the first short field of its live
    /// `step_progress` (a part when it reads "a/b" or "a of b").
    pub fn of(step: &StepView) -> Self {
        if let Some(total) = step.total.filter(|t| *t > 0) {
            return Progress::Part {
                done: step.done as f64,
                total: total as f64,
                text: format!("{}/{total}", step.done),
            };
        }
        let Some(progress) = step.progress.as_ref().filter(|p| p.live) else {
            return Progress::None;
        };
        for field in progress.shown() {
            let value = field.value.trim();
            if let Some((done, total)) = part(value) {
                return Progress::Part {
                    done,
                    total,
                    text: value.to_owned(),
                };
            }
            if !value.is_empty()
                && ui::ValueSet::of_text(value) == ui::ValueSet::Show
                && value.chars().count() <= 12
            {
                return Progress::Text(value.to_owned());
            }
        }
        Progress::None
    }
}
/// "12/40" or "12 of 40" as a part of a whole.
fn part(text: &str) -> Option<(f64, f64)> {
    let (a, b) = text.split_once('/').or_else(|| text.split_once(" of "))?;
    let (a, b) = (a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?);
    (b > 0.0 && a >= 0.0).then_some((a.min(b), b))
}

/// How long a step's current run took or has run: the view's Span.
#[derive(Clone, Debug, PartialEq)]
pub enum Span {
    None,
    Took(f64),
    /// Running since (RFC 3339): ticking.
    Since(String),
}
impl Span {
    pub fn of(step: &StepView) -> Self {
        match step.shown_timing() {
            Some(t) if t.finished.is_none() => Span::Since(t.started.clone()),
            Some(t) => Span::Took(t.seconds),
            None => Span::None,
        }
    }
}

/// One step as the view draws it.
#[derive(Clone, Debug)]
pub struct Step<'a> {
    pub id: String,
    /// Its name in its unit: its stage, else its id less the unit's.
    pub name: String,
    /// Its whole name, for its link's title ("" for none).
    pub title: String,
    pub href: String,
    pub shown: Shown,
    /// Its run's number and its earlier runs' ends ("run 3"), when it has had more than one.
    pub runs: TrustedHtml,
    pub progress: Progress,
    pub spans: &'a [RunSpan],
    pub span: Span,
    /// What it waits for outside its unit, in words ("waits for Terns"), "" for nothing.
    pub note: String,
}
/// A gate between two of the unit's steps: `to` comes after `from`, on a condition named
/// `when` ("ok", "not ok") or none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub when: String,
}
/// The view's foot: the unit's last message, when it has one.
#[derive(Clone, Debug)]
pub struct Latest {
    /// When it was sent (RFC 3339).
    pub at: String,
    /// Its sender in words as HTML ("work", "@lead", "you"; "orchestrator" muted).
    pub from: String,
    pub text: String,
    /// Its thread.
    pub href: String,
}
/// A unit's step view, ready to draw.
#[derive(Clone, Debug)]
pub struct View<'a> {
    /// What it is named for a screen reader: "Steps of Terns".
    pub label: String,
    /// Its unit's owner in words (`@lead`, "you"), small and muted in its head; "" when
    /// unowned.
    pub owner: String,
    pub steps: Vec<Step<'a>>,
    pub edges: Vec<Edge>,
    pub latest: Option<Latest>,
    /// The step ringed (its own on its page and in the drawer), "" for none.
    pub selected: String,
    /// A row opens its step in the drawer (`data-step`), on the plan and in the drawer.
    pub opens: bool,
    /// The unit is not done: its axis runs to now.
    pub live: bool,
    /// Seconds since the epoch, as the page read the clock.
    pub now: f64,
}

/// A collapsed wait's share of the axis, percent.
const BREAK: f64 = 4.0;
/// The steps "now" is rounded up to, seconds: the least at least a tenth of the time drawn.
const NOW_STEPS: [f64; 10] = [
    300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0, 10_800.0, 21_600.0, 43_200.0, 86_400.0,
];
/// A wait collapses when it is this share of the time drawn to scale, or more.
const COLLAPSE: f64 = 0.1;
/// At most this many waits collapse (the longest).
const MOST_BREAKS: usize = 4;
/// A stretch shorter than this is no wait worth breaking, seconds.
const WAIT_MIN: f64 = 60.0;
/// The tick spacings, seconds.
const NICE: [f64; 22] = [
    1.0,
    2.0,
    5.0,
    10.0,
    15.0,
    30.0,
    60.0,
    120.0,
    300.0,
    600.0,
    900.0,
    1800.0,
    3600.0,
    7200.0,
    10_800.0,
    21_600.0,
    43_200.0,
    86_400.0,
    172_800.0,
    604_800.0,
    1_209_600.0,
    2_592_000.0,
];
/// At most this many spacings across the time drawn to scale.
const TICKS: f64 = 4.0;
/// The least distance between two labels on the axis, percent.
const LABEL_GAP: f64 = 17.0;
/// The rail's lanes: their spacing and the room either side, px; at most this many.
const LANE: f64 = 14.0;
const RAIL_PAD: f64 = 8.0;
const MOST_LANES: usize = 5;
/// A node's radius, px.
const NODE: f64 = 5.0;

/// Where an axis from `t0` ends while the unit is live: `now` rounded up to a step of at least a
/// tenth of the time since `t0` (`NOW_STEPS`), so the drawing moves once a step, not with the
/// clock.
fn now_mark(t0: f64, now: f64) -> f64 {
    let elapsed = (now - t0).max(0.0);
    let step = NOW_STEPS
        .into_iter()
        .find(|s| *s >= elapsed * COLLAPSE)
        .unwrap_or_else(|| (elapsed * COLLAPSE / 86_400.0).ceil() * 86_400.0);
    t0 + ((elapsed / step).floor() + 1.0) * step
}

/// A stretch of the axis: from, to, collapsed into a break.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Piece {
    a: f64,
    b: f64,
    collapsed: bool,
}
/// The time axis: the stretches from the first start to its end, each to scale or a break.
#[derive(Clone, Debug, PartialEq)]
struct Axis {
    t0: f64,
    t1: f64,
    pieces: Vec<Piece>,
    /// The time drawn to scale, seconds (at least one).
    scaled: f64,
    breaks: usize,
    /// It runs to now.
    live: bool,
}
impl Axis {
    /// Over `spans` (every run of the unit), to now (rounded up) when `live`, else to the last
    /// end; none before anything ran.
    fn new(spans: &[&RunSpan], live: bool, now: f64) -> Option<Self> {
        let t0 = spans.iter().map(|s| s.from).reduce(f64::min)?;
        let known = spans
            .iter()
            .flat_map(|s| [Some(s.from), s.to])
            .flatten()
            .fold(t0, f64::max);
        let running = spans.iter().any(|s| s.to.is_none());
        let live = live || running;
        let t1 = if live {
            now_mark(t0, now.max(known))
        } else {
            known.max(t0 + 1.0)
        };
        let mut runs: Vec<(f64, f64)> = spans
            .iter()
            .map(|s| (s.from, s.to.unwrap_or(t1).max(s.from)))
            .collect();
        runs.sort_by(|a, b| a.0.total_cmp(&b.0));
        // what ran, merged; the stretches between are waits, and so is the stretch from the
        // last end to now
        let mut active: Vec<(f64, f64)> = vec![];
        for (from, to) in runs {
            match active.last_mut() {
                Some(last) if from <= last.1 => last.1 = last.1.max(to),
                _ => active.push((from, to)),
            }
        }
        let mut waits: Vec<(f64, f64)> = active.windows(2).map(|w| (w[0].1, w[1].0)).collect();
        let end = active.last().map_or(t0, |a| a.1);
        if end < t1 {
            waits.push((end, t1));
        }
        let ran: f64 = active.iter().map(|(a, b)| b - a).sum();
        // collapse the long waits; each collapsed shrinks what is drawn to scale, so again
        // until none more is long enough
        let mut collapsed = vec![false; waits.len()];
        loop {
            let scaled = ran
                + waits
                    .iter()
                    .zip(&collapsed)
                    .filter(|(_, c)| !**c)
                    .map(|((a, b), _)| b - a)
                    .sum::<f64>();
            let mut longest: Vec<usize> = (0..waits.len())
                .filter(|&i| !collapsed[i])
                .filter(|&i| {
                    let len = waits[i].1 - waits[i].0;
                    len >= WAIT_MIN && len >= COLLAPSE * scaled && len < scaled
                })
                .collect();
            longest
                .sort_by(|&a, &b| (waits[b].1 - waits[b].0).total_cmp(&(waits[a].1 - waits[a].0)));
            let room = MOST_BREAKS - collapsed.iter().filter(|c| **c).count();
            if longest.is_empty() || room == 0 {
                break;
            }
            for i in longest.into_iter().take(room) {
                collapsed[i] = true;
            }
        }
        let mut pieces = vec![];
        for (i, (a, b)) in active.iter().enumerate() {
            pieces.push(Piece {
                a: *a,
                b: *b,
                collapsed: false,
            });
            if let Some((wa, wb)) = waits.get(i)
                && wb > wa
            {
                pieces.push(Piece {
                    a: *wa,
                    b: *wb,
                    collapsed: collapsed[i],
                });
            }
        }
        let scaled = pieces
            .iter()
            .filter(|p| !p.collapsed)
            .map(|p| p.b - p.a)
            .sum::<f64>()
            .max(1.0);
        Some(Self {
            t0,
            t1,
            breaks: collapsed.iter().filter(|c| **c).count(),
            pieces,
            scaled,
            live,
        })
    }
    /// The share of the width drawn to scale, percent.
    fn room(&self) -> f64 {
        100.0 - BREAK * self.breaks as f64
    }
    /// A moment's place, percent of the width.
    fn at(&self, t: f64) -> f64 {
        let mut at = 0.0;
        for p in &self.pieces {
            if t <= p.a {
                break;
            }
            let part = (t.min(p.b) - p.a) / (p.b - p.a).max(f64::MIN_POSITIVE);
            at += if p.collapsed {
                BREAK * part.clamp(0.0, 1.0)
            } else {
                self.room() * (t.min(p.b) - p.a) / self.scaled
            };
        }
        at.clamp(0.0, 100.0)
    }
    /// The last stretch as the page's clock reads it: its start, its place and how far a second
    /// moves along it, percent ("a p k").
    fn now_data(&self) -> String {
        let Some(last) = self.pieces.last() else {
            return String::new();
        };
        let rate = if last.collapsed {
            BREAK / (last.b - last.a).max(1.0)
        } else {
            self.room() / self.scaled
        };
        format!("{:.0} {:.4} {:.8}", last.a, self.at(last.a), rate)
    }
    /// The axis's labels: each break's length, then the start and the ticks of a nice spacing
    /// over the time drawn to scale, each label clear of the others; none in the last stretch
    /// while live, where "now" stands.
    fn labels(&self) -> Vec<(f64, String)> {
        let mut out: Vec<(f64, String)> = vec![];
        let clear =
            |out: &[(f64, String)], at: f64| out.iter().all(|(p, _)| (p - at).abs() >= LABEL_GAP);
        for p in self.pieces.iter().filter(|p| p.collapsed) {
            let at = (self.at(p.a) + self.at(p.b)) / 2.0;
            if clear(&out, at) && !(self.live && at > 100.0 - LABEL_GAP) {
                out.push((at, format!("+{}", tick_text(p.b - p.a))));
            }
        }
        if clear(&out, 0.0) {
            out.push((0.0, "0s".into()));
        }
        let spacing = NICE
            .into_iter()
            .find(|s| self.scaled / s <= TICKS)
            .unwrap_or(NICE[NICE.len() - 1]);
        let mut k = 1.0;
        while self.t0 + k * spacing <= self.t1 && k < 1000.0 {
            let t = self.t0 + k * spacing;
            k += 1.0;
            if self
                .pieces
                .iter()
                .any(|p| p.collapsed && t > p.a && t < p.b)
            {
                continue;
            }
            let at = self.at(t);
            // while live, "now" stands at the end
            let limit = if self.live { 74.0 } else { 96.0 };
            if at <= limit && clear(&out, at) {
                out.push((at, tick_text(t - self.t0)));
            }
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out
    }
}
/// A tick's words: "0s", "30s", "15m", "1h", "1h 30m", "2d", "1d 6h".
fn tick_text(seconds: f64) -> String {
    let s = seconds.round().max(0.0) as u64;
    let (d, h, m) = (s / 86_400, s % 86_400 / 3_600, s % 3_600 / 60);
    match s {
        0..60 => format!("{s}s"),
        60..3_600 => format!("{m}m"),
        3_600..86_400 if m == 0 => format!("{h}h"),
        3_600..86_400 => format!("{h}h {m}m"),
        _ if h == 0 => format!("{d}d"),
        _ => format!("{d}d {h}h"),
    }
}

/// One row of the rail: the node's lane, the lanes passing by, the lanes coming in (each with
/// whether it ends in an arrow) and going out.
#[derive(Clone, Debug, Default, PartialEq)]
struct Rail {
    lane: usize,
    through: Vec<usize>,
    ins: Vec<(usize, bool)>,
    outs: Vec<usize>,
}
/// Lays the rail out over rows `0..n` (each edge from an earlier row to a later): a node takes
/// the lane of the first edge into it (the spine, lane 0, when none), its nearest successor
/// goes on in its lane and each other in the lowest lane free, never one freed on its own row,
/// so a join and a branch never share a line. An edge ends in an arrow at a join, from a
/// branch, or coming in from another lane.
fn rail(n: usize, edges: &[(usize, usize)]) -> (Vec<Rail>, usize) {
    let mut outs: Vec<Vec<usize>> = vec![vec![]; n];
    let mut ins = vec![0usize; n];
    for &(a, b) in edges {
        if a < b && b < n && !outs[a].contains(&b) {
            outs[a].push(b);
            ins[b] += 1;
        }
    }
    for o in &mut outs {
        o.sort_unstable();
    }
    // each lane: the row it leads to and whether it ends in an arrow
    let mut lanes: Vec<Option<(usize, bool)>> = vec![];
    let mut rows = vec![];
    let mut width = 1;
    for i in 0..n {
        let coming: Vec<usize> = (0..lanes.len())
            .filter(|&l| lanes[l].is_some_and(|(t, _)| t == i))
            .collect();
        let lane = coming
            .first()
            .copied()
            .or_else(|| lanes.iter().position(Option::is_none))
            .unwrap_or(lanes.len())
            .min(MOST_LANES - 1);
        let mut row = Rail {
            lane,
            ..Rail::default()
        };
        for &l in &coming {
            let arrow = lanes[l].is_some_and(|(_, a)| a) || l != lane || ins[i] > 1;
            row.ins.push((l, arrow));
            lanes[l] = None;
        }
        row.through = (0..lanes.len())
            .filter(|&l| l != lane && lanes[l].is_some())
            .collect();
        let branch = outs[i].len() > 1;
        for (k, &to) in outs[i].iter().enumerate() {
            let l = if k == 0 && lanes.get(lane).is_none_or(Option::is_none) {
                lane
            } else {
                (0..)
                    .find(|&l| {
                        l != lane
                            && !coming.contains(&l)
                            && lanes.get(l).is_none_or(Option::is_none)
                    })
                    .unwrap_or(0)
                    .min(MOST_LANES - 1)
            };
            if l >= lanes.len() {
                lanes.resize(l + 1, None);
            }
            lanes[l] = Some((to, branch));
            row.outs.push(l);
        }
        width = width.max(lanes.len()).max(lane + 1);
        rows.push(row);
    }
    (rows, width.min(MOST_LANES))
}
/// `edges` less each one another path already implies (a value passed from the first step to
/// the last of a chain is a gate the chain draws): the rail draws the fewest lines that say the
/// same order. An edge in `keep` (a condition, named at its row) is always drawn.
fn reduce(edges: &[(usize, usize)], keep: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let reaches = |from: usize, to: usize, skip: (usize, usize)| {
        let mut seen = vec![from];
        let mut next = vec![from];
        while let Some(at) = next.pop() {
            for &(a, b) in edges {
                if a == at && (a, b) != skip && !seen.contains(&b) {
                    if b == to {
                        return true;
                    }
                    seen.push(b);
                    next.push(b);
                }
            }
        }
        false
    };
    edges
        .iter()
        .copied()
        .filter(|&(a, b)| keep.contains(&(a, b)) || !reaches(a, b, (a, b)))
        .collect()
}
/// A lane's x, px: lane 0 at the right, by the names.
fn lane_x(lane: usize, width: usize) -> f64 {
    RAIL_PAD + (width - 1 - lane.min(width - 1)) as f64 * LANE
}

/// The order a unit's steps are drawn in: `edges` respected (a step after every step it comes
/// after), else as given.
fn graph_order(steps: &[Step<'_>], edges: &[Edge]) -> Vec<usize> {
    let at: BTreeMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    let mut before = vec![0usize; steps.len()];
    let mut after: Vec<Vec<usize>> = vec![vec![]; steps.len()];
    for e in edges {
        if let (Some(&a), Some(&b)) = (at.get(e.from.as_str()), at.get(e.to.as_str()))
            && a != b
            && !after[a].contains(&b)
        {
            after[a].push(b);
            before[b] += 1;
        }
    }
    let mut order = vec![];
    let mut done = vec![false; steps.len()];
    while order.len() < steps.len() {
        // the first ready step as given; a cycle (never in a compiled plan) breaks at its first
        let next = (0..steps.len())
            .find(|&i| !done[i] && before[i] == 0)
            .or_else(|| (0..steps.len()).find(|&i| !done[i]))
            .expect("a step is left");
        done[next] = true;
        order.push(next);
        for &b in &after[next] {
            before[b] = before[b].saturating_sub(1);
        }
    }
    order
}

impl View<'_> {
    pub fn html(&self) -> TrustedHtml {
        let order = graph_order(&self.steps, &self.edges);
        let row_of: BTreeMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(row, &i)| (self.steps[i].id.as_str(), row))
            .collect();
        let mut pairs = vec![];
        let mut kept = vec![];
        let mut when: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
        for e in &self.edges {
            if let (Some(&a), Some(&b)) = (row_of.get(e.from.as_str()), row_of.get(e.to.as_str())) {
                pairs.push((a, b));
                if !e.when.is_empty() {
                    kept.push((a, b));
                }
                if !e.when.is_empty() {
                    let at = when.entry(b).or_default();
                    if !at.contains(&e.when.as_str()) {
                        at.push(&e.when);
                    }
                }
            }
        }
        let (rails, width) = rail(order.len(), &reduce(&pairs, &kept));
        let all: Vec<&RunSpan> = self.steps.iter().flat_map(|s| s.spans).collect();
        let axis = Axis::new(&all, self.live, self.now);
        let rail_w = 2.0 * RAIL_PAD + (width - 1) as f64 * LANE;

        let mut out = String::new();
        let host = ui::Host::new("sluice-steps")
            .attr("class", "sv")
            .some("selected", &self.selected)
            .keep("selected");
        out.push_str(host.open().as_str());
        let _ = write!(
            out,
            "<div class=\"sv-g{}\" style=\"--rail:{rail_w:.0}px;--now:100%\"{} role=\"group\" aria-label=\"{}\">",
            if axis.is_some() { "" } else { " sv-untimed" },
            match &axis {
                Some(axis) if axis.live => format!(" data-now=\"{}\"", axis.now_data()),
                _ => String::new(),
            },
            esc(&self.label)
        );
        // the head: the columns' names, the axis's labels over the timeline
        let mut labels = String::new();
        if let Some(axis) = &axis {
            for (at, text) in axis.labels() {
                let edge = if at < 4.0 {
                    " at-start"
                } else if at > 92.0 {
                    " at-end"
                } else {
                    ""
                };
                let _ = write!(
                    labels,
                    "<span class=\"sv-tick{edge}\" style=\"left:{at:.3}%\">{}</span>",
                    esc(&text)
                );
            }
            if axis.live {
                labels.push_str("<span class=\"sv-tick sv-nowlabel\">now</span>");
            }
        }
        // and its unit's owner after the first, small and muted (the columns' names are for the
        // eye; a screen reader hears the owner alone)
        let _ = write!(
            out,
            "<div class=\"sv-hd{}\"><span class=\"sv-h-name\"><span class=\"sv-h-w\" aria-hidden=\"true\">Step</span>{}</span><span class=\"sv-h-st\" aria-hidden=\"true\">State</span><span class=\"sv-h-pr\" aria-hidden=\"true\">Progress</span>{}<span class=\"sv-h-sp\" aria-hidden=\"true\">Span</span></div>",
            if self.owner.is_empty() {
                ""
            } else {
                " has-own"
            },
            if self.owner.is_empty() {
                String::new()
            } else {
                format!("<span class=\"own\">{}</span>", esc(&self.owner))
            },
            if axis.is_some() {
                format!("<span class=\"sv-axis\" aria-hidden=\"true\">{labels}</span>")
            } else {
                String::new()
            }
        );
        // behind the rows' timelines: a line at each label, each break, the line at now
        if let Some(axis) = &axis {
            out.push_str("<div class=\"sv-ov\" aria-hidden=\"true\">");
            for (at, _) in axis.labels() {
                let _ = write!(out, "<i class=\"sv-grid\" style=\"left:{at:.3}%\"></i>");
            }
            for p in axis.pieces.iter().filter(|p| p.collapsed) {
                let _ = write!(
                    out,
                    "<i class=\"sv-break\" style=\"left:{:.3}%;width:{BREAK}%\" title=\"{}\"></i>",
                    axis.at(p.a),
                    esc(&format!("Nothing ran for {}", ui::duration_text(p.b - p.a)))
                );
            }
            if axis.live {
                out.push_str("<i class=\"sv-now\"></i>");
            }
            out.push_str("</div>");
        }
        out.push_str("<ol class=\"sv-rows\">");
        for (row, &i) in order.iter().enumerate() {
            let step = &self.steps[i];
            let r = &rails[row];
            let cell = Cell::of(Some(step.shown));
            let selected = step.id == self.selected;
            let _ = write!(
                out,
                "<li class=\"sv-row sv-c-{}{}\" data-sv=\"{}\">",
                cell.key(),
                if selected { " is-sel" } else { "" },
                esc(&step.id)
            );
            out.push_str(&rail_svg(r, width, cell, step.shown));
            // its name, the condition it runs on before it, what holds it outside its unit
            out.push_str("<span class=\"sv-name\">");
            if let Some(words) = when.get(&row) {
                let _ = write!(
                    out,
                    "<span class=\"sv-when\">if {}</span>",
                    esc(&words.join(" or "))
                );
            }
            let _ = write!(
                out,
                "<a class=\"sv-a\" href=\"{}\"{}{}{}>{}</a>",
                esc(&step.href),
                if self.opens {
                    format!(" data-step=\"{}\"", esc(&step.id))
                } else {
                    String::new()
                },
                if step.title.is_empty() || step.title == step.name {
                    String::new()
                } else {
                    format!(" title=\"{}\"", esc(&step.title))
                },
                if selected {
                    " aria-current=\"step\""
                } else {
                    ""
                },
                esc(&step.name)
            );
            if !step.note.is_empty() {
                let _ = write!(out, "<span class=\"sv-note\">{}</span>", esc(&step.note));
            }
            out.push_str("</span>");
            let _ = write!(
                out,
                "<span class=\"sv-st\">{}{}</span>",
                ui::status(step.shown),
                step.runs
            );
            out.push_str("<span class=\"sv-pr\">");
            match &step.progress {
                Progress::None => out.push_str(NONE),
                Progress::Text(text) => {
                    let _ = write!(out, "<span class=\"num\">{}</span>", esc(text));
                }
                Progress::Part { done, total, text } => {
                    let _ = write!(
                        out,
                        "<span class=\"num\">{}</span><span class=\"sv-part\" aria-hidden=\"true\"><i style=\"width:{:.1}%\"></i></span>",
                        esc(text),
                        done / total * 100.0
                    );
                }
            }
            out.push_str("</span>");
            if let Some(axis) = &axis {
                out.push_str(&track(axis, step));
            }
            out.push_str("<span class=\"sv-sp\">");
            match &step.span {
                Span::None => out.push_str(NONE),
                Span::Took(s) => out.push_str(ui::duration(*s).as_str()),
                Span::Since(at) => out.push_str(ui::since(at).as_str()),
            }
            out.push_str("</span></li>");
        }
        out.push_str("</ol>");
        if let Some(latest) = &self.latest {
            let _ = write!(
                out,
                "<p class=\"sv-foot\"><span class=\"sv-fk\">Last message</span>{}<b>{}</b><a href=\"{}\">{}</a></p>",
                ui::ago(&latest.at),
                latest.from,
                esc(&latest.href),
                esc(&latest.text)
            );
        }
        out.push_str("</div>");
        out.push_str(host.close().as_str());
        TrustedHtml::owned(out)
    }
}
/// A cell with nothing to say: a dash, read as nothing.
const NONE: &str = "<span class=\"sv-none\" aria-hidden=\"true\">—</span>";

/// A row's piece of the rail: the lines passing by, the edges in (an arrow where one is due)
/// and out, and its node over them: filled when done, ringed when not reached, a turning arc
/// while it runs, sand when it wants a look.
fn rail_svg(r: &Rail, width: usize, cell: Cell, shown: Shown) -> String {
    let x = |l: usize| lane_x(l, width);
    let me = x(r.lane);
    // the lines fill the row's height, whatever it is: the svg stands in its cell, out of flow
    let mut out = "<span class=\"sv-rail\" aria-hidden=\"true\"><svg>".to_owned();
    for &l in &r.through {
        let _ = write!(
            out,
            "<line x1=\"{0}\" y1=\"0\" x2=\"{0}\" y2=\"100%\"/>",
            x(l)
        );
    }
    for &(l, arrow) in &r.ins {
        if l == r.lane {
            let _ = write!(out, "<line x1=\"{me}\" y1=\"0\" x2=\"{me}\" y2=\"50%\"/>");
            if arrow {
                let _ = write!(
                    out,
                    "<svg x=\"{me}\" y=\"50%\" overflow=\"visible\"><path class=\"sv-arrow\" d=\"M-3.5 {a} L0 {b} L3.5 {a}\"/></svg>",
                    a = -NODE - 5.0,
                    b = -NODE - 1.5
                );
            }
        } else {
            let from = x(l);
            let end = if from < me {
                me - NODE - 1.5
            } else {
                me + NODE + 1.5
            };
            let _ = write!(
                out,
                "<line x1=\"{from}\" y1=\"0\" x2=\"{from}\" y2=\"50%\"/><line x1=\"{from}\" y1=\"50%\" x2=\"{end}\" y2=\"50%\"/>"
            );
            if arrow {
                let d = if from < me { -3.5 } else { 3.5 };
                let _ = write!(
                    out,
                    "<svg x=\"{end}\" y=\"50%\" overflow=\"visible\"><path class=\"sv-arrow\" d=\"M{d} -3.5 L0 0 L{d} 3.5\"/></svg>"
                );
            }
        }
    }
    for &l in &r.outs {
        let to = x(l);
        if l == r.lane {
            let _ = write!(
                out,
                "<line x1=\"{me}\" y1=\"50%\" x2=\"{me}\" y2=\"100%\"/>"
            );
        } else {
            let _ = write!(
                out,
                "<line x1=\"{me}\" y1=\"50%\" x2=\"{to}\" y2=\"50%\"/><line x1=\"{to}\" y1=\"50%\" x2=\"{to}\" y2=\"100%\"/>"
            );
        }
    }
    let turning = cell == Cell::Run && shown.spec().turns;
    let _ = write!(
        out,
        "<circle class=\"sv-node sv-n-{}{}\" cx=\"{me}\" cy=\"50%\" r=\"{NODE}\"/></svg></span>",
        cell.key(),
        if turning { " spin" } else { "" }
    );
    out
}
/// A row's timeline: a bar a run on the shared axis, its current run whole and the earlier
/// faint, a run still going open to now; read as its runs in words.
fn track(axis: &Axis, step: &Step<'_>) -> String {
    let mut bars = String::new();
    let mut said = vec![];
    let last = step.spans.len().saturating_sub(1);
    for (n, span) in step.spans.iter().enumerate() {
        let outcome = span.outcome.unwrap_or(Shown::Pending);
        let left = axis.at(span.from);
        let end = span.to.unwrap_or(axis.t1).max(span.from);
        let width = (axis.at(end) - left).max(0.0);
        let open = span.to.is_none();
        let _ = write!(
            bars,
            "<i class=\"sv-b sv-b-{}{}\" style=\"--l:{left:.3}%;--w:{width:.3}%\"{}></i>",
            Cell::of(Some(outcome)).key(),
            if n < last { " is-old" } else { "" },
            if open {
                format!(" data-open=\"{left:.3}\"")
            } else {
                String::new()
            }
        );
        let what = match span.outcome {
            Some(shown) => shown.word(),
            None => "no result recorded",
        };
        said.push(match span.seconds() {
            Some(s) => format!("Run {} {what} after {}", n + 1, ui::duration_words(s)),
            None => format!("Run {} running since {}", n + 1, ui::at_text(&span.started)),
        });
    }
    format!(
        "<span class=\"sv-tr\" role=\"img\" aria-label=\"{}\">{bars}</span>",
        esc(&if said.is_empty() {
            "No run yet".to_owned()
        } else {
            said.join(". ")
        })
    )
}

/// The gates between `steps`, from the plan's relations: an `after` entry or a value passed,
/// one edge a pair, named by its condition when it is one ("ok", "not ok").
pub fn edges(relations: &[Relation], steps: &[&StepView]) -> Vec<Edge> {
    let mine = |e: &Endpoint| match e {
        Endpoint::Step(id) => steps.iter().any(|s| &s.id == id).then(|| id.to_string()),
        _ => None,
    };
    let mut out: Vec<Edge> = vec![];
    for r in relations {
        let (Some(from), Some(to)) = (mine(&r.from), mine(&r.to)) else {
            continue;
        };
        let when = match r.kind {
            RelationKind::Condition | RelationKind::NegatedCondition => r.label.clone(),
            _ => String::new(),
        };
        match out.iter_mut().find(|e| e.from == from && e.to == to) {
            Some(e) if e.when.is_empty() => e.when = when,
            Some(_) => {}
            None => out.push(Edge { from, to, when }),
        }
    }
    out
}
/// A unit's steps in the order its view starts from: its recipe's stages in recipe order, then
/// any step no stage names; a unit of no recipe its steps in plan order.
pub fn ordered(unit: &UnitView) -> Vec<&StepView> {
    let mut out: Vec<&StepView> = unit
        .stages
        .iter()
        .filter_map(|stage| unit.stage_step(stage))
        .collect();
    for step in &unit.steps {
        if !out.iter().any(|s| s.id == step.id) {
            out.push(step);
        }
    }
    out
}
/// A unit's step view: its steps, the gates between them from `relations`, its last message;
/// `opens` its rows in the drawer, `selected` the step ringed ("" for none).
pub fn of_unit<'a>(
    unit: &'a UnitView,
    relations: &[Relation],
    opens: bool,
    selected: &str,
) -> View<'a> {
    let steps = ordered(unit);
    let edges = edges(relations, &steps);
    let project = steps.first().map(|s| s.project);
    let views = steps
        .iter()
        .map(|s| Step {
            id: s.id.to_string(),
            name: unit.step_name(s),
            // its own title, when it says what the unit's does not
            title: if [unit.heading(), unit.whole_heading()].contains(&s.whole_heading()) {
                String::new()
            } else {
                s.whole_heading().to_owned()
            },
            href: s.href(),
            shown: s.shown(),
            runs: s.retries_html(),
            progress: Progress::of(s),
            spans: s.timing.as_ref().map_or(&[][..], |t| t.spans.as_slice()),
            span: Span::of(s),
            note: unit
                .waits_of(s)
                .first()
                .map(|w| format!("waits for {}", w.name.text(48)))
                .unwrap_or_default(),
        })
        .collect();
    let latest = (!unit.last_message.trim().is_empty())
        .then(|| {
            project.map(|p| Latest {
                at: unit.changed.clone(),
                from: unit.sender_html(),
                text: crate::markdown::cut(&crate::markdown::plain(&unit.last_message), 200),
                href: unit.last_thread_href(&p),
            })
        })
        .flatten();
    View {
        label: format!("Steps of {}", unit.heading()),
        owner: unit.owner_words(),
        steps: views,
        edges,
        latest,
        selected: selected.to_owned(),
        opens,
        live: !unit.done,
        now: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(from: f64, to: Option<f64>, outcome: &str) -> RunSpan {
        RunSpan {
            started: "2026-10-07T00:00:00Z".into(),
            finished: to.map(|_| "x".into()),
            outcome: Shown::from_key(outcome),
            items: 0,
            from,
            to,
        }
    }

    #[test]
    fn a_long_wait_breaks_and_the_runs_keep_their_scale() {
        const H: f64 = 3600.0;
        let a = span(0.0, Some(600.0), "succeeded");
        let b = span(600.0, Some(4800.0), "succeeded");
        let c = span(4800.0 + 24.0 * H, Some(5400.0 + 24.0 * H), "succeeded");
        let axis = Axis::new(&[&a, &b, &c], false, 0.0).unwrap();
        assert_eq!(axis.breaks, 1);
        // 5400 seconds to scale share the 96% the break leaves
        assert!((axis.at(600.0) - 96.0 * 600.0 / 5400.0).abs() < 1e-9);
        assert!((axis.at(4800.0 + 24.0 * H) - (96.0 * 4800.0 / 5400.0 + 4.0)).abs() < 1e-9);
        assert_eq!(axis.at(5400.0 + 24.0 * H), 100.0);
        let labels = axis.labels();
        assert!(labels.iter().any(|(_, t)| t == "+1d"), "{labels:?}");
        assert_eq!(labels[0], (0.0, "0s".into()));
    }

    #[test]
    fn a_live_unit_runs_to_now_rounded_up_so_the_clock_alone_redraws_nothing() {
        let a = span(0.0, Some(600.0), "succeeded");
        let b = span(600.0, None, "running");
        let at = |now| Axis::new(&[&a, &b], true, now).unwrap();
        assert_eq!(at(1000.0).t1, 1200.0);
        assert_eq!(at(1000.0), at(1100.0));
        assert_ne!(at(1000.0), at(1300.0));
        assert_eq!(now_mark(0.0, 18_000.0), 19_800.0);
        // the clock moves now along the last stretch from its start
        assert_eq!(at(1000.0).now_data(), "0 0.0000 0.08333333");
    }

    #[test]
    fn a_unit_idle_between_steps_breaks_the_idle_stretch_up_to_now() {
        // done an hour's work, then idle for a day: the stretch to now is a break
        let a = span(0.0, Some(3600.0), "succeeded");
        let axis = Axis::new(&[&a], true, 90_000.0).unwrap();
        assert_eq!(axis.breaks, 1);
        assert!(axis.pieces.last().unwrap().collapsed);
        assert_eq!(axis.at(3600.0), 96.0);
    }

    #[test]
    fn the_rail_runs_a_chain_down_one_lane_and_lays_a_branch_and_its_join_beside_it() {
        let (rows, width) = rail(3, &[(0, 1), (1, 2)]);
        assert_eq!(width, 1);
        assert!(rows.iter().all(|r| r.lane == 0 && r.through.is_empty()));
        assert_eq!(rows[1].ins, vec![(0, false)]);
        // a fans out to b and c, which join at d
        let (rows, width) = rail(4, &[(0, 1), (0, 2), (1, 3), (2, 3)]);
        assert_eq!(width, 2);
        assert_eq!(rows[0].outs, vec![0, 1]);
        assert_eq!((rows[1].lane, rows[1].through.clone()), (0, vec![1]));
        assert_eq!(rows[2].lane, 1);
        assert_eq!(rows[3].lane, 0);
        assert_eq!(rows[3].ins, vec![(0, true), (1, true)]);
    }

    #[test]
    fn an_edge_a_chain_implies_is_left_to_the_chain_but_a_condition_is_drawn() {
        let edges = [(0, 1), (1, 2), (0, 2), (2, 3)];
        assert_eq!(reduce(&edges, &[]), [(0, 1), (1, 2), (2, 3)]);
        assert_eq!(reduce(&edges, &[(0, 2)]), edges);
    }

    #[test]
    fn steps_are_drawn_after_what_they_come_after() {
        let empty: &[RunSpan] = &[];
        let step = |id: &str| Step {
            id: id.into(),
            name: id.into(),
            title: String::new(),
            href: String::new(),
            shown: Shown::Pending,
            runs: TrustedHtml::default(),
            progress: Progress::None,
            spans: empty,
            span: Span::None,
            note: String::new(),
        };
        let steps = [step("c"), step("a"), step("b")];
        let edge = |a: &str, b: &str| Edge {
            from: a.into(),
            to: b.into(),
            when: String::new(),
        };
        assert_eq!(
            graph_order(&steps, &[edge("a", "c"), edge("b", "c")]),
            [1, 2, 0]
        );
    }

    #[test]
    fn progress_reads_a_part_or_a_short_value() {
        assert_eq!(part("12/40"), Some((12.0, 40.0)));
        assert_eq!(part("3 of 4"), Some((3.0, 4.0)));
        assert_eq!(part("a/b"), None);
        assert_eq!(tick_text(5400.0), "1h 30m");
        assert_eq!(tick_text(86_400.0), "1d");
    }
}
