use serde_json::json;
use sluice_model::{
    LimitsOverride, ResourcePolicy,
    limits::{CpuMillis, MemoryBytes},
};

#[test]
fn limits_resolve_each_layer_and_spell_unlimited_as_null() {
    let policy = ResourcePolicy::for_host(32 << 30, 8000).unwrap();
    // The shipped per-run defaults never cap CPU or tasks.
    assert_eq!(
        (policy.defaults.cpu_millis, policy.defaults.tasks),
        (None, None)
    );
    let project: LimitsOverride =
        serde_json::from_value(json!({"memory":"4GiB", "cpus":1.25})).unwrap();
    let run: LimitsOverride = serde_json::from_value(json!({"tasks":123})).unwrap();
    let limits = policy.resolve(Some(&project), Some(&run)).unwrap();
    assert_eq!(
        (limits.memory_bytes, limits.cpu_millis, limits.tasks),
        (Some(4 << 30), Some(1250), Some(123))
    );
    // A run's memory is its reservation; its cap is the per-run ceiling.
    assert_eq!(limits.memory_max_bytes, policy.ceilings.memory_bytes);
    assert_eq!(
        serde_json::from_value::<LimitsOverride>(serde_json::to_value(&project).unwrap()).unwrap(),
        project
    );
    // null and "unlimited" are one value, stored as null; a left-out field inherits.
    let unlimited: LimitsOverride =
        serde_json::from_value(json!({"cpus":null, "tasks":"unlimited"})).unwrap();
    assert_eq!(unlimited.memory, None);
    assert_eq!((unlimited.cpus, unlimited.tasks), (Some(None), Some(None)));
    assert_eq!(
        serde_json::to_value(&unlimited).unwrap(),
        json!({"cpus":null,"tasks":null})
    );
    let limited = policy.resolve(Some(&run), Some(&unlimited)).unwrap();
    assert_eq!((limited.cpu_millis, limited.tasks), (None, None));
    // A budget frozen before unlimited values had no memory cap field: its reservation was it.
    let old: sluice_model::RunLimits =
        serde_json::from_value(json!({"memory_bytes":1024,"cpu_millis":500,"tasks":8})).unwrap();
    assert_eq!(old.memory_max_bytes, Some(1024));
    for invalid in [
        json!({"memory":0}),
        json!({"memory":"max"}),
        json!({"memory":"18446744073709551615GiB"}),
        json!({"cpus":0}),
        json!({"cpus":0.0001}),
        json!({"tasks":0}),
        json!({"tasks":"none"}),
        json!({"typo":1}),
    ] {
        assert!(
            serde_json::from_value::<LimitsOverride>(invalid.clone()).is_err(),
            "accepted {invalid}"
        );
    }
    // Under a finite ceiling, unlimited and oversized values are refused.
    let capped = ResourcePolicy {
        ceilings: sluice_model::Limits {
            tasks: Some(4096),
            cpu_millis: Some(4000),
            ..policy.ceilings
        },
        ..policy.clone()
    };
    for over in [
        LimitsOverride {
            memory: Some(Some(MemoryBytes(u64::MAX))),
            ..Default::default()
        },
        LimitsOverride {
            cpus: Some(Some(CpuMillis(u32::MAX))),
            ..Default::default()
        },
        LimitsOverride {
            tasks: Some(None),
            ..Default::default()
        },
    ] {
        assert!(capped.resolve(None, Some(&over)).is_err(), "{over:?}");
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
