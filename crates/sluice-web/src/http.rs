//! Loopback HTTP policy, page registration and Unix-command adapters.
use crate::{
    mcp::{self, McpServer},
    settings,
    views::{self, PageState, board, step},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::post,
};
use futures_util::{StreamExt, future::BoxFuture};
use serde_json::Value;
use sluice_model::{commands::*, error::PublicError, ids::*, plan::FnSignature};
use sluice_runtime::client::CoordinatorClient;
use sluice_store::{ReadPool, projects};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub const MAX_BODY: usize = 1024 * 1024;
const REQUEST_ID: &str = "x-request-id";
#[derive(Clone)]
struct HttpState {
    pages: PageState,
    server: McpServer,
    stop: CancellationToken,
    permits: Arc<tokio::sync::Semaphore>,
}

/// Page owners register through PageState. All routes, including settings and MCP,
/// inherit the same Host/Origin, body, deadline, error and shutdown policy.
pub fn router(
    pages: PageState,
    commands: Arc<dyn mcp::CommandService>,
    stop: CancellationToken,
) -> Router {
    let server = McpServer::new(commands);
    let state = HttpState {
        pages: pages.clone(),
        server: server.clone(),
        stop: stop.clone(),
        permits: Arc::new(tokio::sync::Semaphore::new(64)),
    };
    views::page_router(pages)
        .route("/api/tools/{name}", post(tool).with_state(state.clone()))
        .nest_service("/mcp", mcp::http_service(server, stop))
        .fallback(|| async {
            error_response(PublicError::NotFound {
                message: "route not found".into(),
            })
        })
        .method_not_allowed_fallback(|| async {
            status_error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
        })
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(middleware::from_fn_with_state(state, policy))
}
pub fn error_response(error: PublicError) -> Response {
    let status = match &error {
        PublicError::BadRequest { .. } | PublicError::Invalid { .. } => StatusCode::BAD_REQUEST,
        PublicError::NotFound { .. } => StatusCode::NOT_FOUND,
        PublicError::Conflict { .. } | PublicError::CursorExpired { .. } => StatusCode::CONFLICT,
        PublicError::Busy { .. } => StatusCode::SERVICE_UNAVAILABLE,
        PublicError::Cancelled { .. } => StatusCode::REQUEST_TIMEOUT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, axum::Json(error)).into_response()
}
fn status_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        axum::Json(PublicError::BadRequest {
            message: message.into(),
        }),
    )
        .into_response()
}
fn loopback_host(headers: &HeaderMap) -> Option<(String, u16)> {
    if headers.get_all(header::HOST).iter().count() != 1 {
        return None;
    }
    let authority = headers
        .get(header::HOST)?
        .to_str()
        .ok()?
        .parse::<axum::http::uri::Authority>()
        .ok()?;
    if authority.as_str().contains('@')
        || authority.as_str() != authority.host() && authority.port_u16().is_none()
    {
        return None;
    }
    let host = authority.host().to_ascii_lowercase();
    if !matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]") {
        return None;
    }
    Some((host, authority.port_u16().unwrap_or(80)))
}
/// Missing Origin is valid for native clients; a present one must be this origin.
pub fn valid_origin(headers: &HeaderMap) -> bool {
    let Some((host, port)) = loopback_host(headers) else {
        return false;
    };
    if !headers.contains_key(header::ORIGIN) {
        return true;
    }
    if headers.get_all(header::ORIGIN).iter().count() != 1 {
        return false;
    }
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| url::Url::parse(s).ok())
    else {
        return false;
    };
    origin.scheme() == "http"
        && origin
            .host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case(&host))
        && origin.port_or_known_default() == Some(port)
        && origin.username().is_empty()
        && origin.password().is_none()
        && origin.path() == "/"
        && origin.query().is_none()
        && origin.fragment().is_none()
}
async fn policy(State(state): State<HttpState>, mut request: Request, next: Next) -> Response {
    let id = uuid::Uuid::now_v7().to_string();
    request
        .headers_mut()
        .insert(REQUEST_ID, HeaderValue::from_str(&id).expect("uuid header"));
    let mut response = handle_policy(&state, request, next).await;
    response
        .headers_mut()
        .insert(REQUEST_ID, HeaderValue::from_str(&id).expect("uuid header"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    let permit = response
        .extensions_mut()
        .remove::<Arc<tokio::sync::OwnedSemaphorePermit>>();
    let (parts, body) = response.into_parts();
    let stop = state.stop.clone();
    let stream = body
        .into_data_stream()
        .take_until(stop.cancelled_owned())
        .map(move |chunk| {
            let _keep_permit = &permit;
            chunk
        });
    Response::from_parts(parts, Body::from_stream(stream))
}
async fn handle_policy(state: &HttpState, request: Request, next: Next) -> Response {
    if loopback_host(request.headers()).is_none() {
        return status_error(StatusCode::FORBIDDEN, "Host must name a loopback address");
    }
    let is_mcp = request.uri().path() == "/mcp" || request.uri().path().starts_with("/mcp/");
    let write = !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    if (write || is_mcp) && !valid_origin(request.headers()) {
        return status_error(StatusCode::FORBIDDEN, "Origin must match the loopback Host");
    }
    let Ok(permit) = state.permits.clone().try_acquire_owned() else {
        return error_response(PublicError::Busy {
            message: "HTTP request limit reached".into(),
            retryable: !write,
        });
    };
    if request.method() == Method::GET
        && let Some(response) =
            name_redirect(state, request.uri().path(), request.uri().query()).await
    {
        return response;
    }
    let (parts, body) = request.into_parts();
    let bytes = match tokio::time::timeout(Duration::from_secs(10), to_bytes(body, MAX_BODY)).await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => {
            return status_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body exceeds 1 MiB or could not be read",
            );
        }
        Err(_) => {
            return status_error(
                StatusCode::REQUEST_TIMEOUT,
                "request body deadline exceeded",
            );
        }
    };
    let budget = if is_mcp || parts.uri.path().starts_with("/api/tools/") {
        mcp::WAIT_CAP * 2 + 30
    } else {
        30
    };
    let settings_response =
        parts.uri.path().starts_with("/projects/id/") && parts.uri.path().ends_with("/settings");
    let request = Request::from_parts(parts, Body::from(bytes));
    let mut response =
        match tokio::time::timeout(Duration::from_secs(budget), next.run(request)).await {
            Ok(response) => response,
            Err(_) => {
                return error_response(PublicError::Busy {
                    message: "HTTP response deadline exceeded".into(),
                    retryable: false,
                });
            }
        };
    if (response.status().is_client_error() || response.status().is_server_error())
        && !response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|v| {
                v.as_bytes().starts_with(b"application/json")
                    || (settings_response && v.as_bytes().starts_with(b"text/html"))
            })
    {
        let status = response.status();
        let (parts, body) = response.into_parts();
        let text = to_bytes(body, 64 * 1024)
            .await
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_else(|| status.to_string());
        let mut error = if status == StatusCode::NOT_FOUND {
            error_response(PublicError::NotFound { message: text })
        } else if status.is_server_error() {
            error_response(PublicError::Storage { message: text })
        } else {
            status_error(status, text)
        };
        *error.headers_mut() = parts.headers;
        error.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        response = error;
    }
    response.extensions_mut().insert(Arc::new(permit));
    response
}
async fn name_redirect(state: &HttpState, path: &str, query: Option<&str>) -> Option<Response> {
    let tail = path.strip_prefix("/projects/")?;
    let (name, suffix) = tail.split_once('/').unwrap_or((tail, ""));
    if name == "id" {
        return None;
    }
    let name = url::form_urlencoded::parse(format!("name={name}").as_bytes())
        .next()?
        .1
        .into_owned();
    let selector = match name.parse::<ProjectName>() {
        Ok(name) => ProjectSelector::Name(name),
        Err(_) => {
            return Some(error_response(PublicError::NotFound {
                message: "project not found".into(),
            }));
        }
    };
    let result = state
        .pages
        .dashboard
        .reads
        .snapshot(move |sql| projects::resolve(sql, &selector))
        .await;
    Some(match result {
        Ok(project) => {
            let mut target = format!("/projects/id/{}", project.project_id);
            if !suffix.is_empty() {
                target.push('/');
                target.push_str(suffix);
            }
            if let Some(query) = query {
                target.push('?');
                target.push_str(query);
            }
            Redirect::temporary(&target).into_response()
        }
        Err(error) => error_response(error.into_public(true)),
    })
}
async fn tool(
    State(state): State<HttpState>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    if !request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|s| {
            s.split(';')
                .next()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return status_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/json",
        );
    }
    let bytes = match to_bytes(request.into_body(), MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => return status_error(StatusCode::PAYLOAD_TOO_LARGE, "request body exceeds 1 MiB"),
    };
    let args: sluice_model::rpc::JsonMap = match sluice_model::rpc::decode_json(&bytes) {
        Ok(args) => args,
        Err(error) => return error_response(error),
    };
    let value = serde_json::to_value(args)
        .expect("arguments")
        .as_object()
        .expect("argument map")
        .clone();
    let result = state.server.call(&name, value, Some("http")).await;
    if result.is_error == Some(true) {
        let error: PublicError =
            serde_json::from_value(result.structured_content.expect("structured public error"))
                .expect("public error");
        error_response(error)
    } else {
        let text = result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|c| c.text.clone())
            .unwrap_or_default();
        match serde_json::from_str::<Value>(&text) {
            Ok(value) => axum::Json(value).into_response(),
            Err(_) => text.into_response(),
        }
    }
}

