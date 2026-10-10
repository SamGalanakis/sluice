//! Transport contracts tested without a store, scheduler, processes or live home.
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use serde_json::{Value, json};
use sluice_model::{
    commands::{CommandReply, CommandRequest, PlanViewFormat, StepStatus},
    error::PublicError,
    ids::*,
    plan_rows::*,
};
use sluice_runtime::dispatch_ext::plan_values::*;
use sluice_web::mcp::{self, CommandService, McpServer};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

fn fixture(name: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/../sluice-model/tests/fixtures/plan_rows/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap()
}
fn reply(name: &str) -> CommandReply {
    let value = fixture(name);
    match name {
        "plan_get.reply" => CommandReply::Plan(serde_json::from_value(value).unwrap()),
        "plan_read.reply" | "plan_read.full.reply" => {
            CommandReply::PlanRead(serde_json::from_value(value).unwrap())
        }
        "step_get.reply" => CommandReply::Step(serde_json::from_value(value).unwrap()),
        "unit_get.reply" => CommandReply::Unit(serde_json::from_value(value).unwrap()),
        "plan_history.reply" => CommandReply::History(serde_json::from_value(value).unwrap()),
        "plan_edit.reply" | "unit_remove.reply" => {
            CommandReply::Edit(serde_json::from_value(value).unwrap())
        }
        "plan_edit.dry_run.reply" => CommandReply::Preview(serde_json::from_value(value).unwrap()),
        "step_set_input.reply" => CommandReply::Inputs(serde_json::from_value(value).unwrap()),
        "plan_prune.reply" => CommandReply::Pruned(serde_json::from_value(value).unwrap()),
        _ => panic!("no reply fixture {name}"),
    }
}
struct FixtureService {
    answer: CommandReply,
    received: Mutex<Vec<CommandRequest>>,
}
impl CommandService for FixtureService {
    fn command(
        &self,
        request: CommandRequest,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CommandReply, PublicError>> + Send + '_>,
    > {
        self.received.lock().unwrap().push(request);
        Box::pin(async { Ok(self.answer.clone()) })
    }
}
fn map(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[tokio::test]
async fn mcp_http_and_cli_decode_the_same_flat_arguments_and_fixture_replies() {
    for (tool, request_fixture, reply_fixture) in [
        ("plan_get", None, "plan_get.reply"),
        ("plan_read", Some("plan_read.request"), "plan_read.reply"),
        (
            "plan_read",
            Some("plan_read.full.request"),
            "plan_read.full.reply",
        ),
        ("step_get", Some("step_get.request"), "step_get.reply"),
        ("unit_get", Some("unit_get.request"), "unit_get.reply"),
        (
            "plan_history",
            Some("plan_history.request"),
            "plan_history.reply",
        ),
        ("plan_edit", Some("plan_edit.request"), "plan_edit.reply"),
        (
            "plan_edit",
            Some("plan_edit.dry_run.request"),
            "plan_edit.dry_run.reply",
        ),
        (
            "unit_update",
            Some("unit_update.request"),
            "plan_edit.reply",
        ),
        (
            "unit_remove",
            Some("unit_remove.request"),
            "unit_remove.reply",
        ),
        ("step_set_input", None, "step_set_input.reply"),
        ("plan_prune", None, "plan_prune.reply"),
    ] {
        let mut args = request_fixture.map(fixture).unwrap_or_else(|| match tool {
            "step_set_input" => {
                json!({"project":"lash","steps":["release"],"inputs":{"engine":"codex"}})
            }
            "plan_prune" => json!({"project":"lash"}),
            _ => json!({"project":"demo"}),
        });
        // Explicit authors make all three transport requests exactly equal.
        if mcp::tool_schema(tool).unwrap()["properties"]
            .get("author")
            .is_some()
        {
            args["author"] = json!("sam");
        }
        let expected = mcp::decode_tool(tool, map(args.clone()), Some("test")).unwrap();
        assert!(sluice::plan_tools::handles(tool));
        assert_eq!(
            sluice::plan_tools::decode(tool, map(args.clone()), "test").unwrap(),
            expected
        );
        let service = Arc::new(FixtureService {
            answer: reply(reply_fixture),
            received: Mutex::new(vec![]),
        });
        let server = McpServer::new(service.clone());
        let result = server.call(tool, map(args.clone()), Some("test")).await;
        assert_ne!(result.is_error, Some(true), "{tool}: {result:?}");
        assert_eq!(
            result.structured_content,
            Some(fixture(reply_fixture)),
            "{tool}"
        );
        let response = mcp::call_http(
            &server,
            tool,
            Request::builder()
                .header("content-type", "application/json")
                .body(Body::from(args.to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), 200, "{tool}");
        let actual: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(actual, fixture(reply_fixture), "{tool}");
        assert_eq!(
            *service.received.lock().unwrap(),
            vec![expected.clone(), expected]
        );
    }
}

#[test]
fn every_new_tool_is_listed_with_flat_schema_and_defaults() {
    for name in [
        "plan_get",
        "plan_read",
        "step_get",
        "unit_get",
        "plan_edit",
        "unit_update",
        "unit_remove",
        "plan_history",
        "plan_view",
    ] {
        assert!(mcp::tools().iter().any(|t| t.name == name), "{name}");
        assert!(sluice::plan_tools::handles(name));
        let schema = mcp::tool_schema(name).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        for hidden in ["edit", "selection", "read"] {
            assert!(schema["properties"].get(hidden).is_none());
        }
        assert_eq!(mcp::renames(name), &[]);
    }
    for name in ["plan_edit", "unit_update", "unit_remove"] {
        let schema = mcp::tool_schema(name).unwrap();
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("reason"))
        );
        assert!(schema["properties"]["reason"].get("default").is_none());
        assert_eq!(schema["properties"]["preview_scope"]["default"], "impact");
    }
    assert_eq!(
        mcp::tool_schema("plan_read").unwrap()["properties"]["compact"]["default"],
        true
    );
    assert_eq!(
        mcp::tool_schema("step_get").unwrap()["properties"]["compact"]["default"],
        false
    );
}

#[test]
fn scalar_filters_are_lists_and_absent_and_empty_filters_differ() {
    for name in ["plan_read", "plan_view"] {
        let request=mcp::decode_tool(name,map(json!({"project":"lash","units":"normalize","steps":"normalize-work","status":"pending"})),None).unwrap();
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["args"]["units"], json!(["normalize"]));
        assert_eq!(value["args"]["steps"], json!(["normalize-work"]));
        assert_eq!(value["args"]["status"], json!(["pending"]));
        let absent = mcp::decode_tool(name, map(json!({"project":"lash"})), None).unwrap();
        let empty =
            mcp::decode_tool(name, map(json!({"project":"lash","units":[]})), None).unwrap();
        assert_ne!(absent, empty);
    }
}

