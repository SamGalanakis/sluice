//! The project page's plan on the neutral fixture (DESIGN.md, Pages): each unit drawn once in
//! its band, For you and Stopped first, Running quiet first then the longest, a block a recipe
//! with its stages as columns, a unit of no recipe its own small graph, loose steps one cell
//! each, the long run's progress in the margin, Recently finished and the Done index; in
//! Chromium no sideways scroll from 320px to 3840px, beside the board too.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
mod neutral;
mod plan_html;
use board_fixture::Fixture;
use chrome::Chrome;

fn at(html: &str, needle: &str) -> usize {
    html.find(needle)
        .unwrap_or_else(|| panic!("no {needle} in {html}"))
}

#[tokio::test]
async fn every_unit_of_the_fixture_is_drawn_once_in_its_band_in_rank() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let (status, html) = f.get(&format!("/projects/id/{}", n.almanac)).await;
    assert!(status.is_success(), "{html}");
    // a-7 asks the owner: For you, not again under Running; survey is the margin's long run
    let mut want: Vec<(&str, &str)> = vec![("a-7", "asks"), ("survey", "margin")];
    want.extend(neutral::STOPPED.iter().map(|u| (*u, "stopped")));
    want.extend(neutral::WAITING.iter().map(|u| (*u, "waiting")));
    want.extend(neutral::DONE.iter().map(|u| (*u, "done")));
    want.extend(["a-6", "s-3", "index"].iter().map(|u| (*u, "running")));
    for (unit, band) in want {
        assert_eq!(plan_html::place(&html, unit), band, "{unit}");
        // once: one row, module or line a unit
        let drawn = html.matches(&format!("data-unit=\"{unit}\"")).count()
            + html
                .matches(&format!("<code class=\"pl-did\">{unit}</code>"))
                .count();
        assert_eq!(drawn, 1, "{unit}");
    }
    // the bands in order: For you and Stopped, Running, Waiting, Done
    let order = [
        "<!--r:plan-asks-->",
        "<!--r:plan-stopped-->",
        "<!--r:plan-running-->",
        "<!--r:plan-waiting-->",
        "<!--r:plan-done-->",
    ];
    for pair in order.windows(2) {
        assert!(at(&html, pair[0]) < at(&html, pair[1]), "{pair:?}");
    }
    // Stopped: the failures before the cancel
    assert!(at(&html, "<!--r:s-a-4-->") < at(&html, "<!--r:s-a-5-->"));
    assert!(at(&html, "<!--r:s-s-4-->") < at(&html, "<!--r:s-a-5-->"));
    // Running: a block a recipe in the order of its first unit, the quiet one first, then the
    // longest; the unit of no recipe after them; the band's line says where the others went
    let running = plan_html::band(&html, "plan-running");
    assert!(at(running, "<!--r:u-s-3-->") < at(running, "<!--r:u-a-6-->"));
    assert!(at(running, "<!--r:u-a-6-->") < at(running, "<!--r:u-index-->"));
    assert!(
        running.contains(
            "Quiet first, then the longest. a-7 asks you above. survey runs in the margin."
        ),
        "{running}"
    );
    for head in [">article</a></h3>", ">scan</a></h3>", "<h3>No recipe</h3>"] {
        assert!(running.contains(head), "{head}: {running}");
    }
    // the stages from each recipe: article's three columns, scan's one
    let article = plan_html::row(&html, "a-6");
    for stage in neutral::ARTICLE {
        assert!(
            plan_html::cell(&html, &format!("a-6-{stage}")).contains("sc-name\">"),
            "{stage}"
        );
    }
    assert_eq!(article.matches("<li class=\"sc").count(), 3, "{article}");
    assert_eq!(
        plan_html::row(&html, "s-3")
            .matches("<li class=\"sc")
            .count(),
        1
    );
    // the overrun: its chip on the row, how far on its live cell, its usual time said
    assert!(article.contains("class=\"overrun\""), "{article}");
    assert!(article.contains(" · usually "), "{article}");
    assert!(plan_html::cell(&html, "a-6-draft").contains("sc-over"));
    // the unit of no recipe: its two gathers fan in to its merge, a connector an edge
    let index = plan_html::row(&html, "index");
    assert!(index.contains("<div class=\"pl-graph\""), "{index}");
    assert_eq!(index.matches("<path ").count(), 2, "{index}");
    // the margin: the long run's progress, its first field large
    let margin = plan_html::band(&html, "plan-margin");
    assert!(
        margin.contains("<article class=\"mod swell-margin\""),
        "{margin}"
    );
    assert!(
        margin.contains("<div class=\"mm-f mm-lead\"><dt>"),
        "{margin}"
    );
    assert!(margin.contains("data-step=\"survey\""), "{margin}");
    // Waiting in plan order, each saying what holds it
    let waiting = plan_html::band(&html, "plan-waiting");
    assert!(at(waiting, "<!--r:u-a-8-->") < at(waiting, "<!--r:u-a-9-->"));
    assert!(plan_html::row(&html, "a-8").contains("draft waits for <a"));
    // Recently finished: the latest five, newest first; the Done index every one, folded
    let recent = plan_html::between(&html, "<section class=\"band-strip\"", "</section>");
    assert_eq!(recent.matches("<li>").count(), 5, "{recent}");
    assert!(at(recent, "a-3") < at(recent, "a-1"), "{recent}");
    let done = plan_html::band(&html, "plan-done");
    assert!(done.contains("5 units · 11 steps"), "{done}");
    assert!(done.contains("<details class=\"pl-index\" data-preserve-attr=\"open\">"));
    // the sentence over it all
    let summary = plan_html::summary(&html);
    assert!(
        summary.starts_with("<p class=\"band-summary\"><a class=\"ask\"")
            && summary.contains("1 question for you</a>. 2 failed, 1 cancelled.")
            && summary
                .contains("2 article units, 1 scan unit and 2 units without a recipe at work"),
        "{summary}"
    );
    // the head over the units of no recipe says them the same way
    assert!(
        html.contains("1 unit without a recipe and 1 loose step")
            || html.contains("1 unit without a recipe: each draws its own steps"),
        "{html}"
    );
    assert!(!html.contains("of no recipe"), "{html}");
}

