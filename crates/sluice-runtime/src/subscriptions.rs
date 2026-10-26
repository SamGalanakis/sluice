//! Home-owned subscriptions share the bounded event log, with independent durable cursors.
//! The maintenance settings namespace is additive and survives older coordinators.
use crate::{calls::public, watch::Clock};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sluice_model::{
    commands::{
        MessageVerb, NextResult, StepSays, StepStatus, SubscriptionSet, SubscriptionWake as Wake,
    },
    error::PublicError,
    events::{Event, Record},
    identity::SessionName,
    ids::{ProjectId, RecordSeq, RunId},
};
use sluice_store::{
    ChangeKey, ReadPool, RetrySafety, Writer, identities, messages,
    records::{self, RecordFilter, ScopeFilter},
};
use std::{collections::BTreeMap, time::Duration};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Stored {
    name: String,
    #[serde(default)]
    include_routine: bool,
    projects: Vec<ProjectId>,
    wake: Vec<Wake>,
    say_from_steps: StepSays,
    digest: Option<String>,
    me: String,
    cursor: RecordSeq,
    #[serde(default)]
    cursors: BTreeMap<ProjectId, RecordSeq>,
    #[serde(default)]
    start_seq: Option<RecordSeq>,
    revision: RunId,
}
fn bad(message: impl Into<String>) -> PublicError {
    PublicError::BadRequest {
        message: message.into(),
    }
}
fn disconnected() -> PublicError {
    PublicError::Cancelled {
        message: "subscription client disconnected; cursor retained".into(),
    }
}
fn conflict() -> PublicError {
    PublicError::Conflict {
        message: "subscription changed or was consumed by another reader; retry next".into(),
        current_rev: None,
    }
}
fn settings(sql: &Connection) -> sluice_store::Result<Value> {
    let raw: String = sql.query_row(
        "SELECT settings FROM maintenance WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&raw)?)
}
fn load(settings: &Value, name: &str) -> sluice_store::Result<Stored> {
    let value = settings["subscriptions"]
        .get(name)
        .cloned()
        .ok_or_else(|| PublicError::NotFound {
            message: format!("subscription {name} not found"),
        })?;
    Ok(serde_json::from_value(value)?)
}
fn save(sql: &Connection, settings: &Value) -> sluice_store::Result<()> {
    sql.execute(
        "UPDATE maintenance SET settings=?1 WHERE singleton=1",
        [serde_json::to_string(settings)?],
    )?;
    Ok(())
}
fn interval(value: &str) -> Result<Duration, PublicError> {
    let (number, multiplier) = match value.as_bytes().last() {
        Some(b's') => (&value[..value.len() - 1], 1),
        Some(b'm') => (&value[..value.len() - 1], 60),
        Some(b'h') => (&value[..value.len() - 1], 3600),
        Some(b'd') => (&value[..value.len() - 1], 86400),
        _ => (value, 1),
    };
    let seconds = number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .filter(|n| *n > 0 && *n <= 86400 * 365);
    seconds
        .map(Duration::from_secs)
        .ok_or_else(|| bad("digest must be a positive duration up to 365d (e.g. 30m, 10s, 1h)"))
}
pub async fn set(writer: &Writer, request: SubscriptionSet) -> Result<Value, PublicError> {
    if request.name.trim().is_empty() || request.name.len() > 128 || request.wake.is_empty() {
        return Err(bad(
            "subscription needs a name (1..128 bytes) and wake kinds",
        ));
    }
    let me = SessionName::parse(&request.me)?;
    if let Some(digest) = &request.digest {
        interval(digest)?;
    }
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let mut state = settings(tx.sql())?;
            let previous = state["subscriptions"]
                .get(&request.name)
                .map(|_| load(&state, &request.name))
                .transpose()?;
            let mut projects = vec![];
            let mut top = RecordSeq(tx.sql().query_row(
                "SELECT coalesce(max(seq),0) FROM records",
                [],
                |r| r.get(0),
            )?);
            let dynamic = request.projects.is_empty();
            for selector in request.projects {
                let p = messages::resolve_project(tx.sql(), &selector)?;
                if !projects.contains(&p) {
                    projects.push(p);
                }
                top.0 = top.0.max(records::bounds(tx.sql(), Some(p))?.1.0);
                identities::register(tx, p, &me)?;
            }
            let selected = if dynamic {
                crate::watch::addressed_projects(tx.sql(), Some(me.as_str()))?
            } else {
                projects.clone()
            };
            let mut cursors = previous
                .as_ref()
                .map(|s| s.cursors.clone())
                .unwrap_or_default();
            for project in selected {
                cursors.entry(project).or_insert(
                    previous
                        .as_ref()
                        .map_or(top, |s| s.start_seq.unwrap_or(s.cursor)),
                );
            }
            let stored = Stored {
                cursors,
                start_seq: Some(
                    previous
                        .as_ref()
                        .map_or(top, |s| s.start_seq.unwrap_or(s.cursor)),
                ),
                include_routine: request.include_routine,
                name: request.name,
                projects,
                wake: request.wake,
                say_from_steps: request.say_from_steps,
                digest: request.digest,
                me: me.to_string(),
                cursor: previous.as_ref().map_or(top, |s| s.cursor),
                revision: RunId::new(),
            };
            let value = serde_json::to_value(&stored)?;
            state["subscriptions"][&stored.name] = value.clone();
            save(tx.sql(), &state)?;
            tx.changed(None, "subscriptions");
            Ok(value)
        })
        .await
}
pub async fn get(reads: &ReadPool, name: String) -> Result<Value, PublicError> {
    reads
        .snapshot(move |sql| Ok(serde_json::to_value(load(&settings(sql)?, &name)?)?))
        .await
        .map_err(public)
}
pub async fn list(reads: &ReadPool) -> Result<Value, PublicError> {
    reads
        .snapshot(|sql| {
            let state = settings(sql)?;
            let mut items = state["subscriptions"]
                .as_object()
                .map(|o| o.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            items.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            Ok(Value::Array(items))
        })
        .await
        .map_err(public)
}
pub async fn remove(writer: &Writer, name: String) -> Result<(), PublicError> {
    writer
        .write(RetrySafety::Idempotent, move |tx| {
            let mut state = settings(tx.sql())?;
            if let Some(items) = state["subscriptions"].as_object_mut() {
                items.remove(&name);
            }
            save(tx.sql(), &state)?;
            tx.changed(None, "subscriptions");
            Ok(())
        })
        .await
}
fn kind(record: &Record) -> String {
    match &record.event {
        Event::Message(m) => match m.verb {
            MessageVerb::Ask => "ask",
            MessageVerb::Reply => "reply",
            MessageVerb::Say => "say",
        }
        .into(),
        Event::StepStatus {
            to: StepStatus::Failed,
            ..
        } => "failed".into(),
        Event::StepStatus {
            to: StepStatus::Stale,
            ..
        } => "stale".into(),
        Event::UnitSettled { .. } => "settled".into(),
        _ => serde_json::to_value(&record.event).expect("event JSON")["kind"]
            .as_str()
            .unwrap_or("other")
            .to_owned(),
    }
}
fn matches(sql: &Connection, s: &Stored, record: &Record) -> sluice_store::Result<bool> {
    let has = |wake| s.wake.contains(&wake);
    if let Event::Message(m) = &record.event
        && m.from == "sluice"
        && m.to.as_deref() == Some(&s.me)
        && m.data
            .as_ref()
            .is_some_and(|d| d.as_value().get("run_check").is_some())
    {
        return Ok(true);
    }

    Ok(match &record.event {
        Event::Message(m) if m.from != s.me => {
            let addressed =
                m.to.as_deref()
                    .is_none_or(|to| to == s.me || to == "orchestrator");
            match m.verb {
                MessageVerb::Ask => addressed && has(Wake::Ask),
                MessageVerb::Reply => addressed && has(Wake::Reply),
                MessageVerb::Say => {
                    let limit = m.from == "sluice"
                        && m.data
                            .as_ref()
                            .is_some_and(|v| v.as_value().get("execution_limit").is_some());
                    if limit && addressed && has(Wake::Limit) {
                        true
                    } else if !addressed || !has(Wake::SayToMe) {
                        false
                    } else {
                        let project = record.project.map(|p| p.to_string());
                        let step: bool = sql.query_row(
                            "SELECT EXISTS(SELECT 1 FROM steps WHERE project_id=?1 AND step_id=?2)",
                            (&project, &m.from),
                            |r| r.get(0),
                        )?;
                        !(step || m.run.is_some()) || match s.say_from_steps {
                            StepSays::All => true,
                            StepSays::None => false,
                            StepSays::AsksOnly => sql.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE project_id=?1 AND \"from\"=?2 AND \"to\"=?3 AND needs_reply=1 AND resolved_by IS NULL AND closed_at IS NULL)", (&project, &s.me, &m.from), |r| r.get(0))?,
                        }
                    }
                }
            }
        }
        Event::StepStatus { to, error, .. } => {
            (*to == StepStatus::Failed && has(Wake::Failed))
                || (*to == StepStatus::Stale && has(Wake::Stale))
                || (*to == StepStatus::Failed
                    && has(Wake::Quota)
                    && matches!(error, Some(PublicError::AgentFailure { kind, .. }) if kind == "QuotaExhausted"))
                || (*to == StepStatus::Failed
                    && has(Wake::Limit)
                    && matches!(error, Some(PublicError::ResourceExhausted { .. })))
        }
        Event::UnitSettled { .. } => has(Wake::Settled),
        _ => false,
    })
}

