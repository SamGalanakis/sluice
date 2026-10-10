//! A project's stats (DESIGN.md, Pages: Stats): how long its stages take, how its runs end, how
//! many units it finishes, and its slowest runs, over a window the owner picks (the last 24
//! hours, 7 days, 30 days, or all). This is where a stage's usual time lives: no other page
//! compares a run with it.
//!
//! Generic by rule: built from the project's runs, its steps, its units and its recipes alone.
//! A unit's stages are its recipe's, in recipe order; a unit the plan has retired is put with
//! the recipe whose stages fit its runs best; units of no recipe and loose steps form one group
//! by step. Read-only and server-rendered; its stream patches only when what it draws changes:
//! its version is the drawing, which reads no clock but the window's edges.
use super::failure::Failure;
use super::ui::{self, Shown, esc};
use super::{DashboardState, Frame, NavView, TrustedHtml, Viewer, render_framed, render_nav};
use axum::{
    Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use sluice_model::{error::PublicError, ids::ProjectId, shown::Band};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// How many of the slowest runs the page lists.
pub const SLOWEST: usize = 10;
/// An "all" window longer than this many days counts its throughput by the week.
pub const DAILY_UP_TO: u64 = 92;

/// The window the page covers, chosen by `?window=`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Span {
    /// The last 24 hours, by the hour.
    Day,
    /// The last 7 days on the reader's clock, today included.
    #[default]
    Week,
    /// The last 30 days.
    Month,
    /// Every run the project has.
    All,
}
impl Span {
    pub const EVERY: [Span; 4] = [Span::Day, Span::Week, Span::Month, Span::All];
    /// Its value in `?window=`.
    pub fn key(self) -> &'static str {
        match self {
            Span::Day => "24h",
            Span::Week => "7d",
            Span::Month => "30d",
            Span::All => "all",
        }
    }
    /// Its link in the switch.
    pub fn label(self) -> &'static str {
        match self {
            Span::Day => "24 hours",
            Span::Week => "7 days",
            Span::Month => "30 days",
            Span::All => "All",
        }
    }
    /// The window `?window=` names; the default (7 days) for anything else.
    pub fn parse(key: Option<&str>) -> Self {
        Self::EVERY
            .into_iter()
            .find(|s| Some(s.key()) == key)
            .unwrap_or_default()
    }
}

/// One run of a step, as the page reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct StatRun {
    pub run_id: String,
    pub step: String,
    /// Its unit's name, "" for a loose step.
    pub unit: String,
    /// A scatter's item, -1 for none.
    pub item: i64,
    /// Its try of its step (and item): 1 for the first.
    pub n: usize,
    pub started: u64,
    pub ended: Option<u64>,
    /// How it ended, from the status table; `Running` while it runs, none when it ended with
    /// no result.
    pub outcome: Option<Shown>,
    /// A failure's kind in words ("engine exited", "fn failure"); "" otherwise.
    pub kind: String,
    /// Its step is still in the plan.
    pub kept: bool,
}
impl StatRun {
    /// How long it ran; none while it runs.
    pub fn seconds(&self) -> Option<u64> {
        self.ended.map(|e| e.saturating_sub(self.started))
    }
    /// What it counts under: its unit, or a loose step itself.
    pub fn owner(&self) -> &str {
        if self.unit.is_empty() {
            &self.step
        } else {
            &self.unit
        }
    }
    /// Its stage: its step past its unit's name ("draft"), else its step.
    pub fn stage(&self) -> &str {
        if self.unit.is_empty() || self.step == self.unit {
            return &self.step;
        }
        self.step
            .strip_prefix(self.unit.as_str())
            .and_then(|rest| rest.strip_prefix('-'))
            .unwrap_or(&self.step)
    }
    /// Its step's runs while the plan keeps the step, else its records on the project's log.
    pub fn href(&self, project: ProjectId) -> String {
        let step: String = url::form_urlencoded::byte_serialize(self.step.as_bytes()).collect();
        if self.kept {
            format!("/projects/id/{project}/steps/{step}?tab=runs")
        } else {
            format!("/projects/id/{project}/log?step={step}")
        }
    }
}

/// What the page reads from one snapshot.
#[derive(Clone, Debug, Default)]
pub struct StatsData {
    /// Every run of its steps, oldest first.
    pub runs: Vec<StatRun>,
    /// Each unit the plan holds (a loose step by its id): whether all its steps are done.
    pub planned: BTreeMap<String, bool>,
    /// Its names and recipes.
    pub names: Arc<sluice_runtime::naming::ProjectNaming>,
}

