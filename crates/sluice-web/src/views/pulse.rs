//! A pulse strip's details (DESIGN.md, At work): what a unit's agent did in one minute of its
//! strip, or when its run began, asked for as a reader points at a mark or the strip's faded
//! left edge (`sluice-pulse`) and drawn into the page's one popover. Never in the page or its
//! stream: a strip carries only its marks' times, and its details are read when asked for, the
//! minute's turns and calls from its run's transcript (`sluice_agents::activity::outline`, read
//! as the Activity tab reads it) and its messages from the store.
use super::ui::esc;
use super::{DashboardState, ui, ui::Host};
use crate::clock::ClockRead;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sluice_agents::activity::{self as act, Homes, Window};
use sluice_model::ids::ProjectId;
use std::fmt::Write;

/// The page's popover for its strips' details (`id`; its body `<id>-body` is what the last
/// `@get` patched in). The page's is drawn once after its main (the layout), outside every
/// region a stream patches, so a roster's patch never empties it while it is read; only its
/// own answers patch it.
pub fn popover(id: &str) -> String {
    format!(
        "<div id=\"{id}\" class=\"dm-p pulse-pop\" popover=\"auto\"><div id=\"{id}-body\" class=\"pp-b\" aria-live=\"polite\"></div></div>",
        id = esc(id)
    )
}
/// Items a minute's details list at most; the rest are counted.
const MOST: usize = 8;
/// A line's words at most (a command, a sentence, an excerpt), before the popover clips it.
const LINE: usize = 160;

/// The listener on what holds a page's strips and their popover `pop` (At work's section, the
/// gallery's part): a strip's pick (`sluice-pulse`, `detail.q` its query) asks `src` for its
/// details, debounced, a newer pick cancelling the one in flight (`requestCancellation:
/// 'cleanup'`: the same action on the same element), sending no signals.
pub fn listen(src: &str, pop: &str) -> String {
    format!(
        " data-pulse-pop=\"{}\" data-on:sluice-pulse__debounce.120ms=\"@get('{}?pop={}&amp;' + evt.detail.q, {{requestCancellation: 'cleanup', filterSignals: {{include: /^$/}}}})\"",
        esc(pop),
        esc(&src.replace('\\', "\\\\").replace('\'', "\\'")),
        esc(pop)
    )
}
/// A strip's host (`sluice-pulse`): `class` its look, `unit` (and `step`, a step's own strip in
/// a drilldown) what its details are asked of, `from` when its run began (seconds; the script
/// fades its left edge while that is before the window), `label` what a screen reader hears.
pub fn host(class: &str, unit: &str, step: &str, from: Option<f64>, label: &str) -> Host {
    Host::new("sluice-pulse")
        .attr("class", class)
        .attr("data-pulse", format!("{}", super::unit_steps::PULSE as u64))
        .attr("tabindex", "0")
        .attr("role", "group")
        .attr("aria-label", label)
        .attr(
            "aria-description",
            "The arrow keys say what it did each minute.",
        )
        .attr("unit", unit)
        .some("step", step)
        .some("from", from.map(|f| format!("{f:.0}")).unwrap_or_default())
        .keep("data-before")
}

/// Keep the changing accessible description independent of a strip's stable marks container.
/// A new mark changes this small text region, never the whole pulse host.
pub fn labelled(host: Host, id: &str, words: &str, marks: &str) -> String {
    let label = format!(
        "<!--r:{id}--><span id=\"{id}\" class=\"vh\">{}</span><!--/r:{id}-->",
        esc(words)
    );
    host.attr("aria-describedby", id)
        .wrap(&super::TrustedHtml::owned(format!("{label}{marks}")))
        .to_string()
}

#[derive(Deserialize)]
pub struct PulseQuery {
    /// The popover it is drawn in (`popover`'s id).
    pub pop: Option<String>,
    pub unit: Option<String>,
    pub step: Option<String>,
    /// A minute's start, seconds since the epoch.
    pub at: Option<u64>,
    /// When its run began, seconds: the strip's left edge.
    pub from: Option<u64>,
    /// The pick's key, echoed (`data-for`), so the strip shows only its own answer.
    pub k: Option<String>,
}

