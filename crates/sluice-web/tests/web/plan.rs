//! The project page's plan on the neutral fixture (DESIGN.md, The plan): its head's sentence,
//! Needs you (a question, the stops, stops sharing a cause as one, a silence), what finished
//! lately, At work (a group a recipe, no recipe and the long runs after, the longest silent
//! first) and the steps each row opens to without script; what the owner sets aside comes back
//! when it changes; the read cursor is this browser's; in Chromium no sideways scroll from a
//! phone to a wide screen, beside the board too.
use crate::board_fixture;
use crate::chrome;
use crate::neutral;
use crate::plan_html;
use axum::{
    Extension, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use board_fixture::Fixture;
use chrome::Chrome;
use sluice_model::{
    commands::{NeedsDismiss, StepDismiss},
    error::PublicError,
};
use sluice_store::{RetrySafety, Writer};
use sluice_web::views::step::{Action, CommandService, Commands, OwnerCommand};
use std::{future::Future, pin::Pin, sync::Arc};
use tower::ServiceExt;

/// The value after `marker` in `html`, to the next quote.
fn value<'a>(html: &'a str, marker: &str) -> &'a str {
    let rest = &html[at(html, marker) + marker.len()..];
    &rest[..rest.find('"').unwrap()]
}
fn at(html: &str, needle: &str) -> usize {
    html.find(needle)
        .unwrap_or_else(|| panic!("no {needle} in {html}"))
}

