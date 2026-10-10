//! The live path: what a page's stream sends, when it renders, and how a page keeps the owner's
//! state across its patches. The clock alone never patches a page; a state change patches only
//! the regions it changed (the plan's lines apart from the cards); an idle stream reads a cheap
//! token instead of rendering; a step or unit that left the plan, or a deleted project, is said in
//! place and its stream stays open; a stream's first batch names the build; a hidden tab asks for
//! its title alone; and in Chromium, the settings forms refuse an apply over another author's
//! change, a confirmation, the More menu and a refused field's note survive patches, and the
//! line under the nav says when updates stopped and opens the stream again.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
mod seed;
use axum::{
    Router,
    body::{Body, Bytes},
    http::{Request, StatusCode},
};
use board_fixture::Fixture;
use chrome::Chrome;
use futures_util::{Stream, StreamExt};
use sluice_model::{
    commands::StepStatus,
    error::PublicError,
    events::Event,
    ids::{ProjectId, ProjectSelector},
};
use sluice_store::{RetrySafety, projects};
use sluice_web::views::{PageState, board, page_router};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
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
    while !text.contains("datastar-patch-signals") {
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
/// `step` changed state just now: the board's Units row for its unit counts from this second.
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
async fn a_quiet_board_patches_nothing_while_its_units_ages_tick() {
    let f = Fixture::new().await;
    touch(&f, "alpha-review").await;
    let router = f.router();
    let path = format!("/projects/id/{}", f.id);
    let (_, page) = f.get(&path).await;
    // the Units row's age is the clock's: a time the page ticks, not text the version counts
    let units = page.split("board-units").nth(1).unwrap();
    assert!(
        units.contains("<time data-since=\"") && units.contains("class=\"meta\""),
        "{units}"
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let (_, again) = f.get(&path).await;
    assert_eq!(
        drawn_version(&page),
        drawn_version(&again),
        "a second later the same board is the same version"
    );
    let mut body = open(&router, &format!("{path}/stream")).await;
    let first = first_batch(&mut body).await;
    assert!(first.contains("selector #project-board"), "{first}");
    let rest = read_for(&mut body, Duration::from_secs(4)).await;
    assert!(
        !rest.contains("datastar-patch-elements") && !rest.contains("datastar-patch-signals"),
        "a quiet board sent: {rest}"
    );
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
    // beta-build stays failed, for another reason: only its stopped module and the board's
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
    let wire = read_for(&mut body, Duration::from_secs(4)).await;
    let sent = selectors(&wire);
    assert!(sent.contains(&"#s-beta"), "{sent:?}");
    for whole in [
        "#project-board",
        "#plan-band",
        "#plan-stopped",
        "#plan-waiting",
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
    // alpha-review starts: alpha moves from Waiting to Running, and the band's sentence says so;
    // the stopped module and the Done index are not sent
    sql(
        &f,
        "UPDATE steps SET status='running' WHERE project_id=?1 AND step_id='alpha-review'",
    )
    .await;
    let wire = read_for(&mut body, Duration::from_secs(4)).await;
    let sent = selectors(&wire);
    assert!(sent.contains(&"#plan-band"), "{sent:?}");
    assert!(wire.contains("data-unit=\"alpha\""), "{wire}");
    for whole in ["#project-board", "#s-beta", "#plan-stopped", "#plan-done"] {
        assert!(!sent.contains(&whole), "{whole}: {sent:?}");
    }
    assert!(
        wire.len() < page.len() / 2,
        "{} of {}",
        wire.len(),
        page.len()
    );
}

/// Counts the board's and the drawer's renders: each reads the registry's signatures once.
struct Counting(AtomicUsize);
impl board::RegistrySource for Counting {
    fn signatures(&self, project: ProjectId) -> Result<board::RegistrySnapshot, PublicError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        board_fixture::Registry.signatures(project)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_stream_reads_its_token_and_renders_only_when_it_moves() {
    let f = Fixture::new().await;
    let counting = Arc::new(Counting(AtomicUsize::new(0)));
    let router = page_router(PageState::new(f.dashboard.clone()))
        .layer(axum::Extension(board::Registry(counting.clone())));
    let base = format!("/projects/id/{}", f.id);
    let mut board = open(&router, &format!("{base}/stream")).await;
    let mut drawer = open(&router, &format!("{base}/steps/alpha-review/stream")).await;
    first_batch(&mut board).await;
    first_batch(&mut drawer).await;
    let (_, _) = tokio::join!(
        read_for(&mut board, Duration::from_millis(3500)),
        read_for(&mut drawer, Duration::from_millis(3500))
    );
    assert_eq!(
        counting.0.load(Ordering::Relaxed),
        2,
        "one render each in 3.5 s while nothing changed"
    );
    // a commit moves the token: both render again, and the board says what changed
    sql(
        &f,
        "UPDATE steps SET status='running' WHERE project_id=?1 AND step_id='alpha-review'",
    )
    .await;
    let (wire, _) = tokio::join!(
        read_for(&mut board, Duration::from_millis(2500)),
        read_for(&mut drawer, Duration::from_millis(2500))
    );
    assert_eq!(counting.0.load(Ordering::Relaxed), 4);
    assert!(wire.contains("datastar-patch-elements"), "{wire}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_step_or_unit_that_left_the_plan_is_said_in_place_and_its_stream_stays_quiet() {
    let f = Fixture::new().await;
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
async fn a_streams_first_batch_names_the_build_that_drew_it() {
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
        (StatusCode::OK, "1 failed · lanes · sluice")
    );
    let (_, page) = f.get(&format!("/projects/id/{}", f.id)).await;
    assert!(page.contains("<title>1 failed · lanes · sluice</title>"));
    assert!(page.contains(&format!(
        "data-page-title=\"1 failed · lanes · sluice\" data-title-src=\"/title?project={}\"",
        f.id
    )));
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
async fn chromium_a_settings_form_refuses_an_apply_over_another_authors_change() {
    let f = Fixture::new().await;
    let (base, server) = serve(f.router()).await;
    let id = f.id;
    let settings = format!("{base}/projects/id/{id}/settings");
    let mut browser = tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&settings).unwrap();
        browser
            .wait("document.querySelector('#project-description') && document.querySelector('[data-preview]').hidden === false")
            .unwrap();
        browser
            .eval("document.querySelector('#project-description').value='The owner\\'s words'")
            .unwrap();
        browser
    })
    .await
    .unwrap();
    // an agent rewrites the description while the owner's form is open
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::project_update(
                tx,
                &ProjectSelector::Id(id),
                projects::UpdateProject {
                    description: Some("The agent's words".into()),
                    author: "cli".into(),
                    ..Default::default()
                },
                &projects::NoResourceSettings,
            )
        })
        .await
        .unwrap();
    let mut browser = tokio::task::spawn_blocking(move || {
        browser.wait("document.querySelector('form[data-setting=description] .setting-changed')?.textContent.startsWith('Changed by cli since you opened this · Reload this field')").unwrap();
        // the other fields were not changed: no note, and their forms apply as before
        assert_eq!(browser.eval("document.querySelectorAll('.setting-changed').length").unwrap(), 1);
        if let Some(path) = screens("settings-changed.png") {
            browser.viewport(1440, "light").unwrap();
            browser.screenshot(&path).unwrap();
        }
        // the owner applies anyway: refused, never written over the agent's
        browser.eval("document.querySelector('form[data-setting=description]').requestSubmit()").unwrap();
        browser.wait("document.querySelector('#description-feedback').textContent.startsWith('Not applied: it changed since you opened it.')").unwrap();
        assert_eq!(browser.eval("document.querySelector('#project-description').value").unwrap(), "The owner's words");
        // Reload this field: the agent's words, and the form applies again
        browser.eval("document.querySelector('[data-reload-field]').click()").unwrap();
        browser.wait("document.querySelector('#project-description').value==='The agent\\'s words' && !document.querySelector('.setting-changed')").unwrap();
        browser.eval("document.querySelector('#project-description').value='Both, merged';document.querySelector('form[data-setting=description]').requestSubmit()").unwrap();
        browser.wait("document.querySelector('#description-feedback').textContent==='Saved'").unwrap();
        // an unrelated field still applies after another author's change elsewhere
        browser.eval("document.querySelector('#prune-done-after').value='6';document.querySelector('form[data-setting=prune_done_after]').requestSubmit()").unwrap();
        browser.wait("document.querySelector('#prune-done-after-feedback').textContent==='Saved'").unwrap();
        browser
    })
    .await
    .unwrap();
    let description = f
        .dashboard
        .reads
        .snapshot(move |c| Ok(projects::resolve(c, &ProjectSelector::Id(id))?.description))
        .await
        .unwrap();
    assert_eq!(description, "Both, merged");
    tokio::task::spawn_blocking(move || drop(browser.eval("1")))
        .await
        .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_a_patch_keeps_the_confirmation_the_more_menu_and_a_refused_fields_note() {
    let f = Fixture::new().await;
    let titled = f.titled().await;
    let (base, server) = serve(f.router()).await;
    let lanes = f.id;
    let page = format!("{base}/projects/id/{titled}#step:watch");
    let mut browser = tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&page).unwrap();
        browser.wait("document.querySelector('#step-detail details.confirm-flow > summary')").unwrap();
        // the running step's Cancel: the summary the server draws, focused
        browser.eval("window.cancel=document.querySelector('#step-detail details.confirm-flow > summary');cancel.focus();window.sver=document.querySelector('#step-detail').outerHTML.length").unwrap();
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
        // and it still opens the dialog, which Keep closes back onto it
        browser.eval("cancel.click()").unwrap();
        browser.wait("document.querySelector('#confirmation').open").unwrap();
        browser.eval("document.querySelector('#confirmation [data-keep]').click()").unwrap();
        browser.wait("!document.querySelector('#confirmation').open && document.activeElement===cancel").unwrap();
        browser.navigate(&format!("{base}/projects/id/{lanes}")).unwrap();
        browser.wait("document.querySelector('form.board-form') && document.querySelector('#project-board')").unwrap();
        // a refused field: its note under the field
        browser.eval("document.querySelector('details.tool-more').open=true").unwrap();
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
        assert_eq!(browser.eval("document.querySelector('details.tool-more').open").unwrap(), true);
        assert_eq!(browser.eval("(()=>{const n=document.querySelector('[data-error-for=\"field-0\"]');return !n.hidden && n.textContent===note && note.length>0})()").unwrap(), true);
        // editing the field clears its note
        browser.eval("(()=>{const i=document.querySelector('[name=field-0]');i.value='gamma';i.dispatchEvent(new Event('input',{bubbles:true}))})()").unwrap();
        assert_eq!(browser.eval("document.querySelector('[data-error-for=\"field-0\"]').hidden").unwrap(), true);
        browser.eval("window.plan=document.querySelector('#plan-pane')").unwrap();
        browser
    })
    .await
    .unwrap();
    // one unit's state: its stopped module alone is patched, in place
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
            .wait("document.querySelector('#s-beta').textContent.includes('lint failed')")
            .unwrap();
        assert_eq!(
            browser
                .eval("plan.isConnected && document.querySelector('details.tool-more').open")
                .unwrap(),
            true
        );
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_updates_stopped_say_so_reconnect_and_a_new_build_asks_for_a_reload() {
    let f = Fixture::new().await;
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
        browser.eval("Object.defineProperty(document,'hidden',{configurable:true,get:()=>true});window.setInterval=(fn)=>{fn();return 0};document.dispatchEvent(new Event('visibilitychange'))").unwrap();
        browser
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
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
            .wait("document.title==='2 failed · lanes · sluice'")
            .unwrap();
    })
    .await
    .unwrap();
    server.abort();
}
