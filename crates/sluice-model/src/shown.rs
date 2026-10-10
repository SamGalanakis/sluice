//! How a step reads to a person: the one table every surface draws a status from (SPEC §13,
//! DESIGN.md's status ramp). A step is stored as one of six statuses (`StepStatus`, kept as a
//! string); what a page shows is that status named more closely from a few facts read beside it
//! (a cancel is a failed step whose error says so, a quiet run is a running one that has written
//! nothing past its cadence, …). `classify` names it, `Shown::spec` says how it is drawn: its
//! glyph, word, lane mark, tone, band and whether it needs attention.
//!
//! The declaration order is the one priority: the state that most needs someone first. A unit, a
//! project, a matrix row, the index's order and every count read in it; `Shown`'s `Ord` is that
//! order, so the state that stands for many steps is their `min()`. Every match here names every
//! state (no catch-all arm): adding a state fails the build until each surface says how it reads.
#![deny(clippy::wildcard_enum_match_arm)]

use crate::{commands::StepStatus, error::PublicError};
use serde::{Deserialize, Serialize};

/// A step as a page shows it, in priority order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shown {
    /// Failed, not by a cancel.
    Failed,
    /// Failed because its owner (or an agent for them) cancelled it.
    Cancelled,
    /// Succeeded, but a step it reads changed since.
    Stale,
    /// Running, and its run has written nothing past its cadence: maybe stuck.
    Quiet,
    /// Pending behind a step that failed, was cancelled or went stale.
    Blocked,
    /// Running, and a cancel was asked for: its run is stopping.
    Stopping,
    /// Running, and its run has submitted: only finishing (SPEC §6.4).
    Finishing,
    Running,
    /// Pending, ready, and done outside sluice (`core.external`).
    External,
    /// Pending, held by its own pause or its project's.
    Paused,
    /// Pending, reading a plan input that has no value.
    Held,
    /// Pending and ready, waiting for a resource.
    Queued,
    Pending,
    /// Succeeded with its outputs set by hand.
    Manual,
    Succeeded,
    Skipped,
}

/// Where Live first draws a unit: under its first state's band.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Band {
    /// Stopped: a step failed, was cancelled or went stale, or is held up by one.
    Stopped,
    /// Running: a step runs (quiet, stopping or finishing too) or is done outside sluice.
    Running,
    /// Waiting: nothing of it runs yet.
    Waiting,
    /// Done: the shelf.
    Done,
}
impl Band {
    /// The band's label on the board.
    pub const fn label(self) -> &'static str {
        match self {
            Band::Stopped => "Stopped",
            Band::Running => "Running",
            Band::Waiting => "Waiting",
            Band::Done => "Done",
        }
    }
}

/// The colour a state is drawn in: a token of the dashboard's status ramp (`g-<key>` and
/// `b-<key>` in style.css take it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// Ink, the loudest: a failure (never coral).
    Ink,
    /// Muted ink: a stop on purpose.
    Muted,
    /// Sand: look at it.
    Attention,
    /// The channel's blue: work going on.
    Active,
    /// Muted ink: a hold someone chose.
    Paused,
    /// Idle navy: nothing to do yet.
    Idle,
    /// Sky: done well.
    Success,
}
/// How one state is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    /// Its name on the wire and in CSS: `g-{key}`, `is-{key}`, `b-{key}`; `Count`'s `of`.
    pub key: &'static str,
    /// Its one word: a glyph's accessible name, a caption, a tally ("set by hand").
    pub word: &'static str,
    /// Its glyph, a Lucide icon by Lucide's name (the Shape Carries It Rule).
    pub icon: &'static str,
    /// The glyph turns: the state is work in motion.
    pub turns: bool,
    /// Its mark in a lane string (`fork✓ work▶ land·`): one alphabet for the board, the Units
    /// table and `status`.
    pub lane: char,
    pub tone: Tone,
    pub band: Band,
    /// It needs someone to look: Show: Attention, a unit's alarm, the tab title's counts.
    pub attention: bool,
    /// A card says its word as its caption.
    pub caption: bool,
    /// What its caption means, for the caption's title (no legend: each says itself).
    pub help: &'static str,
}

