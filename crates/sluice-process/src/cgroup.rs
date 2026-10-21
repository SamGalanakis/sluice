//! Delegated cgroup v2 handles retain directory descriptors across removal.
use crate::identity::OwnedProcess;
use rustix::{
    fd::{AsRawFd, OwnedFd},
    fs::{CWD, Mode, OFlags, mkdirat, openat, statfs},
};
use sluice_model::{
    ids::InvocationId,
    limits::{GUARDIAN_CPU_MILLIS, GUARDIAN_TASKS, Limits, RunLimits, RunUsage},
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
        if guardian_memory_bytes == 0 || limits.validate().is_err() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "run limits must be positive or unlimited",
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
        control.configure_limits(Limits {
            memory_bytes: Some(guardian_memory_bytes),
            cpu_millis: Some(GUARDIAN_CPU_MILLIS),
            tasks: Some(GUARDIAN_TASKS),
        })?;
        let payload = service.child("payload")?;
        payload.configure_limits(limits.caps())?;
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
/// The service's caps: the payload's plus the guardian's share; unlimited stays unlimited.
pub fn service_limits(limits: RunLimits, guardian_memory_bytes: u64) -> io::Result<Limits> {
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
    let caps = limits.caps();
    Ok(Limits {
        memory_bytes: caps
            .memory_bytes
            .map(|n| n.checked_add(guardian_memory_bytes).ok_or_else(overflow))
            .transpose()?,
        cpu_millis: caps
            .cpu_millis
            .map(|n| n.checked_add(GUARDIAN_CPU_MILLIS).ok_or_else(overflow))
            .transpose()?,
        tasks: caps
            .tasks
            .map(|n| n.checked_add(GUARDIAN_TASKS).ok_or_else(overflow))
            .transpose()?,
    })
}
impl Cgroup {
    /// Unlimited dimensions are written as `max`, and read back the same way.
    fn configure_limits(&self, limits: Limits) -> io::Result<()> {
        let cpu = match limits.cpu_millis {
            None => "max 100000".to_owned(),
            Some(millis) => {
                let period = if millis < 10 { 1_000_000 } else { 100_000 };
                format!("{} {period}", u64::from(millis) * period / 1000)
            }
        };
        let text = |n: Option<String>| n.unwrap_or_else(|| "max".into());
        let memory = text(limits.memory_bytes.map(|n| n.to_string()));
        let tasks = text(limits.tasks.map(|n| n.to_string()));
        self.write("memory.max", memory.as_bytes())?;
        self.write("memory.swap.max", b"0")?;
        self.write("cpu.max", cpu.as_bytes())?;
        self.write("pids.max", tasks.as_bytes())?;
        let retained = match limits.memory_bytes {
            None => self.read("memory.max")?.trim() == "max",
            Some(limit) => self
                .read("memory.max")?
                .trim()
                .parse::<u64>()
                .is_ok_and(|n| n != 0 && n <= limit),
        };
        if !retained
            || self.read("memory.swap.max")?.trim() != "0"
            || self.read("cpu.max")?.trim() != cpu
            || self.read("pids.max")?.trim() != tasks
        {
            return Err(io::Error::other("kernel did not retain resource limits"));
        }
        Ok(())
    }
    /// The payload's limit counters of a service this process does not own (the coordinator
    /// sampling a live run). Only Sluice service roots are opened.
    pub fn service_usage(service: &str) -> io::Result<RunUsage> {
        Self::open_service(service)?
            .open_child("payload")?
            .resource_usage()
    }

    /// The payload's memory charge, composition and PSI of a service this process does not own (the
    /// coordinator measuring a live run for admission); `None` before the payload exists.
    pub fn service_memory(service: &str) -> io::Result<Option<sluice_model::limits::RunMemory>> {
        let payload = match Self::open_service(service)?.open_child("payload") {
            Ok(payload) => payload,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(Some(payload.memory()?))
    }

    /// Current payload memory split into working memory and reclaimable file cache.
    pub fn memory(&self) -> io::Result<sluice_model::limits::RunMemory> {
        let number = |name: &str| -> io::Result<u64> {
            self.read(name)?
                .trim()
                .parse()
                .map_err(|_| io::Error::other(format!("invalid {name}")))
        };
        let stat = self.read("memory.stat")?;
        let shmem = counter(&stat, "shmem")?;
        let pressure_some_avg10 = match self.read("memory.pressure") {
            Ok(text) => text.lines().find_map(|line| {
                line.strip_prefix("some ")?
                    .split_whitespace()
                    .find_map(|field| field.strip_prefix("avg10=")?.parse().ok())
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        Ok(sluice_model::limits::RunMemory {
            current_bytes: number("memory.current")?,
            peak_bytes: number("memory.peak")?,
            working_bytes: Some(
                counter(&stat, "anon")?
                    .saturating_add(shmem)
                    .saturating_add(counter(&stat, "slab_unreclaimable")?),
            ),
            file_cache_bytes: Some(counter(&stat, "file")?.saturating_sub(shmem)),
            pressure_some_avg10,
            oom_events: counter(&self.read("memory.events")?, "oom")?,
        })
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
            memory_working_peak_bytes: self.memory()?.working_bytes,
            oom_kills: counter(&self.read("memory.events")?, "oom_kill")?,
            pids_limit_hits: self.pids_limit_hits()?,
            cpu_throttled_usec: counter(&self.read("cpu.stat")?, "throttled_usec")?,
            memory_max_hits: counter(&self.read("memory.events")?, "max")?,
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
    /// Failed forks in this subtree. Before kernels with `pids.events.local`, `pids.events`
    /// counts only in the cgroup whose task forked, so the leaves are summed.
    fn pids_limit_hits(&self) -> io::Result<u64> {
        let own = counter(&self.read("pids.events")?, "max")?;
        if self.read("pids.events.local").is_ok() {
            return Ok(own);
        }
        let mut total = own;
        for entry in fs::read_dir(format!("/proc/self/fd/{}", self.dir.as_raw_fd()))? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let name = entry.file_name();
                let name = name
                    .to_str()
                    .ok_or_else(|| io::Error::other("invalid cgroup name"))?;
                match self
                    .open_child(name)
                    .and_then(|child| child.pids_limit_hits())
                {
                    Ok(n) => total = total.saturating_add(n),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(total)
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
