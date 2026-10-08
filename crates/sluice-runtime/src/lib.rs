//! sluice-runtime: coordination and fn dispatch.

pub mod builtins;
pub mod calls;
pub mod config;
mod contain;
pub mod coordinator;
pub mod drain;
pub mod inline;
mod models;
pub mod naming;
pub mod notify;
pub mod python;
pub mod registry;
pub mod retire;
pub mod scheduler;
pub mod status;
pub mod verify;
pub mod watch;

pub mod client;
pub mod dispatch;
pub mod execution;

pub mod agent_factory;
pub mod compose;
pub mod publication;

mod sidecar;

pub mod dispatch_ext;
pub mod docs;
pub mod dotenv;
pub mod install;
