//! The project page's plan on the neutral fixture (DESIGN.md, The plan): its head's sentence,
//! Needs you (a question, the stops, stops sharing a cause as one, a silence), what finished
//! lately, At work (a group a recipe, no recipe and the long runs after, the longest silent
//! first) and the steps each row opens to; what the owner sets aside comes back
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
            // a row open, its steps asked for and drawn
            browser
                .eval("(() => { const row = document.querySelector('.ro-u'); if (row) row.open = true; })()")
                .unwrap();
            browser
                .wait("!document.querySelector('.ro-u') || document.querySelector('.ro-u .ro-drill sluice-steps')")
                .unwrap();
            // the tracks' places are the page's clock's (the server's version leaves them
            // out): each moved off, the next tick puts it back
            let moved = browser
                .eval("(() => { const bars = [...document.querySelectorAll('[data-log] i[data-a]')]; for (const b of bars) b.style.setProperty('--l', '-1%'); document.querySelector('main').append(document.createElement('i')); return bars.length; })()")
                .unwrap();
            assert!(slug != "almanac" || moved.as_u64().unwrap() > 0, "{slug}: {moved}");
            browser
                .wait("[...document.querySelectorAll('[data-log] i[data-a]')].every(b => b.style.getPropertyValue('--l') !== '-1%')")
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
        let html = f.opened(&format!("/projects/id/{}", n.almanac)).await;
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
        // the sections in order: Needs you, At work, Finished lately; no Done index unasked
        let order = [
            "<!--r:plan-needs-->",
            "<!--r:plan-roster-->",
            "<!--r:plan-finished-->",
            "<!--r:plan-done-->",
        ];
        for pair in order.windows(2) {
            assert!(at(&html, pair[0]) < at(&html, pair[1]), "{pair:?}");
        }
        assert!(!html.contains("class=\"pl-idx\""), "{html}");
        // At work says how to read its tracks behind its ⓘ
        let roster = &html[at(&html, "<!--r:plan-roster-->")..at(&html, "<!--r:plan-finished-->")];
        assert!(
            roster.contains("<summary class=\"dm-b\" aria-label=\"How to read At work\""),
            "{roster}"
        );
        assert!(
            roster.contains("its last 12 hours on a log scale"),
            "{roster}"
        );
        // a running step's Overview: Now, then its brief (its spec's first paragraph, the whole
        // in Inputs) and what it has set so far (nothing yet, Outputs one click away)
        let (_, step) = f
            .get(&format!("/projects/id/{}/steps/survey", n.almanac))
            .await;
        let (now, brief, so_far) = (
            at(&step, "id=\"ov-now-h\""),
            at(&step, "id=\"ov-brief-h\""),
            at(&step, "id=\"ov-sofar-h\""),
        );
        assert!(now < brief && brief < so_far, "{step}");
        assert!(step.contains("<a class=\"meta\" href=\"#tp-inputs\" data-tab-to=\"inputs\">Whole brief</a></div><p class=\"brief-text\">Go through the archive&#39;s photographs a folder at a time"), "{step}");
        assert!(
            !step[brief..so_far].contains("Write the mismatches"),
            "{step}"
        );
        assert!(
            step[so_far..].contains("<p class=\"meta d-unset\">Not set yet: <code>mismatches</code> <span class=\"ty\">int</span>, <code>report</code> <span class=\"ty\">string</span></p>"),
            "{step}"
        );
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
            cause.contains("Two units failed: they hit their wall-clock cap. <span class=\"nd-next\">Give the agents more time: set <code>SLUICE_AGENT_MAX_MIN</code>, their limit in minutes, in the project's <code>.env</code> file, then Retry all.</span>")
                && cause.contains("<summary>Show the two</summary>")
                && cause.contains(">Retry all</button>")
                && cause.contains("value=\"stop:a-4-review stop:s-4-scan\""),
            "{cause}"
        );
        for item in &needs {
            assert_eq!(
                item.matches("<button class=\"icon\" aria-label=\"Set aside")
                    .count(),
                1,
                "{item}"
            );
        }
        assert!(
            needs[0].contains("<span class=\"nk nk-ask\" role=\"img\" aria-label=\"Question\">")
        );
        assert!(
            needs[3].contains(
                "<span class=\"nk\"><span class=\"g g-quiet\" role=\"img\" aria-label=\"quiet\">"
            ),
            "{}",
            needs[3]
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
            html.contains("<title>(1) ⚠4 · almanac · sluice</title>"),
            "{html}"
        );
        assert!(html.contains("href=\"/static/favicon-ask.svg?v="), "{html}");
        // Finished lately, with no look kept: the latest four, newest first, by title; a card
        // for the one that said what it produced, the three that left nothing in one line
        let cards = plan_html::band(&html, "plan-finished");
        assert!(
            cards.contains("<h2 id=\"finished\">Finished lately</h2>"),
            "{cards}"
        );
        assert_eq!(cards.matches("<li class=\"fin-i\"").count(), 1, "{cards}");
        assert!(
            cards.contains("<p class=\"fin-v\">Fourteen entries")
                && cards.contains("<p class=\"fin-quiet\">3 more finished with nothing to say: <a"),
            "{cards}"
        );
        assert!(
            at(cards, "/units/a-3\"") < at(cards, "/units/s-2\""),
            "{cards}"
        );
        // At work: a group a recipe, then no recipe, the long runs last; the longest silent
        // first; the axis's labels over the tracks
        let roster = plan_html::band(&html, "plan-roster");
        assert!(
            at(roster, ">article</a></b>") < at(roster, ">scan</a></b>")
                && at(roster, ">scan</a></b>") < at(roster, "<b>Other units</b>")
                && at(roster, "<b>Other units</b>") < at(roster, "<b>Long runs</b>"),
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
        // a row: its state in words beside its mark, how long it has run, when it was heard
        // from (its line says what it said, not again when), its track a tick a message
        let a6 = plan_html::row(&html, "a-6");
        assert!(
            a6.contains("<span class=\"vh\">running, </span>draft · ")
                && a6.contains("class=\"ro-heard\"><span class=\"dot run pulse\" aria-hidden=\"true\"></span>heard <time data-ago=")
                && a6.contains("<b class=\"uv-from\">draft</b>: Entries 41 to 58")
                && a6.contains("aria-label=\"Its last twelve hours, on a log scale: 3 messages\""),
            "{a6}"
        );
        assert_eq!(a6.matches(" class=\"tk-m\"></i>").count(), 3, "{a6}");
        // quiet: one sand pill, its stage and how long it has been silent
        let s3 = plan_html::row(&html, "s-3");
        assert!(
            s3.contains("<span class=\"pill look\"><span class=\"g g-quiet\"")
                && s3.contains("</span>scan · quiet <time"),
            "{s3}"
        );
        // nothing heard since its run began: said once
        let index = plan_html::row(&html, "index");
        assert!(
            index.contains("<span class=\"ro-heard\">no word yet</span>"),
            "{index}"
        );
        assert!(s3.contains(" class=\"tk-quiet\"></i>"), "{s3}");
        // the drilldown: a disclosure whose body is the unit's steps on its
        // axis, the current one marked, each step what it runs on, then its latest and actions
        assert!(
            a6.starts_with(&format!("<details id=\"u-a-6\" class=\"ro-u\" data-unit=\"a-6\" data-said=\"a-6-draft running|a-6-review pending|a-6-publish pending\" data-preserve-attr=\"open\" data-steps=\"a-6-draft a-6-review a-6-publish\" data-on:toggle=\"el.open &amp;&amp; @get('/projects/id/{}/units/a-6/drill')\"><summary class=\"ro-r\">", n.almanac)),
            "{a6}"
        );
        let drill = &a6[at(
            a6,
            "<div id=\"drill-a-6\" class=\"ro-drill\" data-ignore-morph>",
        )..];
        assert!(drill.contains("<sluice-steps class=\"sv\""), "{drill}");
        assert!(
            plan_html::cell(drill, "a-6-draft").starts_with("<li class=\"sv-row sv-c-run is-cur\""),
            "{drill}"
        );
        // its draft runs on an engine, its other steps on the fn: a column says which
        assert!(
            drill.contains("<div class=\"sv-g sv-no-pr\"")
                && drill.contains("<span class=\"sv-h-on\" aria-hidden=\"true\">Runs on</span>")
                && !drill.contains(">Progress<"),
            "{drill}"
        );
        // its foot: when it was last heard from (its row says what), Open step the one move,
        // the rest quiet links
        assert!(
            drill.contains("<p class=\"ro-ev\">Last heard ")
                && !drill.contains("Entries 41 to 58")
                && drill.contains("<span class=\"ro-links\"><a href=")
                && drill.contains(">Message</a>")
                && drill.contains(" data-step=\"a-6-draft\">Open step</a>"),
            "{drill}"
        );
        // a unit of no recipe: in its step view its two gathers fan in to its merge
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
        assert!(
            plan_html::band(&html, "plan-needs")
                .contains("<p class=\"nd-none\">Nothing needs you.</p>")
        );
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
/// A plain request for a page, as a link or a reload makes it: its status, where it is sent, the cookie it is given
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
    // × on the two stops at their cap
    let (status, _, _, _) = browse(
        &router,
        &format!("{plan}/needs"),
        Some(&[
            ("action", "dismiss"),
            ("items", "stop:a-4-review stop:s-4-scan"),
        ]),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
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
        html.contains("<title>(1) ⚠2 · almanac · sluice</title>"),
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
        html.contains("<title>(1) ⚠3 · almanac · sluice</title>"),
        "{html}"
    );
    // Set all aside sets every stop and silence aside, the question keeping its own ×;
    // Undo brings one back
    let keys = value(&html, "name=\"items\" value=\"").to_owned();
    assert!(
        keys.starts_with("stop:") && keys.contains("quiet:s-3-scan@") && !keys.contains("ask:"),
        "{keys}"
    );
    assert!(html.contains(">Set all aside</button>"), "{html}");
    let (status, _, _, _) = browse(
        &router,
        &format!("{plan}/needs"),
        Some(&[("action", "dismiss"), ("items", &keys)]),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, _, _, html) = browse(&router, &plan, None, "").await;
    assert_eq!(plan_html::needs(&html).len(), 1, "{html}");
    // the tab still counts the open question, no longer the stops
    assert!(
        html.contains("<title>(1) almanac · sluice</title>"),
        "{html}"
    );
    // the question set aside by its own ×: the well one muted line
    let ask = value(&html, "name=\"items\" value=\"").to_owned();
    assert!(ask.starts_with("ask:"), "{ask}");
    browse(
        &router,
        &format!("{plan}/needs"),
        Some(&[("action", "dismiss"), ("items", &ask)]),
        "",
    )
    .await;
    let (_, _, _, html) = browse(&router, &plan, None, "").await;
    assert!(
        html.contains("<p class=\"nd-none\">Nothing needs you. 4 set aside.</p>"),
        "{html}"
    );
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
        ]),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, _, _, html) = browse(&router, &plan, None, "").await;
    assert_eq!(plan_html::place(&html, "a-4"), "stopped", "{html}");
}