/// Every run of `project`'s steps, its plan's units and its names, from one snapshot.
pub fn load(c: &rusqlite::Connection, project: ProjectId) -> sluice_store::Result<StatsData> {
    let p = project.to_string();
    let mut q = c.prepare_cached(
        "SELECT r.run_id,r.step_id,coalesce(r.unit,s.unit,''),r.item_index,coalesce(r.started_at,r.created_at),r.finished_at,json_extract(r.result,'$.status'),json_extract(r.result,'$.error'),s.step_id IS NOT NULL FROM runs r LEFT JOIN steps s ON s.project_id=r.project_id AND s.step_id=r.step_id WHERE r.project_id=?1 AND r.step_id IS NOT NULL ORDER BY r.created_at,r.run_id",
    )?;
    let mut rows = q.query([&p])?;
    let mut runs = vec![];
    let mut tries: BTreeMap<(String, i64), usize> = BTreeMap::new();
    while let Some(r) = rows.next()? {
        let began: String = r.get(4)?;
        let Some(started) = super::timestamp(&began) else {
            continue;
        };
        let step: String = r.get(1)?;
        let item: i64 = r.get(3)?;
        let finished: Option<String> = r.get(5)?;
        let status: Option<String> = r.get(6)?;
        let error: Option<String> = r.get(7)?;
        let outcome = match (
            &finished,
            status.map(|s| s.parse::<sluice_model::commands::StepStatus>()),
        ) {
            (None, _) => Some(Shown::Running),
            (Some(_), Some(Ok(status))) => {
                Some(sluice_model::shown::classify(&sluice_model::shown::Facts {
                    cancelled: error
                        .as_deref()
                        .is_some_and(sluice_model::shown::stored_is_cancel),
                    ..sluice_model::shown::Facts::of(status)
                }))
            }
            _ => None,
        };
        let kind = match (outcome, &error) {
            (Some(Shown::Failed), Some(e)) => Failure::parse(e, None).kind_words(),
            (Some(Shown::Failed), None) => "no reason recorded".to_owned(),
            _ => String::new(),
        };
        let n = tries.entry((step.clone(), item)).or_default();
        *n += 1;
        runs.push(StatRun {
            run_id: r.get(0)?,
            n: *n,
            step,
            unit: r.get(2)?,
            item,
            started,
            ended: finished.as_deref().and_then(super::timestamp),
            outcome,
            kind,
            kept: r.get(8)?,
        });
    }
    let mut planned: BTreeMap<String, bool> = BTreeMap::new();
    let mut q = c.prepare_cached(
        "SELECT step_id,coalesce(unit,''),status FROM steps WHERE project_id=?1",
    )?;
    let mut rows = q.query([&p])?;
    while let Some(r) = rows.next()? {
        let step: String = r.get(0)?;
        let unit: String = r.get(1)?;
        let status: String = r.get(2)?;
        let done = matches!(status.as_str(), "succeeded" | "skipped");
        let owner = if unit.is_empty() { step } else { unit };
        let all = planned.entry(owner).or_insert(true);
        *all &= done;
    }
    let names = sluice_runtime::naming::for_project(c, &super::home_of(c), project)?;
    Ok(StatsData {
        runs,
        planned,
        names,
    })
}

/// The window's edges and its buckets for throughput, on the reader's clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub span: Span,
    /// Its start (0 for all) and its end, seconds since the epoch.
    pub start: u64,
    pub end: u64,
    /// Minutes east of UTC.
    pub zone: i64,
    /// Each bucket's start, oldest first; each ends where the next starts (the last at `end`).
    pub buckets: Vec<u64>,
    /// A bucket's length: an hour, a day or a week.
    pub step: u64,
}
impl Window {
    /// The window as of `now`; `first` is the project's first run (for all).
    pub fn new(span: Span, now: u64, zone: i64, first: Option<u64>) -> Self {
        let z = zone * 60;
        let local_floor = |at: u64, unit: i64| -> u64 {
            ((at as i64 + z).div_euclid(unit) * unit - z).max(0) as u64
        };
        let hour = 3600;
        let day = 86_400;
        let (start, end, step) = match span {
            Span::Day => {
                let end = local_floor(now, hour) + hour as u64;
                (end - 24 * hour as u64, end, hour as u64)
            }
            Span::Week | Span::Month => {
                let days = if span == Span::Week { 7 } else { 30 };
                let end = local_floor(now, day) + day as u64;
                (end - days * day as u64, end, day as u64)
            }
            Span::All => {
                let end = local_floor(now, day) + day as u64;
                let start = local_floor(first.unwrap_or(now).min(now), day);
                let days = (end - start) / day as u64;
                let step = if days > DAILY_UP_TO { 7 * day as u64 } else { day as u64 };
                (start, end, step)
            }
        };
        let mut buckets = vec![];
        let mut at = start;
        while at < end {
            buckets.push(at);
            at += step;
        }
        Self {
            span,
            start,
            end,
            zone,
            buckets,
            step,
        }
    }
    /// Whether an instant falls in the window.
    pub fn holds(&self, at: u64) -> bool {
        (self.span == Span::All || at >= self.start) && at < self.end
    }
    /// "in the last 7 days", "in the last 24 hours", "since 28 September".
    pub fn words(&self) -> String {
        match self.span {
            Span::Day => "in the last 24 hours".into(),
            Span::Week => "in the last 7 days".into(),
            Span::Month => "in the last 30 days".into(),
            Span::All => format!("since {}", self.date(self.start, true)),
        }
    }
    /// The local date of an instant: "6 Oct" (or "6 October" when `long`).
    pub fn date(&self, at: u64, long: bool) -> String {
        const MONTHS: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        let local = (at as i64 + self.zone * 60).max(0) as u64;
        let date = super::rfc3339(local);
        let month: usize = date.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(1);
        let day: usize = date.get(8..10).and_then(|d| d.parse().ok()).unwrap_or(1);
        let name = MONTHS[(month - 1).min(11)];
        if long {
            format!("{day} {name}")
        } else {
            format!("{day} {}", &name[..3])
        }
    }
    /// A bucket as a label: its hour ("14:00") or its date ("6 Oct").
    pub fn bucket_label(&self, at: u64) -> String {
        if self.step == 3600 {
            let local = (at as i64 + self.zone * 60).rem_euclid(86_400);
            format!("{:02}:00", local / 3600)
        } else {
            self.date(at, false)
        }
    }
    /// The bucket an instant falls in.
    fn bucket(&self, at: u64) -> Option<usize> {
        if !self.holds(at) || at < self.start {
            return None;
        }
        Some(((at - self.start) / self.step) as usize).filter(|i| *i < self.buckets.len())
    }
}

