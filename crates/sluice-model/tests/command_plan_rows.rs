use serde_json::{Value, json};
use sluice_model::{commands::*, plan_rows::*, rpc::decode_json};

fn fixture(name: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/tests/fixtures/plan_rows/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap()
}
fn request(
    name: &str,
    mut args: Value,
) -> Result<CommandRequest, sluice_model::error::PublicError> {
    args["project"] = serde_json::to_value(
        args["project"]
            .as_str()
            .unwrap()
            .parse::<sluice_model::ids::ProjectSelector>()
            .unwrap(),
    )
    .unwrap();
    decode_json(&serde_json::to_vec(&json!({"command":name,"args":args})).unwrap())
}

#[test]
fn every_new_request_is_a_flat_command_variant() {
    for (tool, name) in [
        ("plan_read", "plan_read.request"),
        ("plan_read", "plan_read.next.request"),
        ("plan_read", "plan_read.full.request"),
        ("step_get", "step_get.request"),
        ("unit_get", "unit_get.request"),
        ("plan_edit", "plan_edit.request"),
        ("plan_edit", "plan_edit.dry_run.request"),
        ("unit_update", "unit_update.request"),
        ("unit_remove", "unit_remove.request"),
        ("plan_history", "plan_history.request"),
        ("plan_view", "plan_view.request"),
    ] {
        let command = request(tool, fixture(name)).unwrap();
        command.check_plan_arguments().unwrap();
        let value = serde_json::to_value(&command).unwrap();
        assert_eq!(value["command"], tool);
        assert_eq!(
            decode_json::<CommandRequest>(&serde_json::to_vec(&value).unwrap()).unwrap(),
            command
        );
        assert!(value["args"].get("edit").is_none());
    }
}

#[test]
fn new_reply_variants_return_the_pinned_objects() {
    macro_rules! check {
        ($variant:ident,$ty:ty,$fixture:literal) => {{
            let value = fixture($fixture);
            let reply =
                CommandReply::$variant(serde_json::from_value::<$ty>(value.clone()).unwrap());
            let wire = serde_json::to_value(&reply).unwrap();
            assert_eq!(wire["data"], value);
            assert_eq!(
                decode_json::<CommandReply>(&serde_json::to_vec(&wire).unwrap()).unwrap(),
                reply
            );
        }};
    }
    check!(Plan, PlanGetResult, "plan_get.reply");
    check!(PlanRead, PlanReadResult, "plan_read.reply");
    check!(PlanRead, PlanReadResult, "plan_read.full.reply");
    check!(Step, StepGetResult, "step_get.reply");
    check!(Unit, UnitGetResult, "unit_get.reply");
    check!(History, PlanHistoryPage, "plan_history.reply");
    check!(Edit, EditResult, "plan_edit.reply");
    check!(Edit, EditResult, "unit_remove.reply");
    check!(Preview, EditPreview, "plan_edit.dry_run.reply");
    check!(Inputs, InputEditResult, "step_set_input.reply");
    check!(Pruned, PruneResult, "plan_prune.reply");
}

#[test]
fn old_patch_and_open_changes_are_not_commands() {
    for (tool, args) in [
        (
            "plan_patch",
            json!({"project":"lash","rev":1,"reason":"old","ops":[]}),
        ),
        (
            "step_update",
            json!({"project":"lash","step":"work","changes":{"when":true},"edit":{"dry_run":false,"reason":""}}),
        ),
        (
            "plan_edit",
            json!({"project":"lash","reason":"bad","ops":[{"op":"replace","path":"/steps/work","value":{}}]}),
        ),
        (
            "plan_edit",
            json!({"project":"lash","reason":"bad","ops":[{"op":"step.update","step":"work","changes":{"unknown":1}}]}),
        ),
        (
            "plan_edit",
            json!({"project":"lash","reason":"bad","ops":[{"op":"step.add","step":"work","spec":{},"unknown":1}]}),
        ),
    ] {
        assert!(request(tool, args).is_err(), "{tool}");
    }
}

#[test]
fn reasons_are_required_and_default_options_are_impact() {
    for (tool, args) in [
        (
            "plan_edit",
            json!({"project":"lash","ops":[{"op":"input.put","name":"repo","declaration":"string"}]}),
        ),
        (
            "unit_update",
            json!({"project":"lash","unit":"lane","changes":{"lane-work":{"priority":1}}}),
        ),
        ("unit_remove", json!({"project":"lash","unit":"lane"})),
    ] {
        assert!(request(tool, args).is_err(), "{tool}");
    }
    let CommandRequest::PlanEdit(edit) =
        request("plan_edit", fixture("plan_edit.request")).unwrap()
    else {
        panic!()
    };
    assert_eq!(edit.preview_scope, PreviewScope::Impact);
    assert!(edit.start);
    assert!(!edit.dry_run);
}

#[test]
fn step_changes_keep_absent_and_null_distinct() {
    let CommandRequest::StepUpdate(update)=request("step_update",json!({"project":"lash","step":"work","changes":{"paused":null,"priority":0},"edit":{"reason":"","dry_run":false}})).unwrap() else { panic!() };
    assert_eq!(update.changes.paused, Some(None));
    assert_eq!(update.changes.doc, None);
    assert_eq!(update.edit.preview_scope, PreviewScope::Impact);
    assert_eq!(
        serde_json::to_value(update.changes).unwrap(),
        json!({"paused":null,"priority":0})
    );
}

#[test]
fn supplied_empty_and_full_preview_refusals_are_typed() {
    for (args, message) in [
        (
            json!({"project":"lash","reason":"why","ops":[]}),
            "ops: name at least one operation",
        ),
        (
            json!({"project":"lash","reason":"why","ops":[{"op":"step.update","step":"work","changes":{}}]}),
            "ops[0].changes: name at least one field",
        ),
        (
            json!({"project":"lash","reason":"why","ops":[{"op":"order.set","collection":"steps","ids":[]}]}),
            "order.set needs rev: the revision whose order it lists",
        ),
        (
            json!({"project":"lash","reason":"why","preview_scope":"all","ops":[{"op":"input.put","name":"repo","declaration":"string"}]}),
            "preview_scope \"all\" needs dry_run: true",
        ),
    ] {
        assert_eq!(
            request("plan_edit", args)
                .unwrap()
                .check_plan_arguments()
                .unwrap_err()
                .to_string(),
            message
        );
    }
}

#[test]
fn edit_log_labels_cover_new_edits_and_preserve_authors() {
    for (name, fixture_name) in [
        ("plan_edit", "plan_edit.request"),
        ("unit_update", "unit_update.request"),
        ("unit_remove", "unit_remove.request"),
    ] {
        let mut value = fixture(fixture_name);
        value["author"] = json!("sam");
        let command = request(name, value).unwrap();
        assert_eq!(edit_label(&command), Some((name, Some("sam"))));
    }
    assert_eq!(edit_label(&CommandRequest::ProjectsList), None);
}
