//! Delegated cgroup v2 handles retain directory descriptors across removal.
use crate::identity::OwnedProcess;
use rustix::{
    fd::{AsRawFd, OwnedFd},
    fs::{CWD, Mode, OFlags, mkdirat, openat, statfs},
};
use sluice_model::{
    ids::InvocationId,
    limits::{GUARDIAN_CPU_MILLIS, GUARDIAN_TASKS, RunLimits, RunUsage},
};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

#[derive(Debug)]
pub struct Cgroup {
    dir: OwnedFd,
    path: String,
}
#[derive(Debug)]
pub struct RunCgroups {
    pub service: Cgroup,
    pub control: Cgroup,
    payload: Cgroup,
}
impl RunCgroups {
    /// Call only inside the admitted delegated service. Move the guardian before
    /// enabling controllers; payload launchers then inherit control membership.
    pub fn create(
        service: Cgroup,
        guardian: &mut OwnedProcess,
        limits: RunLimits,
        guardian_memory_bytes: u64,
    ) -> io::Result<Self> {
        if guardian_memory_bytes == 0
            || limits.memory_bytes == 0
            || limits.cpu_millis == 0
            || limits.tasks == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "run limits must be positive",
            ));
        }
        if guardian.identity().cgroup != service.path() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "guardian is outside service cgroup",
            ));
        }
        service.configure_limits(service_limits(limits, guardian_memory_bytes)?)?;
        let control = service.child("control")?;
        control.move_process(guardian)?;
        service.write("cgroup.subtree_control", b"+cpu +memory +pids")?;
        control.configure_limits(RunLimits {
            memory_bytes: guardian_memory_bytes,
            cpu_millis: GUARDIAN_CPU_MILLIS,
            tasks: GUARDIAN_TASKS,
        })?;
        let payload = service.child("payload")?;
        payload.configure_limits(limits)?;
        payload.write("memory.oom.group", b"1")?;
        if payload.read("memory.oom.group")?.trim() != "1" {
            return Err(io::Error::other("payload OOM grouping was not applied"));
        }
        payload.write("cgroup.subtree_control", b"+cpu +memory +pids")?;
        payload.resource_usage()?;
        Ok(Self {
            service,
            control,
            payload,
        })
    }
    /// The guardian retains terminal authority over every payload leaf.
    pub fn payload(&self) -> &Cgroup {
        &self.payload
    }
    pub fn payload_empty(&self) -> io::Result<bool> {
        Ok(!self.payload.populated()?)
    }
    pub fn invocation(&self, id: InvocationId) -> io::Result<Cgroup> {
        self.service.payload_invocation(id, true)
    }
}
pub(crate) fn service_limits(
    limits: RunLimits,
    guardian_memory_bytes: u64,
) -> io::Result<RunLimits> {
    let overflow = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "run and guardian budget overflow",
        )
    };
    limits
        .validate()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    if guardian_memory_bytes == 0 {
        return Err(overflow());
    }
    Ok(RunLimits {
        memory_bytes: limits
            .memory_bytes
            .checked_add(guardian_memory_bytes)
            .ok_or_else(overflow)?,
        cpu_millis: limits
            .cpu_millis
            .checked_add(GUARDIAN_CPU_MILLIS)
            .ok_or_else(overflow)?,
        tasks: limits
            .tasks
            .checked_add(GUARDIAN_TASKS)
            .ok_or_else(overflow)?,
    })
}
impl Cgroup {
    fn configure_limits(&self, limits: RunLimits) -> io::Result<()> {
        let period = if limits.cpu_millis < 10 {
            1_000_000
        } else {
            100_000
        };
        let cpu = format!("{} {period}", u64::from(limits.cpu_millis) * period / 1000);
        self.write("memory.max", limits.memory_bytes.to_string().as_bytes())?;
        self.write("memory.swap.max", b"0")?;
        self.write("cpu.max", cpu.as_bytes())?;
        self.write("pids.max", limits.tasks.to_string().as_bytes())?;
        let memory: u64 = self
            .read("memory.max")?
            .trim()
            .parse()
            .map_err(|_| io::Error::other("unbounded memory.max"))?;
        if memory == 0
            || memory > limits.memory_bytes
            || self.read("memory.swap.max")?.trim() != "0"
            || self.read("cpu.max")?.trim() != cpu
            || self.read("pids.max")?.trim() != limits.tasks.to_string()
        {
            return Err(io::Error::other("kernel did not retain resource limits"));
        }
        Ok(())
    }

