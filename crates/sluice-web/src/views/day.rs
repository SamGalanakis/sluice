//! The day (DESIGN.md, Pages: Day): every run of the last 24 hours, across the projects or of
//! one, drawn from the runs table alone. The band names the day and says it in one sentence;
//! the day line draws each project's runs as bars on one track, stacked where they overlapped,
//! with a rule at now; the timetable sets each run in the hour it started, a column a project,
//! by its start minute, its unit or step, its run number, how it ended and how long it took.
//!
//! Generic by rule: a run is a step's run, named by its unit and its step, its outcome read
//! from the status table. Read-only and server-rendered; its stream patches only when the runs
//! (or the band's nav) change: its version is the runs' facts, never the clock.
use super::ui::{self, Shown, esc};
use super::{
    DashboardState, Frame, NavView, TrustedHtml, Viewer, render_framed, render_nav,
};
use axum::{
    Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use sluice_model::{error::PublicError, ids::ProjectId};
use std::collections::BTreeMap;

/// The hours the day covers: the last 24, the current one last.
pub const HOURS: u64 = 24;
/// A run that succeeded in less than this (seconds) is quick: counted, not listed or drawn.
pub const QUICK: u64 = 60;
/// How many runs an hour's cell lists before it is busy: a busy cell lists its notable runs
/// (`DayView::notable`) and folds the rest, which all succeeded, behind "+N more".
pub const CELL_RUNS: usize = 6;
/// A run that took at least this long (seconds) is notable however it ended.
pub const LONG: u64 = 60 * 60;
/// How many tracks a project's day line stacks before its last holds the rest.
pub const LANES: usize = 14;

/// One run, as the day reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct DayRun {
    pub project: ProjectId,
    pub run_id: String,
    pub step: String,
    /// Its unit's name, "" for a step of no unit.
    pub unit: String,
    /// Its step's run number (its first is 1).
    pub n: usize,
    /// A scatter's item, -1 for none.
    pub item: i64,
    /// When it started and ended, seconds since the epoch; no end while it runs.
    pub started: u64,
    pub ended: Option<u64>,
    /// How it ended, from the status table; `Running` while it runs, none when it ended with
    /// no result recorded.
    pub outcome: Option<Shown>,
    /// Its start as stored (RFC 3339), for a ticking "running for".
    pub started_at: String,
    /// Its step is still in the plan (a done unit retires its steps, but their runs stay).
    pub kept: bool,
    /// Its unit's title (a loose step's own), when its plan still names one: how a line names
    /// it, its id being its page's; "" when it has none.
    pub title: String,
}
impl DayRun {
    /// How long it ran, as of `now`.
    pub fn seconds(&self, now: u64) -> u64 {
        self.ended.unwrap_or(now).saturating_sub(self.started)
    }
    /// It succeeded in less than `QUICK`: counted, not listed.
    pub fn quick(&self) -> bool {
        self.outcome == Some(Shown::Succeeded)
            && self.ended.is_some_and(|e| e.saturating_sub(self.started) < QUICK)
    }
    pub fn running(&self) -> bool {
        self.ended.is_none()
    }
    /// Its name in two parts: its unit's (or its step's) in ink, and what follows it (its step
    /// past the unit's prefix) muted.
    pub fn name(&self) -> (String, String) {
        if self.unit.is_empty() {
            (self.step.clone(), String::new())
        } else if let Some(rest) = self.step.strip_prefix(&format!("{}-", self.unit)) {
            (self.unit.clone(), rest.to_owned())
        } else if self.step == self.unit {
            (self.unit.clone(), String::new())
        } else {
            (self.unit.clone(), self.step.clone())
        }
    }
    /// Its name as a line says it: its title (else its unit's or step's name) in ink, and its
    /// stage after it, muted.
    pub fn label(&self) -> (String, String) {
        let (name, rest) = self.name();
        if self.title.is_empty() {
            (name, rest)
        } else {
            (sluice_model::naming::cut(&self.title, 64), rest)
        }
    }
    /// Its step's runs while the plan keeps the step, else its records on the project's log,
    /// which keeps them after the step is retired.
    pub fn href(&self) -> String {
        if self.kept {
            format!(
                "/projects/id/{}/steps/{}?tab=runs",
                self.project,
                url_part(&self.step)
            )
        } else {
            format!(
                "/projects/id/{}/log?step={}",
                self.project,
                url_part(&self.step)
            )
        }
    }
}
fn url_part(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}
/// A project as the day lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct DayProject {
    pub id: ProjectId,
    pub name: String,
    pub paused: bool,
    pub archived: bool,
}
/// What the day reads from one snapshot: the projects and every run that overlaps its window.
#[derive(Clone, Debug)]
pub struct DayData {
    pub projects: Vec<DayProject>,
    pub runs: Vec<DayRun>,
    /// The last run started before the window, by project (RFC 3339): "nothing since".
    pub last_before: BTreeMap<ProjectId, String>,
}
/// The reader's clock: minutes east of UTC, from the `sluice_zone` cookie `nav.js` keeps (UTC
/// until it has).
pub fn zone(headers: &HeaderMap) -> i64 {
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|c| c.to_str().ok())
        .unwrap_or("")
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(name, _)| *name == "sluice_zone")
        .and_then(|(_, v)| v.parse::<i64>().ok())
        .filter(|m| m.abs() <= 14 * 60)
        .unwrap_or(0)
}
/// "UTC", "UTC+02:00", "UTC−05:30".
pub fn zone_name(minutes: i64) -> String {
    if minutes == 0 {
        return "UTC".into();
    }
    format!(
        "UTC{}{:02}:{:02}",
        if minutes > 0 { "+" } else { "−" },
        minutes.abs() / 60,
        minutes.abs() % 60
    )
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
/// The window the day covers on the reader's clock: from the start of the hour 23 hours before
/// the current one to the end of the current one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub start: u64,
    pub end: u64,
    pub zone: i64,
}
impl Window {
    pub fn at(now: u64, zone: i64) -> Self {
        let local = now as i64 + zone * 60;
        let hour = local.div_euclid(3600) * 3600 - zone * 60;
        let end = (hour + 3600) as u64;
        Self {
            start: end - HOURS * 3600,
            end,
            zone,
        }
    }
    /// The local hour (0–23) of an instant.
    pub fn hour(&self, at: u64) -> u64 {
        ((at as i64 + self.zone * 60).div_euclid(3600)).rem_euclid(24) as u64
    }
    /// The local minute of an instant.
    pub fn minute(&self, at: u64) -> u64 {
        ((at as i64 + self.zone * 60).div_euclid(60)).rem_euclid(60) as u64
    }
    /// "Saturday", "10 October 2026": the local day of an instant.
    pub fn day_name(&self, at: u64) -> (&'static str, String) {
        let days = (at as i64 + self.zone * 60).div_euclid(86_400);
        const WEEK: [&str; 7] = [
            "Thursday",
            "Friday",
            "Saturday",
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
        ];
        let date = super::rfc3339((days * 86_400).max(0) as u64);
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
        let year = date.get(0..4).unwrap_or("");
        let month: usize = date.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(1);
        let day: usize = date.get(8..10).and_then(|d| d.parse().ok()).unwrap_or(1);
        (
            WEEK[days.rem_euclid(7) as usize],
            format!("{day} {} {year}", MONTHS[(month - 1).min(11)]),
        )
    }
    /// Where an instant falls along the window, 0 to 100.
    pub fn percent(&self, at: u64) -> f64 {
        let span = (self.end - self.start) as f64;
        ((at.clamp(self.start, self.end) - self.start) as f64 / span * 100.0).clamp(0.0, 100.0)
    }
    /// The start of each hour, oldest first.
    pub fn hours(&self) -> impl Iterator<Item = u64> + '_ {
        (0..HOURS).map(|h| self.start + h * 3600)
    }
}
/// Every run overlapping the last day (and a little more: a window's start moves within the
/// hour) of `project`, or of every project, from one snapshot.
pub fn load(
    c: &rusqlite::Connection,
    project: Option<ProjectId>,
    now: u64,
) -> sluice_store::Result<DayData> {
    let mut projects = vec![];
    let mut q = c.prepare_cached(
        "SELECT project_id,name,paused,archived FROM projects WHERE deleted_at IS NULL ORDER BY name",
    )?;
    let mut rows = q.query([])?;
    while let Some(r) = rows.next()? {
        let raw: String = r.get(0)?;
        let Ok(id) = raw.parse::<ProjectId>() else {
            continue;
        };
        if project.is_some_and(|p| p != id) {
            continue;
        }
        projects.push(DayProject {
            id,
            name: r.get(1)?,
            paused: r.get(2)?,
            archived: r.get(3)?,
        });
    }
    // a day and an hour back: the window's start, on any reader's clock
    let since = super::rfc3339(now.saturating_sub((HOURS + 1) * 3600 + 14 * 3600));
    let only = project.map(|p| p.to_string()).unwrap_or_default();
    let mut q = c.prepare_cached(
        "WITH numbered AS (SELECT r.project_id,r.run_id,r.step_id,r.unit,r.item_index,coalesce(r.started_at,r.created_at) AS began,r.finished_at,r.result,row_number() OVER (PARTITION BY r.project_id,r.step_id ORDER BY r.created_at,r.run_id) AS n FROM runs r WHERE r.step_id IS NOT NULL AND (?1='' OR r.project_id=?1)) \
         SELECT n.project_id,n.run_id,n.step_id,coalesce(n.unit,s.unit,''),n.n,n.item_index,n.began,n.finished_at,json_extract(n.result,'$.status'),json_extract(n.result,'$.error'),s.step_id IS NOT NULL FROM numbered n JOIN projects p ON p.project_id=n.project_id AND p.deleted_at IS NULL LEFT JOIN steps s ON s.project_id=n.project_id AND s.step_id=n.step_id \
         WHERE n.finished_at IS NULL OR julianday(n.finished_at)>=julianday(?2) ORDER BY n.began,n.run_id",
    )?;
    let mut rows = q.query((&only, &since))?;
    let mut runs = vec![];
    while let Some(r) = rows.next()? {
        let Ok(project) = r.get::<_, String>(0)?.parse::<ProjectId>() else {
            continue;
        };
        let began: String = r.get(6)?;
        let Some(started) = super::timestamp(&began) else {
            continue;
        };
        let finished: Option<String> = r.get(7)?;
        let status: Option<String> = r.get(8)?;
        let error: Option<String> = r.get(9)?;
        let outcome = match (&finished, status.map(|s| s.parse::<sluice_model::commands::StepStatus>())) {
            (None, _) => Some(Shown::Running),
            (Some(_), Some(Ok(status))) => Some(sluice_model::shown::classify(
                &sluice_model::shown::Facts {
                    cancelled: error
                        .as_deref()
                        .is_some_and(sluice_model::shown::stored_is_cancel),
                    ..sluice_model::shown::Facts::of(status)
                },
            )),
            _ => None,
        };
        runs.push(DayRun {
            project,
            run_id: r.get(1)?,
            step: r.get(2)?,
            unit: r.get(3)?,
            n: r.get::<_, i64>(4)?.max(1) as usize,
            item: r.get(5)?,
            started,
            ended: finished.as_deref().and_then(super::timestamp),
            outcome,
            started_at: began,
            kept: r.get(10)?,
            title: String::new(),
        });
    }
    // each run named by its unit's title, as its plan names it now
    let home = super::home_of(c);
    let mut names = BTreeMap::new();
    for run in &mut runs {
        let naming = names.entry(run.project).or_insert_with(|| {
            sluice_runtime::naming::for_project(c, &home, run.project).ok()
        });
        if let Some(naming) = naming {
            let title = if run.unit.is_empty() {
                naming.naming.step_title(&run.step)
            } else {
                naming.naming.unit_title(&run.unit)
            };
            if title != run.unit && title != run.step {
                run.title = title.to_owned();
            }
        }
    }
    // for a project that ran nothing in the day: when it last did
    let mut last_before = BTreeMap::new();
    let mut q = c.prepare_cached(
        "SELECT project_id,max(coalesce(finished_at,started_at,created_at)) FROM runs WHERE step_id IS NOT NULL AND (?1='' OR project_id=?1) GROUP BY project_id",
    )?;
    let mut rows = q.query([&only])?;
    while let Some(r) = rows.next()? {
        if let (Ok(id), Some(at)) = (
            r.get::<_, String>(0)?.parse::<ProjectId>(),
            r.get::<_, Option<String>>(1)?,
        ) {
            last_before.insert(id, at);
        }
    }
    Ok(DayData {
        projects,
        runs,
        last_before,
    })
}

