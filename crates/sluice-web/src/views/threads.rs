//! Owned conversation projections, loaded with navigation in one SQLite snapshot.
use super::ui::StepRef;
use super::{
    DashboardSnapshot, FunctionCatalog, NavView, TrustedHtml, Viewer, load_snapshot, render_layout,
};
use askama::Template;
use sluice_model::{
    commands::{Message, MessageView},
    error::PublicError,
    ids::ProjectId,
};
use sluice_store::{
    ReadPool,
    messages::{self, QuestionState},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub struct MessageItem {
    pub project: ProjectId,
    pub project_name: String,
    pub message: Message,
    pub body: TrustedHtml,
    pub state: String,
    pub stopped: String,
    pub answer_json: String,
    /// Its body runs long (past 700 characters or 14 lines): a conversation folds it.
    pub long: bool,
    /// Who sent it and to whom, named as a page names them (a step by its title).
    pub from_who: Who,
    pub to_who: Who,
}
impl MessageItem {
    pub fn id(&self) -> i64 {
        self.message.id.0
    }
    /// Its title, else its body's first line: a card never reads just "Question".
    pub fn title(&self) -> String {
        match self
            .message
            .title
            .as_deref()
            .filter(|t| !t.trim().is_empty())
        {
            Some(title) => title.to_owned(),
            None => headline(&self.message.body, 90),
        }
    }
    /// Its body under its title: without its first line when the title is that line whole,
    /// so a question never says its opening twice.
    pub fn shown_body(&self) -> TrustedHtml {
        let body = &self.message.body;
        let mut lines = body.lines().skip_while(|l| l.trim().is_empty());
        let first = lines.next().unwrap_or("");
        let whole = crate::markdown::plain(first);
        // its first line said again: the title it became, or a title that is that line
        let same = |a: &str, b: &str| {
            let key = |t: &str| {
                t.trim()
                    .trim_end_matches(['.', ':', '?', '!'])
                    .to_lowercase()
            };
            key(a) == key(b)
        };
        let repeats = match self
            .message
            .title
            .as_deref()
            .filter(|t| !t.trim().is_empty())
        {
            Some(title) => same(title, &whole),
            None => headline(body, 90) == whole,
        };
        if whole.is_empty() || !repeats {
            return self.body.clone();
        }
        let rest: Vec<&str> = lines.collect();
        crate::markdown::render(&rest.join("\n"))
    }
    /// Open, but whoever asked has stopped: nobody is waiting on the answer.
    pub fn stopped(&self) -> bool {
        !self.stopped.is_empty()
    }
    pub fn recipient(&self) -> &str {
        self.message.to.as_deref().unwrap_or("anyone")
    }
    pub fn ui(&self) -> &str {
        self.message.ui.as_deref().unwrap_or("")
    }
    /// The dashboard speaks as the owner, and a reply goes to the question's sender, so the
    /// owner answers every open question but its own.
    pub fn answerable(&self) -> bool {
        self.state == "open" && self.message.from != "owner"
    }
    pub fn input(&self) -> &str {
        self.message.input.as_deref().unwrap_or("")
    }
    pub fn reply_url(&self) -> String {
        format!("/projects/id/{}/messages/{}/reply", self.project, self.id())
    }
    pub fn thread_url(&self) -> String {
        thread_url(self.project, &self.message.thread)
    }
    /// An open question someone waits on, put to the owner: the one place coral is spent.
    pub fn awaits_owner(&self) -> bool {
        self.state == "open" && self.stopped.is_empty() && self.recipient() == "owner"
    }
    /// Its state as a tag: an open question to the owner "Awaiting your reply" in coral, to
    /// anyone else "Awaiting reply" in gold, else "Answered" or "Closed", muted; a note none.
    pub fn state_tag(&self) -> TrustedHtml {
        match self.state.as_str() {
            "open" if self.awaits_owner() => super::ui::tag("Awaiting your reply", "ask", None),
            "open" => super::ui::tag("Awaiting reply", "attn", None),
            "answered" => super::ui::tag("Answered", "muted", None),
            "closed" => super::ui::tag("Closed", "muted", None),
            _ => TrustedHtml::owned(String::new()),
        }
    }
    /// Itself as a one-item list: a template binds `item` to it to draw its answer form.
    pub fn one(&self) -> &[MessageItem] {
        std::slice::from_ref(self)
    }
}
#[derive(Clone, Debug)]
pub struct ThreadView {
    pub project: ProjectId,
    pub project_name: String,
    pub thread: String,
    /// Who the owner's message on this thread goes to: its step while the step is in
    /// the plan, else the orchestrator.
    pub recipient: String,
    pub messages: Vec<MessageItem>,
    pub through: i64,
    /// Its step's title, while the plan has the step and it has one.
    pub step_title: String,
    /// On its own page, its notes to the owner not marked read yet.
    pub unread: usize,
    /// Its messages as a conversation draws them (`Conversation`).
    pub conversation: Conversation,
}
impl ThreadView {
    pub fn href(&self) -> String {
        thread_url(self.project, &self.thread)
    }
    /// The last message's opening, cut at a word with an ellipsis.
    pub fn preview(&self) -> String {
        self.messages
            .last()
            .map(|m| crate::markdown::cut(&crate::markdown::plain(&m.message.body), 160))
            .unwrap_or_default()
    }
    /// The thread by what it is about: a step's ("Step k2-owner"), the orchestrator's, or a
    /// conversation's first message ("Can the lane land today?").
    pub fn name(&self) -> String {
        if let Some(step) = self.thread.strip_prefix("step-") {
            return if self.step_title.is_empty() {
                format!("Step {step}")
            } else {
                format!("Step: {}", self.step_title)
            };
        }
        if self.thread == sluice_store::messages::ORCHESTRATOR_STREAM {
            return "Orchestrator".into();
        }
        if self.thread == "owner" {
            return "Notes to you".into();
        }
        self.messages
            .first()
            .map(MessageItem::title)
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| self.thread.clone())
    }
    /// When its last message came.
    /// The plan step whose thread this is, while the plan has it.
    pub fn step(&self) -> Option<&str> {
        self.thread
            .strip_prefix("step-")
            .filter(|s| *s == self.recipient)
    }
    /// Its newest message, where "Jump to latest" leads.
    pub fn last_id(&self) -> i64 {
        self.messages.last().map_or(0, MessageItem::id)
    }
    /// Its step as the way back names it: the title (cut to 64), else "step <id>".
    pub fn step_label(&self) -> String {
        match self.step() {
            Some(step) if self.step_title.is_empty() => format!("step {step}"),
            Some(_) => sluice_model::naming::cut(&self.step_title, 64),
            None => String::new(),
        }
    }
    pub fn last_at(&self) -> &str {
        self.messages
            .last()
            .map(|m| m.message.at.as_str())
            .unwrap_or("")
    }
    pub fn read_url(&self) -> String {
        format!("/projects/id/{}/messages/read", self.project)
    }
}
/// A body's first line as a title.
fn headline(body: &str, most: usize) -> String {
    let first = body
        .lines()
        .map(crate::markdown::plain)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    crate::markdown::cut(&first, most)
}
pub fn thread_url(project: ProjectId, thread: &str) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("thread", thread);
    format!("/projects/id/{project}/thread?{}", query.finish())
}
#[derive(Clone, Debug)]
pub struct InboxView {
    pub nav: DashboardSnapshot,
    pub project: Option<ProjectId>,
    pub view: MessageView,
    pub questions: Vec<MessageItem>,
    pub threads: Vec<ThreadView>,
    /// On the inbox, the notes the owner marked read in the last day, by thread: "Read today".
    pub read: Vec<ThreadView>,
}
impl InboxView {
    pub fn title(&self) -> &str {
        match self.view {
            MessageView::Inbox => "Inbox",
            MessageView::Questions => "Questions",
            MessageView::History => "History",
            MessageView::Thread => "Thread",
        }
    }
    /// The tab's title: a thread by what it is about ("Thread · <its step's title>"), a
    /// project's pages with the project's name.
    pub fn page_title(&self) -> String {
        let what = match (&self.view, self.threads.first()) {
            (MessageView::Thread, Some(thread)) => format!(
                "Thread · {}",
                sluice_model::naming::cut(
                    if thread.step_title.is_empty() {
                        thread.name()
                    } else {
                        thread.step_title.clone()
                    }
                    .as_str(),
                    48
                )
            ),
            _ => self.title().to_owned(),
        };
        match self
            .project
            .and_then(|id| self.nav.projects.iter().find(|p| p.id == id))
        {
            Some(project) => format!("{what} · {}", project.name),
            None => what,
        }
    }
    /// The nav section it is under: a project's Messages, or none (the tray is the inbox).
    pub fn tab(&self) -> &str {
        if self.project.is_some() {
            "messages"
        } else {
            ""
        }
    }
    /// The open questions someone is waiting on.
    pub fn waiting(&self) -> Vec<&MessageItem> {
        self.questions.iter().filter(|q| !q.stopped()).collect()
    }
    /// The notes not read yet, across their threads (a thread card says how many it holds).
    pub fn unread_notes(&self) -> usize {
        self.threads.iter().map(|t| t.messages.len()).sum()
    }
    /// The notes read in the last day, across their threads.
    pub fn read_notes(&self) -> usize {
        self.read.iter().map(|t| t.messages.len()).sum()
    }
    /// The open questions someone waits on that are put to the owner: what the nav's Inbox
    /// counts.
    pub fn for_you(&self) -> Vec<&MessageItem> {
        self.waiting()
            .into_iter()
            .filter(|q| q.recipient() == "owner")
            .collect()
    }
    /// The open questions one agent put to another; the owner may answer them too.
    pub fn between_agents(&self) -> Vec<&MessageItem> {
        self.waiting()
            .into_iter()
            .filter(|q| q.recipient() != "owner")
            .collect()
    }
    /// The open questions nobody is waiting on any more.
    pub fn stopped(&self) -> Vec<&MessageItem> {
        self.questions.iter().filter(|q| q.stopped()).collect()
    }
    /// "Close all n": the questions nobody waits on, closed at once once confirmed.
    pub fn close_all(&self, stopped: &[&MessageItem]) -> super::ui::Confirm {
        let mut hidden = vec![("next", self.path())];
        hidden.extend(
            stopped
                .iter()
                .map(|item| ("m", format!("{}/{}", item.project, item.id()))),
        );
        super::ui::Confirm {
            opener: format!("Close all {}", stopped.len()),
            title: format!("Close all {} questions?", stopped.len()),
            id: String::new(),
            action: "/messages/close".into(),
            hidden,
            copy: "Closed, they leave this list and no one can answer them.".into(),
            reason: None,
            confirm: "Close them",
            keep: "Keep them",
        }
    }
    /// The project a project's page is in: its name, for the way back.
    pub fn project_name(&self) -> &str {
        self.project
            .and_then(|id| self.nav.projects.iter().find(|p| p.id == id))
            .map(|p| p.name.as_str())
            .unwrap_or("")
    }
    pub fn base(&self) -> String {
        self.project
            .map(|id| format!("/projects/id/{id}"))
            .unwrap_or_default()
    }
    pub fn path(&self) -> String {
        format!("{}/{}", self.base(), self.title().to_lowercase())
    }
    pub fn version(&self) -> String {
        sluice_store::artifacts::fingerprint(
            format!(
                "{}{:?}{:?}{:?}",
                self.nav.version(),
                self.questions,
                self.threads,
                self.read
            )
            .as_bytes(),
        )
    }
    pub fn body(&self) -> Result<TrustedHtml, PublicError> {
        let html = match self.view {
            MessageView::Inbox | MessageView::Questions => {
                TrustedHtml::from_template(&InboxTemplate { view: self })
            }
            MessageView::History => TrustedHtml::from_template(&HistoryTemplate { view: self }),
            MessageView::Thread => TrustedHtml::from_template(&ThreadTemplate { view: self }),
        };
        html.map_err(render_error)
    }
    pub fn render(
        &self,
        viewer: &Viewer,
        path: &str,
        stream: &str,
    ) -> Result<TrustedHtml, PublicError> {
        let nav = NavView::new(&self.nav, self.project, self.tab())?;
        render_layout(
            &self.page_title(),
            &self.body()?,
            &nav,
            viewer,
            stream,
            &self.version(),
            path,
        )
        .map_err(render_error)
    }
}
#[derive(Template)]
#[template(path = "inbox.html")]
struct InboxTemplate<'a> {
    view: &'a InboxView,
}
#[derive(Template)]
#[template(path = "history.html")]
struct HistoryTemplate<'a> {
    view: &'a InboxView,
}
#[derive(Template)]
#[template(path = "thread.html")]
struct ThreadTemplate<'a> {
    view: &'a InboxView,
}
pub(crate) fn render_error(error: askama::Error) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}

