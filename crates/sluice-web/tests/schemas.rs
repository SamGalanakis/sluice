#[test]
fn mcp_uses_the_shared_schemars_vocabulary() {
    let schema = rmcp::schemars::schema_for!(sluice_model::commands::StepRetry);
    let wire = serde_json::to_value(schema).unwrap();
    assert!(wire["properties"].get("message").is_some());
    assert!(wire["properties"].get("selection").is_some());
    let input = serde_json::to_value(rmcp::schemars::schema_for!(
        sluice_model::commands::StepSetInput
    ))
    .unwrap();
    assert!(
        input["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "inputs")
    );
    assert_eq!(input["additionalProperties"], false);
}
