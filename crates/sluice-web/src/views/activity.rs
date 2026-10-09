//! A run's activity outline on the step page and in the drawer: what its agent did, turn by
//! turn (what sluice sent, the agent's last words, its calls counted), each turn folding open to
//! its tool calls (a kind's icon, the tool, its key argument, how long it took, how it ended),
//! each call folding open to its arguments and result. Read when the page draws, from the
//! transcript its engine keeps (`sluice_agents::activity`); nothing is stored.
//!
//! Failures are never hidden: a failed call and its turn always show, whatever is left out (the
//! latest turns only, the latest calls of a long turn, a fold of reads and searches), and on a
//! failed step the last call that failed is open, its turn too, and "Why it failed" links to it.
use super::{
    DashboardState, TrustedHtml,
    icons::{Icon, icon},
    step::StepView,
    ui,
};
use askama::Template;
use serde::Serialize;
use sluice_agents::activity::{self as act, Homes, Kind, Outcome, SentKind, Window};

/// Turns drawn unless all are asked for: the latest, and every earlier one with a failure.
pub const TURNS_SHOWN: usize = 5;
/// A turn's calls drawn unless all are asked for: the latest, and every earlier failure.
pub const CALLS_SHOWN: usize = 12;
/// A result longer than this many lines or characters folds behind "Show all".
const RESULT_LINES: usize = 6;
const RESULT_CHARS: usize = 480;