/// A message as a page draws it.
pub fn item(
    sql: &rusqlite::Connection,
    project: ProjectId,
    project_name: &str,
    steps: &BTreeMap<String, StepRef>,
    message: sluice_model::commands::Message,
) -> Result<MessageItem, sluice_store::StoreError> {
    let (state, stopped) = match message.state {
        Some(state) => (
            match state {
                QuestionState::Open => "open",
                QuestionState::Answered => "answered",
                QuestionState::Closed => "closed",
            }
            .into(),
            messages::question(sql, project, message.id)?
                .stopped
                .unwrap_or_default(),
        ),
        None => ("note".into(), String::new()),
    };
    let long = message.body.chars().count() > 700 || message.body.lines().count() > 14;
    Ok(MessageItem {
        project,
        project_name: project_name.to_owned(),
        body: crate::markdown::render(&message.body),
        state,
        stopped,
        answer_json: message
            .answer
            .as_ref()
            .map(serde_json::to_string_pretty)
            .transpose()?
            .unwrap_or_default(),
        long,
        from_who: Who::of(Some(&message.from), None, steps),
        to_who: Who::of(message.to.as_deref(), None, steps),
        message,
    })
}
/// Every step of a project's plan by its id, named as a page names it (its title and stage).
pub fn step_names(
    sql: &rusqlite::Connection,
    project: ProjectId,
) -> Result<BTreeMap<String, StepRef>, sluice_store::StoreError> {
    let names = sluice_runtime::naming::for_project(sql, &super::home_of(sql), project)?;
    let mut q = sql.prepare_cached("SELECT step_id FROM steps WHERE project_id=?1")?;
    let ids = q
        .query_map([project.to_string()], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids
        .into_iter()
        .map(|id| {
            let name = StepRef::new(&id, names.naming.step(&id));
            (id, name)
        })
        .collect())
}
/// The notes to the owner on these messages' threads it has not marked read: past the owner's
/// read mark on each thread.
pub fn unread_ids(
    sql: &rusqlite::Connection,
    project: ProjectId,
    items: &[MessageItem],
) -> Result<BTreeSet<i64>, sluice_store::StoreError> {
    let mut cursors: BTreeMap<&str, i64> = BTreeMap::new();
    let mut unread = BTreeSet::new();
    for m in items
        .iter()
        .filter(|m| m.state == "note" && m.message.to.as_deref() == Some("owner"))
    {
        let thread = m.message.thread.as_str();
        let cursor = match cursors.get(thread) {
            Some(c) => *c,
            None => {
                let c = messages::reader(sql, project, "owner", "owner", thread)?
                    .cursor
                    .0;
                cursors.insert(thread, c);
                c
            }
        };
        if m.id() > cursor {
            unread.insert(m.id());
        }
    }
    Ok(unread)
}

