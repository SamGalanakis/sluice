use serde_json::json;
use sluice_model::{
    LimitsOverride, ResourcePolicy,
    limits::{CpuMillis, MemoryBytes},
};

#[test]
fn limits_reject_unbounded_values_and_resolve_each_layer() {
    let policy = ResourcePolicy::for_host(32 << 30, 8000).unwrap();
    let project: LimitsOverride =
        serde_json::from_value(json!({"memory":"4GiB", "cpus":1.25})).unwrap();
    let run: LimitsOverride = serde_json::from_value(json!({"tasks":123})).unwrap();
    let limits = policy.resolve(Some(&project), Some(&run)).unwrap();
    assert_eq!(
        (limits.memory_bytes, limits.cpu_millis, limits.tasks),
        (4 << 30, 1250, 123)
    );
    assert_eq!(
        serde_json::from_value::<LimitsOverride>(serde_json::to_value(&project).unwrap()).unwrap(),
        project
    );
    for invalid in [
        json!({"memory":0}),
        json!({"memory":"max"}),
        json!({"memory":"18446744073709551615GiB"}),
        json!({"cpus":0}),
        json!({"cpus":0.0001}),
        json!({"tasks":0}),
        json!({"typo":1}),
    ] {
        assert!(
            serde_json::from_value::<LimitsOverride>(invalid.clone()).is_err(),
            "accepted {invalid}"
        );
    }
    for over in [
        LimitsOverride {
            memory: Some(MemoryBytes(u64::MAX)),
            ..Default::default()
        },
        LimitsOverride {
            cpus: Some(CpuMillis(u32::MAX)),
            ..Default::default()
        },
        LimitsOverride {
            tasks: Some(u32::MAX),
            ..Default::default()
        },
    ] {
        assert!(policy.resolve(None, Some(&over)).is_err());
    }
}

#[test]
fn plan_limits_are_work_and_survive_incremental_edits() {
    use sluice_model::{
        StateSnapshot,
        plan::FnSignature,
        plan_rows::{PlanOp, StepChanges},
    };
    let signatures = indexmap::IndexMap::from([("work".to_string(), FnSignature::default())]);
    let plan = crate::support::compile(
        json!({"steps":{"a":{"run":"work","limits":{"memory":"1GiB","cpus":0.5,"tasks":20}}}}),
        &signatures,
    )
    .unwrap();
    let reference = sluice_reference::plan::Plan::parse(
        &crate::support::map(
            json!({"steps":{"a":{"run":"work","limits":{"memory":"1GiB","cpus":0.5,"tasks":20}}}}),
        ),
        &indexmap::IndexMap::from([(
            "work".to_string(),
            sluice_reference::plan::FnSignature::default(),
        )]),
    );
    assert!(reference.is_ok());
    let id: sluice_model::ids::StepId = "a".parse().unwrap();
    let changed = crate::support::compile(
        json!({"steps":{"a":{"run":"work","limits":{"memory":"2GiB","cpus":0.5,"tasks":20}}}}),
        &signatures,
    )
    .unwrap();
    assert!(!plan.steps()[&id].same_work(&changed.steps()[&id]));
    let edits = crate::support::prepare(
        &plan,
        &StateSnapshot::default(),
        &signatures,
        vec![PlanOp::StepUpdate {
            step: id.clone(),
            changes: Box::new(
                serde_json::from_value::<StepChanges>(json!({"limits":null})).unwrap(),
            ),
        }],
        crate::support::options(),
    );
    assert!(edits.is_ok());
}
