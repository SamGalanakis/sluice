//! One durable wake filter shared by next and watch. No plan reconstruction at read time.
use crate::calls::{parse_id, public};
use serde_json::{Value, json};
use sluice_model::{
    commands::{Next, NextResult, Settles, StepStatus},
    error::PublicError,
    events::{Event, Record, UnitStep},
    gates::{GateDecision, evaluate_step},
    ids::{ProjectId, ProjectSelector, RecordSeq, WorkGeneration},
    plan::Plan,
    rpc::JsonMap,
};
use sluice_store::{
    ChangeKey, ReadPool, RetrySafety, WriteTransaction, Writer, messages, plans,
    records::{self, RecordFilter},
};
use std::{path::Path, time::Duration};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// Called by reconciliation after status, pause and plan edits, in their transaction.
/// A generation certificate survives feed trimming; outputs and membership freeze here.
pub fn record_settlements(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    plan: &Plan,
) -> sluice_store::Result<Vec<Record>> {
    let stored: String = tx.sql().query_row(
        "SELECT doc FROM plans WHERE project_id=?1",
        [project.to_string()],
        |r| r.get(0),
    )?;
    if serde_json::from_str::<Value>(&stored)? != serde_json::to_value(plan)? {
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
    let mut emitted = vec![];
    for unit in plan.units().values() {
        if !unit.settled(plan, &state) {
            continue;
        }
        let mut stamp = vec![];
        let mut work = 1;
        for id in &unit.steps {
            let (generation, w): (i64, i64) = tx.sql().query_row(
                "SELECT generation,work_generation FROM steps WHERE project_id=?1 AND step_id=?2",
                (project.to_string(), id.as_str()),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            stamp.push(json!([id, generation, w]));
            work = work.max(w);
        }
        let key = format!("{project}/{}", unit.name);
        let stamp = Value::Array(stamp);
        let previous = &settings["settlements"][&key];
        if previous["signature"] == stamp {
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
                let held = status == StepStatus::Pending
                    && (step.is_external()
                        || evaluate_step(plan, &state, step) != GateDecision::Ready);
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
        settings["settlements"][key] = json!({"signature":stamp,"work":work});
    }
    if !emitted.is_empty() {
        tx.sql().execute(
            "UPDATE maintenance SET settings=?1 WHERE singleton=1",
            [settings.to_string()],
        )?;
        tx.changed(Some(project), "status");
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
    fn now(&self) -> Duration {
        match self {
            Clock::Real(start) => start.elapsed(),
            Clock::Manual(clock) => *clock.now.borrow(),
        }
    }
    async fn sleep_until(&self, at: Duration) {
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
/// deadline the new time has reached.
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: tokio::sync::watch::Sender<Duration>,
}
impl ManualClock {
    pub fn new() -> Self {
        Self {
            now: tokio::sync::watch::channel(Duration::ZERO).0,
        }
    }
    pub fn advance(&self, by: Duration) {
        self.now.send_modify(|now| *now += by);
    }
    async fn sleep_until(&self, at: Duration) {
        let mut receiver = self.now.subscribe();
        while *receiver.borrow_and_update() < at {
            if receiver.changed().await.is_err() {
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
    pub projects: Vec<ProjectId>,
    pub since_seq: Option<RecordSeq>,
    pub me: String,
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
            projects: vec![],
            since_seq: None,
            me: "orchestrator".into(),
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
                let classification = if options.all {
                    Classification::Wake
                } else {
                    match &record.event {
                        Event::UnitSettled { .. } => Classification::Wake,
                        Event::StepStatus {
                            to: StepStatus::Failed | StepStatus::Stale | StepStatus::Skipped,
                            ..
                        } => Classification::Wake,
                        Event::Message(msg) => {
                            let wake = messages::message_wakes(
                                sql,
                                record.project.ok_or_else(|| PublicError::Invalid {
                                    message: "message record without project".into(),
                                    errors: vec![],
                                })?,
                                msg,
                                &options.me,
                            )?;
                            if wake {
                                Classification::Wake
                            } else if !msg.needs_reply && msg.from != options.me {
                                Classification::Note
                            } else {
                                Classification::Ignore
                            }
                        }
                        Event::ProjectPause { author, .. }
                        | Event::ProjectArchive { author, .. }
                            if author != &options.me =>
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
    let selectors = request.projects;
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
            projects: selected,
            since_seq: Some(request.since_seq),
            me: request.me,
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
    if options.me.trim().is_empty()
        || options.since_seq.is_some_and(|s| s.0 < 0)
        || options.unread_alert_min.is_some_and(|n| n < 0)
    {
        return Err(PublicError::BadRequest {
            message: "reader must be nonblank and cursor/alert threshold nonnegative".into(),
        });
    }
    options.projects = projects(reads, options.projects).await?;
    let mut cursor = if let Some(seq) = options.since_seq {
        seq
    } else {
        let selected = options.projects.clone();
        reads
            .snapshot(move |sql| {
                let mut top = RecordSeq(0);
                for p in selected {
                    top = RecordSeq(top.0.max(records::bounds(sql, Some(p))?.1.0));
                }
                Ok(top)
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
        let now = options.clock.now();
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
    let me = options.me.clone();
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
pub fn line(record: &Record, settles: Settles, cut: usize) -> Result<String, PublicError> {
    if cut == 0 {
        return Err(PublicError::BadRequest {
            message: "cut must be positive".into(),
        });
    }
    Ok(match &record.event {
        Event::Message(m) => format!(
            "{} {} {} -> {}: {}",
            if m.needs_reply { "MSG" } else { "NOTE" },
            m.thread,
            m.from,
            m.to.as_deref().unwrap_or("-"),
            m.body.trim().replace('\n', "\n  ")
        ),
        Event::StepStatus {
            step,
            from,
            to,
            error,
            ..
        } => {
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
                "STEP {step} {} -> {}{tail}",
                status_text(from.as_ref().unwrap_or(&StepStatus::Pending)),
                status_text(to)
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
                    format!(
                        "{} {}{}",
                        name(&s.id),
                        status_text(&s.status),
                        if s.held { " (held)" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ");
            let mut lines = vec![format!("UNIT {unit} settled: {marks}")];
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
fn status_text(s: &StepStatus) -> &'static str {
    match s {
        StepStatus::Pending => "pending",
        StepStatus::Running => "running",
        StepStatus::Succeeded => "succeeded",
        StepStatus::Failed => "failed",
        StepStatus::Stale => "stale",
        StepStatus::Skipped => "skipped",
    }
}
pub fn render(
    result: &NextResult,
    settles: Settles,
    cut: usize,
    json_lines: bool,
) -> Result<String, PublicError> {
    let mut lines = vec![];
    for record in messages_first(result) {
        lines.push(if json_lines {
            serde_json::to_string(record).map_err(|e| PublicError::Storage {
                message: e.to_string(),
            })?
        } else {
            line(record, settles.clone(), cut)?
        });
    }
    lines.push(if json_lines {
        json!({"seq":result.last_seq,"timed_out":result.timed_out}).to_string()
    } else {
        format!(
            "{}seq {}",
            if result.timed_out { "timeout " } else { "" },
            result.last_seq.0
        )
    });
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
            let post=serde_json::from_value(json!({"project":{"kind":"id","value":project},"from":"sluice","to":"owner","needs_reply":true,"title":format!("No orchestrator has read {name}'s log for {minutes} min (seq {seq})"),"body":format!("Unread settlement: {payload}\nResnapshot status/messages if the cursor expired; otherwise resume with sluice next -p id:{project} --since-seq {cursor}."),"data":{"unread_record":seq}}))?;
            posted.push(messages::message_post(tx,post,&messages::NoPlanInputs)?);
        }Ok(posted)
    }).await
}
