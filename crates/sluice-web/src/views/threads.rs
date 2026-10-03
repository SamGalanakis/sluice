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
    pub fn title(&self) -> &str {
        self.message.title.as_deref().unwrap_or("Question")
    }
    pub fn recipient(&self) -> &str {
        self.message.to.as_deref().unwrap_or("anyone")
    }
    pub fn ui(&self) -> &str {
        self.message.ui.as_deref().unwrap_or("")
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
    pub messages: Vec<MessageItem>,
    pub through: i64,
}
impl ThreadView {
    pub fn href(&self) -> String {
        thread_url(self.project, &self.thread)
    }
    pub fn preview(&self) -> String {
        self.messages
            .last()
            .map(|m| m.message.body.chars().take(160).collect())
            .unwrap_or_default()
    }
    pub fn read_url(&self) -> String {
        format!("/projects/id/{}/messages/read", self.project)
    }
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
        let nav = NavView::new(&self.nav, self.project, &self.title().to_lowercase())?;
        render_layout(
            self.title(),
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
            let nav = load_snapshot(sql, FunctionCatalog::default(), false)?;
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
                    let (state, stopped) = if message.needs_reply {
                        let q = messages::question(sql, p.id, message.id)?;
                        (
                            match q.state {
                                QuestionState::Open => "open",
                                QuestionState::Answered => "answered",
                                QuestionState::Closed => "closed",
                            }
                            .into(),
                            q.stopped.unwrap_or_default(),
                        )
                    } else {
                        ("note".into(), String::new())
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
                        sender_class: if is_step { "step" } else { "lead" }.into(),
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
                        let group = groups.entry(key).or_insert_with(|| ThreadView {
                            project: p.id,
                            project_name: p.name.clone(),
                            thread: item.message.thread.clone(),
                            messages: vec![],
                            through: 0,
                        });
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
