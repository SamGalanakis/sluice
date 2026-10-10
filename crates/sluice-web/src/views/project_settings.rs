use super::*;
pub fn registration() -> PageRegistration {
    PageRegistration {
        routes: |state| {
            state
                .settings
                .clone()
                .map(crate::settings::router)
                .unwrap_or_default()
        },
        nav: |_| vec![],
        assets: &[],
    }
}