/// The plan never scrolls sideways and keeps its sections in its column from a phone to a
/// 2560px screen, light and dark, alone and beside a board, a row open to its steps.
/// `SLUICE_PLAN_SCREENS=<dir>` saves each.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_the_plan_fits_every_width_from_a_phone_to_a_wide_screen() {
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
  const sheet = document.querySelector('.plan-sheet');
  const box = sheet.getBoundingClientRect();
  const row = document.querySelector('.ro-u'); if (row) row.open = true;
  const out = [...sheet.querySelectorAll(':scope > section')].filter(e => e.checkVisibility()).filter(b => { const r = b.getBoundingClientRect(); return r.left < box.left - 1 || r.right > box.right + 1; }).map(b => b.id);
  const head = document.querySelector('.plan-band h1').getBoundingClientRect();
  return {scroll: document.documentElement.scrollWidth - document.documentElement.clientWidth,
          out, edge: Math.abs(head.left - box.left) < 1,
          clipped: [...sheet.querySelectorAll('.ro-t, .nd-say, .fin-t, .sv-a')].filter(e => e.getBoundingClientRect().right > document.documentElement.clientWidth + 0.5).length,
          errors: window.browserErrors};
})()"#;
        for (project, slug) in [(n.almanac, "almanac"), (n.chores, "chores"), (lanes, "both")] {
            browser.navigate(&format!("{base}/projects/id/{project}")).unwrap();
            browser
                .wait("document.readyState === 'complete' && document.querySelector('.plan-sheet')")
                .unwrap();
            for width in [390, 1440, 2560] {
                for theme in ["light", "dark"] {
                    browser.viewport(width, theme).unwrap();
                    let g = browser.eval(LOOK).unwrap();
                    let label = format!("{slug} {width} {theme}");
                    assert_eq!(g["scroll"], 0, "{label}: sideways {g}");
                    assert_eq!(g["out"], serde_json::json!([]), "{label}: a section out of the sheet {g}");
                    assert_eq!(g["edge"], true, "{label}: the head and the sheet share a left edge {g}");
                    assert_eq!(g["clipped"], 0, "{label}: {g}");
                    assert_eq!(g["errors"], serde_json::json!([]), "{label}: {g}");
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

#[tokio::test]
async fn plan_units_and_loose_steps() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    {
        let (status, html) = f.get(&format!("/projects/id/{}", n.almanac)).await;
        assert!(status.is_success(), "{html}");
        // a-7 asks the owner; the stops in Needs you; at work a row each; done ones lately
        // finished as cards, the oldest only counted; units not begun only counted
        let mut want: Vec<(&str, &str)> = vec![("a-7", "asks"), ("a-1", "")];
        want.extend(neutral::STOPPED.iter().map(|u| (*u, "stopped")));
        want.extend(neutral::WAITING.iter().map(|u| (*u, "")));
        want.extend(["a-2", "a-3", "s-1", "s-2"].iter().map(|u| (*u, "done")));
        want.extend(
            ["a-6", "s-3", "index", "survey"]
                .iter()
                .map(|u| (*u, "running")),
        );
        for (unit, place) in want {
            assert_eq!(plan_html::place(&html, unit), place, "{unit}");
        }
        // the sections in order: Needs you, Finished lately, At work; no Done index unasked
        let order = [
            "<!--r:plan-needs-->",
            "<!--r:plan-finished-->",
            "<!--r:plan-roster-->",
            "<!--r:plan-done-->",
        ];
        for pair in order.windows(2) {
            assert!(at(&html, pair[0]) < at(&html, pair[1]), "{pair:?}");
        }
        assert!(!html.contains("class=\"pl-idx\""), "{html}");
        // Needs you: the question, the failures (two at their wall-clock cap said once, with
        // what to try), the cancel, the silence; each one action and a ×
        let needs = plan_html::needs(&html);
        let ids: Vec<&str> = needs
            .iter()
            .map(|i| &i[8..i[8..].find('"').unwrap() + 8])
            .collect();
        assert_eq!(
            ids,
            [
                format!("n-ask-{}", n.question).as_str(),
                "n-cause-wallcap",
                "n-stop-a-5-draft",
                "n-quiet-s-3-scan",
            ]
        );
        let cause = needs[1];
        assert!(
            cause.contains("Two units failed: they hit their wall-clock cap. <span class=\"nd-next\">Raise the agents&#39; time limit (SLUICE_AGENT_MAX_MIN in the project&#39;s .env), then Retry all.</span>")
                && cause.contains("<summary>Show the two</summary>")
                && cause.contains(">Retry all</button>")
                && cause.contains("value=\"stop:a-4-review stop:s-4-scan\""),
            "{cause}"
        );
        for item in &needs {
            assert_eq!(item.matches("class=\"x\"").count(), 1, "{item}");
        }
        assert!(
            needs[0].contains("<span class=\"nk nk-ask\" role=\"img\" aria-label=\"Question\">")
        );
        assert!(
            needs[3].contains("<span class=\"nk nk-quiet\" role=\"img\" aria-label=\"Quiet\">")
        );
        assert!(needs[3].contains(">Message it</a>"), "{}", needs[3]);
        // the head's sentence, from the counts alone
        let summary = plan_html::summary(&html);
        assert!(
            summary.contains(
                "1 question for you</a>, 2 failed, 1 cancelled, 1 quiet. 5 at work; 5 of 16 done."
            ),
            "{summary}"
        );
        // the tab says it too, and its icon is the question's
        assert!(
            html.contains("<title>(1) ✕4 · almanac · sluice</title>"),
            "{html}"
        );
        assert!(html.contains("href=\"/static/favicon-ask.svg?v="), "{html}");
        // Finished lately, with no look kept: the latest four, newest first, by title
        let cards = plan_html::band(&html, "plan-finished");
        assert!(
            cards.contains("<h2 id=\"finished\">Finished lately</h2>"),
            "{cards}"
        );
        assert_eq!(cards.matches("<li class=\"fin-i\"").count(), 4, "{cards}");
        assert!(
            at(cards, "/units/a-3\"") < at(cards, "/units/s-2\""),
            "{cards}"
        );
        // At work: a group a recipe, then no recipe, the long runs last; the longest silent
        // first; the axis's labels over the tracks
        let roster = plan_html::band(&html, "plan-roster");
        assert!(
            at(roster, ">article</a></b>") < at(roster, ">scan</a></b>")
                && at(roster, ">scan</a></b>") < at(roster, "<b>No recipe</b>")
                && at(roster, "<b>No recipe</b>") < at(roster, "<b>Long runs</b>"),
            "{roster}"
        );
        assert!(at(roster, "<!--r:u-a-6-->") < at(roster, "<!--r:u-a-7-->"));
        assert!(
            roster.contains(">12h</span>") && roster.contains(">now</span>"),
            "{roster}"
        );
        assert!(
            roster.contains("<b>3 waiting</b> on earlier work · <a href=")
                && roster.contains("<b>5 done</b></a>, the last"),
            "{roster}"
        );
        // a row: its state in words beside its mark, its track a tick a message
        let a6 = plan_html::row(&html, "a-6");
        assert!(
            a6.contains("<span class=\"vh\">running, </span>draft · ")
                && a6.contains("heard ")
                && a6.contains("aria-label=\"Its last twelve hours, on a log scale: 3 messages\""),
            "{a6}"
        );
        assert_eq!(a6.matches("<i class=\"tk-m\"").count(), 3, "{a6}");
        let s3 = plan_html::row(&html, "s-3");
        assert!(
            s3.contains("<span class=\"pill pill-quiet\">quiet "),
            "{s3}"
        );
        assert!(s3.contains("<i class=\"tk-quiet\""), "{s3}");
        // the drilldown, without script: a disclosure whose body is the unit's steps on its
        // axis, the current one marked, each step what it runs on, then its latest and actions
        assert!(
            a6.starts_with("<details id=\"u-a-6\" class=\"ro-u\" data-unit=\"a-6\" data-said=\"a-6-draft running|a-6-review pending|a-6-publish pending\" data-preserve-attr=\"open\"><summary class=\"ro-r\">"),
            "{a6}"
        );
        let drill = &a6[at(a6, "<div class=\"ro-drill\">")..];
        assert!(drill.contains("<sluice-steps class=\"sv\""), "{drill}");
        assert!(
            plan_html::cell(drill, "a-6-draft").starts_with("<li class=\"sv-row sv-c-run is-cur\""),
            "{drill}"
        );
        // every step runs on the one fn: no column says so
        assert!(
            drill.contains("<div class=\"sv-g sv-one-on sv-no-pr\"")
                && !drill.contains("runs on ")
                && !drill.contains(">Progress<"),
            "{drill}"
        );
        // its foot: Open step the one move, the rest quiet links
        assert!(
            drill.contains("<p class=\"ro-ev\">Latest · ")
                && drill.contains("<span class=\"ro-links\"><a href=")
                && drill.contains(">Message</a>")
                && drill.contains(" data-step=\"a-6-draft\">Open step</a>"),
            "{drill}"
        );
        // a unit of no recipe: in its step view its two gathers fan in to its merge
        let index = plan_html::row(&html, "index");
        let merge = plan_html::between(index, "data-sv=\"index-merge\"", "</li>");
        assert_eq!(merge.matches("class=\"sv-arrow\"").count(), 2, "{merge}");
        // the Done index when asked for: every done unit
        let (_, done) = f
            .get(&format!("/projects/id/{}?show=done", n.almanac))
            .await;
        let index = plan_html::band(&done, "plan-done");
        assert!(index.contains("5 units · 11 steps"), "{index}");
        assert_eq!(plan_html::done_lines(&done, "a-1"), 1);
    }
    {
        let (_, html) = f.get(&format!("/projects/id/{}", n.chores)).await;
        let units: Vec<&str> = ["sort-mail", "file-receipts", "water-plants"].to_vec();
        let places: Vec<&str> = units.iter().map(|u| plan_html::place(&html, u)).collect();
        assert_eq!(places, ["running", "", "done"], "{html}");
        // a loose step's card leads to its own page
        assert!(
            plan_html::band(&html, "plan-finished").contains(&format!(
                "href=\"/projects/id/{}/steps/water-plants\"",
                n.chores
            )),
            "{html}"
        );
        assert!(
            plan_html::summary(&html).contains("1 at work, all heard from recently. 1 of 3 done."),
            "{html}"
        );
        assert!(plan_html::band(&html, "plan-needs").contains("<p class=\"nd-none\">Nothing.</p>"));
    }
}

/// Setting aside and back as the coordinator takes it: the owner's marks in the store.
struct Marks(Writer);
impl CommandService for Marks {
    fn execute(
        &self,
        command: OwnerCommand,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicError>> + Send + '_>> {
        Box::pin(async move {
            self.0
                .write(RetrySafety::Idempotent, move |tx| match command.action {
                    Action::SetAside | Action::Restore => sluice_store::messages::needs_dismiss(
                        tx,
                        NeedsDismiss {
                            project: command.project,
                            items: command.items,
                            dismissed: command.action == Action::SetAside,
                        },
                    ),
                    Action::Dismiss | Action::Undismiss => sluice_store::messages::dismiss(
                        tx,
                        StepDismiss {
                            project: command.project,
                            step: command.step.expect("a step"),
                            dismissed: command.action == Action::Dismiss,
                        },
                    ),
                    _ => Err(PublicError::BadRequest {
                        message: "only marks here".into(),
                    }
                    .into()),
                })
                .await
        })
    }
}
/// A browser's request without script: its status, where it is sent, the cookie it is given
/// and the page.
async fn browse(
    router: &Router,
    path: &str,
    form: Option<&[(&str, &str)]>,
    cookie: &str,
) -> (StatusCode, String, String, String) {
    let request = match form {
        Some(form) => Request::post(path)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(
                url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(form)
                    .finish(),
            )),
        None => Request::get(path).body(Body::empty()),
    };
    let mut request = request.unwrap();
    if !cookie.is_empty() {
        request
            .headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
    }
    request
        .headers_mut()
        .insert(header::ACCEPT, "text/html".parse().unwrap());
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let get = |name| {
        response
            .headers()
            .get(name)
            .map(|v: &header::HeaderValue| v.to_str().unwrap().to_owned())
            .unwrap_or_default()
    };
    let (location, set) = (get(header::LOCATION), get(header::SET_COOKIE));
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        location,
        set,
        String::from_utf8(body.to_vec()).unwrap(),
    )
}

