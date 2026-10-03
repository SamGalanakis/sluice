use sluice_model::ids::ProjectId;
use sluice_store::Writer;
use sluice_web::views::PageState;
pub async fn seed(_writer: &Writer, _id: ProjectId) {}
pub async fn configure(state: &mut PageState, _writer: &Writer, _id: ProjectId) {
    state.log = true;
}

pub fn layer(router: axum::Router) -> axum::Router {
    router
}