impl Shown {
    /// Every state, in priority order.
    pub const ALL: [Shown; 16] = [
        Shown::Failed,
        Shown::Cancelled,
        Shown::Stale,
        Shown::Quiet,
        Shown::Blocked,
        Shown::Stopping,
        Shown::Finishing,
        Shown::Running,
        Shown::External,
        Shown::Paused,
        Shown::Held,
        Shown::Queued,
        Shown::Pending,
        Shown::Manual,
        Shown::Succeeded,
        Shown::Skipped,
    ];

    /// How it is drawn: the one table.
    pub const fn spec(self) -> Spec {
        match self {
            Shown::Failed => Spec {
                key: "failed",
                word: "failed",
                icon: "circle-x",
                turns: false,
                lane: '✗',
                tone: Tone::Ink,
                band: Band::Stopped,
                attention: true,
                caption: true,
                help: "Its last run failed; Retry runs it again",
            },
            Shown::Cancelled => Spec {
                key: "cancelled",
                word: "cancelled",
                icon: "circle-stop",
                turns: false,
                lane: '■',
                tone: Tone::Muted,
                band: Band::Stopped,
                attention: true,
                caption: true,
                help: "Cancelled by its owner; Retry runs it again",
            },
            Shown::Stale => Spec {
                key: "stale",
                word: "stale",
                icon: "rotate-cw",
                turns: false,
                lane: '~',
                tone: Tone::Attention,
                band: Band::Stopped,
                attention: true,
                caption: false,
                help: "A step it reads changed since it ran; Retry runs it again",
            },
            Shown::Quiet => Spec {
                key: "quiet",
                word: "quiet",
                icon: "hourglass",
                turns: false,
                lane: '◔',
                tone: Tone::Attention,
                band: Band::Running,
                attention: true,
                caption: true,
                help: "Running, but its run has written nothing for a while: the time since it last did",
            },
            Shown::Blocked => Spec {
                key: "blocked",
                word: "blocked",
                icon: "circle-minus",
                turns: false,
                lane: '⊖',
                tone: Tone::Ink,
                band: Band::Stopped,
                attention: false,
                caption: true,
                help: "Waits on a step that failed, was cancelled or went stale",
            },
            Shown::Stopping => Spec {
                key: "stopping",
                word: "stopping",
                icon: "circle-stop",
                turns: true,
                lane: '□',
                tone: Tone::Muted,
                band: Band::Running,
                attention: false,
                caption: true,
                help: "A cancel was asked for: its run is stopping",
            },
            Shown::Finishing => Spec {
                key: "finishing",
                word: "finishing",
                icon: "loader-circle",
                turns: true,
                lane: '▷',
                tone: Tone::Active,
                band: Band::Running,
                attention: false,
                caption: true,
                help: "Its agent submitted; its run is ending",
            },
            Shown::Running => Spec {
                key: "running",
                word: "running",
                icon: "loader-circle",
                turns: true,
                lane: '▶',
                tone: Tone::Active,
                band: Band::Running,
                attention: false,
                caption: false,
                help: "Running",
            },
            Shown::External => Spec {
                key: "external",
                word: "outside",
                icon: "square-arrow-out-up-right",
                turns: false,
                lane: '↗',
                tone: Tone::Active,
                band: Band::Running,
                attention: false,
                caption: true,
                help: "Done outside sluice: set its outputs when the work lands",
            },
            Shown::Paused => Spec {
                key: "paused",
                word: "paused",
                icon: "circle-pause",
                turns: false,
                lane: '‖',
                tone: Tone::Paused,
                band: Band::Waiting,
                attention: false,
                caption: false,
                help: "Held by a pause, its own or its project's",
            },
            Shown::Held => Spec {
                key: "held",
                word: "held",
                icon: "circle-dot-dashed",
                turns: false,
                lane: '∅',
                tone: Tone::Idle,
                band: Band::Waiting,
                attention: false,
                caption: true,
                help: "Reads a plan input that has no value yet",
            },
            Shown::Queued => Spec {
                key: "queued",
                word: "queued",
                icon: "circle-ellipsis",
                turns: false,
                lane: '≡',
                tone: Tone::Idle,
                band: Band::Waiting,
                attention: false,
                caption: true,
                help: "Ready, waiting for a resource to free up",
            },
            Shown::Pending => Spec {
                key: "pending",
                word: "pending",
                icon: "circle-dashed",
                turns: false,
                lane: '·',
                tone: Tone::Idle,
                band: Band::Waiting,
                attention: false,
                caption: false,
                help: "Waits for what it runs after",
            },
            Shown::Manual => Spec {
                key: "manual",
                word: "set by hand",
                icon: "circle-dot",
                turns: false,
                lane: '⊙',
                tone: Tone::Success,
                band: Band::Done,
                attention: false,
                caption: false,
                help: "Its outputs were set by hand",
            },
            Shown::Succeeded => Spec {
                key: "succeeded",
                word: "succeeded",
                icon: "circle-check",
                turns: false,
                lane: '✓',
                tone: Tone::Success,
                band: Band::Done,
                attention: false,
                caption: false,
                help: "Its last run succeeded",
            },
            Shown::Skipped => Spec {
                key: "skipped",
                word: "skipped",
                icon: "circle-slash",
                turns: false,
                lane: '–',
                tone: Tone::Idle,
                band: Band::Done,
                attention: false,
                caption: false,
                help: "Skipped: what it runs after said not to run it",
            },
        }
    }
    pub const fn key(self) -> &'static str {
        self.spec().key
    }
    pub const fn word(self) -> &'static str {
        self.spec().word
    }
    /// Its place in the priority order, 0 first.
    pub const fn rank(self) -> usize {
        self as usize
    }
    /// The state a key names (`Count`'s `of`, a page's query): none for a key no state has.
    pub fn from_key(key: &str) -> Option<Shown> {
        Shown::ALL.into_iter().find(|s| s.key() == key)
    }
    /// The stored status it is a name on.
    pub const fn stored(self) -> StepStatus {
        match self {
            Shown::Failed | Shown::Cancelled => StepStatus::Failed,
            Shown::Stale => StepStatus::Stale,
            Shown::Quiet | Shown::Stopping | Shown::Finishing | Shown::Running => {
                StepStatus::Running
            }
            Shown::Blocked
            | Shown::External
            | Shown::Paused
            | Shown::Held
            | Shown::Queued
            | Shown::Pending => StepStatus::Pending,
            Shown::Manual | Shown::Succeeded => StepStatus::Succeeded,
            Shown::Skipped => StepStatus::Skipped,
        }
    }
}