/// The bug this catches: a stop set aside staying hidden after it was retried and failed again
/// (or never coming back at all), and the tab still counting it while hidden.
#[tokio::test]
async fn a_stop_set_aside_comes_back_when_it_fails_again() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let router = f
        .router()
        .layer(Extension(Commands(Arc::new(Marks(f.writer.clone())))));
    let plan = format!("/projects/id/{}", n.almanac);
    // × on the two stops at their cap, without script: back to the plan at its Undo
    let (status, location, _, _) = browse(
        &router,
        &format!("{plan}/needs"),
        Some(&[
            ("action", "dismiss"),
            ("items", "stop:a-4-review stop:s-4-scan"),
            ("next", &format!("{plan}#undo-n-cause-wallcap")),
        ]),
        "",
    )
    .await;
    assert_eq!(
        (status, location.as_str()),
        (
            StatusCode::SEE_OTHER,
            format!("{plan}#undo-n-cause-wallcap").as_str()
        )
    );
    let (_, _, _, html) = browse(&router, &plan, None, "").await;
    assert_eq!(plan_html::place(&html, "a-4"), "", "{html}");
    let well = plan_html::band(&html, "plan-needs");
    assert!(
        well.contains(
            "<span>Set aside: the failures of two units (they hit their wall-clock cap)</span>"
        ) && well.contains("id=\"undo-n-cause-wallcap\""),
        "{well}"
    );
    assert!(
        html.contains("<title>(1) ✕2 · almanac · sluice</title>"),
        "{html}"
    );
    // one of them changes: a retry ran and failed again (its work generation moved on)
    let id = n.almanac;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET work_generation=work_generation+1 WHERE project_id=?1 AND step_id='a-4-review'",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    let (_, _, _, html) = browse(&router, &plan, None, "").await;
    assert_eq!(plan_html::place(&html, "a-4"), "stopped", "{html}");
    assert!(!plan_html::band(&html, "plan-needs").contains("Set aside: two units"));
    assert!(
        html.contains("<title>(1) ✕3 · almanac · sluice</title>"),
        "{html}"
    );
    // Set all aside sets every item aside; Undo brings one back
    let keys = value(&html, "name=\"items\" value=\"").to_owned();
    assert!(
        keys.starts_with("ask:") && keys.contains("quiet:s-3-scan@"),
        "{keys}"
    );
    let (status, _, _, _) = browse(
        &router,
        &format!("{plan}/needs"),
        Some(&[("action", "dismiss"), ("items", &keys), ("next", &plan)]),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, _, _, html) = browse(&router, &plan, None, "").await;
    assert!(plan_html::needs(&html).is_empty(), "{html}");
    // the tab still counts the open question (the inbox holds it), no longer the stops
    assert!(
        html.contains("<title>(1) almanac · sluice</title>"),
        "{html}"
    );
    assert!(html.contains("<p class=\"nd-none\">Nothing.</p>"), "{html}");
    // setting aside is about attention alone: the head still says what is true (the failures,
    // the cancel, which Set all aside never settles, and the silence), and the cancelled unit
    // is still a stop, never a row
    let said = plan_html::summary(&html);
    assert!(
        said.contains("2 failed, 1 cancelled, 1 quiet") && said.contains("5 of 16 done"),
        "{said}"
    );
    assert!(
        !plan_html::band(&html, "plan-roster").contains("data-unit=\"a-5\""),
        "{html}"
    );
    let (status, _, _, _) = browse(
        &router,
        &format!("{plan}/needs"),
        Some(&[
            ("action", "undo"),
            ("items", "stop:a-4-review stop:s-4-scan"),
            ("next", &plan),
        ]),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, _, _, html) = browse(&router, &plan, None, "").await;
    assert_eq!(plan_html::place(&html, "a-4"), "stopped", "{html}");
}

