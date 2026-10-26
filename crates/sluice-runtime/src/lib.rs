//! sluice-runtime: coordination and fn dispatch.

pub mod builtins;
pub mod calls;
pub mod config;
mod contain;
pub mod coordinator;
pub mod drain;
pub mod edits;
pub mod headroom;
pub mod inline;
mod models;
pub mod naming;
pub mod notify;
pub mod park;
pub mod plan_cache;
pub mod python;
pub mod registry;
pub mod retire;
pub mod scheduler;
pub mod status;
pub mod verify;
pub mod watch;

pub mod client;
pub mod dispatch;
mod engine_cap;
pub mod execution;

pub mod agent_factory;
pub mod compose;
pub mod publication;

mod sidecar;

pub mod dispatch_ext;
pub mod docs;
pub mod dotenv;
pub mod install;

pub mod budget;

pub mod subscriptions;

mod routine;
pub mod run_health;
mod run_timing;
