//! Argument-safe transient user services. An attempted start is never retried.
use crate::host::test_mode;
use serde::{Deserialize, Serialize};
use sluice_model::ids::RunId;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitState {
    pub name: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub cgroup: Option<String>,
    pub main_pid: Option<u32>,
}
impl UnitState {
    pub fn stopped(&self) -> bool {
        matches!(self.active_state.as_str(), "inactive" | "failed")
            || self.load_state == "not-found"
    }
}
#[derive(Debug)]
pub enum StartOutcome {
    Confirmed {
        state: UnitState,
        reconciled: bool,
    },
    /// Includes successful commands whose short-lived units disappeared. This
    /// never authorizes another start for the same attempt.
    Uncertain {
        unit: String,
        message: String,
    },
}
#[derive(Debug)]
pub struct ServiceCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<OsString, OsString>,
}
impl ServiceCommand {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
        }
    }
}
/// The prefix of every user unit this process starts: `sluice-test-` in tests,
/// `sluice-` otherwise.
pub fn unit_prefix() -> &'static str {
    if test_mode() {
        "sluice-test-"
    } else {
        "sluice-"
    }
}
#[derive(Debug)]
pub struct TransientService {
    name: String,
    attempted: bool,
}
impl TransientService {
    /// The unit a run launched by this process gets: `sluice-run-<run>.service`, or
    /// `sluice-test-<run>.service` in tests.
    pub fn for_launch(run: RunId) -> Self {
        if test_mode() {
            Self::for_test(run)
        } else {
            Self::for_run(run)
        }
    }
    pub fn for_run(run: RunId) -> Self {
        Self {
            name: format!("sluice-run-{run}.service"),
            attempted: false,
        }
    }
    pub fn for_test(run: RunId) -> Self {
        Self {
            name: format!("sluice-test-{run}.service"),
            attempted: false,
        }
    }
    /// Every name a run's unit can have, the launch name first. Releases before
    /// the production prefix launched production runs as `sluice-test-<run>`, so
    /// both stay adoptable whatever this process's mode.
    pub fn names(run: RunId) -> [String; 2] {
        let launch = Self::for_launch(run).name;
        let other = if test_mode() {
            Self::for_run(run).name
        } else {
            Self::for_test(run).name
        };
        [launch, other]
    }
    /// Adoption is query/stop only. The durable spawn_attempted claim belongs
    /// to the coordinator; reconstructing a service must preserve that claim.
    /// `name` must be one of [`Self::names`] for `run`.
    pub fn adopt(run: RunId, name: &str) -> io::Result<Self> {
        if !Self::names(run).iter().any(|known| known == name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unit is not bound to run",
            ));
        }
        Ok(Self {
            name: name.into(),
            attempted: true,
        })
    }
    /// The run's unit as it exists now: the first of [`Self::names`] the user
    /// manager has loaded, with its state, or `None` when neither is loaded.
    pub async fn adopt_loaded(run: RunId) -> io::Result<Option<(Self, UnitState)>> {
        for name in Self::names(run) {
            let service = Self::adopt(run, &name)?;
            let state = service.query().await?;
            if state.load_state != "not-found" {
                return Ok(Some((service, state)));
            }
        }
        Ok(None)
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub async fn start_once(&mut self, spec: &ServiceCommand) -> io::Result<StartOutcome> {
        if self.attempted {
            return Ok(self.reconcile("start already attempted".into()).await);
        }
        if !spec.program.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "service executable must be absolute",
            ));
        }
        if spec.cwd.as_ref().is_some_and(|cwd| !cwd.is_absolute()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "service working directory must be absolute",
            ));
        }
        for key in spec.env.keys() {
            let valid = key.to_str().is_some_and(|key| {
                !key.is_empty()
                    && key.bytes().enumerate().all(|(index, ch)| {
                        ch == b'_' || ch.is_ascii_alphabetic() || (index > 0 && ch.is_ascii_digit())
                    })
            });
            if !valid {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid service environment key",
                ));
            }
        }
        // Reserve before any await, including the preflight lookup.
        self.attempted = true;
        let existing = self.query().await?;
        if existing.load_state != "not-found" {
            return Ok(StartOutcome::Confirmed {
                state: existing,
                reconciled: true,
            });
        }
        let mut command = Command::new("/usr/bin/systemd-run");
        command
            .args([
                "--user",
                "--quiet",
                "--collect",
                "--service-type=exec",
                "--unit",
            ])
            .arg(&self.name)
            .args([
                "--property=Delegate=yes",
                "--property=KillMode=control-group",
                "--property=Restart=no",
                "--property=TimeoutStopSec=5s",
            ])
            .arg(format!("--description=Sluice run {}", self.name));
        if let Some(cwd) = &spec.cwd {
            command.arg("--working-directory").arg(cwd);
        }
        for (key, value) in &spec.env {
            let mut assignment = OsString::from("--setenv=");
            assignment.push(key);
            assignment.push("=");
            assignment.push(value);
            command.arg(assignment);
        }
        command.arg("--").arg(&spec.program).args(&spec.args);
        let result = manager_output(command).await;
        match result {
            Ok(_) => match self.query().await {
                Ok(state) if state.load_state != "not-found" => Ok(StartOutcome::Confirmed {
                    state,
                    reconciled: false,
                }),
                other => Ok(StartOutcome::Uncertain {
                    unit: self.name.clone(),
                    message: format!("start acknowledged; reconciliation={other:?}"),
                }),
            },
            Err(e) => Ok(self.reconcile(format!("ambiguous start: {e}")).await),
        }
    }
    async fn reconcile(&self, message: String) -> StartOutcome {
        match self.query().await {
            Ok(state) if state.load_state != "not-found" => StartOutcome::Confirmed {
                state,
                reconciled: true,
            },
            Ok(_) => StartOutcome::Uncertain {
                unit: self.name.clone(),
                message,
            },
            Err(e) => StartOutcome::Uncertain {
                unit: self.name.clone(),
                message: format!("{message}; query failed: {e}"),
            },
        }
    }
    pub async fn query(&self) -> io::Result<UnitState> {
        let mut command = Command::new("/usr/bin/systemctl");
        command
            .args([
                "--user",
                "show",
                "--property=LoadState,ActiveState,SubState,ControlGroup,MainPID",
            ])
            .arg(&self.name);
        let output = manager_output(command).await?;
        parse_state(
            &self.name,
            &String::from_utf8(output.stdout).map_err(io::Error::other)?,
        )
    }
    pub async fn stop(&self) -> io::Result<UnitState> {
        let mut command = Command::new("/usr/bin/systemctl");
        command.args(["--user", "stop"]).arg(&self.name);
        let result = manager_output(command).await;
        let state = self.query().await?;
        if !state.stopped() {
            return Err(io::Error::other(format!(
                "{} still {}; stop={result:?}",
                self.name, state.active_state
            )));
        }
        Ok(state)
    }
    pub async fn reset_failed(&self) -> io::Result<()> {
        let mut command = Command::new("/usr/bin/systemctl");
        command.args(["--user", "reset-failed"]).arg(&self.name);
        match manager_output(command).await {
            Ok(_) => Ok(()),
            Err(_) if self.query().await?.load_state == "not-found" => Ok(()),
            Err(e) => Err(e),
        }
    }
}
fn parse_state(name: &str, text: &str) -> io::Result<UnitState> {
    let mut props = BTreeMap::new();
    for line in text.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| io::Error::other("invalid unit query"))?;
        if props.insert(key, value).is_some() {
            return Err(io::Error::other("duplicate unit property"));
        }
    }
    let get = |key| {
        props
            .get(key)
            .copied()
            .ok_or_else(|| io::Error::other(format!("missing {key}")))
    };
    let pid = get("MainPID")?.parse::<u32>().map_err(io::Error::other)?;
    let cgroup = get("ControlGroup")?;
    Ok(UnitState {
        name: name.into(),
        load_state: get("LoadState")?.into(),
        active_state: get("ActiveState")?.into(),
        sub_state: get("SubState")?.into(),
        cgroup: (!cgroup.is_empty()).then(|| cgroup.into()),
        main_pid: (pid != 0).then_some(pid),
    })
}
async fn manager_output(command: Command) -> io::Result<Output> {
    // Environment assignments can contain credentials. Never format full argv.
    let program = command.get_program().to_string_lossy().into_owned();
    let mut command = tokio::process::Command::from(command);
    command.stdin(Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("{program} timed out")))??;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output)
}