/// The latest agent run's outline, as its section draws it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ActivityView {
    /// Its run's number among the step's runs (1 is the first).
    pub run: usize,
    pub engine: &'static str,
    /// What it was read from, in words ("its Claude session transcript").
    pub source: &'static str,
    /// Its run still runs: its last turn, unless the engine closed it, is open.
    pub live: bool,
    pub turns: Vec<TurnView>,
    /// Earlier turns left out (none failed); "Show earlier turns" draws them.
    pub earlier: usize,
    /// Every turn and call is drawn.
    pub all: bool,
    pub calls: usize,
    pub failed: usize,
    /// Records too long to read.
    pub skipped: usize,
    /// The step's own page, for "Show earlier turns".
    pub step_href: String,
    /// On a failed step, the last call that failed.
    pub failing: Option<FailingCall>,
    /// The raw transcript its run directory holds, served masked: its name and link.
    pub raw: Option<(String, String)>,
}
/// The call "Why it failed" links to.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FailingCall {
    pub anchor: String,
    pub tool: String,
    pub key: String,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TurnView {
    pub n: usize,
    pub anchor: String,
    pub sent_kind: &'static str,
    pub sent: String,
    pub said: String,
    pub calls: usize,
    pub failed: usize,
    /// Its calls by tool, most first ("Bash 8 · Edit 3 · Read 1").
    pub tools: String,
    /// "took 4m" once it ended.
    pub took: String,
    /// While it is the live run's open turn: when it started (RFC 3339), for its ticking time.
    pub since: String,
    pub running: bool,
    /// Drawn open: the live run's open turn, the failing call's turn, or a run's only turn.
    pub open: bool,
    pub items: Vec<Item>,
    /// Earlier calls left out (none failed).
    pub earlier_calls: usize,
}
/// One row under a turn: a call, or a fold of reads, searches and listings in a row.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Item {
    Call(CallView),
    Looks { words: String, calls: Vec<CallView> },
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CallView {
    pub anchor: String,
    pub tool: String,
    pub kind: Kind,
    pub key: String,
    pub args: Vec<(String, String)>,
    pub result: String,
    /// The whole result was longer than the kept head and tail.
    pub result_chars: usize,
    /// "done", "failed", "running" (the live run's call with no result yet) or "none" (its
    /// run ended before it had one).
    pub outcome: &'static str,
    /// "12s" once it ended, when its transcript times it.
    pub took: String,
    pub open: bool,
    /// The call "Why it failed" links to.
    pub failing: bool,
}
impl CallView {
    pub fn icon(&self) -> TrustedHtml {
        let shape = match self.kind {
            Kind::Shell => Icon::Terminal,
            Kind::Edit => Icon::FilePen,
            Kind::Read => Icon::FileText,
            Kind::Search => Icon::Search,
            Kind::List => Icon::FolderOpen,
            Kind::Agent => Icon::Bot,
            Kind::Web => Icon::Globe,
            Kind::Message => Icon::MessageSquare,
            Kind::Other => Icon::Wrench,
        };
        icon(shape, 16, "act-k")
    }
    /// Its result folds behind "Show all": more than a few lines.
    pub fn long(&self) -> bool {
        self.result.lines().count() > RESULT_LINES || self.result.chars().count() > RESULT_CHARS
    }
    /// "Error" for a failed call's result, else "Result".
    pub fn result_head(&self) -> &'static str {
        if self.outcome == "failed" {
            "Error"
        } else {
            "Result"
        }
    }
    /// How much of a long result is kept: "12,408 characters; its start and end are kept".
    pub fn cut_words(&self) -> String {
        let kept = self.result.chars().count();
        if self.result_chars <= kept {
            return String::new();
        }
        format!(
            "{} characters; its start and end are kept",
            thousands(self.result_chars)
        )
    }
}
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
impl TurnView {
    pub fn sent_html(&self) -> TrustedHtml {
        crate::markdown::excerpt(&self.sent, 220).0
    }
    pub fn said_html(&self) -> TrustedHtml {
        crate::markdown::excerpt(&self.said, 320).0
    }
    /// "12 tool calls · Bash 8 · Edit 3 · Read 1", "No tool calls".
    pub fn calls_words(&self) -> String {
        if self.calls == 0 {
            return "No tool calls".into();
        }
        let mut words = ui::count(self.calls, "tool call", "tool calls");
        if !self.tools.is_empty() {
            words.push_str(" · ");
            words.push_str(&self.tools);
        }
        words
    }
}
impl ActivityView {
    /// The outline, its tab's panel's content (the panel's head names it).
    pub fn render(&self) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&ActivityTemplate { a: self })
    }
    /// "8 turns · 34 tool calls · 2 failed".
    pub fn summary(&self) -> String {
        let mut parts = vec![
            ui::count(self.turns.len() + self.earlier, "turn", "turns"),
            ui::count(self.calls, "tool call", "tool calls"),
        ];
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        parts.join(" · ")
    }
    pub fn all_href(&self) -> String {
        format!("{}?activity=all&tab=activity#activity", self.step_href)
    }
    pub fn latest_href(&self) -> String {
        format!("{}?tab=activity#activity", self.step_href)
    }
}
#[derive(Template)]
#[template(path = "activity.html")]
struct ActivityTemplate<'a> {
    a: &'a ActivityView,
}

/// A run's calls by tool as one meta line: "Bash 42 · Edit 9 · Read 17 · 2 failed", four tools
/// at most and "n other" after them.
pub fn profile_words(outline: &act::Outline) -> String {
    let profile = outline.profile();
    let mut parts: Vec<String> = profile
        .iter()
        .take(4)
        .map(|(tool, n)| format!("{tool} {n}"))
        .collect();
    let other: usize = profile.iter().skip(4).map(|(_, n)| n).sum();
    if other > 0 {
        parts.push(format!("{other} other"));
    }
    let failed = outline.failed();
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    parts.join(" · ")
}
fn tools_words(turn: &act::Turn) -> String {
    let profile = turn.profile();
    let mut parts: Vec<String> = profile
        .iter()
        .take(3)
        .map(|(tool, n)| format!("{tool} {n}"))
        .collect();
    let other: usize = profile.iter().skip(3).map(|(_, n)| n).sum();
    if other > 0 {
        parts.push(format!("{other} other"));
    }
    parts.join(" · ")
}
fn took(started: Option<u64>, ended: Option<u64>) -> String {
    match (started, ended) {
        (Some(s), Some(e)) if e >= s => {
            format!("took {}", ui::duration_text((e - s) as f64 / 1000.0))
        }
        _ => String::new(),
    }
}
fn call_took(call: &act::Call) -> String {
    match (call.started_ms, call.ended_ms) {
        (Some(s), Some(e)) if e >= s => ui::duration_text((e - s) as f64 / 1000.0),
        _ => String::new(),
    }
}

