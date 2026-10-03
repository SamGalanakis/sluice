use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sluice_model::{commands::*, error::*, events::*, gates::*, ids::*, plan::*, rpc::*, types::*};

// This checks the vocabulary emitted by this workspace, rather than adding another schema SDK.
fn matches_schema(root: &Value, schema: &Value, value: &Value) -> bool {
    if let Some(b) = schema.as_bool() {
        return b;
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        return matches_schema(
            root,
            root.pointer(reference.strip_prefix('#').expect("local schema reference"))
                .expect("resolved schema reference"),
            value,
        );
    }
    for keyword in ["anyOf", "oneOf", "allOf"] {
        if let Some(options) = schema.get(keyword).and_then(Value::as_array) {
            let count = options
                .iter()
                .filter(|s| matches_schema(root, s, value))
                .count();
            if (keyword == "anyOf" && count == 0)
                || (keyword == "oneOf" && count != 1)
                || (keyword == "allOf" && count != options.len())
            {
                return false;
            }
        }
    }
    if schema.get("const").is_some_and(|c| c != value) {
        return false;
    }
    if schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|vs| !vs.contains(value))
    {
        return false;
    }
    if let Some(t) = schema.get("type").and_then(Value::as_str) {
        let good = match t {
            "null" => value.is_null(),
            "boolean" => value.is_boolean(),
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            _ => panic!("unexpected schema type {t}"),
        };
        if !good {
            return false;
        }
    }
    if let Some(n) = value.as_f64()
        && (schema
            .get("minimum")
            .and_then(Value::as_f64)
            .is_some_and(|min| n < min)
            || schema
                .get("maximum")
                .and_then(Value::as_f64)
                .is_some_and(|max| n > max))
    {
        return false;
    }
    if let Some(s) = value.as_str()
        && let Some(pattern) = schema.get("pattern").and_then(Value::as_str)
    {
        let good = if pattern == "^[a-z0-9][a-z0-9_-]*$" {
            StepId::new(s).is_ok()
        } else {
            ProjectId::try_from(uuid::Uuid::parse_str(s).unwrap_or(uuid::Uuid::nil())).is_ok()
        };
        if !good {
            return false;
        }
    }
    if let Some(values) = value.as_array()
        && let Some(items) = schema.get("items")
        && !values.iter().all(|v| matches_schema(root, items, v))
    {
        return false;
    }
    if let Some(values) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array)
            && !required
                .iter()
                .all(|k| values.contains_key(k.as_str().expect("property name")))
        {
            return false;
        }
        let props = schema.get("properties").and_then(Value::as_object);
        for (key, v) in values {
            if let Some(s) = props.and_then(|ps| ps.get(key)) {
                if !matches_schema(root, s, v) {
                    return false;
                }
            } else if let Some(s) = schema.get("additionalProperties")
                && !matches_schema(root, s, v)
            {
                return false;
            }
        }
    }
    true
}
static FIXTURES: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    serde_json::from_str(include_str!("fixtures/contracts.json")).unwrap()
});
static SCHEMAS: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../docs/rust/schemas.json")).unwrap()
});
fn contract<T: DeserializeOwned + Serialize + JsonSchema>(name: &str) {
    let fixtures = &*FIXTURES;
    let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    let schemas = &*SCHEMAS;
    assert_eq!(schemas[name], schema, "schema snapshot for {name}");
    for value in fixtures[name].as_array().expect("registered fixture") {
        let wire = serde_json::to_vec(value).unwrap();
        let typed: T = decode_json(&wire).unwrap_or_else(|e| panic!("{name}: {e}: {value}"));
        assert_eq!(
            serde_json::to_value(&typed).unwrap(),
            *value,
            "wire round trip for {name}"
        );
        assert!(
            matches_schema(&schema, &schema, value),
            "schema rejected {name}: {value}"
        );
        let mut wrong = value.clone();
        if let Some(object) = wrong.as_object_mut() {
            object.insert("unexpected_field".into(), json!(true));
            if !matches_schema(&schema, &schema, &wrong) {
                assert!(
                    decode_json::<T>(&serde_json::to_vec(&wrong).unwrap()).is_err(),
                    "unknown field accepted for {name}"
                );
            }
        }
    }
}

#[test]
fn contract_project_id() {
    contract::<ProjectId>("ProjectId");
}

#[test]
fn contract_run_id() {
    contract::<RunId>("RunId");
}

#[test]
fn contract_attempt_id() {
    contract::<AttemptId>("AttemptId");
}

#[test]
fn contract_result_id() {
    contract::<ResultId>("ResultId");
}

#[test]
fn contract_invocation_id() {
    contract::<InvocationId>("InvocationId");
}

