//! The project page's plan on the neutral fixture (DESIGN.md, The plan): its head's sentence,
//! Needs you (a question, the stops, stops sharing a cause as one, a silence), what finished
//! lately, At work (a group a recipe, no recipe and the long runs after, creation order
//! by default) and the steps each row opens to; what the owner sets aside comes back
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
  const out = [...sheet.querySelectorAll('section')].filter(e => e.checkVisibility()).filter(b => { const r = b.getBoundingClientRect(); return r.left < box.left - 1 || r.right > box.right + 1; }).map(b => b.id);
  const head = document.querySelector('.plan-band .page-line').getBoundingClientRect();
  return {scroll: document.documentElement.scrollWidth - document.documentElement.clientWidth,
          out, edge: Math.abs(head.left - box.left) < 1,
          clipped: [...sheet.querySelectorAll('.ro-t, .nd-l, .fin-t, .wt, .track')].filter(e => e.getBoundingClientRect().right > document.documentElement.clientWidth + 0.5).length,
          errors: window.browserErrors};
})()"#;
        for (project, slug) in [(n.almanac, "almanac"), (n.chores, "chores"), (lanes, "both")] {
            browser.navigate(&format!("{base}/projects/id/{project}")).unwrap();
            browser
                .wait("document.readyState === 'complete' && document.querySelector('.plan-sheet')")
                .unwrap();
            // a row open, its conversation asked for and drawn
            browser
                .eval("(() => { const row = document.querySelector('.ro-u'); if (row) row.open = true; })()")
                .unwrap();
            browser
                .wait("!document.querySelector('.ro-u') || document.querySelector('.ro-u .ro-drill .ro-foot')")
                .unwrap();
            // the strips' places are the page's clock's (the server's version leaves them
            // out): each moved off, the next tick puts it back
            let moved = browser
                .eval("(() => { const bars = [...document.querySelectorAll('[data-pulse] i[data-a]')]; for (const b of bars) b.style.setProperty('--l', '-1%'); document.querySelector('main').append(document.createElement('i')); return bars.length; })()")
                .unwrap();
            assert!(slug != "almanac" || moved.as_u64().unwrap() > 0, "{slug}: {moved}");
            browser
                .wait("[...document.querySelectorAll('[data-pulse] i[data-a]')].every(b => b.style.getPropertyValue('--l') !== '-1%')")
                .unwrap();
            for width in [390, 1440, 2560] {
                for theme in ["light", "dark"] {
                    browser.viewport(width, theme).unwrap();
                    // Use the real preference signal too: a live patch re-applies that
                    // signal, so changing only the DOM attribute can capture light twice.
                    let stock = if theme == "light" { "paper" } else { "carbon" };
                    browser.eval(&format!("document.querySelector('input[name=theme][value={stock}]').click()")).unwrap();
                    browser.wait(&format!("document.documentElement.dataset.theme === '{stock}'")).unwrap();
                    let g = browser.eval(LOOK).unwrap();
                    let label = format!("{slug} {width} {theme}");
                    assert_eq!(g["scroll"], 0, "{label}: sideways {g}");
                    assert_eq!(g["out"], serde_json::json!([]), "{label}: a section out of the sheet {g}");
                    assert_eq!(g["edge"], true, "{label}: the head and the sheet share a left edge {g}");
                    assert_eq!(g["clipped"], 0, "{label}: {g}");
                    assert_eq!(g["errors"], serde_json::json!([]), "{label}: {g}");
                    if let Some(dir) = &screens {
                        std::fs::create_dir_all(dir).unwrap();
                        browser.eval("Promise.all(document.getAnimations().filter(a => a.effect?.getComputedTiming().iterations !== Infinity).map(a => a.finished.catch(() => {})))").unwrap();
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
        // finished as plain rows; units not begun only counted
        let mut want: Vec<(&str, &str)> = vec![("a-7", "asks"), ("a-1", "done")];
        want.extend(neutral::STOPPED.iter().map(|u| (*u, "stopped")));
        want.extend(neutral::WAITING.iter().map(|u| (*u, "")));
        want.extend(["a-2", "a-3", "s-1", "s-2"].iter().map(|u| (*u, "done")));
        want.extend(["a-6", "s-3", "index"].iter().map(|u| (*u, "running")));
        // a long run is an ordinary row, in the final group
        want.push(("survey", "running"));
        for (unit, place) in want {
            assert_eq!(plan_html::place(&html, unit), place, "{unit}");
        }
        // the sections in order: Needs you, At work, then Finished; no Done index unasked
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
        let roster = plan_html::band(&html, "plan-roster");
        assert!(!roster.contains("How to read At work"));
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
        // what to try), the cancel, the silence; each its actions and one ×
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
        let kinds: Vec<&str> = needs.iter().map(|i| plan_html::kind(i)).collect();
        assert_eq!(kinds, ["ask", "fail", "mute", "quiet"]);
        let cause = needs[1];
        assert!(
            cause.contains("<span class=\"nd-t\">Two units</span> <span class=\"nd-s\">failed: they hit their wall-clock cap</span>")
                && cause.contains("<span class=\"nd-next\">Agent runs now have no time limit. Retry all to use the current release.</span>")
                && cause.contains("<summary>Show the two</summary>")
                && cause.contains(">Retry all</button>")
                && cause.contains("value=\"stop:a-4-review@"),
            "{cause}"
        );
        for item in &needs {
            assert_eq!(
                item.matches("<button class=\"x\" aria-label=\"Set aside")
                    .count(),
                1,
                "{item}"
            );
        }
        // the question: its words beside the asker's avatar, answered in place, Answer filled
        let ask = needs[0];
        assert!(
            ask.contains("<span class=\"nd-s\">asks you</span>")
                && ask.contains("<div class=\"nd-q\"><span class=\"av ")
                && ask.contains("class=\"answer-in\"")
                && ask.contains("<textarea ")
                && ask.contains(" data-answer ")
                && ask.contains("<button class=\"primary\">Answer</button>"),
            "{ask}"
        );
        assert!(needs[3].contains(">Message it</a>"), "{}", needs[3]);
        // the head's sentence, from the counts alone
        let summary = plan_html::summary(&html);
        assert!(
            summary.contains(
                "1 question for you</a> · 2 failed · 1 cancelled · 1 quiet · 5 at work · 5 of 16 done"
            ),
            "{summary}"
        );
        // the tab says it too, and its icon is the question's
        assert!(
            html.contains("<title>(1) ⚠4 · almanac · sluice</title>"),
            "{html}"
        );
        assert!(html.contains("href=\"/static/favicon-ask.svg?v="), "{html}");
        let long = plan_html::row(&html, "survey");
        assert!(
            long.contains("checked 1240 · remaining 310") && !long.contains("class=\"live quiet\""),
            "{long}"
        );
        let cards = plan_html::band(&html, "plan-finished");
        assert!(
            cards.contains("<h2 id=\"finished\">Finished lately</h2>"),
            "{cards}"
        );
        assert_eq!(cards.matches("<li class=\"fin-r\"").count(), 5, "{cards}");
        assert!(
            !cards.contains("fin-v")
                && !cards.contains("Fourteen entries")
                && !cards.contains("removed")
                && !cards.contains("discarded"),
            "{cards}"
        );
        assert!(
            at(cards, "/units/a-3\"") < at(cards, "/units/s-2\""),
            "{cards}"
        );
        // At work: a group a recipe, then no recipe; creation order by default; the window's
        // labels over the strips; what waits and what is done in a line under it
        assert!(
            at(roster, ">article</a></b>") < at(roster, ">scan</a></b>")
                && at(roster, ">scan</a></b>") < at(roster, "<b>Other units</b>")
                && at(roster, "<b>Other units</b>") < at(roster, "<b>Long runs</b>"),
            "{roster}"
        );
        assert!(at(roster, "<!--r:u-a-6-->") < at(roster, "<!--r:u-a-7-->"));
        assert!(
            !roster.contains("<span>1h ago</span>") && !roster.contains("<span>now</span>"),
            "{roster}"
        );
        assert!(
            roster.contains("3 waiting on earlier work · <a href=")
                && roster.contains(">5 done</a>, the last <time"),
            "{roster}"
        );
        // a row: the engine it runs on, its title, its stages (the current one green and how
        // long, the next, the rest folded), when it was heard from, what it said last; its
        // strip a tall mark a message
        let a6 = plan_html::row(&html, "a-6");
        assert!(
            a6.contains("<span class=\"engine\" title=\"Runs on claude\">claude</span>")
                && a6.contains(
                    "<span class=\"ro-t\" title=\"Terns: the spring guide&#39;s entries\">"
                )
                && a6.contains("class=\"current-stage run\" data-state=\"running\"")
                && a6.contains("data-step=\"a-6-draft\">draft ")
                && !a6.contains("fold-n")
                && a6.contains("class=\"heard\">· heard <time data-ago=")
                && a6.contains("<b class=\"uv-from\">draft</b>: Entries 41 to 58")
                && a6.contains("aria-label=\"Its pulse: acted in ")
                && a6.contains(" minutes of its run, 3 messages lately\""),
            "{a6}"
        );
        let strip = &a6[at(a6, "class=\"ro-track\"")..at(a6, "<div id=\"drill-a-6\"")];
        assert_eq!(
            strip.matches(" class=\"pk pk-m\"></i>").count(),
            3,
            "{strip}"
        );
        // quiet: its stage says so and for how long, the dot beside it ochre
        let s3 = plan_html::row(&html, "s-3");
        assert!(
            s3.contains("class=\"current-stage quiet\" data-state=\"quiet\"")
                && s3.contains("scan quiet <time")
                && s3.contains("<span class=\"live quiet\""),
            "{s3}"
        );
        assert!(s3.contains(" class=\"pk-tail is-quiet\"></i>"), "{s3}");
        // nothing heard since its run began: said once
        let index = plan_html::row(&html, "index");
        assert!(
            index.contains("<span class=\"heard\">no word yet</span>"),
            "{index}"
        );
        // opened, a row is its conversation, oldest first, each message with its sender's
        // avatar, then when it was last heard from and its ways on; no step table
        assert!(
            a6.starts_with(&format!("<details id=\"u-a-6\" class=\"ro-u\" data-unit=\"a-6\" data-said=\"a-6-draft running|a-6-review pending|a-6-publish pending\" data-preserve-attr=\"open data-flash\" data-steps=\"a-6-draft a-6-review a-6-publish\" data-on:toggle=\"el.open &amp;&amp; @get('/projects/id/{}/units/a-6/drill')\"><summary class=\"ro-r\">", n.almanac)),
            "{a6}"
        );
        let drill = &a6[at(
            a6,
            "<div id=\"drill-a-6\" class=\"ro-drill\" data-ignore-morph>",
        )..];
        assert!(
            !drill.contains("sluice-steps") && !drill.contains("sv-row"),
            "{drill}"
        );
        let said: Vec<usize> = [
            "Entries 1 to 20 drafted.",
            "Entries 21 to 40 drafted.",
            "Entries 41 to 58 drafted;",
        ]
        .iter()
        .map(|w| at(drill, w))
        .collect();
        assert!(said.windows(2).all(|p| p[0] < p[1]), "{drill}");
        assert!(
            drill.contains("<div class=\"msg\"><div class=\"by\"><span class=\"av agent ")
                && drill.contains("<b class=\"agent\">draft</b><span>to @lead · "),
            "{drill}"
        );
        let foot = &drill[at(drill, "<div class=\"ro-foot\">")..];
        assert!(
            foot.contains("<span>Last heard ")
                && foot.contains(">Message</a>")
                && foot.contains(">Unit page</a>")
                && foot.contains(" data-step=\"a-6-draft\">Open step</a>"),
            "{foot}"
        );
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
            plan_html::summary(&html).contains("1 at work · 1 of 3 done"),
            "{html}"
        );
        let needs = plan_html::band(&html, "plan-needs");
        assert!(
            needs.contains("hidden") && !needs.contains("Needs you"),
            "{needs}"
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
    // Needs you is then only its Undo, and the lead says so first
    let said = plan_html::summary(&html);
    assert!(
        !said.contains("question for you") && !said.contains("failed"),
        "{said}"
    );
    let needs = plan_html::band(&html, "plan-needs");
    assert!(
        !needs.contains("<h2")
            && needs.contains(
                "<ul class=\"nd-gone\" role=\"status\"><li><span>Set aside: the question “"
            )
            && needs.matches(">Undo</button>").count() == 4,
        "{needs}"
    );
    // Setting aside removes the attention counts; the cancelled unit remains unfinished.
    let said = plan_html::summary(&html);
    assert!(
        !said.contains("failed") && said.contains("5 of 16 done"),
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
            && seen
                .contains("<h3>Nothing new since you looked</h3><p>Seen through <time data-clock")
            && seen.contains(" · <button class=\"quiet\">Undo</button></p>")
            && seen.contains("<input type=\"hidden\" name=\"through\" value=\"0\">")
            && !seen.contains("class=\"fin-r\""),
        "{seen}"
    );
    let (_, _, _, undone) = browse(
        &router,
        &plan,
        None,
        &format!("sluice_seen_{}=0", n.almanac),
    )
    .await;
    assert!(plan_html::band(&undone, "plan-finished").contains("class=\"fin-r\""));
    let (_, _, _, other) = browse(&router, &plan, None, "").await;
    assert!(plan_html::band(&other, "plan-finished").contains("class=\"fin-r\""));
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
        cards.contains("<h2 id=\"finished\">Finished since you looked</h2>")
            && cards.matches("<li class=\"fin-r\"").count() == 5
            && !cards.contains("fin-v")
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
        roster.contains(">25 running · each heard from in the last ")
            && roster.contains(">23 running · each heard from in the last ")
            && roster.matches("<details id=\"u-e-").count() == 25,
        "{roster}"
    );
    assert_eq!(
        roster
            .matches("data-preserve-attr=\"open\"><summary class=\"ro-group\">")
            .count(),
        2,
        "two groups folded: {roster}"
    );
    assert_eq!(
        roster
            .matches("data-preserve-attr=\"open\" open><summary class=\"ro-group\">")
            .count(),
        1,
        "{roster}"
    );
    // scan: its quiet row in sight, first, the seven healthy folded to one line under it
    let scan = &roster[at(roster, ">scan</a></b>")..];
    assert!(
        at(scan, "<!--r:u-c-50-->") < at(scan, "<details class=\"qf ro-fold\"")
            && scan.matches("<details id=\"u-c-").count() == 8,
        "{scan}"
    );
    // a finished card with no output says the unit's last message
    let cards = plan_html::band(&html, "plan-finished");
    assert!(
        cards.contains("class=\"fin-r\"") && !cards.contains("Checked against the regional list"),
        "{cards}"
    );
    // a small plan leaves every group open
    let (_, html) = f.get(&format!("/projects/id/{}", f.id)).await;
    let roster = plan_html::band(&html, "plan-roster");
    assert!(
        !roster.contains("data-preserve-attr=\"open\"><summary class=\"ro-group\">"),
        "{roster}"
    );
}

/// The bug this catches: Mark as seen set the page's look before sluice took it, so a mark that
/// never arrived still emptied Finished; and what it left stacked a word a line on a phone.
/// Now the answer sets the `seen` signal, the stream reopens on it, the section keeps its head
/// and draws its empty state, "Seen through … · Undo" on one line, and Undo brings the rows
/// back.
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
            .wait("document.readyState === 'complete' && !!document.querySelector('#plan-finished .fin-r')")
            .unwrap();
        browser
            .eval("document.querySelector('#mark-seen button').click()")
            .unwrap();
        browser
            .wait("!!document.querySelector('#plan-finished .state p')")
            .unwrap();
        let line = browser
            .eval("(() => { const l = document.querySelector('#plan-finished .state p'); return {h: l.getBoundingClientRect().height, text: l.textContent, head: document.querySelector('#plan-finished h2').textContent, said: document.querySelector('#mark-seen-said').textContent}; })()")
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
            .eval("document.querySelector('#plan-finished .state p button').click()")
            .unwrap();
        browser
            .wait("!!document.querySelector('#plan-finished .fin-r')")
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

/// Regression: agent writes reordered the roster and its whole-region morph reset group and
/// quiet folds; moving a unit into another group also lost its open drilldown. Counts alone
/// must patch their own text. This drives the real page, stream, fold memory and sort cookie.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_plan_folds_and_order_survive_activity_and_group_moves() {
    use serde_json::json;
    let f = Fixture::new().await;
    let project = neutral::seed_big(&f.writer, f._home.path()).await;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let url = format!("http://{addr}/projects/id/{project}");
    let mut browser = tokio::task::spawn_blocking(move || {
        let mut b = Chrome::open("about:blank").unwrap();
        b.send("Page.addScriptToEvaluateOnNewDocument", json!({"source": "window.patches=[]; document.addEventListener('datastar-fetch', e => { if(e.detail.type==='datastar-patch-signals' && JSON.parse(e.detail.argsRaw?.signals??'{}').rel) window.streamReady=true; if(e.detail.type==='datastar-patch-elements') patches.push(e.detail.argsRaw?.selector); });"})).unwrap();
        b.navigate(&url).unwrap();
        b.wait("window.streamReady && !!customElements.get('sluice-board')").unwrap();
        b.eval("window.entry = [...document.querySelectorAll('.ro-g')].find(g=>g.querySelector(':scope > summary a')?.textContent==='entry').id; window.scan = [...document.querySelectorAll('.ro-g')].find(g=>g.querySelector(':scope > summary a')?.textContent==='scan').id; window.order = () => [...document.getElementById(entry).querySelectorAll('.ro-u')].map(r=>r.dataset.unit); window.initialOrder=order();").unwrap();
        assert_eq!(b.eval("document.getElementById(entry).open").unwrap(), false);
        assert_eq!(b.eval("document.getElementById(scan).open").unwrap(), true);
        assert_eq!(b.eval("[...document.querySelectorAll('#plan-pane details')].every(d=>(d.getAttribute('data-preserve-attr')||'').split(' ').includes('open'))").unwrap(), true);
        b.eval("document.getElementById(entry).open=true; document.getElementById(scan).open=false; document.querySelector('#'+scan+' .ro-fold').open=true; document.getElementById('u-e-096').open=true").unwrap();
        b.wait("!!document.querySelector('#drill-e-096 .ro-foot')").unwrap();
        b.eval("window.patches=[]").unwrap();
        b
    }).await.unwrap();
    crate::stored_messages::stored(
        &f.writer,
        project,
        crate::stored_messages::Stored {
            thread: "step-e-096-check",
            from: "e-096-check",
            body: "Activity that used to move this row",
            ..Default::default()
        },
    )
    .await;
    browser = tokio::task::spawn_blocking(move || {
        browser.wait("document.querySelector('#u-e-096 .ro-said').textContent.includes('Activity that used to move')").unwrap();
        assert_eq!(browser.eval("order()").unwrap(), browser.eval("initialOrder").unwrap());
        assert_eq!(browser.eval("patches.includes('#plan-roster')").unwrap(), false);
        browser.eval("window.patches=[]").unwrap();
        browser
    }).await.unwrap();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='pending' WHERE project_id=?1 AND step_id='e-096-check'",
                [project.to_string()],
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    browser = tokio::task::spawn_blocking(move || {
        browser.wait("document.getElementById(entry+'-counts').textContent.includes('1 waiting')").unwrap();
        assert_eq!(browser.eval("patches.includes('#plan-roster')").unwrap(), false);
        assert_eq!(browser.eval("order()").unwrap(), browser.eval("initialOrder").unwrap());
        assert_eq!(browser.eval("document.getElementById(entry).open && !document.getElementById(scan).open && document.querySelector('#'+scan+' .ro-fold').open && document.getElementById('u-e-096').open").unwrap(), true);
        browser
    }).await.unwrap();
    // A new member makes e-096 no longer match the one-step entry recipe. A new entry is
    // inserted first in plan position, but was created after every other entry.
    f.writer.write(RetrySafety::NonIdempotent, move |tx| {
        let mut doc = serde_json::to_value(sluice_store::plans::read_plan_rows(tx.sql(), project)?.to_document())?;
        let mut steps = serde_json::Map::new();
        steps.insert("new-check".into(), json!({"run":"custom.open", "tags":["unit:new"], "in":{"species":{"default":"New entry"}}}));
        steps.extend(doc["steps"].as_object().unwrap().clone());
        steps.insert("e-096-extra".into(), json!({"run":"custom.open", "tags":["unit:e-096"]}));
        doc["steps"] = json!(steps);
        crate::seed::put(tx, project, json!(doc))?;
        tx.sql().execute("UPDATE steps SET status='running' WHERE project_id=?1 AND step_id IN ('new-check','e-096-check')", [project.to_string()])?;
        tx.changed(Some(project), "status");
        Ok(())
    }).await.unwrap();
    browser = tokio::task::spawn_blocking(move || {
        browser.wait("!!document.querySelector('#u-new') && document.querySelector('#u-e-096').closest('.ro-g').id !== entry").unwrap();
        assert_eq!(browser.eval("document.getElementById('u-e-096').open && document.getElementById(entry).open && !document.getElementById(scan).open && document.querySelector('#'+scan+' .ro-fold').open").unwrap(), true);
        assert_eq!(browser.eval("order().at(-1)").unwrap(), "new");
        // The order menu changes the cookie before reconnecting the real stream.
        browser.eval("document.querySelector('#plan-band details.dm').open = true; [...document.querySelectorAll('.order-options [role=menuitemradio]')][1].click()").unwrap();
        browser.wait("order()[0] === 'new'").unwrap();
        assert_eq!(browser.eval("document.cookie.includes('=plan')").unwrap(), true);
        // The old document already has both the row and streamReady. Clear its flag
        // before reloading so this wait can only pass on the new document's stream.
        browser.eval("window.streamReady=false; location.reload()").unwrap();
        browser.wait("window.streamReady && !!document.querySelector('#u-new')").unwrap();
        assert_eq!(browser.eval("document.querySelector('.order-options [aria-checked=true]').textContent").unwrap(), "Plan order");
        assert_eq!(browser.eval("[...document.querySelector('.ro-g').querySelectorAll('.ro-u')][0].dataset.unit").unwrap(), "new");
        assert_eq!(browser.eval("document.getElementById('u-e-096').open").unwrap(), true);
        browser.eval("document.querySelector('#plan-band details.dm').open = true; [...document.querySelectorAll('.order-options [role=menuitemradio]')][2].click()").unwrap();
        browser
    }).await.unwrap();
    crate::stored_messages::stored(
        &f.writer,
        project,
        crate::stored_messages::Stored {
            thread: "step-e-097-check",
            from: "e-097-check",
            body: "Most recent entry activity",
            ..Default::default()
        },
    )
    .await;
    tokio::task::spawn_blocking(move || {
        browser.wait("[...document.querySelector('.ro-g').querySelectorAll('.ro-u')][0].dataset.unit === 'e-097'").unwrap();
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
    }).await.unwrap();
    server.abort();
    let _ = server.await;
}