// ---- a conversation: the one way every page draws messages ----------------------------------

/// Who a message is from or to, as a conversation names them: the step the page is about
/// ("This step"), another step (by its title, its id after it), the orchestrator, the owner
/// ("You"), or any other name as itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Who {
    This,
    Step(StepRef),
    Orchestrator,
    Owner,
    Anyone,
    Other(String),
}
impl Who {
    pub fn of(id: Option<&str>, subject: Option<&str>, steps: &BTreeMap<String, StepRef>) -> Self {
        match id {
            None => Who::Anyone,
            Some(id) if Some(id) == subject => Who::This,
            Some("owner") => Who::Owner,
            Some(messages::ORCHESTRATOR_STREAM) => Who::Orchestrator,
            Some(id) => steps
                .get(id)
                .map_or_else(|| Who::Other(id.to_owned()), |s| Who::Step(s.clone())),
        }
    }
    /// Its chip: a step a link to its page (the drawer opens it on the board), its title with
    /// its id after it in data mono.
    pub fn html(&self, project: &ProjectId) -> TrustedHtml {
        use super::ui::esc;
        TrustedHtml::owned(match self {
            Who::This => "<span class=\"who who-this\">This step</span>".into(),
            Who::Owner => "<span class=\"who who-you\">You</span>".into(),
            Who::Orchestrator => "<span class=\"who\">Orchestrator</span>".into(),
            Who::Anyone => "<span class=\"who\">Anyone</span>".into(),
            Who::Other(name) => format!("<span class=\"who\">{}</span>", esc(name)),
            Who::Step(step) => format!(
                "<span class=\"who who-step\">{}</span>",
                step.link(
                    &format!("/projects/id/{project}/steps/{}", step.id),
                    48,
                    true
                )
            ),
        })
    }
    fn key(&self) -> String {
        match self {
            Who::This => "\u{0}this".into(),
            Who::Step(s) => s.id.clone(),
            Who::Orchestrator => messages::ORCHESTRATOR_STREAM.into(),
            Who::Owner => "owner".into(),
            Who::Anyone => "\u{0}anyone".into(),
            Who::Other(name) => name.clone(),
        }
    }
}
/// One message in a conversation: its sender and recipient named, the replies to it (a
/// question's answer sits under it), and whether it is drawn whole or as its first words.
#[derive(Clone, Debug)]
pub struct Entry {
    pub item: MessageItem,
    pub from: Who,
    pub to: Who,
    /// Its recipient differs from its group's first: its own line says it.
    pub to_differs: bool,
    pub replies: Vec<Reply>,
    /// Its first 360 characters only, with a link to it whole (the step's Overview).
    pub excerpt: bool,
    /// Where it is drawn whole, for "Read it in the thread".
    pub href: String,
}
impl Entry {
    pub fn ask(&self) -> bool {
        self.item.message.is_question()
    }
    /// Its first words as one line of inline HTML, code spans kept, and whether that cut it.
    pub fn excerpt_html(&self) -> (TrustedHtml, bool) {
        crate::markdown::excerpt(&self.item.message.body, 360)
    }
    /// A reply to a message this conversation does not hold: its id, said in its line.
    pub fn reply_to(&self) -> Option<i64> {
        self.item.message.to_message.map(|m| m.0)
    }
}
#[derive(Clone, Debug)]
pub struct Reply {
    pub item: MessageItem,
    pub from: Who,
}
/// What a conversation lists: a day's start, the first message not read yet, or the messages
/// one sender sent in a row.
#[derive(Clone, Debug)]
pub enum Row {
    Day(String),
    New,
    Group(Group),
}
#[derive(Clone, Debug)]
pub struct Group {
    pub from: Who,
    pub to: Who,
    /// "out" from the step the page is about, "in" to it, "you" the owner's, else "other".
    pub tone: &'static str,
    pub entries: Vec<Entry>,
}
/// Messages as every page draws them (the step's Thread tab and Overview, a thread's page and
/// the inbox's notes): one column, consecutive messages from one sender grouped under who sent
/// them and to whom, a line at each day's start and at the first message not read yet, a
/// question's replies directly under it.
#[derive(Clone, Debug, Default)]
pub struct Conversation {
    pub project: ProjectId,
    pub rows: Vec<Row>,
    /// Messages drawn, replies too.
    pub count: usize,
    /// The newest message drawn, where "Jump to latest" leads.
    pub last: i64,
    /// Earlier messages left out, and where they are all drawn.
    pub earlier: usize,
    pub whole_href: String,
    /// It says how many it holds and offers "Jump to latest".
    pub head: bool,
    /// The box to write to its step or the orchestrator, at its end.
    pub composer: Option<Composer>,
}
/// A conversation's message box: to whom it writes, and where it posts.
#[derive(Clone, Debug)]
pub struct Composer {
    pub project: ProjectId,
    pub to: String,
    /// Its recipient in words ("Message to fig-5492-work").
    pub label: String,
}
/// How a conversation is built: the step it is about, the steps' names, what is unread, how
/// many messages at most (the latest kept), and whether each is its first words only.
pub struct Build<'a> {
    pub project: ProjectId,
    pub subject: Option<&'a str>,
    pub steps: &'a BTreeMap<String, StepRef>,
    pub unread: &'a BTreeSet<i64>,
    pub most: Option<usize>,
    pub excerpt: bool,
    /// Where an excerpt's "Read it in the thread" leads: before the message's id (`#message-N`
    /// on the same page, else the thread's page).
    pub href: &'a dyn Fn(&MessageItem) -> String,
}
impl Conversation {
    pub fn build(b: Build<'_>, items: Vec<MessageItem>) -> Self {
        let ids: BTreeSet<i64> = items.iter().map(MessageItem::id).collect();
        let questions: BTreeSet<i64> = items
            .iter()
            .filter(|m| m.message.is_question())
            .map(MessageItem::id)
            .collect();
        // a reply to a question here sits under it; any other message stands in its order
        let mut replies: BTreeMap<i64, Vec<MessageItem>> = BTreeMap::new();
        let mut top = vec![];
        for item in items {
            match item.message.to_message.map(|m| m.0) {
                Some(q) if questions.contains(&q) && ids.contains(&q) && !b.excerpt => {
                    replies.entry(q).or_default().push(item)
                }
                _ => top.push(item),
            }
        }
        let earlier = b.most.map_or(0, |most| top.len().saturating_sub(most));
        let top: Vec<MessageItem> = top.into_iter().skip(earlier).collect();
        let mut c = Conversation {
            project: b.project,
            ..Conversation::default()
        };
        c.earlier = earlier;
        let mut day = String::new();
        let mut new_said = false;
        for item in top {
            let replies: Vec<Reply> = replies
                .remove(&item.id())
                .unwrap_or_default()
                .into_iter()
                .map(|r| Reply {
                    from: Who::of(Some(&r.message.from), b.subject, b.steps),
                    item: r,
                })
                .collect();
            let unread = b.unread.contains(&item.id())
                || replies.iter().any(|r| b.unread.contains(&r.item.id()));
            c.count += 1 + replies.len();
            c.last = c
                .last
                .max(item.id())
                .max(replies.iter().map(|r| r.item.id()).max().unwrap_or(0));
            let mut fresh = false;
            let this_day = item.message.at.get(..10).unwrap_or("").to_owned();
            if this_day != day && !b.excerpt {
                c.rows.push(Row::Day(day_words(&this_day)));
                day = this_day;
                fresh = true;
            }
            if unread && !new_said {
                c.rows.push(Row::New);
                new_said = true;
                fresh = true;
            }
            let from = Who::of(Some(&item.message.from), b.subject, b.steps);
            let to = Who::of(item.message.to.as_deref(), b.subject, b.steps);
            let href = (b.href)(&item);
            let mut entry = Entry {
                item,
                from: from.clone(),
                to: to.clone(),
                to_differs: false,
                replies,
                excerpt: b.excerpt,
                href,
            };
            match c.rows.last_mut() {
                Some(Row::Group(g)) if !fresh && g.from.key() == from.key() => {
                    entry.to_differs = g.to.key() != to.key();
                    g.entries.push(entry);
                }
                _ => c.rows.push(Row::Group(Group {
                    tone: match (&from, &to) {
                        (Who::This, _) => "out",
                        (Who::Owner, _) => "you",
                        (_, Who::This) => "in",
                        _ => "other",
                    },
                    from,
                    to,
                    entries: vec![entry],
                })),
            }
        }
        c
    }
    /// With "n messages · Jump to latest" over it, past two messages.
    pub fn with_head(mut self, whole_href: String) -> Self {
        self.head = true;
        self.whole_href = whole_href;
        self
    }
    pub fn with_composer(mut self, composer: Option<Composer>) -> Self {
        self.composer = composer;
        self
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn render(&self) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&ConversationTemplate { c: self })
    }
}
#[derive(Template)]
#[template(path = "conversation.html")]
struct ConversationTemplate<'a> {
    c: &'a Conversation,
}
/// A day as a conversation's separator says it: "Thu 8 Oct 2026" (UTC, as every stored time).
fn day_words(day: &str) -> String {
    let parts: Vec<i64> = day.split('-').filter_map(|p| p.parse().ok()).collect();
    let [y, m, d] = parts[..] else {
        return day.to_owned();
    };
    // days since 1970-01-01 (Howard Hinnant's days_from_civil), 1970-01-01 a Thursday
    let (yy, mm) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    const WEEK: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let Some(month) = usize::try_from(m - 1).ok().and_then(|i| MONTHS.get(i)) else {
        return day.to_owned();
    };
    format!("{} {d} {month} {y}", WEEK[days.rem_euclid(7) as usize])
}
/// Messages gathered into their threads, the thread with the newest first.
fn group(
    sql: &rusqlite::Connection,
    groups: &mut BTreeMap<(String, String), ThreadView>,
    p: &super::ProjectView,
    item: MessageItem,
) -> Result<(), sluice_store::StoreError> {
    let key = (p.id.to_string(), item.message.thread.clone());
    if !groups.contains_key(&key) {
        let step = item.message.thread.strip_prefix("step-");
        let in_plan: bool = sql.query_row(
            "SELECT EXISTS(SELECT 1 FROM steps WHERE project_id=?1 AND step_id=?2)",
            (p.id.to_string(), step.unwrap_or("")),
            |r| r.get(0),
        )?;
        groups.insert(
            key.clone(),
            ThreadView {
                project: p.id,
                project_name: p.name.clone(),
                thread: item.message.thread.clone(),
                recipient: step
                    .filter(|_| in_plan)
                    .unwrap_or(messages::ORCHESTRATOR_STREAM)
                    .into(),
                messages: vec![],
                through: 0,
                unread: 0,
                step_title: match step.filter(|_| in_plan) {
                    Some(step) => {
                        let names =
                            sluice_runtime::naming::for_project(sql, &super::home_of(sql), p.id)?;
                        names
                            .naming
                            .step(step)
                            .map(|n| n.title.clone())
                            .unwrap_or_default()
                    }
                    None => String::new(),
                },
                conversation: Conversation::default(),
            },
        );
    }
    let group = groups.get_mut(&key).expect("thread group");
    group.through = group.through.max(item.id());
    group.messages.push(item);
    Ok(())
}
/// Each thread's messages drawn as a conversation: on a thread's own page about its step (while
/// the plan has it), with the first unread note marked, its head and its message box; in the
/// inbox as they are.
fn converse(
    sql: &rusqlite::Connection,
    threads: &mut [ThreadView],
    page: bool,
) -> Result<(), sluice_store::StoreError> {
    let mut names: BTreeMap<ProjectId, BTreeMap<String, StepRef>> = BTreeMap::new();
    for thread in threads {
        let steps = match names.entry(thread.project) {
            std::collections::btree_map::Entry::Occupied(known) => known.into_mut(),
            std::collections::btree_map::Entry::Vacant(new) => {
                new.insert(step_names(sql, thread.project)?)
            }
        };
        let unread = if page {
            unread_ids(sql, thread.project, &thread.messages)?
        } else {
            BTreeSet::new()
        };
        thread.unread = unread.len();
        let subject = if page {
            thread.step().map(str::to_owned)
        } else {
            None
        };
        let conversation = Conversation::build(
            Build {
                project: thread.project,
                subject: subject.as_deref(),
                steps,
                unread: &unread,
                most: None,
                excerpt: false,
                href: &|m| format!("#message-{}", m.id()),
            },
            thread.messages.clone(),
        );
        thread.conversation = if page {
            let label = match thread.step() {
                Some(_) if !thread.step_title.is_empty() => {
                    format!(
                        "Message to {}",
                        sluice_model::naming::cut(&thread.step_title, 48)
                    )
                }
                _ => format!("Message to {}", thread.recipient),
            };
            conversation
                .with_head(thread.href())
                .with_composer(Some(Composer {
                    project: thread.project,
                    to: thread.recipient.clone(),
                    label,
                }))
        } else {
            conversation
        };
    }
    Ok(())
}
fn threads_of(groups: BTreeMap<(String, String), ThreadView>) -> Vec<ThreadView> {
    let mut threads: Vec<ThreadView> = groups.into_values().collect();
    threads.sort_by_key(|t| std::cmp::Reverse(t.through));
    threads
}
pub async fn load(
    reads: &ReadPool,
    project: Option<ProjectId>,
    view: MessageView,
    thread: Option<String>,
) -> Result<InboxView, PublicError> {
    reads
        .snapshot(move |sql| {
            let nav = load_snapshot(sql, FunctionCatalog::default())?;
            if let Some(id) = project {
                messages::resolve_project(sql, &sluice_model::ids::ProjectSelector::Id(id))?;
            }
            // "today": the last day, so a note read last night is still there in the morning
            let since = super::rfc3339(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
                    .saturating_sub(86_400),
            );
            let mut questions = vec![];
            let mut groups = BTreeMap::new();
            let mut read = BTreeMap::new();
            for p in nav
                .projects
                .iter()
                .filter(|p| project.is_none_or(|id| id == p.id))
            {
                let selected =
                    messages::messages(sql, p.id, view.clone(), thread.as_deref(), None, "owner")?;
                let read_today = if matches!(view, MessageView::Inbox) {
                    messages::read_notes(sql, p.id, "owner", &since)?
                } else {
                    vec![]
                };
                if selected.is_empty() && read_today.is_empty() {
                    continue;
                }
                let steps = step_names(sql, p.id)?;
                for message in selected {
                    let item = item(sql, p.id, &p.name, &steps, message)?;
                    if item.state == "open"
                        && matches!(view, MessageView::Inbox | MessageView::Questions)
                    {
                        questions.push(item);
                    } else {
                        group(sql, &mut groups, p, item)?;
                    }
                }
                for message in read_today {
                    let item = item(sql, p.id, &p.name, &steps, message)?;
                    group(sql, &mut read, p, item)?;
                }
            }
            questions.sort_by_key(MessageItem::id);
            let mut threads = threads_of(groups);
            let mut read = threads_of(read);
            match view {
                MessageView::Thread => converse(sql, &mut threads, true)?,
                MessageView::Inbox => {
                    converse(sql, &mut threads, false)?;
                    converse(sql, &mut read, false)?;
                }
                MessageView::Questions | MessageView::History => {}
            }
            Ok(InboxView {
                nav,
                project,
                view,
                questions,
                threads,
                read,
            })
        })
        .await
        .map_err(|e| e.into_public(true))
}
