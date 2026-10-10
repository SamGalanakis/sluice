//! Test-only signatures of lane B's pinned API. These are never linked into sluice.
use rusqlite::Connection;
use sluice_model::{error::PublicError, ids::*, plan_rows::*};
#[derive(Debug)]
pub struct StoreError(pub PublicError);
impl From<PublicError> for StoreError {
    fn from(error: PublicError) -> Self {
        Self(error)
    }
}
pub type Result<T> = std::result::Result<T, StoreError>;
pub struct ReadPool;
impl ReadPool {
    pub async fn snapshot<T, F>(&self, _read: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        unreachable!("signature checking does not open storage")
    }
}
pub mod messages {
    use super::*;
    pub fn resolve_project(_: &Connection, _: &ProjectSelector) -> Result<ProjectId> {
        unreachable!()
    }
}
pub mod projects {
    use super::*;
    pub struct Project {
        pub name: ProjectName,
    }
    pub fn resolve(_: &Connection, _: &ProjectSelector) -> Result<Project> {
        unreachable!()
    }
}
pub mod plans {
    use super::*;
    pub fn plan_header(_: &Connection, _: ProjectId) -> Result<PlanHeader> {
        unreachable!()
    }
    pub fn read_steps(
        _: &Connection,
        _: ProjectId,
        _: &RowSelection,
        _: StepProjection,
    ) -> Result<StepRows> {
        unreachable!()
    }
    pub fn read_references(
        _: &Connection,
        _: ProjectId,
        _: &ReferenceSelection,
    ) -> Result<ReferenceRows> {
        unreachable!()
    }
    pub fn read_graph(_: &Connection, _: ProjectId, _: &RowSelection) -> Result<GraphRows> {
        unreachable!()
    }
    pub fn history(
        _: &Connection,
        _: ProjectId,
        _: Option<Revision>,
        _: Option<RecordSeq>,
        _: u32,
    ) -> Result<(Vec<HistoryRecord>, Option<RecordSeq>)> {
        unreachable!()
    }
}
