//! A run's activity outline on the step page and in the drawer, read at render time from real
//! (trimmed, masked) Claude, Codex and Devin transcripts: turns and calls, failures never hidden,
//! the page opening on the failing call, live turns over the step's stream, the raw transcript
//! served masked, and a phone's single column in Chromium. `SLUICE_ACTIVITY_SCREENS=<dir>` also
//! saves a screenshot of each width and theme there.
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
#[path = "../../sluice-agents/tests/fixtures/activity/lay.rs"]
mod lay;
use axum::{
    Extension, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use futures_util::StreamExt;
use serde_json::json;
use sluice_model::{
    error::PublicError,
    ids::{AttemptId, ProjectId, RunId},
    plan::FnSignature,
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use sluice_web::views::{
    self,
    board::{Registry, RegistrySnapshot, RegistrySource},
};
use std::sync::Arc;
use tower::ServiceExt;

struct Catalog;
impl views::CatalogSource for Catalog {
    fn catalog(&self, _: Option<ProjectId>) -> Result<views::FunctionCatalog, PublicError> {
        Ok(views::FunctionCatalog::default())
    }
}
struct Exact;
impl RegistrySource for Exact {
    fn signatures(&self, _: ProjectId) -> Result<RegistrySnapshot, PublicError> {
        Ok(RegistrySnapshot {
            version: "1".into(),
            functions: vec![(
                "agent.claude".into(),
                FnSignature {
                    open: true,
                    ..Default::default()
                },
            )],
        })
    }
}

/// A home with one step, `work`, whose one run `engine`'s fixture transcript is laid out for.
struct Fixture {
    home: tempfile::TempDir,
    writer: Writer,
    app: Router,
    project: ProjectId,
    run: RunId,
    laid: lay::Laid,
}
/// When each fixture's run started: just before its transcript's first record.
fn started(engine: &str) -> &'static str {
    match engine {
        "claude" => "2026-10-08T10:08:00Z",
        "codex" => "2026-10-08T13:01:00Z",
        _ => "2026-10-08T02:30:00Z",
    }
}
async fn fixture(engine: &str, status: &str) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let project = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "outline".parse().unwrap(),
                    description: String::new(),
                    icon: None,
                    resources: None,
                    author: "owner".into(),
                },
                &EmptyPlanInitializer,
                &NoResourceSettings,
            )
        })
        .await
        .unwrap()
        .project_id;
    let run = RunId::new();
    let (status, start) = (status.to_owned(), started(engine).to_owned());
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let doc = json!({"steps":{"work":{"run":"agent.claude","outputs":{"ready":"boolean"}}}});
            tx.sql().execute("UPDATE plans SET doc=?2 WHERE project_id=?1", (project.to_string(), doc.to_string()))?;
            tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status,error,run_ids) VALUES(?1,'work',0,?2,?3,?4,?5)", (project.to_string(), doc["steps"]["work"].to_string(), &status, (status == "failed").then_some(r#"{"error":"agent_failure","kind":"Engine","message":"engine stopped"}"#), json!([run]).to_string()))?;
            let attempt = AttemptId::new();
            tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at) VALUES(?1,?2,'work','executing','{}','fixture',?3)", (attempt.to_string(), project.to_string(), &start))?;
            let finished = (status != "running").then_some("2026-10-08T23:00:00Z");
            let result = match status.as_str() {
                "failed" => Some(json!({"status":"failed","error":{"error":"agent_failure","kind":"Engine","message":"engine stopped"}}).to_string()),
                "succeeded" => Some(json!({"status":"succeeded","outputs":{"ready":true}}).to_string()),
                _ => None,
            };
            tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at,started_at,finished_at,result) VALUES(?1,?2,?3,'work',?4,?4,?5,?6)", (run.to_string(), project.to_string(), attempt.to_string(), &start, finished, result))?;
            tx.changed(Some(project), "project");
            Ok(())
        })
        .await
        .unwrap();
    let claude = home.path().join("claude-home");
    let laid = lay::lay(home.path(), &claude, engine, &run.to_string());
    let mut state =
        views::DashboardState::new(ReadPool::open(home.path(), 2).unwrap(), Arc::new(Catalog));
    state.claude_home = Some(claude);
    let app = views::dashboard_router(state).layer(Extension(Registry(Arc::new(Exact))));
    Fixture {
        home,
        writer,
        app,
        project,
        run,
        laid,
    }
}
impl Fixture {
    async fn get(&self, path: &str) -> (StatusCode, String) {
        let response = self
            .app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 16 << 20).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }
    fn page(&self) -> String {
        format!("/projects/id/{}/steps/work", self.project)
    }
}
/// The page's Activity section.
fn section(html: &str) -> &str {
    let at = html
        .find("<section class=\"d-sec d-activity\" id=\"activity\"")
        .unwrap_or_else(|| panic!("an Activity section: {html}"));
    let rest = &html[at..];
    &rest[..rest.find("</section>").unwrap()]
}
/// Each turn's `<li>`, in order.
fn turns(section: &str) -> Vec<&str> {
    section.split("<li class=\"act-turn").skip(1).collect()
}