#[tokio::test]
async fn three_loose_steps_are_a_cell_each() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let (_, html) = f.get(&format!("/projects/id/{}", n.chores)).await;
    let units: Vec<&str> = ["sort-mail", "file-receipts", "water-plants"].to_vec();
    let places: Vec<&str> = units.iter().map(|u| plan_html::place(&html, u)).collect();
    assert_eq!(places, ["running", "waiting", "done"], "{html}");
    for unit in &units[..2] {
        let row = plan_html::row(&html, unit);
        assert_eq!(row.matches("<li class=\"sc").count(), 1, "{row}");
        assert!(!row.contains("pl-graph"), "{row}");
        // its cell is the step's own, opening it in the drawer
        assert!(
            row.contains(&format!(" id=\"n-{unit}\" data-step=\"{unit}\"")),
            "{row}"
        );
    }
    // no recipe, no "unit without a recipe": loose steps, said so
    assert!(html.contains("1 loose step."), "{html}");
    assert!(!html.contains("each draws its own steps"), "{html}");
    // a loose step's links lead to its own page, not a unit page
    assert!(!html.contains("/units/water-plants"), "{html}");
}

/// The plan never scrolls sideways and keeps its bands in its column from a 320px phone to a
/// 3840px screen, light and dark, alone and beside a board; at 2000px of sheet and over
/// Running and Waiting stand side by side. `SLUICE_PLAN_SCREENS=<dir>` saves each.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_the_plan_fits_every_width_from_a_phone_to_a_wide_screen() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let lanes = f.id;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let screens = std::env::var_os("SLUICE_PLAN_SCREENS").map(std::path::PathBuf::from);
        let base = format!("http://{addr}");
        let mut browser = Chrome::open(&format!("{base}/projects/id/{}", n.almanac)).unwrap();
        const LOOK: &str = r#"(() => {
  const sheet = document.querySelector('#plan-grid');
  const box = sheet.getBoundingClientRect();
  const bands = [...sheet.querySelectorAll(':scope > section, :scope > aside')].filter(e => e.checkVisibility());
  const out = bands.filter(b => { const r = b.getBoundingClientRect(); return r.left < box.left - 1 || r.right > box.right + 1; }).map(b => b.id);
  const r = id => document.getElementById(id)?.getBoundingClientRect();
  const run = r('plan-running'), wait = r('plan-waiting');
  return {scroll: document.documentElement.scrollWidth - document.documentElement.clientWidth,
          sheet: Math.round(box.width), out,
          side: !!(run && wait && Math.abs(run.top - wait.top) < 1 && run.right <= wait.left),
          clipped: [...sheet.querySelectorAll('.pl-t, .mod-t, .sc-name')].filter(e => e.getBoundingClientRect().right > document.documentElement.clientWidth + 0.5).length,
          errors: window.browserErrors};
})()"#;
        for (project, slug) in [(n.almanac, "almanac"), (n.chores, "chores"), (lanes, "both")] {
            browser.navigate(&format!("{base}/projects/id/{project}")).unwrap();
            browser
                .wait("document.readyState === 'complete' && document.querySelector('#plan-grid')")
                .unwrap();
            for width in [320, 390, 768, 1024, 1440, 1920, 2560, 3840] {
                for theme in ["light", "dark"] {
                    browser.viewport(width, theme).unwrap();
                    let g = browser.eval(LOOK).unwrap();
                    let label = format!("{slug} {width} {theme}");
                    assert_eq!(g["scroll"], 0, "{label}: sideways {g}");
                    assert_eq!(g["out"], serde_json::json!([]), "{label}: a band out of the sheet {g}");
                    assert_eq!(g["clipped"], 0, "{label}: {g}");
                    assert_eq!(g["errors"], serde_json::json!([]), "{label}: {g}");
                    if slug == "almanac" {
                        let wide = g["sheet"].as_f64().unwrap() >= 2000.0;
                        assert_eq!(g["side"], wide, "{label}: Running beside Waiting {g}");
                    }
                    if let Some(dir) = &screens {
                        std::fs::create_dir_all(dir).unwrap();
                        browser.screenshot(&dir.join(format!("plan-{slug}-{width}-{theme}.png"))).unwrap();
                    }
                }
            }
        }
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}