/// The outline as drawn: `live` its run still runs; `fail` the step failed (not cancelled), so
/// its last failed call opens; `all` every turn and call drawn.
pub fn view(
    outline: &act::Outline,
    run: usize,
    live: bool,
    fail: bool,
    all: bool,
    step_href: String,
) -> ActivityView {
    let count = outline.turns.len();
    let failing_at = if fail {
        outline
            .turns
            .iter()
            .enumerate()
            .rev()
            .find_map(|(t, turn)| {
                turn.calls
                    .iter()
                    .rposition(|c| c.outcome == Outcome::Failed)
                    .map(|c| (t, c))
            })
    } else {
        None
    };
    let mut turns = vec![];
    let mut earlier = 0;
    for (t, turn) in outline.turns.iter().enumerate() {
        let recent = t + TURNS_SHOWN >= count;
        if !all && !recent && turn.failed() == 0 {
            earlier += 1;
            continue;
        }
        let running = live && t + 1 == count && !turn.closed;
        let anchor = format!("act-r{run}-t{}", t + 1);
        let calls: Vec<CallView> = turn
            .calls
            .iter()
            .enumerate()
            .map(|(c, call)| {
                let failing = failing_at == Some((t, c));
                CallView {
                    anchor: format!("{anchor}-c{}", c + 1),
                    tool: call.tool.clone(),
                    kind: call.kind,
                    key: call.key.clone(),
                    args: call.args.clone(),
                    result: call.result.clone(),
                    result_chars: call.result_chars,
                    outcome: match call.outcome {
                        Outcome::Done => "done",
                        Outcome::Failed => "failed",
                        Outcome::Running if live => "running",
                        Outcome::Running => "none",
                    },
                    took: call_took(call),
                    open: failing,
                    failing,
                }
            })
            .collect();
        // the latest calls, and every earlier failure
        let shown = calls.len();
        let mut earlier_calls = 0;
        let mut kept = vec![];
        for (c, call) in calls.into_iter().enumerate() {
            if all || c + CALLS_SHOWN >= shown || call.outcome == "failed" {
                kept.push(call);
            } else {
                earlier_calls += 1;
            }
        }
        let open = running || failing_at.is_some_and(|(ft, _)| ft == t) || count == 1;
        turns.push(TurnView {
            n: t + 1,
            anchor,
            sent_kind: match turn.sent.kind {
                SentKind::Task => "Task",
                SentKind::Message => "Message",
                SentKind::Text => "Sent",
                SentKind::Carried => "Carried on",
            },
            sent: if turn.sent.kind == SentKind::Carried {
                "Its session went on from an earlier run.".into()
            } else {
                turn.sent.text.clone()
            },
            said: turn.said.clone(),
            calls: turn.calls.len(),
            failed: turn.failed(),
            tools: tools_words(turn),
            took: if running {
                String::new()
            } else {
                took(turn.started_ms, turn.ended_ms)
            },
            since: if running {
                turn.started_ms.map(act::rfc3339).unwrap_or_default()
            } else {
                String::new()
            },
            running,
            open,
            items: fold_looks(kept),
            earlier_calls,
        });
    }
    let failing = failing_at.map(|(t, c)| {
        let call = &outline.turns[t].calls[c];
        FailingCall {
            anchor: format!("act-r{run}-t{}-c{}", t + 1, c + 1),
            tool: call.tool.clone(),
            key: call.key.clone(),
        }
    });
    ActivityView {
        run,
        engine: outline.engine.name(),
        source: outline.engine.source(),
        live,
        turns,
        earlier,
        all,
        calls: outline.calls().count(),
        failed: outline.failed(),
        skipped: outline.skipped,
        step_href,
        failing,
        raw: None,
    }
}
/// Two or more reads, searches or listings in a row fold into one row ("3 reads, 2 searches");
/// a failed one never folds.
fn fold_looks(calls: Vec<CallView>) -> Vec<Item> {
    let looks = |c: &CallView| c.kind.quiet() && c.outcome != "failed" && !c.failing;
    let mut items = vec![];
    let mut run: Vec<CallView> = vec![];
    let flush = |run: &mut Vec<CallView>, items: &mut Vec<Item>| match run.len() {
        0 => {}
        1 => items.push(Item::Call(run.remove(0))),
        _ => {
            let calls = std::mem::take(run);
            let count = |k: Kind| calls.iter().filter(|c| c.kind == k).count();
            let words: Vec<String> = [
                (Kind::Read, "read", "reads"),
                (Kind::Search, "search", "searches"),
                (Kind::List, "listing", "listings"),
            ]
            .into_iter()
            .filter(|(k, _, _)| count(*k) > 0)
            .map(|(k, one, many)| ui::count(count(k), one, many))
            .collect();
            items.push(Item::Looks {
                words: words.join(", "),
                calls,
            });
        }
    };
    for call in calls {
        if looks(&call) {
            run.push(call);
        } else {
            flush(&mut run, &mut items);
            items.push(Item::Call(call));
        }
    }
    flush(&mut run, &mut items);
    items
}

