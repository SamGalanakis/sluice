//! Owned conversation projections, loaded with navigation in one SQLite snapshot.
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
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct MessageItem {
    pub project: ProjectId,
    pub project_name: String,
    pub message: Message,
    pub body: TrustedHtml,
    pub state: String,
    pub stopped: String,
    pub sender_class: String,
    pub answer_json: String,
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
        let titled = self
            .message
            .title
            .as_deref()
            .is_some_and(|t| !t.trim().is_empty());
        let body = &self.message.body;
        let mut lines = body.lines().skip_while(|l| l.trim().is_empty());
        let first = lines.next().unwrap_or("");
        let whole = crate::markdown::plain(first);
        if titled || whole.is_empty() || headline(body, 90) != whole {
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
    /// The tab's title: a thread by its name, a project's pages with the project's name.
    pub fn page_title(&self) -> String {
        let what = match (&self.view, self.threads.first()) {
            (MessageView::Thread, Some(thread)) => thread.name(),
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
                "{}{:?}{:?}",
                self.nav.version(),
                self.questions,
                self.threads
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
            let mut questions = vec![];
            let mut groups = BTreeMap::new();
            for p in nav
                .projects
                .iter()
                .filter(|p| project.is_none_or(|id| id == p.id))
            {
                let selected =
                    messages::messages(sql, p.id, view.clone(), thread.as_deref(), None, "owner")?;
                for message in selected {
                    let (state, stopped) = match message.state {
                        Some(state) => (
                            match state {
                                QuestionState::Open => "open",
                                QuestionState::Answered => "answered",
                                QuestionState::Closed => "closed",
                            }
                            .into(),
                            messages::question(sql, p.id, message.id)?
                                .stopped
                                .unwrap_or_default(),
                        ),
                        None => ("note".into(), String::new()),
                    };
                    let is_step: bool = sql.query_row(
                        "SELECT EXISTS(SELECT 1 FROM steps WHERE project_id=?1 AND step_id=?2)",
                        (p.id.to_string(), &message.from),
                        |r| r.get(0),
                    )?;
                    let item = MessageItem {
                        project: p.id,
                        project_name: p.name.clone(),
                        body: crate::markdown::render(&message.body),
                        state,
                        stopped,
                        sender_class: if is_step {
                            "step"
                        } else if message.from == "owner" {
                            "owner"
                        } else {
                            "lead"
                        }
                        .into(),
                        answer_json: message
                            .answer
                            .as_ref()
                            .map(serde_json::to_string_pretty)
                            .transpose()?
                            .unwrap_or_default(),
                        message,
                    };
                    if item.state == "open"
                        && matches!(view, MessageView::Inbox | MessageView::Questions)
                    {
                        questions.push(item);
                    } else {
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
                                    step_title: match step.filter(|_| in_plan) {
                                        Some(step) => {
                                            let names = sluice_runtime::naming::for_project(
                                                sql,
                                                &super::home_of(sql),
                                                p.id,
                                            )?;
                                            names
                                                .naming
                                                .step(step)
                                                .map(|n| n.title.clone())
                                                .unwrap_or_default()
                                        }
                                        None => String::new(),
                                    },
                                },
                            );
                        }
                        let group = groups.get_mut(&key).expect("thread group");
                        group.through = group.through.max(item.id());
                        group.messages.push(item);
                    }
                }
            }
            questions.sort_by_key(MessageItem::id);
            let mut threads: Vec<ThreadView> = groups.into_values().collect();
            threads.sort_by_key(|t| std::cmp::Reverse(t.through));
            Ok(InboxView {
                nav,
                project,
                view,
                questions,
                threads,
            })
        })
        .await
        .map_err(|e| e.into_public(true))
}
