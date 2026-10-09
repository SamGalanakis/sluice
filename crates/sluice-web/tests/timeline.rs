//! A unit's runs in time (SPEC §13): the unit page's timeline and the step page's fold of it, how
//! long a recipe's stage usually takes, a step's chain on the board (`?root=`), and a recipe's
//! every unit (`?recipe=`), each read over HTTP as a browser gets it.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
use axum::http::StatusCode;
use board_fixture::{Fixture, lane_recipe};
use serde_json::json;
use sluice_model::ids::ProjectId;
use sluice_store::RetrySafety;

fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from} in {html}"));
    let end = html[start..].find(to).map_or(html.len(), |e| start + e);
    &html[start..end]
}

/// A minute `minutes` after a base an hour before now, as stored ("…T…Z") and as drawn ("… UTC"):
/// a run still going runs to the real now, so its seeds sit just before it, never on a fixed date.
fn recent(minutes: i64) -> (&'static str, String) {
    // read once, so a minute turning over mid-test never splits seeds from their assertions
    static BASE: std::sync::OnceLock<time::OffsetDateTime> = std::sync::OnceLock::new();
    let base = *BASE.get_or_init(|| {
        let now = time::OffsetDateTime::now_utc();
        now.replace_second(0)
            .unwrap()
            .replace_nanosecond(0)
            .unwrap()
            - time::Duration::minutes(60)
    });
    let t = base + time::Duration::minutes(minutes);
    let stored = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:00Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute()
    );
    let drawn = format!("{} UTC", stored[..16].replace('T', " "));
    (Box::leak(stored.into_boxed_str()), drawn)
}

