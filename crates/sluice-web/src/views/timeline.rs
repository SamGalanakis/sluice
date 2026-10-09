//! A unit's timeline (DESIGN.md, Unit and Step): a row a stage, a bar a run on one time axis,
//! retries as successive bars. Long waits collapse: a stretch where nothing ran that is a tenth
//! or more of the time drawn to scale becomes a 48px break named "waited 6h 12m". A run still
//! going runs to the axis's right end, "now", with an open end. That end is the read's time
//! rounded up to a step of a tenth of the time drawn or more (5 minutes at least: `now_mark`), so
//! what is drawn changes once a step, never with each tick of the clock; the run's time in words
//! is a `<time data-since>` the page ticks. Drawn on the server with percentages; no legend, the
//! bars carry their glyphs and their words.
use super::TrustedHtml;
use super::ui::{Shown, duration_text, duration_words, esc};
use askama::Template;
use serde::Serialize;

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
    fn open(&self) -> bool {
        self.to.is_none()
    }
    fn seconds(&self) -> Option<f64> {
        self.to.map(|to| (to - self.from).max(0.0))
    }
}

/// A row to draw: a step of the unit, named by its stage (or its id), and its runs.
pub struct Lane<'a> {
    pub label: String,
    /// Its whole name, for the label's title.
    pub title: String,
    pub href: String,
    /// The step whose page shows the timeline: its row is marked.
    pub current: bool,
    pub spans: &'a [RunSpan],
}

/// A collapsed wait's fixed width, px.
const BREAK: f64 = 48.0;
/// The steps "now" is rounded up to, seconds: the least at least a tenth of the time drawn.
const NOW_STEPS: [f64; 10] = [
    300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0, 10_800.0, 21_600.0, 43_200.0, 86_400.0,
];
/// A wait collapses when it is this share of the time drawn to scale, or more.
const COLLAPSE: f64 = 0.1;
/// At most this many waits collapse (the longest), so the bars keep their room.
const MOST_BREAKS: usize = 4;
/// The least width the track is drawn at (a timeline under 490px reads as lines of words; less
/// its stage column and insets, the track is wider than this): what
/// keeps the axis's labels apart and decides which runs are too short to draw.
const TRACK_MIN: f64 = 380.0;
/// The least space between two labels' anchors on the axis, px.
const LABEL_GAP: f64 = 72.0;
/// A run narrower than this at the least width is drawn as its glyph alone, px.
const DOT: f64 = 6.0;
/// A stretch shorter than this is no wait worth naming, seconds.
const WAIT_MIN: f64 = 60.0;