#[test]
fn contract_home_id() {
    contract::<HomeId>("HomeId");
}

#[test]
fn contract_step_id() {
    contract::<StepId>("StepId");
}

#[test]
fn contract_unit_name() {
    contract::<UnitName>("UnitName");
}

#[test]
fn contract_project_name() {
    contract::<ProjectName>("ProjectName");
}

#[test]
fn contract_revision() {
    contract::<Revision>("Revision");
}

#[test]
fn contract_record_seq() {
    contract::<RecordSeq>("RecordSeq");
}

#[test]
fn contract_message_id() {
    contract::<MessageId>("MessageId");
}

#[test]
fn contract_work_generation() {
    contract::<WorkGeneration>("WorkGeneration");
}

#[test]
fn contract_step_generation() {
    contract::<StepGeneration>("StepGeneration");
}

#[test]
fn contract_lease_id() {
    contract::<LeaseId>("LeaseId");
}

#[test]
fn contract_json_value() {
    contract::<JsonValue>("JsonValue");
}

#[test]
fn contract_json_map() {
    contract::<JsonMap>("JsonMap");
}

#[test]
fn contract_request_id() {
    contract::<RequestId>("RequestId");
}

#[test]
fn contract_run_capability() {
    contract::<RunCapability>("RunCapability");
}

#[test]
fn contract_path_errors() {
    contract::<PathErrors>("PathErrors");
}

#[test]
fn contract_value_ref() {
    contract::<ValueRef>("ValueRef");
}

#[test]
fn contract_validated_plan() {
    contract::<ValidatedPlan>("ValidatedPlan");
}

#[test]
fn contract_type() {
    contract::<Type>("Type");
}

#[test]
fn contract_project_identity() {
    contract::<ProjectIdentity>("ProjectIdentity");
}

#[test]
fn contract_project_update() {
    contract::<ProjectUpdate>("ProjectUpdate");
}

#[test]
fn contract_project_delete() {
    contract::<ProjectDelete>("ProjectDelete");
}

#[test]
fn contract_step_selection() {
    contract::<StepSelection>("StepSelection");
}

#[test]
fn contract_edit_options() {
    contract::<EditOptions>("EditOptions");
}

#[test]
fn contract_plan_patch() {
    contract::<PlanPatch>("PlanPatch");
}

#[test]
fn contract_step_add() {
    contract::<StepAdd>("StepAdd");
}

#[test]
fn contract_unit_add() {
    contract::<UnitAdd>("UnitAdd");
}

#[test]
fn contract_edge_edit() {
    contract::<EdgeEdit>("EdgeEdit");
}

#[test]
fn contract_step_update() {
    contract::<StepUpdate>("StepUpdate");
}

#[test]
fn contract_step_remove() {
    contract::<StepRemove>("StepRemove");
}

#[test]
fn contract_step_pause() {
    contract::<StepPause>("StepPause");
}

#[test]
fn contract_unit_tag() {
    contract::<UnitTag>("UnitTag");
}

#[test]
fn contract_plan_prune() {
    contract::<PlanPrune>("PlanPrune");
}

#[test]
fn contract_plan_set_input() {
    contract::<PlanSetInput>("PlanSetInput");
}

#[test]
fn contract_step_set_input() {
    contract::<StepSetInput>("StepSetInput");
}

#[test]
fn contract_step_set_output() {
    contract::<StepSetOutput>("StepSetOutput");
}

#[test]
fn contract_step_retry() {
    contract::<StepRetry>("StepRetry");
}

#[test]
fn contract_step_cancel() {
    contract::<StepCancel>("StepCancel");
}

#[test]
fn contract_step_submit() {
    contract::<StepSubmit>("StepSubmit");
}

#[test]
fn contract_message_answer() {
    contract::<MessageAnswer>("MessageAnswer");
}

#[test]
fn contract_message_post() {
    contract::<MessagePost>("MessagePost");
}

#[test]
fn contract_messages() {
    contract::<Messages>("Messages");
}

#[test]
fn contract_mark_read() {
    contract::<MarkRead>("MarkRead");
}

#[test]
fn contract_fn_call() {
    contract::<FnCall>("FnCall");
}

#[test]
fn contract_log_read() {
    contract::<LogRead>("LogRead");
}

#[test]
fn contract_log_wait() {
    contract::<LogWait>("LogWait");
}

#[test]
fn contract_next() {
    contract::<Next>("Next");
}

#[test]
fn contract_query() {
    contract::<Query>("Query");
}

#[test]
fn contract_acquire_lease() {
    contract::<AcquireLease>("AcquireLease");
}

#[test]
fn contract_release_lease() {
    contract::<ReleaseLease>("ReleaseLease");
}

