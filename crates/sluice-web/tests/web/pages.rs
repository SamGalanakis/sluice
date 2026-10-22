//! The pages around the plan, on the neutral fixture: home puts what needs the owner first and
//! answers a question in place; the day sets every run of the last 24 hours in the hour it
//! started, on the reader's clock, with its outcome and a rule at now, and its stream patches
//! only when the runs change; and in Chromium a question answered on home says so where it was
//! asked, and no page scrolls sideways at 390, 1440 and 2560px in either theme.
//! `SLUICE_PAGES_SCREENS=<dir>` saves each page at every width and theme.
use crate::board_fixture;
use crate::chrome;
use crate::neutral;
use axum::{
    Router,
    body::{Body, Bytes},
    http::{Request, StatusCode},
};
use board_fixture::Fixture;
use chrome::Chrome;
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};
use sluice_model::{
    commands::CommandRequest,
    ids::{AttemptId, ProjectId, RunId},
};
use sluice_store::RetrySafety;
use sluice_web::clock::Clock;
use std::time::Duration;
use tower::ServiceExt;

fn between<'a>(text: &'a str, from: &str, to: &str) -> &'a str {
    let start = text
        .find(from)
        .unwrap_or_else(|| panic!("no {from} in {text}"));
    let rest = &text[start..];
    &rest[..rest[from.len()..]
        .find(to)
        .map_or(rest.len(), |e| e + from.len())]
}
async fn get_with(router: &Router, path: &str, cookie: &str) -> String {
    let response = router
        .clone()
        .oneshot(
            Request::get(path)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(body.to_vec()).unwrap()
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
/// A run of `step` that started `ago` seconds ago and, with `took`, ended with `status`.
async fn run(
    f: &Fixture,
    project: ProjectId,
    step: &'static str,
    ago: u64,
    took: Option<u64>,
    status: &'static str,
) {
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let (attempt, run) = (AttemptId::new(), RunId::new());
            let start = neutral::ago(ago);
            let end = took.map(|t| neutral::ago(ago.saturating_sub(t)));
            let result = took.map(|_| json!({ "status": status }).to_string());
            tx.sql().execute(
                "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at,finished_at) VALUES (?1,?2,?3,1,1,?4,'{}','hash',?5,?6)",
                (attempt.to_string(), project.to_string(), step, if end.is_some() { "terminal" } else { "executing" }, &start, &end),
            )?;
            tx.sql().execute(
                "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at,finished_at,result) VALUES (?1,?2,?3,?4,1,1,?5,?5,?6,?7)",
                (run.to_string(), project.to_string(), attempt.to_string(), step, &start, &end, result),
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
}
/// The hour (0–23) an instant `ago` seconds back falls in, `zone` minutes east of UTC.
fn hour(ago: u64, zone: i64) -> String {
    let at = now() as i64 - ago as i64 + zone * 60;
    format!("{:02}", at.div_euclid(3600).rem_euclid(24))
}
/// The timetable's row for the hour an instant falls in, by its heading.
fn row<'a>(page: &'a str, hour: &str) -> &'a str {
    let at = page
        .find(&format!("<span class=\"tt-hour\">{hour}</span>"))
        .unwrap_or_else(|| panic!("no row {hour}"));
    let start = page[..at].rfind("<section class=\"tt-row").unwrap();
    let rest = &page[start..];
    &rest[..rest[10..]
        .find("<section class=\"tt-row")
        .map_or(rest.len(), |e| e + 10)]
}

/// Seconds ago for `minute` minutes into the clock hour that began between ten and eleven
/// hours back, so every run seeded with it starts in that one hour.
fn in_busy_hour(minute: u64) -> u64 {
    let start = (now() - 10 * 3600) / 3600 * 3600;
    now() - (start + minute * 60)
}
/// A busy hour of chores: eight water-plants successes of 2m and one of 10m, a failed
/// sort-mail and an hour-long file-receipts success.
async fn seed_busy_hour(f: &Fixture, chores: ProjectId) {
    for k in 0..8 {
        run(
            f,
            chores,
            "water-plants",
            in_busy_hour(2 + k),
            Some(120),
            "succeeded",
        )
        .await;
    }
    run(
        f,
        chores,
        "water-plants",
        in_busy_hour(20),
        Some(600),
        "succeeded",
    )
    .await;
    run(f, chores, "sort-mail", in_busy_hour(25), Some(90), "failed").await;
    run(
        f,
        chores,
        "file-receipts",
        in_busy_hour(30),
        Some(3720),
        "succeeded",
    )
    .await;
}
/// The chores cell of a timetable row.
fn chores_cell(row: &str) -> &str {
    let from = row
        .find("<p class=\"tt-pname\">chores</p>")
        .unwrap_or_else(|| panic!("no chores cell: {row}"));
    let rest = &row[from..];
    &rest[..rest.find("<div class=\"tt-c\">").unwrap_or(rest.len())]
}

async fn open(router: &Router, path: &str) -> impl Stream<Item = Bytes> + Unpin + use<> {
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
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    Box::pin(
        response
            .into_body()
            .into_data_stream()
            .map(|chunk| chunk.unwrap()),
    )
}
async fn read_for(body: &mut (impl Stream<Item = Bytes> + Unpin), window: Duration) -> String {
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + window;
    while let Ok(Some(chunk)) = tokio::time::timeout_at(deadline, body.next()).await {
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    text
}
fn drawn_version(page: &str) -> String {
    let signals = page
        .split("data-signals=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap()
        .replace("&#34;", "\"")
        .replace("&quot;", "\"");
    serde_json::from_str::<Value>(&signals).unwrap()["ver"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn the_day_patches_only_when_its_runs_change() {
    let mut f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    tokio::time::pause();
    f.dashboard = f
        .dashboard
        .with_clock(Clock::tokio(std::time::SystemTime::now()));
    let router = f.router();
    let page = get_with(&router, "/day", "sluice_zone=0").await;
    let version = drawn_version(&page);
    // the stream opened at the page's version sends nothing to bring it up to date
    let mut body = open(
        &router,
        &format!("/day/stream?datastar=%7B%22ver%22%3A%22{version}%22%7D"),
    )
    .await;
    let first = read_for(&mut body, Duration::from_secs(3)).await;
    assert!(!first.contains("datastar-patch-elements"), "{first}");
    // a commit that changes no run (a project's words, a message): nothing is sent
    let almanac = n.almanac;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE projects SET description='Other words' WHERE project_id=?1",
                [almanac.to_string()],
            )?;
            tx.changed(Some(almanac), "project");
            Ok(())
        })
        .await
        .unwrap();
    let quiet = read_for(&mut body, Duration::from_secs(4)).await;
    assert!(
        !quiet.contains("datastar-patch-elements") && !quiet.contains("datastar-patch-signals"),
        "no run changed, yet it sent: {quiet}"
    );
    // a run starts: the day is drawn again
    run(&f, n.chores, "file-receipts", 60, None, "").await;
    let wire = read_for(&mut body, Duration::from_secs(4)).await;
    assert!(wire.contains("selector #day-view"), "{wire}");
    assert!(wire.contains("<b>File the receipts</b>"), "{wire}");
    // a busy hour's runs: the patch folds its successes and lists what is notable
    seed_busy_hour(&f, n.chores).await;
    let wire = read_for(&mut body, Duration::from_secs(4)).await;
    assert!(wire.contains("+9 more succeeded, 2m to 10m"), "{wire}");
    // one more plain success joins the fold; a failure in that hour is listed
    run(
        &f,
        n.chores,
        "water-plants",
        in_busy_hour(40),
        Some(120),
        "succeeded",
    )
    .await;
    let wire = read_for(&mut body, Duration::from_secs(4)).await;
    assert!(wire.contains("+10 more succeeded, 2m to 10m"), "{wire}");
    run(
        &f,
        n.chores,
        "water-plants",
        in_busy_hour(45),
        Some(130),
        "failed",
    )
    .await;
    let wire = read_for(&mut body, Duration::from_secs(4)).await;
    let cell = chores_cell(
        &wire[wire
            .find(&format!("tt-more-{}-", (now() - 10 * 3600) / 3600 * 3600))
            .map_or(0, |at| {
                wire[..at].rfind("<section class=\"tt-row").unwrap_or(0)
            })..],
    );
    let listed = cell.split("<details").next().unwrap();
    assert!(
        listed.contains("<span class=\"badge fail\"><span class=\"g g-failed\""),
        "{cell}"
    );
    assert_eq!(listed.matches("g g-failed").count(), 2, "{cell}");
}

async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (
        format!("http://{addr}"),
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    )
}
const FRAMES: &str = "new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))";

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_no_page_scrolls_sideways_from_a_small_phone_to_a_wide_screen() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    // a page that cannot be drawn, as the server's error layer draws it for the address after
    // `/missing`
    let dashboard = f.dashboard.clone();
    let router = f.router().route(
        "/missing/{*rest}",
        axum::routing::get(
            move |axum::extract::Path(rest): axum::extract::Path<String>,
                  headers: axum::http::HeaderMap| {
                let dashboard = dashboard.clone();
                async move {
                    sluice_web::views::missing::page(
                        &dashboard,
                        &format!("/{rest}"),
                        StatusCode::NOT_FOUND,
                        "",
                        &headers,
                    )
                    .await
                    .unwrap()
                }
            },
        ),
    );
    let (base, server) = serve(router).await;
    tokio::task::spawn_blocking(move || {
        let pages = [
            ("home", "/".to_owned()),
            ("day", "/day".to_owned()),
            ("plan", format!("/projects/id/{}", n.almanac)),
            ("step", format!("/projects/id/{}/steps/a-4-review", n.almanac)),
            ("unit", format!("/projects/id/{}/units/a-4", n.almanac)),
            ("stats", format!("/projects/id/{}/stats", n.almanac)),
            ("gallery", "/_ui".to_owned()),
            ("project-day", format!("/projects/id/{}/day", n.almanac)),
            ("log", "/log".to_owned()),
            ("project-log", format!("/projects/id/{}/log", n.almanac)),
            ("functions", "/fns".to_owned()),
            ("settings", format!("/projects/id/{}/settings", n.almanac)),
            ("history", "/history".to_owned()),
            ("missing", "/missing/projects/id/nothing-here".to_owned()),
            ("missing-step", format!("/missing/projects/id/{}/steps/no-such-step", n.almanac)),
            ("docs", "/docs".to_owned()),
            ("docs-page", "/docs/board".to_owned()),
        ];
        let screens = std::env::var_os("SLUICE_PAGES_SCREENS").map(std::path::PathBuf::from);
        let mut browser = Chrome::open(&format!("{base}/")).unwrap();
        for width in [390, 1440, 2560] {
            for theme in ["light", "dark"] {
                browser.viewport(width, theme).unwrap();
                for (name, path) in &pages {
                    browser.navigate(&format!("{base}{path}")).unwrap();
                    browser.wait("document.readyState === 'complete'").unwrap();
                    browser.eval("document.fonts.ready").unwrap();
                    browser.eval(FRAMES).unwrap();
                    let g = browser
                        .eval("({scroll: document.documentElement.scrollWidth, width: document.documentElement.clientWidth})")
                        .unwrap();
                    assert!(
                        g["scroll"].as_f64() <= g["width"].as_f64(),
                        "{name} at {width}px scrolls sideways: {g}"
                    );
                    // the top bar's content (its sections first) and the page share their left
                    // edge, in the column centred beside the sidebar
                    let edges = browser.eval("(n => { const l = n.getBoundingClientRect().left + parseFloat(getComputedStyle(n).paddingLeft); return [Math.round(l), Math.round(document.querySelector('main > :not(sluice-banner):not([hidden])')?.getBoundingClientRect().left ?? l)]; })(document.querySelector('header.top .top-in'))").unwrap();
                    assert_eq!(edges[0], edges[1], "{name} {width}: {edges}");
                    // and that column centred in the room beside the sidebar
                    let gap = browser.eval("(m => { const a = document.querySelector('.mainarea').getBoundingClientRect(), r = m.getBoundingClientRect(); return Math.abs((r.left - a.left) - (a.right - r.right)); })(document.querySelector('main'))").unwrap();
                    assert!(gap.as_f64().unwrap() < 1.0, "{name} {width}: off centre by {gap}");
                    if let Some(dir) = &screens {
                        browser
                            .screenshot(&dir.join(format!("{name}-{width}-{theme}.png")))
                            .unwrap();
                    }
                }
            }
        }
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test]
async fn home_attention_and_reader_day() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    {
        let (status, home) = f.get("/").await;
        assert_eq!(status, 200);
        // the head: "All projects" for a screen reader (the bar names the page), the sentence
        // across the projects its lead, counts alone: the question first, linked to For you;
        // what finished is on each project's card
        let band = between(
            &home,
            "<div id=\"home-band\" class=\"band-wrap\">",
            "<main id=\"content\"",
        );
        assert!(
            band.contains("<h1 class=\"vh\">All projects</h1>"),
            "{band}"
        );
        assert!(
            band.contains("<p class=\"page-line\"><a class=\"ask\" href=\"#for-you\">1 question for you</a>, 3 failed, 1 cancelled, 1 quiet; 6 running in 2 projects.</p>"),
            "{band}"
        );
        assert!(!band.contains("Recently finished"), "{band}");
        // nothing holds the runner in a test home: said under the sentence
        assert!(
            band.contains("<div class=\"page-alert runner-off\" role=\"status\">"),
            "{band}"
        );
        // For you: the question as a line of the Needs-you list, its teal dot, Answer the
        // way to its step (where it is answered) and × to set it aside
        let ask = between(
            &home,
            &format!(
                "<li id=\"item-{}-{}\" class=\"nd-i ask\"",
                n.almanac, n.question
            ),
            "</li>",
        );
        assert!(
            ask.contains("Use the checklist&#39;s new spring dates in this draft?"),
            "{ask}"
        );
        assert!(
            ask.contains(&format!(
                "<a class=\"btn primary\" href=\"/projects/id/{}/steps/a-7-draft\">Answer</a>",
                n.almanac
            )),
            "{ask}"
        );
        assert!(
            ask.contains(&format!("name=\"items\" value=\"ask:{}\"", n.question)),
            "{ask}"
        );
        assert!(!ask.contains("<sluice-answer "), "{ask}");
        // the projects by what needs the owner: the question's, then the stopped, then the running
        let grid = between(&home, "<div class=\"proj-grid\">", "</section>");
        let at = |name: &str| {
            grid.find(&format!("aria-label=\"{name}\""))
                .unwrap_or_else(|| panic!("no {name} in {grid}"))
        };
        assert!(at("almanac") < at("lanes"), "{grid}");
        assert!(at("lanes") < at("chores"), "{grid}");
        // a project with no steps has no card: a line under the cards names it
        assert!(!grid.contains("aria-label=\"plain\""), "{grid}");
        let almanac = between(grid, "aria-label=\"almanac\"", "</article>");
        // its card is its plan in brief: the plan's sentence (which counts what needs the
        // owner, said once), At work's groups a line each, what finished lately by name
        assert!(
            almanac.contains("<p class=\"pm-summary\"><a class=\"ask\""),
            "{almanac}"
        );
        assert!(!almanac.contains("need you"), "{almanac}");
        assert!(
            almanac.contains("<ul class=\"pm-groups\"><li><b>"),
            "{almanac}"
        );
        assert!(
            almanac.contains(
                "<p class=\"pm-done\"><span class=\"pm-done-h\">Finished lately:</span> <a href="
            ),
            "{almanac}"
        );
        // no step's own rows, no square a unit
        assert!(
            !almanac.contains("pm-row") && !almanac.contains("pm-squares"),
            "{almanac}"
        );
        // Today: a line a project, and the way to the timetable
        let today = between(&home, "<div class=\"mod td", "</div>");
        assert!(
            today.contains(&format!(
                "<a class=\"td-name\" href=\"/projects/id/{}/day\">almanac</a>",
                n.almanac
            )),
            "{today}"
        );
        assert!(today.contains("<a href=\"/day\">Timetable</a>"), "{today}");
        // no index repeats the cards' counts; the project with no steps is named under them
        assert!(!home.contains("class=\"ix\""), "{home}");
        let others = between(&home, "<p class=\"home-others meta\">", "</p>");
        assert!(
            others.starts_with("<p class=\"home-others meta\">No steps yet: <a href=")
                && others.contains(">plain</a>"),
            "{others}"
        );
        // the answer's words post from its form
        let response = f
            .router()
            .oneshot(super::forms::post(
                format!("/projects/id/{}/messages/{}/reply", n.almanac, n.question),
                "reply-form",
                &[("body", "Use the new dates.")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(matches!(
            f.commands.0.lock().unwrap().last(),
            Some(CommandRequest::Reply(r)) if r.body == "Use the new dates."
        ));
    }
    {
        // a quick run: counted, not listed
        run(
            &f,
            n.chores,
            "water-plants",
            3 * 3600,
            Some(20),
            "succeeded",
        )
        .await;
        let router = f.router();
        let page = get_with(&router, "/day", "sluice_zone=0").await;
        // the day line's sentence: the runs counted by project
        assert!(page.contains("<p class=\"dl-said\">"), "{page}");
        let summary = between(&page, "<p class=\"dl-said\">", "</p>");
        assert!(summary.contains(" in almanac"), "{summary}");
        assert!(summary.contains("failed"), "{summary}");
        assert!(
            summary.contains("running now, the longest for <time data-since="),
            "{summary}"
        );
        assert!(
            summary.contains("1 succeeded in under a minute.")
                && !summary.contains("counted, not listed"),
            "{summary}"
        );
        // the head says the date, not whose clock the hours are on
        assert!(!page.contains("hours on"), "{page}");
        // the day line: a row a project, its bars, the rule at now
        let line = between(&page, "<sluice-day class=\"dl\"", "</section>");
        assert!(
            line.contains("<h3 class=\"dl-name\">almanac</h3>"),
            "{line}"
        );
        assert!(line.contains("class=\"dl-bar dl-look\""), "{line}");
        assert!(line.contains("class=\"dl-bar dl-run\""), "{line}");
        assert!(line.contains("data-open=\""), "{line}");
        assert!(
            line.contains("<div class=\"dl-now\"><span>now, <time data-clock"),
            "{line}"
        );
        // the timetable: a row an hour, the current one last with the rule at now; two or more
        // hours in which nothing started one line, "03–14 · nothing ran", all 24 hours told
        let gaps: Vec<u64> = page
            .split("<section class=\"tt-row tt-gap\"")
            .skip(1)
            .map(|gap| {
                assert!(gap.contains("nothing ran</p>"), "{gap}");
                let said = &between(gap, "<span class=\"vh\">", "</span>")[17..];
                let hour = |at: usize| said[at..at + 2].parse::<u64>().unwrap();
                (hour(9) + 24 - hour(0)) % 24 + 1
            })
            .collect();
        assert!(!gaps.is_empty() && gaps.iter().all(|n| *n >= 2), "{gaps:?}");
        assert_eq!(
            page.matches("<section class=\"tt-row").count() - gaps.len()
                + gaps.iter().sum::<u64>() as usize,
            24,
            "{page}"
        );
        let current = row(&page, &hour(0, 0));
        assert!(current.contains("tt-current"), "{current}");
        assert!(
            current.contains("<p class=\"tt-nowrule\"><span>now, <time data-clock"),
            "{current}"
        );
        // a-4-review failed two hours ago after 18m: in its hour, by its minute, on the sand
        let failed = row(&page, &hour(2 * 3600, 0));
        assert!(
        failed.contains(
            "<b>Waders: the autumn guide&#39;s entries</b> <span class=\"tt-stage\">review</span>"
        ),
        "{failed}"
    );
        assert!(
            failed.contains(
                "<span class=\"tt-out\"><span class=\"badge fail\"><span class=\"g g-failed\""
            ),
            "{failed}"
        );
        assert!(failed.contains("</span>failed 18m</span>"), "{failed}");
        // a-6-draft runs: the blue, ticking
        let running = row(&page, &hour(70 * 60, 0));
        assert!(
            running.contains(
                "<b>Terns: the spring guide&#39;s entries</b> <span class=\"tt-stage\">draft</span>"
            ),
            "{running}"
        );
        assert!(
            running.contains("<span class=\"tt-out\"><span class=\"badge running\">"),
            "{running}"
        );
        // a succeeded run: its check and how long on the sky pill, said in words for a screen
        // reader
        let done = row(&page, &hour(3 * 3600, 0));
        assert!(
            done.contains(
                "<b>Scan the checklist for renamed species</b> <span class=\"tt-stage\">scan</span>"
            ),
            "{done}"
        );
        assert!(
            done.contains("<span class=\"vh\">succeeded after </span>4m"),
            "{done}"
        );
        assert!(
            done.contains("<p class=\"tt-quick\">1 run under a minute, all succeeded</p>"),
            "{done}"
        );
        // the long run started days ago: in the first row, its day and time
        let first = between(&page, "<section class=\"tt-row", "</section>");
        assert!(
            first.contains("<b>Survey the archive&#39;s photographs against the checklist</b>"),
            "{first}"
        );
        // which run it is, the link's title says, not its line
        assert!(first.contains("title=\"survey, run 2\""), "{first}");
        assert!(!first.contains("class=\"tt-n\""), "{first}");
        // each run links its step's runs
        assert!(page.contains(&format!(
            "href=\"/projects/id/{}/steps/a-4-review?tab=runs\"",
            n.almanac
        )));
        // on the reader's clock, two hours east: every hour moves
        let east = get_with(&router, "/day", "sluice_zone=120").await;
        assert!(!east.contains("your clock"), "{east}");
        let failed = row(&east, &hour(2 * 3600, 120));
        assert!(
        failed.contains(
            "<b>Waders: the autumn guide&#39;s entries</b> <span class=\"tt-stage\">review</span>"
        ),
        "{failed}"
    );
        // a project's own day: its column alone, the page's name for a screen reader, the tab current
        let own = get_with(&router, &format!("/projects/id/{}/day", n.chores), "").await;
        assert!(own.contains("<h1 class=\"vh\">Day</h1>"), "{own}");
        assert!(!own.contains("Waders"), "{own}");
        assert!(
            own.contains(&format!(
                "<a href=\"/projects/id/{}/day\" aria-current=\"page\">Day</a>",
                n.chores
            )),
            "{own}"
        );
        // the home's sections link it too
        let (_, home) = f.get("/").await;
        assert!(home.contains("<a href=\"/day\">Day</a>"), "{home}");
    }
}
