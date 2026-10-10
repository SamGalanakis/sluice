//! The rehearsal host reaches nothing outside the copy (`docs/design/plan-rows.md` §10.10):
//! asked to reconcile a run whose unit really exists in the user service manager, it reports
//! the guardian gone and leaves the unit, its process and its cgroup exactly as they were; it
//! launches nothing and runs no fn. Its source names no way out of the process.

use cutover_rehearsal::host::RehearsalHost;
use sluice_model::{
    error::PublicError,
    ids::{
        AttemptId, HomeId, InvocationId, MessageId, ProjectId, RunId, StepGeneration, StepId,
        WorkGeneration,
    },
    rpc::{FnInvocation, JsonMap, RunCapability},
};
use sluice_process::{
    guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence},
    journal::{AttemptKey, CleanupEvidence, CompletionJournal, PayloadResult},
    socket::AssignedRange,
};
use sluice_runtime::execution::{ExecutionHost, Launch, LaunchOutcome};
use std::{collections::BTreeMap, process::Command};

/// A real `sluice-test-*` user unit running `sleep`, stopped when dropped.
struct Unit(String);
impl Unit {
    fn start(name: &str) -> Self {
        assert!(name.starts_with("sluice-test-"), "refusing to start {name}");
        let started = Command::new("systemd-run")
            .args([
                "--user",
                "--quiet",
                "--collect",
                "--unit",
                name,
                "/usr/bin/sleep",
                "600",
            ])
            .output()
            .unwrap();
        assert!(started.status.success(), "{started:?}");
        let unit = Self(name.to_owned());
        for _ in 0..200 {
            if unit.show()["ActiveState"] == "active" && unit.show()["MainPID"] != "0" {
                return unit;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("{name} never became active: {:?}", unit.show());
    }
    fn show(&self) -> BTreeMap<String, String> {
        let out = Command::new("systemctl")
            .args([
                "--user",
                "show",
                "-p",
                "ActiveState,MainPID,ControlGroup,NRestarts,InvocationID",
                &self.0,
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    }
    /// The unit's state, its main process and that process's start time, and its cgroup's
    /// members: everything a stop or a signal would change.
    fn evidence(&self) -> (BTreeMap<String, String>, String, String) {
        let show = self.show();
        let pid = &show["MainPID"];
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let procs = std::fs::read_to_string(format!(
            "/sys/fs/cgroup{}/cgroup.procs",
            show["ControlGroup"]
        ))
        .unwrap_or_default();
        (show, stat, procs)
    }
}
impl Drop for Unit {
    fn drop(&mut self) {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &self.0])
            .output();
    }
}

fn key(run: RunId) -> AttemptKey {
    AttemptKey {
        home: HomeId::new(),
        project: Some(ProjectId::new()),
        step: Some(StepId::new("tests-main").unwrap()),
        generation: StepGeneration(1),
        work: WorkGeneration(1),
        run,
        attempt: AttemptId::new(),
    }
}

#[tokio::test]
async fn reconciling_a_unit_that_exists_touches_nothing_and_reports_the_guardian_gone() {
    let run = RunId::new();
    let unit = Unit::start(&format!("sluice-test-{run}.service"));
    let before = unit.evidence();
    assert!(
        !before.1.is_empty() && !before.2.trim().is_empty(),
        "{before:?}"
    );
    let scratch = tempfile::tempdir().unwrap();
    let host = RehearsalHost::new();
    let attempt = AdoptionAttempt {
        identity: key(run),
        guardian: None,
        run_dir: scratch.path().join("runs").join(run.to_string()),
        unit: unit.0.clone(),
        service_cgroup: Some(before.0["ControlGroup"].clone()),
        capability: RunCapability::new("rehearsal"),
    };
    let presence = host.reconcile(&attempt).await.unwrap();
    let GuardianPresence::Gone(CleanupEvidence { empty, cgroup, .. }) = presence else {
        panic!("the guardian is reported gone: {presence:?}")
    };
    assert!(empty);
    assert!(
        !cgroup.starts_with('/'),
        "the evidence names no cgroup path: {cgroup}"
    );
    // Twice, as a second adoption pass would.
    host.reconcile(&attempt).await.unwrap();
    assert_eq!(
        unit.evidence(),
        before,
        "the unit, its process and its cgroup are untouched"
    );
    assert!(
        !attempt.run_dir.exists(),
        "reconcile writes nothing, not even the run dir"
    );

    let invocation = FnInvocation {
        project: ProjectId::new(),
        step: None,
        run,
        attempt: AttemptId::new(),
        invocation: InvocationId::new(),
        name: "fixture.echo".into(),
        inputs: JsonMap::default(),
    };
    let launch = Launch {
        identity: key(run),
        invocation: invocation.clone(),
        assigned: AssignedRange {
            after: MessageId(0),
            through: MessageId(0),
        },
        prev_run: None,
        capability: RunCapability::new("rehearsal"),
        timeout_seconds: None,
        context: None,
    };
    let refused = host.launch(launch).await.unwrap();
    assert!(
        matches!(refused, LaunchOutcome::Refused(PublicError::Busy { .. })),
        "{refused:?}"
    );
    assert!(host.invoke(invocation).await.is_err());
    let journal = CompletionJournal {
        protocol: 1,
        identity: key(run),
        completion_id: "c".into(),
        result: PayloadResult::Lost("gone".into()),
        starts: vec![],
        exits: vec![],
        cleanup: vec![CleanupEvidence {
            cgroup: before.0["ControlGroup"].clone(),
            empty: true,
            escalated: false,
        }],
        submissions: JsonMap::default(),
        submission_version: None,
        delivery_acks: vec![],
    };
    assert!(host.cleanup_valid(&journal).await.unwrap());
    assert_eq!(unit.evidence(), before);
    let asked = host.asked();
    assert_eq!(asked.reconciled, [run, run]);
    assert_eq!(asked.launches, [run]);
    assert_eq!(asked.invocations, ["fixture.echo"]);
}

/// The code of a source file, its comments left out.
fn code(path: &str) -> String {
    std::fs::read_to_string(format!("{}/{path}", env!("CARGO_MANIFEST_DIR")))
        .unwrap()
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_host_and_the_play_name_no_way_out_of_the_process() {
    for path in ["src/host.rs", "src/rehearse.rs"] {
        let code = code(path);
        for forbidden in [
            "systemd",
            "systemctl",
            "TransientService",
            "Command::new",
            "std::process",
            "tokio::process",
            "sluice_process::signals",
            "sluice_process::systemd",
            "sluice_process::cgroup",
            "spawn",
            "kill",
            "signal",
            "UnixStream",
            "Socket",
            "Cgroup",
            "OsHost",
            "OsFnHost",
            "OsAdoptionHost",
            "stop_run",
            "serve(",
            "acquire_scheduler",
            "reconcile_project",
        ] {
            assert!(!code.contains(forbidden), "{path} uses {forbidden}");
        }
    }
}