    /// Open or create a leaf in the already admitted service payload subtree.
    pub fn payload_invocation(&self, id: InvocationId, create: bool) -> io::Result<Self> {
        let payload = self.open_child("payload")?;
        let name = id.to_string();
        if create {
            mkdirat(&payload.dir, &name, Mode::RWXU)?;
        }
        payload.open_child(&name)
    }
    /// Only Sluice service roots are accepted. The caller must reconcile and own
    /// this unit; a path alone is not an admission or cancellation capability.
    pub fn open_service(path: &str) -> io::Result<Self> {
        let p = Path::new(path);
        let valid = path.starts_with('/')
            && p.components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
            && p.file_name().and_then(|s| s.to_str()).is_some_and(|s| {
                (s.starts_with("sluice-run-") || s.starts_with("sluice-test-"))
                    && s.ends_with(".service")
            });
        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a Sluice service cgroup",
            ));
        }
        let mount = openat(
            CWD,
            "/sys/fs/cgroup",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )?;
        if statfs("/sys/fs/cgroup")?.f_type != 0x6367_7270 {
            return Err(io::Error::other("cgroup v2 required"));
        }
        let mut dir = mount;
        for component in p.components() {
            if let Component::Normal(name) = component {
                dir = openat(
                    &dir,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )?;
            }
        }
        Ok(Self {
            dir,
            path: path.into(),
        })
    }
    /// Read the retained service's payload subtree without creating directories.
    pub fn payload_populated(&self) -> io::Result<bool> {
        match self.open_child("payload") {
            Ok(payload) => payload.populated(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
    /// Read counters from the retained payload parent, including all sibling leaves.
    pub fn resource_usage(&self) -> io::Result<RunUsage> {
        Ok(RunUsage {
            memory_peak_bytes: self
                .read("memory.peak")?
                .trim()
                .parse()
                .map_err(|_| io::Error::other("invalid memory.peak"))?,
            oom_kills: counter(&self.read("memory.events")?, "oom_kill")?,
            pids_limit_hits: counter(&self.read("pids.events")?, "max")?,
            cpu_throttled_usec: counter(&self.read("cpu.stat")?, "throttled_usec")?,
        })
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn filesystem_path(&self) -> PathBuf {
        Path::new("/sys/fs/cgroup").join(self.path.trim_start_matches('/'))
    }
    fn child(&self, name: &str) -> io::Result<Self> {
        match mkdirat(&self.dir, name, Mode::RWXU) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(e.into()),
        }
        self.open_child(name)
    }
    fn open_child(&self, name: &str) -> io::Result<Self> {
        let dir = openat(
            &self.dir,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )?;
        Ok(Self {
            dir,
            path: format!("{}/{name}", self.path),
        })
    }
    pub(crate) fn read(&self, name: &str) -> io::Result<String> {
        let mut file = File::from(openat(
            &self.dir,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )?);
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(text)
    }
    fn write(&self, name: &str, data: &[u8]) -> io::Result<()> {
        let mut file = File::from(openat(
            &self.dir,
            name,
            OFlags::WRONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )?);
        file.write_all(data)
    }
    /// Placement requires a blocked launcher or the guardian itself. The caller
    /// owns its wait/reap and must not let another thread reap it during this call.
    pub fn move_process(&self, process: &mut OwnedProcess) -> io::Result<()> {
        process.refresh()?;
        self.write(
            "cgroup.procs",
            process.identity().pid.to_string().as_bytes(),
        )?;
        if process.refresh()?.cgroup != self.path {
            return Err(io::Error::other("cgroup placement failed"));
        }
        Ok(())
    }
    pub fn contains(&self, path: &str) -> bool {
        path == self.path
            || path
                .strip_prefix(&self.path)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }
    pub fn populated(&self) -> io::Result<bool> {
        match self.read("cgroup.events") {
            Ok(text) => parse_populated(&text),
            // A removed, pinned cgroup cannot regain members. Never reopen by path.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
    pub async fn wait_populated(&self, expected: bool, timeout: Duration) -> io::Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.populated()? == expected {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{} populated != {expected}", self.path),
                ));
            }
            tokio::time::sleep(
                Duration::from_millis(10)
                    .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
        }
    }
    pub fn kill(&self) -> io::Result<()> {
        match self.write("cgroup.kill", b"1") {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound && !self.populated()? => Ok(()),
            Err(e) => Err(e),
        }
    }
    pub fn member_pids(&self) -> io::Result<Vec<u32>> {
        let mut pids = BTreeSet::new();
        self.members(&mut pids)?;
        Ok(pids.into_iter().collect())
    }
    fn members(&self, pids: &mut BTreeSet<u32>) -> io::Result<()> {
        let text = match self.read("cgroup.procs") {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for pid in text.split_whitespace() {
            pids.insert(
                pid.parse().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid cgroup pid")
                })?,
            );
        }
        // Resolve through the pinned descriptor, including after unlink/recreation.
        for entry in fs::read_dir(format!("/proc/self/fd/{}", self.dir.as_raw_fd()))? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let name = entry.file_name();
                let name = name
                    .to_str()
                    .ok_or_else(|| io::Error::other("invalid cgroup name"))?;
                match self.open_child(name) {
                    Ok(child) => child.members(pids)?,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }
}
fn parse_populated(text: &str) -> io::Result<bool> {
    let mut found = None;
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.first() == Some(&"populated") {
            if found.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate populated",
                ));
            }
            found = match fields.as_slice() {
                ["populated", "0"] => Some(false),
                ["populated", "1"] => Some(true),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid populated",
                    ));
                }
            };
        }
    }
    found.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing populated"))
}
fn counter(text: &str, name: &str) -> io::Result<u64> {
    text.lines()
        .find_map(|line| {
            let (key, value) = line.split_once(' ')?;
            (key == name).then_some(value)
        })
        .ok_or_else(|| io::Error::other(format!("missing cgroup counter {name}")))?
        .trim()
        .parse()
        .map_err(|_| io::Error::other(format!("invalid cgroup counter {name}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_ambiguous_emptiness() {
        assert!(!parse_populated("populated 0\nfrozen 0\n").unwrap());
        assert!(parse_populated("populated 1\n").unwrap());
        for bad in ["", "populated 2", "populated 0\npopulated 1"] {
            assert!(parse_populated(bad).is_err());
        }
    }
}
