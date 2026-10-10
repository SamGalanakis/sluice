//! `RehearsalHost`: the execution, fn and adoption host the rehearsal opens the old
//! coordinator with. It reaches nothing outside the copy: it never calls `systemd-run` or
//! `systemctl`, never signals a process, never opens a cgroup or a socket and launches
//! nothing. Every reconcile reports the guardian gone with its processes proven gone, so one
//! adoption pass settles every nonterminal attempt through the old release's own completion
//! path: cancelled where a cancel intent was stored, lost otherwise.
//!
//! The copy's runs name the live home's units and cgroups (a copy keeps every identity), so
//! anything that looked them up could reach, and stop, production work. This host only
//! records which runs it was asked about; `tests/isolation.rs` holds it to that against a
//! real unit of the same name.

use sluice_model::{
    error::PublicError,
    ids::RunId,
    rpc::{FnInvocation, JsonMap},
};
use sluice_process::{
    guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence},
    journal::{CleanupEvidence, CompletionJournal},
};
use sluice_runtime::execution::{ExecutionHost, Launch, LaunchOutcome};
use std::sync::{Arc, Mutex};

/// What the host was asked to do, for the report and the tests.
#[derive(Debug, Default)]
pub struct Asked {
    /// Runs reconciled, in order.
    pub reconciled: Vec<RunId>,
    /// Launches refused (none should be asked for: the drain refuses new work).
    pub launches: Vec<RunId>,
    /// Fn invocations refused.
    pub invocations: Vec<String>,
}

#[derive(Clone, Default)]
pub struct RehearsalHost {
    asked: Arc<Mutex<Asked>>,
}
impl RehearsalHost {
    pub fn new() -> Self {
        Self::default()
    }
    /// The runs reconciled, launches refused and invocations refused so far.
    pub fn asked(&self) -> std::sync::MutexGuard<'_, Asked> {
        self.asked.lock().unwrap_or_else(|e| e.into_inner())
    }
}
fn refused(what: &str) -> PublicError {
    PublicError::Busy {
        message: format!("the cutover rehearsal {what}"),
        retryable: false,
    }
}
impl FnHost for RehearsalHost {
    async fn invoke(&self, invocation: FnInvocation) -> Result<JsonMap, PublicError> {
        self.asked().invocations.push(invocation.name.clone());
        Err(refused("runs no fn"))
    }
}
impl AdoptionHost for RehearsalHost {
    async fn reconcile(&self, attempt: &AdoptionAttempt) -> std::io::Result<GuardianPresence> {
        self.asked().reconciled.push(attempt.identity.run);
        // Not a cgroup path: nothing may take it for one. The evidence says what the copy
        // proves, that no process of this copy exists.
        Ok(GuardianPresence::Gone(CleanupEvidence {
            cgroup: format!("rehearsal-copy:{}", attempt.unit),
            empty: true,
            escalated: false,
        }))
    }
}
impl ExecutionHost for RehearsalHost {
    /// The catalog the old coordinator builds for the home: its configured fn registry.
    fn composition_enabled(&self) -> bool {
        true
    }
    async fn launch(&self, launch: Launch) -> Result<LaunchOutcome, PublicError> {
        self.asked().launches.push(launch.identity.run);
        // No process was created, which is what `Refused` promises.
        Ok(LaunchOutcome::Refused(refused("launches nothing")))
    }
    async fn cleanup_valid(&self, journal: &CompletionJournal) -> Result<bool, PublicError> {
        Ok(journal.cleanup.iter().all(|proof| proof.empty))
    }
}
