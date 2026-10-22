//! A step's page, a unit's page, the inbox, Questions and a thread on the neutral fixture
//! (`tests/neutral`, a field guide's `almanac`): a step's band carries its name, state, its
//! unit's stages with its own ringed and its actions; a unit's band draws its stage strip from
//! its recipe (two recipes and a unit of none); in Chromium, the question to the owner swells
//! first on Overview and is answered in place, Overview stands two to a row at 2560, a phone's
//! question is a whole-width card with 44px buttons, and no page scrolls sideways from 320 to
//! 3840px in Sluice Light or Dark. `SLUICE_STEP_SCREENS=<dir>` also saves each page at each width
//! and theme there.

use crate::board_fixture;
use crate::chrome;
use crate::neutral;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::json;

fn serve(f: &Fixture) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let router = f.router();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    (
        addr,
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    )
}
const FRAMES: &str = "new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))";
const FITS: &str = "document.documentElement.scrollWidth <= document.documentElement.clientWidth";

/// A completed run must retain its actual budget and peak even when project defaults change;
/// the shared failure summary must identify memory exhaustion on the owning unit too.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_run_budgets_and_resource_failures_fit_step_and_unit_pages() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let project = n.almanac;
    f.writer
        .write(sluice_store::RetrySafety::NonIdempotent, move |tx| {
            let limits = json!({"memory_bytes": 8_u64 << 30, "cpu_millis": 1500, "tasks": 256});
            let usage = json!({"memory_peak_bytes": 8_u64 << 30, "memory_working_peak_bytes": 2_u64 << 30, "oom_kills": 1,
                "pids_limit_hits": 0, "cpu_throttled_usec": 1200});
            let error = json!({"error": "resource_exhausted", "resource": "memory",
                "limit": 8_u64 << 30, "peak": 8_u64 << 30,
                "message": "Memory limit exceeded: 8589934592 byte budget; payload stopped"});
            tx.sql().execute(
                "UPDATE projects SET limits=?2 WHERE project_id=?1",
                (project.to_string(), json!({"memory":"512MiB","cpus":0.25,"tasks":64}).to_string()),
            )?;
            let changed = tx.sql().execute(
                "UPDATE attempts SET limits=?2,request=json_set(request,'$.limits',json(?2),'$.resource_usage',json(?3))
                 WHERE project_id=?1 AND step_id='a-4-review'",
                (project.to_string(), limits.to_string(), usage.to_string()),
            )?;
            assert_eq!(changed, 1, "the fixture has one failed review attempt");
            tx.sql().execute(
                "UPDATE runs SET result=json_set(result,'$.error',json(?2))
                 WHERE project_id=?1 AND step_id='a-4-review'",
                (project.to_string(), error.to_string()),
            )?;
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='a-4-review'",
                (project.to_string(), error.to_string()),
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    let (addr, server) = serve(&f);
    tokio::task::spawn_blocking(move || {
        let base = format!("http://{addr}/projects/id/{project}");
        let pages = [
            ("step-runs", format!("{base}/steps/a-4-review")),
            ("unit", format!("{base}/units/a-4")),
        ];
        let screens = std::env::var_os("SLUICE_BUDGET_SCREENS").map(std::path::PathBuf::from);
        let mut browser = Chrome::open(&pages[0].1).unwrap();
        for width in [390, 1440, 2560] {
            for theme in ["light", "dark"] {
                for (name, url) in &pages {
                    browser.viewport(width, theme).unwrap();
                    browser.navigate(url).unwrap();
                    browser.wait("document.readyState === 'complete'").unwrap();
                    browser.viewport(width, theme).unwrap();
                    let stock = if theme == "light" { "paper" } else { "carbon" };
                    browser.eval(&format!("document.querySelector('input[name=theme][value={stock}]').click()")).unwrap();
                    browser.wait(&format!("document.documentElement.dataset.theme === '{stock}' && getComputedStyle(document.documentElement).colorScheme === '{theme}'")).unwrap();
                    browser.eval("document.fonts.ready").unwrap();
                    if *name == "step-runs" {
                        browser
                            .wait("document.querySelector('[role=tab][data-tab=runs]') && customElements.get('sluice-tabs')")
                            .unwrap();
                        browser
                            .eval("document.querySelector('[role=tab][data-tab=runs]').click()")
                            .unwrap();
                        browser
                            .wait("document.querySelector('#tp-runs')?.checkVisibility()")
                            .unwrap();
                        let facts = browser.eval(r#"(() => {
                            const rows = [...document.querySelectorAll('#tp-runs .a-facts > div')];
                            return ['Budget', 'Usage'].map(label => {
                                const row = rows.find(r => r.querySelector('dt')?.textContent === label);
                                const value = row?.querySelector('dd');
                                return value?.checkVisibility() ? value.textContent.trim() : null;
                            });
                        })()"#).unwrap();
                        assert_eq!(facts, json!(["8192 MiB memory cap · 1.5 CPUs · 256 tasks", "peak working 2 GiB (incl. cache 8 GiB)"]), "{width} {theme}");
                        assert_eq!(browser.eval("[...document.querySelectorAll('#tp-runs .a-facts dd')].every(e => e.scrollWidth <= e.clientWidth + 1)").unwrap(), json!(true), "run facts clipped at {width} {theme}");
                    } else {
                        browser
                            .wait("document.querySelector('#us-a-4-review')")
                            .unwrap();
                        assert_eq!(browser.eval("document.querySelector('#us-a-4-review').closest('.u-step').querySelector('.u-why').innerText").unwrap(), json!("Stopped: it was killed at its memory cap."), "{width} {theme}");
                    }
                    browser.eval(FRAMES).unwrap();
                    assert_eq!(browser.eval(FITS).unwrap(), json!(true), "{name} at {width} {theme} scrolls sideways");
                    let alignment = browser.eval(r#"(() => {
                        const head = document.querySelector('.page-top');
                        const rect = head.getBoundingClientRect();
                        const bar = document.querySelector('header.top .top-in');
                        const area = document.querySelector('.mainarea').getBoundingClientRect();
                        const navLeft = bar.getBoundingClientRect().left + parseFloat(getComputedStyle(bar).paddingLeft);
                        const contentLeft = rect.left + parseFloat(getComputedStyle(head).paddingLeft);
                        return [Math.abs((rect.left - area.left) - (area.right - rect.right)) <= 1,
                                Math.abs(contentLeft - navLeft) <= 1];
                    })()"#).unwrap();
                    assert_eq!(alignment, json!([true, true]), "{name} column at {width} {theme}");
                    assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
                    assert_eq!(browser.eval("getComputedStyle(document.documentElement).colorScheme").unwrap(), json!(theme));
                    if let Some(dir) = &screens {
                        std::fs::create_dir_all(dir).unwrap();
                        browser.screenshot(&dir.join(format!("budget-{name}-{width}-{theme}.png"))).unwrap();
                    }
                }
            }
        }
    })
    .await
    .unwrap();
    server.abort();
}
