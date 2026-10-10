//! Scratch homes with live work, built with the old release's own coordinator: what the
//! rehearsal's tests and the legacy fixture generator start from. The `Launcher` host accepts
//! every launch and starts nothing, so an admitted step stays live (its attempt nonterminal,
//! its run unfinished) until the test completes it.

use serde_json::Value;
use sluice_model::{
    commands::{CommandReply, CommandRequest},
    error::PublicError,
    rpc::{FnInvocation, JsonMap, decode_json},
};
use sluice_process::{
    guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence},
    journal::{CleanupEvidence, CompletionJournal, PayloadResult},
};
use sluice_runtime::{
    coordinator::Coordinator,
    execution::{ExecutionHost, Launch, LaunchOutcome},
};
use std::sync::{Arc, Mutex};

/// Accepts every launch without starting anything; adoption finds its runs still owned.
#[derive(Clone, Default)]
pub struct Launcher {
    pub launches: Arc<Mutex<Vec<Launch>>>,
}
impl Launcher {
    pub fn launches(&self) -> Vec<Launch> {
        self.launches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}
impl FnHost for Launcher {
    async fn invoke(&self, _: FnInvocation) -> Result<JsonMap, PublicError> {
        Ok(JsonMap::default())
    }
}
impl AdoptionHost for Launcher {
    async fn reconcile(&self, _: &AdoptionAttempt) -> std::io::Result<GuardianPresence> {
        Ok(GuardianPresence::Ambiguous(
            "the scratch launcher owns it".into(),
        ))
    }
}
impl ExecutionHost for Launcher {
    async fn launch(&self, launch: Launch) -> Result<LaunchOutcome, PublicError> {
        self.launches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(launch);
        Ok(LaunchOutcome::Accepted)
    }
    async fn cleanup_valid(&self, journal: &CompletionJournal) -> Result<bool, PublicError> {
        Ok(journal.cleanup.iter().all(|proof| proof.empty))
    }
}

/// A command given as its JSON (`{"command": …, "args": …}`).
pub async fn command<H: ExecutionHost>(
    broker: &Coordinator<H>,
    request: Value,
) -> Result<CommandReply, PublicError> {
    let request: CommandRequest = decode_json(&serde_json::to_vec(&request).expect("JSON"))
        .unwrap_or_else(|e| panic!("{request}: {e}"));
    broker.command(request).await
}

/// A `Data` reply's value.
pub async fn data<H: ExecutionHost>(broker: &Coordinator<H>, request: Value) -> Value {
    match command(broker, request.clone()).await {
        Ok(CommandReply::Data(value)) => value.into_value(),
        other => panic!("{request}: {other:?}"),
    }
}

pub fn map(value: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&value).expect("JSON")).expect("a JSON object")
}

/// The completion journal a guardian would write for `launch`.
pub fn journal(launch: &Launch, result: PayloadResult) -> CompletionJournal {
    CompletionJournal {
        protocol: 1,
        identity: launch.identity.clone(),
        completion_id: format!("complete-{}", launch.identity.run),
        result,
        starts: vec![],
        exits: vec![],
        cleanup: vec![CleanupEvidence {
            cgroup: format!("scratch/{}", launch.identity.run),
            empty: true,
            escalated: false,
        }],
        submissions: JsonMap::default(),
        submission_version: None,
        delivery_acks: vec![],
    }
}
