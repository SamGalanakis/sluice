use crate::{common, import_home};
use serde_json::json;
use std::{collections::BTreeMap, time::Instant};

#[test]
#[ignore = "G7 measured stopped-copy window, never schedules copied live work"]
fn g7_window() {
    let stamp = common::live_stamp();
    let scratch = common::scratch();
    let root = scratch.path();
    let snapshot = common::snapshot(root);
    // The online backup and release build belong to preflight, outside the pause.
    let has_install = common::install_available();
    let release = has_install.then(|| common::release_fixture(root));
    if has_install {
        let out = common::install(root, &["fence", "scratch measured window"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut timings = BTreeMap::new();
    let start = Instant::now();
    let step = Instant::now();
    std::fs::rename(root.join("source"), root.join("prepared-source")).unwrap();
    common::tree(&root.join("prepared-source"), &root.join("source"));
    timings.insert("final_stopped_copy", step.elapsed().as_secs_f64());
    let step = Instant::now();
    let destination = root.join("rust-home");
    let report = common::import(root, &destination);
    timings.insert("import", step.elapsed().as_secs_f64());
    let step = Instant::now();
    let counts = import_home::assert_table(&root.join("source"), &destination, &report);
    timings.insert("import_validation", step.elapsed().as_secs_f64());
    let step = Instant::now();
    if let Some(release) = release.as_ref() {
        let out = common::install(
            root,
            &[
                "select",
                release.to_str().unwrap(),
                destination.to_str().unwrap(),
            ],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    timings.insert("select_release_home", step.elapsed().as_secs_f64());
    let step = Instant::now();
    let env = BTreeMap::from([(
        "SLUICE_INSTALL_DIR".into(),
        root.join("install").to_string_lossy().into(),
    )]);
    let executable = release
        .as_ref()
        .map_or_else(common::binary, |r| r.join("bin/sluice"));
    let _coordinator = common::boot(&executable, &destination, &env);
    timings.insert("boot_to_maintenance", step.elapsed().as_secs_f64());
    let mut retry_count = 0;
    let step = Instant::now();
    let ledger: serde_json::Value =
        serde_json::from_slice(&std::fs::read(destination.join("import-ledger.json")).unwrap())
            .unwrap();
    let old = common::readonly(&root.join("source"));
    let mut interrupted = old.prepare("SELECT project,doc FROM states").unwrap();
    let mut refused = vec![];
    for row in interrupted
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
    {
        let (name, raw) = row.unwrap();
        let state: serde_json::Value = serde_json::from_str(&raw).unwrap();
        for (id, state) in state["steps"].as_object().unwrap() {
            if state["status"] != "running" {
                continue;
            }
            let predecessors = &ledger["projects"][&name]["steps"][id]["predecessors"];
            // A native checkpoint is the evidence this was an agent, not a shell/push.
            if !predecessors.as_object().unwrap().values().any(|ids| {
                destination
                    .join("runs")
                    .join(ids["run"].as_str().unwrap())
                    .join("native.json")
                    .is_file()
            }) {
                continue;
            }
            let reply = common::rpc(
                &destination,
                json!({"command":"step_retry","args":{"project":{"kind":"name","value":name},"selection":{"steps":[id],"tags":null},"message":"sluice was upgraded; continue where you left off","reason":"scratch measured cutover","author":"fixture"}}),
            );
            if reply["result"]["status"] == "ok" {
                retry_count += 1;
            } else {
                refused.push(reply["result"]["value"].clone());
            }
        }
    }
    timings.insert("retry_interrupted_agents", step.elapsed().as_secs_f64());
    let step = Instant::now();
    let unfenced = if has_install {
        common::install(root, &["unfence"]).status.success()
    } else {
        false
    };
    timings.insert("unfence", step.elapsed().as_secs_f64());
    let total = start.elapsed().as_secs_f64();
    let dominant = timings.iter().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
    // No scheduler lease or unpause is ever issued in a full-home copy.
    let db = common::readonly(&destination);
    assert_eq!(
        db.query_row("SELECT count(*) FROM projects WHERE paused=0", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM attempts WHERE phase<>'terminal'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(stamp, common::live_stamp());
    common::evidence(
        "g7_window",
        &json!({"snapshot":snapshot,"counts":counts,"seconds":timings,"total_seconds":total,"target_seconds":300,"dominant":dominant,"retry_count":retry_count,"retry_refusals":refused,"install_available":has_install,"unfenced":unfenced,"complete_window":has_install && refused.is_empty() && unfenced,"copied_projects_executed":0}),
    );
    assert!(
        total < 300.0,
        "window exceeds five minutes: stage immutable session homes before pausing"
    );
    // p7-02 owns the installation fence and maintenance retry contract. Missing
    // prerequisites are reported explicitly; a partial timing is never acceptance.
    if has_install {
        assert!(
            refused.is_empty(),
            "p7-02: maintenance retry refused: {refused:?}"
        );
        assert!(unfenced, "p7-02: unfence refused");
    }
}