#[test]
fn contract_completion_action_target() {
    contract::<CompletionActionTarget>("CompletionActionTarget");
}

#[test]
fn contract_register_completion_action() {
    contract::<RegisterCompletionAction>("RegisterCompletionAction");
}

#[test]
fn contract_edit_preview() {
    contract::<EditPreview>("EditPreview");
}

#[test]
fn contract_edit_result() {
    contract::<EditResult>("EditResult");
}

#[test]
fn contract_retry_result() {
    contract::<RetryResult>("RetryResult");
}

#[test]
fn contract_input_edit_result() {
    contract::<InputEditResult>("InputEditResult");
}

#[test]
fn contract_unsupported_input() {
    contract::<UnsupportedInput>("UnsupportedInput");
}

#[test]
fn contract_message() {
    contract::<Message>("Message");
}

#[test]
fn contract_message_page() {
    contract::<MessagePage>("MessagePage");
}

#[test]
fn contract_record_page() {
    contract::<RecordPage>("RecordPage");
}

#[test]
fn contract_next_result() {
    contract::<NextResult>("NextResult");
}

#[test]
fn contract_completion_action_conflict() {
    contract::<CompletionActionConflict>("CompletionActionConflict");
}

#[test]
fn contract_transient_retry() {
    contract::<TransientRetry>("TransientRetry");
}

#[test]
fn contract_file_binding() {
    contract::<FileBinding>("FileBinding");
}

#[test]
fn contract_snapshot() {
    contract::<Snapshot>("Snapshot");
}

#[test]
fn contract_plan_edit() {
    contract::<PlanEdit>("PlanEdit");
}

#[test]
fn contract_prepared_edit() {
    contract::<PreparedEdit>("PreparedEdit");
}

#[test]
fn contract_rpc_request() {
    contract::<RpcRequest>("RpcRequest");
}

#[test]
fn contract_rpc_reply() {
    contract::<RpcReply>("RpcReply");
}

#[test]
fn contract_fn_invocation() {
    contract::<FnInvocation>("FnInvocation");
}

#[test]
fn contract_record() {
    contract::<Record>("Record");
}

#[test]
fn contract_unit_step() {
    contract::<UnitStep>("UnitStep");
}

#[test]
fn contract_change_cursor() {
    contract::<ChangeCursor>("ChangeCursor");
}

#[test]
fn contract_change_batch() {
    contract::<ChangeBatch>("ChangeBatch");
}

#[test]
fn contract_stream_batch() {
    contract::<StreamBatch>("StreamBatch");
}

#[test]
fn contract_html_patch() {
    contract::<HtmlPatch>("HtmlPatch");
}

#[test]
fn contract_stream_version() {
    contract::<StreamVersion>("StreamVersion");
}

#[test]
fn contract_patch_operation() {
    contract::<PatchOperation>("PatchOperation");
}

#[test]
fn contract_message_view() {
    contract::<MessageView>("MessageView");
}

#[test]
fn contract_settles() {
    contract::<Settles>("Settles");
}

#[test]
fn contract_step_status() {
    contract::<StepStatus>("StepStatus");
}

#[test]
fn contract_attempt_phase() {
    contract::<AttemptPhase>("AttemptPhase");
}

#[test]
fn contract_lease_state() {
    contract::<LeaseState>("LeaseState");
}

#[test]
fn contract_completion_action_outcome() {
    contract::<CompletionActionOutcome>("CompletionActionOutcome");
}

#[test]
fn contract_command_request() {
    contract::<CommandRequest>("CommandRequest");
}

#[test]
fn contract_plan_view_format() {
    contract::<PlanViewFormat>("PlanViewFormat");
}

#[test]
fn contract_command_reply() {
    contract::<CommandReply>("CommandReply");
}

#[test]
fn contract_project_selector() {
    contract::<ProjectSelector>("ProjectSelector");
}

#[test]
fn contract_rpc_result() {
    contract::<RpcResult>("RpcResult");
}

#[test]
fn contract_bound_value() {
    contract::<BoundValue>("BoundValue");
}

#[test]
fn contract_skip_reason() {
    contract::<SkipReason>("SkipReason");
}

#[test]
fn contract_gate() {
    contract::<Gate>("Gate");
}

#[test]
fn contract_event() {
    contract::<Event>("Event");
}

#[test]
fn contract_adoption_outcome() {
    contract::<AdoptionOutcome>("AdoptionOutcome");
}

#[test]
fn contract_notification_outcome() {
    contract::<NotificationOutcome>("NotificationOutcome");
}

#[test]
fn contract_stream_event() {
    contract::<StreamEvent>("StreamEvent");
}

#[test]
fn contract_public_error() {
    contract::<PublicError>("PublicError");
}
