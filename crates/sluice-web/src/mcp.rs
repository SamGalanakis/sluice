//! MCP and tool adapters use the same typed commands as the CLI and Unix broker.
use futures_util::future::BoxFuture;
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::*,
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde_json::{Map, Value, json};
use sluice_model::{
    RuntimeApi,
    commands::{CommandReply, CommandRequest},
    error::PublicError,
};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub const WAIT_CAP: u64 = 3600;
pub trait CommandService: Send + Sync {
    fn command(&self, request: CommandRequest) -> BoxFuture<'_, Result<CommandReply, PublicError>>;
}
impl<T: RuntimeApi> CommandService for T {
    fn command(&self, request: CommandRequest) -> BoxFuture<'_, Result<CommandReply, PublicError>> {
        Box::pin(RuntimeApi::command(self, request))
    }
}
#[derive(Clone)]
pub struct McpServer {
    commands: Arc<dyn CommandService>,
}
impl McpServer {
    pub fn new(commands: Arc<dyn CommandService>) -> Self {
        Self { commands }
    }
    pub async fn call(
        &self,
        name: &str,
        args: Map<String, Value>,
        client: Option<&str>,
    ) -> CallToolResult {
        let result = async {
            let request = request_for_tool(self.commands.as_ref(), name, args, client).await?;
            if let CommandRequest::Docs { topic } = request {
                return docs(topic.as_deref());
            }
            let wait = match &request {
                CommandRequest::FnCall(r) => r.wait_seconds.unwrap_or(0),
                CommandRequest::LogWait(r) => r.timeout_seconds,
                CommandRequest::Next(r) => r.timeout_seconds.saturating_add(r.settle_max_seconds),
                _ => 0,
            };
            let reply = tokio::time::timeout(
                Duration::from_secs(wait.saturating_add(30)),
                self.commands.command(request),
            )
            .await
            .map_err(|_| PublicError::Busy {
                message: "command deadline exceeded; inspect status before repeating a write"
                    .into(),
                retryable: false,
            })??;
            reply_value(reply)
        }
        .await;
        match result {
            Ok(Value::String(text)) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Ok(value) => {
                let structured = if value.is_object() {
                    value.clone()
                } else {
                    json!({"result":value})
                };
                let mut result =
                    CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
                result.structured_content = Some(structured);
                result
            }
            Err(error) => {
                CallToolResult::structured_error(serde_json::to_value(error).expect("public error"))
            }
        }
    }
}

/// Public tool arguments are flat; nested `edit`, `selection`, and `read` are internal only.
pub fn decode_tool(
    name: &str,
    mut args: Map<String, Value>,
    client: Option<&str>,
) -> Result<CommandRequest, PublicError> {
    let tool = tools()
        .iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| bad(format!("unknown tool {name}")))?;
    let properties = tool.input_schema["properties"]
        .as_object()
        .expect("tool properties");
    for key in args.keys() {
        if !properties.contains_key(key) {
            return Err(bad(format!(
                "{name} takes no argument '{key}'; its arguments are {}",
                properties.keys().cloned().collect::<Vec<_>>().join(", ")
            )));
        }
    }
    for (key, schema) in properties {
        if !args.contains_key(key)
            && let Some(default) = schema.get("default")
        {
            args.insert(key.clone(), default.clone());
        }
    }
    if properties.contains_key("author") {
        let given = args.get("author").and_then(Value::as_str);
        args.insert("author".into(), json!(author_of(given, client, "mcp")));
    }
    if name == "log_wait"
        && let Some(wake) = args.get_mut("wake")
    {
        *wake = match wake.as_str() {
            Some("any") => json!(false),
            Some("questions") => json!(true),
            _ => return Err(bad("wake must be any or questions")),
        };
    }
    for field in ["steps", "tags", "projects", "state"] {
        if let Some(value) = args.get_mut(field)
            && value.is_string()
        {
            *value = json!([value.clone()]);
        }
    }
    for (public, internal) in renames(name) {
        if let Some(value) = args.remove(*public) {
            args.insert((*internal).into(), value);
        }
    }
    if name != "step_submit"
        && let Some(project) = args.get_mut("project")
        && !project.is_null()
    {
        let selector = project
            .as_str()
            .ok_or_else(|| bad("project must be a current name or id:<uuid>"))?
            .parse::<sluice_model::ids::ProjectSelector>()
            .map_err(|e| bad(e.to_string()))?;
        *project = serde_json::to_value(selector).expect("project selector");
    }
    if let Some(projects) = args.get_mut("projects")
        && !projects.is_null()
    {
        let projects = projects
            .as_array_mut()
            .ok_or_else(|| bad("projects must be an array"))?;
        for project in projects {
            let selector = project
                .as_str()
                .ok_or_else(|| bad("project must be a current name or id:<uuid>"))?
                .parse::<sluice_model::ids::ProjectSelector>()
                .map_err(|e| bad(e.to_string()))?;
            *project = serde_json::to_value(selector).expect("project selector");
        }
    }
    let root = command_schema();
    let raw = argument_schema(root, name);
    for field in ["edit", "selection", "read"] {
        if raw["properties"].get(field).is_some() {
            let nested = resolve(root, &raw["properties"][field]);
            let mut values = Map::new();
            for key in nested["properties"]
                .as_object()
                .expect("nested properties")
                .keys()
            {
                if let Some(value) = args.remove(key) {
                    values.insert(key.clone(), value);
                }
            }
            args.insert(field.into(), Value::Object(values));
        }
    }
    for field in [
        "wait_seconds",
        "timeout_seconds",
        "settle_seconds",
        "settle_max_seconds",
    ] {
        if let Some(value) = args.get_mut(field)
            && !value.is_null()
        {
            let number = value
                .as_u64()
                .ok_or_else(|| bad(format!("{field} must be a nonnegative integer")))?;
            *value = json!(number.min(WAIT_CAP));
        }
    }
    let value = if name == "projects_list" {
        json!({"command":name})
    } else {
        json!({"command":name,"args":args})
    };
    sluice_model::rpc::decode_json(&serde_json::to_vec(&value).expect("tool command"))
}