/// One stage's numbers over the window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StageStats {
    pub name: String,
    /// How long its succeeded runs took, seconds, shortest first.
    pub took: Vec<u64>,
    pub succeeded: usize,
    pub failed: usize,
    pub cancelled: usize,
    /// Its runs that ended any other way (stale, or with no result).
    pub other: usize,
    /// Its runs that were a second or later try of their step.
    pub retried: usize,
    /// Its failures by kind, in words.
    pub kinds: BTreeMap<String, usize>,
}
impl StageStats {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            ..Default::default()
        }
    }
    fn add(&mut self, run: &StatRun) {
        let Some(seconds) = run.seconds() else {
            return;
        };
        match run.outcome {
            Some(Shown::Succeeded) => {
                self.succeeded += 1;
                self.took.push(seconds);
            }
            Some(Shown::Failed) => {
                self.failed += 1;
                *self.kinds.entry(run.kind.clone()).or_default() += 1;
            }
            Some(Shown::Cancelled) => self.cancelled += 1,
            _ => self.other += 1,
        }
        if run.n > 1 {
            self.retried += 1;
        }
    }
    fn settle(&mut self) {
        self.took.sort_unstable();
    }
    /// Every run that ended.
    pub fn ended(&self) -> usize {
        self.succeeded + self.failed + self.cancelled + self.other
    }
    /// The middle of its succeeded runs (the mean of the two middles for an even count).
    pub fn median(&self) -> Option<u64> {
        let t = &self.took;
        match t.len() {
            0 => None,
            n if n % 2 == 1 => Some(t[n / 2]),
            n => Some((t[n / 2 - 1] + t[n / 2]) / 2),
        }
    }
    /// The 90th percentile of its succeeded runs, by nearest rank.
    pub fn p90(&self) -> Option<u64> {
        let n = self.took.len();
        (n > 0).then(|| self.took[(n * 9).div_ceil(10).max(1) - 1])
    }
    pub fn longest(&self) -> Option<u64> {
        self.took.last().copied()
    }
    /// Its failed runs out of those that ended, in percent.
    pub fn failure_rate(&self) -> Option<f64> {
        let ended = self.ended();
        (ended > 0).then(|| self.failed as f64 * 100.0 / ended as f64)
    }
}

/// A recipe's stages (or the units of no recipe's steps), each with its numbers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Group {
    /// The recipe's name; "" for the units of no recipe and the loose steps.
    pub recipe: String,
    pub stages: Vec<StageStats>,
    /// How many units (a loose step counts as one) ran in the window.
    pub units: usize,
}
impl Group {
    pub fn runs(&self) -> usize {
        self.stages.iter().map(StageStats::ended).sum()
    }
}