/// The bug this catches: Finished since you looked forgetting where this browser looked (or
/// sharing it).
#[tokio::test]
async fn the_read_cursor_is_this_browsers_and_mark_as_seen_moves_it() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let router = f.router();
    let plan = format!("/projects/id/{}", n.almanac);
    let (_, _, _, html) = browse(&router, &plan, None, "").await;
    let cards = plan_html::band(&html, "plan-finished");
    assert!(cards.contains("Finished lately"), "{cards}");
    // Mark as seen: the look kept in this browser's cookie (the page's stream draws it at once
    // from its `seen` signal)
    let through = value(cards, "name=\"through\" value=\"").to_owned();
    let (status, _, cookie, _) = browse(
        &router,
        &format!("{plan}/seen"),
        Some(&[("through", &through)]),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        cookie.starts_with(&format!("sluice_seen_{}={through}; Path=/;", n.almanac)),
        "{cookie}"
    );
    let jar = cookie.split(';').next().unwrap().to_owned();
    // with it, nothing has finished since: the line says through when, with its Undo (the look
    // taken back, `through` 0); without it, another browser still sees the cards
    let (_, _, _, html) = browse(&router, &plan, None, &jar).await;
    let seen = plan_html::band(&html, "plan-finished");
    assert!(
        seen.contains("<h2 id=\"finished\">Finished lately</h2>")
            && seen.contains("<p class=\"seen-line\">Seen through <time data-clock")
            && seen.contains("· <button class=\"quiet\">Undo</button></p>")
            && seen.contains("<input type=\"hidden\" name=\"through\" value=\"0\">")
            && !seen.contains("class=\"fin-i\""),
        "{seen}"
    );
    let (_, _, _, undone) = browse(
        &router,
        &plan,
        None,
        &format!("sluice_seen_{}=0", n.almanac),
    )
    .await;
    assert!(plan_html::band(&undone, "plan-finished").contains("class=\"fin-i\""));
    let (_, _, _, other) = browse(&router, &plan, None, "").await;
    assert!(plan_html::band(&other, "plan-finished").contains("class=\"fin-i\""));
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
            "<h2 id=\"finished\">Finished since you looked</h2><span class=\"sec-note\">since "
        ) && cards.matches("<li class=\"fin-i\"").count() == 2
            && cards.contains("3 more finished with nothing to say")
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
    // entry and article, healthy (article's failures are in Needs you, not on the roster),
    // fold whole to their lines, their rows still drawn for a click
    assert!(
        roster.contains(">entry</a></b> · 25 running · each heard from in the last ")
            && roster.contains(">article</a></b> · 23 running · each heard from in the last ")
            && roster.matches("<details id=\"u-e-").count() == 25,
        "{roster}"
    );
    assert_eq!(
        roster.matches("<details class=\"ro-g\"><summary").count(),
        2,
        "{roster}"
    );
    // scan: its quiet row in sight, first, the seven healthy folded to one line under it
    let scan = &roster[at(roster, ">scan</a></b> · 1 quiet, 7 running</h3>")..];
    assert!(
        at(scan, "<!--r:u-c-50-->")
            < at(
                scan,
                "<details class=\"qf ro-fold\"><summary>+7 running · each heard from in the last "
            )
            && scan.matches("<details id=\"u-c-").count() == 8,
        "{scan}"
    );
    // a finished card with no output says the unit's last message
    let cards = plan_html::band(&html, "plan-finished");
    assert!(
        cards.contains("<p class=\"fin-v\">Checked against the regional list"),
        "{cards}"
    );
    // a small plan leaves every group open
    let (_, html) = f.get(&format!("/projects/id/{}", f.id)).await;
    let roster = plan_html::band(&html, "plan-roster");
    assert!(
        !roster.contains("<details class=\"ro-g\"><summary"),
        "{roster}"
    );
}