#[tokio::test]
async fn each_engines_run_reads_as_turns_and_calls_with_its_failures_flagged() {
    // (engine, turns, calls, failed, the Runs row's profile)
    for (engine, count, calls, failed, profile) in [
        ("claude", 8, 34, 2, "Bash 34 · 2 failed"),
        (
            "codex",
            2,
            30,
            1,
            "web search 18 · shell 7 · read 4 · edit 1 · 1 failed",
        ),
        (
            "devin",
            4,
            17,
            1,
            "read 5 · exec 4 · get_output 3 · sidekick 3 · 2 other · 1 failed",
        ),
    ] {
        let f = fixture(engine, "succeeded").await;
        let (status, html) = f.get(&format!("{}?activity=all", f.page())).await;
        assert_eq!(status, StatusCode::OK, "{engine}");
        let activity = section(&html);
        let words = format!(
            "Run 1 · {count} turns · {calls} tool calls · {failed} failed · read from its {}",
            match engine {
                "claude" => "Claude session transcript",
                "codex" => "Codex session rollout",
                _ => "Devin hook journal",
            }
        );
        assert!(activity.contains(&words), "{engine}: {activity}");
        assert_eq!(turns(activity).len(), count, "{engine}");
        assert_eq!(
            activity.matches("class=\"act-c is-failed\"").count(),
            failed,
            "{engine}"
        );
        // a failed call is ink with the failed glyph; its result is its error, its end in view
        let fail = &activity[activity.find("act-c is-failed").unwrap()..];
        let fail = &fail[..fail.find("</details>").unwrap()];
        assert!(fail.contains("aria-label=\"failed\""), "{engine}: {fail}");
        assert!(
            fail.contains("<p class=\"act-rh\">Error") && fail.contains("err-box act-err"),
            "{engine}: {fail}"
        );
        // the Runs row counts the run's calls by tool
        assert!(
            html.contains(&format!("<p class=\"a-prof\">{profile}</p>")),
            "{engine}: {}",
            &html[html.find("ol class=\"attempts\"").unwrap()..]
        );
        // every turn says what sluice sent, and a finished run's turns how long they took
        let first = turns(activity)[0];
        assert!(
            first.contains("<span class=\"act-lab\">Task</span>"),
            "{engine}: {first}"
        );
        assert!(first.contains(" · took "), "{engine}: {first}");
        assert!(
            !activity.contains("is-running"),
            "{engine}: an ended run runs nothing"
        );
    }
}