/// Resolve the mutable project name once before constructing an id-scoped submission.
pub async fn request_for_tool(
    commands: &dyn CommandService,
    name: &str,
    mut args: Map<String, Value>,
    client: Option<&str>,
) -> Result<CommandRequest, PublicError> {
    if name == "step_submit"
        && let Some(project) = args.get("project").and_then(Value::as_str)
    {
        let project = project.to_owned();
        if let Some(id) = project.strip_prefix("id:") {
            args.insert("project".into(), json!(id));
        } else if project.parse::<sluice_model::ids::ProjectId>().is_err() {
            let reply = tokio::time::timeout(
                Duration::from_secs(30),
                commands.command(CommandRequest::ProjectsList),
            )
            .await
            .map_err(|_| bad("project resolution timed out"))??;
            let projects = reply_value(reply)?;
            let id = projects
                .as_array()
                .and_then(|projects| projects.iter().find(|p| p["name"] == project))
                .and_then(|p| p.get("project_id"))
                .cloned()
                .ok_or_else(|| PublicError::NotFound {
                    message: format!("project {project} not found"),
                })?;
            args.insert("project".into(), id);
        }
    }
    decode_tool(name, args, client)
}

pub fn author_of(given: Option<&str>, client: Option<&str>, fallback: &str) -> String {
    let env = std::env::var("SLUICE_AUTHOR").ok();
    let step = std::env::var("SLUICE_STEP")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!("step:{}", s.trim()));
    [
        given,
        env.as_deref(),
        step.as_deref(),
        client,
        Some(fallback),
    ]
    .into_iter()
    .flatten()
    .find(|s| !s.trim().is_empty())
    .unwrap_or(fallback)
    .trim()
    .into()
}
pub fn reply_value(reply: CommandReply) -> Result<Value, PublicError> {
    sluice_runtime::compose::reply_value(reply)
}
fn bad(message: impl Into<String>) -> PublicError {
    PublicError::BadRequest {
        message: message.into(),
    }
}
fn command_schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(CommandRequest)).expect("command schema")
    })
}
fn resolve<'a>(root: &'a Value, value: &'a Value) -> &'a Value {
    if let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        &root["$defs"][reference
            .strip_prefix("#/$defs/")
            .expect("local schema reference")]
    } else {
        value
    }
}
fn argument_schema<'a>(root: &'a Value, name: &str) -> &'a Value {
    let variant = root["oneOf"]
        .as_array()
        .expect("command variants")
        .iter()
        .find(|v| v["properties"]["command"]["const"] == name)
        .expect("registered command");
    resolve(root, &variant["properties"]["args"])
}
fn renames(name: &str) -> &'static [(&'static str, &'static str)] {
    match name {
        "fn_call" => &[("wait", "wait_seconds")],
        "fn_save" => &[("fn", "manifest")],
        "log_wait" => &[("timeout", "timeout_seconds"), ("wake", "questions_only")],
        "next" => &[
            ("timeout", "timeout_seconds"),
            ("settle", "settle_seconds"),
            ("settle_max", "settle_max_seconds"),
        ],
        "plan_prune" => &[("older_than", "older_than_seconds"), ("rev", "expected")],
        "step_add" | "unit_add" | "step_update" | "step_remove" | "step_pause" | "unit_tag"
        | "edge_add" | "edge_remove" | "step_set_input" | "plan_set_input" => {
            &[("rev", "expected")]
        }
        _ => &[],
    }
}
fn default_value(name: &str, key: &str) -> Option<Value> {
    if name == "plan_patch" && key == "reason" {
        return None;
    }
    match key {
        "reason" | "description" => Some(json!("")),
        "dry_run" | "force" | "direct" | "all" | "brief" => Some(json!(false)),
        "view" if name == "status" => Some(json!("steps")),
        "start" | "paused" if name == "step_pause" || key == "start" => Some(json!(true)),
        "params" if name == "query" => Some(json!([])),
        "questions_only" => Some(json!("any")),
        "params" | "resources" | "after" | "inputs"
            if matches!(name, "unit_add" | "project_create" | "fn_call") =>
        {
            Some(json!({}))
        }
        "add" | "remove" | "tags" if name == "unit_tag" || name == "unit_add" => Some(json!([])),
        "limit" => Some(json!(200)),
        "timeout_seconds" => Some(json!(300)),
        "older_than_seconds" | "since_seq" if name == "plan_prune" || name == "next" => {
            Some(json!(0))
        }
        "settle_seconds" => Some(json!(20)),
        "settle_max_seconds" => Some(json!(120)),
        "settles" => Some(json!("short")),
        "me" => Some(json!("orchestrator")),
        "projects" if name == "next" => Some(json!([])),
        "format" => Some(json!("mermaid")),
        _ => None,
    }
}
/// Definitions are generated from the canonical commands, including p1c's folded fields.
pub fn tools() -> &'static [Tool] {
    static TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();
    TOOLS.get_or_init(|| {
        DESCRIPTIONS
            .iter()
            .map(|(name, description)| tool_definition(name, description))
            .collect()
    })
}
fn tool_definition(name: &str, description: &str) -> Tool {
    let root = command_schema();
    let raw = argument_schema(root, name);
    let mut properties = Map::new();
    let mut required = Vec::new();
    schema_fields(root, name, raw, &mut properties, &mut required);
    let mut definitions = root["$defs"].clone();
    let mut id = root["$defs"]["ProjectId"].clone();
    id.as_object_mut().expect("id schema").remove("format");
    if let Some(pattern) = id["pattern"].as_str() {
        id["pattern"] = json!(format!("^id:{}", pattern.trim_start_matches('^')));
    }
    definitions["ProjectSelector"] = json!({"anyOf":[root["$defs"]["ProjectName"].clone(),id],"description":"Current project name or id:<uuid>. Stale names return not_found."});
    if name == "step_submit" {
        properties.insert("project".into(), definitions["ProjectSelector"].clone());
    }
    let needed = reachable_definitions(&Value::Object(properties.clone()), &definitions);
    let schema = json!({"type":"object","properties":properties,"required":required,"additionalProperties":false,"$defs":needed});
    Tool::new(
        name.to_owned(),
        description.to_owned(),
        schema.as_object().expect("object schema").clone(),
    )
}
fn schema_fields(
    root: &Value,
    name: &str,
    schema: &Value,
    properties: &mut Map<String, Value>,
    required: &mut Vec<Value>,
) {
    let Some(fields) = schema["properties"].as_object() else {
        return;
    };
    for (key, value) in fields {
        // Only the dashboard speaks as the owner; MCP callers read as it at most.
        if key == "owner" && matches!(name, "ask" | "say" | "reply") {
            continue;
        }
        if matches!(key.as_str(), "edit" | "selection" | "read") {
            schema_fields(root, name, resolve(root, value), properties, required);
            continue;
        }
        let mut value = value.clone();
        let public = renames(name)
            .iter()
            .find(|(_, internal)| *internal == key)
            .map_or(key.as_str(), |(public, _)| *public);
        // ask and say name their recipient: required here, though a call without
        // one decodes, so the store refuses it as `invalid` like an unknown one.
        if key == "to" && matches!(name, "ask" | "say") {
            value.as_object_mut().expect("to schema").remove("default");
            required.push(json!(key));
            properties.insert(key.clone(), value);
            continue;
        }
        let default = default_value(name, key).or_else(|| value.get("default").cloned());
        if let Some(default) = default {
            value["default"] = default;
        } else if schema["required"]
            .as_array()
            .is_some_and(|r| r.contains(&json!(key)))
        {
            required.push(json!(public));
        }
        if key == "questions_only" {
            value = json!({"type":"string","enum":["any","questions"],"default":"any"});
        }
        if matches!(key.as_str(), "steps" | "tags" | "projects" | "state") {
            let default = value.get("default").cloned();
            value = json!({"anyOf":[value,{"type":"string"}]});
            if let Some(default) = default {
                value["default"] = default;
            }
        }
        properties.insert(public.into(), value);
    }
}