#[derive(Clone, Debug, PartialEq, Serialize, Template)]
#[template(path = "timeline.html")]
pub struct Timeline {
    pub rows: Vec<Row>,
    pub breaks: Vec<Break>,
    pub ticks: Vec<Tick>,
    /// A run still goes: the axis ends in "now".
    pub open: bool,
    /// Two or more stages at its end with no run yet, each its name and page: one row, "land,
    /// landed, close, rm: no run yet", not a row each.
    pub rest: Vec<(String, String)>,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Row {
    pub label: String,
    pub title: String,
    pub href: String,
    pub current: bool,
    pub marks: Vec<Mark>,
    /// How its last run stands, for its glyph in the phone's line (none without a run).
    pub last: Option<Shown>,
    /// "3 runs · 52m · waited 6h 12m", its running time a ticking `<time data-since>`.
    pub words: String,
    /// The runs in words for a screen reader: the track's name.
    pub said: String,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Mark {
    /// How it reads; a run with no result recorded draws pending's glyph and bar.
    pub outcome: Shown,
    /// CSS `left` and `width` of its bar (the part known to have run, for a run still going).
    pub left: String,
    pub width: String,
    /// Too short to draw: its glyph alone, at its start.
    pub dot: bool,
    /// Still running: it runs to the axis's end, "now", its end open.
    pub open: bool,
    /// CSS `left` of its glyph's centre: where it ended (the axis's end while it runs).
    pub end: String,
    pub title: String,
}
impl Row {
    /// Its last run's glyph; nothing without a run.
    pub fn last_glyph(&self) -> TrustedHtml {
        self.last
            .map(super::ui::glyph)
            .unwrap_or_else(|| TrustedHtml::owned(String::new()))
    }
}
impl Mark {
    pub fn glyph(&self) -> TrustedHtml {
        super::ui::glyph(self.outcome)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Break {
    pub left: String,
    /// "waited 6h 12m"
    pub words: String,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Tick {
    pub left: String,
    pub text: String,
    /// Which edge of the label sits on its place: "start", "mid" or "end".
    pub align: &'static str,
}

/// The axis: the stretches from the first start to the last known moment, each run to scale or
/// collapsed to a break.
struct Axis {
    t0: f64,
    pieces: Vec<(f64, f64, bool)>,
    /// The time drawn to scale, seconds (at least a second).
    scaled: f64,
    breaks: usize,
    open: bool,
}
/// A place on the axis: its share of the scaled width and how many breaks lie before it
/// (fractional inside one).
#[derive(Clone, Copy, Debug, PartialEq)]
struct At {
    share: f64,
    breaks: f64,
}
impl Axis {
    fn new(spans: &[&RunSpan], now: f64) -> Self {
        let t0 = spans.iter().map(|s| s.from).fold(f64::INFINITY, f64::min);
        let known = spans
            .iter()
            .flat_map(|s| [Some(s.from), s.to])
            .flatten()
            .fold(t0, f64::max);
        let open = spans.iter().any(|s| s.open());
        // a run still going runs to "now", rounded up to its step
        let t1 = if open {
            now_mark(t0, now.max(known))
        } else {
            known
        };
        let mut runs: Vec<(f64, f64)> = spans
            .iter()
            .map(|s| (s.from, s.to.unwrap_or(t1).max(s.from)))
            .collect();
        runs.sort_by(|a, b| a.0.total_cmp(&b.0));
        // what ran, merged; the stretches between are waits
        let mut active: Vec<(f64, f64)> = vec![];
        for (from, to) in runs {
            match active.last_mut() {
                Some(last) if from <= last.1 => last.1 = last.1.max(to),
                _ => active.push((from, to)),
            }
        }
        let waits: Vec<(f64, f64)> = active.windows(2).map(|w| (w[0].1, w[1].0)).collect();
        let ran: f64 = active.iter().map(|(a, b)| b - a).sum();
        // collapse the long waits; each collapsed shrinks what is drawn to scale, so again
        // until none more is long enough (Temporal's fixpoint)
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
                    len >= WAIT_MIN && len >= COLLAPSE * scaled
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
            pieces.push((*a, *b, false));
            if let Some((wa, wb)) = waits.get(i) {
                pieces.push((*wa, *wb, collapsed[i]));
            }
        }
        let scaled = pieces
            .iter()
            .filter(|p| !p.2)
            .map(|(a, b, _)| b - a)
            .sum::<f64>()
            .max(1.0);
        Self {
            t0,
            breaks: collapsed.iter().filter(|c| **c).count(),
            pieces,
            scaled,
            open,
        }
    }
    fn at(&self, t: f64) -> At {
        let (mut share, mut breaks) = (0.0, 0.0);
        for &(a, b, collapsed) in &self.pieces {
            if t <= a {
                break;
            }
            let part = ((t.min(b) - a) / (b - a).max(f64::MIN_POSITIVE)).clamp(0.0, 1.0);
            if collapsed {
                breaks += part;
            } else {
                share += (t.min(b) - a) / self.scaled;
            }
        }
        At {
            share: share.min(1.0),
            breaks,
        }
    }
    /// A place as CSS: the scaled share of what the breaks leave, and the breaks before it.
    fn css(&self, at: At) -> String {
        let share = at.share;
        if self.breaks == 0 {
            return format!("{:.3}%", share * 100.0);
        }
        format!(
            "calc((100% - {}px) * {:.5} + {:.1}px)",
            self.breaks as f64 * BREAK,
            share,
            at.breaks * BREAK
        )
    }
    /// The span between two places as CSS, a width.
    fn css_width(&self, from: At, to: At) -> String {
        self.css(At {
            share: (to.share - from.share).max(0.0),
            breaks: (to.breaks - from.breaks).max(0.0),
        })
    }
    /// A place in px when the track is at its least width.
    fn px(&self, at: At) -> f64 {
        (TRACK_MIN - self.breaks as f64 * BREAK) * at.share + at.breaks * BREAK
    }
}

impl Timeline {
    /// The timeline of `lanes` (a unit's steps, in its order), or none when none has run.
    pub fn new(lanes: &[Lane<'_>], now: f64) -> Option<Self> {
        let all: Vec<&RunSpan> = lanes.iter().flat_map(|l| l.spans).collect();
        if all.is_empty() {
            return None;
        }
        let axis = Axis::new(&all, now);
        let rows = lanes
            .iter()
            .map(|lane| Self::row(&axis, lane, &all))
            .collect();
        // the axis's labels, the most needed first: each wait collapsed (the longest first),
        // then the start, then the end ("now" while a run goes)
        let mut breaks = vec![];
        let mut candidates: Vec<(f64, Tick, f64)> = vec![];
        let mut label = |place: f64, left: String, text: String, align: &'static str| {
            let width = text.chars().count() as f64 * 6.6;
            candidates.push((place, Tick { left, text, align }, width));
        };
        let mut waits: Vec<(f64, f64)> = axis
            .pieces
            .iter()
            .filter(|p| p.2)
            .map(|p| (p.0, p.1))
            .collect();
        for &(a, _) in &waits {
            breaks.push(Break {
                left: axis.css(axis.at(a)),
                words: String::new(),
            });
        }
        for (b, &(a, end)) in breaks.iter_mut().zip(&waits) {
            b.words = format!("waited {}", duration_text(end - a));
        }
        waits.sort_by(|x, y| (y.1 - y.0).total_cmp(&(x.1 - x.0)));
        for (a, b) in waits {
            let start = axis.at(a);
            let mid = At {
                share: start.share,
                breaks: start.breaks + 0.5,
            };
            label(
                axis.px(mid),
                axis.css(mid),
                format!("waited {}", duration_text(b - a)),
                "mid",
            );
        }
        label(0.0, axis.css(axis.at(axis.t0)), "start".into(), "start");
        if axis.open {
            label(TRACK_MIN, "100%".into(), "now".into(), "end");
        } else {
            let end = axis.pieces.last().map_or(axis.t0, |p| p.1);
            label(
                TRACK_MIN,
                "100%".into(),
                format!("+{}", duration_text(end - axis.t0)),
                "end",
            );
        }
        // the labels kept, in that order, each at least LABEL_GAP from the others and never
        // over one
        let extent = |place: f64, width: f64, align: &str| match align {
            "start" => (place, place + width),
            "end" => (place - width, place),
            _ => (place - width / 2.0, place + width / 2.0),
        };
        let mut kept: Vec<(f64, (f64, f64), Tick)> = vec![];
        for (place, tick, width) in candidates {
            let span = extent(place, width, tick.align);
            let clear = kept.iter().all(|(p, (a, b), _)| {
                (place - p).abs() >= LABEL_GAP && (span.1 + 8.0 <= *a || *b + 8.0 <= span.0)
            });
            if clear {
                kept.push((place, span, tick));
            }
        }
        kept.sort_by(|a, b| a.0.total_cmp(&b.0));
        // the stages at its end not reached yet are one row
        let mut rows: Vec<Row> = rows;
        let unreached = rows
            .iter()
            .rev()
            .take_while(|r| r.marks.is_empty() && !r.current)
            .count();
        let rest = if unreached >= 2 {
            rows.split_off(rows.len() - unreached)
                .into_iter()
                .map(|r| (r.label, r.href))
                .collect()
        } else {
            vec![]
        };
        Some(Self {
            rows,
            rest,
            breaks,
            ticks: kept.into_iter().map(|(_, _, t)| t).collect(),
            open: axis.open,
        })
    }
    fn row(axis: &Axis, lane: &Lane<'_>, all: &[&RunSpan]) -> Row {
        let mut marks = vec![];
        let mut said = vec![];
        let known_end = axis.pieces.last().map_or(axis.t0, |p| p.1);
        for (n, span) in lane.spans.iter().enumerate() {
            let from = axis.at(span.from);
            let to = axis.at(span.to.unwrap_or(known_end).max(span.from));
            let outcome = span.outcome.unwrap_or(Shown::Pending);
            let what = match span.outcome {
                Some(shown) => shown.word(),
                None => "no result recorded",
            };
            let items = if span.items > 0 {
                format!(", {}", super::ui::count(span.items, "item", "items"))
            } else {
                String::new()
            };
            let at = super::ui::at_text(&span.started);
            let (title, words) = match span.seconds() {
                Some(s) => (
                    format!(
                        "Run {}: {what}{items}; started {at}, took {}",
                        n + 1,
                        duration_text(s)
                    ),
                    format!("Run {} {what} after {}", n + 1, duration_words(s)),
                ),
                None => (
                    format!("Run {}: running{items}; started {at}", n + 1),
                    format!("Run {} running since {at}", n + 1),
                ),
            };
            said.push(words);
            let dot = !span.open() && axis.px(to) - axis.px(from) < DOT;
            marks.push(Mark {
                outcome,
                left: axis.css(from),
                width: axis.css_width(from, to),
                dot,
                open: span.open(),
                end: if dot { axis.css(from) } else { axis.css(to) },
                title,
            });
        }
        Row {
            label: lane.label.clone(),
            title: lane.title.clone(),
            href: lane.href.clone(),
            current: lane.current,
            marks,
            last: lane
                .spans
                .last()
                .map(|s| s.outcome.unwrap_or(Shown::Pending)),
            words: words(lane.spans, all),
            said: if said.is_empty() {
                "No run yet".into()
            } else {
                said.join(". ")
            },
        }
    }
    pub fn html(&self) -> TrustedHtml {
        TrustedHtml::from_template(self).unwrap_or_else(|e| TrustedHtml::owned(esc(&e.to_string())))
    }
}

/// A row's words: "3 runs · 52m · running 2h 4m · waited 6h 12m", "No run yet." The time
/// waited is what passed with none of its runs going: from the end of the unit's last run
/// before its first, and between its own runs.
fn words(spans: &[RunSpan], all: &[&RunSpan]) -> String {
    let Some(first) = spans.first() else {
        return "No run yet.".into();
    };
    let mut parts = vec![super::ui::count(spans.len(), "run", "runs")];
    let took: f64 = spans.iter().filter_map(RunSpan::seconds).sum();
    if spans.iter().any(|s| !s.open()) {
        parts.push(duration_text(took));
    }
    if let Some(running) = spans.iter().find(|s| s.open()) {
        parts.push(format!(
            "running <time data-since=\"{a}\" datetime=\"{a}\">{}</time>",
            esc(&super::ui::at_text(&running.started)),
            a = esc(&running.started)
        ));
    }
    let before = all
        .iter()
        .filter(|s| !spans.iter().any(|own| std::ptr::eq(own, **s)))
        .filter_map(|s| s.to)
        .filter(|to| *to <= first.from)
        .fold(f64::NEG_INFINITY, f64::max);
    let mut waited = if before.is_finite() {
        first.from - before
    } else {
        0.0
    };
    for pair in spans.windows(2) {
        if let Some(to) = pair[0].to {
            waited += (pair[1].from - to).max(0.0);
        }
    }
    if waited >= WAIT_MIN {
        parts.push(format!("waited {}", duration_text(waited)));
    }
    parts.join(" · ")
}

/// Where an axis from `t0` ends while a run goes: `now` rounded up to a step of at least a tenth
/// of the time since `t0` (`NOW_STEPS`), so the drawing moves once a step, not with the clock.
fn now_mark(t0: f64, now: f64) -> f64 {
    let elapsed = (now - t0).max(0.0);
    let step = NOW_STEPS
        .into_iter()
        .find(|s| *s >= elapsed * COLLAPSE)
        .unwrap_or_else(|| (elapsed * COLLAPSE / 86_400.0).ceil() * 86_400.0);
    t0 + ((elapsed / step).floor() + 1.0) * step
}

/// The middle of `samples` (the mean of the two middles for an even count); none for fewer
/// than three, too few to say what a stage usually takes.
pub fn median(mut samples: Vec<f64>) -> Option<f64> {
    if samples.len() < 3 {
        return None;
    }
    samples.sort_by(f64::total_cmp);
    let mid = samples.len() / 2;
    Some(if samples.len().is_multiple_of(2) {
        (samples[mid - 1] + samples[mid]) / 2.0
    } else {
        samples[mid]
    })
}

/// Seconds since the epoch of a julian day, as SQLite's `julianday` gives it.
pub fn from_julian(day: f64) -> f64 {
    (day - 2_440_587.5) * 86_400.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(from: f64, to: Option<f64>, outcome: &str) -> RunSpan {
        RunSpan {
            started: format!("2026-10-07T{:02}:00:00Z", (from / 3600.0) as u32),
            finished: to.map(|_| "x".into()),
            outcome: Shown::from_key(outcome),
            items: 0,
            from,
            to,
        }
    }
    fn lane<'a>(label: &str, spans: &'a [RunSpan]) -> Lane<'a> {
        Lane {
            label: label.into(),
            title: String::new(),
            href: String::new(),
            current: false,
            spans,
        }
    }

    #[test]
    fn a_long_wait_collapses_to_a_named_break_and_the_runs_keep_their_scale() {
        const H: f64 = 3600.0;
        // fork 10m, work 40m that failed, then 20m again; land after a 6h wait
        let fork = [span(0.0, Some(600.0), "succeeded")];
        let work = [
            span(600.0, Some(3000.0), "failed"),
            span(3000.0, Some(4200.0), "succeeded"),
        ];
        let land = [span(4200.0 + 6.0 * H, Some(4800.0 + 6.0 * H), "succeeded")];
        let t = Timeline::new(
            &[
                lane("fork", &fork),
                lane("work", &work),
                lane("land", &land),
            ],
            0.0,
        )
        .unwrap();
        assert_eq!(t.breaks.len(), 1);
        assert_eq!(t.breaks[0].words, "waited 6h 0m");
        // fork is 600 of the 4800 seconds drawn to scale
        assert_eq!(
            t.rows[0].marks[0].left,
            "calc((100% - 48px) * 0.00000 + 0.0px)"
        );
        assert_eq!(
            t.rows[0].marks[0].width,
            "calc((100% - 48px) * 0.12500 + 0.0px)"
        );
        // land starts past the break
        assert_eq!(
            t.rows[2].marks[0].left,
            "calc((100% - 48px) * 0.87500 + 48.0px)"
        );
        // retries are successive bars, each ending in its glyph
        assert_eq!(t.rows[1].marks.len(), 2);
        assert_eq!(t.rows[1].marks[0].outcome, Shown::Failed);
        assert_eq!(t.rows[1].words, "2 runs · 1h 0m");
        assert_eq!(t.rows[2].words, "1 run · 10m · waited 6h 0m");
        assert_eq!(
            t.ticks.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            ["start", "waited 6h 0m"]
        );
    }

    #[test]
    fn a_running_run_runs_to_now_rounded_up_so_the_clock_alone_redraws_nothing() {
        let fork = [span(0.0, Some(600.0), "succeeded")];
        let work = [span(600.0, None, "running")];
        let draw = |now| Timeline::new(&[lane("fork", &fork), lane("work", &work)], now).unwrap();
        let t = draw(1000.0);
        assert!(t.open);
        // now (1000s in) rounds up to 1200s: the run is the second half
        let run = &t.rows[1].marks[0];
        assert!(run.open);
        assert_eq!(
            (run.left.as_str(), run.width.as_str()),
            ("50.000%", "50.000%")
        );
        assert_eq!(run.end, "100.000%");
        // the same drawing until the next step
        assert_eq!(draw(1100.0), t);
        assert_ne!(draw(1300.0), t);
        // a step is a tenth of the time drawn or more: after 5h, 30 minutes
        assert_eq!(now_mark(0.0, 18_000.0), 19_800.0);
        assert!(
            t.rows[1]
                .words
                .starts_with("1 run · running <time data-since=\"")
        );
        assert_eq!(t.ticks.last().unwrap().text, "now");
    }

    #[test]
    fn a_run_too_short_to_draw_is_its_glyph_alone() {
        let a = [
            span(0.0, Some(10.0), "failed"),
            span(10.0, Some(36_000.0), "succeeded"),
        ];
        let t = Timeline::new(&[lane("work", &a)], 0.0).unwrap();
        assert!(t.rows[0].marks[0].dot);
        assert!(!t.rows[0].marks[1].dot);
    }

    #[test]
    fn labels_never_crowd_and_short_waits_stay_to_scale() {
        // two waits of 2h either side of a few seconds' work: both collapse, but their labels
        // would sit 49px apart, so only one is written
        let a = [span(0.0, Some(3600.0), "succeeded")];
        let b = [span(10_800.0, Some(10_810.0), "succeeded")];
        let c = [span(18_010.0, Some(18_070.0), "succeeded")];
        let t = Timeline::new(&[lane("a", &a), lane("b", &b), lane("c", &c)], 0.0).unwrap();
        assert_eq!(t.breaks.len(), 2);
        assert_eq!(
            t.ticks
                .iter()
                .filter(|t| t.text.starts_with("waited"))
                .count(),
            1
        );
        // a wait under a tenth of the time stays drawn to scale
        let d = [
            span(0.0, Some(3600.0), "succeeded"),
            span(3700.0, Some(7200.0), "succeeded"),
        ];
        assert!(
            Timeline::new(&[lane("d", &d)], 0.0)
                .unwrap()
                .breaks
                .is_empty()
        );
    }

    #[test]
    fn a_median_needs_three_samples() {
        assert_eq!(median(vec![1.0, 2.0]), None);
        assert_eq!(median(vec![30.0, 10.0, 20.0]), Some(20.0));
        assert_eq!(median(vec![4.0, 1.0, 3.0, 2.0]), Some(2.5));
    }
}
