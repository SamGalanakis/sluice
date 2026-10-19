//! Argument-safe transient user services. An attempted start is never retried.
use crate::host::test_mode;
use serde::{Deserialize, Serialize};
use sluice_model::{
    ids::{HomeId, RunId},
    limits::{Limits, RunLimits},
};
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
    pub workload_slice: Option<WorkloadSlice>,
    pub run_budget: Option<(RunLimits, u64)>,
    pub start_fence: Option<crate::locks::RunFence>,
}
impl ServiceCommand {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            workload_slice: None,
            run_budget: None,
            start_fence: None,
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
        let budget = spec
            .run_budget
            .map(|(limits, guardian)| crate::cgroup::service_limits(limits, guardian))
            .transpose()?;
        let _slice_guard = if let Some(slice) = &spec.workload_slice {
            let guard = slice.lock().await?;
            slice.ensure_locked().await?;
            Some(guard)
        } else {
            None
        };
        // A failed preflight query has not attempted a launch and is safe to retry.
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
                "--property=OOMPolicy=continue",
                "--property=KillMode=control-group",
                "--property=Restart=no",
                "--property=TimeoutStopSec=5s",
            ])
            .arg(format!("--description=Sluice run {}", self.name));
        if let Some(slice) = &spec.workload_slice {
            command.arg(format!("--slice={}", slice.name));
        }
        if let Some(budget) = budget {
            let properties = properties(budget);
            tracing::info!(unit = %self.name, ?properties, "starting a run's service with its limits");
            for property in properties {
                command.arg(format!("--property={property}"));
            }
        }
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
        // Reserve before the helper can send a start to the manager. From here onward,
        // a timeout is ambiguous and may only be reconciled, never replayed.
        self.attempted = true;
        let result = manager_start(command, spec.start_fence.as_ref()).await;
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
        let output = manager_probe(command).await?;
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
async fn manager_start(
    command: Command,
    fence: Option<&crate::locks::RunFence>,
) -> io::Result<Output> {
    let Some(fence) = fence else {
        return manager_output(command).await;
    };
    let child = fence.spawn_start_helper(command)?;
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "systemd start still pending"))??;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "systemd-run: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output)
}

