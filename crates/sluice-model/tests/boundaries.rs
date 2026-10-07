use serde_json::{Value, json};
use sluice_model::{commands::*, error::PublicError, ids::*, rpc::*};

#[test]
fn strict_json_rejects_duplicate_keys_at_every_depth() {
    for input in [
        r#"{"a":1,"a":2}"#,
        r#"{"a":[{"x":0,"x":1}]}"#,
        r#"{"command":"projects_list","command":"projects_list"}"#,
        r#"{"\u0061":0,"a":1}"#,
    ] {
        assert!(
            decode_json::<JsonValue>(input.as_bytes()).is_err(),
            "{input}"
        );
    }
}
#[test]
fn strict_json_rejects_integer_overflow_and_nonfinite_numbers() {
    for input in [
        "9223372036854775808",
        "-9223372036854775809",
        "18446744073709551616",
        "-18446744073709551616",
        "NaN",
        "Infinity",
        "-Infinity",
        "1e999",
        "{\"value\":9223372036854775808}",
        "null null",
    ] {
        assert!(
            decode_json::<JsonValue>(input.as_bytes()).is_err(),
            "{input}"
        );
    }
    for input in [
        "9223372036854775807",
        "-9223372036854775808",
        "1.0",
        "1e20",
        "-1e20",
        "1.7976931348623157e308",
        r#""9223372036854775808""#,
    ] {
        assert!(
            decode_json::<JsonValue>(input.as_bytes()).is_ok(),
            "{input}"
        );
    }
    assert!(JsonValue::try_from(json!({"n":u64::MAX})).is_err());
}
#[test]
fn identifiers_refuse_invalid_names_and_uuid_versions() {
    for name in [
        "",
        "Upper",
        "_start",
        "with.dot",
        "with/slash",
        "has space",
        "é",
    ] {
        assert!(StepId::new(name).is_err());
        assert!(decode_json::<StepId>(&serde_json::to_vec(name).unwrap()).is_err());
        assert!(UnitName::new(name).is_err());
    }
    for id in [
        "00000000-0000-0000-0000-000000000000",
        "019a2b3c-4d5e-4f01-8234-56789abcdef0",
        "019a2b3c-4d5e-7f01-0234-56789abcdef0",
    ] {
        assert!(id.parse::<ProjectId>().is_err());
        assert!(decode_json::<AttemptId>(&serde_json::to_vec(id).unwrap()).is_err());
    }
    let id = ProjectId::new();
    assert_eq!(id.as_uuid().get_version_num(), 7);
    assert_eq!(id.to_string().parse::<ProjectId>().unwrap(), id);
    let selector: ProjectSelector = format!("id:{id}").parse().unwrap();
    assert_eq!(selector.to_string(), format!("id:{id}"));
    assert_eq!(
        "project-1".parse::<ProjectSelector>().unwrap().to_string(),
        "project-1"
    );
    // Agents hold their project as a bare id (SLUICE_PROJECT_ID); it selects that id.
    assert_eq!(
        id.to_string().parse::<ProjectSelector>().unwrap(),
        ProjectSelector::Id(id)
    );
    assert_eq!(
        id.to_string()
            .to_uppercase()
            .parse::<ProjectSelector>()
            .unwrap(),
        ProjectSelector::Id(id)
    );
    // A bare UUID that is not a project id stays a name; project creation refuses such names.
    let v4 = "0d4f1f3e-2b6c-4a5e-9f1d-3c2b1a0f9e8d";
    assert_eq!(
        v4.parse::<ProjectSelector>().unwrap(),
        ProjectSelector::Name(v4.parse().unwrap())
    );
    assert!(v4.parse::<ProjectName>().unwrap().looks_like_id());
    assert!(
        id.to_string()
            .replace('-', "")
            .parse::<ProjectName>()
            .unwrap()
            .looks_like_id()
    );
    assert!(
        !"deadbeef-cafe"
            .parse::<ProjectName>()
            .unwrap()
            .looks_like_id()
    );
    assert!("id:project-1".parse::<ProjectSelector>().is_err());
    assert_eq!(Revision(2).to_string(), "2");
    assert_eq!(RecordSeq(3).to_string(), "3");
    assert_eq!(MessageId(4).to_string(), "4");
    assert_eq!(WorkGeneration(5).to_string(), "5");
    assert_eq!(StepId::new("work-1").unwrap().to_string(), "work-1");
    assert_eq!(UnitName::new("unit_1").unwrap().to_string(), "unit_1");
    assert_eq!(
        InvocationId::new()
            .to_string()
            .parse::<InvocationId>()
            .unwrap()
            .as_uuid()
            .get_version_num(),
        7
    );
}
#[test]
fn rpc_frames_are_bounded_complete_and_versioned() {
    let request = RpcRequest {
        protocol: PROTOCOL_VERSION,
        request_id: RequestId("request-1".into()),
        run_capability: Some(RunCapability::new("test-secret")),
        command: CommandRequest::ProjectsList,
    };
    let frame = encode_frame(&request).unwrap();
    assert_eq!(decode_frame(&frame).unwrap(), request);
    assert!(!format!("{:?}", request.run_capability).contains("test-secret"));
    assert!(decode_frame(&frame[..frame.len() - 1]).is_err());
    let mut trailing = frame.clone();
    trailing.push(0);
    assert!(decode_frame(&trailing).is_err());
    assert!(decode_frame(&[0, 0, 0]).is_err());
    assert!(decode_frame(&(MAX_FRAME_BYTES as u32 + 1).to_be_bytes()).is_err());
    let mut unsupported = request;
    unsupported.protocol = 2;
    assert!(decode_frame(&encode_frame(&unsupported).unwrap()).is_err());
}
#[test]
fn error_envelope_and_required_command_fields_are_closed() {
    let error = PublicError::Invalid {
        message: "invalid input".into(),
        errors: vec!["steps.work.in.engine: required".into()],
    };
    assert_eq!(
        serde_json::to_value(error).unwrap(),
        json!({"error":"invalid","message":"invalid input","errors":["steps.work.in.engine: required"]})
    );
    let fixtures: Value = serde_json::from_str(include_str!("fixtures/contracts.json")).unwrap();
    for name in ["StepSetInput", "ProjectDelete", "Messages"] {
        let mut f = fixtures[name][0].clone();
        f.as_object_mut().unwrap().remove(match name {
            "StepSetInput" => "inputs",
            "ProjectDelete" => "confirm_name",
            _ => "view",
        });
        let wire = serde_json::to_vec(&f).unwrap();
        assert!(match name {
            "StepSetInput" => decode_json::<StepSetInput>(&wire).is_err(),
            "ProjectDelete" => decode_json::<ProjectDelete>(&wire).is_err(),
            _ => decode_json::<Messages>(&wire).is_err(),
        });
    }
}