/// What a step is classified from, each fact read once, after its run has been observed.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    pub status: StepStatus,
    /// Failed by a cancel (`is_cancel` of its stored error).
    pub cancelled: bool,
    /// Held by its own pause or its project's.
    pub paused: bool,
    /// Ready `core.external` work.
    pub external: bool,
    /// Behind a step that failed or went stale (cancels are failures to the store).
    pub blocked: bool,
    /// Reads a plan input with no value.
    pub held: bool,
    /// Ready but short of a resource.
    pub queued: bool,
    /// Its run has written nothing past its cadence.
    pub quiet: bool,
    /// Its run has submitted.
    pub finishing: bool,
    /// A cancel was asked for and its run has not ended.
    pub stopping: bool,
    /// Its outputs were set by hand.
    pub manual: bool,
}
impl Facts {
    /// The stored status and nothing more known about it.
    pub fn of(status: StepStatus) -> Self {
        Self {
            status,
            cancelled: false,
            paused: false,
            external: false,
            blocked: false,
            held: false,
            queued: false,
            quiet: false,
            finishing: false,
            stopping: false,
            manual: false,
        }
    }
}

/// The state a step reads as, from its stored status and the facts beside it.
pub fn classify(f: &Facts) -> Shown {
    match f.status {
        StepStatus::Failed if f.cancelled => Shown::Cancelled,
        StepStatus::Failed => Shown::Failed,
        StepStatus::Stale => Shown::Stale,
        StepStatus::Running if f.stopping => Shown::Stopping,
        StepStatus::Running if f.finishing => Shown::Finishing,
        StepStatus::Running if f.quiet => Shown::Quiet,
        StepStatus::Running => Shown::Running,
        StepStatus::Pending if f.paused => Shown::Paused,
        StepStatus::Pending if f.external => Shown::External,
        StepStatus::Pending if f.blocked => Shown::Blocked,
        StepStatus::Pending if f.held => Shown::Held,
        StepStatus::Pending if f.queued => Shown::Queued,
        StepStatus::Pending => Shown::Pending,
        StepStatus::Succeeded if f.manual => Shown::Manual,
        StepStatus::Succeeded => Shown::Succeeded,
        StepStatus::Skipped => Shown::Skipped,
    }
}

