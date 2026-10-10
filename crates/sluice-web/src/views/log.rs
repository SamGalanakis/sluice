//! Bounded, snapshot-coherent log pages with kind/thread filters and keyset paging.
use super::{DashboardSnapshot, DashboardState, FunctionCatalog, NavView, TrustedHtml, Viewer};
use askama::Template;
use axum::{
    Router,
    extract::{Path, State},
    http::HeaderMap,
    response::{Html, IntoResponse, Response, Sse, sse::KeepAlive},
    routing::get,
};
use rusqlite::{params_from_iter, types::Value};
use sluice_model::{
    error::PublicError,
    events::{Event, Record},
    ids::{ProjectId, RecordSeq},
};
use sluice_store::{
    ReadPool,
    records::{self, RecordFilter},
};
use std::time::Duration;
const PAGE_SIZE: usize = 50;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogQuery {
    pub kinds: Vec<String>,
    pub threads: Vec<String>,
    /// Only what one step did and said: its records and its thread's messages.
    pub step: String,
    /// Only what one unit's steps did and said, and the unit's own records.
    pub unit: String,
    /// Only failures: steps that failed, failed calls and orphaned runs.
    pub errors: bool,
    pub before: Option<i64>,
    pub after: Option<i64>,
}
impl LogQuery {
    pub fn parse(query: &str) -> Result<Self, PublicError> {
        let mut result = Self::default();
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            match key.as_ref() {
                "kind" | "kinds" => split(&value, &mut result.kinds),
                "thread" => split(&value, &mut result.threads),
                "step" => result.step = value.trim().chars().take(200).collect(),
                "unit" => result.unit = value.trim().chars().take(200).collect(),
                "errors" => result.errors = value == "1",
                "before" | "after" => {
                    if value.is_empty() || value == "0" {
                        continue;
                    }
                    let seq = value
                        .parse::<i64>()
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| bad("page cursor must be a positive integer"))?;
                    if key == "before" {
                        result.before = Some(seq);
                    } else {
                        result.after = Some(seq);
                    }
                }
                _ => {}
            }
        }
        if result.before.is_some() && result.after.is_some() {
            return Err(bad("give before or after, not both"));
        }
        Ok(result)
    }
    pub fn query(&self, before: Option<i64>, after: Option<i64>) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for kind in &self.kinds {
            query.append_pair("kind", kind);
        }
        if !self.threads.is_empty() {
            query.append_pair("thread", &self.threads.join(","));
        }
        if !self.step.is_empty() {
            query.append_pair("step", &self.step);
        }
        if !self.unit.is_empty() {
            query.append_pair("unit", &self.unit);
        }
        if self.errors {
            query.append_pair("errors", "1");
        }
        if let Some(seq) = before {
            query.append_pair("before", &seq.to_string());
        }
        if let Some(seq) = after {
            query.append_pair("after", &seq.to_string());
        }
        query.finish()
    }
    pub fn kinds_text(&self) -> String {
        self.kinds.join(",")
    }
    /// Which of the presets this is: "all" (no kind, no errors), a kind group's, "errors", or
    /// none ("" for a custom choice of kinds).
    pub fn preset(&self) -> &str {
        match (self.errors, self.kinds.as_slice()) {
            (true, []) => "errors",
            (false, []) => "all",
            (false, [one]) if ["step", "run", "message"].contains(&one.as_str()) => one,
            _ => "",
        }
    }
    pub fn is_preset(&self, preset: &str) -> bool {
        self.preset() == preset
    }
    /// A preset's link: its kinds, the step and threads kept.
    pub fn preset_query(&self, preset: &str) -> String {
        let mut query = Self {
            kinds: vec![],
            threads: self.threads.clone(),
            step: self.step.clone(),
            unit: self.unit.clone(),
            errors: preset == "errors",
            before: None,
            after: None,
        };
        if ["step", "run", "message"].contains(&preset) {
            query.kinds.push(preset.into());
        }
        query.query(None, None)
    }
    pub fn threads_text(&self) -> String {
        self.threads.join(",")
    }
}
fn bad(message: &str) -> PublicError {
    PublicError::BadRequest {
        message: message.into(),
    }
}
fn split(text: &str, into: &mut Vec<String>) {
    for item in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !into.iter().any(|s| s == item) {
            into.push(item.into());
        }
    }
}
pub const KIND_OPTIONS: &[&str] = &[
    "plan",
    "plan.edit",
    "plan.input",
    "step",
    "step.output",
    "step.retry",
    "step.cancel",
    "step.submit",
    "step.settle",
    "step.status",
    "step.lease",
    "step.queued",
    "call",
    "message",
    "project",
    "project.pause",
    "project.archive",
    "project.update",
    "project.board",
    "project.rename",
    "project.delete",
    "project.capacity",
    "project.notify",
    "run",
    "run.adopt",
    "run.orphan",
    "run.completion_action",
    "run.completion_action.register",
    "unit",
    "unit.settled",
];
#[derive(Clone, Debug)]
pub struct LogRow {
    pub seq: i64,
    pub at: String,
    pub kind: String,
    pub summary: String,
    /// The summary with its step, unit and thread linked to their pages.
    pub html: TrustedHtml,
    /// How many records in a row said the same (this the newest), and the oldest's seq.
    pub count: usize,
    pub oldest: i64,
    /// On the global log, the record's project: its name and page ("" for the home's own).
    pub place: (String, String),
    pub json: String,
    /// sluice's own housekeeping (retiring done units): such records in a row are one quiet
    /// row, "sluice retired done units 4 times" (`Some` its plan revision and changes).
    pub chore: Option<(u64, usize)>,
    /// The first plan revision of a row of chores, for its sentence.
    pub chore_from: u64,
    /// A step's status change that names no failure: its step and the states it went from and
    /// to, so a step's changes within a minute read as one row ("x pending → running →
    /// succeeded").
    pub change: Option<(String, String, String)>,
    /// What its sentence names that has a page (`links`), to link a merged sentence again.
    pub links: Vec<(String, String, String, String)>,
    /// A unit settling: its unit, its steps, and how many of their status changes before it
    /// this row holds (`folded`), said at its end and linked to the unit's log.
    pub settled: Option<(String, Vec<String>)>,
    pub folded: usize,
    /// A message: drawn with its sender and recipient named as pages name them, and one note
    /// sent to several in a row folded into one row, "a-7-draft to 6 steps: …".
    pub note: Option<LogNote>,
}
/// A message record as its log row says it: who sent it to whom, its first words linked to
/// its thread.
#[derive(Clone, Debug)]
pub struct LogNote {
    from: String,
    from_html: String,
    /// Each recipient: its id and how the row names it.
    to: Vec<(String, String)>,
    /// Every recipient is a step of the plan: "to 6 steps", else "to 3 recipients".
    steps: bool,
    thread: String,
    thread_href: String,
    body: String,
    title: Option<String>,
    question: bool,
    text: String,
}
impl LogNote {
    fn new(
        m: &sluice_model::commands::Message,
        project: ProjectId,
        names: Option<&sluice_runtime::naming::ProjectNaming>,
    ) -> Self {
        let to = m.to.clone().unwrap_or_default();
        let (to_html, steps) = who_html(&to, project, names, false);
        Self {
            from: m.from.clone(),
            from_html: who_html(&m.from, project, names, true).0,
            to: vec![(to, to_html)],
            steps,
            thread: m.thread.clone(),
            thread_href: super::threads::thread_url(project, &m.thread),
            body: m.body.clone(),
            title: m.title.clone(),
            question: m.verb == sluice_model::commands::MessageVerb::Ask,
            text: cut(&crate::markdown::plain(&m.body), 160),
        }
    }
    /// The same note as this one, sent on to someone else within a minute.
    fn repeats(&self, other: &LogNote) -> bool {
        !self.question
            && !other.question
            && self.from == other.from
            && self.thread == other.thread
            && self.body == other.body
            && self.title == other.title
            && other.to.iter().all(|(id, _)| !self.to.iter().any(|(t, _)| t == id))
    }
    fn words(&self) -> String {
        let to = match self.to.as_slice() {
            [(id, _)] => id.clone(),
            many => format!("{} {}", many.len(), if self.steps { "steps" } else { "recipients" }),
        };
        format!("{} to {to}: {}", self.from, self.text)
    }
    fn html(&self) -> TrustedHtml {
        let to = match self.to.as_slice() {
            [(_, html)] => html.clone(),
            many => format!("{} {}", many.len(), if self.steps { "steps" } else { "recipients" }),
        };
        // its words as plain muted text, the thread one small link at their end: the names are
        // the row's links, not the whole excerpt
        TrustedHtml::owned(format!(
            "{} to {to}: <span class=\"lg-said\">{}</span> <a class=\"lg-thread\" href=\"{}\">thread</a>",
            self.from_html,
            super::ui::esc(&self.text),
            super::ui::esc(&self.thread_href)
        ))
    }
}
/// A sender or recipient as a log row names it: a step of the plan by its title and id,
/// linked; the owner as you; anyone else by name. And whether it is a step.
fn who_html(
    id: &str,
    project: ProjectId,
    names: Option<&sluice_runtime::naming::ProjectNaming>,
    lead: bool,
) -> (String, bool) {
    match id {
        "owner" => ((if lead { "You" } else { "you" }).into(), false),
        "orchestrator" => ((if lead { "The orchestrator" } else { "the orchestrator" }).into(), false),
        "" => ("anyone".into(), false),
        id => match names.and_then(|n| n.naming.step(id)) {
            Some(name) => {
                let named = super::ui::StepRef::new(id, Some(name));
                (
                    format!(
                        "<a href=\"/projects/id/{project}/steps/{}\"{}>{}</a>",
                        super::ui::esc(id),
                        if named.titled() { format!(" title=\"{}\"", super::ui::esc(&named.title)) } else { String::new() },
                        named.with_id_html(48).0
                    ),
                    true,
                )
            }
            None => (super::ui::esc(id), false),
        },
    }
}
#[derive(Clone, Debug)]
pub struct LogView {
    pub nav: DashboardSnapshot,
    pub project: Option<ProjectId>,
    pub query: LogQuery,
    pub rows: Vec<LogRow>,
    pub older: String,
    pub newer: String,
}
impl LogView {
    pub fn kinds(&self) -> &'static [&'static str] {
        KIND_OPTIONS
    }
    pub fn selected(&self, kind: &str) -> bool {
        self.query.kinds.iter().any(|k| k == kind)
    }
    /// The log as it is, without its unit filter: the unit line's way back.
    pub fn every_unit(&self) -> String {
        let query = LogQuery {
            unit: String::new(),
            before: None,
            after: None,
            ..self.query.clone()
        };
        format!("{}?{}", self.base(), query.query(None, None))
    }
    /// What an empty page says: no records of what it asks for, said as its filters say it
    /// ("No error records for step a-6-draft."), with a link that drops each filter.
    pub fn empty_html(&self) -> TrustedHtml {
        use super::ui::esc;
        let q = &self.query;
        let what = if q.errors {
            "error records".to_owned()
        } else if q.kinds.is_empty() {
            "records".to_owned()
        } else {
            format!("{} records", q.kinds.join(", "))
        };
        let mut words = format!("No {what}");
        if !q.step.is_empty() {
            words.push_str(&format!(" for step {}", q.step));
        }
        if !q.unit.is_empty() {
            words.push_str(&format!(" in unit {}", q.unit));
        }
        if !q.threads.is_empty() {
            words.push_str(&format!(" on {}", q.threads.join(", ")));
        }
        if q.before.is_some() || q.after.is_some() {
            words.push_str(" on this page");
        }
        words.push('.');
        let href = |query: LogQuery| format!("{}?{}", self.base(), query.query(None, None));
        let mut ways: Vec<(String, &str)> = vec![];
        let paged = LogQuery {
            before: None,
            after: None,
            ..q.clone()
        };
        if q.errors || !q.kinds.is_empty() {
            ways.push((
                href(LogQuery {
                    errors: false,
                    kinds: vec![],
                    ..paged.clone()
                }),
                "Show every kind",
            ));
        }
        if !q.step.is_empty() {
            ways.push((
                href(LogQuery {
                    step: String::new(),
                    ..paged.clone()
                }),
                "Any step",
            ));
        }
        if !q.unit.is_empty() {
            ways.push((
                href(LogQuery {
                    unit: String::new(),
                    ..paged.clone()
                }),
                "Any unit",
            ));
        }
        if !q.threads.is_empty() {
            ways.push((
                href(LogQuery {
                    threads: vec![],
                    ..paged.clone()
                }),
                "Any thread",
            ));
        }
        if q.before.is_some() || q.after.is_some() {
            ways.push((href(paged.clone()), "The latest records"));
        }
        // a step's records alone, none found: it may have left with its unit
        let retired = !q.step.is_empty() && !q.errors && q.kinds.is_empty() && q.threads.is_empty();
        TrustedHtml::owned(format!(
            "<div class=\"empty log-empty\"><p>{}{}</p>{}</div>",
            esc(&words),
            if retired {
                " A step retired with its unit keeps its records until the log is trimmed."
            } else {
                ""
            },
            if ways.is_empty() {
                String::new()
            } else {
                format!(
                    "<p class=\"log-ways\">{}</p>",
                    ways.iter()
                        .map(|(h, w)| format!("<a href=\"{}\">{w}</a>", esc(h)))
                        .collect::<Vec<_>>()
                        .join(" · ")
                )
            }
        ))
    }
    pub fn base(&self) -> String {
        self.project
            .map(|id| format!("/projects/id/{id}/log"))
            .unwrap_or_else(|| "/log".into())
    }
    pub fn body(&self) -> Result<TrustedHtml, PublicError> {
        TrustedHtml::from_template(&LogTemplate { view: self })
            .map_err(super::threads::render_error)
    }
    pub fn version(&self) -> String {
        sluice_store::artifacts::fingerprint(
            format!("{}{:?}{:?}", self.nav.version(), self.query, self.rows).as_bytes(),
        )
    }
    pub fn render(&self, viewer: &Viewer) -> Result<TrustedHtml, PublicError> {
        let nav = NavView::new(&self.nav, self.project, "log")?;
        let stream = if self.query.before.is_none() && self.query.after.is_none() {
            format!("{}/stream?{}", self.base(), self.query.query(None, None))
        } else {
            String::new()
        };
        // the tab: "Log · almanac" on a project's log
        let title = match self
            .project
            .and_then(|id| self.nav.projects.iter().find(|p| p.id == id))
        {
            Some(project) => format!("Log · {}", project.name),
            None => "Log".into(),
        };
        super::render_framed(
            &title,
            &self.body()?,
            &nav,
            viewer,
            &stream,
            &self.version(),
            &self.base(),
            &super::Frame {
                head: self.head(),
                ..super::Frame::default()
            },
        )
        .map_err(super::threads::render_error)
    }
    /// The page's head: "Log", and what this page of it shows ("42 records on this page; the
    /// newest 3m ago").
    pub fn head(&self) -> TrustedHtml {
        let shown: usize = self.rows.iter().map(|r| r.count.max(1)).sum();
        let what = match self.query.preset() {
            "step" => "step records",
            "run" => "run records",
            "message" => "messages",
            "errors" => "errors",
            "all" => "records",
            _ => "records of the kinds chosen",
        };
        let note = match self.rows.first() {
            Some(newest) => format!(
                "{shown} {what} on this page; the newest {}.",
                super::ui::ago(&newest.at)
            ),
            None => format!("No {what} here."),
        };
        TrustedHtml::owned(format!(
            "<div id=\"log-band\" class=\"band-wrap\">{}</div>",
            super::ui::page_head_note("Log", &TrustedHtml::owned(note))
        ))
    }
}
#[derive(Template)]
#[template(path = "log.html")]
struct LogTemplate<'a> {
    view: &'a LogView,
}
pub async fn load(
    reads: &ReadPool,
    project: Option<ProjectId>,
    query: LogQuery,
) -> Result<LogView, PublicError> {
    reads.snapshot(move |sql| {
        if let Some(project) = project { sluice_store::messages::resolve_project(sql, &sluice_model::ids::ProjectSelector::Id(project))?; }
        let nav = super::load_snapshot(sql, FunctionCatalog::default())?;
        // The store owns the closed set of valid event kinds.
        records::read_records(sql, project, &RecordFilter { kinds: query.kinds.clone(), threads: query.threads.clone(), limit: 1, ..Default::default() })?;
        // a project's log is its records; the global log is every project's and the home's
        let (mut condition, mut args) = match project {
            Some(id) => ("project_id=?".to_owned(), vec![Value::Text(id.to_string())]),
            None => ("1".to_owned(), vec![]),
        };
        let mut kinds = query.kinds.clone();
        if kinds.is_empty() && !query.threads.is_empty() { kinds.push("message".into()); }
        if !kinds.is_empty() {
            condition.push_str(" AND (");
            for (index, kind) in kinds.iter().enumerate() {
                if index > 0 { condition.push_str(" OR "); }
                if ["plan", "step", "project", "run", "unit"].contains(&kind.as_str()) { condition.push_str("kind GLOB ?"); args.push(Value::Text(format!("{kind}.*"))); }
                else { condition.push_str("kind=?"); args.push(Value::Text(kind.clone())); }
            }
            condition.push(')');
        }
        if !query.threads.is_empty() {
            condition.push_str(" AND (kind!='message' OR thread IN ("); condition.push_str(&vec!["?"; query.threads.len()].join(",")); condition.push_str("))"); args.extend(query.threads.iter().cloned().map(Value::Text));
        }
        // a fn call that did not fail is noise (a capacity fn runs every few seconds) unless
        // the calls were asked for
        if !query.kinds.iter().any(|k| k == "call") { condition.push_str(" AND NOT (kind='call' AND coalesce(json_extract(payload,'$.status'),'')!='failed')"); }
        // a status record that changes nothing (a running step restarted under a new run) is
        // left out unless step.status was asked for
        if !query.kinds.iter().any(|k| k == "step.status") { condition.push_str(" AND NOT (kind='step.status' AND json_extract(payload,'$.from') IS json_extract(payload,'$.to'))"); }
        if !query.step.is_empty() {
            condition.push_str(" AND (step_id=? OR thread=?)");
            args.push(Value::Text(query.step.clone()));
            args.push(Value::Text(format!("step-{}", query.step)));
        }
        if !query.unit.is_empty() {
            // its steps' records and threads (a step on its own is its own unit), and its own
            condition.push_str(" AND (step_id IN (SELECT step_id FROM steps s WHERE s.project_id=records.project_id AND coalesce(s.unit,s.step_id)=?) OR thread IN (SELECT 'step-'||step_id FROM steps s WHERE s.project_id=records.project_id AND coalesce(s.unit,s.step_id)=?) OR (kind='unit.settled' AND json_extract(payload,'$.unit')=?))");
            args.extend(std::iter::repeat_n(Value::Text(query.unit.clone()), 3));
        }
        // a lease held or let go is the scheduler's bookkeeping, and a notification's delivery
        // the inbox's: each left out unless asked for by name
        if !query.kinds.iter().any(|k| k == "step.lease") { condition.push_str(" AND kind!='step.lease'"); }
        if !query.kinds.iter().any(|k| k == "project.notify") { condition.push_str(" AND kind!='project.notify'"); }
        if query.errors {
            // a cancel is the owner's choice, not an error: `shown::is_cancel`, as SQL
            condition.push_str(&format!(" AND ((kind='step.status' AND json_extract(payload,'$.to')='failed' AND NOT {}) OR (kind='call' AND json_extract(payload,'$.status')='failed') OR kind='run.orphan')", sluice_model::shown::cancel_sql("payload", "$.error")));
        }
        let base_args = args.clone();
        let base_condition = condition.clone();
        if let Some(before) = query.before { condition.push_str(" AND seq<?"); args.push(Value::Integer(before)); }
        if let Some(after) = query.after { condition.push_str(" AND seq>?"); args.push(Value::Integer(after)); }
        let order = if query.after.is_some() { "ASC" } else { "DESC" };
        let mut stmt = sql.prepare(&format!("SELECT seq,at,payload,payload_version,project_id FROM records WHERE {condition} ORDER BY seq {order} LIMIT {PAGE_SIZE}"))?;
        let raw = stmt.query_map(params_from_iter(args), |r| Ok((r.get::<_,i64>(0)?, r.get::<_,String>(1)?, r.get::<_,String>(2)?, r.get::<_,i64>(3)?, r.get::<_,Option<String>>(4)?)))?.collect::<Result<Vec<_>,_>>()?;
        let mut rows = vec![];
        // each project's names, read once: a sentence names a step by its title and id
        let mut names: std::collections::HashMap<ProjectId, std::sync::Arc<sluice_runtime::naming::ProjectNaming>> = Default::default();
        let home = super::home_of(sql);
        for (seq, at, payload, version, owner) in raw {
            if version != sluice_store::schema::RECORD_PAYLOAD_VERSION { return Err(sluice_store::StoreError::InvalidDatabase("unsupported record payload version".into())); }
            let event: Event = serde_json::from_str(&payload)?;
            let json = serde_json::to_value(&event)?;
            let kind = json.get("kind").and_then(|v| v.as_str()).unwrap_or("").into();
            // one sentence a row: a long one (a conflict's file list) is cut, its JSON whole
            let summary = cut(&summary(&event), 240);
            let owner: Option<ProjectId> = owner.and_then(|p| p.parse().ok());
            let named = match owner {
                Some(id) => match names.get(&id) {
                    Some(n) => Some(n.clone()),
                    None => {
                        let n = sluice_runtime::naming::for_project(sql, &home, id).ok();
                        if let Some(n) = &n { names.insert(id, n.clone()); }
                        n
                    }
                },
                None => None,
            };
            let found = links(&json, owner, named.as_deref());
            let note = match (&event, owner) {
                (Event::Message(m), Some(project)) if !(m.verb == sluice_model::commands::MessageVerb::Ask && m.to.as_deref() == Some("owner")) => Some(LogNote::new(m, project, named.as_deref())),
                _ => None,
            };
            let html = match &note {
                Some(note) => note.html(),
                None => linked(&summary, &found),
            };
            // the global log names each record's project
            let place = match (project, owner) {
                (None, Some(id)) => nav.projects.iter().find(|p| p.id == id).map(|p| (p.name.to_string(), p.href())).unwrap_or_else(|| ("a deleted project".into(), String::new())),
                _ => (String::new(), String::new()),
            };
            let chore = match &event {
                Event::PlanEdit { rev, author, reason, ops } if author == "sluice" && reason.starts_with("retire done units") => Some((rev.0, ops.len())),
                _ => None,
            };
            let change = match &event {
                Event::StepStatus { step, from: Some(from), to, error, .. } if error.is_none() || *to != sluice_model::commands::StepStatus::Failed => {
                    Some((step.to_string(), stored_word(from).to_owned(), stored_word(to).to_owned()))
                }
                _ => None,
            };
            let settled = match &event {
                Event::UnitSettled { unit, steps, .. } => Some((unit.to_string(), steps.iter().map(|s| s.id.to_string()).collect())),
                _ => None,
            };
            rows.push(LogRow { seq, at: at.clone(), kind, summary, html, count: 1, oldest: seq, place, chore, chore_from: chore.map_or(0, |c| c.0), change, links: found, settled, folded: 0, note, json: serde_json::to_string_pretty(&Record { seq: RecordSeq(seq), at, project: owner, event })? });
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.seq));
        // records in a row that say the same are one line, "×10"
        let mut grouped: Vec<LogRow> = Vec::with_capacity(rows.len());
        let near = |a: &str, b: &str, secs: u64| match (super::timestamp(a), super::timestamp(b)) {
            (Some(a), Some(b)) => a.abs_diff(b) <= secs,
            _ => false,
        };
        let base = project.map(|id| format!("/projects/id/{id}/log")).unwrap_or_else(|| "/log".into());
        for row in rows {
            // one note sent to several in a row (within a minute) is one row, "x to 6 steps: …",
            // as its thread draws it
            if let Some(note) = &row.note
                && let Some(last) = grouped.last_mut()
                && last.place == row.place
                && near(&last.at, &row.at, 60)
                && let Some(kept) = last.note.as_mut()
                && kept.repeats(note)
            {
                kept.to.extend(note.to.iter().cloned());
                kept.steps &= note.steps;
                last.summary = kept.words();
                last.html = kept.html();
                last.oldest = last.oldest.min(row.seq);
                continue;
            }
            // a settled unit holds its steps' status changes before it (within six hours): one
            // row, "Unit x settled, 5 steps · 9 status changes", those linked to the unit's log,
            // which lists them (and folds nothing)
            if query.unit.is_empty()
                && let Some((step, _, _)) = &row.change
                && let Some(unit) = grouped.iter_mut().rev().take(40).find(|l| {
                    l.place == row.place
                        && l.settled.as_ref().is_some_and(|(_, steps)| steps.contains(step))
                        && near(&l.at, &row.at, 6 * 3600)
                })
            {
                unit.folded += 1;
                unit.oldest = unit.oldest.min(row.seq);
                let (name, _) = unit.settled.clone().unwrap_or_default();
                let lead = linked(&unit.summary, &unit.links).0;
                let href = match unit.place.1.as_str() {
                    "" => format!("{base}?unit={}", super::ui::esc(&name)),
                    page => format!("{page}/log?unit={}", super::ui::esc(&name)),
                };
                unit.html = TrustedHtml::owned(format!(
                    "{lead} · <a href=\"{href}\">{}</a>",
                    super::ui::count(unit.folded, "status change", "status changes")
                ));
                continue;
            }
            // a step's status changes within a minute are one row, "x pending → running →
            // succeeded", the newer reading on from the older (a few rows between them allowed)
            if let Some((step, from, to)) = &row.change {
                let at = grouped.len().saturating_sub(3);
                if let Some(newer) = grouped[at..].iter_mut().rev().find(|n| {
                    n.place == row.place
                        && n.change.as_ref().is_some_and(|(s, f, _)| s == step && f == to)
                        && near(&n.at, &row.at, 60)
                }) {
                    let (_, _, last) = newer.change.clone().unwrap_or_default();
                    let words = format!("{} {from} → {}", step, newer.summary.strip_prefix(&format!("{step} ")).unwrap_or(&last));
                    newer.change = Some((step.clone(), from.clone(), last));
                    newer.html = linked(&words, &newer.links);
                    newer.summary = words;
                    newer.oldest = newer.oldest.min(row.seq);
                    continue;
                }
            }
            // sluice's housekeeping within half an hour is one quiet row, whatever came between
            if row.chore.is_some()
                && let Some(last) = grouped.iter_mut().rev().find(|l| l.chore.is_some() && l.place == row.place)
                && near(&last.at, &row.at, 1800)
            {
                last.count += 1;
                last.oldest = last.oldest.min(row.seq);
                if let (Some((rev, n)), Some((from, m))) = (last.chore, row.chore) {
                    last.chore = Some((rev, n + m));
                    last.chore_from = from;
                }
                let (rev, changes) = last.chore.unwrap_or_default();
                let words = format!(
                    "sluice retired done units {} times: plan revs {} to {rev}, {}",
                    last.count,
                    last.chore_from,
                    super::ui::count(changes, "change", "changes")
                );
                last.html = TrustedHtml::owned(super::ui::esc(&words));
                last.summary = words;
                continue;
            }
            match grouped.last_mut() {
                Some(last) if last.kind == row.kind && last.summary == row.summary && last.place == row.place => {
                    last.count += 1;
                    last.oldest = row.seq;
                }
                _ => grouped.push(row),
            }
        }
        let rows = grouped;
        let exists = |seq, comparator: &str| -> sluice_store::Result<bool> {
            let mut args = base_args.clone(); args.push(Value::Integer(seq));
            Ok(sql.query_row(&format!("SELECT EXISTS(SELECT 1 FROM records WHERE {base_condition} AND seq{comparator}?)"), params_from_iter(args), |r| r.get(0))?)
        };
        let older = if let Some(last) = rows.last() { if exists(last.oldest, "<")? { format!("{base}?{}", query.query(Some(last.oldest), None)) } else { String::new() } } else { String::new() };
        let newer = if let Some(first) = rows.first() { if exists(first.seq, ">")? { format!("{base}?{}", query.query(None, Some(first.seq))) } else { String::new() } } else { String::new() };
        Ok(LogView { nav, project, query, rows, older, newer })
    }).await.map_err(|e| e.into_public(true))
}
/// What a record's sentence names that has a page: its step, unit and thread (in its project).
/// Each is the name as the sentence says it, its page, and how the link names it (a step by its
/// title and id, a unit by its title and id; "" for the name itself).
fn links(
    json: &serde_json::Value,
    project: Option<ProjectId>,
    names: Option<&sluice_runtime::naming::ProjectNaming>,
) -> Vec<(String, String, String, String)> {
    let Some(project) = project else {
        return vec![];
    };
    let field = |name: &str| json.get(name).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let mut links = vec![];
    if let Some(thread) = field("thread") {
        links.push((thread.to_owned(), super::threads::thread_url(project, thread), String::new(), String::new()));
    }
    // a question to the owner says who asked it first ("a-7-draft asked you: …"): a step
    // asking is linked as any step is
    let asker = field("from").filter(|from| {
        field("verb") == Some("ask")
            && field("to") == Some("owner")
            && !["owner", "orchestrator"].contains(from)
    });
    if let Some(step) = field("step").or(asker) {
        let named = super::ui::StepRef::new(step, names.and_then(|n| n.naming.step(step)));
        let (words, full) = if named.titled() { (named.with_id_html(48).0, named.title.clone()) } else { Default::default() };
        links.push((step.to_owned(), format!("/projects/id/{project}/steps/{step}"), words, full));
    }
    if let Some(unit) = field("unit") {
        let title = names.map(|n| n.naming.unit_title(unit)).unwrap_or(unit);
        let full = if title != unit { title.to_owned() } else { String::new() };
        let words = if title != unit {
            format!(
                "<span class=\"sref\"><span class=\"sref-t\">{}</span> <code class=\"sref-id\">{}</code></span>",
                super::ui::esc(&sluice_model::naming::cut(title, 48)),
                super::ui::esc(unit)
            )
        } else {
            String::new()
        };
        links.push((unit.to_owned(), format!("/projects/id/{project}/units/{unit}"), words, full));
    }
    links
}
/// `text`, HTML-escaped, with the first whole-word use of each name linked to its page (a step
/// or unit named there by its title and id).
/// A link whose title is cut carries the whole title as its `title`.
fn linked(text: &str, links: &[(String, String, String, String)]) -> TrustedHtml {
    let word = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/');
    let mut spans: Vec<(usize, usize, &str, &str, &str)> = vec![];
    for (name, href, words, full) in links {
        let found = text.match_indices(name.as_str()).find(|(at, _)| {
            let before = text[..*at].chars().next_back();
            let after = text[at + name.len()..].chars().next();
            !before.is_some_and(word)
                && !after.is_some_and(|c| word(c) && c != '.' && c != '/')
                && !spans.iter().any(|(s, e, _, _, _)| at < e && at + name.len() > *s)
        });
        if let Some((at, _)) = found {
            spans.push((at, at + name.len(), href, words, full));
        }
    }
    spans.sort();
    let esc = super::ui::esc;
    let mut html = String::new();
    let mut at = 0;
    for (start, end, href, words, full) in spans {
        html.push_str(&esc(&text[at..start]));
        let words = if words.is_empty() { esc(&text[start..end]) } else { words.to_owned() };
        let title = if full.is_empty() { String::new() } else { format!(" title=\"{}\"", esc(full)) };
        html.push_str(&format!("<a href=\"{}\"{title}>{words}</a>", esc(href)));
        at = end;
    }
    html.push_str(&esc(&text[at..]));
    TrustedHtml::owned(html)
}
/// A record in one plain sentence: who did what to which step, unit, run or project. A
/// failure reads as `views::failure` says it (a cancel as a cancel), never as a pane dump or
/// JSON; the record's JSON is one click away under it.
fn summary(event: &Event) -> String {
    let because = |reason: &str| {
        let reason = reason.trim();
        if reason.is_empty() {
            String::new()
        } else {
            format!(": {}", cut(reason, 160))
        }
    };
    let maybe = |reason: &Option<String>| because(reason.as_deref().unwrap_or(""));
    match event {
        // a question to the owner by its title: "a-7-draft asked you: Publish a-6 first?"
        Event::Message(m)
            if m.verb == sluice_model::commands::MessageVerb::Ask
                && m.to.as_deref() == Some("owner") =>
        {
            format!(
                "{} asked you: {}",
                m.from,
                m.title
                    .as_deref()
                    .filter(|t| !t.trim().is_empty())
                    .map_or_else(|| super::threads::headline(&m.body, 160), str::to_owned)
            )
        }
        Event::Message(m) => format!(
            "{} to {}: {}",
            m.from,
            m.to.as_deref().unwrap_or("anyone"),
            cut(&crate::markdown::plain(&m.body), 160)
        ),
        Event::StepStatus {
            step, from, to, error, ..
        } => match error {
            Some(error) if *to == sluice_model::commands::StepStatus::Failed => {
                format!("{step}: {}", super::failure::Failure::new(error, None).headline)
            }
            _ => match from {
                Some(from) => format!("{step} {} → {}", stored_word(from), stored_word(to)),
                None => format!("{step} added, {}", stored_word(to)),
            },
        },
        Event::PlanEdit {
            rev,
            author,
            reason,
            ops,
        } => format!(
            "Plan rev {} by {author}, {}{}",
            rev.0,
            super::ui::count(ops.len(), "change", "changes"),
            because(reason)
        ),
        Event::PlanInput {
            name,
            value,
            author,
            reason,
            ..
        } => format!(
            "Plan input {name} set to {} by {author}{}",
            cut(&value.as_value().to_string(), 80),
            because(reason)
        ),
        Event::StepOutput {
            step,
            outputs,
            author,
            reason,
            ..
        } => format!(
            "{step}'s {} set by {author}{}",
            names(outputs.0.keys()),
            because(reason)
        ),
        Event::StepRetry {
            step,
            author,
            reason,
            ..
        } => format!("{step} retried by {author}{}", because(reason)),
        Event::StepCancel {
            step,
            author,
            reason,
        } => format!("{step} cancelled by {author}{}", because(reason)),
        Event::StepSubmit { step, outputs, .. } => {
            format!("{step} submitted {}", names(outputs.0.keys()))
        }
        Event::StepSettle {
            step,
            author,
            reason,
            ..
        } => format!("{step} settled by {author}{}", because(reason)),
        Event::StepLease {
            step,
            resource,
            amount,
            state,
            ..
        } => format!(
            "{} {} {amount} {resource}",
            step.as_ref().map_or("a run", |s| s.as_str()),
            word(state)
        ),
        Event::StepQueued { step, reason, .. } => format!("{step} queued{}", because(reason)),
        Event::Call {
            name,
            status,
            error,
            ..
        } => match error {
            Some(error) if *status == sluice_model::commands::StepStatus::Failed => {
                format!("{name}: {}", super::failure::Failure::new(error, None).headline)
            }
            _ => format!("{name} {}", stored_word(status)),
        },
        Event::ProjectPause {
            paused,
            reason,
            author,
        } => format!(
            "Project {} by {author}{}",
            if *paused { "paused" } else { "unpaused" },
            maybe(reason)
        ),
        Event::ProjectArchive {
            archived,
            reason,
            author,
        } => format!(
            "Project {} by {author}{}",
            if *archived {
                "archived"
            } else {
                "taken out of the archive"
            },
            maybe(reason)
        ),
        Event::ProjectUpdate {
            fields,
            reason,
            author,
        } => capital(&format!(
            "{} changed by {author}{}",
            sentence_list(fields.iter().map(|f| field_words(f).to_owned())),
            maybe(reason)
        )),
        Event::ProjectBoard {
            rev,
            cleared,
            reason,
            author,
        } => {
            if *cleared {
                format!("Board cleared by {author}{}", maybe(reason))
            } else {
                format!("Board set to rev {} by {author}{}", rev.0, maybe(reason))
            }
        }
        Event::ProjectRename {
            old_name,
            new_name,
            author,
        } => format!("Project renamed from {old_name} to {new_name} by {author}"),
        Event::ProjectDelete { name, author, .. } => {
            format!("Project {name} deleted by {author}")
        }
        Event::ProjectCapacity {
            resource,
            name,
            capacity,
            error,
        } => match (capacity, error) {
            (Some(c), _) => format!("{resource} capacity {c}, from {name}"),
            (None, Some(e)) => format!(
                "{resource} capacity unknown: {}",
                super::failure::Failure::new(e, None).headline
            ),
            (None, None) => format!("{resource} capacity unknown, from {name}"),
        },
        Event::ProjectNotify {
            message, outcome, ..
        } => format!("Notification for message {message}: {}", word(outcome)),
        Event::RunAdopt {
            run, step, outcome, ..
        } => format!(
            "Run {run}{} adopted: {}",
            step.as_ref().map(|s| format!(" of {s}")).unwrap_or_default(),
            word(outcome)
        ),
        Event::RunOrphan { run } => format!("Run {run} orphaned: its process was lost"),
        Event::RunCompletionActionRegistered {
            run,
            message,
            author,
            ..
        } => format!(
            "{author} set what run {run} does when it ends{}",
            because(message)
        ),
        Event::RunCompletionAction {
            run,
            outcome,
            author,
        } => format!("Run {run}'s completion action by {author}: {}", word(outcome)),
        Event::UnitSettled { unit, steps, .. } => format!(
            "Unit {unit} settled, {}",
            super::ui::count(steps.len(), "step", "steps")
        ),
        // a kind this release does not know yet: its name, the JSON under it
        other => serde_json::to_value(other)
            .ok()
            .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_owned))
            .map(|kind| format!("A {kind} record"))
            .unwrap_or_else(|| "A record".into()),
    }
}
/// A stored status in the status table's words (`shown`): what the step read as with nothing
/// more known of it.
fn stored_word(status: &sluice_model::commands::StepStatus) -> &'static str {
    sluice_model::shown::classify(&sluice_model::shown::Facts::of(status.clone())).word()
}
/// A status, state or outcome as its wire name, in words: `not_found` reads "not found"; a
/// tagged outcome (`{"outcome": "applied", …}`) by its tag.
fn word<T: serde::Serialize>(value: &T) -> String {
    let value = serde_json::to_value(value).unwrap_or_default();
    let name = match &value {
        serde_json::Value::String(s) => s.as_str(),
        serde_json::Value::Object(o) => o
            .get("outcome")
            .or_else(|| o.values().next())
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        _ => "",
    };
    name.replace('_', " ")
}
fn cut(text: &str, width: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= width {
        return text;
    }
    let mut out: String = text.chars().take(width - 1).collect();
    out.push('…');
    out
}
fn names<'a>(keys: impl Iterator<Item = &'a String>) -> String {
    let keys: Vec<&String> = keys.collect();
    if keys.is_empty() {
        "nothing".into()
    } else {
        sentence_list(keys.into_iter().cloned())
    }
}
/// "a", "a and b", "a, b and c".
fn sentence_list(items: impl Iterator<Item = String>) -> String {
    let items: Vec<String> = items.collect();
    match items.as_slice() {
        [] => "nothing".into(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
/// A project field as its settings page names it.
fn field_words(field: &str) -> &str {
    match field {
        "board_doc" => "the board's document",
        "prune_done_after" => "when done units retire",
        "prune_keep" => "the units never retired",
        "resources" => "the resources",
        "icon" => "the icon",
        "description" => "the description",
        other => other,
    }
}
fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}
pub fn router(state: DashboardState) -> Router {
    Router::new()
        .route("/log", get(global_page))
        .route("/log/stream", get(global_stream))
        .route("/projects/id/{project}/log", get(project_page))
        .route("/projects/id/{project}/log/stream", get(project_stream))
        .with_state(state)
}
async fn global_page(
    State(state): State<DashboardState>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    headers: HeaderMap,
) -> Response {
    page(state, None, uri.query().unwrap_or(""), headers).await
}
async fn project_page(
    State(state): State<DashboardState>,
    Path(project): Path<ProjectId>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    headers: HeaderMap,
) -> Response {
    page(state, Some(project), uri.query().unwrap_or(""), headers).await
}
async fn page(
    state: DashboardState,
    project: Option<ProjectId>,
    raw: &str,
    headers: HeaderMap,
) -> Response {
    let result = async {
        load(&state.reads, project, LogQuery::parse(raw)?)
            .await?
            .render(&Viewer::from_headers(&headers))
    }
    .await;
    match result {
        Ok(html) => Html(html.as_str().to_owned()).into_response(),
        Err(error) => super::inbox::error_response(error),
    }
}
async fn global_stream(
    State(state): State<DashboardState>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    headers: HeaderMap,
) -> Response {
    stream(state, None, uri.query().unwrap_or(""), headers)
}
async fn project_stream(
    State(state): State<DashboardState>,
    Path(project): Path<ProjectId>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    headers: HeaderMap,
) -> Response {
    stream(state, Some(project), uri.query().unwrap_or(""), headers)
}
fn stream(
    state: DashboardState,
    project: Option<ProjectId>,
    raw: &str,
    headers: HeaderMap,
) -> Response {
    use crate::streams::{PatchRegion, RenderedBatch, StreamQuery, VersionSignal, page_events};
    let query = match LogQuery::parse(raw) {
        Ok(q) => q,
        Err(e) => return super::inbox::error_response(e),
    };
    let datastar = url::form_urlencoded::parse(raw.as_bytes())
        .find(|(k, _)| k == "datastar")
        .map(|(_, v)| v.into_owned());
    let version = StreamQuery { project, datastar }.version(VersionSignal::Page);
    let stop = state.stop.clone();
    let viewer = Viewer::from_headers(&headers);
    let events = page_events(
        state.watch(project),
        move || {
            let state = state.clone();
            let query = query.clone();
            let viewer = viewer.clone();
            async move {
                let page = load(&state.reads, project, query).await?;
                let nav = NavView::new(&page.nav, project, "log")?;
                Ok(RenderedBatch {
                    version: page.version(),
                    regions: vec![
                        PatchRegion::new("log-view", page.body()?),
                        PatchRegion::new("log-band", page.head()),
                        PatchRegion::new(
                            "top-nav",
                            super::render_nav(&nav, &viewer, &page.base())
                                .map_err(super::threads::render_error)?,
                        ),
                    ],
                })
            }
        },
        version,
        VersionSignal::Page,
        stop,
    );
    Sse::new(events)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

pub fn registration() -> super::PageRegistration {
    use super::{NavEntry, PageRegistration};
    PageRegistration {
        routes: |state| {
            if state.log {
                router(state.dashboard.clone())
            } else {
                Router::new()
            }
        },
        nav: |project| {
            vec![NavEntry::new(
                "log",
                project
                    .map(|id| format!("/projects/id/{id}/log"))
                    .unwrap_or_else(|| "/log".into()),
                "Log",
                40,
            )]
        },
        assets: &[],
    }
}
