//! The engine hook journal, `<run_dir>/engine-hooks`: how the guardian hands an engine hook
//! to the run's agent supervisor and waits for its decision.
//!
//! One exchange is `<id>.request.json`, written by the guardian; the supervisor claims it by
//! renaming it to `<id>.claimed.json` before deciding (claim before decision is the
//! at-most-once boundary) and answers with `<id>.reply.json`, written through
//! `<id>.reply.tmp`. The journal holds only exchanges in flight, so its size is bounded by
//! the hooks in flight, never by the hooks a run has had:
//!
//! - The supervisor owns claims. It removes a claim once the reply is durable, and at the
//!   start of every pass removes any claim and reply temporary it finds: it decides
//!   synchronously within a pass and one supervisor at a time serves a journal, so such a
//!   claim is either answered (a crash before the removal, or an older release) or uncertain
//!   (a crash while deciding), which is never decided again.
//! - The guardian owns replies. It removes its reply once it has read it, and before each
//!   exchange removes every reply present (with the claim it answers, if still there) and its
//!   own write temporaries: it waits on one exchange at a time, so a reply left then is one it stopped
//!   waiting for (a decision that timed out, a crash, or an older release) and nobody reads.
//! - Nobody removes an unclaimed request. One whose wait timed out is still decided, or
//!   refused by the next supervisor start, and its reply goes with the next exchange.
use crate::{
    journal::{atomic_json, read_json},
    socket::{EngineHookReply, EngineHookRequest},
};
use sluice_model::{error::PublicError, ids::InvocationId, rpc::decode_json};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

pub const DIRECTORY: &str = "engine-hooks";
/// Unclaimed requests plus claims without a reply. The guardian waits on one hook at a
/// time, so only a supervisor that stops claiming lets this grow.
pub const MAX_IN_FLIGHT: usize = 1024;
const MAX_REPLY_BYTES: usize = 1024 * 1024;
const REQUEST: &str = ".request.json";
const CLAIM: &str = ".claimed.json";
const REPLY: &str = ".reply.json";
const REPLY_TEMPORARY: &str = ".reply.tmp";

pub fn path(run_dir: &Path) -> PathBuf {
    run_dir.join(DIRECTORY)
}

/// The journal's exchanges by state, each a sorted set of exchange ids.
#[derive(Debug, Default)]
pub struct Listing {
    pub requests: BTreeSet<String>,
    pub claims: BTreeSet<String>,
    pub replies: BTreeSet<String>,
    reply_temporaries: Vec<String>,
    request_temporaries: Vec<String>,
}
impl Listing {
    pub fn read(journal: &Path) -> io::Result<Self> {
        let mut listing = Self::default();
        let entries = match fs::read_dir(journal) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(listing),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                // atomic_json's temporary for a request: `.<id>.request.json-<nonce>`.
                if name.contains(&format!("{REQUEST}-")) {
                    listing.request_temporaries.push(name);
                }
            } else if let Some(id) = name.strip_suffix(REQUEST) {
                listing.requests.insert(id.to_owned());
            } else if let Some(id) = name.strip_suffix(CLAIM) {
                listing.claims.insert(id.to_owned());
            } else if let Some(id) = name.strip_suffix(REPLY) {
                listing.replies.insert(id.to_owned());
            } else if name.ends_with(REPLY_TEMPORARY) {
                listing.reply_temporaries.push(name);
            }
        }
        Ok(listing)
    }
    fn undecided(&self) -> usize {
        self.claims.difference(&self.replies).count()
    }
}
fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}
fn storage(error: io::Error) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}

/// The guardian's side of one exchange: journal the request and wait for its reply.
/// Dropping the future (the guardian's decision timeout) leaves the request to be decided
/// and its reply to the next exchange.
pub async fn exchange(
    run_dir: &Path,
    request: &EngineHookRequest,
) -> Result<EngineHookReply, PublicError> {
    let journal = path(run_dir);
    fs::create_dir_all(&journal).map_err(storage)?;
    let listing = Listing::read(&journal).map_err(storage)?;
    for id in &listing.replies {
        // The claim first: a crash between the two leaves a reply, never a claim that
        // looks undecided.
        remove(&journal.join(format!("{id}{CLAIM}"))).map_err(storage)?;
        remove(&journal.join(format!("{id}{REPLY}"))).map_err(storage)?;
    }
    for name in &listing.request_temporaries {
        remove(&journal.join(name)).map_err(storage)?;
    }
    let unclaimed = listing.requests.len();
    let undecided = listing.undecided();
    if unclaimed + undecided >= MAX_IN_FLIGHT {
        return Err(PublicError::BadRequest {
            message: format!(
                "engine hook journal bound exceeded: {} hooks in flight ({unclaimed} unclaimed, \
                 {undecided} claimed without a reply; the limit is {MAX_IN_FLIGHT}); the \
                 supervisor is not answering hooks",
                unclaimed + undecided
            ),
        });
    }
    let id = InvocationId::new().to_string();
    atomic_json(&journal, &format!("{id}{REQUEST}"), request).map_err(storage)?;
    let reply_path = journal.join(format!("{id}{REPLY}"));
    loop {
        if let Some(reply) =
            read_json::<Result<EngineHookReply, PublicError>>(&reply_path).map_err(storage)?
        {
            remove(&journal.join(format!("{id}{CLAIM}"))).map_err(storage)?;
            remove(&reply_path).map_err(storage)?;
            return reply;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The supervisor's start of a pass: no claim is being decided, so every claim and reply
/// temporary is a leftover (see the module documentation). Returns what is left.
pub fn settle_claims(journal: &Path) -> io::Result<Listing> {
    let mut listing = Listing::read(journal)?;
    for name in listing.reply_temporaries.drain(..) {
        remove(&journal.join(name))?;
    }
    for id in std::mem::take(&mut listing.claims) {
        remove(&journal.join(format!("{id}{CLAIM}")))?;
    }
    Ok(listing)
}

/// Claims an unclaimed request; `None` when it is no longer there.
pub fn claim(journal: &Path, id: &str) -> io::Result<Option<EngineHookRequest>> {
    let path = journal.join(format!("{id}{REQUEST}"));
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let request: EngineHookRequest = decode_json(&bytes).map_err(io::Error::other)?;
    match fs::rename(&path, journal.join(format!("{id}{CLAIM}"))) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    }
    fs::File::open(journal)?.sync_all()?;
    Ok(Some(request))
}

/// Answers a claim: the reply is made durable, then the claim is removed.
pub fn answer(
    journal: &Path,
    id: &str,
    result: &Result<EngineHookReply, PublicError>,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(result).map_err(io::Error::other)?;
    if bytes.len() > MAX_REPLY_BYTES {
        return Err(io::Error::other("hook reply too large"));
    }
    let tmp = journal.join(format!("{id}{REPLY_TEMPORARY}"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(tmp, journal.join(format!("{id}{REPLY}")))?;
    fs::File::open(journal)?.sync_all()?;
    remove(&journal.join(format!("{id}{CLAIM}")))
}

/// The unclaimed requests, for a supervisor deciding whether it must look before claiming.
pub fn requests(run_dir: &Path) -> io::Result<Vec<EngineHookRequest>> {
    let journal = path(run_dir);
    let mut requests = Vec::new();
    for id in Listing::read(&journal)?.requests {
        match fs::read(journal.join(format!("{id}{REQUEST}"))) {
            Ok(bytes) => requests.push(decode_json(&bytes).map_err(io::Error::other)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(requests)
}