/// The bug this catches: Mark as seen set the page's look before sluice took it, so a mark that
/// never arrived still emptied Finished; and what it left stacked a word a line on a phone.
/// Now the answer sets the `seen` signal, the stream reopens on it, the section keeps its head
/// and says "Seen through … · Undo" on one line, and Undo brings the cards back.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_mark_as_seen_and_its_undo_go_through_the_answer() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let mut browser =
            Chrome::open(&format!("http://{addr}/projects/id/{}", n.almanac)).unwrap();
        browser.viewport(390, "light").unwrap();
        browser
            .wait("document.readyState === 'complete' && !!document.querySelector('#plan-finished .fin-i')")
            .unwrap();
        browser
            .eval("document.querySelector('#mark-seen button').click()")
            .unwrap();
        browser
            .wait("!!document.querySelector('#plan-finished .seen-line')")
            .unwrap();
        let line = browser
            .eval("(() => { const l = document.querySelector('#plan-finished .seen-line'); return {h: l.getBoundingClientRect().height, text: l.textContent, head: document.querySelector('#plan-finished h2').textContent, said: document.querySelector('#mark-seen-said').textContent}; })()")
            .unwrap();
        assert!(line["h"].as_f64().unwrap() < 30.0, "{line}");
        assert!(
            line["text"].as_str().unwrap().starts_with("Seen through ")
                && line["text"].as_str().unwrap().ends_with(" · Undo"),
            "{line}"
        );
        assert_eq!(line["head"], "Finished lately", "{line}");
        assert_eq!(line["said"], "", "{line}");
        browser
            .eval("document.querySelector('#plan-finished .seen-line button').click()")
            .unwrap();
        browser
            .wait("!!document.querySelector('#plan-finished .fin-i')")
            .unwrap();
        assert_eq!(
            browser.eval("window.browserErrors ?? []").unwrap(),
            serde_json::json!([])
        );
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}
