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
fn flat_edit_commands_are_closed_and_preserve_options() {
    {
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

    {
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

    {
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

    {
        let CommandRequest::StepUpdate(update)=request("step_update",json!({"project":"lash","step":"work","changes":{"paused":null,"priority":0},"edit":{"reason":"","dry_run":false}})).unwrap() else { panic!() };
        assert_eq!(update.changes.paused, Some(None));
        assert_eq!(update.changes.doc, None);
        assert_eq!(update.edit.preview_scope, PreviewScope::Impact);
        assert_eq!(
            serde_json::to_value(update.changes).unwrap(),
            json!({"paused":null,"priority":0})
        );
    }

    {
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
}
