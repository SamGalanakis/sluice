use crate::support::home;

use std::{
    os::unix::net::UnixStream,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub struct ScratchHome {
    home: home::ScratchHome,
    coordinator: Option<Child>,
}
impl ScratchHome {
    pub fn new() -> Result<Self, sluice_model::error::PublicError> {
        Ok(Self {
            home: home::ScratchHome::new()?,
            coordinator: None,
        })
    }
    pub fn served() -> Result<Self, sluice_model::error::PublicError> {
        let mut home = Self::new()?;
        home.coordinator = Some(
            Command::new(env!("CARGO_BIN_EXE_sluice"))
                .env("SLUICE_HOME", home.path())
                .env("TOKIO_WORKER_THREADS", "2")
                .arg("coordinator")
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while UnixStream::connect(home.path().join("coordinator.sock")).is_err() {
            assert!(
                home.coordinator
                    .as_mut()
                    .unwrap()
                    .try_wait()
                    .unwrap()
                    .is_none(),
                "coordinator exited"
            );
            assert!(Instant::now() < deadline, "coordinator never bound");
            std::thread::yield_now();
        }
        Ok(home)
    }
    pub fn path(&self) -> &Path {
        self.home.path()
    }
    pub fn root(&self) -> &Path {
        self.home.root()
    }
}
impl Drop for ScratchHome {
    fn drop(&mut self) {
        if let Some(mut child) = self.coordinator.take() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(self.path().join("coordinator.sock"));
        }
    }
}
