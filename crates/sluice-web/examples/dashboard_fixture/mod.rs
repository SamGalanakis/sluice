use super::*;
macro_rules! register_fixture_pages {
    ($($module:ident),* $(,)?) => {
        $(pub mod $module;)*
        pub async fn seed(writer: &Writer, id: ProjectId) { $($module::seed(writer, id).await;)* }
        pub fn layers(router: axum::Router) -> axum::Router { $(let router = $module::layer(router);)* router }
        pub async fn configure(state: &mut PageState, writer: &Writer, id: ProjectId) { $($module::configure(state, writer, id).await;)* }
    };
}
register_fixture_pages! { home, messages, board, settings, log }