/// Runs for `step` in `project`: each its start, its end (none while it runs) and its result's
/// status ("failed" with an error, "succeeded", or none).
async fn runs(
    f: &Fixture,
    project: ProjectId,
    step: &'static str,
    runs: &[(&'static str, Option<&'static str>, &'static str)],
) {
    let runs = runs.to_vec();
    let home = f._home.path().to_owned();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            for (start, end, status) in &runs {
                let attempt = uuid_like();
                let run = uuid_like();
                let phase = if end.is_some() { "terminal" } else { "executing" };
                tx.sql().execute(
                    "INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at) VALUES (?1,?2,?3,?4,'{}','hash',?5)",
                    (&attempt, project.to_string(), step, phase, start),
                )?;
                let result = match *status {
                    "" => None,
                    "failed" => Some(json!({"status":"failed","outputs":{},"error":{"error":"bad_request","message":"tests failed"}}).to_string()),
                    s => Some(json!({"status": s, "outputs": {}}).to_string()),
                };
                if end.is_none() {
                    // a run that writes as it goes is not quiet
                    let dir = home.join("runs").join(&run);
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(dir.join("stderr.txt"), "working").unwrap();
                }
                tx.sql().execute(
                    "INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at,started_at,finished_at,result) VALUES (?1,?2,?3,?4,?5,?5,?6,?7)",
                    (&run, project.to_string(), &attempt, step, start, end, result),
                )?;
            }
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
}
fn uuid_like() -> String {
    sluice_model::ids::RunId::new().to_string()
}

/// A project `recipes` whose recipe `lane` (with a view) made units d1, d2 and d3 (done: work
/// took 10m, 30m and 20m; only d1 and d2 have a land run) and r1 (forked, its work running).
async fn recipes(f: &Fixture) -> ProjectId {
    let specs = f._home.path().join("specs");
    std::fs::create_dir_all(&specs).unwrap();
    let mut steps = serde_json::Map::new();
    let mut statuses = vec![];
    for (unit, done) in [("d1", true), ("d2", true), ("d3", true), ("r1", false)] {
        let spec = specs.join(format!("{unit}.md"));
        std::fs::write(&spec, format!("# Ship {unit}\n\nThe body.")).unwrap();
        let tags = json!([format!("unit:{unit}")]);
        steps.insert(
            format!("{unit}-fork"),
            json!({"run":"custom.open","tags":tags}),
        );
        steps.insert(
            format!("{unit}-work"),
            json!({"run":"custom.open","tags":tags,"after":[format!("{unit}-fork")],
            "in":{"ticket":{"default":unit.to_uppercase()},"spec":{"file":spec.to_str().unwrap()}}}),
        );
        steps.insert(
            format!("{unit}-land"),
            json!({"run":"custom.open","tags":tags,"after":[format!("{unit}-work")]}),
        );
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        statuses.push((leak(format!("{unit}-fork")), "succeeded"));
        statuses.push((
            leak(format!("{unit}-work")),
            if done { "succeeded" } else { "running" },
        ));
        if done {
            statuses.push((leak(format!("{unit}-land")), "succeeded"));
        }
    }
    let id = f
        .project("recipes", json!({"steps": steps}), &statuses)
        .await;
    let dir = f
        ._home
        .path()
        .join("projects")
        .join(id.to_string())
        .join("recipes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lane.json"),
        serde_json::to_vec(&lane_recipe()).unwrap(),
    )
    .unwrap();
    for (unit, work) in [("d1", "10"), ("d2", "30"), ("d3", "20")] {
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        runs(
            f,
            id,
            leak(format!("{unit}-fork")),
            &[(
                "2026-10-06T09:00:00Z",
                Some("2026-10-06T09:01:00Z"),
                "succeeded",
            )],
        )
        .await;
        let end = leak(format!("2026-10-06T10:{work}:00Z"));
        runs(
            f,
            id,
            leak(format!("{unit}-work")),
            &[("2026-10-06T10:00:00Z", Some(end), "succeeded")],
        )
        .await;
        if unit != "d3" {
            runs(
                f,
                id,
                leak(format!("{unit}-land")),
                &[(
                    "2026-10-06T11:00:00Z",
                    Some("2026-10-06T11:05:00Z"),
                    "succeeded",
                )],
            )
            .await;
        }
    }
    runs(
        f,
        id,
        "r1-fork",
        &[(
            "2026-10-07T09:00:00Z",
            Some("2026-10-07T09:01:00Z"),
            "succeeded",
        )],
    )
    .await;
    runs(f, id, "r1-work", &[("2026-10-07T09:01:00Z", None, "")]).await;
    id
}

#[tokio::test]
async fn a_unit_page_draws_its_runs_on_one_axis_its_long_waits_collapsed_and_its_running_run_open()
{
    let f = Fixture::new().await;
    let id = f.titled().await;
    // l1: forked, its work failed once and runs again; l2: forked, then its work failed 6h later
    runs(
        &f,
        id,
        "l1-fork",
        &[(recent(0).0, Some(recent(2).0), "succeeded")],
    )
    .await;
    runs(
        &f,
        id,
        "l1-work",
        &[
            (recent(2).0, Some(recent(40).0), "failed"),
            (recent(41).0, None, ""),
        ],
    )
    .await;
    runs(
        &f,
        id,
        "l2-fork",
        &[(
            "2026-10-07T03:00:00Z",
            Some("2026-10-07T03:10:00Z"),
            "succeeded",
        )],
    )
    .await;
    runs(
        &f,
        id,
        "l2-work",
        &[(
            "2026-10-07T09:10:00Z",
            Some("2026-10-07T09:40:00Z"),
            "failed",
        )],
    )
    .await;

    let (status, html) = f.get(&format!("/projects/id/{id}/units/l1")).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let tl = between(&html, "<section class=\"d-sec unit-tl\"", "</section>");
    // a row a stage, in the unit's order, each named by its stage and leading to its step
    let rows: Vec<&str> = tl
        .split("<a class=\"tl-stage\"")
        .skip(1)
        .map(|r| between(r, "<span>", "</span>").trim_start_matches("<span>"))
        .collect();
    assert_eq!(rows, ["fork", "work", "land"], "{tl}");
    assert!(
        tl.contains(&format!("href=\"/projects/id/{id}/steps/l1-work\"")),
        "{tl}"
    );
    // the retry is a second bar after the failed one; the running one runs to now, open
    let work = between(tl, "steps/l1-work", "</li>");
    assert_eq!(work.matches("class=\"tl-bar ").count(), 2, "{work}");
    assert!(work.contains("class=\"tl-bar tl-failed\""), "{work}");
    assert!(work.contains("class=\"tl-bar tl-running tl-on\""), "{work}");
    assert!(
        work.contains("class=\"tl-g at-now\" style=\"left:100.000%\""),
        "{work}"
    );
    assert!(
        work.contains(&format!(
            "aria-label=\"Run 1 failed after 38 minutes. Run 2 running since {}\"",
            recent(41).1
        )),
        "{work}"
    );
    // its words say the runs, the time they took, the running one's ticking time
    assert!(
        work.contains(&format!(
            "<p class=\"tl-words\">2 runs · 38m · running <time data-since=\"{}\"",
            recent(41).0
        )),
        "{work}"
    );
    assert!(
        between(tl, "steps/l1-land", "</li>").contains("No run yet."),
        "{tl}"
    );
    // the axis: where it starts and "now", no break (nothing waited long)
    assert!(
        tl.contains(">start</span>") && tl.contains(">now</span>"),
        "{tl}"
    );
    assert!(!tl.contains("class=\"tl-break\""), "{tl}");

    // l2's 6h wait between fork and work is a break, named on the axis and in work's words
    let (_, html) = f.get(&format!("/projects/id/{id}/units/l2")).await;
    let tl = between(&html, "<section class=\"d-sec unit-tl\"", "</section>");
    assert_eq!(tl.matches("class=\"tl-break\"").count(), 1, "{tl}");
    assert!(tl.contains("title=\"waited 6h 0m\""), "{tl}");
    assert!(tl.contains("class=\"tl-tick at-mid\""), "{tl}");
    assert!(tl.contains("1 run · 30m · waited 6h 0m"), "{tl}");
    // the bars keep their scale past the break: fork 10m of the 40m drawn
    assert!(
        tl.contains("style=\"left:calc((100% - 48px) * 0.00000 + 0.0px);width:calc((100% - 48px) * 0.25000 + 0.0px)\""),
        "{tl}"
    );
    // a unit with no run has no timeline
    let (_, html) = f.get(&format!("/projects/id/{id}/units/l3")).await;
    assert!(!html.contains("unit-tl"), "{html}");

    // the step page folds its unit's timeline under Runs, its own row marked
    let (_, html) = f.get(&format!("/projects/id/{id}/steps/l1-work")).await;
    let fold = between(&html, "<details class=\"tl-fold more-fold\"", "</details>");
    assert!(
        fold.contains("<span>Its unit&#39;s timeline</span>"),
        "{fold}"
    );
    assert!(
        fold.contains("<li class=\"tl-row is-current\"><a class=\"tl-stage\""),
        "{fold}"
    );
    assert!(
        between(fold, "is-current", "</a>").contains("aria-current=\"page\""),
        "{fold}"
    );
    assert!(
        html.find("tl-fold").unwrap() < html.find("<ol class=\"attempts\"").unwrap(),
        "the fold is under Runs' head, before the runs"
    );
}

#[tokio::test]
async fn a_running_step_says_how_long_its_stage_usually_takes_from_three_done_units_or_more() {
    let f = Fixture::new().await;
    let id = recipes(&f).await;
    // work took 10m, 30m and 20m in the done units: usually 20m
    let (status, html) = f.get(&format!("/projects/id/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let pill = between(&html, "id=\"n-r1-work\"", "</a>");
    assert!(pill.contains("data-usually=\"1200\""), "{pill}");
    assert!(pill.contains("; its stage usually takes 20m\""), "{pill}");
    // far past twice its usual time, its timer's words say how far, one sentence for a reader
    // (no space before a comma), and the pill shows it in the attention tone, hidden from one
    assert!(
        pill.contains("<span class=\"vh\" data-tail=\", usually 20 minutes, ")
            && pill.contains("× its usual time</span></time>"),
        "{pill}"
    );
    assert!(pill.contains("<span class=\"over-x\" title=\""), "{pill}");
    assert!(
        pill.contains("× its usual time (usually 20m)\" aria-hidden=\"true\">"),
        "{pill}"
    );
    // a row drawn as a lane string (a phone, the drawer open) keeps it on its live stage
    let row = between(&html, "<tr id=\"unit-r1\"", "</tr>");
    let lane = between(row, "<p class=\"mx-lane fb-lane\"", "</p>");
    assert!(
        lane.contains("× its usual time\">") && lane.contains("<span class=\"over-x\""),
        "{lane}"
    );
    let (_, step) = f.get(&format!("/projects/id/{id}/steps/r1-work")).await;
    let badges = step
        .split("<p class=\"d-badges\">")
        .nth(1)
        .unwrap_or_default();
    let badges = &badges[..badges.find("</p>").unwrap_or(badges.len())];
    assert!(
        badges.contains(
            "<span class=\"meta d-usual over\">usually 20m</span><span class=\"over-x\">"
        ) && badges.contains("× usual</span>"),
        "{badges}"
    );
    // the clock never enters the drawing: the pill starts `<time data-since=`
    assert!(
        pill.contains("<time data-since=\"2026-10-07T09:01:00Z\""),
        "{pill}"
    );
    // a finished card carries no estimate
    assert!(
        !between(&html, "id=\"n-r1-fork\"", "</a>").contains("usually"),
        "{html}"
    );

    let (_, page) = f.get(&format!("/projects/id/{id}/steps/r1-work")).await;
    // its run started days ago, far past twice its usual 20m: the "usually" reads in ink
    assert!(
        page.contains("<span class=\"meta d-usual over\">usually 20m</span>"),
        "{page}"
    );
    // a done unit's step says it after how long it took
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/d2-work")).await;
    assert!(page.contains(" · took 30m · usually 20m</p>"), "{page}");
    // land ran in two done units only: too few to say
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/r1-land")).await;
    assert!(!page.contains("usually"), "{page}");
}

#[tokio::test]
async fn the_board_shows_a_steps_chain_from_its_address_and_says_so() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let shown = |html: &str| -> Vec<String> {
        html.split("data-step=\"")
            .skip(1)
            .map(|s| s[..s.find('"').unwrap()].to_owned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    // what comes after l2-land: l3's fork, and through it the rest of l3
    let (status, html) = f
        .get(&format!("/projects/id/{id}?root=l2-land&down=1"))
        .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert_eq!(
        shown(&html),
        ["l2-land", "l3-fork", "l3-land", "l3-work"],
        "{html}"
    );
    let note = between(&html, "<p class=\"focus-note meta\">", "</p>");
    assert!(
        note.starts_with(
            "<p class=\"focus-note meta\">Showing what comes after <a class=\"sref-a\""
        ),
        "{note}"
    );
    assert!(
        note.contains(&format!(
            " <a href=\"/projects/id/{id}\">Show everything</a>"
        )),
        "{note}"
    );
    // the form keeps the focus as it applies, and so does the stream
    assert!(html.contains("<input type=\"hidden\" name=\"root\" value=\"l2-land\"><input type=\"hidden\" name=\"down\" value=\"1\">"), "{html}");
    assert!(
        html.contains("/stream?order=live&#38;show=all&#38;root=l2-land&#38;down=1'"),
        "{html}"
    );
    // what it comes after, one step deep
    let (_, html) = f
        .get(&format!("/projects/id/{id}?root=l2-land&up=1&depth=1"))
        .await;
    assert_eq!(shown(&html), ["l2-land", "l2-work"], "{html}");
    assert!(html.contains(", 1 step each way."), "{html}");
    // both ways when neither is said
    let (_, html) = f.get(&format!("/projects/id/{id}?root=l2-land")).await;
    assert_eq!(
        shown(&html),
        [
            "l2-fork", "l2-land", "l2-work", "l3-fork", "l3-land", "l3-work"
        ],
        "{html}"
    );
    assert!(html.contains("Showing the chain of "), "{html}");
    // a step no longer in the plan: said, and nothing else drawn
    let (status, html) = f.get(&format!("/projects/id/{id}?root=gone")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("No step <code>gone</code> is in the plan now."),
        "{html}"
    );
    assert!(shown(&html).is_empty(), "{html}");
    // the step page leads there
    let (_, html) = f.get(&format!("/projects/id/{id}/steps/l2-land")).await;
    assert!(
        html.contains(&format!("<p class=\"chain-link meta\"><a href=\"/projects/id/{id}?root=l2-land&#38;up=1&#38;down=1\">Show its chain on the plan</a></p>")),
        "{html}"
    );
    // a step nothing joins has no chain to show
    let (_, html) = f.get(&format!("/projects/id/{id}/steps/plain")).await;
    assert!(!html.contains("chain-link"), "{html}");
}

#[tokio::test]
async fn a_matrix_head_leads_to_every_unit_its_recipe_made_done_ones_too() {
    let f = Fixture::new().await;
    let id = recipes(&f).await;
    let (_, html) = f.get(&format!("/projects/id/{id}")).await;
    assert!(
        html.contains(&format!(
            "<a class=\"mx-name\" href=\"/projects/id/{id}?recipe=lane&#38;show=all\""
        )),
        "{html}"
    );
    let (status, html) = f
        .get(&format!("/projects/id/{id}?recipe=lane&show=all"))
        .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(
        html.contains("Every unit of recipe <code>lane</code>: 4 units · 1 running · 3 done."),
        "{html}"
    );
    // the matrix stays live units only; the done ones are on the shelf, drawn open
    let matrix = between(&html, "data-matrix=\"lane\"", "</section>");
    assert!(matrix.contains("<tr id=\"unit-r1\""), "{matrix}");
    assert!(!matrix.contains("unit-d1"), "{matrix}");
    let shelf = &html[html.find("class=\"done-shelf\"").unwrap()..];
    assert!(
        html.contains("id=\"shelf-open\" class=\"done-shelf\""),
        "{html}"
    );
    for unit in ["d1", "d2", "d3"] {
        assert!(
            shelf.contains(&format!("id=\"unit-{unit}\" class=\"box done\"")),
            "{unit}: {shelf}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_draws_the_timeline_to_its_width_and_a_running_card_how_far_along_it_is() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    runs(
        &f,
        id,
        "l2-fork",
        &[(
            "2026-10-07T03:00:00Z",
            Some("2026-10-07T03:10:00Z"),
            "succeeded",
        )],
    )
    .await;
    runs(
        &f,
        id,
        "l2-work",
        &[(
            "2026-10-07T09:10:00Z",
            Some("2026-10-07T09:40:00Z"),
            "failed",
        )],
    )
    .await;
    let lanes = recipes(&f).await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let base = format!("http://{addr}");
        let mut browser = chrome::Chrome::open(&format!("{base}/projects/id/{id}/units/l2")).unwrap();
        let ready = "document.querySelector('.tl') && document.readyState === 'complete'";
        browser.wait(ready).unwrap();
        const GEOMETRY: &str = r#"(() => {
          const r = s => { const e = document.querySelector(s); return e && e.checkVisibility() ? e.getBoundingClientRect() : null; };
          const track = r('.tl-row:nth-child(2) .tl-track'), brk = r('.tl-break');
          const bars = [...document.querySelectorAll('.tl-bar, .tl-g')].map(e => e.getBoundingClientRect());
          const ticks = [...document.querySelectorAll('.tl-tick')].filter(e => e.checkVisibility()).map(e => e.getBoundingClientRect());
          return {scroll: document.documentElement.scrollWidth, width: document.documentElement.clientWidth,
                  track: track && [track.left, track.right], brk: brk && brk.width,
                  inside: !track || bars.every(b => b.left >= track.left - 8.5 && b.right <= track.right + 8.5),
                  apart: ticks.every((t, i) => i === 0 || t.left >= ticks[i - 1].right + 8),
                  ticks: ticks.length, phone: r('.tl-words') && getComputedStyle(document.querySelector('.tl-row')).display,
                  errors: window.browserErrors};
        })()"#;
        for theme in ["light", "dark"] {
            for width in [390, 1440, 2560] {
                browser.viewport(width, theme).unwrap();
                let g = browser.eval(GEOMETRY).unwrap();
                let label = format!("{width} {theme}");
                assert!(g["scroll"].as_f64().unwrap() <= g["width"].as_f64().unwrap(), "{label}: sideways scroll {g}");
                assert_eq!(g["errors"], serde_json::json!([]), "{label}: {g}");
                if width == 390 {
                    // a phone reads each stage as one line of words, no track
                    assert_eq!(g["track"], serde_json::Value::Null, "{label}: {g}");
                    assert_eq!(g["phone"], "flex", "{label}: {g}");
                } else {
                    // the break is 48px whatever the width; every bar and glyph in its track;
                    // the axis's labels never meet
                    assert_eq!(g["brk"], 48.0, "{label}: {g}");
                    assert_eq!(g["inside"], true, "{label}: {g}");
                    assert_eq!(g["apart"], true, "{label}: {g}");
                    assert!(g["ticks"].as_u64().unwrap() >= 2, "{label}: {g}");
                }
            }
        }
        // a running card whose stage usually takes 20m: nav.js draws how far along (past it: all)
        browser.navigate(&format!("{base}/projects/id/{lanes}")).unwrap();
        browser.viewport(1440, "light").unwrap();
        let along = browser
            .wait("document.querySelector('#n-r1-work')?.style.getPropertyValue('--along')")
            .unwrap();
        assert_eq!(along, "1.000");
        let line = browser
            .eval("getComputedStyle(document.querySelector('#n-r1-work'), '::after').width")
            .unwrap();
        assert_ne!(line, "0px", "{line}");
        // a finished card has none
        assert_eq!(
            browser.eval("getComputedStyle(document.querySelector('#n-r1-fork'), '::after').content").unwrap(),
            "none"
        );
    })
    .await
    .unwrap();
    server.abort();
}
