//! The live path: what a page's stream sends, when it renders, and how a page keeps the owner's
//! state across its patches. The clock alone never patches a page; a state change patches only
//! the regions it changed (the plan's lines apart from the cards); an idle stream reads a cheap
//! token instead of rendering; a step or unit that left the plan, or a deleted project, is said in
//! place and its stream stays open; a stream's first batch names the build; a hidden tab asks for
//! its title alone; and in Chromium, the settings forms refuse an apply over another author's
//! change, an action, the More menu and a refused field's note survive patches, and the
//! line under the nav says when updates stopped and opens the stream again.
use crate::board_fixture;
use crate::chrome;
use axum::{
    Router,
    body::{Body, Bytes},
    http::{Request, StatusCode},
};
use board_fixture::Fixture;
use chrome::Chrome;
use futures_util::{Stream, StreamExt};
use sluice_model::{commands::StepStatus, error::PublicError, events::Event, ids::ProjectSelector};
use sluice_store::{RetrySafety, projects};
use sluice_web::clock::Clock;
use std::time::Duration;
use tower::ServiceExt;

/// A stream's body, read as it comes.
async fn open(router: &Router, path: &str) -> impl Stream<Item = Bytes> + Unpin + use<> {
    let response = router
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
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
/// What the stream sends within `window` (it may stay open after).
async fn read_for(body: &mut (impl Stream<Item = Bytes> + Unpin), window: Duration) -> String {
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + window;
    while let Ok(Some(chunk)) = tokio::time::timeout_at(deadline, body.next()).await {
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    text
}
/// The first batch: everything up to its signals.
async fn first_batch(body: &mut (impl Stream<Item = Bytes> + Unpin)) -> String {
    let mut text = String::new();
    while !text.contains("\"stale\"") && !text.contains("\"sstale\"") {
        let chunk = tokio::time::timeout(Duration::from_secs(10), body.next())
            .await
            .expect("a first batch")
            .expect("the stream stays open");
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    text
}
fn selectors(wire: &str) -> Vec<&str> {
    wire.lines()
        .filter_map(|l| l.strip_prefix("data: selector "))
        .collect()
}
fn drawn_version(page: &str) -> String {
    let signals = page
        .split("data-signals=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap()
        .replace("&#34;", "\"")
        .replace("&quot;", "\"");
    serde_json::from_str::<serde_json::Value>(&signals).unwrap()["ver"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn sql(f: &Fixture, statement: &'static str) {
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(statement, [id.to_string()])?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_state_change_patches_only_the_regions_it_changed() {
    let f = Fixture::new().await;
    let router = f.router();
    let path = format!("/projects/id/{}", f.id);
    let (_, page) = f.get(&path).await;
    // the plan draws no relations for a script to draw lines from
    assert!(!page.contains("class=\"board-edges\""));
    let mut body = open(&router, &format!("{path}/stream")).await;
    first_batch(&mut body).await;
    // beta-build stays failed, for another reason: only its item in Needs you and the board's
    // StepStatus say it
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let error = PublicError::FnFailure {
                message: "lint failed".into(),
            };
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='beta-build'",
                (id.to_string(), serde_json::to_string(&error).unwrap()),
            )?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    let wire = first_batch(&mut body).await;
    let sent = selectors(&wire);
    assert!(sent.contains(&"#n-stop-beta-build"), "{sent:?}");
    for whole in [
        "#project-board",
        "#plan-band",
        "#plan-needs",
        "#plan-roster",
        "#plan-done",
    ] {
        assert!(!sent.contains(&whole), "{whole}: {sent:?}");
    }
    assert!(wire.contains("Its fn failed: lint failed."), "{wire}");
    assert!(
        wire.len() < page.len() / 2,
        "{} of {}",
        wire.len(),
        page.len()
    );
    // alpha-review starts: alpha's row goes from waiting to at work, and the head's sentence
    // says so; Needs you and the Done index are not sent
    sql(
        &f,
        "UPDATE steps SET status='running' WHERE project_id=?1 AND step_id='alpha-review'",
    )
    .await;
    let wire = first_batch(&mut body).await;
    let sent = selectors(&wire);
    assert!(sent.contains(&"#plan-band"), "{sent:?}");
    assert!(wire.contains("id=\"u-alpha\" class=\"ro-u\""), "{wire}");
    for whole in [
        "#project-board",
        "#n-stop-beta-build",
        "#plan-needs",
        "#plan-done",
    ] {
        assert!(!sent.contains(&whole), "{whole}: {sent:?}");
    }
    assert!(
        wire.len() < page.len() / 2,
        "{} of {}",
        wire.len(),
        page.len()
    );
}

#[tokio::test]
async fn a_step_or_unit_that_left_the_plan_is_said_in_place_and_its_stream_stays_quiet() {
    let mut f = Fixture::new().await;
    tokio::time::pause();
    f.dashboard = f
        .dashboard
        .with_clock(Clock::tokio(std::time::SystemTime::now()));
    {
        touch(&f, "alpha-review").await;
        let router = f.router();
        let path = format!("/projects/id/{}", f.id);
        let (_, page) = f.get(&path).await;
        let units = page.split("board-units").nth(1).unwrap();
        assert!(
            units.contains("<time data-since=\"") && units.contains("class=\"meta\""),
            "{units}"
        );
        let mut body = open(&router, &format!("{path}/stream")).await;
        let first = first_batch(&mut body).await;
        assert!(first.contains("selector #project-board"), "{first}");
        let rest = read_for(&mut body, Duration::from_secs(4)).await;
        assert!(
            !rest.contains("datastar-patch-elements") && !rest.contains("datastar-patch-signals"),
            "a quiet board sent: {rest}"
        );
        let (_, again) = f.get(&path).await;
        assert_eq!(
            drawn_version(&page),
            drawn_version(&again),
            "the units' age ticks without changing their version"
        );
    }

    let router = f.router();
    let base = format!("/projects/id/{}", f.id);
    let mut gone = open(&router, &format!("{base}/steps/no-such-step/stream")).await;
    let first = first_batch(&mut gone).await;
    assert!(first.contains("selector #step-detail"), "{first}");
    assert!(
        first.contains("This step left the plan; it may have retired."),
        "{first}"
    );
    assert!(
        first.contains(&format!("href=\"{base}/log?step=no-such-step\"")),
        "{first}"
    );
    // the drawer's own signals: it never says the page's updates paused
    assert!(!first.contains("\"stale\""), "{first}");
    assert!(first.contains("\"sstale\":false"), "{first}");
    // it stays open, quiet: no end for the drawer to reconnect from
    let rest = read_for(&mut gone, Duration::from_secs(3)).await;
    assert_eq!(rest, "");
    let mut unit = open(&router, &format!("{base}/units/no-such-unit/stream")).await;
    let first = first_batch(&mut unit).await;
    assert!(
        first.contains("selector #unit-detail")
            && first.contains("This unit left the plan; it may have retired."),
        "{first}"
    );
    assert_eq!(read_for(&mut unit, Duration::from_secs(2)).await, "");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hidden_tab_asks_for_its_title_alone() {
    let f = Fixture::new().await;
    let (status, home) = f.get("/title").await;
    assert_eq!(
        (status, home.as_str()),
        (StatusCode::OK, "1 failed · Projects · sluice")
    );
    let (status, lanes) = f.get(&format!("/title?project={}", f.id)).await;
    assert_eq!(
        (status, lanes.as_str()),
        (StatusCode::OK, "⚠1 · lanes · sluice")
    );
    let (_, page) = f.get(&format!("/projects/id/{}", f.id)).await;
    assert!(page.contains("<title>⚠1 · lanes · sluice</title>"));
    // the title carries the state, the favicon its look variant: a stop, a sand square
    assert!(page.contains(
        "data-page-title=\"⚠1 · lanes · sluice\" data-favicon=\"/static/favicon-look.svg?v="
    ));
    assert!(page.contains(&format!("data-title-src=\"/title?project={}\"", f.id)));
    assert!(page.contains("<link id=\"favicon\" rel=\"icon\" href=\"/static/favicon-look.svg?v="));
    let (status, _) = f.get(&format!("/title?project={}", f.plain)).await;
    assert_eq!(status, StatusCode::OK);
}

/// A server for Chromium on its own thread.
async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    assert_ne!(addr.port(), 3065);
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), server)
}
fn screens(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("SLUICE_LIVE_SCREENS").map(|d| std::path::PathBuf::from(d).join(name))
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_a_patch_keeps_the_action_the_more_menu_and_a_refused_fields_note() {
    let f = Fixture::new().await;
    let titled = f.titled().await;
    let (base, server) = serve(f.router()).await;
    let lanes = f.id;
    let page = format!("{base}/projects/id/{titled}#step:watch");
    let mut browser = tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&page).unwrap();
        browser.wait("document.querySelector('#step-detail .dm-acts form:has(input[value=cancel]) button')").unwrap();
        // the running step's Cancel, in its head's open ⋯: the summary the server draws, focused
        browser.eval("document.querySelector('#step-detail .sh-name details.dm').open=true;window.cancel=document.querySelector('#step-detail .dm-acts form:has(input[value=cancel]) button');cancel.focus();window.sver=document.querySelector('#step-detail').outerHTML.length").unwrap();
        browser
    })
    .await
    .unwrap();
    // something new on the step's thread: the drawer's region is patched
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,at) VALUES (880001,?1,'step-watch','watch','orchestrator','Main is green again.','2026-10-08T09:00:00Z')", [titled.to_string()])?;
            tx.changed(Some(titled), "messages");
            Ok(())
        })
        .await
        .unwrap();
    let mut browser = tokio::task::spawn_blocking(move || {
        browser.wait("document.querySelector('#step-detail').textContent.includes('Main is green again.')").unwrap();
        assert_eq!(browser.eval("cancel.isConnected && document.activeElement===cancel").unwrap(), true);
        // The direct action keeps its focus through the patch.
        assert_eq!(browser.eval("cancel.textContent").unwrap(), "Cancel");
        browser.navigate(&format!("{base}/projects/id/{lanes}")).unwrap();
        browser.wait("document.querySelector('form.board-form') && document.querySelector('#project-board')").unwrap();
        // a refused field: its note under the field
        browser.eval("document.querySelector('#plan-band details.dm').open=true").unwrap();
        browser.eval("(()=>{const f=document.querySelector('form.board-form');f.querySelector('[name=field-0]').value='';f.requestSubmit(f.querySelector('button[value=\"0\"]'))})()").unwrap();
        browser.wait("!document.querySelector('[data-error-for=\"field-0\"]').hidden").unwrap();
        browser.eval("window.note=document.querySelector('[data-error-for=\"field-0\"]').textContent").unwrap();
        browser
    })
    .await
    .unwrap();
    // the project's description changes (the board's whole region), and a step's state (its pane)
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::project_update(
                tx,
                &ProjectSelector::Id(lanes),
                projects::UpdateProject {
                    description: Some("Lanes, rewritten.".into()),
                    author: "cli".into(),
                    ..Default::default()
                },
                &projects::NoResourceSettings,
            )?;
            tx.sql().execute(
                "UPDATE steps SET status='succeeded' WHERE project_id=?1 AND step_id='alpha-review'",
                [lanes.to_string()],
            )?;
            tx.changed(Some(lanes), "status");
            Ok(())
        })
        .await
        .unwrap();
    let mut browser = tokio::task::spawn_blocking(move || {
        browser.wait("document.querySelector('#plan-band').textContent.includes('Lanes, rewritten.')").unwrap();
        browser.wait("document.querySelector('#board-pane').textContent.includes('alpha-review') && [...document.querySelectorAll('#board-pane .ou-table td')].some(td=>td.textContent==='succeeded' && td.previousElementSibling?.textContent==='alpha-review')").unwrap();
        assert_eq!(browser.eval("document.querySelector('#plan-band details.dm').open").unwrap(), true);
        assert_eq!(browser.eval("(()=>{const n=document.querySelector('[data-error-for=\"field-0\"]');return !n.hidden && n.textContent===note && note.length>0})()").unwrap(), true);
        // editing the field clears its note
        browser.eval("(()=>{const i=document.querySelector('[name=field-0]');i.value='gamma';i.dispatchEvent(new Event('input',{bubbles:true}))})()").unwrap();
        assert_eq!(browser.eval("document.querySelector('[data-error-for=\"field-0\"]').hidden").unwrap(), true);
        browser.eval("window.plan=document.querySelector('#plan-pane')").unwrap();
        browser
    })
    .await
    .unwrap();
    // one unit's state: its item in Needs you alone is patched, in place
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let error = PublicError::FnFailure {
                message: "lint failed".into(),
            };
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='beta-build'",
                (lanes.to_string(), serde_json::to_string(&error).unwrap()),
            )?;
            tx.changed(Some(lanes), "status");
            Ok(())
        })
        .await
        .unwrap();
    tokio::task::spawn_blocking(move || {
        browser
            .wait(
                "document.querySelector('#n-stop-beta-build').textContent.includes('lint failed')",
            )
            .unwrap();
        assert_eq!(
            browser
                .eval("plan.isConnected && document.querySelector('#plan-band details.dm').open")
                .unwrap(),
            true
        );
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_updates_stopped_say_so_reconnect_and_a_new_build_asks_for_a_reload() {
    let f = Fixture::new().await;
    let router = f.router();
    let path = format!("/projects/id/{}", f.id);
    let (_, page) = f.get(&path).await;
    let release = page
        .split("data-release=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap()
        .to_owned();
    assert!(!release.is_empty());
    // at the drawn version, the first batch only confirms it, and names the build
    let at = url::form_urlencoded::byte_serialize(
        serde_json::json!({"ver": drawn_version(&page)})
            .to_string()
            .as_bytes(),
    )
    .collect::<String>();
    let mut body = open(&router, &format!("{path}/stream?datastar={at}")).await;
    let first = first_batch(&mut body).await;
    assert!(first.contains(&format!("\"rel\":\"{release}\"")), "{first}");
    assert!(!first.contains("datastar-patch-elements"), "{first}");
    // a step's own page likewise: its stream is the page's, at the version it was drawn with
    let step = format!("{path}/steps/alpha-review");
    let (_, page) = f.get(&step).await;
    let at = url::form_urlencoded::byte_serialize(
        serde_json::json!({"ver": drawn_version(&page)})
            .to_string()
            .as_bytes(),
    )
    .collect::<String>();
    let mut body = open(&router, &format!("{step}/stream?page=true&datastar={at}")).await;
    let first = first_batch(&mut body).await;
    assert!(first.contains(&format!("\"rel\":\"{release}\"")), "{first}");
    assert!(!first.contains("datastar-patch-elements"), "{first}");

    let (base, server) = serve(f.router()).await;
    let lanes = f.id;
    let page = format!("{base}/projects/id/{lanes}");
    let mut browser = tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&page).unwrap();
        browser.wait("document.readyState==='complete' && document.querySelector('#stream-state')?.hidden===true").unwrap();
        // the stream ends for good (the server answers 204): Datastar will not retry it
        browser.eval("window.realFetch=window.fetch;window.fetch=(input,opts)=>String(input).includes('/stream')?Promise.resolve(new Response(null,{status:204})):realFetch(input,opts)").unwrap();
        browser.eval("document.querySelector('.stream-retry').click()").unwrap();
        browser.wait("!document.querySelector('#stream-state').hidden && /^Updates stopped at \\d/.test(document.querySelector('#stream-state .stream-words').textContent)").unwrap();
        if let Some(path) = screens("updates-stopped.png") {
            browser.viewport(1440, "light").unwrap();
            browser.screenshot(&path).unwrap();
        }
        // back online: it opens the stream again, and the line goes
        browser.eval("window.fetch=realFetch;dispatchEvent(new Event('online'))").unwrap();
        browser.wait("document.querySelector('#stream-state').hidden").unwrap();
        // a stream from another build: the calm line asks for a reload
        assert_eq!(browser.eval("document.querySelector('#release-state').hidden").unwrap(), true);
        browser.eval("document.body.dataset.release='an-older-build';document.querySelector('.stream-retry').click()").unwrap();
        browser.wait("!document.querySelector('#release-state').hidden").unwrap();
        assert_eq!(browser.eval("document.querySelector('#release-state').textContent").unwrap(), "sluice was updated · Reload");
        if let Some(path) = screens("release.png") {
            browser.viewport(1440, "dark").unwrap();
            browser.screenshot(&path).unwrap();
        }
        // a hidden tab asks for its title alone (each 30 s; here at once)
        browser.eval("window.titleFetched=false; const titleFetch=window.fetch; window.fetch=async (...args)=>{const r=await titleFetch(...args); if(String(args[0]).includes('/title')) {await r.clone().text(); window.titleFetched=true;} return r;};Object.defineProperty(document,'hidden',{configurable:true,get:()=>true});window.setInterval=(fn)=>{fn();return 0};document.dispatchEvent(new Event('visibilitychange'))").unwrap();
        browser
    })
    .await
    .unwrap();
    let mut browser = tokio::task::spawn_blocking(move || {
        browser.wait("window.titleFetched === true").unwrap();
        browser
    })
    .await
    .unwrap();
    sql(
        &f,
        "UPDATE steps SET status='failed' WHERE project_id=?1 AND step_id='alpha-review'",
    )
    .await;
    tokio::task::spawn_blocking(move || {
        browser
            .eval("document.dispatchEvent(new Event('visibilitychange'))")
            .unwrap();
        browser
            .wait("document.title==='⚠2 · lanes · sluice'")
            .unwrap();
    })
    .await
    .unwrap();
    server.abort();
}

