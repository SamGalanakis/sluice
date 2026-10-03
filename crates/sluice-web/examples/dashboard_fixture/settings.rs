use sluice_model::{error::PublicError, ids::ProjectId};
use sluice_store::{RetrySafety, Writer, projects};
use sluice_web::views::PageState;
use std::sync::Arc;
struct FixtureResources;
impl sluice_model::plan::SignatureProvider for FixtureResources {
    fn signature(&self, name: &str) -> Option<sluice_model::plan::FnSignature> {
        (name == "fixture.capacity").then(|| sluice_model::plan::FnSignature {
            outputs: [("capacity".into(), sluice_model::types::Type::Int)]
                .into_iter()
                .collect(),
            ..Default::default()
        })
    }
}
impl projects::ResourceSettings for FixtureResources {
    fn set_resources(
        &self,
        tx: &mut sluice_store::WriteTransaction<'_>,
        project: ProjectId,
        patch: &serde_json::Value,
    ) -> sluice_store::Result<bool> {
        sluice_store::resources::patch_resources(tx, project, patch, self)
    }
}

pub async fn seed(_writer: &Writer, _id: ProjectId) {}
pub async fn configure(state: &mut PageState, writer: &Writer, id: ProjectId) {
    writer.write(RetrySafety::NonIdempotent, move |tx| {
        projects::project_update(tx, &sluice_model::ids::ProjectSelector::Id(id), projects::UpdateProject {
            resources:Some(serde_json::json!({"workers":3,"dynamic":{"capacity_fn":"fixture.capacity"}})),
            author:"owner".into(), ..Default::default()
        }, &FixtureResources)?;
        sluice_store::resources::observe_capacity(tx, id, "dynamic", 1, Err(PublicError::FnFailure {message:"Capacity service is unavailable. Last good capacity retained.".into()}))?;
        Ok(())
    }).await.unwrap();
    let guard = Arc::new(projects::StoredWorkOnly);
    let commands = Arc::new(sluice_web::settings::StoreCommands {
        writer: writer.clone(),
        resources: Arc::new(FixtureResources),
        deletion_guard: guard.clone(),
    });
    let settings =
        sluice_web::settings::SettingsState::new(state.dashboard.clone(), commands, guard);
    state.settings = Some(settings);
}

pub fn layer(router: axum::Router) -> axum::Router {
    router
}
