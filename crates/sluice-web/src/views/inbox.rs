//! Message HTTP routes. Mutations cross the injected application command service.
use super::{DashboardState, Viewer, threads};
use axum::{
    Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response, Sse, sse::KeepAlive},
    routing::{get, post},
};
use serde::Deserialize;
use sluice_model::{
    RuntimeApi,
    commands::{
        Ask, CommandReply, CommandRequest, MarkRead, MessageAnswer, MessageView, Reply, Say,
    },
    error::PublicError,
    ids::{MessageId, ProjectId, ProjectSelector},
};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

pub type CommandFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CommandReply, PublicError>> + Send + 'a>>;
pub trait MessageCommands: Send + Sync {
    fn command(&self, request: CommandRequest) -> CommandFuture<'_>;
}
impl<T: RuntimeApi> MessageCommands for T {
    fn command(&self, request: CommandRequest) -> CommandFuture<'_> {
        Box::pin(RuntimeApi::command(self, request))
    }
}
#[derive(Clone)]
pub struct MessageState {
    pub dashboard: DashboardState,
    pub commands: Arc<dyn MessageCommands>,
}
pub fn router(state: MessageState) -> Router {
    Router::new()
        .route("/inbox", get(global_page))
        .route("/questions", get(global_page))
        .route("/history", get(global_page))
        .route("/inbox/stream", get(global_stream))
        .route("/questions/stream", get(global_stream))
        .route("/history/stream", get(global_stream))
        .route("/projects/id/{project}/inbox", get(project_page))
        .route("/projects/id/{project}/questions", get(project_page))
        .route("/projects/id/{project}/history", get(project_page))
        .route("/projects/id/{project}/thread", get(project_page))
        .route("/projects/id/{project}/inbox/stream", get(project_stream))
        .route(
            "/projects/id/{project}/questions/stream",
            get(project_stream),
        )
        .route("/projects/id/{project}/history/stream", get(project_stream))
        .route("/projects/id/{project}/thread/stream", get(project_stream))
        .route(
            "/projects/id/{project}/messages/{message}/reply",
            post(reply),
        )
        .route("/projects/id/{project}/messages/read", post(mark_read))
        .route("/projects/id/{project}/messages", post(post_message))
        .with_state(state)
}
#[derive(Clone, Deserialize, Default)]
pub struct MessageQuery {
    pub thread: Option<String>,
    pub datastar: Option<String>,
}
fn view(path: &str) -> MessageView {
    match path.trim_end_matches("/stream").rsplit('/').next() {
        Some("questions") => MessageView::Questions,
        Some("history") => MessageView::History,
        Some("thread") => MessageView::Thread,
        _ => MessageView::Inbox,
    }
}
async fn global_page(
    State(state): State<MessageState>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    Query(query): Query<MessageQuery>,
    headers: HeaderMap,
) -> Response {
    page(state, None, uri.path().into(), query, headers).await
}
async fn project_page(
    State(state): State<MessageState>,
    Path(project): Path<ProjectId>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    Query(query): Query<MessageQuery>,
    headers: HeaderMap,
) -> Response {
    page(state, Some(project), uri.path().into(), query, headers).await
}
async fn page(
    state: MessageState,
    project: Option<ProjectId>,
    path: String,
    query: MessageQuery,
    headers: HeaderMap,
) -> Response {
    let mut stream = format!("{path}/stream");
    if let Some(thread) = &query.thread {
        stream.push('?');
        stream.push_str(
            &url::form_urlencoded::Serializer::new(String::new())
                .append_pair("thread", thread)
                .finish(),
        );
    }
    match threads::load(&state.dashboard.reads, project, view(&path), query.thread)
        .await
        .and_then(|page| page.render(&Viewer::from_headers(&headers), &path, &stream))
    {
        Ok(html) => Html(html.as_str().to_owned()).into_response(),
        Err(error) => error_response(error),
    }
}
async fn global_stream(
    State(state): State<MessageState>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    Query(query): Query<MessageQuery>,
    headers: HeaderMap,
) -> Response {
    stream(state, None, uri.path().into(), query, headers).await
}
async fn project_stream(
    State(state): State<MessageState>,
    Path(project): Path<ProjectId>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    Query(query): Query<MessageQuery>,
    headers: HeaderMap,
) -> Response {
    stream(state, Some(project), uri.path().into(), query, headers).await
}
async fn stream(
    state: MessageState,
    project: Option<ProjectId>,
    path: String,
    query: MessageQuery,
    headers: HeaderMap,
) -> Response {
    use crate::streams::{PatchRegion, RenderedBatch, VersionSignal, page_events};
    let version = query
        .datastar
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("ver").and_then(|v| v.as_str()).map(str::to_owned))
        .unwrap_or_default();
    let stop = state.dashboard.stop.clone();
    let viewer = Viewer::from_headers(&headers);
    let events = page_events(
        move || {
            let state = state.clone();
            let path = path.clone();
            let viewer = viewer.clone();
            let query = query.clone();
            async move {
                let page =
                    threads::load(&state.dashboard.reads, project, view(&path), query.thread)
                        .await?;
                let nav = super::NavView::new(&page.nav, project, &page.title().to_lowercase())?;
                Ok(Some(RenderedBatch {
                    version: page.version(),
                    regions: vec![
                        PatchRegion::new("messages-view", page.body()?),
                        PatchRegion::new(
                            "top-nav",
                            super::render_nav(&nav, &viewer, path.trim_end_matches("/stream"))
                                .map_err(threads::render_error)?,
                        ),
                    ],
                }))
            }
        },
        version,
        VersionSignal::Page,
        stop,
    );
    Sse::new(events)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}