#[tokio::test]
async fn removed_patch_is_unknown_on_every_transport_without_dispatch() {
    let service = Arc::new(FixtureService {
        answer: CommandReply::Ack,
        received: Mutex::new(vec![]),
    });
    let server = McpServer::new(service.clone());
    assert!(!mcp::tools().iter().any(|t| t.name == "plan_patch"));
    assert!(mcp::tool_schema("plan_patch").is_none());
    assert!(!sluice::plan_tools::handles("plan_patch"));
    assert!(sluice::plan_tools::decode("plan_patch", map(json!({})), "cli").is_err());
    assert_eq!(
        server
            .call("plan_patch", map(json!({})), None)
            .await
            .is_error,
        Some(true)
    );
    let response = mcp::call_http(
        &server,
        "plan_patch",
        Request::builder()
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), 400);
    assert!(service.received.lock().unwrap().is_empty());
}

#[test]
fn required_reasons_closed_changes_and_preview_refusals_agree_on_cli_and_mcp() {
    for (name, args) in [
        ("plan_read", json!({"project":"lash","limit":0})),
        ("plan_history", json!({"project":"lash","limit":0})),
        ("plan_edit", json!({"project":"lash","ops":[]})),
        (
            "unit_update",
            json!({"project":"lash","unit":"normalize","changes":{"normalize-work":{"priority":1}}}),
        ),
        ("unit_remove", json!({"project":"lash","unit":"normalize"})),
        (
            "plan_edit",
            json!({"project":"lash","reason":"why","ops":[]}),
        ),
        (
            "step_update",
            json!({"project":"lash","step":"work","changes":{"when":true}}),
        ),
        (
            "step_update",
            json!({"project":"lash","step":"work","changes":{}}),
        ),
        (
            "step_add",
            json!({"project":"lash","step":"work","spec":{},"preview_scope":"all"}),
        ),
        (
            "unit_remove",
            json!({"project":"lash","unit":"normalize","reason":"why","preview_scope":"all"}),
        ),
        (
            "plan_edit",
            json!({"project":"lash","reason":"why","ops":[{"op":"step.add","step":"work","spec":{},"unknown":1}]}),
        ),
    ] {
        let mcp = mcp::decode_tool(name, map(args.clone()), Some("test")).unwrap_err();
        let cli = sluice::plan_tools::decode(name, map(args), "test").unwrap_err();
        assert_eq!(mcp, cli, "{name}");
    }
}