/// The band's name is sized by the band's own width, not the window's: with the drawer open
/// beside the plan at 1440px the band is narrower, so the name takes the band's tablet row and
/// a size for that width, and stays whole on one line.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_the_bands_name_follows_the_band_and_stays_whole_beside_the_drawer() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let page = format!("http://{addr}/projects/id/{}", n.almanac);
        let mut browser = Chrome::open(&page).unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelector('.band-head h1')")
            .unwrap();
        browser.viewport(1440, "light").unwrap();
        const NAME: &str = r#"(() => {
  const h1 = document.querySelector('.band-head > h1');
  const band = document.querySelector('.band-in');
  const pad = parseFloat(getComputedStyle(band).paddingLeft) + parseFloat(getComputedStyle(band).paddingRight);
  const size = parseFloat(getComputedStyle(h1).fontSize);
  const words = [...document.querySelectorAll('.band-sub')].map(e => e.getBoundingClientRect());
  return {size, band: band.clientWidth - pad, cls: h1.className,
          lines: Math.round(h1.getBoundingClientRect().height / (size * 0.8)),
          right: h1.getBoundingClientRect().right, edge: band.getBoundingClientRect().right,
          under: words.length > 0 && words[0].top >= h1.getBoundingClientRect().bottom - 1,
          scroll: document.documentElement.scrollWidth - document.documentElement.clientWidth};
})()"#;
        // alone: "almanac" (seven letters, the long step) at 7.54% of a 1376px band
        let alone = browser.eval(NAME).unwrap();
        assert_eq!(alone["cls"], "long", "{alone}");
        let band = alone["band"].as_f64().unwrap();
        let size = alone["size"].as_f64().unwrap();
        assert!((size - (band * 0.0754).clamp(64.0, 166.0)).abs() < 1.0, "{alone}");
        assert_eq!(alone["lines"], 1, "{alone}");
        assert_eq!(alone["under"], false, "beside its words at 1440 {alone}");
        // the drawer open beside it: the band narrows, the name follows the band
        browser
            .eval("history.replaceState(null,'','#step:a-6-draft');dispatchEvent(new HashChangeEvent('hashchange'))")
            .unwrap();
        browser
            .wait("document.documentElement.classList.contains('drawer-open')")
            .unwrap();
        browser.wait("new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r(true))))").unwrap();
        let beside = browser.eval(NAME).unwrap();
        let narrow = beside["band"].as_f64().unwrap();
        assert!(narrow < band - 500.0, "the drawer narrows the band {beside}");
        let size = beside["size"].as_f64().unwrap();
        assert!((size - (narrow * 0.0754).clamp(64.0, 166.0)).abs() < 1.0, "{beside}");
        assert_eq!(beside["lines"], 1, "the name stays whole {beside}");
        assert!(beside["right"].as_f64().unwrap() <= beside["edge"].as_f64().unwrap() + 0.5, "{beside}");
        assert_eq!(beside["under"], true, "a band this narrow sets its words under the name {beside}");
        assert_eq!(beside["scroll"], 0, "{beside}");
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}
