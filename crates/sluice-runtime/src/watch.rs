//! One durable wake filter shared by next and watch. No plan reconstruction at read time.
use crate::calls::{parse_id, public};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use sluice_model::{
    commands::{MessageVerb, Next, NextResult, Scope, Settles, StepStatus},
    error::PublicError,
    events::{Event, Record, UnitStep},
    ids::{ProjectId, ProjectSelector, RecordSeq, Revision, WorkGeneration},
    plan::Plan,
    rpc::JsonMap,
    units::settled_steps,
};
use sluice_store::{
    ChangeKey, ReadPool, RetrySafety, WriteTransaction, Writer, identities, messages, plans,
    records::{self, RecordFilter, ScopeFilter},
};
use std::{path::Path, time::Duration};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// Called by reconciliation after status, pause and plan edits, in their transaction, with
/// the plan compiled at `rev` (the stored revision, or the call is refused as stale).
/// A unit settles (§6.7) each time it comes to rest unlike its last settlement: with another
/// stamp (each member's id, generation and work generation) or other member statuses. The
/// certificate (`settlements` in the maintenance settings: `signature` the stamp, `statuses`,
/// `work`) survives feed trimming; outputs and membership freeze here. Every step's stamp is
/// read at once, a unit whose stamp and statuses its last settlement already recorded is
/// skipped before its state is evaluated, and the plan's settled steps (§7.1) are worked out
/// once, at the first unit that needs them.
///
/// A certificate written before statuses were recorded (no `statuses`) whose stamp still
/// matches is upgraded without a record: it takes the current statuses if the unit is at rest,
/// or none (`[]`, so its next rest settles) if not. `signature` keeps its meaning, so a release
/// from before statuses reads the certificates as it wrote them.
pub fn record_settlements(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    rev: Revision,
    plan: &Plan,
) -> sluice_store::Result<Vec<Record>> {
    let stored: i64 = tx.sql().query_row(
        "SELECT rev FROM plans WHERE project_id=?1",
        [project.to_string()],
        |r| r.get(0),
    )?;
    if u64::try_from(stored).ok() != Some(rev.0) {
        return Err(PublicError::Conflict {
            message: "settlement plan is stale".into(),
            current_rev: None,
        }
        .into());
    }
    let state = plans::read_state(tx.sql(), project)?;
    let raw: String = tx.sql().query_row(
        "SELECT settings FROM maintenance WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    let mut settings: Value = serde_json::from_str(&raw)?;
    let generations: std::collections::HashMap<String, (i64, i64)> = tx
        .sql()
        .prepare("SELECT step_id,generation,work_generation FROM steps WHERE project_id=?1")?
        .query_map([project.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?)))
        })?
        .collect::<Result<_, _>>()?;
    let mut held = None;
    let mut emitted = vec![];
    let mut upgraded = false;
    for unit in plan.units().values() {
        let mut stamp = vec![];
        let mut work = 1;
        for id in &unit.steps {
            let (generation, w) =
                *generations
                    .get(id.as_str())
                    .ok_or_else(|| PublicError::Conflict {
                        message: format!("settlement step {id} has no row"),
                        current_rev: None,
                    })?;
            stamp.push(json!([id, generation, w]));
            work = work.max(w);
        }
        let key = format!("{project}/{}", unit.name);
        let stamp = Value::Array(stamp);
        let statuses: Value = unit
            .steps
            .iter()
            .map(|id| json!(state.status(id)))
            .collect();
        let previous = &settings["settlements"][&key];
        let same = previous["signature"] == stamp;
        if same && previous["statuses"] == statuses {
            continue;
        }
        let held = held.get_or_insert_with(|| settled_steps(plan, &state));
        let settled = unit.settled_in(held);
        if same && previous.get("statuses").is_none() {
            settings["settlements"][&key]["statuses"] = if settled { statuses } else { json!([]) };
            upgraded = true;
            continue;
        }
        if !settled {
            continue;
        }
        if let Some(previous_work) = previous["work"].as_i64() {
            work = work.max(
                previous_work
                    .checked_add(1)
                    .ok_or_else(|| PublicError::Storage {
                        message: "unit work generation exhausted".into(),
                    })?,
            );
        }
        let steps = unit
            .steps
            .iter()
            .map(|id| {
                let step = &plan.steps()[id];
                let status = state.status(id);
                let held = status == StepStatus::Pending && held.contains(id);
                let names: Vec<_> = if step.declared_outputs.is_empty() {
                    step.signature.outputs.keys().collect()
                } else {
                    step.declared_outputs.keys().collect()
                };
                let mut outputs = JsonMap::default();
                if status == StepStatus::Succeeded
                    && let Some(s) = state.steps.get(id)
                {
                    for name in names {
                        if let Some(v) = s.outputs.0.get(name)
                            && !empty(v.as_value())
                        {
                            outputs.0.insert(name.clone(), v.clone());
                        }
                    }
                }
                UnitStep {
                    id: id.clone(),
                    status,
                    held,
                    outputs: Some(outputs),
                    omitted: vec![],
                }
            })
            .collect();
        emitted.push(tx.append_record(
            Some(project),
            Event::UnitSettled {
                unit: unit.name.clone(),
                work: WorkGeneration(work as u64),
                steps,
            },
        )?);
        if !settings["settlements"].is_object() {
            settings["settlements"] = json!({});
        }
        settings["settlements"][key] = json!({"signature":stamp,"statuses":statuses,"work":work});
    }
    if !emitted.is_empty() || upgraded {
        tx.sql().execute(
            "UPDATE maintenance SET settings=?1 WHERE singleton=1",
            [settings.to_string()],
        )?;
    }
    if !emitted.is_empty() {
        tx.changed(Some(project), "status");
    } else if upgraded {
        tx.changed(None, "maintenance");
    }
    Ok(emitted)
}
fn empty(v: &Value) -> bool {
    v.is_null()
        || v.as_str() == Some("")
        || v.as_array().is_some_and(Vec::is_empty)
        || v.as_object().is_some_and(serde_json::Map::is_empty)
}