/// `GET /projects/id/<p>/pulse?unit=<u>[&step=<s>]&(at=<minute>|from=<start>)&k=<key>`: the
/// popover's body for one minute of a unit's strip (one step's, with `step`), or for its left
/// edge.
pub async fn details(
    State(state): State<DashboardState>,
    Path(project): Path<ProjectId>,
    Query(q): Query<PulseQuery>,
) -> Response {
    let (key, pop) = (q.k.clone().unwrap_or_default(), pop(&q));
    if let Some(from) = q.from {
        return body(&pop, &key, &started(from));
    }
    let (Some(unit), Some(at)) = (q.unit.clone(), q.at) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let step = q.step.clone().filter(|s| !s.is_empty());
    let read = state
        .reads
        .clock_snapshot(move |c| minute_rows(c, project, &unit, step.as_deref(), at))
        .await;
    let (runs, messages) = match read {
        Ok(found) => found,
        Err(e) => return super::board::error_response(e.into_public(true)),
    };
    let homes = Homes {
        sluice: state.reads.home().to_owned(),
        claude: state.claude_home.clone(),
    };
    let href = format!("/projects/id/{project}");
    let items = tokio::task::spawn_blocking(move || minute(&homes, &runs, messages, at, &href))
        .await
        .unwrap_or_default();
    body(&pop, &key, &minute_html(at, &items))
}
/// The popover asked for: its id, when it is one.
fn pop(q: &PulseQuery) -> String {
    q.pop
        .clone()
        .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or_else(|| "pulse-pop".into())
}
/// The popover `pop`'s body, for the pick `key`.
fn body(pop: &str, key: &str, inner: &str) -> Response {
    super::write::events([crate::streams::StreamEvent::Elements(
        datastar::prelude::PatchElements::new(format!(
            "<div id=\"{pop}-body\" class=\"pp-b\" aria-live=\"polite\" data-for=\"{}\">{inner}</div>",
            esc(key)
        )),
    )])
}
/// Its left edge: when its run began, how long ago on the reader's clock.
pub fn started(from: u64) -> String {
    let at = super::rfc3339(from);
    format!(
        "<p class=\"pp-h\">Started {}, at {}</p>",
        ui::ago(&at),
        ui::clock(&at)
    )
}

/// A running run of the unit: its id, its step, when it began (ms) and its number among its
/// step's runs.
struct Run {
    id: String,
    step: String,
    since_ms: u64,
    n: usize,
}
/// A message the unit's steps sent in the minute: who, its words, when (ms).
struct Said {
    from: String,
    body: String,
    at_ms: u64,
}
fn minute_rows(
    c: &rusqlite::Connection,
    project: ProjectId,
    unit: &str,
    step: Option<&str>,
    at: u64,
) -> sluice_store::Result<(Vec<Run>, Vec<Said>)> {
    let project = project.to_string();
    let mut steps = vec![];
    let mut q = c.prepare_cached(
        "SELECT step_id FROM steps WHERE project_id=?1 AND coalesce(unit,step_id)=?2 AND (?3 IS NULL OR step_id=?3) ORDER BY step_id",
    )?;
    let mut rows = q.query((&project, unit, step))?;
    while let Some(r) = rows.next()? {
        steps.push(r.get::<_, String>(0)?);
    }
    let mut runs = vec![];
    let mut said = vec![];
    let mut q_runs = c.prepare_cached(
        "SELECT r.run_id,CAST(strftime('%s',coalesce(r.started_at,r.created_at)) AS INTEGER),(SELECT count(*) FROM runs o WHERE o.project_id=r.project_id AND o.step_id=r.step_id AND o.generation=r.generation AND (o.created_at<r.created_at OR (o.created_at=r.created_at AND o.run_id<=r.run_id))) FROM runs r JOIN steps s ON s.project_id=r.project_id AND s.step_id=r.step_id AND s.generation=r.generation WHERE r.project_id=?1 AND r.step_id=?2 AND r.finished_at IS NULL",
    )?;
    let mut q_said = c.prepare_cached(
        "SELECT \"from\",body,CAST(round((julianday(at)-2440587.5)*86400000.0) AS INTEGER) FROM messages WHERE project_id=?1 AND \"from\"=?2 AND julianday(at)>=julianday(?3) AND julianday(at)<julianday(?4) ORDER BY id",
    )?;
    let (start, end) = (super::rfc3339(at), super::rfc3339(at + 60));
    for step in &steps {
        let mut rows = q_runs.query((&project, step))?;
        while let Some(r) = rows.next()? {
            runs.push(Run {
                id: r.get(0)?,
                step: step.clone(),
                since_ms: r.get::<_, i64>(1)?.max(0) as u64 * 1000,
                n: r.get::<_, i64>(2)?.max(1) as usize,
            });
        }
        let mut rows = q_said.query((&project, step, &start, &end))?;
        while let Some(r) = rows.next()? {
            said.push(Said {
                from: r.get(0)?,
                body: r.get(1)?,
                at_ms: r.get::<_, i64>(2)?.max(0) as u64,
            });
        }
    }
    Ok((runs, said))
}