/// The day drawn: its window on the reader's clock, its runs in it.
#[derive(Clone, Debug)]
pub struct DayView {
    pub project: Option<ProjectId>,
    pub window: Window,
    pub now: u64,
    pub projects: Vec<DayProject>,
    /// The runs in the window, by start.
    pub runs: Vec<DayRun>,
    pub last_before: BTreeMap<ProjectId, String>,
}
/// A project's column: its runs in the window, how many and how many were quick.
pub struct Column<'a> {
    pub project: &'a DayProject,
    pub runs: Vec<&'a DayRun>,
}
impl DayView {
    pub fn new(data: DayData, project: Option<ProjectId>, now: u64, zone: i64) -> Self {
        let window = Window::at(now, zone);
        let runs = data
            .runs
            .into_iter()
            .filter(|r| r.started < window.end && r.ended.is_none_or(|e| e >= window.start))
            .collect();
        Self {
            project,
            window,
            now,
            projects: data.projects,
            runs,
            last_before: data.last_before,
        }
    }
    /// What its version reads: its window and each run's facts (never the clock), so its stream
    /// patches when a run starts, ends or moves to another hour, and on the hour.
    pub fn facts(&self) -> String {
        let mut out = format!("{}:{}:{:?}", self.window.start, self.window.zone, self.project);
        for p in &self.projects {
            out.push_str(&format!("|{}:{}:{}:{}", p.id, p.name, p.paused, p.archived));
        }
        for r in &self.runs {
            out.push_str(&format!(
                "|{}:{}:{}:{:?}:{:?}",
                r.run_id,
                r.started,
                r.n,
                r.ended,
                r.outcome.map(|o| o.key())
            ));
        }
        out
    }
    /// A run worth its own line in a busy hour: it did not succeed (it failed, was cancelled,
    /// still runs or ended with no result), or it took `LONG` or more.
    pub fn notable(&self, r: &DayRun) -> bool {
        r.outcome != Some(Shown::Succeeded) || r.seconds(self.now) >= LONG
    }
    /// The projects that ran in the window, the busiest first; a project's own day has its
    /// column whether or not it ran.
    pub fn columns(&self) -> Vec<Column<'_>> {
        let mut columns: Vec<Column<'_>> = self
            .projects
            .iter()
            .map(|p| Column {
                project: p,
                runs: self.runs.iter().filter(|r| r.project == p.id).collect(),
            })
            .filter(|c| self.project.is_some() || !c.runs.is_empty())
            .collect();
        columns.sort_by_key(|c| std::cmp::Reverse(c.runs.len()));
        columns
    }
    /// The projects that ran nothing in the window.
    pub fn idle(&self) -> Vec<&DayProject> {
        self.projects
            .iter()
            .filter(|p| !p.archived && !self.runs.iter().any(|r| r.project == p.id))
            .collect()
    }
    fn tally(runs: &[&DayRun]) -> ui::Tally {
        runs.iter().filter_map(|r| r.outcome).collect()
    }
    /// The band's sentence: how many runs and where, how many ended needing a look, what runs
    /// now and how many were quick.
    pub fn summary(&self) -> TrustedHtml {
        let columns = self.columns();
        let all: Vec<&DayRun> = self.runs.iter().collect();
        if all.is_empty() {
            return TrustedHtml::owned("No run in the last 24 hours.".into());
        }
        let mut parts = vec![];
        let total = ui::count(all.len(), "run", "runs");
        if self.project.is_none() && columns.len() > 1 {
            let each: Vec<String> = columns
                .iter()
                .map(|c| format!("{} in {}", c.runs.len(), c.project.name))
                .collect();
            parts.push(format!("{}: {}.", esc(&total), esc(&join(&each))));
        } else {
            parts.push(format!("{} in the last 24 hours.", esc(&total)));
        }
        let tally = Self::tally(&all);
        let looks = ended_words(&tally);
        if !looks.is_empty() {
            parts.push(format!("{}.", esc(&capital(&join(&looks)))));
        }
        let running: Vec<&DayRun> = all.iter().copied().filter(|r| r.running()).collect();
        match running.iter().min_by_key(|r| r.started) {
            Some(oldest) => parts.push(format!(
                "{} running now, the longest for {}.",
                running.len(),
                ui::since(&oldest.started_at)
            )),
            None => parts.push("Nothing is running now.".into()),
        }
        let quick = all.iter().filter(|r| r.quick()).count();
        if quick > 0 {
            parts.push(format!(
                "{} succeeded in under a minute: counted, not listed.",
                if quick == all.len() {
                    "All of them".to_owned()
                } else {
                    quick.to_string()
                }
            ));
        }
        TrustedHtml::owned(parts.join(" "))
    }
    /// The page's head: "Day" (the nav names a project's), and under it the date and whose
    /// clock its hours are on.
    pub fn head(&self) -> TrustedHtml {
        let (_, date) = self.window.day_name(self.now);
        ui::page_head_note(
            "Day",
            &TrustedHtml::owned(format!(
                "{} · the last 24 hours, hours on {}",
                esc(&date),
                esc(&if self.window.zone == 0 {
                    "UTC".to_owned()
                } else {
                    format!("your clock ({})", zone_name(self.window.zone))
                })
            )),
        )
    }
    pub fn title(&self) -> String {
        match self.project.and_then(|id| self.projects.iter().find(|p| p.id == id)) {
            Some(p) => format!("Day · {}", p.name),
            None => "Day".into(),
        }
    }
    /// The page's body: the day line, then the timetable.
    pub fn body(&self) -> TrustedHtml {
        let mut html = String::from("<div id=\"day-view\" class=\"day\">");
        html.push_str(self.day_line().as_str());
        html.push_str(self.timetable().as_str());
        html.push_str("</div>");
        TrustedHtml::owned(html)
    }
    /// The day line: a row a project, its runs as bars from start to end (running ones to now),
    /// stacked into tracks where they overlapped; the hours along the top, a rule at now.
    pub fn day_line(&self) -> TrustedHtml {
        let w = &self.window;
        let columns = self.columns();
        let mut ticks = String::new();
        for (i, at) in w.hours().enumerate() {
            ticks.push_str(&format!(
                "<span class=\"dl-tick{}\" style=\"left:{:.3}%\"><span>{:02}</span></span>",
                if i % 2 == 1 { " dl-odd" } else { "" },
                w.percent(at),
                w.hour(at)
            ));
        }
        let now_at = w.percent(self.now);
        let mut rows = String::new();
        for column in &columns {
            let drawn: Vec<&&DayRun> = column.runs.iter().filter(|r| !r.quick()).collect();
            // tracks: each bar on the first track free at its start (its end at least a
            // sliver past its start, so short bars never pile into one place)
            let sliver = (w.end - w.start) / 300;
            let mut ends: Vec<u64> = vec![];
            let mut bars = String::new();
            for run in &drawn {
                let start = run.started.max(w.start);
                let end = run.ended.unwrap_or(self.now).min(w.end).max(start + sliver);
                let lane = match ends.iter().position(|e| *e <= start) {
                    Some(i) => i,
                    None if ends.len() < LANES => {
                        ends.push(0);
                        ends.len() - 1
                    }
                    None => LANES - 1,
                };
                ends[lane] = ends[lane].max(end);
                let (name, rest) = run.label();
                let left = w.percent(start);
                let kind = match run.outcome {
                    None => "dl-ended",
                    Some(s) => match ui::Cell::of(Some(s)) {
                        ui::Cell::Run => "dl-run",
                        ui::Cell::Look => "dl-look",
                        _ => "dl-done",
                    },
                };
                let style = format!(
                    "left:{left:.3}%;width:{:.3}%;--lane:{lane}",
                    (w.percent(end) - left).max(0.15)
                );
                bars.push_str(&format!(
                    "<a class=\"dl-bar {kind}\" style=\"{style}\"{open} href=\"{href}\" title=\"{t}\" tabindex=\"-1\"></a>",
                    open = if run.running() {
                        format!(" data-open=\"{}\"", run.started.max(w.start))
                    } else {
                        String::new()
                    },
                    href = esc(&run.href()),
                    t = esc(&format!(
                        "{name}{} · run {} · {}",
                        if rest.is_empty() {
                            String::new()
                        } else {
                            format!(" {rest}")
                        },
                        run.n,
                        match run.outcome {
                            Some(Shown::Running) | None => ui::duration_text(run.seconds(self.now) as f64),
                            Some(s) => format!("{} after {}", s.word(), ui::duration_text(run.seconds(self.now) as f64)),
                        }
                    ))
                ));
            }
            let lanes = ends.len().max(1);
            let tally = Self::tally(&column.runs);
            let said = format!(
                "{}: {}{}",
                column.project.name,
                ui::count(column.runs.len(), "run", "runs"),
                if tally.total() > 0 {
                    format!(", {}", ui::states_words(&tally))
                } else {
                    String::new()
                }
            );
            rows.push_str(&format!(
                "<div class=\"dl-row\"><h3 class=\"dl-name\">{name}</h3><div class=\"dl-track\" role=\"img\" aria-label=\"{said}\" style=\"--lanes:{lanes}\">{bars}</div></div>",
                name = esc(&column.project.name),
                said = esc(&said),
            ));
        }
        let note = self.idle_note();
        let body = if columns.is_empty() {
            "<p class=\"dl-empty\">No run in the last 24 hours.</p>".to_owned()
        } else {
            format!(
                "<div class=\"dl\" data-from=\"{from}\" data-to=\"{to}\" style=\"--now:{now_at:.3}%\"><div class=\"dl-ticks\" aria-hidden=\"true\">{ticks}<div class=\"dl-now\"><span>now, {clock}</span></div></div>{rows}</div>",
                from = w.start,
                to = w.end,
                clock = ui::clock(&super::rfc3339(self.now)),
            )
        };
        TrustedHtml::owned(format!(
            "<section class=\"day-line\" aria-labelledby=\"day-line-h\">{}<p class=\"dl-said\">{}</p>{body}{}</section>",
            ui::section_head(
                "day-line-h",
                "The day line",
                "Each bar a run, start to end; stacked where runs overlapped"
            ),
            self.summary(),
            if note.is_empty() {
                String::new()
            } else {
                format!("<p class=\"dl-note\">{}</p>", esc(&note))
            }
        ))
    }
    /// "Nothing ran in chores or almanac (paused) in these 24 hours."
    fn idle_note(&self) -> String {
        if self.project.is_some() {
            return String::new();
        }
        let idle: Vec<String> = self
            .idle()
            .iter()
            .map(|p| {
                if p.paused {
                    format!("{} (paused)", p.name)
                } else {
                    p.name.clone()
                }
            })
            .collect();
        if idle.is_empty() {
            String::new()
        } else {
            format!("Nothing ran in {} in these 24 hours.", join_or(&idle))
        }
    }
    /// The timetable: a row an hour (the current one last), a column a project, each run by
    /// its start minute.
    pub fn timetable(&self) -> TrustedHtml {
        let w = &self.window;
        let columns = self.columns();
        let n = columns.len().max(1);
        let mut head = String::from("<div class=\"tt-head\" aria-hidden=\"true\"><span class=\"tt-hh\">Hour</span>");
        for c in &columns {
            head.push_str(&format!(
                "<span class=\"tt-ph\"><b>{}</b> {}</span>",
                esc(&c.project.name),
                esc(&ui::count(c.runs.len(), "run", "runs"))
            ));
        }
        head.push_str("</div>");
        let mut rows = String::new();
        let hours: Vec<u64> = w.hours().collect();
        for (i, at) in hours.iter().enumerate() {
            let from = if i == 0 { 0 } else { *at };
            let to = at + 3600;
            let mut cells = String::new();
            let mut started = 0;
            for c in &columns {
                let here: Vec<&DayRun> = c
                    .runs
                    .iter()
                    .copied()
                    .filter(|r| r.started >= from && r.started < to)
                    .collect();
                started += here.len();
                cells.push_str(&self.cell(c.project, &here, i == 0, *at));
            }
            let (day, _) = w.day_name(*at);
            let first_of_day = i == 0 || w.hour(*at) == 0;
            let current = i + 1 == hours.len();
            rows.push_str(&format!(
                "<section class=\"tt-row{busy}{cur}\" aria-labelledby=\"tt-{i}\"><div class=\"tt-h\"><h3 id=\"tt-{i}\"><span class=\"tt-hour\">{hour:02}</span><span class=\"vh\">:00{dayw}</span></h3>{dayl}<p class=\"tt-count\">{count}</p></div>{cells}{now}</section>",
                busy = if started > 0 { " tt-busy" } else { "" },
                cur = if current { " tt-current" } else { "" },
                hour = w.hour(*at),
                dayw = if first_of_day { format!(", {day}") } else { String::new() },
                dayl = if first_of_day {
                    format!("<p class=\"tt-day\" aria-hidden=\"true\">{}</p>", &day[..3])
                } else {
                    String::new()
                },
                count = if started > 0 {
                    esc(&ui::count(started, "run", "runs"))
                } else {
                    String::new()
                },
                now = if current { self.now_line() } else { String::new() },
            ));
        }
        TrustedHtml::owned(format!(
            "<section class=\"timetable\" aria-labelledby=\"timetable-h\">{}<div class=\"tt{}\" style=\"--n:{n}\">{head}{rows}</div></section>",
            ui::section_head(
                "timetable-h",
                "Timetable",
                "Each run in the hour it started: its minute, its unit and step, how it ended and how long it took"
            ),
            // four projects or more stack under each hour sooner than a phone's width
            if n >= 4 { " tt-wide" } else { "" },
        ))
    }
    /// One project's runs in one hour, by their minute; the quick ones counted. A busy hour
    /// (more than `CELL_RUNS` listed) lists its notable runs (`notable`; its longest when none
    /// is) and folds the rest, every one a success, behind "+N more" that opens in place
    /// (a `details`, so it opens without script, its state kept through a patch).
    fn cell(
        &self,
        project: &DayProject,
        runs: &[&DayRun],
        first: bool,
        hour: u64,
    ) -> String {
        let (quick, listed): (Vec<&DayRun>, Vec<&DayRun>) = runs.iter().partition(|r| r.quick());
        let mut shown: Vec<&DayRun> = listed.clone();
        let mut folded: Vec<&DayRun> = vec![];
        if listed.len() > CELL_RUNS {
            let longest = listed
                .iter()
                .max_by_key(|r| r.seconds(self.now))
                .map(|r| r.run_id.as_str())
                .unwrap_or("");
            let any = listed.iter().any(|r| self.notable(r));
            (shown, folded) = listed
                .iter()
                .partition(|r| self.notable(r) || (!any && r.run_id == longest));
        }
        let mut html = format!(
            "<div class=\"tt-c\"><p class=\"tt-pname\">{}</p>",
            esc(&project.name)
        );
        if !shown.is_empty() {
            // a long hour flows into columns as wide as the screen allows
            html.push_str(if shown.len() > 6 {
                "<ol class=\"tt-runs tt-many\">"
            } else {
                "<ol class=\"tt-runs\">"
            });
            for r in &shown {
                html.push_str(&self.run_item(r, first));
            }
            html.push_str("</ol>");
        }
        if !folded.is_empty() {
            let longest = folded.iter().map(|r| r.seconds(self.now)).max().unwrap_or(0);
            let shortest = folded.iter().map(|r| r.seconds(self.now)).min().unwrap_or(0);
            html.push_str(&format!(
                "<details class=\"tt-more\" id=\"tt-more-{hour}-{}\" data-preserve-attr=\"open\"><summary>+{} more succeeded, {} to {}</summary><ol class=\"tt-runs{}\">",
                project.id,
                folded.len(),
                ui::duration_text(shortest as f64),
                ui::duration_text(longest as f64),
                if folded.len() > 6 { " tt-many" } else { "" }
            ));
            for r in &folded {
                html.push_str(&self.run_item(r, first));
            }
            html.push_str("</ol></details>");
        }
        if !quick.is_empty() {
            html.push_str(&format!(
                "<p class=\"tt-quick\">{} under a minute, all succeeded</p>",
                esc(&ui::count(quick.len(), "quick run", "quick runs"))
            ));
        }
        html.push_str("</div>");
        html
    }
    /// One run's line: ":15 Terns: the spring guide draft ✓ 1h 04m"; its step and run number on
    /// its link's title.
    fn run_item(&self, r: &DayRun, first: bool) -> String {
        let w = &self.window;
        let (name, rest) = r.label();
        // a run that started before the window, in its first row: its day too
        let at = if first && r.started < w.start {
            format!(
                "{} {:02}:{:02}",
                &w.day_name(r.started).0[..3],
                w.hour(r.started),
                w.minute(r.started)
            )
        } else {
            format!(":{:02}", w.minute(r.started))
        };
        let took = ui::duration_text(r.seconds(self.now) as f64);
        let outcome = match r.outcome {
            Some(Shown::Running) => format!(
                "<span class=\"tt-out tt-live\">{}running {}</span>",
                ui::mark(Shown::Running),
                ui::since(&r.started_at)
            ),
            Some(s) if s.spec().band == sluice_model::shown::Band::Done => format!(
                "<span class=\"tt-out\">{}<span class=\"vh\">{} after </span>{}</span>",
                ui::mark(s),
                esc(s.word()),
                esc(&took)
            ),
            Some(s) => format!(
                "<span class=\"tt-out tt-look tt-{}\">{}{} <span class=\"tt-took\">{}</span></span>",
                s.key(),
                ui::mark(s),
                esc(s.word()),
                esc(&took)
            ),
            None => format!(
                "<span class=\"tt-out tt-ended\">ended {}</span>",
                esc(&took)
            ),
        };
        format!(
            "<li class=\"tt-run\"><span class=\"tt-m\">{at}</span><a class=\"tt-name\" href=\"{href}\" title=\"{ids}\"><b>{name}</b>{rest}</a>{outcome}</li>",
            href = esc(&r.href()),
            // what identifies the run rides on its link, not its line: its step and run
            ids = esc(&match (r.n, r.item) {
                (1, i) if i < 0 => r.step.clone(),
                (n, i) if i < 0 => format!("{}, run {n}", r.step),
                (n, i) => format!("{}, item {i}, run {n}", r.step),
            }),
            name = esc(&name),
            rest = if rest.is_empty() {
                String::new()
            } else {
                format!(" <span class=\"tt-stage\">{}</span>", esc(&rest))
            },
        )
    }
    /// Under the current hour: the rule at now, when the last run started and what runs now.
    fn now_line(&self) -> String {
        let last = self.runs.iter().map(|r| r.started).max();
        let running: Vec<String> = self
            .runs
            .iter()
            .filter(|r| r.running())
            .map(|r| {
                let (name, rest) = r.label();
                if rest.is_empty() { name } else { format!("{name} {rest}") }
            })
            .collect();
        let mut words = vec![];
        if let Some(last) = last {
            words.push(format!(
                "The last run started at {:02}:{:02}.",
                self.window.hour(last),
                self.window.minute(last)
            ));
        }
        words.push(match running.len() {
            0 => "Nothing is running.".to_owned(),
            1..=6 => format!("Running now: {}.", join(&running)),
            n => format!("{n} running now."),
        });
        format!(
            "<div class=\"tt-nowline\"><p class=\"tt-nowrule\"><span>now, {}</span></p><p class=\"tt-nowsaid\">{}</p></div>",
            ui::clock(&super::rfc3339(self.now)),
            esc(&words.join(" "))
        )
    }
    /// Per project, the day in one line for home's Today: "41 runs, 2 failed; 3 running now,
    /// the longest since 13:32." (or when it last ran).
    pub fn project_line(&self, project: &ProjectId) -> TrustedHtml {
        let runs: Vec<&DayRun> = self.runs.iter().filter(|r| &r.project == project).collect();
        if runs.is_empty() {
            return TrustedHtml::owned(match self.last_before.get(project) {
                Some(at) => format!("Nothing in the last 24 hours; its last run ended {}.", ui::ago(at)),
                None => "No run yet.".to_owned(),
            });
        }
        let tally = Self::tally(&runs);
        let looks = ended_words(&tally);
        let running: Vec<&&DayRun> = runs.iter().filter(|r| r.running()).collect();
        let mut out = esc(&ui::count(runs.len(), "run", "runs"));
        if !looks.is_empty() {
            out.push_str(&format!(", {}", esc(&join(&looks))));
        }
        out.push('.');
        match running.iter().min_by_key(|r| r.started) {
            Some(oldest) => out.push_str(&format!(
                " {} running now, the longest for {}.",
                running.len(),
                ui::since(&oldest.started_at)
            )),
            None => {
                if let Some(last) = runs.iter().filter_map(|r| r.ended).max() {
                    out.push_str(&format!(
                        " The last ended {}.",
                        ui::ago(&super::rfc3339(last))
                    ));
                }
            }
        }
        TrustedHtml::owned(out)
    }
}
/// The page's head (`ui::page_head`) named `id`, so a stream patches it in place.
pub fn band_with_id(head: &TrustedHtml, id: &str) -> TrustedHtml {
    TrustedHtml::owned(head.as_str().replacen(
        "<div class=\"page-head\"",
        &format!("<div id=\"{}\" class=\"page-head\"", esc(id)),
        1,
    ))
}
/// How the runs that need a look ended: "58 failed", "7 were cancelled", "1 ended stale".
fn ended_words(tally: &ui::Tally) -> Vec<String> {
    tally
        .iter()
        .filter(|(s, _)| *s != Shown::Running && s.spec().attention)
        .map(|(s, n)| match s {
            Shown::Failed => format!("{n} failed"),
            Shown::Cancelled if n == 1 => "1 was cancelled".to_owned(),
            Shown::Cancelled => format!("{n} were cancelled"),
            other => format!("{n} ended {}", other.word()),
        })
        .collect()
}
fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}
/// "a", "a and b", "a, b and c".
pub fn join(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
/// "a", "a or b", "a, b or c".
fn join_or(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// The day of `project` (or every project) as of now on the reader's clock, with the nav from
/// the same snapshot.
pub async fn snapshot(
    state: &DashboardState,
    project: Option<ProjectId>,
    zone: i64,
) -> Result<(super::DashboardSnapshot, DayView), PublicError> {
    let now = now();
    let catalog = state.catalog.catalog(project)?;
    let (shared, data) = state
        .reads
        .snapshot(move |c| {
            let shared = super::load_snapshot(c, catalog)?;
            let data = load(c, project, now)?;
            Ok((shared, data))
        })
        .await
        .map_err(|e| e.into_public(true))?;
    if let Some(id) = project
        && !shared.projects.iter().any(|p| p.id == id)
    {
        return Err(PublicError::NotFound {
            message: format!("project {id} not found"),
        });
    }
    Ok((shared, DayView::new(data, project, now, zone)))
}
fn paths(project: Option<ProjectId>) -> (String, String) {
    match project {
        Some(id) => (
            format!("/projects/id/{id}/day"),
            format!("/projects/id/{id}/day/stream"),
        ),
        None => ("/day".into(), "/day/stream".into()),
    }
}
/// The page's regions and its version: the day's facts and the band's nav, never the clock.
fn batch(
    shared: &super::DashboardSnapshot,
    view: &DayView,
    viewer: &Viewer,
) -> Result<crate::streams::RenderedBatch, PublicError> {
    let nav = NavView::new(shared, view.project, "day")?;
    let (path, _) = paths(view.project);
    let nav_html = render_nav(&nav, viewer, &path).map_err(error)?;
    let head = view.head();
    let version = sluice_store::artifacts::fingerprint(
        format!("{}\u{0}{}", view.facts(), nav_html.as_str()).as_bytes(),
    );
    Ok(crate::streams::RenderedBatch {
        version,
        regions: vec![
            crate::streams::PatchRegion::new("day-view", view.body()),
            crate::streams::PatchRegion::new(
                "day-head",
                band_with_id(&head, "day-head"),
            ),
            crate::streams::PatchRegion::new("top-nav", nav_html),
        ],
    })
}
fn error(e: askama::Error) -> PublicError {
    PublicError::Storage {
        message: e.to_string(),
    }
}
async fn page(state: DashboardState, project: Option<ProjectId>, headers: HeaderMap) -> Response {
    let viewer = Viewer::from_headers(&headers);
    let drawn = async {
        let (shared, view) = snapshot(&state, project, zone(&headers)).await?;
        let nav = NavView::new(&shared, project, "day")?;
        let drawn = batch(&shared, &view, &viewer)?;
        let (path, stream) = paths(project);
        render_framed(
            &view.title(),
            &drawn.regions[0].html,
            &nav,
            &viewer,
            &stream,
            &drawn.version,
            &path,
            &Frame {
                head: drawn.regions[1].html.clone(),
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
async fn global_page(State(state): State<DashboardState>, headers: HeaderMap) -> Response {
    page(state, None, headers).await
}
async fn project_page(
    State(state): State<DashboardState>,
    Path(project): Path<ProjectId>,
    headers: HeaderMap,
) -> Response {
    page(state, Some(project), headers).await
}
async fn stream(
    state: DashboardState,
    project: Option<ProjectId>,
    query: crate::streams::StreamQuery,
    headers: HeaderMap,
) -> Response {
    use crate::streams::{VersionSignal, page_events};
    use axum::response::sse::{KeepAlive, Sse};
    let viewer = Viewer::from_headers(&headers);
    let zone = zone(&headers);
    let stop = state.stop.clone();
    let watch = state.watch(project);
    let loader = move || {
        let state = state.clone();
        let viewer = viewer.clone();
        async move {
            let (shared, view) = snapshot(&state, project, zone).await?;
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
async fn global_stream(
    State(state): State<DashboardState>,
    Query(query): Query<crate::streams::StreamQuery>,
    headers: HeaderMap,
) -> Response {
    stream(state, None, query, headers).await
}
async fn project_stream(
    State(state): State<DashboardState>,
    Path(project): Path<ProjectId>,
    Query(query): Query<crate::streams::StreamQuery>,
    headers: HeaderMap,
) -> Response {
    stream(state, Some(project), query, headers).await
}
pub fn registration() -> super::PageRegistration {
    use super::{NavEntry, PageRegistration};
    PageRegistration {
        routes: |state| {
            Router::new()
                .route("/day", get(global_page))
                .route("/day/stream", get(global_stream))
                .route("/projects/id/{project}/day", get(project_page))
                .route("/projects/id/{project}/day/stream", get(project_stream))
                .with_state(state.dashboard.clone())
        },
        nav: |project| {
            vec![NavEntry::new(
                "day",
                project
                    .map(|id| format!("/projects/id/{id}/day"))
                    .unwrap_or_else(|| "/day".into()),
                "Day",
                // a project's: after its plan; every project's: before the log
                if project.is_some() { 15 } else { 30 },
            )]
        },
        assets: &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_the_last_24_hours_on_the_readers_clock() {
        // 2026-10-10 01:22:43 UTC, read at UTC+02:00: 03:22 local
        let now = super::super::timestamp("2026-10-10T01:22:43Z").unwrap();
        let w = Window::at(now, 120);
        assert_eq!(super::super::rfc3339(w.end), "2026-10-10T02:00:00Z");
        assert_eq!(w.end - w.start, 24 * 3600);
        assert_eq!(w.hour(now), 3);
        assert_eq!(w.minute(now), 22);
        assert_eq!(w.hours().count(), 24);
        assert_eq!(w.hour(w.start), 4);
        assert_eq!(w.day_name(now), ("Saturday", "10 October 2026".into()));
        assert_eq!(zone_name(120), "UTC+02:00");
        assert_eq!(zone_name(-330), "UTC−05:30");
        let utc = Window::at(now, 0);
        assert_eq!(utc.hour(now), 1);
        assert_eq!(utc.day_name(now).0, "Saturday");
    }

    #[test]
    fn a_run_is_named_by_its_unit_and_its_step() {
        let run = |unit: &str, step: &str| DayRun {
            title: String::new(),
            project: ProjectId::new(),
            run_id: "r".into(),
            step: step.into(),
            unit: unit.into(),
            n: 1,
            item: -1,
            started: 0,
            ended: Some(30),
            outcome: Some(Shown::Succeeded),
            started_at: String::new(),
            kept: true,
        };
        assert_eq!(run("a-1", "a-1-draft").name(), ("a-1".into(), "draft".into()));
        // a step the plan still has links its runs; a retired one its records on the log
        let kept = run("a-1", "a-1-draft");
        assert!(kept.href().ends_with("/steps/a-1-draft?tab=runs"), "{}", kept.href());
        let gone = DayRun { kept: false, ..kept };
        assert!(gone.href().ends_with("/log?step=a-1-draft"), "{}", gone.href());
        assert_eq!(run("", "survey").name(), ("survey".into(), String::new()));
        assert_eq!(run("index", "index-merge").name(), ("index".into(), "merge".into()));
        assert!(run("", "x").quick());
    }
}