#[tokio::test]
async fn what_sluice_sent_reads_from_the_file_it_handed_over_and_secrets_are_masked() {
    let f = fixture("claude", "succeeded").await;
    let (_, html) = f.get(&format!("{}?activity=all", f.page())).await;
    let activity = section(&html);
    let turns = turns(activity);
    // the task's own words, from its "## Task" section, not sluice's pointer to the file
    assert!(
        turns[0].contains("<span class=\"act-x\">You work in the kiln fork"),
        "{}",
        turns[0]
    );
    assert!(
        !activity.contains("read it fully, then do it"),
        "{activity}"
    );
    // a message handed over as a file reads as the message
    assert!(
        turns[2].contains("<span class=\"act-lab\">Message</span> <span class=\"act-x\">Question 138961 from fig-5416-work"),
        "{}",
        turns[2]
    );
    // the agent's last words in a turn
    assert!(
        turns[7].contains(
            "<span class=\"act-lab\">Said</span> <span class=\"act-x\">Rebased onto main"
        ),
        "{}",
        turns[7]
    );
    // a token in a result is masked, everywhere it is drawn
    assert!(!html.contains("ghp_FAKEfixture"), "{activity}");
    assert!(activity.contains("GH_TOKEN=[redacted]"), "{activity}");
    // a call's arguments are name and value rows; its key argument heads its row
    assert!(
        activity.contains("<dl class=\"act-args\"><div><dt>command</dt><dd>"),
        "{activity}"
    );
    assert!(activity.contains("<span class=\"act-tool\">Bash</span><code class=\"act-key\">"));
}

#[tokio::test]
async fn failures_show_through_every_fold() {
    // Claude: eight turns; only the latest five are drawn, and turn 1, which has a failed call
    let f = fixture("claude", "succeeded").await;
    let (_, html) = f.get(&f.page()).await;
    let activity = section(&html);
    let shown = turns(activity);
    assert_eq!(shown.len(), 6, "{activity}");
    assert!(shown[0].contains("id=\"act-r1-t1\""), "{}", shown[0]);
    assert!(shown[0].contains("act-c is-failed"), "{}", shown[0]);
    assert!(shown[1].contains("id=\"act-r1-t4\""), "{}", shown[1]);
    assert!(
        activity.contains("2 earlier turns not shown, none with a failed call. <a href=\"")
            && activity.contains("?activity=all#activity\">Show earlier turns</a>"),
        "{activity}"
    );
    // a turn with a failure says so in its meta line, in ink with the failed glyph
    assert!(
        shown[0].contains("<span class=\"act-failed\"><span class=\"g g-failed\""),
        "{}",
        shown[0]
    );
    // Devin: reads in a row fold into one row; a failed call never folds away
    let f = fixture("devin", "succeeded").await;
    let (_, html) = f.get(&f.page()).await;
    let activity = section(&html);
    assert!(
        activity.contains("<li class=\"act-looks\"><details data-preserve-attr=\"open\"><summary>")
            && activity.contains("<span>2 reads</span>"),
        "{activity}"
    );
    for fold in activity.split("<li class=\"act-looks\">").skip(1) {
        let fold = &fold[..fold.find("</details></li>").unwrap()];
        assert!(!fold.contains("is-failed"), "{fold}");
    }
    assert_eq!(activity.matches("act-c is-failed").count(), 1, "{activity}");
    // a call whose run ended before it had a result says so
    assert!(
        activity.contains("<span class=\"act-none\">no result</span>"),
        "{activity}"
    );
}

