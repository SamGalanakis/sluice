use serde_json::json;
use sluice_runtime::config::{load_resource_policy, resource_policy_from_value};

#[test]
fn execution_policy_fails_closed_and_reserves_guardian_capacity() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.json"), "{broken").unwrap();
    assert!(load_resource_policy(home.path()).is_err());
    let policy = resource_policy_from_value(&json!({"execution":{"total":{"memory":"2GiB","cpus":1,"tasks":128},"defaults":{"memory":"512MiB","cpus":0.25,"tasks":32}}}), 8 << 30, 4000).unwrap();
    assert_eq!(policy.defaults.memory_bytes, Some(512 << 20));
    assert_eq!(policy.ceilings.tasks, None);
    assert_eq!(policy.ceilings.cpu_millis, None);
    assert!(
        policy.ceilings.memory_bytes.unwrap() + policy.guardian_memory_bytes
            <= policy.total.memory_bytes.unwrap()
    );
    for invalid in [
        json!({"execution":null}),
        json!({"execution":{"typo":1}}),
        json!({"execution":{"defaults":{"memory":"40GiB"}}}),
        json!({"execution":{"total":{"cpus":5}}}),
        json!({"execution":{"ceilings":{"tasks":8192},"total":{"tasks":4096}}}),
        // An unlimited per-run value needs an unlimited ceiling.
        json!({"execution":{"ceilings":{"tasks":64},"defaults":{"tasks":null}}}),
    ] {
        assert!(
            resource_policy_from_value(&invalid, 8 << 30, 4000).is_err(),
            "accepted {invalid}"
        );
    }
    let small = resource_policy_from_value(&json!({}), 128 << 20, 1000).unwrap();
    assert!(
        small.defaults.memory_bytes.unwrap() + small.guardian_memory_bytes
            <= small.total.memory_bytes.unwrap()
    );
    // Every level may be unlimited, spelled null or "unlimited"; a finite ceiling clamps an
    // inherited unlimited default.
    let open = resource_policy_from_value(&json!({"execution":{"total":{"memory":null,"cpus":"unlimited","tasks":null},"ceilings":{"memory":null},"defaults":{"memory":"unlimited"}}}), 8 << 30, 4000).unwrap();
    assert_eq!(open.total, sluice_model::Limits::UNLIMITED);
    assert_eq!(open.ceilings, sluice_model::Limits::UNLIMITED);
    assert_eq!(open.defaults, sluice_model::Limits::UNLIMITED);
    let clamped = resource_policy_from_value(
        &json!({"execution":{"ceilings":{"tasks":4096}}}),
        8 << 30,
        4000,
    )
    .unwrap();
    assert_eq!(clamped.defaults.tasks, Some(4096));
}