/// The page drawn: its window, its groups, its throughput and its slowest runs.
#[derive(Clone, Debug)]
pub struct StatsView {
    pub project: ProjectId,
    pub window: Window,
    pub groups: Vec<Group>,
    /// Per bucket: units finished, units started.
    pub finished: Vec<usize>,
    pub started: Vec<usize>,
    /// The slowest runs that ended in the window, the slowest first, with their names.
    pub slowest: Vec<(StatRun, String)>,
    /// Runs that ended in the window, and whether the project has run at all.
    pub ended: usize,
    pub any: bool,
    /// What its ids start with: "" on its page, a prefix where it is drawn twice (`/_ui`).
    pub ids: String,
}
impl StatsView {
    pub fn new(data: &StatsData, project: ProjectId, span: Span, now: u64, zone: i64) -> Self {
        let first = data.runs.iter().map(|r| r.started).min();
        let window = Window::new(span, now, zone, first);
        let naming = &data.names;
        // each unit's steps' stages, over all its runs: what a retired unit's recipe is read by
        let mut stages_of: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for run in data.runs.iter().filter(|r| !r.unit.is_empty()) {
            stages_of.entry(&run.unit).or_default().insert(run.stage());
        }
        let recipes: Vec<(&str, Vec<String>)> = naming
            .recipes
            .iter()
            .map(|(name, recipe)| (name.as_str(), recipe.stages()))
            .collect();
        // a unit's recipe: the one its plan names, else (a unit the plan has retired) the one
        // whose stages hold every stage its runs ran with the fewest left over
        let recipe_of = |unit: &str| -> Option<&str> {
            if unit.is_empty() {
                return None;
            }
            if let Some(named) = naming.naming.unit(unit) {
                return (!named.recipe.is_empty())
                    .then_some(named.recipe.as_str())
                    .and_then(|r| recipes.iter().find(|(n, _)| *n == r).map(|(n, _)| *n));
            }
            if data.planned.contains_key(unit) {
                return None;
            }
            let ran = stages_of.get(unit)?;
            recipes
                .iter()
                .filter(|(_, stages)| ran.iter().all(|s| stages.iter().any(|t| t == s)))
                .min_by_key(|(_, stages)| stages.len() - ran.len())
                .map(|(name, _)| *name)
        };
        let mut unit_recipe: BTreeMap<&str, Option<&str>> = BTreeMap::new();
        for run in &data.runs {
            unit_recipe
                .entry(&run.unit)
                .or_insert_with(|| recipe_of(&run.unit));
        }
        // the groups: each recipe's stages in its order, then the rest by step
        let mut groups: Vec<Group> = recipes
            .iter()
            .map(|(name, stages)| Group {
                recipe: (*name).to_owned(),
                stages: stages.iter().map(|s| StageStats::new(s)).collect(),
                units: 0,
            })
            .collect();
        let mut loose = Group::default();
        let mut seen: BTreeMap<usize, BTreeSet<&str>> = BTreeMap::new();
        let mut ended = 0;
        for run in data.runs.iter().filter(|r| r.ended.is_some_and(|e| window.holds(e))) {
            ended += 1;
            let recipe = unit_recipe.get(run.unit.as_str()).copied().flatten();
            let (at, group) = match recipe.and_then(|r| groups.iter().position(|g| g.recipe == r))
            {
                Some(i) => (i, &mut groups[i]),
                None => (usize::MAX, &mut loose),
            };
            seen.entry(at).or_default().insert(run.owner());
            let stage = run.stage();
            let i = match group.stages.iter().position(|s| s.name == stage) {
                Some(i) => i,
                None => {
                    group.stages.push(StageStats::new(stage));
                    group.stages.len() - 1
                }
            };
            group.stages[i].add(run);
        }
        for (i, group) in groups.iter_mut().enumerate() {
            group.units = seen.get(&i).map_or(0, BTreeSet::len);
        }
        loose.units = seen.get(&usize::MAX).map_or(0, BTreeSet::len);
        // the steps of no recipe: the busiest first, then by name
        loose
            .stages
            .sort_by(|a, b| b.ended().cmp(&a.ended()).then(a.name.cmp(&b.name)));
        groups.retain(|g| g.units > 0);
        if loose.units > 0 {
            groups.push(loose);
        }
        for stage in groups.iter_mut().flat_map(|g| g.stages.iter_mut()) {
            stage.settle();
        }
        // throughput: a unit starts with its first run and finishes with its last, once all its
        // steps are done (the plan's word for a unit it holds; a retired one's every step's
        // last run succeeded)
        let mut by_unit: BTreeMap<&str, Vec<&StatRun>> = BTreeMap::new();
        for run in &data.runs {
            by_unit.entry(run.owner()).or_default().push(run);
        }
        let mut finished = vec![0; window.buckets.len()];
        let mut started = vec![0; window.buckets.len()];
        for (unit, runs) in &by_unit {
            if let Some(i) = runs.iter().map(|r| r.started).min().and_then(|s| window.bucket(s))
            {
                started[i] += 1;
            }
            let done = match data.planned.get(*unit) {
                Some(done) => *done,
                None => {
                    let mut last: BTreeMap<(&str, i64), &StatRun> = BTreeMap::new();
                    for run in runs {
                        last.insert((&run.step, run.item), run);
                    }
                    last.values().all(|r| r.outcome == Some(Shown::Succeeded))
                }
            };
            if done
                && let Some(i) = runs
                    .iter()
                    .filter_map(|r| r.ended)
                    .max()
                    .and_then(|e| window.bucket(e))
            {
                finished[i] += 1;
            }
        }
        // the slowest runs that ended in the window, named by their unit's title
        let mut slow: Vec<&StatRun> = data
            .runs
            .iter()
            .filter(|r| r.ended.is_some_and(|e| window.holds(e)))
            .collect();
        slow.sort_by(|a, b| b.seconds().cmp(&a.seconds()).then(a.run_id.cmp(&b.run_id)));
        let slowest = slow
            .into_iter()
            .take(SLOWEST)
            .map(|r| {
                let title = if r.unit.is_empty() {
                    naming.naming.step_title(&r.step)
                } else {
                    naming.naming.unit_title(&r.unit)
                };
                (r.clone(), title.to_owned())
            })
            .collect();
        Self {
            project,
            window,
            groups,
            finished,
            started,
            slowest,
            ended,
            any: !data.runs.is_empty(),
            ids: String::new(),
        }
    }

