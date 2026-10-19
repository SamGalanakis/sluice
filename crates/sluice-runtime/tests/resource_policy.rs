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
    // inherited unlimited default. Unlimited memory needs the explicit opt-in.
    let open = resource_policy_from_value(&json!({"execution":{"allow_unlimited_memory":true,"total":{"memory":null,"cpus":"unlimited","tasks":null},"ceilings":{"memory":null},"defaults":{"memory":"unlimited"}}}), 8 << 30, 4000).unwrap();
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

/// Bugs (incident 2026-10-19): config.json had `defaults.memory` and `ceilings.memory` null, so
/// 57 runs were admitted against a 92 GiB total reserving only their guardians' 256 MiB, and
/// each ran with memory.max=max. Null memory now needs `allow_unlimited_memory`; an opted-in
/// unlimited run still reserves a 4 GiB floor; the shipped total leaves the host its share.
#[test]
fn unlimited_memory_needs_the_opt_in_and_still_reserves_a_floor() {
    use sluice_model::limits::{Reserved, UNLIMITED_MEMORY_RESERVATION};
    for level in ["defaults", "ceilings", "total"] {
        let config = json!({"execution":{level:{"memory":null}}});
        let error = resource_policy_from_value(&config, 128 << 30, 32000).unwrap_err();
        assert!(
            error.contains(&format!("execution.{level}.memory"))
                && error.contains("allow_unlimited_memory"),
            "{error}"
        );
    }
    // The incident's configuration, opted in: no cap, and a 4 GiB reservation, not 256 MiB.
    let incident = json!({"execution":{"allow_unlimited_memory":true,
        "total":{"memory":"92GiB","cpus":null,"tasks":65536},
        "ceilings":{"memory":null,"cpus":null,"tasks":null},
        "defaults":{"memory":null,"cpus":null,"tasks":null}}});
    let policy = resource_policy_from_value(&incident, 125 << 30, 32000).unwrap();
    let run = policy.resolve(None, None).unwrap();
    assert_eq!(run.memory_max_bytes, None);
    assert_eq!(run.memory_bytes, Some(UNLIMITED_MEMORY_RESERVATION));
    assert_eq!(
        Reserved::of(run, policy.guardian_memory_bytes)
            .unwrap()
            .memory_bytes,
        UNLIMITED_MEMORY_RESERVATION + policy.guardian_memory_bytes
    );
    // A finite home default is what an unlimited run reserves.
    let reserved = json!({"execution":{"allow_unlimited_memory":true,
        "ceilings":{"memory":null},"defaults":{"memory":"6GiB"}}});
    let policy = resource_policy_from_value(&reserved, 125 << 30, 32000).unwrap();
    let run = policy
        .resolve(
            None,
            Some(&serde_json::from_value(json!({"memory":null})).unwrap()),
        )
        .unwrap();
    assert_eq!(
        (run.memory_bytes, run.memory_max_bytes),
        (Some(6 << 30), None)
    );
    // Shipped: min(70 GiB, 55% of memory) for all runs, 6 GiB reserved and 10 GiB capped each.
    let big = resource_policy_from_value(&json!({}), 256 << 30, 32000).unwrap();
    assert_eq!(big.total.memory_bytes, Some(70 << 30));
    assert_eq!(
        (big.defaults.memory_bytes, big.ceilings.memory_bytes),
        (Some(6 << 30), Some(10 << 30))
    );
    let small = resource_policy_from_value(&json!({}), 64 << 30, 8000).unwrap();
    assert_eq!(small.total.memory_bytes, Some((64u64 << 30) * 55 / 100));
}
