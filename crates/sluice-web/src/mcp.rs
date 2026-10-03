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
    if name == "message_post"
        && args
            .get("from")
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty())
    {
        args.insert("from".into(), args["author"].clone());
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
    for field in ["steps", "tags", "projects"] {
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
    if let CommandReply::Ack = reply {
        return Ok(json!({"ok":true}));
    }
    let value = serde_json::to_value(reply).map_err(|e| bad(e.to_string()))?;
    Ok(value.get("data").cloned().unwrap_or(Value::Null))
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
        "dry_run" | "force" | "direct" | "all" => Some(json!(false)),
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
        if matches!(key.as_str(), "edit" | "selection" | "read") {
            schema_fields(root, name, resolve(root, value), properties, required);
            continue;
        }
        let mut value = value.clone();
        let public = renames(name)
            .iter()
            .find(|(_, internal)| *internal == key)
            .map_or(key.as_str(), |(public, _)| *public);
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
        if matches!(key.as_str(), "steps" | "tags" | "projects") {
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

const DOCS: &[(&str, &str)] = &[
    (
        "composing",
        include_str!("../../../src/sluice/docs/composing.md"),
    ),
    (
        "examples",
        include_str!("../../../src/sluice/docs/examples.md"),
    ),
    ("fns", include_str!("../../../src/sluice/docs/fns.md")),
    ("inbox", include_str!("../../../src/sluice/docs/inbox.md")),
    (
        "instructions",
        include_str!("../../../src/sluice/docs/instructions.md"),
    ),
    ("plans", include_str!("../../../src/sluice/docs/plans.md")),
    (
        "threads",
        include_str!("../../../src/sluice/docs/threads.md"),
    ),
    ("types", include_str!("../../../src/sluice/docs/types.md")),
];

const DESCRIPTIONS: &[(&str, &str)] = &[
    (
        "docs",
        "Read sluice's documentation for agents.\n\nArgs:\n    topic: a page name such as \"plans\", \"types\", \"fns\" or \"examples\". Leave it out to\n        get the index: {topic: first heading} for every page.",
    ),
    (
        "projects_list",
        "List projects: [{name, description, rev, counts, archived, paused, resources?,\nicon?}]; counts maps step status -> number of steps in the project's plan; resources,\nwhen it declares any, maps each to {\"capacity\": n} or {\"capacity_fn\": fn}; icon, when\nthe project has one, is {\"kind\": \"image\", \"type\": <content type>} or {\"kind\": \"text\",\n\"text\": <text>}.",
    ),
    (
        "project_create",
        "Create a project with an empty plan (rev 1). Returns {name}.\n\nArgs:\n    name: lowercase letters, digits, - and _ (starting with a letter or digit).\n    description: what the project is for; put any context an orchestrator needs here.\n    icon: the project's icon: an absolute path to an image file (SVG, PNG, WebP, JPEG\n        or GIF, at most 256 KB, copied into the project's row), or a short\n        text icon (an emoji; at most 16 characters).\n    resources: named capacities its steps' `needs` draw on (docs(\"plans\")):\n        {name: n} or {name: {\"capacity\": n}} for a fixed capacity (an integer >= 0),\n        {name: {\"capacity_fn\": \"<fn>\"}} for one a fn the project sees returns as\n        {capacity}, called about every 10 s, e.g. {\"lane\": 56, \"cpu\":\n        {\"capacity_fn\": \"cpu-free\"}}.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "project_update",
        "Update a project by current name or id:<uuid>, including new_name, description, icon, resources, paused or archived. expected_settings_rev fences stale settings. Each change records author and reason. Returns project_id and current name.",
    ),
    (
        "project_delete",
        "Delete an archived project and its owned state. confirm_name must equal the current name and expected_settings_rev must match. Refused while any step, call or guardian is live. Returns project_id, name and deleted.",
    ),
    (
        "fn_list",
        "List the functions a project sees (built-in, global, then the project's own; without\na project: built-in and global): [{name, doc, inputs, outputs, scope, icon?}]. A function\nwith a problem (bad fn.json, name collision) carries `error`; see verify.\n\nArgs:\n    project: the project whose functions to list.",
    ),
    (
        "fn_get",
        "Return one function's fn.json plus its `scope` and `path` (its directory).\n\nArgs:\n    name: the function name, e.g. \"git.head\".\n    project: look it up as this project sees it (project functions included).",
    ),
    (
        "fn_save",
        "Create or replace a function: writes fns/<name>/fn.json and main.py into the project\n(or, without one, the global fns dir). Returns {scope, path}. Read docs(\"fns\") first.\n\nArgs:\n    fn: the fn.json: {name, doc?, inputs: {name: type}, outputs: {name: type}}. Checked\n        before anything is written; the name may not collide with a built-in or global\n        function (nor, for a global function, with any project's own).\n    main_py: the Python source of main.py (a uv script calling sluice.fn.run).\n    project: the project that owns it; leave out for a global function.",
    ),
    (
        "fn_call",
        "Run one function outside the plan. Returns {call, status, outputs?, error?}; status\nis pending, running, succeeded or failed. Poll call_status(call) for a slow one; every\nstatus change is also a `call` record in the log (log_read).\n\nArgs:\n    name: the function to run.\n    inputs: an object keyed by the function's input names; checked against its types\n        before anything runs (an `invalid` error lists every mismatch with its path).\n    project: run it in this project (its functions and .env); leave out for none.\n    wait: how many seconds to wait for the result, a number such as 60 (default 0:\n        return at once; capped at 3600).\n    direct: run it here and now, to the end, instead of queueing it for the runner\n        (for use without a runner, e.g. from the command line); ignores `wait`.\n    author: who is calling (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "call_status",
        "Return {call, status, outputs?, error?, stderr_tail?} for a fn_call.\n\nArgs:\n    call: the id fn_call returned.\n    project: the project it ran in, if any.",
    ),
    (
        "plan_get",
        "Return {rev, plan}: the project's plan (without rev) and its revision.\n\nArgs:\n    project: the project.",
    ),
    (
        "plan_patch",
        "Edit a project's plan with RFC 6902 JSON Patch ops. A step it adds starts as soon\nas it is ready; pass start=false to add it paused (a draft), and unpause it with\nstep_pause. Cap how many run at once with resources and `needs`. Returns {rev}.\n\nArgs:\n    project: the project.\n    rev: the revision you read; if the plan moved on you get `conflict` with\n        current_rev, so re-read and retry.\n    ops: JSON Patch operations against the plan without rev, e.g.\n        [{\"op\": \"add\", \"path\": \"/steps/x\", \"value\": {...}}]. The result is validated;\n        an `invalid` error lists every problem with its path.\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n    start: false adds the new steps paused (unless a step sets `paused` itself).\n\ndry_run: preview without committing. Every edit records author and reason.",
    ),
    (
        "step_add",
        "Add one step to a plan: plan_patch for a single step, at the current rev. It\nstarts as soon as it is ready; start=false (or `paused` in the spec) adds it paused.\nReturns {rev}.\n\nArgs:\n    project: the project.\n    step: the new step's id.\n    spec: the step, {run, in, scatter?, doc?, outputs?, paused?, after?, tags?}.\n    reason: why, recorded in the plan's history.\n    start: false adds it paused.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing. Every edit records author and reason.",
    ),
    (
        "recipe_list",
        "List the recipes a project sees: [{name, doc, params, scope}] by name (scope global:\nSLUICE_HOME/recipes/, or project: the project's own recipes/, which wins on a name\nclash); a broken recipe file is listed as {name, scope, error}. params maps each param\nto its type, `unit` (always there) included. Read docs(\"plans\") on recipes.\n\nArgs:\n    project: the project.",
    ),
    (
        "unit_add",
        "Add one unit of work from a recipe: its steps with `{param}` filled in, each tagged\n`unit:<unit>` (and `tags`), with the edges and input overrides given, in one plan\nedit at the current rev: one call stages a whole lane. They start as soon as they are\nready; start=false adds them paused. Refused (`bad_request`) when an id it would add\nis already in the plan; `invalid` lists every param, expansion or staging problem\n(nothing is written). Returns {rev, steps}.\n\nArgs:\n    project: the project.\n    recipe: the recipe's name (recipe_list).\n    params: {unit: \"<name of the unit, a valid step id>\", <param>: value, ...}, each\n        checked against the recipe's param types.\n    start: false adds the new steps paused.\n    tags: more tags for every step of the unit, e.g. [\"arc:tsvm\"] (select by them in\n        status, step_pause, step_cancel, plan_prune, ...); `unit:` ones are reserved.\n    after: {suffix: [step ids]}: ids appended to that recipe step's `after` (its own\n        kept). A suffix is the recipe step's id without the leading \"<unit>-\", e.g.\n        {\"fork\": [\"fig-4200-landed\"]}.\n    when: {suffix: \"<ref>\"}: that step's `when` (replacing the recipe's).\n    inputs: {suffix: {input: value}}: literals bound to that step's inputs\n        ({\"default\": value}, replacing the recipe's binding), e.g.\n        {\"work\": {\"effort\": \"xhigh\"}}; an input its fn does not declare and the recipe\n        does not bind is refused.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n    reason: why, recorded in the plan's history (default \"add unit <unit> (recipe\n        <recipe>)\").\n\ndry_run: preview without committing. Every edit records author and reason. tags adds unit tags; inputs binds inputs by entry step.",
    ),
    (
        "unit_tag",
        "Add and/or remove tags on every step of a unit (those tagged `unit:<unit>`), in one\nplan edit at the current rev, e.g. to put units into an arc after the fact. `unit:`\ntags are reserved; an unknown unit is `not_found`. Nothing to change: no edit, the\ncurrent rev. Returns {rev, steps}: the unit's steps.\n\nArgs:\n    project: the project.\n    unit: the unit's name.\n    add: tags to add, e.g. [\"arc:tsvm\"].\n    remove: tags to remove.\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing. Every edit records author and reason.",
    ),
    (
        "edge_add",
        "Make a step run after other steps: append them to its `after` (deduplicated, its\nexisting edges kept) in one plan edit at the current rev, with no rev to read and no\nway to drop an edge someone else added. Edges already there: no edit, the current rev.\nAn unknown step is `invalid`, and so is a cycle. Returns {rev, after}: its `after`\nnow. (A step's one `when` is set with step_update.)\n\nArgs:\n    project: the project.\n    step: the step that waits.\n    after: the step id(s) it waits for.\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing. Every edit records author and reason.",
    ),
    (
        "edge_remove",
        "Remove ids from a step's `after` in one plan edit at the current rev (the others\nkept). Edges not there: no edit, the current rev. An unknown step is `invalid`.\nReturns {rev, after}: its `after` now.\n\nArgs:\n    project: the project.\n    step: the step that waits.\n    after: the step id(s) it should no longer wait for.\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing. Every edit records author and reason.",
    ),
    (
        "step_update",
        "Change fields of one step: each key of `changes` replaces that field (`in` is\nreplaced whole), null removes it. A running step only takes `paused` and `tags`.\nReturns {rev}.\n\nArgs:\n    project: the project.\n    step: the step id.\n    changes: e.g. {\"doc\": \"...\", \"in\": {...}}.\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing. Every edit records author and reason.",
    ),
    (
        "step_remove",
        "Remove steps from a plan in one edit, selected by ids and/or tags. Refused\n(`invalid`) while a step left or a plan output still reads one, or while one runs.\nEach one that finished keeps its outcome (the `outcomes` table, see query). Returns\n{rev, steps, outcomes}: outcomes is how many were kept.\n\nArgs:\n    project: the project.\n    steps: step ids (one id is fine too).\n    tags: every step carrying any of these tags.\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing. Every edit records author and reason.",
    ),
    (
        "step_pause",
        "Pause or unpause steps in one edit. A paused step does not start, however ready\nits inputs, until unpaused; a running one finishes (pausing never stops it: see\nstep_cancel). Select by ids and/or tags; with subtree, also everything downstream\n(steps that read from or run after them, transitively), including those that become\nready later. Returns {rev, steps}: the steps selected.\n\nArgs:\n    project: the project.\n    steps: step ids (one id is fine too).\n    tags: select every step carrying any of these tags.\n    subtree: include everything downstream of the selected steps.\n    paused: true to pause, false to let them start.\n    reason: why; kept on each paused step (status shows it) and in the history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing. Every edit records author and reason.",
    ),
    (
        "step_cancel",
        "Stop running steps, selected by ids and/or tags: the runner kills their processes\nand fails each with `cancelled: <reason>`; step_retry runs them again. A pending\ncore.external step (work done outside sluice) fails the same way at once. Refused\nunless every selected step is running or a pending core.external one. Returns {steps}.\n\nArgs:\n    project: the project.\n    steps: step ids (one id is fine too).\n    tags: every step carrying any of these tags.\n    reason: why, in their errors and `step.cancel` log records.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "plan_history",
        "Return the plan's log: edits {rev, at, author, reason, ops} and manual values\n{rev, at, author, reason, action, ...}.\n\nArgs:\n    project: the project.\n    since_rev: only entries after this revision.",
    ),
    (
        "plan_set_input",
        "Set a declared plan input; steps reading it can then start. Changing it later makes\nthe succeeded steps that read it (and their dependents) stale. Returns {ok}.\n\nArgs:\n    project: the project.\n    name: the plan input's name.\n    value: its value, checked against the input's type.\n    reason: why, recorded in the plan's history.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing. Every edit records author and reason.",
    ),
    (
        "step_set_input",
        "Set an inputs map on selected steps or tags. Changes apply atomically at rev if supplied. dry_run reports changed, running and unsupported targets without committing. reason and author record why and who.",
    ),
    (
        "step_set_output",
        "Mark a step succeeded with outputs you supply (manual: true); it is not run unless\nretried. Refused (`invalid`, naming them) while a step it reads has not succeeded or a\nplan input it reads has no value, unless force. Returns {ok}.\n\nArgs:\n    project: the project.\n    step: a step that is not running.\n    outputs: every output of its function, type-checked (arrays for a scattered step).\n    reason: why, recorded in the plan's history.\n    force: set it anyway although what it reads is not ready (e.g. a broken\n        upstream); the step then turns stale once those values are all there.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "step_retry",
        "Retry selected finished steps and re-arm their blocked region. message resumes a paused worker with an authored message. steps and tags select targets. Returns steps, rearmed and stopped_at.",
    ),
    (
        "step_submit",
        "Submit the outputs a running step declares (its `outputs`), from the agent doing\nthe step's work. Checked against the declared outputs: every required one present,\ntypes fitting, no others; `invalid` lists every mismatch with its path, so fix them\nand submit again. Submitting again replaces what was sent. When the step's fn exits,\nthese join its outputs; a required one never submitted fails the step. Returns\n{ok, run}.\n\nArgs:\n    project: the project.\n    step: the running step.\n    outputs: an object keyed by declared output name.\n    run: the run id (SLUICE_RUN_ID); needed only when the step runs several times at\n        once (scatter).\n    author: who is submitting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "log_read",
        "Read the log: {records, last_seq}. Records are {seq, at, kind, ...} oldest first\n(seqs increase across the whole home, so one log's have gaps);\nkinds: plan.edit, plan.input, step.output, step.retry, step.status, step.submit,\nstep.cancel, call, message, run.adopt, run.orphan, project.pause, project.archive, project.update.\n\nArgs:\n    project: the project's log; leave out for the home log (calls without a project).\n    since_seq: only records after this seq (pass the last_seq you got to continue).\n        Without it: the last `limit` matching records.\n    kinds: only these kinds; \"step\", \"plan\", \"inbox\", \"run\" or \"project\" match every\n        kind under them.\n    threads: only messages on these threads (and, without kinds, only messages).\n    limit: at most this many records (default 200).",
    ),
    (
        "log_wait",
        "Wait for log records after since_seq: returns {records, last_seq} as soon as at least\none matching record exists, or with no records once `timeout` seconds pass. Call it\nagain with the last_seq it returned to keep watching.\n\nArgs:\n    since_seq: wait for records after this seq (0 for any; last_seq from log_read).\n    project: the project's log; leave out for the home log.\n    kinds: only these kinds (as in log_read).\n    threads: only messages on these threads (as in log_read).\n    timeout: seconds to wait at most (default 300; capped at 3600).\n    limit: at most this many records (default 200).\n    wake: \"any\" (default) or \"questions\": a note (a message posted with needs_reply\n        false) does not end the wait; it comes back with the next record that does,\n        or once `timeout` passes.",
    ),
    (
        "next",
        "Wait for the records an orchestrator should act on across the projects, then\nreturn {records, notes, last_seq, timed_out}. Wakes at once on a step that failed,\nwent stale or was skipped (inside a unit too); a message needing a reply, not from\nyou, addressed to `me` or to nobody; an answering reply in the project not given by you. A unit (the steps\ntagged with the same unit, else a unit of one) wakes once, when it\nsettles — none of its steps running or pending and startable — never on its steps'\nown successes; its record carries `unit: {name, settled, steps: [{id, status,\nheld?, outputs, omitted?}]}` with the succeeded steps' outputs as `settles` says, on\nthe failure itself when a failure settled it. A standalone step wakes on a success\nwhen its fn is open. After the first waking record it keeps collecting until\n`settle` seconds pass with no new one, or `settle_max` seconds after the first.\nMessages come first in `records`, whole. `notes` are the notes (messages with\nneeds_reply false) held on the way — read them before the records. Pass `last_seq`\nback as `since_seq` to continue; nothing is missed or repeated. The command-line\nform is `sluice next`.\n\nArgs:\n    projects: the projects to watch (one or several).\n    since_seq: records after this seq (the last_seq you last got).\n    me: your name; your own messages never wake it (default \"orchestrator\").\n    timeout: seconds to wait at most for the first waking record (default 300;\n        capped at 3600); on a timeout records is empty and timed_out is true.\n    all: every record wakes it (default false).\n    settle: seconds with no new waking record that end the batch (default 20; 0\n        returns at the first).\n    settle_max: seconds after the first waking record that end the batch at the\n        latest (default 120; capped at 3600).\n    settles: how much of a settled unit's outputs each step carries: \"short\"\n        (default) only booleans, numbers, strings of at most 80 characters and a\n        `summary`'s first line (cut to 200), the rest named in `omitted` (read them\n        with status or query); \"full\" every output whole; \"none\" no outputs.",
    ),
    (
        "drain",
        "Pause the projects (default: every project not archived) that are not already\npaused, recording which ones in SQLite so `release` lets exactly those go\nagain. Returns {paused, pending}: `pending` is the running steps and live\nnon-direct calls still to finish — the CLI's `sluice drain` waits for them.\n\nArgs:\n    projects: the projects to drain; leave out for every project not archived.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "release",
        "Unpause exactly the projects the maintenance ledger lists — what `sluice drain --release`\ndoes — and clear ownership. Projects paused otherwise stay paused. Returns {released}.\n\nArgs:\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "step_context",
        "Where a step stands, for the agent doing it — the same as `sluice me` inside the\nstep: its fn, doc, status and running time, inputs, the status and short outputs of\nevery step it reads or runs after, the unanswered messages on its thread\n(step-<id>), the outputs it must submit with the exact step_submit command, and the\nthread with the command to ask a question.\n\nArgs:\n    project: the project.\n    step: the step id.",
    ),
    (
        "query",
        "Run a read-only SQL query against the home database. Returns columns, rows and truncated. sql is one statement, params supplies bound values, limit defaults to 200 and cannot exceed 1000. Mutation, ATTACH, extensions and unsafe functions are refused. Execution has a two-second progress deadline and a 1 MiB response bound.",
    ),
    (
        "verify",
        "Check functions (fn.json shape and types, name collisions), .env files, each\nproject's plan and its state. Returns {ok, problems: [{where, message}], warnings?}\n(warnings: directories under projects/ of no project); changes nothing.\n\nArgs:\n    project: check this project (and the built-in and global functions it sees);\n        leave out to check everything.",
    ),
    (
        "plan_view",
        "Draw the plan with each step's status: a Mermaid flowchart or a standalone HTML page.\nDone units (independent pieces of work whose every step succeeded or was skipped) are\nleft out, with one line saying how many, unless all is true.\n\nArgs:\n    project: the project.\n    format: \"mermaid\" (default) or \"html\".\n    all: include the done units too.",
    ),
    (
        "status",
        "Read project status, outputs, resources and step state. steps and tags select targets.",
    ),
    (
        "plan_prune",
        "Prune settled units, selected by units or tags and older_than in seconds. dry_run previews without committing. rev is the revision you read, reason and author record the edit.",
    ),
    (
        "message_post",
        "Post a message or answer on a project thread. body is markdown. reply_to joins the parent thread, to defaults to its sender. needs_reply distinguishes questions from notes. The first answer resolves a question atomically, including an input value; stale UI answers conflict. from and author name the sender and actor. title, ui, input, data and run carry optional metadata.",
    ),
    (
        "messages",
        "Read project messages using view: inbox, questions, history or thread. thread selects the full conversation for the thread view. since filters by message id.",
    ),
];