    pub fn title(&self, project: &str) -> String {
        format!("Stats · {project}")
    }
    /// The page's head: "Stats", and under it what the window holds.
    pub fn head(&self) -> TrustedHtml {
        let finished: usize = self.finished.iter().sum();
        let note = if !self.any {
            "No run yet.".to_owned()
        } else if self.ended == 0 {
            format!("No run ended {}.", self.window.words())
        } else {
            format!(
                "{} ended and {} finished {}{}.",
                ui::count(self.ended, "run", "runs"),
                ui::count(finished, "unit", "units"),
                self.window.words(),
                self.clock_words()
            )
        };
        ui::page_head_note("Stats", &TrustedHtml::owned(esc(&note)))
    }
    /// ", days on your clock (UTC+02:00)" when the reader's clock is not UTC.
    fn clock_words(&self) -> String {
        if self.window.zone == 0 {
            String::new()
        } else {
            format!(
                ", on your clock ({})",
                super::day::zone_name(self.window.zone)
            )
        }
    }
    /// The window switch in the row: a link a window, the chosen one filled.
    pub fn switch(&self) -> TrustedHtml {
        let links: String = Span::EVERY
            .into_iter()
            .map(|span| {
                format!(
                    "<a href=\"{}?window={}\"{}>{}</a>",
                    esc(&self.href()),
                    span.key(),
                    if span == self.window.span {
                        " aria-current=\"page\""
                    } else {
                        ""
                    },
                    span.label()
                )
            })
            .collect();
        TrustedHtml::owned(format!(
            "<nav class=\"view-switch\" aria-label=\"Window\">{links}</nav>"
        ))
    }
    pub fn href(&self) -> String {
        format!("/projects/id/{}/stats", self.project)
    }
    /// The page's body: durations by stage, outcomes, throughput, the slowest runs.
    pub fn body(&self) -> TrustedHtml {
        let mut html = String::from("<div id=\"stats-view\" class=\"stats\">");
        html.push_str(&self.durations());
        html.push_str(&self.outcomes());
        html.push_str(&self.throughput());
        html.push_str(&self.slowest_html());
        html.push_str("</div>");
        TrustedHtml::owned(html)
    }
    fn empty(&self) -> String {
        format!(
            "<p class=\"st-empty\">{}</p>",
            esc(&if self.any {
                format!("No run ended {}.", self.window.words())
            } else {
                "No run yet.".to_owned()
            })
        )
    }
    /// A group's head: its recipe's name (or what the rest are) and how many units and runs.
    fn group_head(&self, group: &Group, id: &str) -> String {
        let name = if group.recipe.is_empty() {
            "Without a recipe".to_owned()
        } else {
            group.recipe.clone()
        };
        format!(
            "<h3 class=\"st-gh\" id=\"{id}\">{}<span class=\"st-gn\">{} · {}</span></h3>",
            esc(&name),
            esc(&ui::count(group.units, "unit", "units")),
            esc(&ui::count(group.runs(), "run", "runs")),
        )
    }
    /// Durations by stage: a table a group, a row a stage in its recipe's order, its median,
    /// p90, longest and how many succeeded runs they are of, with its median and p90 drawn as
    /// bars against the group's longest p90.
    pub fn durations(&self) -> String {
        let p = &self.ids;
        let mut html = format!(
            "<section class=\"st-sec\" aria-labelledby=\"{p}st-dur-h\">{}",
            ui::section_head(
                &format!("{p}st-dur-h"),
                "Durations by stage",
                "Its runs that succeeded: the bar is the median, its pale end the p90"
            )
        );
        if self.groups.is_empty() {
            html.push_str(&self.empty());
        }
        for (g, group) in self.groups.iter().enumerate() {
            let id = format!("{p}st-dur-{g}");
            let scale = group
                .stages
                .iter()
                .filter_map(StageStats::p90)
                .max()
                .unwrap_or(0)
                .max(1) as f64;
            let mut rows = String::new();
            for stage in &group.stages {
                let cell = |k: &str, v: Option<u64>| match v {
                    Some(s) => format!(
                        "<td class=\"num\" data-k=\"{k}\">{}</td>",
                        ui::duration(s as f64)
                    ),
                    None => format!("<td class=\"num st-none\" data-k=\"{k}\">–</td>"),
                };
                let bar = match (stage.median(), stage.p90()) {
                    (Some(m), Some(p)) => format!(
                        "<span class=\"st-bar\" aria-hidden=\"true\"><span class=\"st-p90\" style=\"--w:{:.2}%\"></span><span class=\"st-med\" style=\"--w:{:.2}%\"></span></span>",
                        p as f64 / scale * 100.0,
                        m as f64 / scale * 100.0
                    ),
                    _ => String::new(),
                };
                rows.push_str(&format!(
                    "<tr><th scope=\"row\">{}</th><td class=\"num\" data-k=\"Runs\">{}</td>{}{}{}<td class=\"st-draw\">{bar}</td></tr>",
                    esc(&stage.name),
                    stage.took.len(),
                    cell("Median", stage.median()),
                    cell("p90", stage.p90()),
                    cell("Longest", stage.longest()),
                ));
            }
            html.push_str(&format!(
                "<div class=\"st-group\">{}<table class=\"st-t st-dur\" aria-labelledby=\"{id}\"><thead><tr><th scope=\"col\">Stage</th><th scope=\"col\" class=\"num\">Runs</th><th scope=\"col\" class=\"num\">Median</th><th scope=\"col\" class=\"num\">p90</th><th scope=\"col\" class=\"num\">Longest</th><th scope=\"col\" class=\"st-draw\"><span class=\"vh\">Median and p90, drawn</span></th></tr></thead><tbody>{rows}</tbody></table></div>",
                self.group_head(group, &id)
            ));
        }
        html.push_str("</section>");
        html
    }
    /// Outcomes: a table a group, a row a stage: how its runs ended, how many were retries,
    /// its failure rate and its failures by kind.
    pub fn outcomes(&self) -> String {
        let p = &self.ids;
        let mut html = format!(
            "<section class=\"st-sec\" aria-labelledby=\"{p}st-out-h\">{}",
            ui::section_head(
                &format!("{p}st-out-h"),
                "Outcomes",
                "How each stage's runs ended; a retry is a second or later try of its step"
            )
        );
        if self.groups.is_empty() {
            html.push_str(&self.empty());
        }
        for (g, group) in self.groups.iter().enumerate() {
            let id = format!("{p}st-out-{g}");
            let mut rows = String::new();
            for stage in &group.stages {
                let n = |k: &str, v: usize| {
                    if v == 0 {
                        format!("<td class=\"num st-none\" data-k=\"{k}\">0</td>")
                    } else {
                        format!("<td class=\"num\" data-k=\"{k}\">{v}</td>")
                    }
                };
                let rate = match stage.failure_rate() {
                    Some(r) if stage.failed > 0 => format!(
                        "<td class=\"num st-bad\" data-k=\"Failure rate\">{}%</td>",
                        esc(&rate_text(r))
                    ),
                    Some(_) => "<td class=\"num st-none\" data-k=\"Failure rate\">0%</td>".to_owned(),
                    None => "<td class=\"num st-none\" data-k=\"Failure rate\">–</td>".to_owned(),
                };
                let mut kinds: Vec<(&String, &usize)> = stage.kinds.iter().collect();
                kinds.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
                let kinds = kinds
                    .iter()
                    .map(|(k, n)| format!("{} {n}", esc(k)))
                    .collect::<Vec<_>>()
                    .join(" · ");
                rows.push_str(&format!(
                    "<tr><th scope=\"row\">{}</th>{}{}{}{}{rate}<td class=\"st-kinds\">{kinds}</td></tr>",
                    esc(&stage.name),
                    n("Succeeded", stage.succeeded),
                    n("Failed", stage.failed),
                    n("Cancelled", stage.cancelled),
                    n("Retried", stage.retried),
                ));
            }
            html.push_str(&format!(
                "<div class=\"st-group\">{}<table class=\"st-t st-outcomes\" aria-labelledby=\"{id}\"><thead><tr><th scope=\"col\">Stage</th><th scope=\"col\" class=\"num\">Succeeded</th><th scope=\"col\" class=\"num\">Failed</th><th scope=\"col\" class=\"num\">Cancelled</th><th scope=\"col\" class=\"num\">Retried</th><th scope=\"col\" class=\"num\">Failure rate</th><th scope=\"col\">Failed by</th></tr></thead><tbody>{rows}</tbody></table></div>",
                self.group_head(group, &id)
            ));
        }
        html.push_str("</section>");
        html
    }
    /// Throughput: units finished a bucket (an hour or a day) as columns, the busiest marked,
    /// with how many started and finished in a sentence and every bucket's counts in a table
    /// for a screen reader.
    pub fn throughput(&self) -> String {
        let p = &self.ids;
        let w = &self.window;
        let unit = match w.step {
            3600 => ("an hour", "hour"),
            86_400 => ("a day", "day"),
            _ => ("a week", "week"),
        };
        let mut html = format!(
            "<section class=\"st-sec\" aria-labelledby=\"{p}st-tp-h\">{}",
            ui::section_head(
                &format!("{p}st-tp-h"),
                "Throughput",
                &format!("Units finished {}", unit.0)
            )
        );
        let finished: usize = self.finished.iter().sum();
        let started: usize = self.started.iter().sum();
        let most = self.finished.iter().copied().max().unwrap_or(0);
        let mut said = format!(
            "{} finished and {} started {}.",
            ui::count(finished, "unit", "units"),
            started,
            w.words()
        );
        if most > 0
            && let Some(i) = self.finished.iter().position(|n| *n == most)
        {
            said.push_str(&format!(
                " The most in {} {}: {most}, {} {}.",
                if unit.1 == "hour" { "an" } else { "a" },
                unit.1,
                if unit.1 == "hour" { "at" } else { "on" },
                w.bucket_label(w.buckets[i])
            ));
        }
        html.push_str(&format!("<p class=\"st-said\">{}</p>", esc(&said)));
        let n = self.finished.len().max(1);
        let top = most.max(1) as f64;
        let mut bars = String::new();
        for (i, count) in self.finished.iter().enumerate() {
            if *count == 0 {
                continue;
            }
            let h = (*count as f64 / top * 60.0).max(2.0);
            bars.push_str(&format!(
                "<rect class=\"st-col{}\" x=\"{}\" y=\"{:.2}\" width=\"8\" height=\"{h:.2}\"></rect>",
                if *count == most { " st-most" } else { "" },
                i * 10 + 1,
                64.0 - h,
            ));
        }
        let first = w.buckets.first().map(|b| w.bucket_label(*b)).unwrap_or_default();
        let last = w.buckets.last().map(|b| w.bucket_label(*b)).unwrap_or_default();
        html.push_str(&format!(
            "<figure class=\"st-spark\" style=\"--n:{n}\"><div class=\"st-plot\"><span class=\"st-top\" aria-hidden=\"true\">{most}</span><svg viewBox=\"0 0 {vw} 64\" preserveAspectRatio=\"none\" role=\"img\" aria-label=\"{label}\" focusable=\"false\"><line class=\"st-base\" x1=\"0\" y1=\"63.5\" x2=\"{vw}\" y2=\"63.5\"></line>{bars}</svg></div><figcaption class=\"st-axis\" aria-hidden=\"true\"><span>{first}</span><span>{last}</span></figcaption></figure>",
            vw = n * 10,
            label = esc(&format!(
                "Units finished {} from {first} to {last}, at most {most}",
                unit.0
            )),
            first = esc(&first),
            last = esc(&last),
        ));
        // every bucket's counts, for a reader
        let mut rows = String::new();
        for (i, at) in w.buckets.iter().enumerate() {
            rows.push_str(&format!(
                "<tr><th scope=\"row\">{}</th><td>{}</td><td>{}</td></tr>",
                esc(&w.bucket_label(*at)),
                self.finished[i],
                self.started[i]
            ));
        }
        html.push_str(&format!(
            "<table class=\"vh\"><caption>Units finished and started each {}</caption><thead><tr><th scope=\"col\">{}</th><th scope=\"col\">Finished</th><th scope=\"col\">Started</th></tr></thead><tbody>{rows}</tbody></table>",
            unit.1,
            if unit.1 == "hour" { "Hour" } else { "Day" }
        ));
        html.push_str("</section>");
        html
    }
    /// The slowest runs that ended in the window: how long, what (its unit's title and its
    /// stage, linked to its runs), how it ended when not a success, when, and its ids behind ⋯.
    pub fn slowest_html(&self) -> String {
        let p = &self.ids;
        let mut html = format!(
            "<section class=\"st-sec\" aria-labelledby=\"{p}st-slow-h\">{}",
            ui::section_head(
                &format!("{p}st-slow-h"),
                "The slowest runs",
                &format!("The {SLOWEST} longest runs that ended {}", self.window.words())
            )
        );
        if self.slowest.is_empty() {
            html.push_str(&self.empty());
            html.push_str("</section>");
            return html;
        }
        html.push_str("<ol class=\"st-slow\">");
        for (run, title) in &self.slowest {
            let seconds = run.seconds().unwrap_or(0) as f64;
            let stage = run.stage();
            let name = if title.is_empty() {
                run.owner().to_owned()
            } else {
                ui::cut(title, 90)
            };
            let rest = if stage != run.owner() && stage != name {
                format!(" <span class=\"st-stage\">{}</span>", esc(stage))
            } else {
                String::new()
            };
            let outcome = match run.outcome {
                Some(Shown::Succeeded) => String::new(),
                Some(s) if s.spec().band == Band::Done => String::new(),
                Some(s) => format!(
                    "<span class=\"st-out st-{}\">{}{}</span>",
                    s.key(),
                    ui::mark(s),
                    esc(s.word())
                ),
                None => "<span class=\"st-out\">ended</span>".to_owned(),
            };
            let ended = super::rfc3339(run.ended.unwrap_or(run.started));
            let mut details = ui::Details::new()
                .id("Step", &run.step)
                .id("Run", &run.run_id)
                .text("Try", &run.n.to_string());
            if !run.unit.is_empty() {
                details = details.id("Unit", &run.unit);
            }
            if run.item >= 0 {
                details = details.text("Item", &run.item.to_string());
            }
            html.push_str(&format!(
                "<li class=\"st-run\"><span class=\"st-took\">{took}</span><a class=\"st-name\" href=\"{href}\"><b>{name}</b>{rest}</a>{outcome}<span class=\"st-when\">ended {when}</span>{menu}</li>",
                took = ui::duration(seconds),
                href = esc(&run.href(self.project)),
                name = esc(&name),
                when = self.when(&ended),
                menu = details.menu(&name),
            ));
        }
        html.push_str("</ol></section>");
        html
    }
    /// An end on the reader's clock: "6 Oct, 14:02" (plain text, so the page's version never moves with
    /// the clock).
    fn when(&self, at: &str) -> String {
        let Some(secs) = super::timestamp(at) else {
            return esc(at);
        };
        let local = (secs as i64 + self.window.zone * 60).rem_euclid(86_400);
        format!(
            "{}, {:02}:{:02}",
            esc(&self.window.date(secs, false)),
            local / 3600,
            local % 3600 / 60
        )
    }
}
/// A failure rate as a page says it: a whole percent, "<1" under one.
fn rate_text(rate: f64) -> String {
    if rate > 0.0 && rate < 1.0 {
        "<1".into()
    } else {
        format!("{:.0}", rate.round())
    }
}

