//! Durable completion records are replayable evidence, never launch authority.
use crate::{identity::ProcessIdentity, signals::EmptyProof};
use serde::{Deserialize, Serialize};
use sluice_model::{
    error::PublicError,
    ids::*,
    rpc::{JsonMap, MAX_FRAME_BYTES, PROTOCOL_VERSION, decode_json},
};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptKey {
    pub home: HomeId,
    pub project: Option<ProjectId>,
    pub step: Option<StepId>,
    pub generation: StepGeneration,
    pub work: WorkGeneration,
    pub run: RunId,
    pub attempt: AttemptId,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PayloadResult {
    Succeeded(JsonMap),
    Failed(PublicError),
    Rejected(String),
    Cancelled(String),
    Lost(String),
    Unknown(String),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExitEvidence {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub executor: Option<ProcessIdentity>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupEvidence {
    pub cgroup: String,
    pub empty: bool,
    pub escalated: bool,
}
impl From<EmptyProof> for CleanupEvidence {
    fn from(proof: EmptyProof) -> Self {
        Self {
            cgroup: proof.cgroup().into(),
            empty: true,
            escalated: proof.escalated(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartEvidence {
    pub invocation: InvocationId,
    pub executor: ProcessIdentity,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryAck {
    pub invocation: InvocationId,
    pub message: MessageId,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionJournal {
    pub protocol: u16,
    pub identity: AttemptKey,
    pub completion_id: String,
    pub result: PayloadResult,
    pub starts: Vec<StartEvidence>,
    pub exits: Vec<ExitEvidence>,
    pub cleanup: Vec<CleanupEvidence>,
    pub submissions: JsonMap,
    pub submission_version: Option<u64>,
    pub delivery_acks: Vec<DeliveryAck>,
}
impl CompletionJournal {
    pub fn validate(&self, expected: &AttemptKey) -> io::Result<()> {
        if self.protocol != PROTOCOL_VERSION
            || &self.identity != expected
            || self.completion_id.is_empty()
            || self.cleanup.is_empty()
            || self.cleanup.iter().any(|p| !p.empty || p.cgroup.is_empty())
        {
            return Err(invalid(
                "completion identity, protocol or cleanup evidence is invalid",
            ));
        }
        Ok(())
    }
    pub fn write(&self, dir: &Path) -> io::Result<()> {
        self.validate(&self.identity)?;
        if let Some(existing) = Self::read(dir, &self.identity)? {
            if existing == *self {
                return Ok(());
            }
            return Err(invalid("a different completion is already journaled"));
        }
        atomic_json(dir, "completion.json", self)
    }
    pub fn read(dir: &Path, expected: &AttemptKey) -> io::Result<Option<Self>> {
        let journal: Option<Self> = read_json(&dir.join("completion.json"))?;
        if let Some(j) = &journal {
            j.validate(expected)?;
        }
        Ok(journal)
    }
    /// The caller must first verify a durable acknowledgement for these exact IDs.
    pub fn remove(&self, dir: &Path) -> io::Result<()> {
        if let Some(existing) = Self::read(dir, &self.identity)? {
            if existing != *self {
                return Err(invalid("completion changed before acknowledgement"));
            }
            // The guardian and an adopter may both hold an acknowledgement
            // for the same journal; whichever removes it second finds it gone.
            match fs::remove_file(dir.join("completion.json")) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            match fs::remove_file(dir.join("collected.json")) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            File::open(dir)?.sync_all()?;
        }
        Ok(())
    }
}
pub(crate) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FRAME_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    decode_json(&bytes).map(Some).map_err(io::Error::other)
}
pub(crate) fn atomic_json<T: Serialize + ?Sized>(
    dir: &Path,
    name: &str,
    value: &T,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let _: sluice_model::rpc::JsonValue = decode_json(&bytes).map_err(io::Error::other)?;
    let temporary = dir.join(format!(".{name}-{}", InvocationId::new()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, dir.join(name))?;
        File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
