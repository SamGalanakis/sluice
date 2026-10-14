//! Scratch dashboard with page-owned fixture data and no runner, the neutral projects
//! (`almanac`, `chores`) every page must render, and `atlas`, a plan of 250 units.
use sluice_model::ids::ProjectId;
use sluice_store::{ReadPool, Writer};
use sluice_web::views::{DashboardState, PageState, page_router};
use std::sync::Arc;
#[path = "dashboard_fixture/mod.rs"]
mod dashboard_fixture;
/// A real (trimmed, masked) Claude transcript, laid out as its engine leaves it.
#[path = "../../sluice-agents/tests/fixtures/activity/lay.rs"]
mod lay;
/// The neutral projects (`almanac`, `chores`) every page must render (tests/neutral).
#[path = "../tests/web/neutral/mod.rs"]
mod neutral;
/// Plans are put through the store's plan edit, as the tests' are.
#[path = "../tests/web/seed/mod.rs"]
mod seed;
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
    println!("ATLAS {}", neutral::seed_big(&writer, home.path()).await);
    // almanac's long survey reads as an agent at work: its running run's transcript, turn by turn
    let run: String = writer
        .write(sluice_store::RetrySafety::NonIdempotent, move |tx| {
            let ids: String = tx.sql().query_row(
                "SELECT run_ids FROM steps WHERE project_id=?1 AND step_id='survey'",
                [n.almanac.to_string()],
                |r| r.get(0),
            )?;
            let ids: Vec<String> = serde_json::from_str(&ids).unwrap();
            Ok(ids.last().cloned().unwrap())
        })
        .await
        .unwrap();
    let claude = home.path().join("claude-home");
    // its records moved to the last hour, inside the run (they were written on 2026-10-08)
    let laid = lay::lay(home.path(), &claude, "claude", &run);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let hour = &sluice_web::views::rfc3339(now - 3600)[..14];
    let text: String = laid
        .lines
        .iter()
        .map(|l| format!("{}\n", l.replace("2026-10-08T10:", hour)))
        .collect();
    std::fs::write(&laid.transcript, text).unwrap();
    let mut state = DashboardState::new(
        ReadPool::open(home.path(), 2).unwrap(),
        Arc::new(home::FixtureCatalog),
    );
    state.claude_home = Some(claude);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    println!("URL http://{}", listener.local_addr().unwrap());
    let missing = state.clone();
    let mut state = PageState::new(state);
    configure(&mut state, &writer, id).await;
    // a page that is not there, drawn as `sluice serve` draws it (its HTTP layer's missing page)
    let router = layers(page_router(state))
        .merge(home::extra_router(writer, id))
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let missing = missing.clone();
                async move {
                    let path = request.uri().path().to_owned();
                    let headers = request.headers().clone();
                    let wants =
                        sluice_web::views::missing::wants_page(request.method(), &headers, &path);
                    let response = next.run(request).await;
                    let drawn = response
                        .headers()
                        .get(axum::http::header::CONTENT_TYPE)
                        .is_some_and(|v| v.as_bytes().starts_with(b"text/html"));
                    if !wants || drawn || !response.status().is_client_error() {
                        return response;
                    }
                    let status = response.status();
                    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                        .await
                        .unwrap_or_default();
                    let message = serde_json::from_slice::<serde_json::Value>(&bytes)
                        .ok()
                        .and_then(|v| v["message"].as_str().map(str::to_owned))
                        .unwrap_or_else(|| String::from_utf8_lossy(&bytes).trim().to_owned());
                    sluice_web::views::missing::page(&missing, &path, status, &message, &headers)
                        .await
                        .unwrap_or_else(|| {
                            axum::response::IntoResponse::into_response((status, message))
                        })
                }
            },
        ));
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .unwrap();
}