#[derive(Deserialize, Default)]
pub struct WindowQuery {
    pub window: Option<String>,
}

/// The stats of `project` as of now on the reader's clock, with the nav from the same
/// snapshot.
pub async fn snapshot(
    state: &DashboardState,
    project: ProjectId,
    span: Span,
    zone: i64,
) -> Result<(super::DashboardSnapshot, StatsView), PublicError> {
    let now = super::day::now();
    let catalog = state.catalog.catalog(Some(project))?;
    let (shared, data) = state
        .reads
        .snapshot(move |c| {
            let shared = super::load_snapshot(c, catalog)?;
            if !shared.projects.iter().any(|p| p.id == project) {
                return Ok((shared, None));
            }
            let data = load(c, project)?;
            Ok((shared, Some(data)))
        })
        .await
        .map_err(|e| e.into_public(true))?;
    let Some(data) = data else {
        return Err(PublicError::NotFound {
            message: format!("project {project} not found"),
        });
    };
    let view = StatsView::new(&data, project, span, now, zone);
    Ok((shared, view))
}
/// The page's regions and its version: what it draws and the nav, never the clock.
fn batch(
    shared: &super::DashboardSnapshot,
    view: &StatsView,
    viewer: &Viewer,
) -> Result<crate::streams::RenderedBatch, PublicError> {
    let nav = NavView::new(shared, Some(view.project), "stats")?;
    let nav_html = render_nav(&nav, viewer, &view.href()).map_err(error)?;
    let head = super::day::band_with_id(&view.head(), "stats-head");
    let body = view.body();
    let version = sluice_store::artifacts::fingerprint(
        format!(
            "{}\u{0}{}\u{0}{}",
            body.as_str(),
            head.as_str(),
            nav_html.as_str()
        )
        .as_bytes(),
    );
    Ok(crate::streams::RenderedBatch {
        version,
        regions: vec![
            crate::streams::PatchRegion::new("stats-view", body),
            crate::streams::PatchRegion::new("stats-head", head),
            crate::streams::PatchRegion::new("top-nav", nav_html),
        ],
    })
}
fn error(e: askama::Error) -> PublicError {
    PublicError::Storage {
        message: e.to_string(),
    }
}
async fn page(
    State(state): State<DashboardState>,
    Path(project): Path<ProjectId>,
    Query(query): Query<WindowQuery>,
    headers: HeaderMap,
) -> Response {
    let viewer = Viewer::from_headers(&headers);
    let span = Span::parse(query.window.as_deref());
    let drawn = async {
        let (shared, view) = snapshot(&state, project, span, super::day::zone(&headers)).await?;
        let nav = NavView::new(&shared, Some(project), "stats")?;
        let drawn = batch(&shared, &view, &viewer)?;
        let name = shared
            .projects
            .iter()
            .find(|p| p.id == project)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        render_framed(
            &view.title(&name),
            &drawn.regions[0].html,
            &nav,
            &viewer,
            &format!("{}/stream?window={}", view.href(), span.key()),
            &drawn.version,
            &view.href(),
            &Frame {
                head: drawn.regions[1].html.clone(),
                tools: view.switch(),
                ..Frame::default()
            },
        )
        .map_err(error)
    };
    match drawn.await {
        Ok(html) => Html(html.0).into_response(),
        Err(e) => crate::http::error_response(e),
    }
}
async fn stream(
    State(state): State<DashboardState>,
    Path(project): Path<ProjectId>,
    Query(window): Query<WindowQuery>,
    Query(query): Query<crate::streams::StreamQuery>,
    headers: HeaderMap,
) -> Response {
    use crate::streams::{VersionSignal, page_events};
    use axum::response::sse::{KeepAlive, Sse};
    let viewer = Viewer::from_headers(&headers);
    let zone = super::day::zone(&headers);
    let span = Span::parse(window.window.as_deref());
    let stop = state.stop.clone();
    let watch = state.watch(Some(project));
    let loader = move || {
        let state = state.clone();
        let viewer = viewer.clone();
        async move {
            let (shared, view) = snapshot(&state, project, span, zone).await?;
            batch(&shared, &view, &viewer)
        }
    };
    Sse::new(page_events(
        watch,
        loader,
        query.version(VersionSignal::Page),
        VersionSignal::Page,
        stop,
    ))
    .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
    .into_response()
}
pub fn registration() -> super::PageRegistration {
    use super::{NavEntry, PageRegistration};
    PageRegistration {
        routes: |state| {
            Router::new()
                .route("/projects/id/{project}/stats", get(page))
                .route("/projects/id/{project}/stats/stream", get(stream))
                .with_state(state.dashboard.clone())
        },
        nav: |project| {
            project
                .map(|id| {
                    // a project's, after its Day
                    vec![NavEntry::new(
                        "stats",
                        format!("/projects/id/{id}/stats"),
                        "Stats",
                        17,
                    )]
                })
                .unwrap_or_default()
        },
        assets: &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(took: &[u64]) -> StageStats {
        let mut s = StageStats::new("x");
        s.took = took.to_vec();
        s.succeeded = took.len();
        s.settle();
        s
    }

    #[test]
    fn a_stage_says_its_median_p90_and_longest() {
        let s = stage(&[1500, 1800, 2100, 1680]);
        assert_eq!(s.median(), Some(1740));
        assert_eq!(s.p90(), Some(2100));
        assert_eq!(s.longest(), Some(2100));
        let s = stage(&(1..=10).map(|n| n * 60).collect::<Vec<_>>());
        assert_eq!(s.median(), Some(330));
        assert_eq!(s.p90(), Some(540));
        assert_eq!(stage(&[]).median(), None);
        assert_eq!(stage(&[7]).p90(), Some(7));
        assert_eq!(rate_text(0.4), "<1");
        assert_eq!(rate_text(33.3), "33");
    }

    #[test]
    fn the_window_is_whole_days_or_hours_on_the_readers_clock() {
        // 2026-10-10 01:22:43 UTC, read at UTC+02:00: 03:22 on the 10th
        let now = super::super::timestamp("2026-10-10T01:22:43Z").unwrap();
        let week = Window::new(Span::Week, now, 120, None);
        assert_eq!(week.buckets.len(), 7);
        assert_eq!(super::super::rfc3339(week.end), "2026-10-10T22:00:00Z");
        assert_eq!(week.bucket_label(week.buckets[6]), "10 Oct");
        assert_eq!(week.words(), "in the last 7 days");
        let day = Window::new(Span::Day, now, 0, None);
        assert_eq!(day.buckets.len(), 24);
        assert_eq!(super::super::rfc3339(day.end), "2026-10-10T02:00:00Z");
        assert_eq!(day.bucket_label(day.buckets[23]), "01:00");
        let first = super::super::timestamp("2026-09-28T10:48:41Z").unwrap();
        let all = Window::new(Span::All, now, 0, Some(first));
        assert_eq!(all.buckets.len(), 13);
        assert_eq!(all.words(), "since 28 September");
        let long = Window::new(Span::All, now, 0, Some(first - 200 * 86_400));
        assert_eq!(long.step, 7 * 86_400);
        assert_eq!(Span::parse(Some("30d")), Span::Month);
        assert_eq!(Span::parse(Some("year")), Span::Week);
    }

    #[test]
    fn a_run_is_put_under_its_stage() {
        let run = |unit: &str, step: &str| StatRun {
            run_id: "r".into(),
            step: step.into(),
            unit: unit.into(),
            item: -1,
            n: 1,
            started: 0,
            ended: Some(60),
            outcome: Some(Shown::Succeeded),
            kind: String::new(),
            kept: true,
        };
        assert_eq!(run("a-1", "a-1-draft").stage(), "draft");
        assert_eq!(run("a-1", "a-1-draft").owner(), "a-1");
        assert_eq!(run("", "survey").stage(), "survey");
        assert_eq!(run("", "survey").owner(), "survey");
        assert_eq!(run("solo", "solo").stage(), "solo");
        assert!(run("", "survey").href(ProjectId::new()).ends_with("/steps/survey?tab=runs"));
    }
}