/// One thing the unit did in the minute.
#[derive(Clone, Debug, Default)]
struct Item {
    at_ms: u64,
    /// Its head: a tool's name, what opened a turn, who sent a message (HTML).
    head: String,
    /// Its words, one line.
    line: String,
    /// It is a tool call: its line is its key argument, in mono.
    call: bool,
    /// Its step, and where the step's Activity tab shows it.
    step: String,
    anchor: Option<String>,
}
fn minute(homes: &Homes, runs: &[Run], said: Vec<Said>, at: u64, href: &str) -> Vec<Item> {
    let (lo, hi) = (at * 1000, at * 1000 + 60_000);
    let inside = |t: Option<u64>| t.is_some_and(|t| t >= lo && t < hi);
    let mut items = vec![];
    for run in runs {
        let Some(outline) = act::outline(
            homes,
            &run.id,
            Window {
                since_ms: run.since_ms,
                until_ms: None,
            },
        ) else {
            continue;
        };
        let activity = |anchor: String| {
            format!(
                "{href}/steps/{}?activity=all&tab=activity#{anchor}",
                run.step
            )
        };
        for (t, turn) in outline.turns.iter().enumerate() {
            let turn_anchor = format!("act-r{}-t{}", run.n, t + 1);
            if inside(turn.started_ms) {
                let (head, words) = super::activity::opened(&turn.sent);
                items.push(Item {
                    at_ms: turn.started_ms.unwrap_or(lo),
                    head: esc(&head),
                    line: sentence(&words),
                    call: false,
                    step: run.step.clone(),
                    anchor: Some(activity(turn_anchor.clone())),
                });
            }
            for (c, call) in turn.calls.iter().enumerate() {
                if inside(call.started_ms) {
                    items.push(Item {
                        at_ms: call.started_ms.unwrap_or(lo),
                        head: esc(&call.tool),
                        line: line(&call.key),
                        call: true,
                        step: run.step.clone(),
                        anchor: Some(activity(format!("{turn_anchor}-c{}", c + 1))),
                    });
                }
            }
        }
    }
    for message in said {
        items.push(Item {
            at_ms: message.at_ms,
            head: ui::party_html(&message.from, false),
            line: sentence(&message.body),
            call: false,
            step: message.from,
            anchor: None,
        });
    }
    items.sort_by_key(|i| i.at_ms);
    items
}
/// The minute's body: its time on the reader's clock, then each thing in order (its step's
/// name too when the unit ran more than one), the rest counted, and a link to the first in its
/// step's Activity tab.
fn minute_html(at: u64, items: &[Item]) -> String {
    let mut out = format!("<p class=\"pp-h\">{}</p>", ui::clock(&super::rfc3339(at)));
    if items.is_empty() {
        out.push_str("<p class=\"pp-none\">Its transcript records nothing this minute; its run wrote to its files.</p>");
        return out;
    }
    let several = items
        .iter()
        .any(|i| i.step != items[0].step && !i.step.is_empty());
    out.push_str("<ul class=\"pp-l\">");
    for item in items.iter().take(MOST) {
        let _ = write!(
            out,
            "<li><span class=\"pp-k\">{}</span>{}<{tag} class=\"pp-w\">{}</{tag}></li>",
            item.head,
            if several {
                format!(" <span class=\"pp-s\">{}</span>", esc(&item.step))
            } else {
                String::new()
            },
            esc(&item.line),
            tag = if item.call { "code" } else { "span" }
        );
    }
    out.push_str("</ul>");
    let more = items.len().saturating_sub(MOST);
    let link = items.iter().find_map(|i| i.anchor.as_deref());
    if more > 0 || link.is_some() {
        out.push_str("<p class=\"pp-more\">");
        if more > 0 {
            let _ = write!(out, "<span>+{more} more</span>");
        }
        if let Some(href) = link {
            let _ = write!(out, "<a href=\"{}\">See it in Activity</a>", esc(href));
        }
        out.push_str("</p>");
    }
    out
}
/// Its first line, cut to `LINE` characters.
fn line(text: &str) -> String {
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    cut(first)
}
/// Its first sentence (its first line, to its first full stop), cut to `LINE` characters;
/// markdown's marks dropped.
fn sentence(text: &str) -> String {
    let first = line(&text.replace(['`', '*', '#', '_'], ""));
    let end = first
        .match_indices(". ")
        .next()
        .map_or(first.len(), |(i, _)| i + 1);
    first[..end].to_owned()
}
fn cut(text: &str) -> String {
    if text.chars().count() <= LINE {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(LINE - 1).collect();
    out.push('…');
    out
}

/// The gallery's strips' details (`/_ui/pulse`): a minute and an edge as a strip draws them,
/// with nothing read.
pub async fn gallery(Query(q): Query<PulseQuery>) -> Response {
    let (key, pop) = (q.k.clone().unwrap_or_default(), pop(&q));
    if let Some(from) = q.from {
        return body(&pop, &key, &started(from));
    }
    let at = q.at.unwrap_or_default();
    let items = [
        (
            "Task",
            "Your task is in task.md; read it fully, then do it.",
            false,
        ),
        (
            "Bash",
            "git fetch -q origin && git log --oneline -5 origin/main",
            true,
        ),
        ("Read", "sections/spring/coast/terns-and-skimmers.md", true),
        ("Edit", "crates/sluice-web/src/views/plan.rs", true),
    ]
    .into_iter()
    .enumerate()
    .map(|(n, (head, words, call))| Item {
        at_ms: at * 1000 + n as u64 * 9_000,
        head: esc(head),
        line: words.to_owned(),
        call,
        step: "draft".into(),
        anchor: Some("#part-plan-briefing".into()),
    })
    .collect::<Vec<_>>();
    body(&pop, &key, &minute_html(at, &items))
}
