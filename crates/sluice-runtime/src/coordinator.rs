use sluice_model::{
    RuntimeApi,
    commands::{CommandReply, CommandRequest},
    error::PublicError,
    events::{ChangeBatch, ChangeCursor},
};

/// Local command client contract, pending the phase 3 socket implementation.
pub struct CoordinatorClient;
impl RuntimeApi for CoordinatorClient {
    async fn command(&self, _request: CommandRequest) -> Result<CommandReply, PublicError> {
        Err(PublicError::not_implemented("coordinator command"))
    }
    async fn changes(&self, _cursor: ChangeCursor) -> Result<ChangeBatch, PublicError> {
        Err(PublicError::not_implemented("coordinator changes"))
    }
}
