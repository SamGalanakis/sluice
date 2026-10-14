use crate::scratch::ScratchHome;
use serde_json::{Value, json};
use std::{path::Path, process::Command};

fn call(home: &Path, name: &str, args: Value) -> Result<Value, Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", home)
        .env("TOKIO_WORKER_THREADS", "2")
        .env("SLUICE_ME", "@test")
        .env_remove("SLUICE_RUN_ID")
        .env_remove("SLUICE_PROJECT_ID")
        .args(["tool", name, &args.to_string()])
        .output()
        .unwrap();
    if output.status.success() {
        Ok(serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| json!(String::from_utf8(output.stdout).unwrap())))
    } else {
        assert_eq!(output.status.code(), Some(1));
        Err(serde_json::from_slice(&output.stderr).unwrap())
    }
}

#[test]
fn plan_tool_projections_filters_and_graph() {
    let (scratch, specs) = plan_fixture();
    let home = scratch.path();
    let all = call(home, "plan_read", json!({"project":"p"})).unwrap();
    assert_eq!(all["steps"].as_array().unwrap().len(), 3);
    assert!(
        all["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s.get("spec").is_none())
    );
    for filter in [json!({"units":[]}), json!({"recipe":"missing"})] {
        let mut args = json!({"project":"p"});
        args.as_object_mut()
            .unwrap()
            .extend(filter.as_object().unwrap().clone());
        assert_eq!(call(home, "plan_read", args).unwrap()["steps"], json!([]));
    }
    let selected = call(
        home,
        "plan_read",
        json!({"project":"p","units":"b","steps":"work","status":"pending","compact":false}),
    )
    .unwrap();
    assert_eq!(selected["steps"].as_array().unwrap().len(), 1);
    assert_eq!(selected["steps"][0]["id"], "work");
    assert_eq!(selected["steps"][0]["spec"], specs["work"]);
    assert_eq!(
        selected["steps"][0]["references"],
        json!([{
            "kind":"binding","slot":"in.value","ordinal":0,"source_kind":"step",
            "source_id":"source","source_port":"value","source_path":""
        }])
    );
    let full = call(home, "step_get", json!({"project":"p","step":"work"})).unwrap();
    assert_eq!(full["step"]["spec"], specs["work"]);
    let reader = call(home, "step_get", json!({"project":"p","step":"reader"})).unwrap();
    assert_eq!(reader["step"]["spec"], specs["reader"]);
    assert_eq!(
        reader["step"]["references"],
        json!([
            {"kind":"gate","slot":"after","ordinal":0,"source_kind":"step","source_id":"work","source_port":"","source_path":""},
            {"kind":"binding","slot":"in.value","ordinal":0,"source_kind":"step","source_id":"work","source_port":"value","source_path":""}
        ])
    );
    let graph = call(
        home,
        "plan_view",
        json!({"project":"p","steps":"work","format":"mermaid"}),
    )
    .unwrap();
    let graph = graph.as_str().unwrap();
    assert!(graph.contains("ext_source -->|\"data\"| s_work"), "{graph}");
    assert!(
        graph
            .lines()
            .any(|line| line.contains("s_work -->") && line.trim().ends_with("ext_reader")),
        "{graph}"
    );
    std::fs::create_dir_all(home.join("recipes")).unwrap();
    std::fs::write(
        home.join("recipes/lane.json"),
        json!({"name":"lane","params":{},"steps":{
            "{unit}-work":{"run":"core.external","outputs":{"value":"int"}}
        }})
        .to_string(),
    )
    .unwrap();
    for unit in ["r1", "r2"] {
        call(
            home,
            "unit_add",
            json!({"project":"p","recipe":"lane","params":{"unit":unit},"start":false}),
        )
        .unwrap();
    }
    let intersect = call(
        home,
        "plan_read",
        json!({"project":"p","recipe":"lane","units":["a","r1"]}),
    )
    .unwrap();
    assert_eq!(intersect["steps"].as_array().unwrap().len(), 1);
    assert_eq!(intersect["steps"][0]["id"], "r1-work");
}

fn plan_fixture() -> (ScratchHome, Value) {
    let scratch = ScratchHome::served().unwrap();
    let home = scratch.path();
    call(home, "project_create", json!({"name":"p"})).unwrap();
    let specs = json!({
        "source":{"run":"core.external","outputs":{"value":"int"},"tags":["unit:a"]},
        "work":{"run":"core.echo","in":{"value":{"source":"source/value"}},"paused":true,"tags":["unit:b"]},
        "reader":{"run":"core.echo","in":{"value":{"source":"work/value"}},"after":["work"],"paused":true,"tags":["unit:c"]}
    });
    let ops: Vec<Value> = specs
        .as_object()
        .unwrap()
        .iter()
        .map(|(step, spec)| json!({"op":"step.add","step":step,"spec":spec}))
        .collect();
    call(
        home,
        "plan_edit",
        json!({"project":"p","rev":1,"reason":"plan","ops":ops}),
    )
    .unwrap();
    (scratch, specs)
}

#[test]
fn plan_tool_cursors_previews_and_refusals() {
    let (scratch, _specs) = plan_fixture();
    let home = scratch.path();
    let page = call(home, "plan_read", json!({"project":"p","limit":1})).unwrap();
    let cursor = page["next_cursor"].as_str().unwrap();
    call(home,"step_set_output",json!({"project":"p","step":"source","outputs":{"value":1},"force":true,"reason":"state change"})).unwrap();
    let next = call(
        home,
        "plan_read",
        json!({"project":"p","limit":1,"cursor":cursor}),
    )
    .unwrap();
    assert_ne!(page["steps"][0]["id"], next["steps"][0]["id"]);
    let mut different = json!({"project":"p","limit":1,"cursor":cursor,"units":"b"});
    assert_eq!(
        call(home, "plan_read", different.clone()).unwrap_err()["error"],
        "bad_request"
    );
    call(
        home,
        "step_update",
        json!({"project":"p","step":"work","changes":{"doc":"updated"}}),
    )
    .unwrap();
    different.as_object_mut().unwrap().remove("units");
    assert_eq!(
        call(home, "plan_read", different).unwrap_err()["error"],
        "cursor_expired"
    );
    let before = call(home, "plan_get", json!({"project":"p"})).unwrap();
    let preview=call(home,"step_update",json!({"project":"p","step":"work","changes":{"priority":5},"rev":before["rev"],"dry_run":true,"preview_scope":"all"})).unwrap();
    assert_eq!(preview["scope"], "all", "{preview}");
    assert_eq!(preview["changes"][0]["declaration"]["priority"], 5);
    assert_eq!(
        call(home, "plan_get", json!({"project":"p"})).unwrap(),
        before
    );
    assert_eq!(
        call(
            home,
            "step_update",
            json!({"project":"p","step":"work","changes":{"priority":5},"rev":1})
        )
        .unwrap_err()["error"],
        "conflict"
    );
    for (name, args) in [
        ("plan_read", json!({"project":"p","limit":0})),
        ("plan_history", json!({"project":"p","limit":0})),
        ("plan_edit", json!({"project":"p","ops":[]})),
        ("unit_remove", json!({"project":"p","unit":"a"})),
        (
            "step_update",
            json!({"project":"p","step":"work","changes":{"when":true}}),
        ),
        (
            "step_update",
            json!({"project":"p","step":"work","changes":{}}),
        ),
        (
            "step_add",
            json!({"project":"p","step":"x","spec":{},"preview_scope":"all"}),
        ),
        ("plan_patch", json!({"project":"p","ops":[]})),
    ] {
        assert!(call(home, name, args).is_err(), "{name}");
    }
    assert_eq!(
        call(home, "plan_get", json!({"project":"p"})).unwrap(),
        before
    );
}