/// The bug this catches: Finished since you looked forgetting where this browser looked (or
/// sharing it), and Mark as seen needing script.
#[tokio::test]
async fn the_read_cursor_is_this_browsers_and_mark_as_seen_moves_it() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let router = f.router();
    let plan = format!("/projects/id/{}", n.almanac);
    let (_, _, _, html) = browse(&router, &plan, None, "").await;
    let cards = plan_html::band(&html, "plan-finished");
    assert!(cards.contains("Finished lately"), "{cards}");
    // Mark as seen, a plain form: the cookie, then back to the plan
    let through = value(cards, "name=\"through\" value=\"").to_owned();
    let (status, location, cookie, _) = browse(
        &router,
        &format!("{plan}/seen"),
        Some(&[("through", &through)]),
        "",
    )
    .await;
    assert_eq!(
        (status, location.as_str()),
        (StatusCode::SEE_OTHER, plan.as_str())
    );
    assert!(
        cookie.starts_with(&format!("sluice_seen_{}={through}; Path=/;", n.almanac)),
        "{cookie}"
    );
    let jar = cookie.split(';').next().unwrap().to_owned();
    // with it, nothing has finished since; without it, another browser still sees the cards
    let (_, _, _, html) = browse(&router, &plan, None, &jar).await;
    assert!(
        html.contains("<!--r:plan-finished--><div id=\"plan-finished\" hidden></div>"),
        "{html}"
    );
    let (_, _, _, other) = browse(&router, &plan, None, "").await;
    assert!(plan_html::band(&other, "plan-finished").contains("Finished lately"));
    // a look kept earlier: what finished after it, said since when
    let a2 = "a-2";
    let (_, _, _, html) = browse(
        &router,
        &plan,
        None,
        &format!("sluice_seen_{}=1", n.almanac),
    )
    .await;
    let cards = plan_html::band(&html, "plan-finished");
    assert!(
        cards.contains(
            "<h2 id=\"finished\">Finished since you looked</h2><span class=\"pb-note\">since "
        ) && cards.matches("<li class=\"fin-i\"").count() == 5
            && cards.contains(&format!("/units/{a2}\"")),
        "{cards}"
    );
}