#[derive(Clone)]
pub struct SocketSettings {
    pub client: CoordinatorClient,
    pub reads: ReadPool,
}
impl settings::SettingsCommands for SocketSettings {
    fn update(
        &self,
        id: ProjectId,
        request: projects::UpdateProject,
    ) -> BoxFuture<'_, Result<projects::Project, PublicError>> {
        Box::pin(async move {
            self.client
                .command(CommandRequest::ProjectUpdate(ProjectUpdate {
                    project: ProjectSelector::Id(id),
                    new_name: request.new_name,
                    description: request.description,
                    icon: request.icon.map(Into::into),
                    resources: request
                        .resources
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|e| PublicError::BadRequest {
                            message: e.to_string(),
                        })?,
                    paused: request.paused,
                    archived: request.archived,
                    expected_settings_rev: request.expected_settings_rev,
                    reason: request.reason,
                    author: Some(request.author),
                }))
                .await?;
            self.reads
                .snapshot(move |sql| projects::resolve(sql, &ProjectSelector::Id(id)))
                .await
                .map_err(|e| e.into_public(true))
        })
    }
    fn delete(
        &self,
        id: ProjectId,
        request: projects::DeleteProject,
    ) -> BoxFuture<'_, Result<(), PublicError>> {
        Box::pin(async move {
            self.client
                .command(CommandRequest::ProjectDelete(ProjectDelete {
                    project: ProjectSelector::Id(id),
                    confirm_name: request.confirm_name.parse().map_err(|e| {
                        PublicError::BadRequest {
                            message: format!("{e}"),
                        }
                    })?,
                    expected_settings_rev: request.expected_settings_rev,
                    author: Some(request.author),
                }))
                .await?;
            Ok(())
        })
    }
}
/// The broker must enforce this revision inside its single command transaction.
#[derive(Clone)]
pub struct SocketOwnerCommands(pub CoordinatorClient);
impl step::CommandService for SocketOwnerCommands {
    fn execute(&self, command: step::OwnerCommand) -> BoxFuture<'_, Result<(), PublicError>> {
        Box::pin(async move {
            let project = ProjectSelector::Id(command.project);
            let selection = StepSelection {
                steps: command.step.map(|step| vec![step]),
                tags: None,
            };
            let edit = EditOptions {
                expected: Some(Revision(command.revision)),
                dry_run: false,
                reason: command.message.clone(),
                author: Some(command.author.into()),
            };
            let request = match command.action {
                step::Action::Pause | step::Action::Unpause => {
                    CommandRequest::StepPause(StepPause {
                        project,
                        selection,
                        subtree: false,
                        paused: command.action == step::Action::Pause,
                        edit,
                    })
                }
                step::Action::Retry => CommandRequest::StepRetry(StepRetry {
                    expected_rev: Some(Revision(command.revision)),
                    project,
                    selection,
                    message: (!command.message.is_empty()).then_some(command.message),
                    reason: None,
                    author: Some(command.author.into()),
                }),
                step::Action::Cancel => CommandRequest::StepCancel(StepCancel {
                    expected_rev: Some(Revision(command.revision)),
                    project,
                    selection,
                    reason: command.message,
                    author: Some(command.author.into()),
                }),
            };
            self.0.command(request).await?;
            Ok(())
        })
    }
}

