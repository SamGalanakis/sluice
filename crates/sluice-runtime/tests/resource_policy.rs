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
        json!({"execution":{"ceilings":{"tasks":8192},"total":{"tasks":4096}}}),
        // An unlimited per-run value needs an unlimited ceiling.
        json!({"execution":{"ceilings":{"tasks":64},"defaults":{"tasks":null}}}),
        json!({"execution":{"total":{"cpus":0}}}),
        json!({"execution":{"total":{"cpus":-1}}}),
        json!({"execution":{"total":{"memory":0}}}),
        json!({"execution":{"total":{"memory":-1}}}),
        json!({"execution":{"cpu_pressure_max":0}}),
        json!({"execution":{"load_per_cpu_max":-1}}),
        json!({"execution":{"warmup_starts":0}}),
        json!({"execution":{"cpu_weight":0}}),
    ] {
        assert!(
            resource_policy_from_value(&invalid, 8 << 30, 4000).is_err(),
            "accepted {invalid}"
        );
    }
    // The workloads' slice leaves a quarter of the CPUs to everything else by default.
    let shipped = resource_policy_from_value(&json!({}), 8 << 30, 4000).unwrap();
    assert_eq!(shipped.total.cpu_millis, Some(3000));
    let small = resource_policy_from_value(&json!({}), 128 << 20, 1000).unwrap();
    assert!(
        small.defaults.memory_bytes.unwrap() + small.guardian_memory_bytes
            <= small.total.memory_bytes.unwrap()
    );
    // Every level may be unlimited, spelled null or "unlimited"; a finite ceiling clamps an
    // inherited unlimited default. Unlimited memory needs the explicit opt-in.
    let open = resource_policy_from_value(&json!({"execution":{"allow_unlimited_memory":true,"total":{"memory":null,"cpus":"unlimited","tasks":null},"ceilings":{"memory":null},"defaults":{"memory":"unlimited"}}}), 8 << 30, 4000).unwrap();
    assert_eq!(open.total.memory_bytes, Some(8 << 30));
    assert_eq!(open.total.cpu_millis, Some(4000));
    assert_eq!(open.total.tasks, None);
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
/// 57 runs were admitted and each ran with memory.max=max. Null memory now needs
/// `allow_unlimited_memory`; the shipped total leaves the host its share and a run's default
/// cap is 6 GiB.
#[test]
fn unlimited_memory_needs_the_opt_in() {
    for level in ["defaults", "ceilings", "total"] {
        let config = json!({"execution":{level:{"memory":null}}});
        let error = resource_policy_from_value(&config, 128 << 30, 32000).unwrap_err();
        assert!(
            error.contains(&format!("execution.{level}.memory"))
                && error.contains("allow_unlimited_memory"),
            "{error}"
        );
    }
    // The incident's configuration, opted in: no cap.
    let incident = json!({"execution":{"allow_unlimited_memory":true,
        "total":{"memory":"92GiB","cpus":null,"tasks":65536},
        "ceilings":{"memory":null,"cpus":null,"tasks":null},
        "defaults":{"memory":null,"cpus":null,"tasks":null}}});
    let policy = resource_policy_from_value(&incident, 125 << 30, 32000).unwrap();
    assert_eq!(policy.resolve(None, None).unwrap().memory_max_bytes, None);
    // Shipped: min(70 GiB, 55% of memory) for the pool, each run capped at 6 GiB and able to
    // ask for up to 10 GiB.
    let big = resource_policy_from_value(&json!({}), 256 << 30, 32000).unwrap();
    assert_eq!(big.total.memory_bytes, Some(70 << 30));
    assert_eq!(
        (big.defaults.memory_bytes, big.ceilings.memory_bytes),
        (Some(6 << 30), Some(10 << 30))
    );
    assert_eq!(
        big.resolve(None, None).unwrap().memory_max_bytes,
        Some(6 << 30)
    );
    let small = resource_policy_from_value(&json!({}), 64 << 30, 8000).unwrap();
    assert_eq!(small.total.memory_bytes, Some((64u64 << 30) * 55 / 100));
}

/// Bug (2026-10-20): `user.slice` had memory.max 96 GiB, the live total was 104 GiB, and
/// host capacity can shrink temporarily; configured totals are ceilings, not load errors.
#[test]
fn totals_follow_effective_capacity_without_rejecting_the_config() {
    use sluice_runtime::config::{HostCapacity, execution_from_value};
    let tree = tempfile::tempdir().unwrap();
    let service = tree
        .path()
        .join("user.slice/user-1000.slice/sluice.service");
    std::fs::create_dir_all(&service).unwrap();
    std::fs::write(
        tree.path().join("user.slice/memory.max"),
        format!("{}\n", 96u64 << 30),
    )
    .unwrap();
    std::fs::write(
        tree.path().join("user.slice/user-1000.slice/memory.max"),
        "max\n",
    )
    .unwrap();
    std::fs::write(
        tree.path().join("user.slice/user-1000.slice/cpu.max"),
        "800000 100000\n",
    )
    .unwrap();
    let host = HostCapacity::from_tree(
        tree.path(),
        "0::/user.slice/user-1000.slice/sluice.service\n",
        "MemTotal:       131072000 kB\n",
        32000,
    )
    .unwrap();
    assert_eq!(host.memory_bytes, 96 << 30);
    assert_eq!(host.memory_cgroup.as_deref(), Some("/user.slice"));
    assert_eq!(
        (host.cpu_millis, host.cpu_cgroup.as_deref()),
        (8000, Some("/user.slice/user-1000.slice"))
    );
    let (cpu, _) = execution_from_value(
        &json!({"execution":{"total":{"cpus":29}}}),
        &HostCapacity::plain(128 << 30, 28000),
    )
    .unwrap();
    assert_eq!(cpu.total.cpu_millis, Some(28000));
    let config = json!({"execution":{"total":{"memory":"104GiB","cpus":29}}});
    let (policy, _) = execution_from_value(&config, &host).unwrap();
    assert_eq!(policy.total.memory_bytes, Some(96 << 30));
    assert_eq!(policy.total.cpu_millis, Some(8000));
    let (policy, _) =
        execution_from_value(&config, &HostCapacity::plain(128 << 30, 32000)).unwrap();
    assert_eq!(policy.total.memory_bytes, Some(104 << 30));
    assert_eq!(policy.total.cpu_millis, Some(29000));
    // Explicit run caps remain valid even below a host-derived total. They reserve nothing.
    let config = json!({"execution":{"defaults":{"memory":"3GiB"},"ceilings":{"memory":"6GiB"}}});
    let (policy, _) = execution_from_value(&config, &HostCapacity::plain(1 << 30, 28000)).unwrap();
    assert_eq!(policy.defaults.memory_bytes, Some(3 << 30));
    assert!(policy.total.memory_bytes.unwrap() < 1 << 30);
    // The new admission settings are checked too.
    for invalid in [
        json!({"execution":{"admission_warmup_s":-1}}),
        json!({"execution":{"engines":{"codex":{"max_runs":-1}}}}),
        json!({"execution":{"engines":{"codex":{"max_runs":1.5}}}}),
        json!({"execution":{"engines":{"typo":{"max_runs":2}}}}),
        json!({"execution":{"total":{"memory":"96GiB"},"admission_charge":"2TiB"}}),
    ] {
        assert!(
            execution_from_value(&invalid, &host).is_err(),
            "accepted {invalid}"
        );
    }
}