#[tokio::test]
async fn a_failed_step_opens_on_its_last_failed_call_and_why_it_failed_links_there() {
    for engine in ["claude", "codex", "devin"] {
        let f = fixture(engine, "failed").await;
        let (_, html) = f.get(&f.page()).await;
        let activity = section(&html);
        let open: Vec<&str> = activity
            .split("<details class=\"act-c ")
            .skip(1)
            .filter(|c| c[..c.find('>').unwrap()].contains(" open"))
            .collect();
        assert_eq!(open.len(), 1, "{engine}: one call open: {activity}");
        let call = open[0];
        assert!(call.starts_with("is-failed\""), "{engine}: {call}");
        assert!(
            call[..call.find('>').unwrap()].contains("data-act-fail"),
            "{engine}"
        );
        let anchor = call
            .split("id=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap();
        // its turn is open too
        let turn = anchor.rsplit_once("-c").unwrap().0;
        let li = &activity[activity.find(&format!("id=\"{turn}\"")).unwrap()..];
        assert!(
            li[..li.find("<summary>").unwrap()].contains(" open>"),
            "{engine}: {li}"
        );
        // Why it failed links to it, before the outline
        let why = &html[html.find("d-sec d-failure").unwrap()..];
        let why = &why[..why.find("</section>").unwrap()];
        assert!(
            why.contains(&format!(
                "<p class=\"act-why\"><a href=\"#{anchor}\">Its last failed call: <span class=\"act-tool\">"
            )),
            "{engine}: {why}"
        );
    }
    // a succeeded step opens no call
    let f = fixture("codex", "succeeded").await;
    let (_, html) = f.get(&f.page()).await;
    assert!(!html.contains("data-act-fail"));
    assert!(!html.contains("act-why"));
}

#[tokio::test]
async fn a_live_runs_open_turn_ticks_and_new_turns_arrive_over_its_stream() {
    let f = fixture("claude", "running").await;
    // the run so far: through its third turn's first call
    let upto = f
        .laid
        .lines
        .iter()
        .position(|l| l.contains("messages/138999.md"))
        .unwrap();
    f.laid.write(upto);
    let stream = format!(
        "{}/stream?page=true&datastar=%7B%22sver%22%3A%22old%22%7D",
        f.page()
    );
    let response = f
        .app
        .clone()
        .oneshot(Request::builder().uri(&stream).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let mut body = response.into_body().into_data_stream();
    let mut first = String::new();
    while !first.contains("</article>") {
        first.push_str(std::str::from_utf8(&body.next().await.unwrap().unwrap()).unwrap());
    }
    let activity = section(&first);
    let shown = turns(activity);
    assert_eq!(shown.len(), 3, "{activity}");
    let open = shown[2];
    assert!(open.starts_with(" is-running\""), "{open}");
    assert!(
        open.contains("<span class=\"act-live\"><span class=\"g g-running\"")
            && open.contains("running for <time data-since=\"2026-10-08T10:"),
        "{open}"
    );
    assert!(
        open[..open.find("<summary>").unwrap()].contains(" open>"),
        "{open}"
    );
    assert!(
        !open.contains(" · took "),
        "an open turn is still going: {open}"
    );
    // the rest of the run arrives: the next batch carries its new turns
    f.laid.write(usize::MAX);
    let mut next = String::new();
    while !next.contains("</article>") {
        next.push_str(std::str::from_utf8(&body.next().await.unwrap().unwrap()).unwrap());
    }
    let activity = section(&next);
    assert_eq!(
        turns(activity).len(),
        6,
        "the latest five and the failed first: {activity}"
    );
    assert!(activity.contains("id=\"act-r1-t8\""), "{activity}");
    // the engine closed its last turn: nothing is open now
    assert!(!activity.contains("is-running"), "{activity}");
    drop(f.writer);
    drop(f.home);
}

#[tokio::test]
async fn the_raw_transcript_is_a_run_file_served_masked() {
    let f = fixture("devin", "succeeded").await;
    let (_, html) = f.get(&f.page()).await;
    let href = format!(
        "/projects/id/{}/runs/{}/files/devin-hooks.jsonl",
        f.project, f.run
    );
    assert!(
        section(&html).contains(&format!(" · <a href=\"{href}\">devin-hooks.jsonl</a></p>")),
        "{html}"
    );
    // a secret the agent saw is masked in the file served
    let journal = std::fs::read_to_string(&f.laid.transcript).unwrap();
    std::fs::write(
        &f.laid.transcript,
        journal.replacen(
            "\"prompt\": \"",
            "\"prompt\": \"token=ghp_FAKEfixture0123456789abcdefABCDEF0123 ",
            1,
        ),
    )
    .unwrap();
    let (status, raw) = f.get(&href).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        raw.contains("token=[redacted]") && !raw.contains("ghp_FAKE"),
        "{raw}"
    );
    // a Claude transcript lives in Claude's home, not the run's directory: nothing links it
    let f = fixture("claude", "succeeded").await;
    let (_, html) = f.get(&f.page()).await;
    assert!(!section(&html).contains("/files/"), "{html}");
}

const GEOMETRY: &str = r#"(() => {
  const fail = document.querySelector('[data-act-fail] > summary').getBoundingClientRect();
  const keys = [...document.querySelectorAll('#activity .act-key')].filter(e => e.checkVisibility());
  return {scroll: document.documentElement.scrollWidth, width: document.documentElement.clientWidth,
          y: scrollY, failTop: fail.top, failBottom: fail.bottom, height: innerHeight,
          clipped: [...document.querySelectorAll('#activity *')].filter(e => {
            const r = e.getBoundingClientRect(); return r.width > 0 && r.right > document.documentElement.clientWidth + 0.5; }).length,
          wraps: keys.some(k => k.getBoundingClientRect().height > 21),
          rows: keys.length};
})()"#;