async fn touch(f: &Fixture, step: &'static str) {
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.append_record(
                Some(id),
                Event::StepStatus {
                    step: step.parse().unwrap(),
                    from: None,
                    to: StepStatus::Pending,
                    error: None,
                    run_ids: vec![],
                    needs: Default::default(),
                },
            )?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
}

/// The bug this catches: a plan at scale resent its rows as the clock moved (a running time
/// "46m" turning "47m", a strip's marks sliding left), hundreds of kilobytes a
/// minute with nothing changed. The clock is the page's to move (`nav.js`'s tick): an idle
/// atlas-sized plan's stream sends nothing while minutes pass.
#[tokio::test]
async fn an_idle_plan_at_scale_sends_nothing_as_the_clock_moves() {
    let mut f = Fixture::new().await;
    let atlas = crate::neutral::seed_big(&f.writer, f._home.path()).await;
    tokio::time::pause();
    f.dashboard = f
        .dashboard
        .with_clock(Clock::tokio(std::time::SystemTime::now()));
    let router = f.router();
    let mut body = open(&router, &format!("/projects/id/{atlas}/stream")).await;
    let first = first_batch(&mut body).await;
    assert!(
        first.contains("selector #project-board"),
        "{:?}",
        selectors(&first)
    );
    let rest = read_for(&mut body, Duration::from_secs(5 * 60)).await;
    assert!(
        !rest.contains("datastar-patch-elements") && !rest.contains("datastar-patch-signals"),
        "an idle plan sent {} bytes as the clock moved: {:?}",
        rest.len(),
        selectors(&rest)
    );
}