#[test]
fn typed_tools_share_impact_and_full_dry_run_options() {
    for (name, args) in [
        (
            "step_add",
            json!({"project":"lash","step":"work","spec":{}}),
        ),
        (
            "step_update",
            json!({"project":"lash","step":"work","changes":{"priority":1}}),
        ),
        ("step_remove", json!({"project":"lash","steps":"work"})),
        ("step_pause", json!({"project":"lash","steps":"work"})),
        ("unit_tag", json!({"project":"lash","unit":"work"})),
        (
            "unit_add",
            json!({"project":"lash","recipe":"lane","unit":"work"}),
        ),
        (
            "edge_add",
            json!({"project":"lash","step":"work","after":["gate"]}),
        ),
        (
            "edge_remove",
            json!({"project":"lash","step":"work","after":["gate"]}),
        ),
        (
            "step_set_input",
            json!({"project":"lash","steps":"work","inputs":{"engine":"codex"}}),
        ),
        ("plan_prune", json!({"project":"lash"})),
    ] {
        let base = mcp::decode_tool(name, map(args.clone()), None).unwrap();
        assert_eq!(
            serde_json::to_value(base).unwrap()["args"]["edit"]["preview_scope"],
            "impact",
            "{name}"
        );
        let mut all = args;
        all["preview_scope"] = json!("all");
        all["dry_run"] = json!(true);
        all["rev"] = json!(41);
        let request = sluice::plan_tools::decode(name, map(all), "cli").unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap()["args"]["edit"]["expected"],
            41
        );
    }
}

#[test]
fn fixture_reply_keys_match_tool_descriptions() {
    for (tool, reply_fixture) in [
        ("plan_get", "plan_get.reply"),
        ("plan_read", "plan_read.reply"),
        ("step_get", "step_get.reply"),
        ("unit_get", "unit_get.reply"),
        ("plan_history", "plan_history.reply"),
        ("plan_edit", "plan_edit.reply"),
        ("unit_update", "plan_edit.reply"),
        ("unit_remove", "unit_remove.reply"),
    ] {
        let description = mcp::tools()
            .iter()
            .find(|t| t.name == tool)
            .unwrap()
            .description
            .as_deref()
            .unwrap();
        let rest = &description[description.find("Returns {").unwrap() + 9..];
        let fields = rest.split('}').next().unwrap();
        for field in fields
            .split(',')
            .map(str::trim)
            .filter(|f| !f.ends_with('?'))
        {
            assert!(
                fixture(reply_fixture).get(field).is_some(),
                "{tool} describes absent {field}"
            );
        }
    }
    for tool in mcp::tools() {
        assert!(
            !tool
                .description
                .as_deref()
                .unwrap_or("")
                .contains("plan_patch")
        );
    }
}

