//! A project's Stats page on the neutral fixture: each recipe's stages in its order with their
//! median, p90 and longest; how each stage's runs ended, its retries and its failures by kind;
//! units finished and started; the slowest runs; the window switch; a project with no runs.
//! And the usual time is gone from every other page: no plan, step, unit, home, Day, drawer or
//! inbox says "usual". In Chromium the page never scrolls sideways at 390, 1440 and 2560, and
//! its window switch works.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
mod neutral;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use board_fixture::Fixture;
use chrome::Chrome;
use futures_util::StreamExt;
use serde_json::json;
use sluice_model::{
    error::PublicError,
    ids::{AttemptId, ProjectId, RunId},
};
use sluice_store::RetrySafety;
use tower::ServiceExt;

async fn get(router: &Router, path: &str) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(
            Request::get(path)
                .header("cookie", "sluice_zone=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}
/// The text a reader sees: no tags, scripts or styles, a screen reader's words kept.
fn visible(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let close = if rest.starts_with("<script") {
            rest.find("</script>").map(|e| e + 9)
        } else if rest.starts_with("<style") {
            rest.find("</style>").map(|e| e + 8)
        } else {
            rest.find('>').map(|e| e + 1)
        };
        let Some(end) = close else { break };
        out.push(' ');
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}
/// A group's table in a section (`st-dur-h` or `st-out-h`), by its head's name.
fn group<'a>(page: &'a str, section: &str, name: &str) -> &'a str {
    let from = page
        .find(&format!("id=\"{section}\""))
        .unwrap_or_else(|| panic!("no {section}"));
    let rest = &page[from..];
    let head = rest
        .find(&format!("\">{name}<span class=\"st-gn\">"))
        .unwrap_or_else(|| panic!("no group {name} in {section}: {rest}"));
    let rest = &rest[head..];
    &rest[..rest.find("</table>").unwrap()]
}
/// A stage's row in a group's table: what each cell shows (a duration's visible part).
fn cells(table: &str, stage: &str) -> Vec<String> {
    let from = table
        .find(&format!("<th scope=\"row\">{stage}</th>"))
        .unwrap_or_else(|| panic!("no {stage} in {table}"));
    let row = &table[from..];
    let row = &row[..row.find("</tr>").unwrap()];
    row.split("<td")
        .skip(1)
        .map(|cell| {
            let cell = &cell[cell.find('>').unwrap() + 1..];
            let cell = &cell[..cell.find("</td>").unwrap_or(cell.len())];
            // a duration says itself twice: its visible text, and its words for a reader
            let shown = match cell.find("<span class=\"vh\">") {
                Some(at) => &cell[..at],
                None => cell,
            };
            visible(shown)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .replace("&lt;", "<")
        })
        .collect()
}
/// A run of `step` that started `ago` seconds ago and took `took`, ended with `result`.
async fn run(
    f: &Fixture,
    project: ProjectId,
    step: &'static str,
    ago: u64,
    took: u64,
    result: serde_json::Value,
) {
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let (attempt, run) = (AttemptId::new(), RunId::new());
            let start = neutral::ago(ago);
            let end = neutral::ago(ago - took);
            tx.sql().execute(
                "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at,finished_at) VALUES (?1,?2,?3,1,1,'terminal','{}','hash',?4,?5)",
                (attempt.to_string(), project.to_string(), step, &start, &end),
            )?;
            tx.sql().execute(
                "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at,finished_at,result) VALUES (?1,?2,?3,?4,1,1,?5,?5,?6,?7)",
                (run.to_string(), project.to_string(), attempt.to_string(), step, &start, &end, result.to_string()),
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn stats_measures_each_recipes_stages_its_outcomes_throughput_and_slowest_runs() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    // a scan failed before s-2's success: the success is a retry
    let broken = PublicError::FnFailure {
        message: "The archive was locked.".into(),
    };
    run(
        &f,
        n.almanac,
        "s-2-scan",
        2 * 3600,
        60,
        json!({"status": "failed", "error": broken}),
    )
    .await;
    let router = f.router();
    let (status, page) = get(&router, &format!("/projects/id/{}/stats", n.almanac)).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    // the tab after Day, current; the head says what the window holds
    let tabs = page.split("<nav class=\"links\"").nth(1).unwrap();
    let tabs = &tabs[..tabs.find("</nav>").unwrap()];
    let day = tabs.find(">Day</a>").unwrap();
    let stats = tabs
        .find(&format!(
            "<a href=\"/projects/id/{}/stats\" aria-current=\"page\">Stats</a>",
            n.almanac
        ))
        .unwrap_or_else(|| panic!("{tabs}"));
    assert!(
        day < stats && stats < tabs.find(">Messages</a>").unwrap(),
        "{tabs}"
    );
    assert!(
        page.contains(
            "<p class=\"page-note\">18 runs ended and 5 units finished in the last 7 days.</p>"
        ),
        "{page}"
    );
    // the window switch: 7 days by default
    assert!(
        page.contains(&format!(
            "<a href=\"/projects/id/{}/stats?window=7d\" aria-current=\"page\">7 days</a>",
            n.almanac
        )),
        "{page}"
    );

    // durations: article's stages in its order, median, p90, longest of the succeeded runs
    let article = group(&page, "st-dur-h", "article");
    let order: Vec<usize> = neutral::ARTICLE
        .iter()
        .map(|s| article.find(&format!(">{s}</th>")).unwrap())
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{article}");
    // drafts took 25m, 30m, 35m and 28m (a-5's was cancelled, a-6's and a-7's still run)
    assert_eq!(cells(article, "draft")[..4], ["4", "29m", "35m", "35m"]);
    assert_eq!(cells(article, "review")[..4], ["3", "40m", "50m", "50m"]);
    assert_eq!(cells(article, "publish")[..4], ["3", "2m", "2m", "2m"]);
    assert!(
        article.contains("<span class=\"st-med\" style=\"--w:"),
        "{article}"
    );
    assert!(
        page.contains("\">article<span class=\"st-gn\">5 units · 12 runs</span></h3>"),
        "{page}"
    );
    let scan = group(&page, "st-dur-h", "scan");
    assert_eq!(cells(scan, "scan")[..4], ["2", "5m", "6m", "6m"]);
    // the units of no recipe and the loose steps: one group by step
    let rest = group(&page, "st-dur-h", "Without a recipe");
    assert_eq!(cells(rest, "birds")[..4], ["1", "14m", "14m", "14m"]);
    assert_eq!(cells(rest, "survey")[..4], ["1", "1d 6h", "1d 6h", "1d 6h"]);

    // outcomes: succeeded, failed, cancelled, retried, the failure rate and the kinds
    let article = group(&page, "st-out-h", "article");
    assert_eq!(cells(article, "draft"), ["4", "0", "1", "0", "0%", ""]);
    assert_eq!(
        cells(article, "review"),
        ["3", "1", "0", "0", "25%", "fn failure 1"]
    );
    let scan = group(&page, "st-out-h", "scan");
    assert_eq!(
        cells(scan, "scan"),
        ["2", "2", "0", "1", "50%", "fn failure 2"]
    );

    // throughput: a-1, a-2, a-3, s-1 and s-2 finished; thirteen units started
    assert!(
        page.contains("<p class=\"st-said\">5 units finished and 13 started in the last 7 days. The most in a day: "),
        "{page}"
    );
    assert!(page.contains("<rect class=\"st-col st-most\""), "{page}");
    assert!(page.contains("<caption>Units finished and started each day</caption>"));

    // the slowest: the survey's 30h run first, then a-3's 50m review, by title and stage
    let slow = page.split("<ol class=\"st-slow\">").nth(1).unwrap();
    let slow = &slow[..slow.find("</ol>").unwrap()];
    let rows: Vec<&str> = slow.split("<li class=\"st-run\">").skip(1).collect();
    assert_eq!(rows.len(), 10, "{slow}");
    assert!(
        rows[0].contains("<span aria-hidden=\"true\">1d 6h</span>"),
        "{}",
        rows[0]
    );
    assert!(
        rows[0].contains("<b>Survey the archive&#39;s photographs against the checklist</b>"),
        "{}",
        rows[0]
    );
    assert!(
        rows[1].contains(
            "<b>Plovers: the spring guide&#39;s entries</b> <span class=\"st-stage\">review</span>"
        ),
        "{}",
        rows[1]
    );
    assert!(
        rows[1].contains(&format!(
            "href=\"/projects/id/{}/steps/a-3-review?tab=runs\"",
            n.almanac
        )),
        "{}",
        rows[1]
    );
    // its ids behind its ⋯, never in its words
    let line = &rows[1][..rows[1].find("<sluice-menu").unwrap()];
    assert!(rows[1].contains("<dt>Step</dt>"), "{}", rows[1]);
    assert!(!visible(line).contains("a-3"), "{line}");
}

#[tokio::test]
async fn the_window_switch_picks_the_runs_it_counts_and_a_project_with_no_runs_says_so() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let router = f.router();
    let base = format!("/projects/id/{}/stats", n.almanac);
    // the last 24 hours: the survey's run ended days ago, so it is not counted, nor started
    let (status, day) = get(&router, &format!("{base}?window=24h")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        day.contains(&format!(
            "<a href=\"{base}?window=24h\" aria-current=\"page\">24 hours</a>"
        )),
        "{day}"
    );
    assert!(
        day.contains(
            "<p class=\"page-note\">16 runs ended and 5 units finished in the last 24 hours.</p>"
        ),
        "{day}"
    );
    assert!(!group(&day, "st-dur-h", "Without a recipe").contains(">survey</th>"));
    assert!(
        day.contains("5 units finished and 12 started in the last 24 hours."),
        "{day}"
    );
    assert!(day.contains("<caption>Units finished and started each hour</caption>"));
    // every run, and an unknown window reads as 7 days
    let (_, all) = get(&router, &format!("{base}?window=all")).await;
    assert!(all.contains("ended and 5 units finished since "), "{all}");
    assert!(group(&all, "st-dur-h", "Without a recipe").contains(">survey</th>"));
    let (_, other) = get(&router, &format!("{base}?window=year")).await;
    assert!(
        other.contains(&format!(
            "<a href=\"{base}?window=7d\" aria-current=\"page\">7 days</a>"
        )),
        "{other}"
    );
    // its stream carries its window
    assert!(
        day.contains(&format!("@get('{base}/stream?window=24h'")),
        "{day}"
    );
    // the loose steps of a tiny project, one group by step
    let (_, chores) = get(&router, &format!("/projects/id/{}/stats", n.chores)).await;
    let rest = group(&chores, "st-dur-h", "Without a recipe");
    assert_eq!(cells(rest, "water-plants")[..4], ["1", "5m", "5m", "5m"]);
    assert!(chores.contains("1 unit finished and 2 started"), "{chores}");
    // a project with no runs: each section says so
    let (status, plain) = get(&router, &format!("/projects/id/{}/stats", f.plain)).await;
    assert_eq!(status, StatusCode::OK, "{plain}");
    assert!(
        plain.contains("<p class=\"page-note\">No run yet.</p>"),
        "{plain}"
    );
    assert_eq!(
        plain
            .matches("<p class=\"st-empty\">No run yet.</p>")
            .count(),
        3,
        "{plain}"
    );
    assert!(
        plain.contains("0 units finished and 0 started in the last 7 days."),
        "{plain}"
    );
    // an unknown project is a missing page
    let (status, _) = get(&router, &format!("/projects/id/{}/stats", ProjectId::new())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // only a project has stats: the pages across the projects do not link them
    let (_, every) = get(&router, "/day").await;
    assert!(!every.contains(">Stats</a>"), "{every}");
}

#[tokio::test]
async fn no_default_view_says_usual() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let router = f.router();
    let p = format!("/projects/id/{}", n.almanac);
    // a-6 has run 70 minutes where its stage's done runs took 25 to 35: no page compares them
    for path in [
        p.clone(),
        format!("{p}?view=plan"),
        format!("{p}/steps/a-6-draft"),
        format!("{p}/steps/a-3-review"),
        format!("{p}/units/a-6"),
        format!("{p}/units/a-3"),
        "/".to_owned(),
        "/day".to_owned(),
        format!("{p}/day"),
        "/inbox".to_owned(),
        format!("{p}/inbox"),
    ] {
        let (status, page) = get(&router, &path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let text = visible(&page).to_lowercase();
        assert!(!text.contains("usual"), "{path}: {text}");
        assert!(
            !page.contains("× usual") && !page.contains("overrun"),
            "{path}"
        );
    }
    // the drawer: a step's stream draws it
    let response = router
        .clone()
        .oneshot(
            Request::get(format!("{p}/steps/a-6-draft/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let mut first = String::new();
    while !first.contains("step-band") || !first.contains("</header>") {
        match tokio::time::timeout(std::time::Duration::from_secs(5), body.next()).await {
            Ok(Some(chunk)) => first.push_str(&String::from_utf8_lossy(&chunk.unwrap())),
            _ => break,
        }
    }
    assert!(first.contains("step-band"), "{first}");
    assert!(!visible(&first).to_lowercase().contains("usual"), "{first}");
}

async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (
        format!("http://{addr}"),
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_stats_fits_every_width_and_its_window_switch_works() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let (base, server) = serve(f.router()).await;
    let shots = std::env::var_os("SLUICE_STATS_SCREENS").map(std::path::PathBuf::from);
    tokio::task::spawn_blocking(move || {
        let page = format!("{base}/projects/id/{}/stats", n.almanac);
        let mut browser = Chrome::open(&page).unwrap();
        for width in [390, 1440, 2560] {
            for theme in ["light", "dark"] {
                browser.viewport(width, theme).unwrap();
                browser.navigate(&page).unwrap();
                browser.wait("document.readyState === 'complete'").unwrap();
                browser.viewport(width, theme).unwrap();
                let fit = browser
                    .eval("[document.documentElement.scrollWidth, innerWidth, [...document.querySelectorAll('.st-t, .st-slow, .st-spark')].every(e => e.getBoundingClientRect().right <= innerWidth + 0.5)]")
                    .unwrap();
                assert!(
                    fit[0].as_f64() <= fit[1].as_f64() && fit[2] == json!(true),
                    "{width} {theme}: {fit}"
                );
                // a stage's numbers sit in their labelled cells on a phone, in columns wider
                let label = browser
                    .eval("getComputedStyle(document.querySelector('.st-dur tbody .num'), '::before').content")
                    .unwrap();
                if width == 390 {
                    assert_ne!(label, json!("none"), "{label}");
                } else {
                    assert_eq!(label, json!("none"), "{width}: {label}");
                }
                if let Some(dir) = &shots {
                    browser
                        .screenshot(&dir.join(format!("stats-fixture-{width}-{theme}.png")))
                        .unwrap();
                }
            }
        }
        // the switch: a link a window, the chosen one filled; a click draws that window
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&page).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        let chosen = "document.querySelector('nav.view-switch[aria-label=\"Window\"] a[aria-current=\"page\"]')";
        assert_eq!(browser.eval(&format!("{chosen}.textContent")).unwrap(), "7 days");
        assert_ne!(
            browser.eval(&format!("getComputedStyle({chosen}).backgroundColor")).unwrap(),
            browser.eval("getComputedStyle(document.body).backgroundColor").unwrap()
        );
        browser
            .eval("[...document.querySelectorAll('nav.view-switch a')].find(a => a.textContent === '24 hours').click()")
            .unwrap();
        browser
            .wait(&format!("location.search === '?window=24h' && document.readyState === 'complete' && {chosen}?.textContent === '24 hours'"))
            .unwrap();
        let note = browser
            .eval("document.querySelector('.page-note').textContent")
            .unwrap();
        // the reader's clock is its own; the counts are the window's
        assert!(
            note.as_str()
                .is_some_and(|n| n.starts_with("16 runs ended and 5 units finished in the last 24 hours")),
            "{note}"
        );
    })
    .await
    .unwrap();
    server.abort();
}