pub(crate) fn error_response(error: PublicError) -> Response {
    let status = match &error {
        PublicError::BadRequest { .. } => StatusCode::BAD_REQUEST,
        PublicError::NotFound { .. } => StatusCode::NOT_FOUND,
        PublicError::Conflict { .. } | PublicError::CursorExpired { .. } => StatusCode::CONFLICT,
        PublicError::Invalid { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        PublicError::Busy { .. } => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, axum::Json(error)).into_response()
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ReplyBody {
    #[serde(default)]
    body: String,
    answer: Option<MessageAnswer>,
    action: Option<String>,
    /// A new message's recipient: a step, or the orchestrator.
    to: Option<String>,
    /// A new message is a question (ask) rather than a note (say).
    #[serde(default)]
    ask: bool,
}
fn decode(headers: &HeaderMap, body: &Bytes) -> Result<ReplyBody, PublicError> {
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next() == Some("application/json"))
    {
        serde_json::from_slice(body).map_err(|e| PublicError::BadRequest {
            message: e.to_string(),
        })
    } else {
        let mut result = ReplyBody::default();
        for (key, value) in url::form_urlencoded::parse(body) {
            match key.as_ref() {
                "body" => result.body = value.into(),
                "action" => result.action = Some(value.into()),
                "to" => result.to = Some(value.into()),
                "ask" => result.ask = value == "true",
                _ => {}
            }
        }
        Ok(result)
    }
}
async fn reply(
    State(state): State<MessageState>,
    Path((project, message)): Path<(ProjectId, i64)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    mutate(state, project, Some(MessageId(message)), headers, body).await
}
async fn post_message(
    State(state): State<MessageState>,
    Path(project): Path<ProjectId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    mutate(state, project, None, headers, body).await
}
async fn mutate(
    state: MessageState,
    project: ProjectId,
    reply_to: Option<MessageId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let result = async {
        let mut fields = decode(&headers, &body)?;
        if let Some(action) = fields.action {
            if fields.answer.is_some() {
                return Err(PublicError::BadRequest {
                    message: "give answer or action".into(),
                });
            }
            fields.answer = Some(MessageAnswer {
                action,
                params: None,
                values: None,
            });
        }
        if fields.body.trim().is_empty() && fields.answer.is_none() {
            return Err(PublicError::Invalid {
                message: "a reply needs a body or action".into(),
                errors: vec![],
            });
        }
        let project = ProjectSelector::Id(project);
        // The dashboard speaks as the owner: answering is a reply, composing an ask or say.
        let command = match (reply_to, fields.to) {
            (Some(to_message), _) => CommandRequest::Reply(Reply {
                project,
                to_message,
                body: fields.body,
                answer: fields.answer,
                run: None,
                owner: true,
            }),
            (None, _) if fields.answer.is_some() => {
                return Err(PublicError::BadRequest {
                    message: "an answer replies to a question".into(),
                });
            }
            (None, None) => {
                return Err(PublicError::Invalid {
                    message: "to is required: a step of the plan or orchestrator".into(),
                    errors: vec![],
                });
            }
            (None, Some(to)) if fields.ask => CommandRequest::Ask(Ask {
                project,
                to,
                body: fields.body,
                title: None,
                ui: None,
                input: None,
                data: None,
                run: None,
                owner: true,
            }),
            (None, Some(to)) => CommandRequest::Say(Say {
                project,
                to,
                body: fields.body,
                data: None,
                run: None,
                owner: true,
            }),
        };
        state.commands.command(command).await
    }
    .await;
    match result {
        Ok(reply) => {
            if headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("application/json"))
            {
                axum::Json(reply).into_response()
            } else {
                axum::response::Redirect::to(&format!("/projects/id/{project}/history"))
                    .into_response()
            }
        }
        Err(error) => error_response(error),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadBody {
    thread: String,
    through: i64,
}
async fn mark_read(
    State(state): State<MessageState>,
    Path(project): Path<ProjectId>,
    axum::Json(body): axum::Json<ReadBody>,
) -> Response {
    if body.through < 0 {
        return error_response(PublicError::BadRequest {
            message: "read watermark must be nonnegative".into(),
        });
    }
    match state
        .commands
        .command(CommandRequest::MarkRead(MarkRead {
            project,
            identity: "owner".into(),
            thread: body.thread,
            through: MessageId(body.through),
        }))
        .await
    {
        Ok(reply) => axum::Json(reply).into_response(),
        Err(error) => error_response(error),
    }
}

pub fn registration() -> super::PageRegistration {
    use super::{Asset, NavEntry, PageRegistration};
    PageRegistration {
        routes: |state| state.messages.clone().map(router).unwrap_or_default(),
        nav: |project| {
            project
                .map(|id| {
                    vec![
                        NavEntry::new("inbox", format!("/projects/id/{id}/inbox"), "Inbox", 20),
                        NavEntry::new(
                            "questions",
                            format!("/projects/id/{id}/questions"),
                            "Questions",
                            30,
                        ),
                        NavEntry::new(
                            "history",
                            format!("/projects/id/{id}/history"),
                            "History",
                            50,
                        ),
                    ]
                })
                .unwrap_or_default()
        },
        assets: &[
            Asset {
                names: &["inbox.js"],
                media_type: "text/javascript",
                bytes: include_bytes!("../../assets/inbox.js"),
            },
            Asset {
                names: &["openui.js"],
                media_type: "text/javascript",
                bytes: include_bytes!("../../assets/openui.js"),
            },
            Asset {
                names: &["lang-core-0.3.0.js"],
                media_type: "text/javascript",
                bytes: include_bytes!("../../assets/lang-core-0.3.0.js"),
            },
            Asset {
                names: &["zod-4.6.5-v4-core.js"],
                media_type: "text/javascript",
                bytes: include_bytes!("../../assets/zod-4.6.5-v4-core.js"),
            },
            Asset {
                names: &["zod-4.6.5-v4.js"],
                media_type: "text/javascript",
                bytes: include_bytes!("../../assets/zod-4.6.5-v4.js"),
            },
        ],
    }
}
