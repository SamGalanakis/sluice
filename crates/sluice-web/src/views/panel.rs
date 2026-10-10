//! The project's board (`docs("board")`): an OpenUI Lang program stored on the project, drawn
//! here on the server beside the plan (its own section on a phone). The question components
//! draw as the inbox draws them; the data components (Units, StepStatus, Output, Metric,
//! Query, Chart, Doc, LatestMessage) are filled from the project when the page renders and
//! again on each live patch; Doc (the board's document), Markdown and LatestMessage draw
//! markdown through the dashboard's renderer. A query runs read-only through the `query`
//! tool's path and limits, with `?` bound to the project's id. A step is named by its id or as
//! `tag:<tag>`, the one step carrying that tag. A component that cannot be drawn becomes an
//! inline error box, and one whose data names a step the plan no longer has says so above it;
//! the page never fails for either. A Button sends the orchestrator a `say` from the owner.
use super::board::{self, CatalogSignatures, Registry};
use super::icons::{Icon, icon};
use super::step::StepView;
use super::{DashboardState, TrustedHtml};
use rusqlite::OptionalExtension;
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use serde::Serialize;
use serde_json::{Value, json};
use sluice_model::{
    commands::{CommandRequest, Say, UnitState},
    error::PublicError,
    ids::{ProjectId, ProjectSelector},
    openui::{self, Board, Component, Problem, Value as Ui},
    plan::Plan,
    status::UnitRow,
};
use sluice_store::query::{self, QueryCell, QueryLimits};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Queries a board may run per render; past that each is an error box.
pub const MAX_QUERIES: usize = 16;
/// The most rows a Query table shows, and the most points a Chart draws.
const TABLE_ROWS: usize = 50;
const CHART_POINTS: usize = 60;
/// A stream re-runs its board's queries when the project changes, and at least this often.
const QUERY_TTL: Duration = Duration::from_secs(30);

/// The board as drawn: its revision and its HTML. Serialized into the page's version, so a
/// change in any value it shows patches the page.
#[derive(Clone, Debug, Serialize)]
pub struct Panel {
    pub rev: u64,
    /// The program's own title: a level-1 Heading that leads it, drawn as the board's head
    /// instead of "Board" (and not again under it).
    pub title: Option<String>,
    /// When the board's words last changed (its `project.board` record or a document edit's
    /// `project.update`), and whether the plan has changed since: what they may be behind.
    pub written: Option<String>,
    pub plan_changed: bool,
    /// The board draws a written document, whose own line says when and by whom it was
    /// last edited: the head then does not say it again.
    pub doc_dated: bool,
    pub html: String,
}
impl Panel {
    pub fn html(&self) -> TrustedHtml {
        TrustedHtml::owned(self.html.clone())
    }
    /// The board's head: its title (the program's, else "Board"), and with `age` when its words
    /// last changed, "the plan has changed since" when it has. The time reads "2h ago" with script.
    pub fn head(&self, age: bool) -> TrustedHtml {
        let mut out = format!(
            "<div class=\"board-head\"><h2 id=\"board-h\" class=\"board-h{}\">{}{}</h2>",
            if self.title.is_some() { "" } else { " generic" },
            icon(Icon::LayoutDashboard, 16, ""),
            esc(self.title.as_deref().unwrap_or("Board"))
        );
        if let Some(at) = self.written.as_deref().filter(|_| age && !self.doc_dated) {
            let _ = write!(
                out,
                "<p class=\"meta board-age\">Updated {}{}.</p>",
                ago(at),
                if self.plan_changed {
                    "; the plan has changed since"
                } else {
                    ""
                }
            );
        }
        out.push_str("</div>");
        TrustedHtml::owned(out)
    }
}

/// One stream's query results, kept while the project does not change.
#[derive(Default)]
pub struct QueryCache(Mutex<Option<CachedQueries>>);
/// The token the results were run at, when, and each query's outcome.
type CachedQueries = (String, Instant, BTreeMap<String, QueryOutcome>);
pub type SharedCache = Arc<QueryCache>;

#[derive(Clone, Debug)]
pub struct QueryTable {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<QueryCell>>,
    pub truncated: bool,
}
type QueryOutcome = Result<QueryTable, String>;

struct UnitsData {
    rows: Vec<UnitRow>,
    done: Option<(usize, usize)>,
    /// How each unit reads on the board (`board::UnitView::shown`) and each of its steps' short
    /// name and state: the table draws them as the plan does, a cancel and a quiet run too.
    looks: BTreeMap<String, (super::ui::Shown, Vec<(String, super::ui::Shown)>)>,
}
/// An Output's value: the step's output, or its progress when that is fresher (`step_progress`).
#[derive(Clone, Debug)]
struct OutputValue {
    value: Value,
    /// Set when the value is progress: when it was set, and whether the step still runs.
    progress: Option<(String, bool)>,
}
/// What one render needs from the store, gathered in one read snapshot.
pub(crate) struct Loaded {
    rev: u64,
    written: Option<String>,
    plan_changed: bool,
    board: Result<Board, Vec<Problem>>,
    units: BTreeMap<Vec<String>, Result<UnitsData, String>>,
    outputs: BTreeMap<(String, String), Option<OutputValue>>,
    steps: BTreeMap<String, Option<StepView>>,
    /// Every step of the plan, for the step checks.
    known: BTreeSet<String>,
    /// Each tag the plan's steps carry, with the steps carrying it (`tag:<tag>`).
    tags: BTreeMap<String, Vec<String>>,
    /// The names the board uses as a sender or a message column's value that are not plan
    /// steps but once were (`StepLookup::was_step`).
    once: BTreeSet<String>,
    /// The board's document, read when the board draws its Doc.
    doc: Option<sluice_store::projects::BoardDoc>,
    /// Each LatestMessage sender's newest message in the project, if any.
    latest: BTreeMap<String, Option<LatestMessage>>,
    /// What each `Count` counts: the board's own count of the project's steps by state.
    counts: super::ui::Tally,
    token: String,
}
impl Loaded {
    /// What `Count(of)` says: the steps in the state keyed `of`, or every step.
    fn count(&self, of: &str) -> Option<usize> {
        match of {
            "steps" => Some(self.counts.total()),
            key => super::ui::Shown::from_key(key).map(|s| self.counts.get(s)),
        }
    }
}
impl openui::StepLookup for Loaded {
    fn has_step(&self, id: &str) -> bool {
        self.known.contains(id)
    }
    fn tagged(&self, tag: &str) -> usize {
        self.tags.get(tag).map_or(0, Vec::len)
    }
    fn was_step(&self, name: &str) -> bool {
        self.once.contains(name)
    }
}
impl Loaded {
    /// The step a component names: its id, or the one step carrying `tag:<tag>`; else why not.
    fn step(&self, named: &str) -> Result<String, String> {
        match openui::StepTarget::parse(named) {
            openui::StepTarget::Step(id) => Ok(id),
            openui::StepTarget::Tag(tag) => match self.tags.get(&tag).map(Vec::as_slice) {
                Some([one]) => Ok(one.clone()),
                None | Some([]) => Err(format!("no plan step carries the tag {tag} (tag:{tag})")),
                Some(many) => Err(format!(
                    "{} plan steps carry the tag {tag} (tag:{tag}); it must name one",
                    many.len()
                )),
            },
        }
    }
}
/// A sender's newest message, as LatestMessage draws it.
#[derive(Clone, Debug)]
struct LatestMessage {
    id: i64,
    thread: String,
    body: String,
    at: String,
}

/// Load and draw the project's board, or `draft` instead (the settings preview), in a snapshot
/// of its own. `None` when the project has no board. `view` is the plan's page model (its steps
/// give StepStatus).
pub async fn load(
    state: &DashboardState,
    project: ProjectId,
    registry: Option<&Registry>,
    view: &board::ProjectView,
    draft: Option<String>,
    cache: Option<&QueryCache>,
) -> Result<Option<Panel>, PublicError> {
    let exact = registry.map(|r| r.0.signatures(project)).transpose()?;
    let catalog = state.catalog.catalog(Some(project))?;
    let view = view.clone();
    let loaded = state
        .reads
        .snapshot(move |c| {
            let doc: String = c.query_row(
                "SELECT doc FROM plans WHERE project_id=?1",
                [project.to_string()],
                |r| r.get(0),
            )?;
            let plan = match &exact {
                Some(exact) => Plan::parse_json(doc.as_bytes(), exact),
                None => Plan::parse_json(doc.as_bytes(), &CatalogSignatures(&catalog)),
            };
            gather(c, project, plan.as_ref().ok(), &view, draft)
        })
        .await
        .map_err(|e| e.into_public(true))?;
    draw(state, project, loaded, cache).await
}