#[derive(Clone, Default)]
pub struct SocketCatalog(Arc<RwLock<BTreeMap<Option<ProjectId>, CachedCatalog>>>);
#[derive(Clone, Default)]
struct CachedCatalog {
    view: views::FunctionCatalog,
    signatures: Vec<(String, FnSignature)>,
}
impl views::CatalogSource for SocketCatalog {
    fn catalog(&self, project: Option<ProjectId>) -> Result<views::FunctionCatalog, PublicError> {
        Ok(self
            .0
            .read()
            .map_err(|_| PublicError::Storage {
                message: "catalog cache lock poisoned".into(),
            })?
            .get(&project)
            .cloned()
            .unwrap_or_default()
            .view)
    }
}
impl board::RegistrySource for SocketCatalog {
    fn signatures(&self, project: ProjectId) -> Result<board::RegistrySnapshot, PublicError> {
        let cache = self
            .0
            .read()
            .map_err(|_| PublicError::Storage {
                message: "catalog cache lock poisoned".into(),
            })?
            .get(&Some(project))
            .cloned()
            .unwrap_or_default();
        Ok(board::RegistrySnapshot {
            version: cache.view.version,
            functions: cache.signatures,
        })
    }
}
impl SocketCatalog {
    pub async fn refresh(&self, client: &CoordinatorClient) -> Result<(), PublicError> {
        let projects = mcp::reply_value(client.command(CommandRequest::ProjectsList).await?)?;
        let mut selectors = vec![None];
        if let Some(projects) = projects.as_array() {
            for project in projects {
                if let Some(id) = project.get("project_id").and_then(Value::as_str) {
                    selectors.push(Some(id.parse().map_err(|e| PublicError::Storage {
                        message: format!("{e}"),
                    })?));
                }
            }
        }
        let mut updated = BTreeMap::new();
        for project in selectors {
            let value = mcp::reply_value(
                client
                    .command(CommandRequest::FnList {
                        project: project.map(ProjectSelector::Id),
                    })
                    .await?,
            )?;
            let entries = value
                .as_array()
                .or_else(|| value.get("result").and_then(Value::as_array))
                .ok_or_else(|| PublicError::Storage {
                    message: "fn_list returned no array".into(),
                })?;
            let mut cache = CachedCatalog::default();
            cache.view.version = sluice_store::artifacts::fingerprint(value.to_string().as_bytes());
            for entry in entries {
                let name = entry["name"].as_str().unwrap_or_default().to_owned();
                let signature = FnSignature {
                    inputs: parse_declarations(&entry["inputs"])?
                        .into_iter()
                        .map(|(name, decl)| (name, decl.ty))
                        .collect(),
                    outputs: parse_declarations(&entry["outputs"])?
                        .into_iter()
                        .map(|(name, decl)| (name, decl.ty))
                        .collect(),
                    submits: parse_declarations(&entry["submits"])?.into_iter().collect(),
                    open: entry["open"].as_bool().unwrap_or(false),
                };
                let ports = |map: &Value| {
                    map.as_object()
                        .into_iter()
                        .flat_map(|map| map.iter())
                        .map(|(name, ty)| views::PortView {
                            name: name.clone(),
                            ty: ty
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| ty.to_string()),
                        })
                        .collect()
                };
                cache.view.entries.push(views::FunctionView {
                    name: name.clone(),
                    scope: entry["scope"].as_str().unwrap_or("global").into(),
                    doc: entry["doc"].as_str().unwrap_or_default().into(),
                    inputs: ports(&serde_json::to_value(&signature.inputs).expect("port types")),
                    outputs: ports(&serde_json::to_value(&signature.outputs).expect("port types")),
                    error: entry["error"].as_str().unwrap_or_default().into(),
                });
                if entry["error"].as_str().is_none_or(str::is_empty) {
                    cache.signatures.push((name, signature));
                }
            }
            updated.insert(project, cache);
        }
        *self.0.write().map_err(|_| PublicError::Storage {
            message: "catalog cache lock poisoned".into(),
        })? = updated;
        Ok(())
    }
}
fn parse_declarations(
    value: &Value,
) -> Result<Vec<(String, sluice_model::plan::Declaration)>, PublicError> {
    value
        .as_object()
        .into_iter()
        .flat_map(|map| map.iter())
        .map(|(name, value)| {
            let wrapped = value.as_object().is_some_and(|map| {
                map.contains_key("doc")
                    || map.len() == 1 && map.contains_key("type")
                    || map.get("type").is_some_and(|ty| !ty.is_string())
            });
            let form = if wrapped { &value["type"] } else { value };
            let ty = sluice_model::types::Type::parse(form).map_err(|e| PublicError::Storage {
                message: format!("fn_list port {name}: {e}"),
            })?;
            Ok((
                name.clone(),
                sluice_model::plan::Declaration {
                    ty,
                    doc: if wrapped {
                        value["doc"].as_str().map(str::to_owned)
                    } else {
                        None
                    },
                },
            ))
        })
        .collect()
}
/// Compose the page-owned registrations without taking coordinator writer ownership.
pub fn socket_pages(
    client: CoordinatorClient,
    reads: ReadPool,
    catalog: SocketCatalog,
) -> PageState {
    let dashboard = views::DashboardState::new(reads.clone(), Arc::new(catalog));
    let mut pages = PageState::new(dashboard.clone());
    pages.messages = Some(views::inbox::MessageState {
        dashboard: dashboard.clone(),
        commands: Arc::new(client.clone()),
    });
    pages.settings = Some(settings::SettingsState::new(
        dashboard,
        Arc::new(SocketSettings { client, reads }),
        Arc::new(projects::StoredWorkOnly),
    ));
    pages.log = true;
    pages
}
/// Bind only loopback; scheduling remains a connection-owned broker lease.
pub async fn serve(
    home: std::path::PathBuf,
    host: String,
    port: u16,
    no_runner: bool,
) -> Result<(), PublicError> {
    let address: std::net::IpAddr = if host.eq_ignore_ascii_case("localhost") {
        "127.0.0.1".parse().expect("loopback")
    } else {
        host.parse().map_err(|_| PublicError::BadRequest {
            message: "host must be a loopback IP or localhost".into(),
        })?
    };
    if !address.is_loopback() {
        return Err(PublicError::BadRequest {
            message: "serve requires a loopback bind".into(),
        });
    }
    let listener = tokio::net::TcpListener::bind((address, port))
        .await
        .map_err(|e| PublicError::Storage {
            message: e.to_string(),
        })?;
    let client = sluice_runtime::client::ensure_coordinator(
        &home,
        &std::env::current_exe().map_err(|e| PublicError::Storage {
            message: e.to_string(),
        })?,
    )
    .await?;
    let _lease = if no_runner {
        None
    } else {
        Some(client.acquire_scheduler().await?)
    };
    let reads = ReadPool::open(&home, 4).map_err(|e| e.into_public(true))?;
    let catalog = SocketCatalog::default();
    let _ = tokio::time::timeout(Duration::from_secs(10), catalog.refresh(&client)).await;
    let stop = CancellationToken::new();
    let pages = socket_pages(client.clone(), reads, catalog.clone());
    let app = router(pages, Arc::new(client.clone()), stop.clone())
        .layer(axum::Extension(board::Registry(Arc::new(catalog.clone()))))
        .layer(axum::Extension(step::Commands(Arc::new(
            SocketOwnerCommands(client.clone()),
        ))));
    let refresh_stop = stop.clone();
    let refresh = tokio::spawn(async move {
        loop {
            tokio::select! { _=refresh_stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{let _=catalog.refresh(&client).await;} }
        }
    });
    let signal_stop = stop.clone();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = sluice_runtime::client::wait_for_signal().await;
            signal_stop.cancel();
        })
        .await;
    stop.cancel();
    refresh.abort();
    let _ = refresh.await;
    result.map_err(|e| PublicError::Storage {
        message: e.to_string(),
    })
}