/// Monotonic clock for the wake windows. Real time in production; tests drive a
/// ManualClock so deadlines never depend on the host's wall clock.
#[derive(Debug, Clone)]
pub enum Clock {
    Real(tokio::time::Instant),
    Manual(ManualClock),
}
impl Clock {
    pub(crate) fn now(&self) -> Duration {
        match self {
            Clock::Real(start) => start.elapsed(),
            Clock::Manual(clock) => clock.state.borrow().now,
        }
    }
    pub(crate) async fn sleep_until(&self, at: Duration) {
        match self {
            Clock::Real(start) => tokio::time::sleep_until(*start + at).await,
            Clock::Manual(clock) => clock.sleep_until(at).await,
        }
    }
}
impl Default for Clock {
    fn default() -> Self {
        Clock::Real(tokio::time::Instant::now())
    }
}
/// Manually advanced clock for tests. `advance` wakes every pending sleep whose
/// deadline the new time has reached. `parked` tells a test when a sleeper is
/// waiting on the clock, so it advances time only once the sleeper is there.
#[derive(Debug, Clone)]
pub struct ManualClock {
    state: tokio::sync::watch::Sender<ManualState>,
}
#[derive(Debug, Default)]
struct ManualState {
    now: Duration,
    sleeps: u64,
    /// Sleeps that have seen the current time and wait for a later one. An
    /// advance empties it: each sleeper must observe the new time to rejoin.
    parked: std::collections::BTreeSet<u64>,
}
impl ManualClock {
    pub fn new() -> Self {
        Self {
            state: tokio::sync::watch::channel(ManualState::default()).0,
        }
    }
    pub fn advance(&self, by: Duration) {
        self.state.send_modify(|state| {
            state.now += by;
            state.parked.clear();
        });
    }
    /// Returns once some sleep has observed the current time and is waiting
    /// for a later one, i.e. only advancing the clock can make it progress.
    pub async fn parked(&self) {
        let mut receiver = self.state.subscribe();
        receiver
            .wait_for(|state| !state.parked.is_empty())
            .await
            .ok();
    }
    async fn sleep_until(&self, at: Duration) {
        struct Unpark<'a>(&'a tokio::sync::watch::Sender<ManualState>, u64);
        impl Drop for Unpark<'_> {
            fn drop(&mut self) {
                self.0
                    .send_if_modified(|state| state.parked.remove(&self.1));
            }
        }
        let mut receiver = self.state.subscribe();
        let mut id = 0;
        self.state.send_if_modified(|state| {
            state.sleeps += 1;
            id = state.sleeps;
            false
        });
        let _unpark = Unpark(&self.state, id);
        loop {
            // Check and park under one lock, so an advance is either seen here
            // or clears the entry and wakes the receiver below.
            let mut reached = false;
            self.state.send_if_modified(|state| {
                reached = state.now >= at;
                !reached && state.parked.insert(id)
            });
            if reached || receiver.changed().await.is_err() {
                return;
            }
        }
    }
}
impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct NextOptions {
    pub include_routine: bool,
    pub projects: Vec<ProjectId>,
    pub since_seq: Option<RecordSeq>,
    /// The waiting session, `@name`, or none (an unnamed reader, kept as
    /// `messages::ANONYMOUS_READER`).
    pub me: Option<String>,
    /// Whose records: `None` is `mine` for a named session, `all` for an unnamed one.
    pub scope: Option<Scope>,
    /// Only these units' records (patterns) / those of units carrying one of these tags.
    pub units: Vec<String>,
    pub tags: Vec<String>,
    pub timeout: Option<Duration>,
    pub all: bool,
    pub settle: Duration,
    pub settle_max: Duration,
    pub unread_alert_min: Option<i64>,
    pub kinds: Vec<String>,
    pub threads: Vec<String>,
    pub clock: Clock,
}
impl Default for NextOptions {
    fn default() -> Self {
        Self {
            include_routine: false,
            projects: vec![],
            since_seq: None,
            me: None,
            scope: None,
            units: vec![],
            tags: vec![],
            timeout: None,
            all: false,
            settle: Duration::from_secs(20),
            settle_max: Duration::from_secs(120),
            unread_alert_min: None,
            kinds: vec![],
            threads: vec![],
            clock: Clock::default(),
        }
    }
}
impl NextOptions {
    /// The scope it reads: as given, else `mine` for a named session and `all` for an unnamed
    /// one. `mine` without a name is refused.
    pub fn scope(&self) -> Result<Scope, PublicError> {
        match (self.scope, &self.me) {
            (Some(Scope::Mine), None) => Err(sluice_model::identity::refusal(
                "scope",
                "scope mine is a named session's: pass me=@name (or SLUICE_ME), or scope all",
            )),
            (Some(scope), _) => Ok(scope),
            (None, Some(_)) => Ok(Scope::Mine),
            (None, None) => Ok(Scope::All),
        }
    }
    /// The record filter for its scope (`next`'s, and `log_wait`'s).
    pub fn scope_filter(&self) -> Result<ScopeFilter, PublicError> {
        Ok(ScopeFilter {
            me: self.me.clone(),
            mine: self.scope()? == Scope::Mine,
            units: self.units.clone(),
            tags: self.tags.clone(),
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    Wake,
    Note,
    Ignore,
}
/// Parent questions live independently of the bounded record feed.
async fn batch(
    reads: &ReadPool,
    options: &NextOptions,
    cursor: RecordSeq,
) -> Result<(Vec<(Record, Classification)>, RecordSeq), PublicError> {
    let options = options.clone();
    let scope = options.scope_filter()?;
    let mine = scope.mine;
    reads
        .snapshot(move |sql| {
            let mut gathered = vec![];
            let mut top = cursor;
            let mut fence = None::<i64>;
            for project in &options.projects {
                let page = records::read_records(
                    sql,
                    Some(*project),
                    &RecordFilter {
                        since: Some(cursor),
                        limit: 200,
                        kinds: options.kinds.clone(),
                        threads: options.threads.clone(),
                        scope: Some(scope.clone()),
                        ..RecordFilter::default()
                    },
                )?
                .into_page()?;
                top = RecordSeq(top.0.max(page.last_seq.0));
                if page.records.len() == 200 {
                    fence = Some(fence.map_or(page.last_seq.0, |s| s.min(page.last_seq.0)));
                }
                gathered.extend(page.records);
            }
            if let Some(fence) = fence {
                gathered.retain(|r| r.seq.0 <= fence);
                top = RecordSeq(fence);
            }
            gathered.sort_by_key(|r| r.seq.0);
            let mut classified = vec![];
            for record in gathered {
                let classification = if !options.include_routine
                    && crate::routine::routine(sql, &record, options.me.as_deref())?
                {
                    Classification::Ignore
                } else if options.all {
                    Classification::Wake
                } else {
                    match &record.event {
                        Event::UnitSettled { .. } => Classification::Wake,
                        Event::StepStatus {
                            to: StepStatus::Failed | StepStatus::Stale | StepStatus::Skipped,
                            ..
                        } => Classification::Wake,
                        Event::Message(msg) => {
                            // Own messages never wake; in scope all a question to any session
                            // does, in scope mine only one to the reader.
                            let wake = options.me.as_deref() != Some(msg.from.as_str())
                                && messages::message_wakes(
                                    sql,
                                    record.project.ok_or_else(|| PublicError::Invalid {
                                        message: "message record without project".into(),
                                        errors: vec![],
                                    })?,
                                    msg,
                                    options.me.as_deref().filter(|_| mine),
                                )?;
                            if wake
                                || (msg.verb == MessageVerb::Say
                                    && options
                                        .me
                                        .as_deref()
                                        .is_some_and(|me| msg.to.as_deref() == Some(me)))
                            {
                                Classification::Wake
                            } else if !msg.is_question()
                                && options.me.as_deref() != Some(msg.from.as_str())
                            {
                                Classification::Note
                            } else {
                                Classification::Ignore
                            }
                        }
                        Event::ProjectPause { author, .. }
                        | Event::ProjectArchive { author, .. }
                            if options.me.as_deref() != Some(author.as_str()) =>
                        {
                            Classification::Wake
                        }
                        // A named session hears who answers the unowned units now.
                        Event::ProjectUpdate { fields, author, .. }
                            if options.me.as_deref().is_some_and(|me| me != author)
                                && fields.iter().any(|f| f == "fallback") =>
                        {
                            Classification::Wake
                        }
                        _ => Classification::Ignore,
                    }
                };
                classified.push((record, classification));
            }
            Ok((classified, top))
        })
        .await
        .map_err(public)
}
async fn projects(
    reads: &ReadPool,
    selected: Vec<ProjectId>,
) -> Result<Vec<ProjectId>, PublicError> {
    reads.snapshot(move|sql|{
        if !selected.is_empty(){let mut result=vec![];for p in selected{messages::resolve_project(sql,&ProjectSelector::Id(p))?;if !result.contains(&p){result.push(p);}}return Ok(result);}
        let mut stmt=sql.prepare("SELECT project_id FROM projects WHERE deleted_at IS NULL AND archived=0 ORDER BY name")?;
        let rows=stmt.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        rows.into_iter().map(parse_id).collect()
    }).await.map_err(public)
}
pub async fn next_command(
    writer: &Writer,
    reads: &ReadPool,
    request: Next,
) -> Result<NextResult, PublicError> {
    if let Some(name) = request.subscription {
        if !request.projects.is_empty()
            || request.since_seq.is_some()
            || request.scope.is_some()
            || request.units.is_some()
            || request.tags.is_some()
            || request.all
        {
            return Err(PublicError::BadRequest {
                message: "subscription owns its projects, filter and cursor".into(),
            });
        }
        return crate::subscriptions::next(
            writer,
            reads,
            name,
            Duration::from_secs(request.timeout_seconds.min(3600)),
            Clock::default(),
            request.include_routine,
        )
        .await;
    }
    let selectors = request.projects;
    let me = request
        .me
        .as_deref()
        .map(str::trim)
        .filter(|me| !me.is_empty())
        .map(sluice_model::identity::SessionName::parse)
        .transpose()?
        .map(|me| me.to_string());
    let selected = reads
        .snapshot(move |sql| {
            selectors
                .iter()
                .map(|s| messages::resolve_project(sql, s))
                .collect()
        })
        .await
        .map_err(public)?;
    next(
        writer,
        reads,
        NextOptions {
            include_routine: request.include_routine.unwrap_or(false),
            projects: selected,
            since_seq: request.since_seq,
            me,
            scope: request.scope,
            units: request.units.unwrap_or_default(),
            tags: request.tags.unwrap_or_default(),
            timeout: Some(Duration::from_secs(request.timeout_seconds)),
            all: request.all,
            settle: Duration::from_secs(request.settle_seconds),
            settle_max: Duration::from_secs(request.settle_max_seconds),
            unread_alert_min: None,
            kinds: vec![],
            threads: vec![],
            clock: Clock::default(),
        },
    )
    .await
}
/// Timeout bounds waiting for the first wake. Subsequent wakes extend only settle,
/// and settle_max remains an absolute deadline measured from the first wake.
pub async fn next(
    writer: &Writer,
    reads: &ReadPool,
    mut options: NextOptions,
) -> Result<NextResult, PublicError> {
    if options.me.as_deref().is_some_and(|me| me.trim().is_empty())
        || options.since_seq.is_some_and(|s| s.0 < 0)
        || options.unread_alert_min.is_some_and(|n| n < 0)
    {
        return Err(PublicError::BadRequest {
            message: "reader must be nonblank and cursor/alert threshold nonnegative".into(),
        });
    }
    options.scope()?;
    options.projects = projects(reads, options.projects).await?;
    // Without a since: a named session resumes where its last batch ended (the earliest of
    // its positions in the selected projects); an unnamed one, or a session's first wait,
    // starts at the top of the logs.
    let mut cursor = if let Some(seq) = options.since_seq {
        seq
    } else {
        let selected = options.projects.clone();
        let me = options.me.clone();
        reads
            .snapshot(move |sql| {
                let mut top = RecordSeq(0);
                let mut resumed: Option<RecordSeq> = None;
                for p in selected {
                    top = RecordSeq(top.0.max(records::bounds(sql, Some(p))?.1.0));
                    // A position the log no longer reaches back to is dropped, not refused.
                    if let Some(me) = &me
                        && let Some(at) = identities::resume(sql, p, me)?
                        && matches!(
                            records::read_records(
                                sql,
                                Some(p),
                                &RecordFilter {
                                    since: Some(at),
                                    limit: 1,
                                    ..RecordFilter::default()
                                },
                            )?,
                            records::RecordRead::Page(_)
                        )
                    {
                        resumed = Some(resumed.map_or(at, |r| RecordSeq(r.0.min(at.0))));
                    }
                }
                Ok(resumed.unwrap_or(top))
            })
            .await
            .map_err(public)?
    };
    // Cursor first; subscribe and recheck before waiting. Record reads below always
    // follow subscription, including commits between the initial snapshot and subscribe.
    let keys = options
        .projects
        .iter()
        .map(|p| ChangeKey::new(Some(*p), "log"))
        .collect();
    let mut subscription = reads.subscribe(writer, keys).await.map_err(public)?;
    let start = options.clock.now();
    let timeout = options.timeout.map(|t| start + t);
    let mut first = None;
    let mut last = None;
    let mut heartbeat = start;
    let mut result = NextResult {
        suppressed: None,
        records: vec![],
        notes: vec![],
        last_seq: cursor,
        timed_out: false,
    };
    loop {
        if options.clock.now() >= heartbeat {
            beat(writer, &options, cursor).await?;
            heartbeat = options.clock.now() + Duration::from_secs(30);
        }
        // Only expire against a snapshot begun at or after the deadline. Time
        // spent reading must not hide a wake committed while that read was pending.
        let now = options.clock.now();
        let (batch, top) = batch(reads, &options, cursor).await?;
        let mut consumed = 0;
        for (record, classification) in batch {
            consumed += 1;
            cursor = record.seq;
            match classification {
                Classification::Note => result.notes.push(record),
                Classification::Wake => {
                    result.records.push(record);
                    let now = options.clock.now();
                    first.get_or_insert(now);
                    last = Some(now);
                    if options.settle.is_zero() {
                        result.last_seq = cursor;
                        beat(writer, &options, cursor).await?;
                        return Ok(result);
                    }
                }
                Classification::Ignore => {}
            }
        }
        cursor = top;
        result.last_seq = cursor;
        let finish = first
            .zip(last)
            .map(|(f, l)| (f + options.settle_max).min(l + options.settle));
        if finish.is_some_and(|d| now >= d) {
            beat(writer, &options, cursor).await?;
            return Ok(result);
        }
        if first.is_none() && timeout.is_some_and(|d| now >= d) {
            result.timed_out = true;
            beat(writer, &options, cursor).await?;
            return Ok(result);
        }
        // Drain pagination before sleeping, without granting a fresh timeout budget.
        if consumed >= 200 {
            continue;
        }
        let deadline = finish.or(timeout).map_or(heartbeat, |d| d.min(heartbeat));
        tokio::select! {
            result = subscription.wait() => {
                if let Err(e) = result {
                    return Err(public(e));
                }
            }
            _ = options.clock.sleep_until(deadline) => {}
        }
    }
}
async fn beat(
    writer: &Writer,
    options: &NextOptions,
    cursor: RecordSeq,
) -> Result<(), PublicError> {
    let projects = options.projects.clone();
    let me = options
        .me
        .clone()
        .unwrap_or_else(|| messages::ANONYMOUS_READER.into());
    let threshold = options.unread_alert_min;
    let result=writer.write(RetrySafety::Idempotent,move|tx|{
        for project in projects{
            let live:bool=tx.sql().query_row("SELECT EXISTS(SELECT 1 FROM projects WHERE project_id=?1 AND deleted_at IS NULL)",[project.to_string()],|r|r.get(0))?;
            if live{messages::orchestrator_read(tx,project,&me,cursor,threshold)?;}
        }Ok(())
    }).await;
    match result {
        Err(PublicError::Busy { .. }) => Ok(()),
        other => other,
    }
}
/// Continuous next batches. Output and flush precede the next durable read advance.
/// Sink errors terminate the stream and must be resumed with its last printed seq.
pub async fn watch<W: AsyncWrite + Unpin>(
    writer: &Writer,
    reads: &ReadPool,
    mut options: NextOptions,
    out: &mut W,
    stop: CancellationToken,
) -> Result<(), PublicError> {
    loop {
        let batch = tokio::select! {_ = stop.cancelled()=>return Ok(()),result=next(writer,reads,options.clone())=>result?};
        for record in messages_first(&batch) {
            let mut bytes = serde_json::to_vec(record).map_err(|e| PublicError::Storage {
                message: e.to_string(),
            })?;
            bytes.push(b'\n');
            out.write_all(&bytes).await.map_err(io_error)?;
        }
        out.flush().await.map_err(io_error)?;
        options.since_seq = Some(batch.last_seq);
    }
}
/// The presentation order never changes cursor consumption order or JSON values.
pub fn messages_first(result: &NextResult) -> Vec<&Record> {
    let mut records: Vec<_> = result.records.iter().chain(&result.notes).collect();
    records.sort_by_key(|r| (!matches!(r.event, Event::Message(_)), r.seq.0));
    records
}
fn io_error(e: std::io::Error) -> PublicError {
    PublicError::Storage {
        message: e.to_string(),
    }
}

/// CLI cursor files are separate from the owner's message read positions.
pub fn load_cursor(path: &Path) -> Result<Option<RecordSeq>, PublicError> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(e)),
        Ok(s) => {
            let seq = s
                .trim()
                .parse::<i64>()
                .map_err(|_| PublicError::BadRequest {
                    message: "cursor file must contain a nonnegative sequence".into(),
                })?;
            if seq < 0 {
                return Err(PublicError::BadRequest {
                    message: "cursor file must contain a nonnegative sequence".into(),
                });
            }
            Ok(Some(RecordSeq(seq)))
        }
    }
}
pub fn save_cursor(path: &Path, seq: RecordSeq) -> Result<(), PublicError> {
    use std::io::Write;
    if seq.0 < 0 {
        return Err(PublicError::BadRequest {
            message: "cursor must be nonnegative".into(),
        });
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temp = parent.join(format!(
        ".sluice-cursor-{}",
        sluice_model::ids::RunId::new()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        writeln!(file, "{}", seq.0)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        std::fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(io_error)
}

fn short(name: &str, value: &Value) -> Option<Value> {
    if value.is_number() || value.is_boolean() {
        return Some(value.clone());
    }
    let s = value.as_str()?;
    if name == "summary" {
        let first = s
            .lines()
            .find(|s| !s.trim().is_empty())
            .unwrap_or("")
            .trim();
        return Some(Value::String(cut_text(first, 200)));
    }
    (s.chars().count() <= 80 && !s.contains('\n')).then(|| value.clone())
}
fn cut_text(s: &str, cut: usize) -> String {
    let mut chars = s.chars();
    let text: String = chars.by_ref().take(cut).collect();
    if chars.next().is_some() {
        format!("{text}…")
    } else {
        text
    }
}
fn one(value: &Value, cut: usize) -> String {
    let s = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    cut_text(&s.split_whitespace().collect::<Vec<_>>().join(" "), cut)
}
/// A message body over this many characters is shown as its first paragraph and the
/// paragraphs that ask (`body_shown`).
pub const BODY_WHOLE: usize = 4000;

/// A message's body as `next` prints it: whole, unless it is over `BODY_WHOLE` characters;
/// then its first paragraph and every later paragraph that is a marked question (it starts
/// with "Question" or "Q:", or ends with "?"), each whole, and how much is left out and where
/// to read it. Paragraphs are split at blank lines, so nothing is cut mid-sentence.
pub fn body_shown(body: &str, id: i64, thread: &str) -> String {
    let body = body.trim();
    let total = body.chars().count();
    if total <= BODY_WHOLE {
        return body.to_owned();
    }
    let mut paragraphs = vec![];
    let mut current: Vec<&str> = vec![];
    for line in body.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                paragraphs.push(current.join("\n"));
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join("\n"));
    }
    let asks = |paragraph: &str| {
        let lead = paragraph
            .trim_start_matches(|c: char| c.is_whitespace() || "#*->_`".contains(c))
            .to_lowercase();
        lead.starts_with("question")
            || lead.starts_with("q:")
            || paragraph.trim_end().ends_with('?')
    };
    let mut kept: Vec<&str> = vec![];
    for (i, paragraph) in paragraphs.iter().enumerate() {
        if i == 0 || asks(paragraph) {
            kept.push(paragraph);
        }
    }
    let shown = kept.join("\n\n");
    let left = total.saturating_sub(shown.chars().count());
    format!(
        "{shown}\n\n[{left} more characters of #{id} not shown: messages(view=\"thread\", thread=\"{thread}\") reads it whole]"
    )
}

/// The steps whose failure a settlement shows was a cancel, by the settlement's seq: what the
/// `UNIT ... settled` line needs beyond the record, read from the step's last `step.status`
/// record before the settlement (else its stored error while it is still failed).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cancels(std::collections::BTreeSet<(i64, String)>);
impl Cancels {
    pub fn read(sql: &rusqlite::Connection, result: &NextResult) -> sluice_store::Result<Self> {
        let mut found = std::collections::BTreeSet::new();
        for record in result.records.iter().chain(&result.notes) {
            let (Event::UnitSettled { steps, .. }, Some(project)) = (&record.event, record.project)
            else {
                continue;
            };
            for step in steps.iter().filter(|s| s.status == StepStatus::Failed) {
                let last: Option<String> = sql
                    .query_row(
                        "SELECT payload FROM records WHERE project_id=?1 AND kind='step.status' AND step_id=?2 AND seq<?3 ORDER BY seq DESC LIMIT 1",
                        rusqlite::params![project.to_string(), step.id.as_str(), record.seq.0],
                        |r| r.get(0),
                    )
                    .optional()?;
                let cancel = match last.map(|p| serde_json::from_str::<Event>(&p)).transpose()? {
                    Some(Event::StepStatus {
                        to: StepStatus::Failed,
                        error,
                        ..
                    }) => error.as_ref().is_some_and(sluice_model::shown::is_cancel),
                    _ => sql
                        .query_row(
                            "SELECT error FROM steps WHERE project_id=?1 AND step_id=?2 AND status='failed'",
                            rusqlite::params![project.to_string(), step.id.as_str()],
                            |r| r.get::<_, Option<String>>(0),
                        )
                        .optional()?
                        .flatten()
                        .is_some_and(|e| sluice_model::shown::stored_is_cancel(&e)),
                };
                if cancel {
                    found.insert((record.seq.0, step.id.to_string()));
                }
            }
        }
        Ok(Self(found))
    }
    pub fn contains(&self, seq: RecordSeq, step: &str) -> bool {
        self.0.contains(&(seq.0, step.to_owned()))
    }
}

/// How a settled unit ended, from its steps' statuses (a cancelled step is a failed one whose
/// failure was a cancel): `succeeded` when none failed, was cancelled or turned stale (or
/// `skipped`/`held` when none even succeeded); the one way it went wrong (`failed`, `2
/// failed`, `cancelled`, `stale`); or each count when they are mixed (`1 failed, 1
/// cancelled`).
pub fn outcome(steps: &[UnitStep], cancelled: impl Fn(&UnitStep) -> bool) -> String {
    let count = |want: &dyn Fn(&UnitStep) -> bool| steps.iter().filter(|s| want(s)).count();
    let bad = [
        (
            "failed",
            count(&|s| s.status == StepStatus::Failed && !cancelled(s)),
        ),
        (
            "cancelled",
            count(&|s| s.status == StepStatus::Failed && cancelled(s)),
        ),
        ("stale", count(&|s| s.status == StepStatus::Stale)),
    ];
    let present: Vec<(&str, usize)> = bad.into_iter().filter(|(_, n)| *n > 0).collect();
    match present.as_slice() {
        [] if count(&|s| s.status == StepStatus::Succeeded) > 0 => "succeeded".into(),
        [] if count(&|s| s.status == StepStatus::Skipped) > 0 => "skipped".into(),
        [] => "held".into(),
        [(word, 1)] => (*word).to_owned(),
        [(word, n)] => format!("{n} {word}"),
        mixed => mixed
            .iter()
            .map(|(word, n)| format!("{n} {word}"))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

pub fn line(
    record: &Record,
    settles: Settles,
    cut: usize,
    cancels: &Cancels,
) -> Result<String, PublicError> {
    if cut == 0 {
        return Err(PublicError::BadRequest {
            message: "cut must be positive".into(),
        });
    }
    Ok(match &record.event {
        Event::Message(m) => {
            // The `#id` to reply to sits on the verb's line; a question's title leads it.
            let body = body_shown(&m.body, m.id.0, &m.thread);
            let text = match m.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                Some(title) if body.is_empty() => title.to_owned(),
                Some(title) => format!("{title}\n{body}"),
                None => body,
            };
            format!(
                "{} #{} {} {} -> {}: {}",
                match m.verb {
                    MessageVerb::Ask => "ASK",
                    MessageVerb::Say => "SAY",
                    MessageVerb::Reply => "REPLY",
                },
                m.id.0,
                m.thread,
                m.from,
                m.to.as_deref().unwrap_or("-"),
                text.replace('\n', "\n  ")
            )
        }
        Event::StepStatus {
            step,
            from,
            to,
            error,
            ..
        } => {
            let class = error.as_ref().map(|e| {
                serde_json::to_value(e.failure_class())
                    .expect("class")
                    .as_str()
                    .unwrap_or("harness")
                    .to_owned()
            });
            let tail = error
                .as_ref()
                .and_then(|e| {
                    e.to_string()
                        .lines()
                        .last()
                        .map(|s| format!(": {}", cut_text(s, 200)))
                })
                .unwrap_or_default();
            format!(
                "STEP {step} {} -> {}{}{tail}",
                from.as_ref().unwrap_or(&StepStatus::Pending).as_str(),
                to.as_str(),
                class.map(|c| format!(" [{c}]")).unwrap_or_default()
            )
        }
        Event::UnitSettled { unit, steps, .. } => {
            let pre = format!("{unit}-");
            let name = |id: &sluice_model::ids::StepId| {
                id.as_str()
                    .strip_prefix(&pre)
                    .unwrap_or(id.as_str())
                    .to_owned()
            };
            let marks = steps
                .iter()
                .map(|s| {
                    let status = if s.status == StepStatus::Failed
                        && cancels.contains(record.seq, s.id.as_str())
                    {
                        "cancelled"
                    } else {
                        s.status.as_str()
                    };
                    format!(
                        "{} {status}{}",
                        name(&s.id),
                        if s.held { " (held)" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ");
            let marks = if marks.is_empty() {
                "no steps".to_owned()
            } else {
                marks
            };
            let ended = outcome(steps, |s| cancels.contains(record.seq, s.id.as_str()));
            let mut lines = vec![format!("UNIT {unit} settled · {ended}: {marks}")];
            let mut omitted = vec![];
            if settles != Settles::None {
                for step in steps {
                    if let Some(outputs) = &step.outputs {
                        for (key, value) in &outputs.0 {
                            let value = if settles == Settles::Short {
                                short(key, value.as_value())
                            } else {
                                Some(value.as_value().clone())
                            };
                            if let Some(value) = value {
                                lines.push(format!(
                                    "  {}.{key}: {}",
                                    name(&step.id),
                                    one(&value, cut)
                                ));
                            } else {
                                omitted.push(format!("{}.{key}", name(&step.id)));
                            }
                        }
                    }
                    omitted.extend(
                        step.omitted
                            .iter()
                            .map(|k| format!("{}.{k}", name(&step.id))),
                    );
                }
            }
            if !omitted.is_empty() {
                lines.push(format!(
                    "  (+ {}: sluice query or --settles full)",
                    omitted.join(", ")
                ));
            }
            lines.join("\n")
        }
        Event::ProjectPause {
            paused,
            reason,
            author,
        } => format!(
            "PROJECT {} {} by {author}{}",
            record
                .project
                .map(|p| p.to_string())
                .unwrap_or_else(|| "home".into()),
            if *paused { "paused" } else { "unpaused" },
            reason
                .as_ref()
                .map(|s| format!(": {}", one(&Value::String(s.clone()), 400)))
                .unwrap_or_default()
        ),
        Event::ProjectArchive {
            archived,
            reason,
            author,
        } => format!(
            "PROJECT {} {} by {author}{}",
            record
                .project
                .map(|p| p.to_string())
                .unwrap_or_else(|| "home".into()),
            if *archived { "archived" } else { "unarchived" },
            reason
                .as_ref()
                .map(|s| format!(": {}", one(&Value::String(s.clone()), 400)))
                .unwrap_or_default()
        ),
        other => {
            let mut value = serde_json::to_value(other).map_err(|e| PublicError::Storage {
                message: e.to_string(),
            })?;
            let kind = value["kind"]
                .as_str()
                .unwrap_or("record")
                .to_uppercase()
                .replace('.', " ");
            if let Some(object) = value.as_object_mut() {
                object.remove("kind");
            }
            format!("{kind} {}", one(&value, 300))
        }
    })
}
/// `sluice next`'s text: one entry per record, messages first, then a blank line and the
/// batch's cursor, `seq N` (`timeout seq N`), kept apart from the `#id`s the entries carry.
pub fn render(
    result: &NextResult,
    settles: Settles,
    cut: usize,
    json_lines: bool,
    cancels: &Cancels,
) -> Result<String, PublicError> {
    let mut lines = vec![];
    for record in messages_first(result) {
        lines.push(if json_lines {
            let mut value = serde_json::to_value(record).map_err(|e| PublicError::Storage {
                message: e.to_string(),
            })?;
            sluice_model::error::show_failure(&mut value);
            value.to_string()
        } else {
            line(record, settles.clone(), cut, cancels)?
        });
    }
    if json_lines {
        lines.push(json!({"seq":result.last_seq,"timed_out":result.timed_out}).to_string());
    } else {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(format!(
            "{}seq {}",
            if result.timed_out { "timeout " } else { "" },
            result.last_seq.0
        ));
    }
    Ok(lines.join("\n") + "\n")
}

/// Once per unread settlement, across reader identities, with durable deduplication: a
/// settlement at least `minutes` old in a project whose orchestrator readers have all been
/// silent that long and have not read past it (config.json `unread_alert_min`).
/// Notify reservations follow the ordinary owner-question posting transaction.
pub async fn unread_alerts(
    writer: &Writer,
    minutes: f64,
) -> Result<Vec<sluice_model::commands::Message>, PublicError> {
    if !minutes.is_finite() || minutes <= 0.0 {
        return Err(PublicError::BadRequest {
            message: "unread_alert_min must be a positive number of minutes".into(),
        });
    }
    let label = format!("{minutes}");
    writer.write(RetrySafety::Idempotent,move|tx|{
        let mut stmt=tx.sql().prepare("SELECT r.project_id,r.seq,r.payload,p.name,max(rd.cursor) FROM records r JOIN projects p ON p.project_id=r.project_id JOIN readers rd ON rd.project_id=p.project_id AND rd.stream='orchestrator' AND rd.thread='' WHERE r.kind='unit.settled' AND p.archived=0 AND p.deleted_at IS NULL GROUP BY r.seq HAVING max(rd.cursor)<r.seq AND (julianday('now')-julianday(max(rd.heartbeat_at)))*1440>=?1 AND (julianday('now')-julianday(r.at))*1440>=?1 ORDER BY r.seq")?;
        let rows=stmt.query_map([minutes],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?)))?.collect::<Result<Vec<_>,_>>()?;drop(stmt);
        let mut posted=vec![];
        for (project,seq,payload,name,cursor)in rows{
            let minutes=&label;
            let project:ProjectId=parse_id(project)?;
            let alerted:bool=tx.sql().query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE project_id=?1 AND \"from\"='sluice' AND json_extract(data,'$.unread_record')=?2)",(project.to_string(),seq),|r|r.get(0))?;
            if alerted{continue;}
            let alert=messages::Post{project:ProjectSelector::Id(project),speaker:messages::Speaker::Sluice,body:format!("Unread settlement: {payload}\nResnapshot status/messages if the cursor expired; otherwise resume with sluice next -p id:{project} --since-seq {cursor}."),verb:messages::Verb::Ask{to:messages::OWNER_STREAM.into(),title:Some(format!("No orchestrator has read {name}'s log for {minutes} min (seq {seq})")),ui:None,input:None,data:Some(serde_json::from_value(json!({"unread_record":seq}))?)}};
            posted.push(messages::post(tx,alert,&messages::NoPlanInputs)?.message);
        }Ok(posted)
    }).await
}