/// The bugs this catches: a big roster drawing every healthy group open, and folding a group
/// whose units failed together (a group of stops names no one unit) or fell silent.
#[tokio::test]
async fn a_big_roster_folds_its_unremarkable_groups() {
    let f = Fixture::new().await;
    let atlas = neutral::seed_big(&f.writer, f._home.path()).await;
    let (_, html) = f.get(&format!("/projects/id/{atlas}")).await;
    let roster = plan_html::band(&html, "plan-roster");
    // entry, healthy, folds to its line, its rows still drawn for a click (or no script)
    let entry = &roster[at(
        roster,
        "<details class=\"ro-g\"><summary class=\"ro-group\"><h3><b><a ",
    )..];
    assert!(
        entry.contains(">entry</a></b> · 25 running · each heard from in the last ")
            && roster.matches("<details id=\"u-e-").count() == 25,
        "{roster}"
    );
    // article (two failures at their cap, said together) and scan (a silence) stay open
    assert_eq!(
        roster.matches("<details class=\"ro-g\" open>").count(),
        2,
        "{roster}"
    );
    assert_eq!(
        roster.matches("<details class=\"ro-g\">").count(),
        1,
        "{roster}"
    );
    // a small plan leaves every group open
    let (_, html) = f.get(&format!("/projects/id/{}", f.id)).await;
    let roster = plan_html::band(&html, "plan-roster");
    assert!(
        !roster.contains("<details class=\"ro-g\"><summary"),
        "{roster}"
    );
}