/// Each agent run's profile (its Runs row) and the latest one's outline (its Activity section),
/// read from their transcripts. `all` draws every turn and call.
pub async fn attach(state: &DashboardState, step: &mut StepView, all: bool) {
    let homes = Homes {
        sluice: state.reads.home().to_owned(),
        claude: state.claude_home.clone(),
    };
    let runs: Vec<(String, Option<u64>, bool)> = step
        .runs
        .iter()
        .map(|r| {
            (
                r.id.to_string(),
                super::timestamp(&r.started).map(|s| s * 1000),
                r.outcome == super::step::Outcome::Running,
            )
        })
        .collect();
    let fail = step.shown() == super::ui::Shown::Failed;
    let href = step.href();
    let runs_dir = state.reads.home().join("runs");
    let read = tokio::task::spawn_blocking(move || {
        let mut profiles = vec![];
        let mut latest = None;
        for (i, (id, since, live)) in runs.iter().enumerate() {
            let until = runs.get(i + 1).and_then(|r| r.1);
            let outline = since.and_then(|since_ms| {
                act::outline(
                    &homes,
                    id,
                    Window {
                        since_ms,
                        until_ms: until,
                    },
                )
            });
            profiles.push(outline.as_deref().map(profile_words).unwrap_or_default());
            if let Some(outline) = outline {
                latest = Some((i, *live, outline));
            }
        }
        // the run going now is an agent's (its engine's record is in its directory), yet no
        // transcript of it could be read here
        let unread = runs.last().is_some_and(|(id, _, live)| {
            *live
                && runs_dir.join(id).join("native.json").is_file()
                && latest.as_ref().is_none_or(|(i, _, _)| *i + 1 != runs.len())
        });
        let view = latest
            .filter(|(i, _, o)| *i + 1 == runs.len() && !o.turns.is_empty())
            .map(|(i, live, o)| view(&o, i + 1, live, fail, all, href));
        (profiles, view, unread)
    })
    .await;
    if let Ok((profiles, mut view, unread)) = read {
        step.transcript_unread = unread;
        for (run, profile) in step.runs.iter_mut().zip(profiles) {
            run.profile = profile;
        }
        if let (Some(view), Some(run)) = (view.as_mut(), step.runs.last()) {
            view.raw = super::step::TRANSCRIPTS
                .iter()
                .find(|name| run.files.contains(name))
                .map(|name| (name.to_string(), run.file_href(&step.project, name)));
        }
        step.activity = view;
    }
}
