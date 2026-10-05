//! sluice-process: run and process ownership.

pub mod cgroup;
pub mod guardian;
pub mod hook_journal;
pub mod host;
pub mod identity;
pub mod journal;
pub mod launcher;
pub mod locks;
pub mod proc;
pub mod signals;
pub mod socket;
pub mod spawn;
pub mod systemd;
pub mod tmux;

pub use guardian::FnHost;