/// Read what the board shows inside the caller's transaction: its program (or `draft`), and
/// the units, outputs and steps its components name. `None` when the project has no board.
/// `plan` is the stored plan compiled, `None` when it does not compile.
pub(crate) fn gather(
    c: &rusqlite::Connection,
    project: ProjectId,
    plan: Option<&Plan>,
    view: &board::ProjectView,
    draft: Option<String>,
) -> sluice_store::Result<Option<Loaded>> {
    let (program, rev): (Option<String>, i64) = c.query_row(
        "SELECT board,board_rev FROM projects WHERE project_id=?1 AND deleted_at IS NULL",
        [project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let Some(program) = draft.or(program) else {
        return Ok(None);
    };
    let steps = || view.units.iter().flat_map(|u| &u.steps);
    let board = openui::check_board(&program);
    // the last change to the board's words (the program, or its document: a project.update
    // whose fields name it), and whether a plan edit came after it (by record order)
    let written: Option<(i64, String)> = c
        .query_row(
            "SELECT seq,at FROM records WHERE project_id=?1 AND (kind='project.board' OR (kind='project.update' AND EXISTS(SELECT 1 FROM json_each(payload,'$.fields') WHERE value='board_doc'))) ORDER BY seq DESC LIMIT 1",
            [project.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let plan_changed = match &written {
        Some((seq, _)) => c.query_row(
            "SELECT EXISTS(SELECT 1 FROM plan_edits WHERE project_id=?1 AND seq>?2)",
            rusqlite::params![project.to_string(), seq],
            |r| r.get(0),
        )?,
        None => false,
    };
    let mut loaded = Loaded {
        rev: rev as u64,
        written: written.map(|(_, at)| at),
        plan_changed,
        units: BTreeMap::new(),
        outputs: BTreeMap::new(),
        steps: BTreeMap::new(),
        known: steps().map(|s| s.id.to_string()).collect(),
        tags: BTreeMap::new(),
        once: BTreeSet::new(),
        doc: None,
        latest: BTreeMap::new(),
        counts: view.project.counts.clone(),
        token: String::new(),
        board,
    };
    for step in steps() {
        for tag in &step.tags {
            loaded
                .tags
                .entry(tag.clone())
                .or_default()
                .push(step.id.to_string());
        }
    }
    let Ok(board) = &loaded.board else {
        return Ok(Some(loaded));
    };
    let plan_rev: i64 = c.query_row(
        "SELECT rev FROM plans WHERE project_id=?1",
        [project.to_string()],
        |r| r.get(0),
    )?;
    let seq: Option<i64> = c.query_row(
        "SELECT max(seq) FROM records WHERE project_id=?1",
        [project.to_string()],
        |r| r.get(0),
    )?;
    // progress writes no record, so it counts apart: a new value or a cleared one reruns them
    let (progressed, progress_at): (i64, Option<String>) = c.query_row(
        "SELECT count(progress),max(progress_at) FROM steps WHERE project_id=?1",
        [project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    loaded.token = format!(
        "{rev}:{plan_rev}:{}:{progressed}:{}",
        seq.unwrap_or(0),
        progress_at.unwrap_or_default()
    );
    // a sender or a message column's value not in the plan is stale only if it was a step
    let history = sluice_store::projects::StoredSteps {
        sql: c,
        project,
        records: false,
    };
    for r in board.step_refs() {
        if let openui::StepTarget::Step(name) = &r.target
            && matches!(
                r.site,
                openui::RefSite::Sender | openui::RefSite::Sql { step_column: false }
            )
            && !loaded.known.contains(name)
            && openui::StepLookup::was_step(&history, name)
        {
            loaded.once.insert(name.clone());
        }
    }
    let mut wants_units = vec![];
    let mut wants_doc = false;
    let mut steps_named = vec![];
    let mut outputs_named = vec![];
    let mut senders = vec![];
    board.walk(&mut |c, _| match c.name.as_str() {
        "Units" => wants_units.push(c.strings_arg(0)),
        "Doc" => wants_doc = true,
        "LatestMessage" => senders.extend(c.str_arg(0).map(str::to_owned)),
        "StepStatus" => steps_named.extend(c.str_arg(0).map(str::to_owned)),
        "Output" => {
            if let (Some(step), Some(field)) = (c.str_arg(0), c.str_arg(1)) {
                outputs_named.push((step.to_owned(), field.to_owned()));
            }
        }
        _ => {}
    });
    for named in steps_named {
        let view = loaded
            .step(&named)
            .ok()
            .and_then(|id| steps().find(|s| s.id.as_str() == id).cloned());
        loaded.steps.insert(named, view);
    }
    for (named, field) in outputs_named {
        loaded.outputs.insert((named, field), None);
    }
    for from in senders {
        loaded.latest.insert(from, None);
    }
    for filter in wants_units {
        let rows = match plan {
            None => Err("the plan cannot be compiled, so its units are unknown".into()),
            Some(plan) => {
                let wanted: Vec<UnitState> = filter
                    .iter()
                    .filter_map(|s| serde_json::from_value(json!(s)).ok())
                    .collect();
                let wanted = (!filter.is_empty()).then_some(wanted.as_slice());
                sluice_runtime::status::unit_rows(c, project, plan, wanted)
                    .map_err(|e| e.into_public(true).to_string())
                    .map(|v| UnitsData {
                        looks: v
                            .rows
                            .iter()
                            .filter_map(|row| view.units.iter().find(|u| u.id.as_str() == row.unit))
                            .map(|unit| {
                                let prefix = format!("{}-", unit.id);
                                let steps = unit
                                    .steps
                                    .iter()
                                    .map(|s| {
                                        let id = s.id.as_str();
                                        let short = id.strip_prefix(&prefix).unwrap_or(id);
                                        (short.to_owned(), s.shown())
                                    })
                                    .collect();
                                (unit.id.to_string(), (unit.shown(), steps))
                            })
                            .collect(),
                        rows: v.rows,
                        done: v.done,
                    })
            }
        };
        loaded.units.insert(filter, rows);
    }
    let keys: Vec<(String, String)> = loaded.outputs.keys().cloned().collect();
    for (named, field) in keys {
        let Ok(step) = loaded.step(&named) else {
            continue;
        };
        let outputs: Option<String> = c
            .query_row(
                "SELECT outputs FROM steps WHERE project_id=?1 AND step_id=?2",
                rusqlite::params![project.to_string(), step],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        // the freshest value: progress unless the step has finished with outputs since
        let progress = sluice_store::attempts::read_progress(c, project, &step)?;
        let value = match progress
            .as_ref()
            .and_then(|p| p.fresher(&field).map(|v| (p, v)))
        {
            Some((p, value)) => Some(OutputValue {
                value: value.clone(),
                progress: Some((p.at.clone(), p.live)),
            }),
            None => outputs
                .and_then(|o| serde_json::from_str::<Value>(&o).ok())
                .and_then(|o| o.get(&field).cloned())
                .map(|value| OutputValue {
                    value,
                    progress: None,
                }),
        };
        loaded.outputs.insert((named, field), value);
    }
    if wants_doc {
        loaded.doc = Some(sluice_store::projects::board_doc(c, project)?);
    }
    let senders: Vec<String> = loaded.latest.keys().cloned().collect();
    for named in senders {
        let Ok(from) = loaded.step(&named) else {
            continue;
        };
        let message = c
            .query_row(
                "SELECT id,thread,body,at FROM messages WHERE project_id=?1 AND \"from\"=?2 ORDER BY id DESC LIMIT 1",
                rusqlite::params![project.to_string(), from],
                |r| {
                    Ok(LatestMessage {
                        id: r.get(0)?,
                        thread: r.get(1)?,
                        body: r.get(2)?,
                        at: r.get(3)?,
                    })
                },
            )
            .optional()?;
        loaded.latest.insert(named, message);
    }
    Ok(Some(loaded))
}

/// Run the gathered board's queries and draw it.
pub(crate) async fn draw(
    state: &DashboardState,
    project: ProjectId,
    loaded: Option<Loaded>,
    cache: Option<&QueryCache>,
) -> Result<Option<Panel>, PublicError> {
    let Some(loaded) = loaded else {
        return Ok(None);
    };
    let queries = match &loaded.board {
        Ok(board) => run_queries(state, project, board, &loaded.token, cache).await?,
        Err(_) => BTreeMap::new(),
    };
    let (html, title) = render(project, &loaded, &queries);
    let doc_dated = loaded.doc.as_ref().is_some_and(|d| {
        d.at.is_some() && !d.markdown.trim().is_empty()
    });
    Ok(Some(Panel {
        rev: loaded.rev,
        title,
        written: loaded.written.clone(),
        plan_changed: loaded.plan_changed,
        doc_dated,
        html,
    }))
}

/// The SQL of every Metric, Query and Chart, in drawing order.
fn queries_of(board: &Board) -> Vec<String> {
    let mut out = vec![];
    board.walk(&mut |c, _| match c.name.as_str() {
        "Metric" => out.extend(c.str_arg(1).map(str::to_owned)),
        "Query" => out.extend(c.str_arg(0).map(str::to_owned)),
        "Chart" => out.extend(c.str_arg(1).map(str::to_owned)),
        _ => {}
    });
    out
}

async fn run_queries(
    state: &DashboardState,
    project: ProjectId,
    board: &Board,
    token: &str,
    cache: Option<&QueryCache>,
) -> Result<BTreeMap<String, QueryOutcome>, PublicError> {
    let mut wanted = queries_of(board);
    wanted.dedup();
    if let Some(cache) = cache {
        let held = cache.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((held_token, at, results)) = held.as_ref()
            && held_token == token
            && at.elapsed() < QUERY_TTL
            && wanted.iter().all(|q| results.contains_key(q))
        {
            return Ok(results.clone());
        }
    }
    let home = state.reads.home().to_owned();
    let results = tokio::task::spawn_blocking(move || {
        let mut out = BTreeMap::new();
        for (index, sql) in wanted.into_iter().enumerate() {
            if out.contains_key(&sql) {
                continue;
            }
            let outcome = if index >= MAX_QUERIES {
                Err(format!("a board runs at most {MAX_QUERIES} queries"))
            } else {
                run_query(&home, project, &sql)
            };
            out.insert(sql, outcome);
        }
        out
    })
    .await
    .map_err(|e| PublicError::Storage {
        message: e.to_string(),
    })?;
    if let Some(cache) = cache {
        *cache.0.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((token.to_owned(), Instant::now(), results.clone()));
    }
    Ok(results)
}

/// One query through the `query` tool's path and limits; each placeholder is the project id.
fn run_query(home: &std::path::Path, project: ProjectId, sql: &str) -> QueryOutcome {
    let count = placeholders(sql)?;
    let params = vec![rusqlite::types::Value::Text(project.to_string()); count];
    query::query_with_limits(home, sql, Some(&params), Some(200), QueryLimits::default())
        .map(|r| QueryTable {
            columns: r.columns().to_vec(),
            rows: r.rows().to_vec(),
            truncated: r.truncated(),
        })
        .map_err(|e| match e {
            PublicError::Invalid { errors, message } if !errors.is_empty() => {
                errors.first().cloned().unwrap_or(message)
            }
            other => other.to_string(),
        })
}

/// How many parameters SQLite will see: `?` takes the next number, `?N` sets it. Named
/// parameters (`:a`, `@a`, `$a`) are refused: a board query binds only the project id.
fn placeholders(sql: &str) -> Result<usize, String> {
    let chars: Vec<char> = sql.chars().collect();
    let (mut i, mut highest) = (0, 0usize);
    while i < chars.len() {
        match chars[i] {
            q @ ('\'' | '"' | '`') => {
                i += 1;
                while i < chars.len() && chars[i] != q {
                    i += 1;
                }
            }
            '[' => {
                while i < chars.len() && chars[i] != ']' {
                    i += 1;
                }
            }
            '-' if chars.get(i + 1) == Some(&'-') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 1;
            }
            '?' => {
                let digits: String = chars[i + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if digits.is_empty() {
                    highest += 1;
                } else {
                    highest = highest.max(digits.parse().map_err(|_| "a bad ?N")?);
                    i += digits.len();
                }
            }
            ':' | '@' | '$'
                if chars
                    .get(i + 1)
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == '_')
                    && i > 0
                    && !chars[i - 1].is_ascii_alphanumeric() =>
            {
                return Err(
                    "a board query binds only ? (the project's id); named parameters are refused"
                        .into(),
                );
            }
            _ => {}
        }
        i += 1;
    }
    if highest > 32 {
        return Err("a board query has at most 32 placeholders".into());
    }
    Ok(highest)
}

/// A rendered document's first section and the rest: it splits before its second heading
/// (a heading in code is escaped, so never matches); a document with one section or none is
/// all first.
fn first_section(html: &str) -> (&str, &str) {
    let heading = |at: usize| {
        let b = html.as_bytes();
        b[at] == b'<'
            && b.get(at + 1) == Some(&b'h')
            && b.get(at + 2).is_some_and(|c| (b'1'..=b'6').contains(c))
            && b.get(at + 3).is_some_and(|c| *c == b'>' || *c == b' ')
    };
    let mut seen = 0;
    for (at, _) in html.match_indices('<') {
        if heading(at) {
            seen += 1;
            if seen == 2 {
                return html.split_at(at);
            }
        }
    }
    (html, "")
}

// ---- drawing -------------------------------------------------------------------------------

use super::ui::esc;
/// `text` escaped, each word with a hyphen inside it (a step id, a unit, `A-5004`) kept on
/// one line: a narrow board breaks between ids, never inside one.
fn prose(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if kept_whole(word) {
            let _ = write!(out, "<span class=\"ou-id\">{}</span>", esc(word));
        } else {
            out.push_str(&esc(word));
        }
    }
    out
}
/// A word kept on one line: one with a hyphen inside it (`a-5004-draft`, `A-5004`), short
/// enough to be an id. A longer one (a path, a hash with dashes) may break anywhere, so it never
/// pushes the board wider than its column.
fn kept_whole(word: &str) -> bool {
    let chars: Vec<char> = word.chars().collect();
    chars.len() <= 40
        && chars
            .windows(3)
            .any(|w| w[1] == '-' && w[0].is_alphanumeric() && w[2].is_alphanumeric())
}
/// A time the page's script reads as "2h ago": without script the UTC day and minute.
fn ago(at: &str) -> String {
    super::ui::ago(at).0
}
/// Markdown drawn through the dashboard's renderer (escaped, unsafe link schemes refused),
/// its headings from level `top` down, each id in its text kept on one line (`keep_ids`).
fn markdown(text: &str, class: &str, top: u8) -> String {
    format!(
        "<div class=\"md board-md{}{class}\">{}</div>",
        if class.is_empty() { "" } else { " " },
        keep_ids(crate::markdown::render_from(text, top).as_str())
    )
}
/// Rendered HTML with each word that has a hyphen inside it (a step or unit id, `A-5004`)
/// in its text wrapped to stay on one line, as `prose` keeps them; tags, attributes and code
/// are left as they are.
fn keep_ids(html: &str) -> String {
    let mut out = String::with_capacity(html.len() + 32);
    let mut code = 0usize;
    let mut rest = html;
    while !rest.is_empty() {
        if rest.starts_with('<') {
            let end = rest.find('>').map_or(rest.len(), |i| i + 1);
            let tag = &rest[..end];
            if tag.starts_with("<code") || tag.starts_with("<pre") {
                code += 1;
            } else if tag.starts_with("</code") || tag.starts_with("</pre") {
                code = code.saturating_sub(1);
            }
            out.push_str(tag);
            rest = &rest[end..];
            continue;
        }
        let end = rest.find('<').unwrap_or(rest.len());
        let text = &rest[..end];
        if code > 0 {
            out.push_str(text);
        } else {
            for (i, word) in text.split(' ').enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                if kept_whole(word) {
                    let _ = write!(out, "<span class=\"ou-id\">{word}</span>");
                } else {
                    out.push_str(word);
                }
            }
        }
        rest = &rest[end..];
    }
    out
}
/// `text` cut to at most `chars` characters, at a space when one is near the end, and
/// whether it was cut.
fn clip(text: &str, chars: usize) -> (String, bool) {
    let text = text.trim_end();
    if text.chars().count() <= chars {
        return (text.to_owned(), false);
    }
    let head: String = text.chars().take(chars).collect();
    let floor = head.len() * 4 / 5;
    let at = head
        .char_indices()
        .rev()
        .find(|(i, c)| *i >= floor && c.is_whitespace())
        .map_or(head.len(), |(i, _)| i);
    (format!("{}…", head[..at].trim_end()), true)
}
fn cut(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_owned()
    } else {
        let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

struct Draw<'a> {
    project: ProjectId,
    /// The root Stack's leading Heading is the board's head: not drawn again.
    skip_lead: bool,
    /// The board leads with its own title, so a level-2 Heading sits right under the head.
    titled: bool,
    /// The level of the heading drawn last (the column's head is 2): markdown's headings
    /// start one under it.
    depth: u8,
    loaded: &'a Loaded,
    queries: &'a BTreeMap<String, QueryOutcome>,
    buttons: usize,
    fields: usize,
    out: String,
}

/// A program's title: a level-1 Heading leading its root Stack (a column).
fn lead_title(root: &Component) -> Option<String> {
    let first = (root.name == "Stack" && root.str_arg(1) != Some("row"))
        .then(|| root.components_arg(0).into_iter().next())??;
    (first.name == "Heading" && first.num_arg(1) == Some(1.0))
        .then(|| first.str_arg(0).map(str::to_owned))?
        .filter(|t| !t.trim().is_empty())
}

/// The board drawn, and its title when the program leads with one (see `lead_title`).
fn render(
    project: ProjectId,
    loaded: &Loaded,
    queries: &BTreeMap<String, QueryOutcome>,
) -> (String, Option<String>) {
    let title = loaded.board.as_ref().ok().and_then(|b| lead_title(&b.root));
    let mut d = Draw {
        project,
        skip_lead: title.is_some(),
        titled: title.is_some(),
        depth: 2,
        loaded,
        queries,
        buttons: 0,
        fields: 0,
        out: String::new(),
    };
    let rev = loaded.rev;
    let _ = write!(
        d.out,
        "<form class=\"board-form\" method=\"post\" action=\"/projects/id/{project}/board/action\" novalidate data-board-rev=\"{rev}\"><input type=\"hidden\" name=\"board_rev\" value=\"{rev}\">"
    );
    match &loaded.board {
        Ok(board) => {
            d.out.push_str("<div class=\"ou-root board-ui\">");
            d.component(&board.root);
            d.out.push_str("</div>");
        }
        Err(problems) => {
            d.error_box(
                "This board does not check",
                &problems
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
            );
        }
    }
    d.out.push_str(
        "<p class=\"board-status ou-status\" role=\"status\" aria-live=\"polite\" data-ignore-morph></p></form>",
    );
    (d.out, title)
}

impl Draw<'_> {
    fn error_box(&mut self, title: &str, lines: &[String]) {
        let _ = write!(
            self.out,
            "<div class=\"ou-error-box\" role=\"note\">{}<div><b>{}</b>",
            icon(Icon::TriangleAlert, 16, "eb-icon"),
            esc(title)
        );
        for line in lines {
            let _ = write!(self.out, "<p>{}</p>", esc(line));
        }
        self.out.push_str("</div></div>");
    }
    fn component_error(&mut self, c: &Component, message: &str) {
        self.error_box(
            &format!("{} (line {})", c.name, c.line),
            &[message.to_owned()],
        );
    }
    fn children(&mut self, items: Vec<&Component>) {
        for child in items {
            self.component(child);
        }
    }
    fn component(&mut self, c: &Component) {
        match c.name.as_str() {
            "Stack" => {
                let row = c.str_arg(1) == Some("row");
                let _ = write!(
                    self.out,
                    "<div class=\"ou-stack {}\">",
                    if row { "ou-row" } else { "ou-col" }
                );
                let mut items = c.components_arg(0);
                if std::mem::take(&mut self.skip_lead) && !items.is_empty() {
                    items.remove(0);
                }
                self.children(items);
                self.out.push_str("</div>");
            }
            "Heading" => {
                // under the column's h2: a titled board's level 2 is h3, an untitled one's
                // level 1 is; never a skipped level
                let level = c.num_arg(1).unwrap_or(2.0).round().clamp(1.0, 3.0) as u8;
                let tag = (level + if self.titled { 1 } else { 2 }).clamp(3, 6);
                self.depth = tag;
                let _ = write!(
                    self.out,
                    "<h{tag} class=\"ou-h{}\">{}</h{tag}>",
                    if level == 1 { " ou-h1" } else { "" },
                    esc(c.str_arg(0).unwrap_or(""))
                );
            }
            "Text" => {
                let muted = c.str_arg(1) == Some("muted");
                let _ = write!(
                    self.out,
                    "<p class=\"ou-text{}\">{}</p>",
                    if muted { " muted" } else { "" },
                    prose(c.str_arg(0).unwrap_or(""))
                );
            }
            "Callout" => {
                let variant = c.str_arg(1).unwrap_or("info");
                let _ = write!(
                    self.out,
                    "<div class=\"ou-callout ou-{}\" role=\"note\">",
                    esc(variant)
                );
                if let Some(title) = c.str_arg(2) {
                    let _ = write!(self.out, "<b>{}</b>", esc(title));
                }
                let _ = write!(
                    self.out,
                    "<div>{}</div></div>",
                    prose(c.str_arg(0).unwrap_or(""))
                );
            }
            "Table" => {
                let columns = c.strings_arg(0);
                let rows: Vec<Vec<String>> = match c.arg(1) {
                    Some(Ui::Array(rows)) => rows
                        .iter()
                        .map(|r| match r {
                            Ui::Array(cells) => {
                                cells.iter().filter_map(openui::scalar_text).collect()
                            }
                            _ => vec![],
                        })
                        .collect(),
                    _ => vec![],
                };
                self.table(&columns, &rows, c.str_arg(2), None);
            }
            "Separator" => self.out.push_str("<hr class=\"ou-sep\">"),
            "Form" => {
                let _ = write!(
                    self.out,
                    "<fieldset class=\"ou-form\" data-form=\"{}\">",
                    esc(c.str_arg(0).unwrap_or(""))
                );
                self.children(c.components_arg(1));
                self.out.push_str("<div class=\"ou-stack ou-row\">");
                self.children(c.components_arg(2));
                self.out.push_str("</div></fieldset>");
            }
            "Input" | "Textarea" | "Select" | "Radio" | "Checkbox" => self.field(c),
            "Button" => {
                let index = self.buttons;
                self.buttons += 1;
                let secondary = c.str_arg(3) == Some("secondary");
                let _ = write!(
                    self.out,
                    "<button type=\"submit\" name=\"button\" value=\"{index}\" class=\"{}\">{}</button>",
                    if secondary { "secondary" } else { "primary" },
                    esc(c.str_arg(0).unwrap_or(""))
                );
            }
            "Units" => self.units(c),
            "StepStatus" => self.step_status(c),
            "Output" => self.output(c),
            "Metric" | "Query" | "Chart" => self.warned(c, Self::query_component),
            "Count" => {
                let of = c.str_arg(1).unwrap_or("");
                match self.loaded.count(of) {
                    Some(n) => {
                        let _ = write!(
                            self.out,
                            "<div class=\"board-metric\"><span class=\"metric-v\">{n}</span><span class=\"metric-l\">{}</span></div>",
                            esc(c.str_arg(0).unwrap_or(""))
                        );
                    }
                    None => self.component_error(c, &format!("Count cannot count {of}")),
                }
            }
            "Doc" => self.doc(c),
            "Markdown" => {
                let html = markdown(c.str_arg(0).unwrap_or(""), "", self.depth + 1);
                self.out.push_str(&html);
            }
            "LatestMessage" => self.warned(c, Self::latest_message),
            other => self.component_error(c, &format!("{other} is not a board component")),
        }
    }
    fn field(&mut self, c: &Component) {
        let index = self.fields;
        self.fields += 1;
        let name = format!("field-{index}");
        let label = match c.name.as_str() {
            "Checkbox" => None,
            _ => c.str_arg(if matches!(c.name.as_str(), "Select" | "Radio") {
                2
            } else {
                1
            }),
        };
        let keep = "data-preserve-attr=\"value checked\"";
        let control = match c.name.as_str() {
            "Input" => {
                let ty = c.str_arg(3).unwrap_or("text");
                format!(
                    "<input name=\"{name}\" type=\"{}\" value=\"{}\" placeholder=\"{}\" {keep}>",
                    esc(ty),
                    esc(c.str_arg(4).unwrap_or("")),
                    esc(c.str_arg(2).unwrap_or(""))
                )
            }
            "Textarea" => format!(
                "<textarea name=\"{name}\" rows=\"{}\" placeholder=\"{}\">{}</textarea>",
                c.num_arg(5).unwrap_or(3.0).clamp(1.0, 30.0) as u32,
                esc(c.str_arg(2).unwrap_or("")),
                esc(c.str_arg(3).unwrap_or(""))
            ),
            "Select" => {
                let value = c.str_arg(3);
                let mut s = format!("<select name=\"{name}\"><option value=\"\">—</option>");
                for option in c.strings_arg(1) {
                    let _ = write!(
                        s,
                        "<option value=\"{0}\" data-preserve-attr=\"selected\"{1}>{0}</option>",
                        esc(&option),
                        if value == Some(option.as_str()) {
                            " selected"
                        } else {
                            ""
                        }
                    );
                }
                s.push_str("</select>");
                s
            }
            "Radio" => {
                let value = c.str_arg(3);
                let mut s = String::from("<div class=\"ou-stack ou-col\">");
                for option in c.strings_arg(1) {
                    let _ = write!(
                        s,
                        "<label class=\"ou-choice\"><input type=\"radio\" name=\"{name}\" value=\"{0}\" {keep}{1}> {0}</label>",
                        esc(&option),
                        if value == Some(option.as_str()) {
                            " checked"
                        } else {
                            ""
                        }
                    );
                }
                s.push_str("</div>");
                s
            }
            _ => format!(
                "<span class=\"ou-choice\"><input type=\"checkbox\" name=\"{name}\" value=\"true\" {keep}{}> {}</span>",
                if c.bool_arg(2) == Some(true) {
                    " checked"
                } else {
                    ""
                },
                esc(c.str_arg(1).unwrap_or(""))
            ),
        };
        let tag = if c.name == "Radio" { "fieldset" } else { "label" };
        let _ = write!(self.out, "<{tag} class=\"ou-field\">");
        if let Some(label) = label {
            if tag == "fieldset" {
                let _ = write!(self.out, "<legend>{}</legend>", esc(label));
            } else {
                let _ = write!(self.out, "<span class=\"ou-label\">{}</span>", esc(label));
            }
        }
        let _ = write!(
            self.out,
            "{control}<span class=\"ou-error\" data-error-for=\"{name}\" data-ignore-morph hidden></span></{tag}>"
        );
    }
    fn table(
        &mut self,
        columns: &[String],
        rows: &[Vec<String>],
        caption: Option<&str>,
        note: Option<String>,
    ) {
        self.out
            .push_str("<div class=\"scroll ou-table-wrap\"><table class=\"ou-table\">");
        if let Some(caption) = caption {
            let _ = write!(self.out, "<caption>{}</caption>", esc(caption));
        }
        self.out.push_str("<thead><tr>");
        for column in columns {
            let _ = write!(self.out, "<th scope=\"col\">{}</th>", esc(column));
        }
        self.out.push_str("</tr></thead><tbody>");
        for row in rows {
            self.out.push_str("<tr>");
            for cell in row {
                let _ = write!(self.out, "<td>{}</td>", prose(cell));
            }
            self.out.push_str("</tr>");
        }
        self.out.push_str("</tbody></table></div>");
        if let Some(note) = note {
            let _ = write!(self.out, "<p class=\"meta ou-note\">{}</p>", esc(&note));
        }
    }
    fn units(&mut self, c: &Component) {
        let filter = c.strings_arg(0);
        let Some(result) = self.loaded.units.get(&filter) else {
            return self.component_error(c, "its units could not be read");
        };
        let data = match result {
            Ok(data) => data,
            Err(message) => return self.component_error(c, message),
        };
        let base = format!("/projects/id/{}", self.project);
        // Explicit roles: a narrow board lays each row out as a grid, which must not cost the
        // table its semantics.
        self.out.push_str("<div class=\"scroll ou-table-wrap\"><table class=\"ou-table board-units\" role=\"table\"><thead role=\"rowgroup\"><tr role=\"row\"><th scope=\"col\" role=\"columnheader\">Unit</th><th scope=\"col\" role=\"columnheader\">State</th><th scope=\"col\" role=\"columnheader\">Steps</th><th scope=\"col\" role=\"columnheader\">Waiting on</th></tr></thead><tbody role=\"rowgroup\">");
        let mut marks = BTreeSet::new();
        for row in &data.rows {
            // the unit and its steps as the plan reads them; the tool's own reading if the
            // board lacks the unit
            let (shown, steps) = match data.looks.get(&row.unit) {
                Some((shown, steps)) => (*shown, steps.clone()),
                None => (row.shown, vec![]),
            };
            // how long since, ticking from the instant it counts from (`nav.js`): the clock's
            // text, which the page's version leaves out, so the age alone never patches it
            let age = match (&row.since, row.age) {
                (Some(at), Some(_)) => format!(
                    " <time data-since=\"{a}\" datetime=\"{a}\" class=\"meta\" title=\"{t}\">{}</time>",
                    esc(sluice_model::status::age_text(row.age).trim()),
                    a = esc(at),
                    t = esc(&super::ui::at_text(at)),
                ),
                _ => String::new(),
            };
            // its last message as plain words: its markdown's marks and line breaks dropped
            let last = crate::markdown::plain(&row.last);
            let waiting = [row.blocked.as_str(), last.trim()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .map(|s| cut(s, 90))
                .collect::<Vec<_>>()
                .join(" · ");
            // each step's short id and mark kept whole; the marks wrap between steps
            let steps = steps
                .iter()
                .map(|(short, step)| {
                    marks.insert(*step);
                    format!(
                        "<span class=\"u-mark\" title=\"{}\">{}</span>",
                        esc(step.word()),
                        super::ui::stage(*step, &esc(short), true)
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            let _ = write!(
                self.out,
                "<tr role=\"row\"><td class=\"u-unit\" role=\"cell\"><a href=\"{base}/units/{0}\">{0}</a></td><td class=\"u-state\" role=\"cell\">{1}<span>{2}</span>{3}</td><td class=\"u-steps\" role=\"cell\">{4}</td><td class=\"u-wait\" role=\"cell\">{5}</td></tr>",
                esc(&row.unit),
                super::ui::mark(shown),
                esc(shown.word()),
                age,
                steps,
                prose(&waiting)
            );
        }
        self.out.push_str("</tbody></table></div>");
        // the key to the marks the table shows, in one line, in the table's order
        let key: Vec<String> = marks
            .iter()
            .map(|step| {
                format!(
                    "<span class=\"u-k\">{}</span>",
                    super::ui::stage(*step, &esc(step.word()), false)
                )
            })
            .collect();
        if !key.is_empty() {
            let _ = write!(
                self.out,
                "<p class=\"meta u-key\"><span class=\"vh\">Step marks: </span>{}</p>",
                key.join(" ")
            );
        }
        let mut notes = vec![];
        if data.rows.is_empty() {
            notes.push("No units match.".to_owned());
        }
        if let Some((units, steps)) = data.done {
            notes.push(format!(
                "{} ({}) left out.",
                super::ui::count(units, "done unit", "done units"),
                super::ui::count(steps, "step", "steps")
            ));
        }
        if !notes.is_empty() {
            let _ = write!(
                self.out,
                "<p class=\"meta ou-note\">{}</p>",
                esc(&notes.join(" "))
            );
        }
    }
    fn step_status(&mut self, c: &Component) {
        let named = c.str_arg(0).unwrap_or("");
        let step = match self.loaded.step(named) {
            Ok(step) => step,
            Err(why) => return self.component_error(c, &why),
        };
        let Some(Some(view)) = self.loaded.steps.get(named) else {
            return self.component_error(c, &format!("the plan has no step {step}"));
        };
        let mut caption = view.caption();
        if caption.is_empty() {
            caption = view.shown().word().to_owned();
        }
        let hold = view.hold_words();
        let reason = (!hold.is_empty())
            .then_some(hold)
            .or_else(|| view.waits.first().cloned())
            .or_else(|| view.queued.first().cloned())
            .or_else(|| (!view.error.is_empty()).then(|| view.error.clone()));
        let _ = write!(
            self.out,
            "<div class=\"board-step\"><a class=\"{} board-chip\" href=\"{}\">{}<span class=\"sid\">{}</span><span class=\"dur\">{}</span></a>",
            view.card_class(),
            view.href(),
            super::ui::glyph(view.shown()),
            esc(view.id.as_str()),
            esc(&caption)
        );
        if let Some(reason) = reason {
            let _ = write!(
                self.out,
                "<p class=\"meta board-why\">{}</p>",
                esc(&cut(&reason, 160))
            );
        }
        self.out.push_str("</div>");
    }
    fn output(&mut self, c: &Component) {
        let (named, field) = (c.str_arg(0).unwrap_or(""), c.str_arg(1).unwrap_or(""));
        let step = match self.loaded.step(named) {
            Ok(step) => step,
            Err(why) => return self.component_error(c, &why),
        };
        let step = step.as_str();
        if !self.loaded.known.contains(step) {
            return self.component_error(c, &format!("the plan has no step {step}"));
        }
        let value = self
            .loaded
            .outputs
            .get(&(named.to_owned(), field.to_owned()))
            .cloned()
            .flatten();
        let progress = value.as_ref().and_then(|v| v.progress.clone());
        let shown = match value.as_ref().map(|v| &v.value) {
            None => "<span class=\"quiet\">Not set yet.</span>".to_owned(),
            Some(Value::String(s)) => format!("<span class=\"v\">{}</span>", esc(&cut(s, 160))),
            Some(Value::Bool(b)) => format!("<span class=\"v bool\">{b}</span>"),
            Some(Value::Number(n)) => format!("<span class=\"v num\">{n}</span>"),
            Some(Value::Null) => "<span class=\"quiet\">none</span>".to_owned(),
            Some(other) => format!("<code class=\"v\">{}</code>", esc(&cut(&other.to_string(), 160))),
        };
        // progress (`step_progress`) is marked as such: live, with the running glyph, while the
        // step runs; plain "progress" after; and when it was set
        let mark = match progress {
            None => String::new(),
            Some((at, live)) => format!(
                "<span class=\"meta board-progress\">{}{}</span>",
                if live {
                    super::ui::tag("live", "live", Some(super::ui::mark(super::ui::Shown::Running)))
                } else {
                    super::ui::tag("progress", "muted", None)
                },
                ago(&at)
            ),
        };
        let _ = write!(
            self.out,
            "<div class=\"board-output\"><span class=\"meta\">{}/{}</span>{shown}{mark}</div>",
            esc(step),
            esc(field)
        );
    }
    /// The board's document as markdown, with when and by whom it was last edited; until it
    /// says something, the fallback (or "Not written yet."), muted.
    fn doc(&mut self, c: &Component) {
        self.out.push_str("<div class=\"board-doc\">");
        match self.loaded.doc.as_ref().filter(|d| !d.markdown.trim().is_empty()) {
            Some(doc) => {
                // its first section, then the rest folded: on a phone the parts after the
                // document (the live widgets, the quick check) stay near the top; the fold
                // opens itself where the board has the room (`sluice-fold`'s `wide`)
                let inner = keep_ids(
                    crate::markdown::render_from(&doc.markdown, self.depth + 1).as_str(),
                );
                let (first, rest) = first_section(&inner);
                let _ = write!(self.out, "<div class=\"md board-md\">{first}</div>");
                if !rest.is_empty() {
                    let _ = write!(
                        self.out,
                        "{}<div class=\"md board-md\">{rest}</div>{}",
                        super::ui::more_open("doc-more", "Read more", "Read less", "", true),
                        super::ui::more_close("Read less"),
                    );
                }
                if let Some(at) = &doc.at {
                    // the board's one "when": the head leaves it to this line
                    let by = doc
                        .author
                        .as_deref()
                        .filter(|a| !a.is_empty())
                        .map(|a| format!(" by {}", esc(a)))
                        .unwrap_or_default();
                    let _ = write!(
                        self.out,
                        "<p class=\"meta board-fresh\">Edited {}{by}{}</p>",
                        ago(at),
                        if self.loaded.plan_changed {
                            "; the plan has changed since"
                        } else {
                            ""
                        }
                    );
                }
            }
            None => match c.str_arg(0) {
                Some(fallback) => self.out.push_str(&markdown(fallback, "muted", self.depth + 1)),
                None => self
                    .out
                    .push_str("<p class=\"ou-text muted\">Not written yet.</p>"),
            },
        }
        self.out.push_str("</div>");
    }
    /// A component whose data names a step the plan no longer has (Metric, Query, Chart,
    /// LatestMessage) drawn under a warning line, since what it shows is that step's last
    /// data; the two kept together, so a row of parts keeps each line over its own part.
    fn warned(&mut self, c: &Component, draw: fn(&mut Self, &Component)) {
        let mut lines = String::new();
        for r in openui::component_refs(c) {
            let Some(openui::RefProblem::Missing(step)) = r.problem(self.loaded) else {
                continue;
            };
            let _ = write!(
                lines,
                "<p class=\"board-stale\" role=\"note\">{}<span>Names step <code>{}</code>, which is not in the plan; this shows its last data.</span></p>",
                icon(Icon::TriangleAlert, 14, "bs-icon"),
                esc(&step)
            );
        }
        if lines.is_empty() {
            return draw(self, c);
        }
        self.out.push_str("<div class=\"board-warned\">");
        self.out.push_str(&lines);
        draw(self, c);
        self.out.push_str("</div>");
    }
    /// The newest message from the sender: who and when (a link to it in its thread), and
    /// its body as markdown, cut to `chars` with a link to the whole.
    fn latest_message(&mut self, c: &Component) {
        let named = c.str_arg(0).unwrap_or("");
        let from = match self.loaded.step(named) {
            Ok(from) => from,
            Err(why) => return self.component_error(c, &why),
        };
        let from = from.as_str();
        let chars = c
            .num_arg(1)
            .map_or(openui::DEFAULT_MESSAGE_CHARS, |n| n as usize);
        let Some(message) = self.loaded.latest.get(named).cloned().flatten() else {
            let _ = write!(
                self.out,
                "<p class=\"ou-text muted board-message\">No message from {} yet.</p>",
                prose(from)
            );
            return;
        };
        let href = format!(
            "{}#message-{}",
            super::threads::thread_url(self.project, &message.thread),
            message.id
        );
        let (body, cut) = clip(&message.body, chars);
        let _ = write!(
            self.out,
            "<div class=\"board-message\"><p class=\"meta board-msg-head\"><span class=\"m-from\">{}</span> · <a href=\"{}\">{}</a></p>{}",
            prose(from),
            esc(&href),
            ago(&message.at),
            markdown(&body, "", self.depth + 1)
        );
        if cut {
            let _ = write!(
                self.out,
                "<p class=\"meta board-more\"><a href=\"{}\">The whole message</a></p>",
                esc(&href)
            );
        }
        self.out.push_str("</div>");
    }
    fn query_component(&mut self, c: &Component) {
        let sql_index = if c.name == "Query" { 0 } else { 1 };
        let Some(sql) = c.str_arg(sql_index) else {
            return self.component_error(c, "it has no query");
        };
        let table = match self.queries.get(sql) {
            Some(Ok(table)) => table.clone(),
            Some(Err(message)) => return self.component_error(c, message),
            None => return self.component_error(c, "its query did not run"),
        };
        match c.name.as_str() {
            "Metric" => {
                let value = table
                    .rows
                    .first()
                    .and_then(|r| r.first())
                    .map(cell_text)
                    .unwrap_or_else(|| "—".into());
                let _ = write!(
                    self.out,
                    "<div class=\"board-metric\"><span class=\"metric-v\">{}</span><span class=\"metric-l\">{}</span></div>",
                    esc(&cut(&value, 24)),
                    esc(c.str_arg(0).unwrap_or(""))
                );
            }
            "Query" => {
                let rows: Vec<Vec<String>> = table
                    .rows
                    .iter()
                    .take(TABLE_ROWS)
                    .map(|r| r.iter().map(|c| cut(&cell_text(c), 120)).collect())
                    .collect();
                let shown = rows.len();
                let note = if table.rows.len() > TABLE_ROWS || table.truncated {
                    Some(format!(
                        "The first {shown} of {}{} rows.",
                        table.rows.len(),
                        if table.truncated { "+" } else { "" }
                    ))
                } else if rows.is_empty() {
                    Some("No rows.".into())
                } else {
                    None
                };
                self.table(&table.columns, &rows, c.str_arg(1), note);
            }
            _ => match chart(c.str_arg(0).unwrap_or("bar"), &table, c.str_arg(2)) {
                Ok(svg) => self.out.push_str(&svg),
                Err(message) => self.component_error(c, &message),
            },
        }
    }
}

fn cell_text(cell: &QueryCell) -> String {
    match cell {
        QueryCell::Null => "—".into(),
        QueryCell::Integer(i) => i.to_string(),
        QueryCell::Real(f) => number(*f),
        QueryCell::Text(t) => t.clone(),
    }
}
fn number(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        let text = format!("{f:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

/// A two-column result (label, value) as a small inline SVG: horizontal bars, or a line.
fn chart(kind: &str, table: &QueryTable, caption: Option<&str>) -> Result<String, String> {
    if table.columns.len() < 2 {
        return Err("a chart's query returns two columns: a label and a number".into());
    }
    let mut points = vec![];
    for (i, row) in table.rows.iter().take(CHART_POINTS).enumerate() {
        let label = row.first().map(cell_text).unwrap_or_default();
        let value = match row.get(1) {
            Some(QueryCell::Integer(n)) => *n as f64,
            Some(QueryCell::Real(f)) => *f,
            Some(QueryCell::Text(t)) => t
                .trim()
                .parse()
                .map_err(|_| format!("row {}: {t:?} is not a number", i + 1))?,
            _ => return Err(format!("row {}: the value is empty", i + 1)),
        };
        points.push((label, value));
    }
    let mut out = String::from("<figure class=\"board-chart\">");
    let described = points
        .iter()
        .take(12)
        .map(|(l, v)| format!("{l} {}", number(*v)))
        .collect::<Vec<_>>()
        .join(", ");
    let name = format!(
        "{}{}{described}",
        caption.unwrap_or(""),
        if caption.is_some() { ": " } else { "" }
    );
    if points.is_empty() {
        out.push_str("<p class=\"meta ou-note\">No rows.</p>");
    } else if kind == "line" {
        line_chart(&mut out, &points, &name);
    } else {
        bar_chart(&mut out, &points, &name);
    }
    if let Some(caption) = caption {
        let _ = write!(out, "<figcaption>{}</figcaption>", esc(caption));
    }
    if table.rows.len() > CHART_POINTS {
        let _ = write!(
            out,
            "<p class=\"meta ou-note\">The first {CHART_POINTS} of {} rows.</p>",
            table.rows.len()
        );
    }
    out.push_str("</figure>");
    Ok(out)
}
fn bar_chart(out: &mut String, points: &[(String, f64)], name: &str) {
    const W: f64 = 400.0;
    const LABEL: f64 = 120.0;
    const VALUE: f64 = 56.0;
    const ROW: f64 = 24.0;
    let height = ROW * points.len() as f64;
    let low = points.iter().map(|p| p.1).fold(0.0_f64, f64::min);
    let high = points.iter().map(|p| p.1).fold(0.0_f64, f64::max);
    let span = if high - low > 0.0 { high - low } else { 1.0 };
    let track = W - LABEL - VALUE;
    let x = |v: f64| LABEL + (v - low) / span * track;
    let _ = write!(
        out,
        "<svg class=\"chart bar-chart\" viewBox=\"0 0 {W} {height}\" width=\"100%\" role=\"img\" aria-label=\"{}\" preserveAspectRatio=\"xMinYMin meet\">",
        esc(name)
    );
    for (i, (label, value)) in points.iter().enumerate() {
        let y = i as f64 * ROW;
        let (a, b) = (x(0.0_f64.max(low)), x(*value));
        let (left, width) = (a.min(b), (a - b).abs().max(if *value != 0.0 { 1.0 } else { 0.0 }));
        let (text_y, label_x) = (y + ROW / 2.0 + 4.0, LABEL - 8.0);
        let _ = write!(
            out,
            "<text class=\"c-label\" x=\"{label_x:.1}\" y=\"{text_y:.1}\" text-anchor=\"end\"><title>{full}</title>{short}</text><rect class=\"c-bar\" x=\"{left:.1}\" y=\"{bar_y:.1}\" width=\"{width:.1}\" height=\"{bar_h:.1}\" rx=\"3\"></rect><text class=\"c-value\" x=\"{value_x:.1}\" y=\"{text_y:.1}\">{value}</text>",
            full = esc(label),
            short = esc(&cut(label, 17)),
            bar_y = y + 5.0,
            bar_h = ROW - 10.0,
            value_x = left + width + 6.0,
            value = esc(&number(*value))
        );
    }
    out.push_str("</svg>");
}
fn line_chart(out: &mut String, points: &[(String, f64)], name: &str) {
    const W: f64 = 400.0;
    const H: f64 = 150.0;
    const LEFT: f64 = 44.0;
    const RIGHT: f64 = 10.0;
    const TOP: f64 = 10.0;
    const BOTTOM: f64 = 24.0;
    let low = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let high = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let (low, high) = if high > low {
        (low.min(0.0), high)
    } else {
        (low.min(0.0), low.max(0.0) + 1.0)
    };
    let plot_w = W - LEFT - RIGHT;
    let plot_h = H - TOP - BOTTOM;
    let step = if points.len() > 1 {
        plot_w / (points.len() - 1) as f64
    } else {
        0.0
    };
    let x = |i: usize| LEFT + if points.len() > 1 { step * i as f64 } else { plot_w / 2.0 };
    let y = |v: f64| TOP + (high - v) / (high - low) * plot_h;
    let _ = write!(
        out,
        "<svg class=\"chart line-chart\" viewBox=\"0 0 {W} {H}\" width=\"100%\" role=\"img\" aria-label=\"{}\">",
        esc(name)
    );
    for v in [high, low] {
        let (line_x, line_y, label_x) = (W - RIGHT, y(v), LEFT - 6.0);
        let _ = write!(
            out,
            "<line class=\"c-grid\" x1=\"{LEFT}\" x2=\"{line_x:.1}\" y1=\"{line_y:.1}\" y2=\"{line_y:.1}\"></line><text class=\"c-value\" x=\"{label_x:.1}\" y=\"{text_y:.1}\" text-anchor=\"end\">{text}</text>",
            text_y = line_y + 4.0,
            text = esc(&number(v))
        );
    }
    let path = points
        .iter()
        .enumerate()
        .map(|(i, (_, v))| format!("{:.1},{:.1}", x(i), y(*v)))
        .collect::<Vec<_>>()
        .join(" ");
    let _ = write!(
        out,
        "<polyline class=\"c-line\" points=\"{path}\" fill=\"none\"></polyline>"
    );
    for (i, (label, v)) in points.iter().enumerate() {
        let _ = write!(
            out,
            "<circle class=\"c-dot\" cx=\"{:.1}\" cy=\"{:.1}\" r=\"2.5\"><title>{}: {}</title></circle>",
            x(i),
            y(*v),
            esc(label),
            esc(&number(*v))
        );
    }
    let first = &points[0].0;
    let _ = write!(
        out,
        "<text class=\"c-label\" x=\"{LEFT}\" y=\"{:.1}\">{}</text>",
        H - 6.0,
        esc(&cut(first, 24))
    );
    if points.len() > 1 {
        let last = &points[points.len() - 1].0;
        let _ = write!(
            out,
            "<text class=\"c-label\" x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
            W - RIGHT,
            H - 6.0,
            esc(&cut(last, 24))
        );
    }
    out.push_str("</svg>");
}

// ---- actions -------------------------------------------------------------------------------

/// The fields a button sends: its Form's, or (outside every Form) those outside every Form,
/// each with its index in drawing order (the `field-<n>` the page posts).
fn fields_of<'a>(board: &'a Board, form: Option<&str>) -> Vec<(usize, &'a Component)> {
    let mut out = vec![];
    let mut index = 0;
    board.walk(&mut |c, in_form| {
        if matches!(
            c.name.as_str(),
            "Input" | "Textarea" | "Select" | "Radio" | "Checkbox"
        ) {
            if in_form == form {
                out.push((index, c));
            }
            index += 1;
        }
    });
    out
}

#[derive(Debug)]
pub struct Action {
    pub label: String,
    pub data: Value,
}
/// The say a press of `button` on board `rev` sends: refused (conflict) when the board is not
/// at `rev` any more, or (invalid) when a primary button's fields break their rules.
pub fn action(
    program: Option<&str>,
    current_rev: u64,
    rev: u64,
    button: usize,
    posted: &BTreeMap<String, String>,
) -> Result<Action, PublicError> {
    if rev != current_rev {
        return Err(PublicError::Conflict {
            message: format!(
                "The board changed since this page was drawn (rev {rev}, now {current_rev}). Reload to act on the current board."
            ),
            current_rev: Some(sluice_model::ids::Revision(current_rev)),
        });
    }
    let board = program
        .map(openui::check_board)
        .ok_or_else(|| PublicError::Conflict {
            message: "This project has no board now. Reload the page.".into(),
            current_rev: Some(sluice_model::ids::Revision(current_rev)),
        })?
        .map_err(|problems| PublicError::Invalid {
            message: "the board does not check".into(),
            errors: problems.iter().map(ToString::to_string).collect(),
        })?;
    let buttons = board.buttons();
    let (component, form) = buttons.get(button).ok_or_else(|| PublicError::BadRequest {
        message: format!("the board has no button {button}"),
    })?;
    let mut values = serde_json::Map::new();
    let mut errors = vec![];
    let primary = component.str_arg(3) != Some("secondary");
    for (index, field) in fields_of(&board, *form) {
        let raw = posted.get(&format!("field-{index}"));
        let name = field.str_arg(0).unwrap_or("").to_owned();
        let value = match field.name.as_str() {
            "Checkbox" => json!(raw.is_some_and(|v| !v.is_empty())),
            "Input" if field.str_arg(3) == Some("number") => {
                let text = raw.cloned().unwrap_or_default();
                match text.trim().parse::<f64>() {
                    Ok(n) if !text.trim().is_empty() => serde_json::Number::from_f64(n)
                        .map(|n| {
                            if n.as_f64().is_some_and(|f| f.fract() == 0.0 && f.abs() < 9e15) {
                                json!(n.as_f64().unwrap_or(0.0) as i64)
                            } else {
                                Value::Number(n)
                            }
                        })
                        .unwrap_or(json!(text)),
                    _ => json!(text),
                }
            }
            _ => json!(raw.cloned().unwrap_or_default()),
        };
        if primary {
            let rules_at = match field.name.as_str() {
                "Input" => 5,
                "Textarea" | "Select" | "Radio" => 4,
                _ => usize::MAX,
            };
            for rule in field.strings_arg(rules_at) {
                if let Some(message) = openui::rule_fails(&rule, &value) {
                    errors.push(json!({"field": format!("field-{index}"), "name": name, "message": message}));
                    break;
                }
            }
        }
        values.insert(name, value);
    }
    if !errors.is_empty() {
        return Err(PublicError::Invalid {
            message: "Check the highlighted fields.".into(),
            errors: errors.iter().map(|e| e.to_string()).collect(),
        });
    }
    let params = component
        .arg(2)
        .map(Ui::to_json)
        .unwrap_or_else(|| json!({}));
    let action = component.str_arg(1).unwrap_or("submit").to_owned();
    Ok(Action {
        label: component.str_arg(0).unwrap_or("").to_owned(),
        data: json!({"board_rev": rev, "action": action, "params": params, "values": values}),
    })
}

fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"))
}
fn error_response(error: PublicError) -> Response {
    let status = match &error {
        PublicError::Conflict { .. } => StatusCode::CONFLICT,
        PublicError::Invalid { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        PublicError::BadRequest { .. } => StatusCode::BAD_REQUEST,
        PublicError::NotFound { .. } => StatusCode::NOT_FOUND,
        PublicError::Busy { .. } => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, axum::Json(error)).into_response()
}

/// `POST /projects/id/<p>/board/action`: a board Button, as a form post (`board_rev`,
/// `button`, `field-<n>`). It sends `say(to: orchestrator)` from the owner with data
/// `{board_rev, action, params, values}`; the board never edits the plan itself.
async fn post_action(
    State(state): State<super::inbox::MessageState>,
    Path(project): Path<ProjectId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut posted = BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(&body) {
        posted.insert(key.into_owned(), value.into_owned());
    }
    let result = async {
        let number = |key: &str| {
            posted
                .get(key)
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| PublicError::BadRequest {
                    message: format!("{key} is required"),
                })
        };
        let (rev, button) = (number("board_rev")?, number("button")? as usize);
        let (program, current): (Option<String>, i64) = state
            .dashboard
            .reads
            .snapshot(move |c| {
                c.query_row(
                    "SELECT board,board_rev FROM projects WHERE project_id=?1 AND deleted_at IS NULL",
                    [project.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(Into::into)
            })
            .await
            .map_err(|e| e.into_public(true))?;
        let action = action(program.as_deref(), current as u64, rev, button, &posted)?;
        state
            .commands
            .command(CommandRequest::Say(Say {
                project: ProjectSelector::Id(project),
                to: "orchestrator".into(),
                body: format!("Board: {}", action.label),
                data: Some(sluice_model::rpc::JsonValue::try_from(action.data).map_err(
                    |e| PublicError::BadRequest {
                        message: e.to_string(),
                    },
                )?),
                run: None,
                owner: true,
            }))
            .await
    }
    .await;
    match result {
        Ok(_) if wants_json(&headers) => {
            axum::Json(json!({"ok": true, "message": "Sent to the orchestrator."})).into_response()
        }
        Ok(_) => Redirect::to(&format!("/projects/id/{project}")).into_response(),
        Err(error) => error_response(error),
    }
}

pub fn registration() -> super::PageRegistration {
    use super::{Asset, PageRegistration};
    PageRegistration {
        routes: |state| match state.messages.clone() {
            Some(messages) => axum::Router::new()
                .route(
                    "/projects/id/{project}/board/action",
                    axum::routing::post(post_action),
                )
                .with_state(messages),
            None => axum::Router::new(),
        },
        nav: |_| vec![],
        assets: &[Asset {
            names: &["board.js"],
            media_type: "text/javascript",
            bytes: include_bytes!("../../assets/board.js"),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_count_as_sqlite_numbers_them() {
        assert_eq!(placeholders("SELECT 1").unwrap(), 0);
        assert_eq!(placeholders("SELECT ? , ?").unwrap(), 2);
        assert_eq!(placeholders("SELECT ?1, ?1, '?'").unwrap(), 1);
        assert_eq!(placeholders("SELECT ?2 -- ?\n").unwrap(), 2);
        assert!(placeholders("SELECT :name").is_err());
        assert_eq!(placeholders("SELECT json_extract(x, '$.a')").unwrap(), 0);
    }


}