fn reachable_definitions(value: &Value, definitions: &Value) -> Map<String, Value> {
    fn visit(value: &Value, definitions: &Value, found: &mut Map<String, Value>) {
        match value {
            Value::Object(map) => {
                if let Some(name) = map
                    .get("$ref")
                    .and_then(Value::as_str)
                    .and_then(|reference| reference.strip_prefix("#/$defs/"))
                    && !found.contains_key(name)
                {
                    let definition = definitions[name].clone();
                    found.insert(name.into(), definition.clone());
                    visit(&definition, definitions, found);
                }
                for value in map.values() {
                    visit(value, definitions, found);
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, definitions, found);
                }
            }
            _ => {}
        }
    }
    let mut found = Map::new();
    visit(value, definitions, &mut found);
    found
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("sluice", env!("CARGO_PKG_VERSION")))
        .with_instructions(doc_page("instructions").unwrap_or("sluice runs plans"))
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tools().to_vec(),
            ttl_ms: Some(0),
            cache_scope: Some(rmcp::model::CacheScope::Private),
            ..Default::default()
        })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let peer = context.client_info();
        let client = peer.as_ref().map(|info| info.name.as_str());
        Ok(self
            .call(&request.name, request.arguments.unwrap_or_default(), client)
            .await
            .into())
    }
    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult {
            resources: DOCS
                .iter()
                .map(|(topic, page)| {
                    Resource::new(format!("sluice://docs/{topic}"), *topic)
                        .with_description(title(page))
                        .with_mime_type("text/markdown")
                })
                .collect(),
            ..Default::default()
        })
    }
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let page = request
            .uri
            .strip_prefix("sluice://docs/")
            .and_then(doc_page)
            .ok_or_else(|| ErrorData::resource_not_found("no such docs resource", None))?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(page, request.uri).with_mime_type("text/markdown"),
        ])
        .into())
    }
}
pub fn http_service(
    server: McpServer,
    stop: CancellationToken,
) -> StreamableHttpService<McpServer, LocalSessionManager> {
    let mut config = StreamableHttpServerConfig::default();
    config.cancellation_token = stop;
    config.max_request_body_bytes = crate::http::MAX_BODY;
    config.stateless_protocol_metadata_required = false;
    config.json_response = true;
    StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    )
}
pub async fn serve_stdio(server: McpServer) -> Result<(), PublicError> {
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| bad(e.to_string()))?;
    service.waiting().await.map_err(|e| bad(e.to_string()))?;
    Ok(())
}
fn title(page: &str) -> &str {
    page.lines()
        .find(|s| !s.trim().is_empty())
        .unwrap_or("")
        .trim_start_matches('#')
        .trim()
}
fn doc_page(topic: &str) -> Option<&'static str> {
    DOCS.iter()
        .find(|(name, _)| *name == topic)
        .map(|(_, page)| *page)
}
pub fn docs(topic: Option<&str>) -> Result<Value, PublicError> {
    match topic {
        Some(topic) => doc_page(topic)
            .map(|s| json!(s))
            .ok_or_else(|| PublicError::NotFound {
                message: format!("no docs topic {topic}"),
            }),
        None => Ok(Value::Object(
            DOCS.iter()
                .map(|(topic, page)| ((*topic).into(), json!(title(page))))
                .collect(),
        )),
    }
}