fn row(id: &str, position: u64) -> StepRowView {
    StepRowView {
        step: StepId::new(id).unwrap(),
        position,
        run: "agent.run".into(),
        unit: UnitName::new("normalize").unwrap(),
        priority: 0,
        paused: PauseValue::Flag(false),
        status: StepStatus::Pending,
        declaration: None,
    }
}

#[test]
fn projected_full_steps_keep_authored_spec_and_scoped_reference_order() {
    let value = fixture("plan_read.full.reply");
    let expected: FullStep = serde_json::from_value(value["steps"][0].clone()).unwrap();
    let full_row = StepRowView {
        step: expected.id.clone(),
        unit: expected.unit.clone(),
        position: expected.position,
        run: expected.run.clone(),
        priority: expected.priority,
        paused: expected.paused.clone(),
        status: expected.status.clone(),
        declaration: Some(expected.spec.clone()),
    };
    let mut refs: Vec<_> = expected
        .references
        .iter()
        .map(|r| ReferenceRow {
            consumer_kind: ConsumerKind::Step,
            consumer_id: expected.id.to_string(),
            slot: r.slot.clone(),
            ordinal: r.ordinal,
            kind: r.kind,
            source_kind: r.source_kind,
            source_id: r.source_id.clone(),
            source_port: r.source_port.clone(),
            source_path: r.source_path.clone(),
        })
        .collect();
    refs.reverse();
    let mut other = refs[0].clone();
    other.consumer_id = "unread".into();
    refs.push(other);
    assert_eq!(
        step_views(vec![full_row], ReferenceRows(refs), &BTreeMap::new()).unwrap(),
        vec![StepView::Full(expected)]
    );
    let compact = step_views(
        vec![row("work", 0)],
        ReferenceRows(vec![]),
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(
        serde_json::to_value(&compact[0])
            .unwrap()
            .get("spec")
            .is_none()
    );
}

#[test]
fn cursor_binds_only_the_tokens_its_filters_need() {
    let project: ProjectId = "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b".parse().unwrap();
    let generation = RecipeGeneration("5f0c1e9a7b3d2468".into());
    let rows = StepRows {
        rev: Revision(41),
        state_epoch: StateEpoch(9120),
        steps: vec![row("normalize-work", 503)],
        more: true,
    };
    let CommandRequest::PlanRead(mut request) =
        mcp::decode_tool("plan_read", map(fixture("plan_read.request")), None).unwrap()
    else {
        panic!()
    };
    request.cursor = next_cursor(&request, project, &rows, &generation);
    assert_eq!(
        request.cursor,
        Some(
            fixture("plan_read.reply")["next_cursor"]
                .as_str()
                .unwrap()
                .into()
        )
    );
    assert_eq!(
        cursor_after(
            &request,
            project,
            Revision(41),
            StateEpoch(9120),
            &generation
        )
        .unwrap(),
        Some((503, StepId::new("normalize-work").unwrap()))
    );
    assert!(matches!(
        cursor_after(
            &request,
            project,
            Revision(41),
            StateEpoch(9121),
            &generation
        ),
        Err(PublicError::CursorExpired { .. })
    ));
    request.units = None;
    assert!(matches!(
        cursor_after(
            &request,
            project,
            Revision(41),
            StateEpoch(9120),
            &generation
        ),
        Err(PublicError::BadRequest { .. })
    ));
    request.status = None;
    request.cursor = next_cursor(&request, project, &rows, &generation);
    assert!(
        cursor_after(
            &request,
            project,
            Revision(41),
            StateEpoch(9999),
            &RecipeGeneration("0000000000000000".into())
        )
        .is_ok()
    );
    assert!(
        cursor_after(
            &request,
            project,
            Revision(42),
            StateEpoch(9999),
            &generation
        )
        .is_err()
    );
    request.recipe = Some("lane".into());
    request.cursor = next_cursor(&request, project, &rows, &generation);
    assert!(
        cursor_after(
            &request,
            project,
            Revision(41),
            StateEpoch(9999),
            &RecipeGeneration("0000000000000000".into())
        )
        .is_err()
    );
}

#[test]
fn filtered_graph_keeps_both_directions_of_boundary_edges() {
    let graph = GraphRows {
        steps: vec![row("work", 1)],
        boundary: vec![row("source", 0), row("reader", 2)],
        edges: vec![
            EdgeRow {
                source: StepId::new("source").unwrap(),
                target: StepId::new("work").unwrap(),
                kind: EdgeKind::Data,
                via_unit: None,
            },
            EdgeRow {
                source: StepId::new("work").unwrap(),
                target: StepId::new("reader").unwrap(),
                kind: EdgeKind::Gate,
                via_unit: Some(UnitName::new("normalize").unwrap()),
            },
        ],
    };
    let text = render_graph("lash", &graph, PlanViewFormat::Mermaid, "");
    assert!(text.contains("ext_source[\"source · outside\"]:::outside"));
    assert!(text.contains("ext_source -->|\"data\"| s_work"));
    assert!(text.contains("s_work -->|\"unit:normalize\"| ext_reader"));
    assert!(text.contains("%% 2 steps outside the selection drawn as boundary nodes"));
    let html = render_graph(
        "<lash>",
        &graph,
        PlanViewFormat::Html,
        "1 done units (3 steps) left out",
    );
    assert!(html.contains("&lt;lash&gt;"));
    assert!(html.contains("<svg"));
    assert!(html.contains("reader · outside"));
}

#[tokio::test]
async fn plan_view_filters_decode_and_text_reply_survives_all_adapters() {
    let args = fixture("plan_view.request");
    let expected = mcp::decode_tool("plan_view", map(args.clone()), None).unwrap();
    assert_eq!(
        sluice::plan_tools::decode("plan_view", map(args.clone()), "cli").unwrap(),
        expected
    );
    let text = "flowchart TD\n  s_work[\"work\"]\n";
    let service = Arc::new(FixtureService {
        answer: CommandReply::Data(json!(text).try_into().unwrap()),
        received: Mutex::new(vec![]),
    });
    let server = McpServer::new(service.clone());
    let result = server.call("plan_view", map(args.clone()), None).await;
    assert_eq!(result.content[0].as_text().unwrap().text, text);
    let response = mcp::call_http(
        &server,
        "plan_view",
        Request::builder()
            .header("content-type", "application/json")
            .body(Body::from(args.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        to_bytes(response.into_body(), 1024 * 1024).await.unwrap(),
        text.as_bytes()
    );
    assert_eq!(
        *service.received.lock().unwrap(),
        vec![expected.clone(), expected]
    );
}

#[test]
fn recipe_and_unit_filters_intersect_and_empty_selections_stay_empty() {
    let recipes = BTreeMap::from([
        ("a".into(), "lane".into()),
        ("b".into(), "lane".into()),
        ("c".into(), "other".into()),
    ]);
    let filter = PlanReadFilter {
        units: Some(vec![
            UnitName::new("a").unwrap(),
            UnitName::new("c").unwrap(),
        ]),
        recipe: Some("lane".into()),
        ..Default::default()
    };
    assert_eq!(
        row_selection(&filter, &recipes).units,
        Some(vec![UnitName::new("a").unwrap()])
    );
    let filter = PlanReadFilter {
        recipe: Some("missing".into()),
        ..Default::default()
    };
    assert_eq!(row_selection(&filter, &recipes).units, Some(vec![]));
    let filter = PlanReadFilter {
        units: Some(vec![]),
        ..Default::default()
    };
    assert_eq!(row_selection(&filter, &recipes).units, Some(vec![]));
    assert_eq!(
        row_selection(&PlanReadFilter::default(), &recipes).units,
        None
    );
}