#[tokio::test(flavor = "multi_thread")]
async fn chromium_draws_the_outline_in_one_column_on_a_phone_and_opens_on_the_failure() {
    let f = fixture("claude", "failed").await;
    let router = f.app.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let page = format!("http://{addr}{}", f.page());
    tokio::task::spawn_blocking(move || {
        let screens = std::env::var_os("SLUICE_ACTIVITY_SCREENS").map(std::path::PathBuf::from);
        let mut browser = chrome::Chrome::open(&page).unwrap();
        let ready = "document.readyState === 'complete' && document.querySelector('#activity')";
        browser.wait(ready).unwrap();
        for width in [390, 1440, 2560] {
            for theme in ["light", "dark"] {
                browser.viewport(width, theme).unwrap();
                browser.navigate(&page).unwrap();
                browser.wait(ready).unwrap();
                browser
                    .eval("new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))")
                    .unwrap();
                let g = browser.eval(GEOMETRY).unwrap();
                let label = format!("{width} {theme}");
                assert!(
                    g["scroll"].as_f64().unwrap() <= g["width"].as_f64().unwrap(),
                    "{label}: sideways scroll {g}"
                );
                assert_eq!(g["clipped"], 0, "{label}: clipped {g}");
                // the page opened on the failing call: it is in view
                assert!(
                    g["failTop"].as_f64().unwrap() >= 0.0
                        && g["failBottom"].as_f64().unwrap() <= g["height"].as_f64().unwrap(),
                    "{label}: the failing call is out of view {g}"
                );
                // a phone wraps a long key argument under its tool; a wide screen keeps one line
                assert_eq!(g["wraps"], width == 390, "{label}: {g}");
                if let Some(dir) = &screens {
                    browser
                        .screenshot(&dir.join(format!("step-failed-{width}-{theme}.png")))
                        .unwrap();
                }
            }
        }
        // in the drawer beside the board, the failing call is brought into view as it arrives
        browser.viewport(1440, "light").unwrap();
        let board = page.rsplit_once("/steps/").unwrap().0.to_owned();
        browser.navigate(&format!("{board}#step:work")).unwrap();
        browser
            .wait("document.querySelector('#drawer [data-act-fail]') && document.querySelector('#drawer').scrollTop > 0")
            .unwrap();
        let g = browser
            .eval("(() => { const d = document.querySelector('#drawer').getBoundingClientRect(), f = document.querySelector('#drawer [data-act-fail] > summary').getBoundingClientRect(); return f.top >= d.top && f.bottom <= d.bottom; })()")
            .unwrap();
        assert_eq!(g, true, "the drawer opened on the failing call");
        if let Some(dir) = &screens {
            browser
                .screenshot(&dir.join("drawer-failed-1440-light.png"))
                .unwrap();
        }
    })
    .await
    .unwrap();
    server.abort();
}
