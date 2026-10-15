use crate::{board_fixture::Fixture, chrome::Chrome};
use serde_json::json;

async fn check_pages(family: &str) {
    let f = Fixture::new().await;
    let paths = match family {
        "home_day" => vec!["/".into(), "/day".into()],
        "plan" => vec![format!("/projects/id/{}", f.id)],
        "step" => vec![format!("/projects/id/{}/steps/alpha-review", f.id)],
        "unit" => vec![
            format!("/projects/id/{}/units/alpha", f.id),
            format!("/projects/id/{}", f.plain),
        ],
        "messages" => vec![
            "/inbox".into(),
            format!("/projects/id/{}/thread?thread=alpha-review", f.id),
            "/history".into(),
        ],
        "settings" => vec![format!("/projects/id/{}/settings", f.id)],
        "catalog_docs" => vec!["/fns".into(), "/docs".into()],
        "log_stats" => vec!["/log".into(), format!("/projects/id/{}/stats", f.id)],
        "gallery" => vec!["/_ui".into()],
        _ => panic!("unknown browser smoke family: {family}"),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    assert_ne!(addr.port(), 3065);
    let app = f.router();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open("about:blank").unwrap();
        browser.send("Emulation.setDeviceMetricsOverride", json!({"width":390,"height":1000,"deviceScaleFactor":1,"mobile":false})).unwrap();
        for path in paths {
            browser.navigate(&format!("http://{addr}{path}")).unwrap();
            browser.wait("document.querySelector('h1')?.checkVisibility() && !!customElements.get('sluice-menu')").unwrap();
            assert_eq!(browser.eval("!!document.querySelector('#top-nav') && document.querySelector('h1').textContent.trim().length > 0").unwrap(), true, "{path}");
            assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]), "{path}");
            // The gallery draws one component in several states, so only real pages
            // promise each id once (a patch targets the first match and misses the rest).
            if path != "/_ui" {
                assert_eq!(browser.eval("[...document.querySelectorAll('[id]')].map(e => e.id).filter((id, i, all) => all.indexOf(id) !== i)").unwrap(), json!([]), "{path} repeats an id");
            }
            // A menu keeps its panel: a <details> inside a <p> is closed out of it by the parser.
            assert_eq!(browser.eval("document.querySelectorAll('sluice-menu:empty').length").unwrap(), json!(0), "{path} has a menu the parser emptied");
            // An idle page sends nothing. Streams stay open and never finish, so every
            // fetch that completes while the page sits untouched is a request a patch or
            // an init sent on its own (the settings preview once re-fired itself on every
            // answer, hundreds of times a second).
            let idle = browser.eval("(async () => { await new Promise(r => setTimeout(r, 100)); performance.clearResourceTimings(); await new Promise(r => setTimeout(r, 400)); return performance.getEntriesByType('resource').filter(e => e.initiatorType === 'fetch').map(e => e.name); })()").unwrap();
            assert_eq!(idle, json!([]), "{path} sent requests while idle");
        }
    }).await.unwrap();
    server.abort();
    let _ = server.await;
}

async fn smoke(family: &str) {
    let binary = Chrome::binary().unwrap();
    let shell = if std::env::var_os("SLUICE_CHROME").is_some() {
        binary.clone()
    } else {
        binary
            .parent()
            .and_then(|p| p.parent())
            .and_then(|revision| {
                let suffix = revision.file_name()?.to_str()?.strip_prefix("chromium-")?;
                let shell = revision
                    .parent()?
                    .join(format!("chromium_headless_shell-{suffix}"))
                    .join("chrome-headless-shell-linux64/chrome-headless-shell");
                shell.is_file().then_some(shell)
            })
            .unwrap_or(binary)
    };
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "browser_smoke::smoke_child",
            "--ignored",
            "--nocapture",
        ])
        .env("SLUICE_CHROME", shell)
        .env("SLUICE_BROWSER_SMOKE_FAMILY", family)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "isolated browser smoke child entry point"]
fn smoke_child() {
    let family =
        std::env::var("SLUICE_BROWSER_SMOKE_FAMILY").expect("a parent supplies the page family");
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(check_pages(&family));
}
#[tokio::test(flavor = "multi_thread")]
async fn browser_home_and_day_smoke() {
    smoke("home_day").await;
}
#[tokio::test(flavor = "multi_thread")]
async fn browser_plan_smoke() {
    smoke("plan").await;
}
#[tokio::test(flavor = "multi_thread")]
async fn browser_step_smoke() {
    smoke("step").await;
}
#[tokio::test(flavor = "multi_thread")]
async fn browser_unit_smoke() {
    smoke("unit").await;
}
#[tokio::test(flavor = "multi_thread")]
async fn browser_messages_smoke() {
    smoke("messages").await;
}
#[tokio::test(flavor = "multi_thread")]
async fn browser_settings_smoke() {
    smoke("settings").await;
}
#[tokio::test(flavor = "multi_thread")]
async fn browser_catalog_and_docs_smoke() {
    smoke("catalog_docs").await;
}
#[tokio::test(flavor = "multi_thread")]
async fn browser_log_and_stats_smoke() {
    smoke("log_stats").await;
}
#[tokio::test(flavor = "multi_thread")]
async fn browser_gallery_smoke() {
    smoke("gallery").await;
}
