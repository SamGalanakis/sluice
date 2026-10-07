use serde_json::Value;
use sluice_model::{commands::*, events::*, ids::*, rpc::*};

static FIXTURES: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    serde_json::from_str(include_str!("fixtures/contracts.json")).unwrap()
});

fn record_keys(bytes: &[u8]) -> std::collections::BTreeSet<String> {
    use serde::de::{MapAccess, Visitor};
    struct Keys;
    impl<'de> Visitor<'de> for Keys {
        type Value = std::collections::BTreeSet<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a record with unique keys")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut keys = std::collections::BTreeSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if !keys.insert(key.clone()) {
                    return Err(serde::de::Error::custom(format!("duplicate key {key}")));
                }
                // JsonValue walks nested objects and rejects their duplicate keys too.
                map.next_value::<JsonValue>()?;
            }
            Ok(keys)
        }
    }
    serde::de::Deserializer::deserialize_map(&mut serde_json::Deserializer::from_slice(bytes), Keys)
        .unwrap()
}

#[test]
fn every_event_record_roundtrips_through_strict_client_decoder() {
    let fixtures = FIXTURES["Event"].as_array().unwrap();
    let kinds: std::collections::BTreeSet<_> = fixtures
        .iter()
        .map(|event| event["kind"].as_str().unwrap())
        .collect();
    let event_schema = serde_json::to_value(schemars::schema_for!(Event)).unwrap();
    let schema_kinds: std::collections::BTreeSet<_> = event_schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|variant| variant["properties"]["kind"]["const"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds, schema_kinds,
        "fixtures must cover every Event variant"
    );
    for fixture in fixtures {
        let event: Event = decode_json(&serde_json::to_vec(fixture).unwrap()).unwrap();
        let record = Record {
            seq: RecordSeq(42),
            at: "2026-10-04T00:00:00Z".into(),
            project: Some(ProjectId::new()),
            event,
        };
        let wire = serde_json::to_vec(&record).unwrap();
        let keys = record_keys(&wire);
        for required in ["seq", "at", "project", "kind"] {
            assert!(keys.contains(required));
        }
        assert_eq!(decode_json::<Record>(&wire).unwrap(), record);
        let value: Value = decode_json(&wire).unwrap();
        assert_eq!(value["at"], record.at);
        if let Event::Message(message) = &record.event {
            assert_ne!(message.at, record.at);
            assert_eq!(value["posted_at"], message.at);
            assert_eq!(value["body"], message.body);
        }
    }
}

#[test]
fn standalone_and_legacy_message_timestamps_remain_readable() {
    let message: Message =
        decode_json(&serde_json::to_vec(&FIXTURES["Message"][1]).unwrap()).unwrap();
    let standalone = serde_json::to_vec(&message).unwrap();
    let value: Value = decode_json(&standalone).unwrap();
    assert_eq!(value["at"], message.at);
    assert!(value.get("posted_at").is_none());
    assert_eq!(decode_json::<Message>(&standalone).unwrap(), message);

    let event = Event::Message(Box::new(message));
    let canonical = serde_json::to_value(&event).unwrap();
    assert!(canonical.get("at").is_none());
    let mut legacy = canonical.clone();
    let posted_at = legacy.as_object_mut().unwrap().remove("posted_at").unwrap();
    legacy["at"] = posted_at;
    assert_eq!(
        decode_json::<Event>(&serde_json::to_vec(&legacy).unwrap()).unwrap(),
        event
    );
    let mut ambiguous = canonical;
    ambiguous["at"] = legacy["at"].clone();
    assert!(decode_json::<Event>(&serde_json::to_vec(&ambiguous).unwrap()).is_err());
}

#[test]
fn next_log_and_changes_replies_frame_message_records_for_clients() {
    let message: Message =
        decode_json(&serde_json::to_vec(&FIXTURES["Message"][1]).unwrap()).unwrap();
    let record = Record {
        seq: RecordSeq(42),
        at: "2026-10-04T00:00:00Z".into(),
        project: Some(ProjectId::new()),
        event: Event::Message(Box::new(message)),
    };
    for command in [
        CommandReply::Next(NextResult {
            records: vec![record.clone()],
            notes: vec![record.clone()],
            last_seq: record.seq,
            timed_out: false,
        }),
        CommandReply::Records(RecordPage {
            records: vec![record.clone()],
            last_seq: record.seq,
        }),
    ] {
        let reply = RpcReply {
            protocol: PROTOCOL_VERSION,
            request_id: RequestId("record-at".into()),
            result: RpcResult::Ok(Box::new(command)),
        };
        let frame = encode_frame(&reply).unwrap();
        assert_eq!(decode_json::<RpcReply>(&frame[4..]).unwrap(), reply);
    }
    let changes = ChangeBatch {
        records: vec![record.clone()],
        cursor: ChangeCursor {
            after: record.seq,
            projects: vec![record.project.unwrap()],
        },
    };
    assert_eq!(
        decode_json::<ChangeBatch>(&serde_json::to_vec(&changes).unwrap()).unwrap(),
        changes
    );
}

#[test]
fn legacy_agent_failure_without_kind_remains_readable() {
    let error: sluice_model::error::PublicError =
        decode_json(br#"{"error":"agent_failure","message":"old release"}"#).unwrap();
    assert!(
        matches!(error, sluice_model::error::PublicError::AgentFailure { kind, session: None, .. } if kind == "AgentFailure")
    );
}