fn manager_command(command: Command) -> tokio::process::Command {
    let mut command = tokio::process::Command::from(command);
    // `systemctl --user` finds the manager through XDG_RUNTIME_DIR, which a scrubbed
    // environment may lack.
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        command.env(
            "XDG_RUNTIME_DIR",
            format!("/run/user/{}", rustix::process::getuid().as_raw()),
        );
    }
    command
}
async fn manager_probe(command: Command) -> io::Result<Output> {
    let output = crate::probe::output(&mut manager_command(command)).await?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "systemd observation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output)
}
async fn manager_output(command: Command) -> io::Result<Output> {
    // Environment assignments can contain credentials. Never format full argv.
    let program = command.get_program().to_string_lossy().into_owned();
    let mut command = manager_command(command);
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

/// A run service's limits as they are in force: systemd's properties and the kernel's files in
/// its cgroup. Unlimited reads `infinity` (systemd) or `max` (kernel).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedLimits {
    pub memory_max: String,
    pub memory_swap_max: String,
    pub tasks_max: String,
    pub cgroup_memory_max: String,
    pub cgroup_memory_swap_max: String,
    pub cgroup_pids_max: String,
    pub cgroup_cpu_max: String,
}
impl AppliedLimits {
    async fn read(unit: &str) -> io::Result<Self> {
        let mut show = Command::new("/usr/bin/systemctl");
        show.args([
            "--user",
            "show",
            "--property=MemoryMax",
            "--property=MemorySwapMax",
            "--property=TasksMax",
            "--property=ControlGroup",
            unit,
        ]);
        let output = manager_probe(show).await?;
        let state = String::from_utf8_lossy(&output.stdout);
        let field = |key: &str| {
            state
                .lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
                .unwrap_or_default()
                .to_owned()
        };
        let group = field("ControlGroup");
        if !group.starts_with('/') {
            return Err(io::Error::other(format!("{unit} has no cgroup")));
        }
        let dir = PathBuf::from("/sys/fs/cgroup").join(group.trim_start_matches('/'));
        let file = |name: &str| -> io::Result<String> {
            Ok(std::fs::read_to_string(dir.join(name))?.trim().to_owned())
        };
        Ok(Self {
            memory_max: field("MemoryMax"),
            memory_swap_max: field("MemorySwapMax"),
            tasks_max: field("TasksMax"),
            cgroup_memory_max: file("memory.max")?,
            cgroup_memory_swap_max: file("memory.swap.max")?,
            cgroup_pids_max: file("pids.max")?,
            cgroup_cpu_max: file("cpu.max")?,
        })
    }
    /// Every way these differ from `caps`, empty when they match. The kernel keeps memory.max
    /// rounded down to a whole page.
    pub fn mismatches(&self, caps: Limits) -> Vec<String> {
        let mut out = Vec::new();
        let mut differ = |name: &str, actual: &str, expected: String| {
            out.push(format!("{name} is {actual}, expected {expected}"));
        };
        let number =
            |n: Option<u64>, unlimited: &str| n.map_or(unlimited.into(), |n| n.to_string());
        let memory = caps.memory_bytes;
        if self.memory_max != number(memory, "infinity") {
            differ("MemoryMax", &self.memory_max, number(memory, "infinity"));
        }
        let page = rustix::param::page_size() as u64;
        let kernel_memory = match memory {
            None => self.cgroup_memory_max == "max",
            Some(cap) => self
                .cgroup_memory_max
                .parse::<u64>()
                .is_ok_and(|n| n <= cap && cap - n < page),
        };
        if !kernel_memory {
            differ("memory.max", &self.cgroup_memory_max, number(memory, "max"));
        }
        if self.memory_swap_max != "0" {
            differ("MemorySwapMax", &self.memory_swap_max, "0".into());
        }
        if self.cgroup_memory_swap_max != "0" {
            differ("memory.swap.max", &self.cgroup_memory_swap_max, "0".into());
        }
        let tasks = caps.tasks.map(u64::from);
        if self.tasks_max != number(tasks, "infinity") {
            differ("TasksMax", &self.tasks_max, number(tasks, "infinity"));
        }
        if self.cgroup_pids_max != number(tasks, "max") {
            differ("pids.max", &self.cgroup_pids_max, number(tasks, "max"));
        }
        if !cpu_matches(&self.cgroup_cpu_max, caps.cpu_millis) {
            let expected = caps
                .cpu_millis
                .map_or("max".into(), |n| format!("{n} milli-CPUs"));
            differ("cpu.max", &self.cgroup_cpu_max, expected);
        }
        out
    }
}
/// Whether a kernel `cpu.max` ("quota period" or "max period") grants `millis` (unlimited:
/// `None`).
fn cpu_matches(raw: &str, millis: Option<u32>) -> bool {
    let mut parts = raw.split_whitespace();
    let quota = parts.next();
    let period = parts.next().and_then(|v| v.parse::<u64>().ok());
    match (millis, quota.and_then(|q| q.parse::<u64>().ok()), period) {
        (None, _, _) => quota == Some("max"),
        (Some(millis), Some(q), Some(p)) => {
            u128::from(q) * 1000 == u128::from(p) * u128::from(millis)
        }
        _ => false,
    }
}
/// Read back what a run's service has in force and compare it with its frozen budget (the
/// payload's caps plus the guardian's share). A mismatch is applied again with
/// `systemctl set-property` and read back once more; one that persists is an error, and the
/// caller must not start a payload.
pub async fn enforce_run_limits(
    unit: &str,
    limits: RunLimits,
    guardian_memory_bytes: u64,
) -> io::Result<AppliedLimits> {
    let caps = crate::cgroup::service_limits(limits, guardian_memory_bytes)?;
    let applied = AppliedLimits::read(unit).await?;
    let wrong = applied.mismatches(caps);
    if wrong.is_empty() {
        return Ok(applied);
    }
    tracing::warn!(%unit, mismatches = ?wrong, "run limits not in force; applying them again");
    let mut update = Command::new("/usr/bin/systemctl");
    update.args(["--user", "set-property", "--runtime", unit]);
    update.args(properties(caps));
    let repaired = manager_output(update).await;
    let applied = AppliedLimits::read(unit).await?;
    let wrong = applied.mismatches(caps);
    if wrong.is_empty() {
        return Ok(applied);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{unit} does not hold its execution limits after set-property ({}): {}",
            match repaired {
                Ok(_) => "accepted".to_owned(),
                Err(error) => error.to_string(),
            },
            wrong.join("; ")
        ),
    ))
}