/// Steps counted by how each reads: the one count every summary, bar, `Count` and tab title
/// reads. Each step is counted once, under its state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally([usize; Shown::ALL.len()]);
impl Tally {
    pub fn add(&mut self, shown: Shown) {
        self.0[shown.rank()] += 1;
    }
    pub fn get(&self, shown: Shown) -> usize {
        self.0[shown.rank()]
    }
    /// Without `n` of the steps counted under `shown` (as many as it has).
    pub fn without(mut self, shown: Shown, n: usize) -> Self {
        let at = shown.rank();
        self.0[at] = self.0[at].saturating_sub(n);
        self
    }
    pub fn total(&self) -> usize {
        self.0.iter().sum()
    }
    /// The state that stands for them all: the first one present in priority order.
    pub fn first(&self) -> Option<Shown> {
        Shown::ALL.into_iter().find(|s| self.get(*s) > 0)
    }
    /// Each state present and its count, in priority order.
    pub fn iter(&self) -> impl Iterator<Item = (Shown, usize)> + '_ {
        Shown::ALL
            .into_iter()
            .map(|s| (s, self.get(s)))
            .filter(|(_, n)| *n > 0)
    }
    /// The steps in states that need attention.
    pub fn attention(&self) -> usize {
        self.iter()
            .filter(|(s, _)| s.spec().attention)
            .map(|(_, n)| n)
            .sum()
    }
    /// The steps whose stored status is `status`.
    pub fn stored(&self, status: &StepStatus) -> usize {
        self.iter()
            .filter(|(s, _)| s.stored() == *status)
            .map(|(_, n)| n)
            .sum()
    }
}
impl FromIterator<Shown> for Tally {
    fn from_iter<I: IntoIterator<Item = Shown>>(states: I) -> Self {
        let mut tally = Tally::default();
        for state in states {
            tally.add(state);
        }
        tally
    }
}
impl std::ops::AddAssign<&Tally> for Tally {
    fn add_assign(&mut self, other: &Tally) {
        for (mine, theirs) in self.0.iter_mut().zip(other.0) {
            *mine += theirs;
        }
    }
}