const DOCS: &[(&str, &str)] = sluice_runtime::docs::PAGES;

const DESCRIPTIONS: &[(&str, &str)] = &[
    (
        "docs",
        "Read sluice's documentation for agents.\n\nArgs:\n    topic: a page name such as \"plans\", \"types\", \"fns\" or \"examples\". Leave it out to\n        get the index: {topic: first heading} for every page.",
    ),
    (
        "projects_list",
        "List the live projects by name: [{project_id, name, description, rev, settings_rev,\ncounts, archived, paused, board_rev, resources?, icon?}]; settings_rev is what project_update\nand project_delete take as expected_settings_rev; board_rev is the board's revision (0 before\nany board; board_set takes it as expected_rev); counts maps step status -> number of steps in\nthe project's plan; resources,\nwhen it declares any, maps each to {\"capacity\": n} or {\"capacity_fn\": fn}; icon, when\nthe project has one, is {\"kind\": \"image\", \"type\": <content type>} or {\"kind\": \"text\",\n\"text\": <text>}.",
    ),
    (
        "project_create",
        "Create a project with an empty plan (rev 1). Returns {project_id, name}.\n\nArgs:\n    name: lowercase letters, digits, - and _ (starting with a letter or digit).\n    description: what the project is for; put any context an orchestrator needs here.\n    icon: the project's icon: an absolute path to an image file (SVG, PNG, WebP, JPEG\n        or GIF, at most 256 KB, copied into the project's row), or a short\n        text icon (an emoji; at most 16 characters).\n    resources: named capacities its steps' `needs` draw on (docs(\"plans\")):\n        {name: n} or {name: {\"capacity\": n}} for a fixed capacity (an integer >= 0),\n        {name: {\"capacity_fn\": \"<fn>\"}} for one a fn the project sees returns as\n        {capacity}, called about every 10 s, e.g. {\"lane\": 56, \"cpu\":\n        {\"capacity_fn\": \"cpu-free\"}}.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "project_update",
        "Update a project by current name or id:<uuid>, including new_name, description, icon, resources, paused or archived. icon is a short text icon, an absolute (or ~/) path to an SVG, PNG, WebP, JPEG or GIF file of at most 256 KB, or {media_type, bytes_base64}; \"\" removes it. expected_settings_rev (projects_list's settings_rev) fences stale settings. Each change records author and reason. Returns project_id and current name.",
    ),
    (
        "project_delete",
        "Delete an archived project and its owned state. confirm_name must equal the current name and expected_settings_rev must match its settings_rev in projects_list. Refused while any step, call or guardian is live. Returns project_id, name and deleted.",
    ),
    (
        "fn_list",
        "List the functions a project sees (built-in, global, then the project's own; without\na project: built-in and global). Returns [{name, doc, inputs, outputs, scope, submits?,\nicon?, open?}]. A function with a problem (bad fn.json, name collision) is left out; verify\nreports it.\n\nArgs:\n    project: the project whose functions to list.",
    ),
    (
        "fn_get",
        "Return one function's fn.json plus where it lives. Returns the fn.json with {scope, path}\nadded (path is its directory, null for a built-in).\n\nArgs:\n    name: the function name, e.g. \"git.head\".\n    project: look it up as this project sees it (project functions included).",
    ),
    (
        "fn_save",
        "Create or replace a function: writes fns/<name>/fn.json and main.py into the project\n(or, without one, the global fns dir) and republishes the functions. Returns {name, scope,\npath, generation}. Read docs(\"fns\") first.\n\nArgs:\n    fn: the fn.json: {name, doc?, inputs: {name: type}, outputs: {name: type}}. Checked\n        before anything is written; the name may not collide with a built-in or global\n        function (nor, for a global function, with any project's own).\n    main_py: the Python source of main.py (a uv script: `from sluice_fn import run`, then\n        `run(main)`).\n    project: the project that owns it; leave out for a global function.",
    ),
    (
        "fn_call",
        "Run one function outside the plan. Returns {call, project_id, status, inputs, outputs,\nerror, direct}; status is pending, running, succeeded or failed. Poll call_status(call) for\na slow one; every status change is also a `call` record in the log (log_read).\n\nArgs:\n    name: the function to run.\n    inputs: an object keyed by the function's input names; checked against its types\n        before anything runs (an `invalid` error lists every mismatch with its path).\n    project: run it in this project (with the functions it sees); leave out for none.\n    wait: how many seconds to wait for the result, a number such as 60 (default 0:\n        return at once; capped at 3600).\n    direct: run it here and now, to the end, instead of queueing it for the runner\n        (for use without a runner, e.g. from the command line); ignores `wait`.\n    author: who is calling (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "call_status",
        "Return a fn_call's state. Returns {call, project_id, status, inputs, outputs, error,\ndirect}, as fn_call does.\n\nArgs:\n    call: the id fn_call returned.\n    project: the project it ran in, if any.",
    ),
    (
        "board_set",
        "Set or clear the project's board: an OpenUI Lang program the dashboard draws beside the\nplan (its own section on a phone), with live components (Units, StepStatus, Output, Metric,\nQuery, Chart) filled from the project; see docs(\"board\"). Returns {rev}: the board's new\nrevision (unchanged when the program is the one it has). A stale expected_rev is a\nconflict; a program that does not check is invalid, each problem as \"line N: ...\". The\nchange is a project.board record (author, reason, rev; not the program).\n\nArgs:\n    project: the project.\n    program: the board program, or null to clear the board.\n    expected_rev: the board_rev you read (status, projects_list, board_get); refused if it\n        has changed since.\n    reason: why, for the record.",
    ),
    (
        "board_get",
        "Read the project's board. Returns {project, rev, program}: the project {project_id,\nname}, the board's revision (0 before any board) and its program (null when none is set).\n\nArgs:\n    project: the project.",
    ),
    (
        "plan_get",
        "Return the project's plan. Returns {project, rev, plan}: the project {project_id, name},\nthe plan's revision and the plan (without rev).\n\nArgs:\n    project: the project.",
    ),
    (
        "plan_patch",
        "Edit a project's plan with RFC 6902 JSON Patch ops. A step it adds starts as soon\nas it is ready; pass start=false to add it paused (a draft), and unpause it with\nstep_pause. Cap how many run at once with resources and `needs`. Returns the edit result {project, rev, preview}: the project {project_id, name}, the new\nrev and what the edit set going (its ops and the steps it starts, queues, skips or stales).\n\nArgs:\n    project: the project.\n    rev: the revision you read; if the plan moved on you get `conflict` with\n        current_rev, so re-read and retry.\n    ops: JSON Patch operations against the plan without rev, e.g.\n        [{\"op\": \"add\", \"path\": \"/steps/x\", \"value\": {...}}]. The result is validated;\n        an `invalid` error lists every problem with its path.\n    reason: why, recorded in the plan's history (required).\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n    start: false adds the new steps paused (unless a step sets `paused` itself).\n\ndry_run: preview without committing (the result is then the preview {ops, would_start,\nwould_queue, would_skip, would_stale, errors}). Every edit records author and reason.",
    ),
    (
        "step_add",
        "Add one step to a plan: plan_patch for a single step, at the current rev. It\nstarts as soon as it is ready; start=false (or `paused` in the spec) adds it paused.\nReturns the edit result {project, rev, preview}: the project {project_id, name}, the new\nrev and what the edit set going (its ops and the steps it starts, queues, skips or stales).\n\nArgs:\n    project: the project.\n    step: the new step's id.\n    spec: the step, {run, in, scatter?, doc?, outputs?, paused?, after?, tags?, needs?}.\n    reason: why, recorded in the plan's history.\n    start: false adds it paused.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing (the result is then the preview {ops, would_start,\nwould_queue, would_skip, would_stale, errors}). Every edit records author and reason.",
    ),
    (
        "recipe_list",
        "List the recipes a project sees, by name (scope global: SLUICE_HOME/recipes/, or project:\nthe project's own recipes/, which wins on a name clash). Returns [{name, doc, params,\nscope}]; a broken recipe file is listed as {name, scope, error}. params maps each param to\nits type (or {type, doc}), `unit` (always there) included. Read docs(\"plans\") on recipes.\n\nArgs:\n    project: the project.",
    ),
    (
        "unit_add",
        "Add one unit of work from a recipe: its steps with `{param}` filled in, each tagged\n`unit:<unit>` (and `tags`), with the edges and input overrides given, in one plan\nedit at the current rev: one call stages a whole lane. They start as soon as they are\nready; start=false adds them paused. Refused (`bad_request`) when an id it would add\nis already in the plan; `invalid` lists every param, expansion or staging problem\n(nothing is written). Returns the edit result {project, rev, preview, steps}: steps\nare the ids it added.\n\nArgs:\n    project: the project.\n    recipe: the recipe's name (recipe_list).\n    params: {unit: \"<name of the unit, a valid step id>\", <param>: value, ...}, each\n        checked against the recipe's param types.\n    start: false adds the new steps paused.\n    tags: more tags for every step of the unit, e.g. [\"arc:tsvm\"] (select by them in\n        status, step_pause, step_cancel, plan_prune, ...); `unit:` ones are reserved.\n    after: {suffix: [step ids]}: ids appended to that recipe step's `after` (its own\n        kept). A suffix is the recipe step's id without the leading \"<unit>-\", e.g.\n        {\"fork\": [\"fig-4200-landed\"]}.\n    inputs: {suffix: {input: value}}: literals bound to that step's inputs\n        ({\"default\": value}, replacing the recipe's binding), e.g.\n        {\"work\": {\"model\": {\"type\": \"normal\", \"model\": \"sol\", \"effort\": \"xhigh\"}}};\n        an input its fn does not declare and the recipe\n        does not bind is refused.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n    reason: why, recorded in the plan's history (default \"add unit <unit> (recipe\n        <recipe>)\").\n\ndry_run: preview without committing. Every edit records author and reason. tags adds unit tags; inputs binds inputs by entry step.",
    ),
    (
        "unit_tag",
        "Add and/or remove tags on every step of a unit (those tagged `unit:<unit>`), in one\nplan edit at the current rev, e.g. to put units into an arc after the fact. `unit:`\ntags are reserved; an unknown unit is `not_found`. Nothing to change: no edit, the\ncurrent rev. Returns the edit result {project, rev, preview, steps}: steps are the\nunit's steps.\n\nArgs:\n    project: the project.\n    unit: the unit's name.\n    add: tags to add, e.g. [\"arc:tsvm\"].\n    remove: tags to remove.\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing. Every edit records author and reason.",
    ),
    (
        "edge_add",
        "Make a step run after others: append gate entries to its `after` (deduplicated, its\nexisting entries kept) in one plan edit at the current rev, with no rev to read and no\nway to drop an entry someone else added. `step` may be `unit:<name>`: every entry step of\nthat unit. An unknown step or unit is `invalid`, and so is a cycle. Returns the edit result {project, rev, preview}: the project {project_id, name}, the new\nrev and what the edit set going (its ops and the steps it starts, queues, skips or stales).\n\nArgs:\n    project: the project.\n    step: the step that waits, or unit:<name>.\n    after: the gate entries it waits for (docs(\"plans\")): a step id, `<id>?` (a skip\n        is fine too), `unit:<name>`, or a boolean output `<id>/<output>` (`!` negates).\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing (the result is then the preview {ops, would_start,\nwould_queue, would_skip, would_stale, errors}). Every edit records author and reason.",
    ),
    (
        "edge_remove",
        "Remove gate entries from a step's `after` in one plan edit at the current rev (the others\nkept). `step` may be `unit:<name>`: every entry step of that unit. An unknown step or unit\nis `invalid`. Returns the edit result {project, rev, preview}: the project {project_id, name}, the new\nrev and what the edit set going (its ops and the steps it starts, queues, skips or stales).\n\nArgs:\n    project: the project.\n    step: the step that waits, or unit:<name>.\n    after: the entries it should no longer wait for.\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing (the result is then the preview {ops, would_start,\nwould_queue, would_skip, would_stale, errors}). Every edit records author and reason.",
    ),
    (
        "step_update",
        "Change fields of one step: each key of `changes` replaces that field (`in` is\nreplaced whole), null removes it; `when` is refused (use `after`). A running step only\ntakes `paused` and `tags`. Returns the edit result {project, rev, preview}: the project {project_id, name}, the new\nrev and what the edit set going (its ops and the steps it starts, queues, skips or stales).\n\nArgs:\n    project: the project.\n    step: the step id.\n    changes: e.g. {\"doc\": \"...\", \"in\": {...}}.\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing (the result is then the preview {ops, would_start,\nwould_queue, would_skip, would_stale, errors}). Every edit records author and reason.",
    ),
    (
        "step_remove",
        "Remove steps from a plan in one edit, selected by ids and/or tags. Refused\n(`invalid`) while a step left or a plan output still reads one, or while one runs.\nEach one that finished keeps its outcome (the `outcomes` table, see query).\nReturns the edit result {project, rev, preview}: the project {project_id, name}, the new\nrev and what the edit set going (its ops and the steps it starts, queues, skips or stales).\n\nArgs:\n    project: the project.\n    steps: step ids (one id is fine too).\n    tags: every step carrying any of these tags.\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing (the result is then the preview {ops, would_start,\nwould_queue, would_skip, would_stale, errors}). Every edit records author and reason.",
    ),
    (
        "step_pause",
        "Pause or unpause steps in one edit. A paused step does not start, however ready\nits inputs, until unpaused; a running one finishes (pausing never stops it: see\nstep_cancel). Select by ids and/or tags; with subtree, also everything downstream\n(steps that read from or run after them, transitively), including those that become\nready later. Pausing what is paused already, or unpausing what is not, is no edit.\nReturns the edit result {project, rev, preview, steps}: the steps selected.\n\nArgs:\n    project: the project.\n    steps: step ids (one id is fine too).\n    tags: select every step carrying any of these tags.\n    subtree: include everything downstream of the selected steps.\n    paused: true to pause, false to let them start.\n    reason: why; kept on each paused step (status shows it) and in the history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing. Every edit records author and reason.",
    ),
    (
        "step_cancel",
        "Stop running steps, selected by ids and/or tags: the runner kills their processes\nand fails each with `cancelled: <reason>`; step_retry runs them again. A pending\ncore.external step (work done outside sluice) fails the same way at once. Refused\nunless every selected step is running or a pending core.external one. Returns {ok: true}.\n\nArgs:\n    project: the project.\n    steps: step ids (one id is fine too).\n    tags: every step carrying any of these tags.\n    reason: why, in their errors and `step.cancel` log records.\n    expected_rev: refuse (`conflict`) unless the plan is at this revision.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "plan_history",
        "Return the plan's history, oldest first. Returns [{seq, at, kind, ...}]: log records,\nevery edit as `plan.edit` {rev, author, reason, ops} from rev 1, and the `plan.input`,\n`step.output` and `step.retry` records.\n\nArgs:\n    project: the project.\n    since_rev: only entries after this revision.",
    ),
    (
        "plan_set_input",
        "Set a declared plan input; steps reading it can then start. Changing it later makes\nthe succeeded steps that read it (and their dependents) stale. Returns {ok: true}; with\ndry_run, the preview {ops, would_start, would_queue, would_skip, would_stale, errors}.\n\nArgs:\n    project: the project.\n    name: the plan input's name.\n    value: its value, checked against the input's type.\n    rev: refuse (`conflict`) unless the plan is at this revision.\n    reason: why, recorded in the plan's history.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "step_set_input",
        "Bind literal inputs ({name: value}, each as {\"default\": value}) on the steps selected by steps and/or tags, in one edit at rev if supplied. Only steps whose fn has the inputs change; running steps are left alone; a succeeded step whose binding changes turns stale. Returns the edit result {project, rev, preview} with changed, running (selected but running) and unsupported ([{step, inputs}]: selected but lacking those inputs); an edit that changes no step is bad_request. dry_run returns the preview. reason and author record why and who.",
    ),
    (
        "step_set_output",
        "Mark a step succeeded with outputs you supply (manual: true); it is not run unless\nretried. Refused (`invalid`, naming them) while a step it reads has not succeeded or a\nplan input it reads has no value, unless force. Returns {ok: true}.\n\nArgs:\n    project: the project.\n    step: a step that is not running.\n    outputs: every output of its function, type-checked (arrays for a scattered step).\n    reason: why, recorded in the plan's history.\n    force: set it anyway although what it reads is not ready (e.g. a broken\n        upstream); the step then turns stale once those values are all there.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "step_retry",
        "Retry succeeded, failed or stale steps and re-arm the region they block. Returns {project,\nsteps, rearmed, stopped_at}: the steps retried, the failed, stale or pending dependents\nre-armed, and the succeeded, running or skipped steps the re-arm stopped at.\n\nArgs:\n    project: the project.\n    steps: step ids (one id is fine too).\n    tags: every step carrying any of these tags.\n    message: posted to each retried step's thread in the same edit, so its next run\n        (a resumed agent session, say) is given it.\n    reason: why, recorded in the log.\n    expected_rev: refuse (`conflict`) unless the plan is at this revision.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "step_submit",
        "Submit the outputs a running step declares (its `outputs`), from the agent doing\nthe step's work. Checked against the declared outputs: every required one present,\ntypes fitting, no others; `invalid` lists every mismatch with its path, so fix them\nand submit again. Submitting again replaces what was sent. When the step's fn exits,\nthese join its outputs; a required one never submitted fails the step. Returns\n{ok: true}.\n\nArgs:\n    project: the project.\n    step: the running step.\n    outputs: an object keyed by declared output name.\n    run: the run id (SLUICE_RUN_ID).\n    author: who is submitting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "log_read",
        "Read the log. Returns {records, last_seq}: records are {seq, at, kind, ...}, oldest first\n(seqs increase across the whole home, so one log's have gaps). Kinds: plan.edit,\nplan.input, step.output, step.retry, step.cancel, step.submit, step.status, step.lease,\nstep.queued, call, message, project.pause, project.archive, project.update,\nproject.board, project.rename, project.delete, project.capacity, project.notify, run.adopt, run.orphan,\nrun.completion_action, run.completion_action.register, unit.settled.\n\nArgs:\n    project: the project's log; leave out for the home log (calls without a project).\n    since_seq: only records after this seq (pass the last_seq you got to continue).\n        Without it: the last `limit` matching records.\n    kinds: only these kinds; \"plan\", \"step\", \"project\", \"run\" or \"unit\" match every\n        kind under them.\n    threads: only messages on these threads (and, without kinds, only messages).\n    limit: at most this many records (default 200).",
    ),
    (
        "log_wait",
        "Wait for log records after since_seq. Returns {records, last_seq} as soon as at least\none matching record exists, or with no records once `timeout` seconds pass. Call it\nagain with the last_seq it returned to keep watching.\n\nArgs:\n    since_seq: wait for records after this seq (0 for any; last_seq from log_read).\n    project: the project's log; leave out for the home log.\n    kinds: only these kinds (as in log_read).\n    threads: only messages on these threads (as in log_read).\n    timeout: seconds to wait at most (default 300; capped at 3600).\n    limit: at most this many records (default 200).\n    wake: \"any\" (default) or \"questions\": a note (a message posted with needs_reply\n        false) does not end the wait; it comes back with the next record that does,\n        or once `timeout` passes.",
    ),
    (
        "next",
        "Wait for the records an orchestrator should act on across the projects. Returns\n{records, notes, last_seq, timed_out}. It wakes on a `unit.settled` record (a unit, the\nsteps tagged with the same unit:<name>, else a step on its own, settles once per work\ngeneration: none of its steps running, or pending and startable); a step that failed,\nwent stale or was skipped; a message needing a reply, not from you, addressed to `me` or\nto nobody; an answering reply not from you; a project paused or archived by someone else.\nA unit.settled record is {unit, work, steps: [{id, status, held, outputs, omitted}]},\nthe succeeded steps' outputs cut as `settles` says. After the first waking record it\nkeeps collecting until `settle` seconds pass with no new one, or `settle_max` seconds\nafter the first. Messages come first in `records`, whole. `notes` are the notes\n(messages with needs_reply false, not from you) held on the way; read them before the\nrecords. Pass `last_seq` back as `since_seq` to continue; nothing is missed or repeated.\nThe command-line form is `sluice next`.\n\nArgs:\n    projects: the projects to watch (one or several; leave out for every live project).\n    since_seq: records after this seq (the last_seq you last got).\n    me: your name; your own messages never wake it (default \"orchestrator\").\n    timeout: seconds to wait at most for the first waking record (default 300;\n        capped at 3600); on a timeout records is empty and timed_out is true.\n    all: every record wakes it (default false).\n    settle: seconds with no new waking record that end the batch (default 20; 0\n        returns at the first).\n    settle_max: seconds after the first waking record that end the batch at the\n        latest (default 120; capped at 3600).\n    settles: how much of a settled unit's outputs each step carries: \"short\"\n        (default) only booleans, numbers, strings of at most 80 characters and a\n        `summary`'s first line (cut to 200), the rest named in `omitted` (read them\n        with status or query); \"full\" every output whole; \"none\" no outputs.",
    ),
    (
        "drain",
        "Pause the projects (default: every project not archived) that are not already\npaused, recording which ones and the author as the drain's owner, so `release` lets\nexactly those go again; new plan work and user calls are refused home-wide until then.\nReturns {paused, status: {mode, owner, paused, blockers, pending_calls, open_questions,\ndrained}}: the projects it paused now, and what is still to finish (the CLI's\n`sluice drain` waits until drained).\n\nArgs:\n    projects: the projects to drain; leave out for every project not archived.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "release",
        "Unpause exactly the projects the drain recorded (what `sluice drain --release` does) and\nreturn the home to normal. Projects paused otherwise stay paused. Returns {released}.\n\nArgs:\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "step_context",
        "Where a step stands, for the agent doing it — the same as `sluice me` inside the\nstep: its fn, doc, status and running time, inputs, the status and short outputs of\nevery step it reads or runs after, the unanswered messages on its thread\n(step-<id>), the outputs it must submit with the exact step_submit command, and the\nthread with the command to ask a question.\n\nArgs:\n    project: the project.\n    step: the step id.",
    ),
    (
        "query",
        "Run one read-only SQL statement against the home database. Returns {columns, rows,\ntruncated}. params binds values; limit defaults to 200 and is at most 1000 rows. Writes,\nATTACH, extensions and unsafe functions are refused; it has a two-second deadline and a\n1 MiB response bound.\n\nArgs:\n    sql: one statement.\n    params: values bound to its placeholders.\n    limit: at most this many rows (default 200).",
    ),
    (
        "verify",
        "Check functions (fn.json shape and types, name collisions), .env syntax, each project's\nplan and its state, and directories under projects/ of no project; changes nothing.\nReturns [{where, message}], empty when all is well.\n\nArgs:\n    project: check this project (and the built-in and global functions it sees);\n        leave out to check everything.",
    ),
    (
        "plan_view",
        "Draw the plan with each step's status: a Mermaid flowchart (top-down, a subgraph per\nunit) or a standalone HTML page. Done units (independent pieces of work whose every step\nsucceeded or was skipped) are left out, with one line saying how many, unless all is true.\n\nArgs:\n    project: the project.\n    format: \"mermaid\" (default) or \"html\".\n    all: include the done units too.",
    ),
    (
        "status",
        "A project's state at a glance: {project, rev, board_rev, paused, inputs, outputs, resources,\nsteps: {id: {status, outputs, error, run_ids, done, total, instances, manual,\npaused?, queued?, waiting?}}, done_units?}. paused is true or the pause's reason; waiting,\non a pending step that is not about to start, lists why: its pause, the project's, each\nhandoff or gate not ready, the resource shortfall (queued names the resources) or the\nexternal wait. Without steps or tags, the done units (every step\nsucceeded or skipped) are left out and counted in done_units {units, steps}, unless all\nis true.\n\nArgs:\n    project: the project.\n    steps: only these steps (ids).\n    tags: only steps carrying any of these tags (with steps: either).\n    brief: cut every string over 200 characters in inputs and outputs to its first 200\n        and `… [n more characters]`.\n    all: include the done units too (steps or tags always return what they select).\n    view: \"steps\" (default) as above, or \"units\": one compact row per unit (the steps\n        sharing a `unit:<name>` tag, else a step on its own) instead: {project, rev,\n        board_rev, paused, resources?, units: [{unit, state, age, engine, steps, blocked, last,\n        line}], done_units?}, oldest first. state is running, failed (a step failed or\n        stale), settled (every step succeeded or skipped), blocked (nothing running or\n        startable, something held), queued (a step waits for resources) or pending; age\n        the seconds its running step has run, else since its last change; engine\n        engine·model·effort of its agent step; steps each step's mark (✓ succeeded,\n        ▶ running, · pending, ✗ failed, ~ stale, – skipped, ‖ paused, ≡ queued); blocked\n        why a blocked unit's first held step is held, or a queued unit's first queued\n        step's reason; last its steps' threads' last message (\"Q: \" for a question);\n        line all of it in at most 80 characters.\n    state: with view \"units\", only units in this state (one or a list).",
    ),
    (
        "plan_prune",
        "Remove done units (every step succeeded or skipped), all or those named in units or tagged with tags, whose last step finished at least older_than seconds ago, in one edit. A unit a surviving step or plan output references is kept. Returns the edit result {project, rev, preview, steps} (steps: the removed steps) with units (the removed units) and kept: [{unit, step}] or [{unit, output}], each kept unit with the step or plan output holding it. Nothing to remove: no edit, the current rev. dry_run previews without committing. rev is the revision you read, reason and author record the edit.",
    ),
    (
        "ask",
        "Ask a question that needs a reply. Returns its receipt {id, to, thread, delivery,\nrun?}. The thread and sender are derived: a message to or from a step lives on\n`step-<step>`, the orchestrator and owner talk on `owner`; you are the orchestrator,\nor the run's step with `run`. delivery is `delivered` (a live run of the step that\nlistens was handed it, or the orchestrator's or owner's inbox), `queued` (the step\nwill run and its next run is assigned it) or `no_live_run` (the step is done, failed\nor paused: a later run, e.g. a retry, gets it). The first reply answers it.\n\nArgs:\n    project: the project.\n    to: a step of the project's current plan, \"orchestrator\" or \"owner\"; anything\n        else is `invalid` and nothing is stored.\n    body: the question, markdown.\n    title: a short title for the inbox.\n    ui: an OpenUI form the owner answers with.\n    input: a plan input the answer sets.\n    data: any JSON for the recipient.\n    run: the asking run (SLUICE_RUN_ID): its step is the sender.",
    ),
    (
        "say",
        "Tell a step, the orchestrator or the owner something; no reply is expected.\nReturns its receipt {id, to, thread, delivery, run?}, as ask does.\n\nArgs:\n    project: the project.\n    to: a step of the project's current plan, \"orchestrator\" or \"owner\".\n    body: the note, markdown.\n    data: any JSON for the recipient.\n    run: the speaking run (SLUICE_RUN_ID): its step is the sender.",
    ),
    (
        "reply",
        "Reply to a message: to its sender, on its thread. Returns its receipt {id, to,\nthread, delivery, run?}. A reply to an open question answers it (with `input`, it\nsets that plan input to `answer.values.value`, `answer.params.value` or the body);\nanswer {\"action\": \"close\"} closes it without setting anything. A reply to a\nquestion already answered or closed is just a message, but one with an `answer` is\nrefused with `conflict`.\n\nArgs:\n    project: the project.\n    to_message: the id of the message replied to.\n    body: the reply, markdown (may be empty with an answer).\n    answer: {action, params?, values?}, the answer to a question's ui form.\n    run: the replying run (SLUICE_RUN_ID): its step is the sender.",
    ),
    (
        "messages",
        "Read a project's messages. Returns {project, messages, last_id}. Each message is {id,\nverb (ask, say or reply), from, to, thread, body, title?, ui?, input?, data?, run?, at,\nto_message?, answer?}; a question also carries state: open, answered (with\nanswered_by, the answering message's id) or closed.\n\nArgs:\n    project: the project.\n    view: inbox (your open questions and unread notes and replies), questions (every\n        open question), history (every conversation you took part in) or thread.\n    thread: the thread, for the thread view (or to narrow another).\n    since: only messages after this id.\n    owner: read as the owner, whose inbox the dashboard shows (default: the\n        orchestrator).",
    ),
];