/// One home's workload budget. Services that coordinate runs do not join it.
#[derive(Debug, Clone)]
pub struct WorkloadSlice {
    name: String,
    totals: Limits,
}
/// systemd properties for caps: unlimited memory and tasks are `infinity`, unlimited CPU an
/// empty quota (none).
fn properties(caps: Limits) -> Vec<String> {
    let infinity = |n: Option<String>| n.unwrap_or_else(|| "infinity".into());
    vec![
        format!(
            "MemoryMax={}",
            infinity(caps.memory_bytes.map(|n| n.to_string()))
        ),
        "MemorySwapMax=0".into(),
        match caps.cpu_millis {
            Some(millis) => format!("CPUQuota={}.{}%", millis / 10, millis % 10),
            None => "CPUQuota=".into(),
        },
        format!("TasksMax={}", infinity(caps.tasks.map(|n| n.to_string()))),
    ]
}
impl WorkloadSlice {
    pub fn new(home: HomeId, totals: Limits) -> io::Result<Self> {
        if totals.validate().is_err() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "slice limits must be positive or unlimited",
            ));
        }
        Ok(Self {
            name: format!(
                "{}workloads-{}.slice",
                unit_prefix(),
                home.to_string().replace('-', "_")
            ),
            totals,
        })
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    /// The slice's cgroup directory, or `None` while systemd has not created it.
    pub async fn control_group(&self) -> io::Result<Option<PathBuf>> {
        let mut show = Command::new("/usr/bin/systemctl");
        show.args([
            "--user",
            "show",
            "--property=ControlGroup",
            "--value",
            &self.name,
        ]);
        let output = manager_probe(show).await?;
        let group = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Ok(group
            .starts_with('/')
            .then(|| PathBuf::from("/sys/fs/cgroup").join(group.trim_start_matches('/'))))
    }
    pub async fn ensure(&self) -> io::Result<()> {
        let _guard = self.lock().await?;
        self.ensure_locked().await
    }
    async fn lock(&self) -> io::Result<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = format!(
            "/run/user/{}/{}.lock",
            rustix::process::getuid().as_raw(),
            self.name
        );
        tokio::task::spawn_blocking(move || {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(path)?;
            file.lock()?;
            Ok(file)
        })
        .await
        .map_err(io::Error::other)?
    }
    async fn ensure_locked(&self) -> io::Result<()> {
        let mut show = Command::new("/usr/bin/systemctl");
        show.args([
            "--user",
            "show",
            "--property=LoadState",
            "--property=MemoryMax",
            "--property=MemorySwapMax",
            "--property=CPUQuotaPerSecUSec",
            "--property=TasksMax",
            "--property=ControlGroup",
            &self.name,
        ]);
        let output = manager_probe(show).await?;
        let state = String::from_utf8_lossy(&output.stdout);
        let field = |key: &str| state.lines().find_map(|line| line.strip_prefix(key));
        if field("LoadState=") != Some("loaded") {
            return Err(io::Error::other(
                "systemd could not load the workload slice",
            ));
        }
        let memory_matches = match self.totals.memory_bytes {
            Some(n) => field("MemoryMax=").and_then(|v| v.parse::<u64>().ok()) == Some(n),
            None => field("MemoryMax=") == Some("infinity"),
        };
        let tasks_matches = match self.totals.tasks {
            Some(n) => field("TasksMax=").and_then(|v| v.parse::<u32>().ok()) == Some(n),
            None => field("TasksMax=") == Some("infinity"),
        };
        // systemctl formats CPUQuotaPerSecUSec as a duration. Read the kernel's
        // quota instead, which also proves the active slice received the limits.
        let group = field("ControlGroup=").filter(|g| g.starts_with('/'));
        let cpu_matches = group
            .and_then(|g| std::fs::read_to_string(format!("/sys/fs/cgroup{g}/cpu.max")).ok())
            .is_some_and(|v| cpu_matches(&v, self.totals.cpu_millis));
        if memory_matches && tasks_matches && cpu_matches && field("MemorySwapMax=") == Some("0") {
            return Ok(());
        }
        // Admission already proved that the live reservations fit these totals, so a changed
        // total applies to a populated slice: existing runs keep their own frozen caps.
        let mut update = Command::new("/usr/bin/systemctl");
        update.args(["--user", "set-property", "--runtime", &self.name]);
        update.args(properties(self.totals));
        manager_output(update).await?;
        Ok(())
    }
}