/// A whole bounded snapshot is classified before returning, including all pages and projects.
/// The compare-and-set commit refuses concurrent consumption or filter changes. Until then
/// cancellation leaves the cursor untouched, so the next client replays the pending batch.
pub async fn next(
    writer: &Writer,
    reads: &ReadPool,
    name: String,
    timeout: Duration,
    clock: Clock,
    include_routine: Option<bool>,
) -> Result<NextResult, PublicError> {
    let gone = crate::coordinator::request_disconnected();
    let s: Stored =
        serde_json::from_value(get(reads, name).await?).map_err(|e| bad(e.to_string()))?;
    let mut keys = if s.projects.is_empty() {
        vec![ChangeKey::new(None, "log")]
    } else {
        s.projects
            .iter()
            .map(|p| ChangeKey::new(Some(*p), "log"))
            .collect()
    };
    keys.push(ChangeKey::new(None, "subscriptions"));
    let mut changes = reads.subscribe(writer, keys).await.map_err(public)?;
    let mut result = NextResult {
        records: vec![],
        notes: vec![],
        last_seq: s.cursor,
        timed_out: false,
        suppressed: Some(BTreeMap::new()),
    };
    let mut cursors = s.cursors.clone();
    let deadline = clock.now() + timeout;
    let digest = s.digest.as_deref().map(interval).transpose()?;
    let mut digest_at = None;
    let mut heartbeat = clock.now();
    loop {
        if gone.is_cancelled() {
            return Err(disconnected());
        }
        let now = clock.now();
        if now >= heartbeat {
            let selected = s.clone();
            writer
                .write(RetrySafety::Idempotent, move |tx| {
                    let me = SessionName::parse(&selected.me)?;
                    let projects = if selected.projects.is_empty() {
                        crate::watch::addressed_projects(tx.sql(), Some(&selected.me))?
                    } else {
                        selected.projects
                    };
                    for project in projects {
                        identities::beat(tx, project, &me)?;
                    }
                    Ok(())
                })
                .await?;
            heartbeat = clock.now() + Duration::from_secs(30);
        }
        let selected = s.clone();
        let mut positions = cursors.clone();
        let (matching, suppressed, top, positions) = reads
            .snapshot(move |sql| {
                if load(&settings(sql)?, &selected.name)? != selected {
                    return Err(conflict().into());
                }
                let mut matching = vec![];
                let mut suppressed = BTreeMap::<String, u64>::new();
                let mut top = positions
                    .values()
                    .copied()
                    .max_by_key(|s| s.0)
                    .unwrap_or(selected.cursor);
                let projects = if selected.projects.is_empty() {
                    crate::watch::addressed_projects(sql, Some(&selected.me))?
                } else {
                    selected.projects.clone()
                };
                for project in &projects {
                    let mut after = *positions
                        .entry(*project)
                        .or_insert(selected.start_seq.unwrap_or(selected.cursor));
                    loop {
                        let page = records::read_records(
                            sql,
                            Some(*project),
                            &RecordFilter {
                                since: Some(after),
                                limit: 200,
                                scope: Some(ScopeFilter {
                                    me: Some(selected.me.clone()),
                                    mine: true,
                                    ..ScopeFilter::default()
                                }),
                                ..RecordFilter::default()
                            },
                        )?
                        .into_page()?;
                        let full = page.records.len() == 200;
                        for record in page.records {
                            if (include_routine.unwrap_or(selected.include_routine)
                                || !crate::routine::routine(sql, &record, Some(&selected.me))?)
                                && matches(sql, &selected, &record)?
                            {
                                matching.push(record);
                            } else {
                                *suppressed.entry(kind(&record)).or_default() += 1;
                            }
                        }
                        positions.insert(*project, page.last_seq);
                        top.0 = top.0.max(page.last_seq.0);
                        if !full {
                            break;
                        }
                        after = page.last_seq;
                    }
                }
                matching.sort_by_key(|r| r.seq.0);
                Ok((matching, suppressed, top, positions))
            })
            .await
            .map_err(public)?;
        cursors = positions;
        result.records.extend(matching);
        let counts = result.suppressed.as_mut().expect("subscription digest");
        for (kind, count) in suppressed {
            *counts.entry(kind).or_default() += count;
        }
        if !counts.is_empty() && digest_at.is_none() {
            digest_at = digest.map(|d| clock.now() + d);
        }
        result.last_seq = top;
        if !result.records.is_empty() || now >= deadline || digest_at.is_some_and(|at| now >= at) {
            result.timed_out = result.records.is_empty() && now >= deadline;
            let selected = s.clone();
            let gone = gone.clone();
            let positions = cursors.clone();
            writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    if gone.is_cancelled() {
                        return Err(disconnected().into());
                    }
                    let mut state = settings(tx.sql())?;
                    if load(&state, &selected.name)? != selected {
                        return Err(conflict().into());
                    }
                    let mut updated = selected;
                    updated.cursor = top;
                    updated.cursors = positions;
                    let me = SessionName::parse(&updated.me)?;
                    for project in updated.cursors.keys() {
                        identities::beat(tx, *project, &me)?;
                    }
                    updated.revision = RunId::new();
                    state["subscriptions"][&updated.name] = serde_json::to_value(&updated)?;
                    save(tx.sql(), &state)?;
                    tx.changed(None, "subscriptions");
                    Ok(())
                })
                .await?;
            return Ok(result);
        }
        let until = digest_at.unwrap_or(deadline).min(deadline).min(heartbeat);
        tokio::select! {
            biased;
            _ = gone.cancelled() => return Err(disconnected()),
            changed = changes.wait() => { changed.map_err(public)?; },
            _ = clock.sleep_until(until) => {}
        }
    }
}
