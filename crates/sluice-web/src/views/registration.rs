use super::{DashboardState, ProjectId};
use axum::{Router, routing::get};
#[derive(Clone)]
pub struct PageState {
    pub dashboard: DashboardState,
    pub messages: Option<super::inbox::MessageState>,
    pub settings: Option<crate::settings::SettingsState>,
    pub log: bool,
}
impl PageState {
    pub fn new(dashboard: DashboardState) -> Self {
        Self {
            dashboard,
            messages: None,
            settings: None,
            log: false,
        }
    }
}
pub struct Asset {
    pub names: &'static [&'static str],
    pub media_type: &'static str,
    pub bytes: &'static [u8],
}
pub struct NavEntry {
    pub key: &'static str,
    pub href: String,
    pub label: &'static str,
    pub order: u8,
}
impl NavEntry {
    pub fn new(key: &'static str, href: String, label: &'static str, order: u8) -> Self {
        Self {
            key,
            href,
            label,
            order,
        }
    }
}
pub struct PageRegistration {
    pub routes: fn(&PageState) -> Router,
    pub nav: fn(Option<ProjectId>) -> Vec<NavEntry>,
    pub assets: &'static [Asset],
}
pub fn page_router(state: PageState) -> Router {
    super::PAGES.iter().fold(
        Router::new()
            .route("/static/{name}", get(super::static_asset))
            .route("/settings", axum::routing::post(super::display_preferences)),
        |router, page| router.merge(((page)().routes)(&state)),
    )
}
