//! What a parked run waits for (`sluice tool park --until`), shared by the CLI that parses
//! it, the coordinator that records it and the run's supervisor that watches it.
use serde::{Deserialize, Serialize};

use crate::{error::PublicError, ids::LeaseId};

/// The default bound, also used for parks recorded by releases that allowed no timeout.
pub const DEFAULT_PARK_TIMEOUT_SECONDS: u64 = 3600;

/// One `--until` condition. Its text form is what the wake message names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParkCondition {
    /// A section lease on the project resource: granted fires it.
    Lease { resource: String, amount: u64 },
    /// The process exits. `start` is its start time (clock ticks since boot, from
    /// `/proc/<pid>/stat`) when the park was asked, so a reused pid never holds the wait.
    Pid { pid: u32, start: Option<u64> },
    /// The file exists and, with a marker, contains it.
    File {
        path: String,
        marker: Option<String>,
    },
}

impl ParkCondition {
    /// `lease:<resource>[:<amount>]`, `pid:<n>` or `file:<absolute path>[:<marker>]`.
    pub fn parse(text: &str) -> Result<Self, PublicError> {
        let bad = |message: String| PublicError::BadRequest { message };
        let (kind, rest) = text.split_once(':').ok_or_else(|| {
            bad(format!(
                "until: expected lease:<resource>[:amount], pid:<n> or file:<path>[:marker], got '{text}'"
            ))
        })?;
        match kind {
            "lease" => {
                let (resource, amount) = match rest.split_once(':') {
                    Some((resource, amount)) => (
                        resource,
                        amount
                            .parse::<u64>()
                            .ok()
                            .filter(|n| *n > 0)
                            .ok_or_else(|| {
                                bad(format!(
                                    "until: lease amount must be a positive integer, got '{amount}'"
                                ))
                            })?,
                    ),
                    None => (rest, 1),
                };
                if resource.is_empty() {
                    return Err(bad("until: lease needs a resource name".into()));
                }
                Ok(Self::Lease {
                    resource: resource.into(),
                    amount,
                })
            }
            "pid" => Ok(Self::Pid {
                pid: rest.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
                    bad(format!(
                        "until: pid must be a positive integer, got '{rest}'"
                    ))
                })?,
                start: None,
            }),
            "file" => {
                let (path, marker) = match rest.split_once(':') {
                    Some((path, marker)) if !marker.is_empty() => (path, Some(marker.to_owned())),
                    Some((path, _)) => (path, None),
                    None => (rest, None),
                };
                if !path.starts_with('/') {
                    return Err(bad(format!(
                        "until: file needs an absolute path, got '{path}'"
                    )));
                }
                Ok(Self::File {
                    path: path.into(),
                    marker,
                })
            }
            _ => Err(bad(format!(
                "until: unknown condition '{kind}'; expected lease, pid or file"
            ))),
        }
    }
}

impl std::fmt::Display for ParkCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lease { resource, amount } if *amount == 1 => write!(f, "lease:{resource}"),
            Self::Lease { resource, amount } => write!(f, "lease:{resource}:{amount}"),
            Self::Pid { pid, .. } => write!(f, "pid:{pid}"),
            Self::File {
                path,
                marker: Some(marker),
            } => write!(f, "file:{path}:{marker}"),
            Self::File { path, marker: None } => write!(f, "file:{path}"),
        }
    }
}

/// A run's park, kept on its attempt from `park` until the run is resumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParkRequest {
    pub condition: ParkCondition,
    pub timeout_seconds: Option<u64>,
    /// The section lease a `lease:` condition waits on.
    pub lease: Option<LeaseId>,
    pub phase: ParkPhase,
    pub requested_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_at: Option<String>,
    /// Why it woke, once it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub woke: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParkPhase {
    /// Asked for; the agent's turn has not ended yet.
    Requested,
    /// The agent's turn ended and the step's needs are released.
    Parked,
    /// The condition fired; the step's needs are queued to be taken again.
    Waking,
}

/// What the run's supervisor asks of the coordinator about its park.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParkOp {
    /// The park, if any, and whether the run holds or waits for a section lease.
    Read,
    /// The agent's turn ended: release the step's needs.
    Enter,
    /// The condition fired (`reason`): queue the step's needs; answered `woke` once they
    /// are held again, which ends the park.
    Wake { reason: String },
}

/// The coordinator's answer to a [`ParkOp`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParkView {
    pub park: Option<ParkRequest>,
    /// The park's lease is held.
    pub lease_held: bool,
    /// The run holds or waits for a section lease.
    pub leases_active: bool,
    /// The step's needs are held again and the park is over.
    pub woke: bool,
    pub cancelled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions_round_trip_through_their_text() {
        for text in [
            "lease:gate",
            "lease:gate:4",
            "pid:42",
            "file:/tmp/done",
            "file:/tmp/log:READY: yes",
        ] {
            assert_eq!(ParkCondition::parse(text).unwrap().to_string(), text);
        }
        for bad in [
            "gate",
            "lease:",
            "lease:gate:0",
            "pid:x",
            "file:relative",
            "bell:1",
        ] {
            assert!(ParkCondition::parse(bad).is_err(), "{bad}");
        }
    }
}

/// A live run's park as exposed by status, units and the dashboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Parked {
    pub run: crate::ids::RunId,
    pub until: String,
    pub phase: ParkPhase,
    pub since: String,
    pub waited_seconds: f64,
    pub timeout_seconds: u64,
}
