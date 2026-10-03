//! Durable ordered delivery. An uncertain input is never automatically replayed.
use crate::engines::{DeliveryOutcome, InputId};
use serde::{Deserialize, Serialize};
use sluice_model::ids::MessageId;
use std::io;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Queued,
    Offered,
    Acknowledged,
    NotAccepted,
    Uncertain,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub id: InputId,
    pub text: String,
    pub state: DeliveryState,
    pub tries: u32,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryLedger {
    pub entries: Vec<Delivery>,
}
impl DeliveryLedger {
    pub fn enqueue(&mut self, id: InputId, text: String) -> io::Result<()> {
        if let Some(old) = self.entries.iter().find(|entry| entry.id == id) {
            if old.text != text {
                return Err(io::Error::other("delivery id reused with different text"));
            }
            return Ok(());
        }
        if self.entries.len() >= 16_384 || text.len() > 1024 * 1024 {
            return Err(io::Error::other("delivery ledger bound exceeded"));
        }
        self.entries.push(Delivery {
            id,
            text,
            state: DeliveryState::Queued,
            tries: 0,
        });
        Ok(())
    }
    pub fn next(&self) -> Option<&Delivery> {
        self.entries.iter().find(|entry| {
            matches!(
                entry.state,
                DeliveryState::Queued | DeliveryState::NotAccepted
            )
        })
    }
    pub fn offer(&mut self, id: &InputId) -> io::Result<()> {
        let entry = self.entry_mut(id)?;
        entry.tries += 1;
        entry.state = DeliveryState::Offered;
        Ok(())
    }
    pub fn outcome(&mut self, id: &InputId, outcome: DeliveryOutcome) -> io::Result<()> {
        let entry = self.entry_mut(id)?;
        if entry.state == DeliveryState::Acknowledged {
            return Ok(());
        }
        if !matches!(
            entry.state,
            DeliveryState::Offered | DeliveryState::Uncertain
        ) {
            return Err(io::Error::other("acknowledgement without an offer"));
        }
        entry.state = match outcome {
            DeliveryOutcome::Acknowledged => DeliveryState::Acknowledged,
            DeliveryOutcome::NotAccepted => DeliveryState::NotAccepted,
            DeliveryOutcome::Pending => DeliveryState::Offered,
            DeliveryOutcome::Uncertain => DeliveryState::Uncertain,
        };
        Ok(())
    }
    pub fn all_acknowledged(&self) -> bool {
        self.entries
            .iter()
            .all(|entry| entry.state == DeliveryState::Acknowledged)
    }
    pub fn uncertain(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.state == DeliveryState::Uncertain)
    }
    pub fn recover(&mut self) {
        for entry in &mut self.entries {
            if entry.state == DeliveryState::Offered {
                entry.state = DeliveryState::Uncertain;
            }
        }
    }
    pub fn acknowledged_messages(&self) -> Vec<MessageId> {
        self.entries
            .iter()
            .filter_map(|e| match e.id {
                InputId::Message { id } if e.state == DeliveryState::Acknowledged => Some(id),
                _ => None,
            })
            .collect()
    }
    fn entry_mut(&mut self, id: &InputId) -> io::Result<&mut Delivery> {
        self.entries
            .iter_mut()
            .find(|e| &e.id == id)
            .ok_or_else(|| io::Error::other("unknown delivery id"))
    }
}
