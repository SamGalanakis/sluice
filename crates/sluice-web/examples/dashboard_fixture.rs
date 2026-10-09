//! Scratch dashboard with page-owned fixture data and no runner, and the neutral projects
//! (`almanac`, `chores`) every page must render.
use sluice_model::ids::ProjectId;
use sluice_store::{ReadPool, Writer};
use sluice_web::views::{DashboardState, PageState, page_router};
use std::sync::Arc;
#[path = "dashboard_fixture/mod.rs"]
mod dashboard_fixture;
/// The neutral projects (`almanac`, `chores`) every page must render (tests/neutral).
#[path = "../tests/neutral/mod.rs"]
mod neutral;
use dashboard_fixture::{configure, home, layers, seed};
#[tokio::main]
async fn main() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let id = home::create(&writer).await;
    seed(&writer, id).await;
    let n = neutral::seed(&writer, home.path()).await;
    println!("ALMANAC {}", n.almanac);
    println!("CHORES {}", n.chores);
    let state = DashboardState::new(
        ReadPool::open(home.path(), 2).unwrap(),
        Arc::new(home::FixtureCatalog),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    println!("URL http://{}", listener.local_addr().unwrap());
    let mut state = PageState::new(state);
    configure(&mut state, &writer, id).await;
    let router = layers(page_router(state)).merge(home::extra_router(writer, id));
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .unwrap();
}