/// A stored error is a cancel: its kind `cancelled`, an agent's `Cancelled`, or a fn's message in
/// the cancel's own words (`cancelled: <reason>`, which `step_cancel` documents). It stays
/// `failed` in the store and every tool; the dashboard names it apart.
pub fn is_cancel(error: &PublicError) -> bool {
    matches!(error, PublicError::Cancelled { .. })
        || matches!(error, PublicError::AgentFailure { kind, .. } if kind == "Cancelled")
        || matches!(error, PublicError::FnFailure { message } if cancel_reason(message).is_some())
}
/// A stored error (its JSON, or a bare message from an older release) is a cancel.
pub fn stored_is_cancel(stored: &str) -> bool {
    match serde_json::from_str::<PublicError>(stored) {
        Ok(error) => is_cancel(&error),
        Err(_) => cancel_reason(stored).is_some(),
    }
}
/// A fn's message in a cancel's words: its reason.
pub fn cancel_reason(message: &str) -> Option<&str> {
    message
        .strip_prefix("cancelled: ")
        .or_else(|| (message == "cancelled").then_some(""))
}
/// The words that join a failed step's cancel to the failure it set aside (`set_aside`).
pub const SET_ASIDE: &str = "it had failed: ";
/// The words a cancel of a failed step keeps, so the failure it set aside stays readable
/// wherever the cancel is (the step's error, its `step.cancel` record and its run's kept
/// cancel): "<reason> (it had failed: <what>)", or "it had failed: <what>" with no reason.
/// `what` is the failure's kind and the first line of its message, cut to 200 characters.
pub fn set_aside(reason: &str, failure: &PublicError) -> String {
    // an agent's failure by its own kind, any other by the error's
    let value = serde_json::to_value(failure).unwrap_or_default();
    let kind = match (value["error"].as_str(), value["kind"].as_str()) {
        (Some("agent_failure"), Some(kind)) => kind,
        (Some(error), _) => error,
        (None, _) => "failed",
    }
    .to_owned();
    let message = value["message"].as_str().unwrap_or_default();
    let line = message
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut what = if line.is_empty() {
        kind
    } else {
        format!("{kind}: {line}")
    };
    if what.chars().count() > 200 {
        what = what.chars().take(199).collect::<String>() + "…";
    }
    let reason = reason.trim();
    if reason.is_empty() {
        format!("{SET_ASIDE}{what}")
    } else {
        format!("{reason} ({SET_ASIDE}{what})")
    }
}
/// A cancel's words apart from the failure it set aside (`set_aside`): its own reason, and
/// the failure's kind and first line when it kept one.
pub fn split_set_aside(words: &str) -> (&str, Option<&str>) {
    if let Some(what) = words.strip_prefix(SET_ASIDE) {
        return ("", Some(what));
    }
    match words.rfind(&format!(" ({SET_ASIDE}")) {
        Some(at) if words.ends_with(')') => (
            &words[..at],
            Some(&words[at + 2 + SET_ASIDE.len()..words.len() - 1]),
        ),
        _ => (words, None),
    }
}
/// `is_cancel` as an SQLite expression over the error at `path` in the JSON `doc` (a record's
/// payload): the same rule, so the log's Errors filter and the pages never disagree. True (1) or
/// false (0), never null.
pub fn cancel_sql(doc: &str, path: &str) -> String {
    let at = |field: &str| format!("json_extract({doc},'{path}{field}')");
    let worded =
        |text: String| format!("({text}='cancelled' OR substr({text},1,11)='cancelled: ')");
    format!(
        "coalesce(({e}='cancelled' OR ({e}='agent_failure' AND {kind}='Cancelled') OR ({e}='fn_failure' AND {fn_words}) OR (json_type({doc},'{path}')='text' AND {bare})),0)",
        e = at(".error"),
        kind = at(".kind"),
        fn_words = worded(at(".message")),
        bare = worded(at("")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_in_declaration_order_and_each_key_word_and_lane_is_its_own() {
        for (i, state) in Shown::ALL.into_iter().enumerate() {
            assert_eq!(state.rank(), i, "{state:?}");
            assert_eq!(Shown::from_key(state.key()), Some(state));
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::json!(state.key())
            );
        }
        let keys: std::collections::BTreeSet<_> = Shown::ALL.map(Shown::key).into();
        let words: std::collections::BTreeSet<_> = Shown::ALL.map(Shown::word).into();
        let lanes: std::collections::BTreeSet<_> = Shown::ALL.map(|s| s.spec().lane).into();
        assert_eq!(keys.len(), Shown::ALL.len());
        assert_eq!(words.len(), Shown::ALL.len());
        assert_eq!(lanes.len(), Shown::ALL.len());
        assert_eq!(Shown::from_key("unknown"), None);
    }

    #[test]
    fn a_state_is_classified_from_its_status_and_the_facts_beside_it() {
        let shown = |status: StepStatus, set: fn(&mut Facts)| {
            let mut facts = Facts::of(status);
            set(&mut facts);
            classify(&facts)
        };
        assert_eq!(
            shown(StepStatus::Failed, |f| f.cancelled = true),
            Shown::Cancelled
        );
        assert_eq!(shown(StepStatus::Failed, |_| {}), Shown::Failed);
        assert_eq!(
            shown(StepStatus::Running, |f| {
                f.stopping = true;
                f.quiet = true;
            }),
            Shown::Stopping
        );
        assert_eq!(
            shown(StepStatus::Running, |f| {
                f.finishing = true;
                f.quiet = true;
            }),
            Shown::Finishing
        );
        assert_eq!(shown(StepStatus::Running, |f| f.quiet = true), Shown::Quiet);
        assert_eq!(
            shown(StepStatus::Pending, |f| {
                f.paused = true;
                f.blocked = true;
            }),
            Shown::Paused
        );
        assert_eq!(
            shown(StepStatus::Pending, |f| f.blocked = true),
            Shown::Blocked
        );
        assert_eq!(shown(StepStatus::Pending, |f| f.held = true), Shown::Held);
        assert_eq!(
            shown(StepStatus::Pending, |f| f.queued = true),
            Shown::Queued
        );
        assert_eq!(
            shown(StepStatus::Succeeded, |f| f.manual = true),
            Shown::Manual
        );
        // a fact that does not apply to the status changes nothing
        assert_eq!(
            shown(StepStatus::Skipped, |f| f.quiet = true),
            Shown::Skipped
        );
        for state in Shown::ALL {
            assert_eq!(
                classify(&Facts::of(state.stored())).stored(),
                state.stored()
            );
        }
    }

    #[test]
    fn a_tally_stands_for_its_steps_by_the_first_state_present() {
        let tally: Tally = [Shown::Running, Shown::Failed, Shown::Quiet, Shown::Running]
            .into_iter()
            .collect();
        assert_eq!(tally.first(), Some(Shown::Failed));
        assert_eq!(tally.total(), 4);
        assert_eq!(tally.attention(), 2);
        assert_eq!(tally.stored(&StepStatus::Running), 3);
        assert_eq!(
            tally.iter().collect::<Vec<_>>(),
            [(Shown::Failed, 1), (Shown::Quiet, 1), (Shown::Running, 2)]
        );
        assert_eq!(Tally::default().first(), None);
    }

    #[test]
    fn a_cancel_is_read_from_its_stored_error_in_every_form() {
        for (stored, cancel) in [
            (
                r#"{"error":"cancelled","message":"cancel requested"}"#,
                true,
            ),
            (
                r#"{"error":"agent_failure","kind":"Cancelled","message":"x"}"#,
                true,
            ),
            (
                r#"{"error":"agent_failure","kind":"WallCap","message":"x"}"#,
                false,
            ),
            (
                r#"{"error":"fn_failure","message":"cancelled: not needed"}"#,
                true,
            ),
            (
                r#"{"error":"fn_failure","message":"Cancelled: shouting"}"#,
                false,
            ),
            (r#"{"error":"fn_failure","message":"exit code 1"}"#, false),
            ("cancelled", true),
            ("cancelled: by hand", true),
            ("tests failed", false),
        ] {
            assert_eq!(stored_is_cancel(stored), cancel, "{stored}");
        }
    }

    #[test]
    fn a_failed_steps_cancel_keeps_the_failure_it_set_aside() {
        let failure = PublicError::AgentFailure {
            kind: "EngineExited".into(),
            message: "\nclaude: transcript record exceeds 1 MiB\npane at failure".into(),
            session: None,
        };
        let words = set_aside("superseded by the new lane", &failure);
        assert_eq!(
            words,
            "superseded by the new lane (it had failed: EngineExited: claude: transcript record exceeds 1 MiB)"
        );
        assert_eq!(
            split_set_aside(&words),
            (
                "superseded by the new lane",
                Some("EngineExited: claude: transcript record exceeds 1 MiB")
            )
        );
        let bare = set_aside(
            " ",
            &PublicError::FnFailure {
                message: "boom".into(),
            },
        );
        assert_eq!(bare, "it had failed: fn_failure: boom");
        assert_eq!(split_set_aside(&bare), ("", Some("fn_failure: boom")));
        assert_eq!(split_set_aside("pivot (later)"), ("pivot (later)", None));
        let long = set_aside(
            "",
            &PublicError::FnFailure {
                message: "x".repeat(400),
            },
        );
        assert!(long.chars().count() < 240 && long.ends_with('…'), "{long}");
    }
}
