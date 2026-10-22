//! A run's activity outline on the step page and in the drawer, read at render time from real
//! (trimmed, masked) Claude, Codex and Devin transcripts: turns and calls, failures never hidden,
//! the page opening on the failing call, live turns over the step's stream, the raw transcript
//! served masked, and a phone's single column in Chromium. `SLUICE_ACTIVITY_SCREENS=<dir>` also
//! saves a screenshot of each width and theme there.
#[path = "../../../sluice-agents/tests/fixtures/activity/lay.rs"]
mod lay;
use crate::seed;
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
                    limits: None,
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
            seed::put(tx, project, doc)?;
            tx.sql().execute("UPDATE steps SET status=?2,error=?3,run_ids=?4 WHERE project_id=?1 AND step_id='work'", (project.to_string(), &status, (status == "failed").then_some(r#"{"error":"agent_failure","kind":"Engine","message":"engine stopped"}"#), json!([run]).to_string()))?;
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
        activity.contains("2 earlier turns not shown. <a href=\"")
            && activity
                .contains("?activity=all&#38;tab=activity#activity\">Show earlier turns</a>"),
        "{activity}"
    );
    // a turn with a failure says how many in its meta line, in words
    assert!(
        shown[0].contains("<span class=\"act-failed\">1 failed</span>"),
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
        let why = &why[..why.find("</article>").unwrap()];
        assert!(
            why.contains(&format!(
                "<p class=\"act-why\"><a href=\"#{anchor}\">Its last failed call</a>: <span class=\"act-tool\">"
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
    while !first.contains("\"stale\"") {
        first.push_str(std::str::from_utf8(&body.next().await.unwrap().unwrap()).unwrap());
    }
    let activity = section(&first);
    let shown = turns(activity);
    assert_eq!(shown.len(), 3, "{activity}");
    let open = shown[2];
    assert!(open.starts_with(" is-running\""), "{open}");
    assert!(
        open.contains("<span class=\"act-live\"><span class=\"dot run pulse\"></span>")
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
    while !next.contains("\"stale\"") {
        next.push_str(std::str::from_utf8(&body.next().await.unwrap().unwrap()).unwrap());
    }
    assert!(!next.contains("data: selector #step-detail\n"), "{next}");
    assert!(next.contains("data: mode append"), "{next}");
    assert!(next.contains("id=\"act-r1-t8\""), "{next}");
    // The failed first turn was already shown: it must not travel with the new turns.
    assert!(!next.contains("id=\"act-r1-t1\""), "{next}");
    assert!(!next.contains("is-running"), "{next}");
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

/// A message turn whose handed-over file is gone reads back from the message it held: its
/// sender and first line from the messages table, its id a link to it; what sluice typed into
/// the agent itself is what it received.
#[tokio::test]
async fn a_message_turn_whose_file_is_gone_reads_back_from_its_message() {
    let f = fixture("claude", "succeeded").await;
    let file = f
        .home
        .path()
        .join("runs")
        .join(f.run.to_string())
        .join("invocations")
        .join(lay::CLAUDE_INVOCATION)
        .join("messages")
        .join("138999.md");
    std::fs::remove_file(&file).unwrap();
    let (_, before) = f.get(&format!("{}?activity=all", f.page())).await;
    assert!(
        section(&before).contains("<span class=\"act-x\">#138999"),
        "{}",
        section(&before)
    );
    let project = f.project;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,needs_reply,at) VALUES (138999,?1,'step-work','orchestrator','work',?2,0,'2026-10-08T10:30:00Z')",
                (project.to_string(), "Your **default** is right.\n\nKeep going."),
            )?;
            tx.changed(Some(project), "messages");
            Ok(())
        })
        .await
        .unwrap();
    let (_, html) = f.get(&format!("{}?activity=all", f.page())).await;
    let activity = section(&html);
    assert!(
        activity.contains("<span class=\"act-lab\">From orchestrator</span> <span class=\"act-x\">Your default is right.")
            && activity
                .contains("<a class=\"act-ref\" href=\"#message-138999\"><code>#138999</code></a>"),
        "{activity}"
    );
    // a turn is never headed by how its words were delivered
    for transport in ["Sent<", "Received<", "Message<"] {
        assert!(
            !activity.contains(&format!("act-lab\">{transport}")),
            "{activity}"
        );
    }
}
