//! A failure's retry advice and its setting aside, through the pages: the plan's item in Needs
//! you and the step's page give one advice, read from the step's last two runs (a repeated
//! failure leads with Retry with feedback); an engine that exited partway is told apart from
//! one that would not start; the step's head offers Cancel and Cancel and dismiss, whose forms
//! post without script and in place with it, and the plan then offers its Undo.
use crate::board_fixture;
use crate::chrome;
use crate::plan_html;
use crate::seed;
use axum::{
    Extension, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::json;
use sluice_model::{
    commands::{NeedsDismiss, StepCancel, StepDismiss, StepSelection},
    error::PublicError,
    ids::{AttemptId, ProjectId, ProjectSelector, RunId},
    plan::Plan,
};
use sluice_store::{
    RetrySafety, Writer,
    plans::{self, PlanContext},
};
use sluice_web::views::step::{Action, CommandService, Commands, OwnerCommand};
use std::{future::Future, pin::Pin, sync::Arc};
use tower::ServiceExt;

fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from} in {html}"));
    let end = html[start..].find(to).map_or(html.len(), |e| start + e);
    &html[start..end]
}

fn fn_failure(message: &str) -> PublicError {
    PublicError::FnFailure {
        message: message.into(),
    }
}
fn exited() -> PublicError {
    PublicError::AgentFailure {
        kind: "EngineExited".into(),
        message: "claude: transcript record exceeds 1 MiB".into(),
        session: None,
    }
}

const PLAN: &str = r#"{"steps":{
    "rep":{"run":"custom.open","doc":"Repeats its failure"},
    "first":{"run":"custom.open","doc":"Fails once"},
    "exited":{"run":"custom.open","doc":"Its engine exits"},
    "cxl":{"run":"custom.open","doc":"Cancelled while it ran"},
    "after":{"run":"custom.open","doc":"Comes after the first","after":["first"]}}}"#;

/// A project `advice`: `rep` failed twice the same way, `first` lost its process on its only
/// run, `exited`'s engine exited partway, `cxl` was cancelled while it ran; `after` waits on
/// `first`.
async fn advice(f: &Fixture) -> ProjectId {
    let doc: serde_json::Value = serde_json::from_str(PLAN).unwrap();
    let id = f
        .project(
            "advice",
            doc,
            &[
                ("rep", "failed"),
                ("first", "failed"),
                ("exited", "failed"),
                ("cxl", "failed"),
            ],
        )
        .await;
    let runs: Vec<(&str, &str, PublicError)> = vec![
        ("rep", "2026-10-01T00:00:00Z", fn_failure("boom: the same")),
        ("rep", "2026-10-01T00:10:00Z", fn_failure("boom: the same")),
        (
            "first",
            "2026-10-01T00:00:00Z",
            PublicError::ProcessLost {
                message: "gone".into(),
            },
        ),
        ("exited", "2026-10-01T00:00:00Z", exited()),
        (
            "cxl",
            "2026-10-01T00:00:00Z",
            PublicError::Cancelled {
                message: "pivot".into(),
            },
        ),
    ];
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            for (step, started, error) in &runs {
                let generation: i64 = tx.sql().query_row(
                    "SELECT generation FROM steps WHERE project_id=?1 AND step_id=?2",
                    (id.to_string(), step),
                    |r| r.get(0),
                )?;
                let (attempt, run) = (AttemptId::new(), RunId::new());
                tx.sql().execute(
                    "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?1,?2,?3,?4,1,'terminal','{}','hash',?5)",
                    (attempt.to_string(), id.to_string(), step, generation, started),
                )?;
                // each ran four minutes
                tx.sql().execute(
                    "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at,finished_at,result) VALUES (?1,?2,?3,?4,?5,1,?6,?6,strftime('%Y-%m-%dT%H:%M:%SZ',?6,'+4 minutes'),?7)",
                    (
                        run.to_string(),
                        id.to_string(),
                        attempt.to_string(),
                        step,
                        generation,
                        started,
                        json!({"status":"failed","error":error}).to_string(),
                    ),
                )?;
                tx.sql().execute(
                    "UPDATE steps SET error=?3 WHERE project_id=?1 AND step_id=?2",
                    (id.to_string(), step, serde_json::to_string(error).unwrap()),
                )?;
            }
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    id
}

/// The advice a stop in Needs you says after its sentence ("" for none).
fn card_advice(card: &str) -> String {
    card.split("<span class=\"nd-next\">")
        .nth(1)
        .map(|rest| rest[..rest.find("</span>").unwrap()].to_owned())
        .unwrap_or_default()
}
/// The advice a step's page says under why it failed ("" for none).
fn page_advice(page: &str) -> String {
    page.split("<p class=\"err-next\">")
        .nth(1)
        .map(|rest| rest[..rest.find("</p>").unwrap()].to_owned())
        .unwrap_or_default()
}

#[tokio::test]
async fn the_plan_card_and_the_step_page_give_one_retry_advice() {
    let f = Fixture::new().await;
    let id = advice(&f).await;
    let (status, plan) = f.get(&format!("/projects/id/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    let mut said = vec![];
    for step in ["rep", "first", "exited", "cxl"] {
        let card = plan_html::stopped(&plan, step);
        let (_, page) = f.get(&format!("/projects/id/{id}/steps/{step}")).await;
        let (on_card, on_page) = (card_advice(card), page_advice(&page));
        assert_eq!(on_card, on_page, "{step}: the card and the page disagree");
        said.push(on_card);
        let band = between(&page, "<header id=\"step-band\"", "</header>");
        if step == "rep" {
            // a repeated failure: Retry with feedback leads on both (the plan's to its page's
            // box), the bare Retry quiet on the page
            assert!(
                card.contains(
                    "/steps/rep#why-feedback\" data-step=\"rep\">Retry with feedback</a>"
                ) && !card.contains("value=\"retry\""),
                "{card}"
            );
            assert!(
                band.contains("<a class=\"d-fb\" href=\"#why-feedback\">")
                    && band.contains("value=\"retry\" class=\"quiet-act\""),
                "{band}"
            );
            assert!(page.contains("id=\"why-feedback\""), "{page}");
        } else {
            assert!(!card.contains(">Retry with feedback</a>"), "{step}: {card}");
            assert!(!band.contains("d-fb"), "{step}: {band}");
        }
    }
    assert_eq!(
        said,
        [
            "Run 1 failed the same way. Retry with feedback, or change its inputs.",
            // a lost process: Retry is the whole move, so no advice repeats it
            "",
            "If it refused or hit a limit, Retry with feedback; if it crashed, Retry.",
            "",
        ]
    );
    // the engine that exited is not one that would not start
    let card = plan_html::stopped(&plan, "exited");
    assert!(
        card.contains("Its engine exited after 4m.") && !card.contains("would not start"),
        "{card}"
    );
}

/// Cancel, Dismiss and Undismiss, and setting items of Needs you aside, as the coordinator
/// takes them: `step_cancel` and the owner's marks in the store.
struct Store(Writer, Plan);
impl CommandService for Store {
    fn execute(
        &self,
        command: OwnerCommand,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicError>> + Send + '_>> {
        Box::pin(async move {
            let project = command.project;
            let step = command.step.clone();
            let plan = self.1.clone();
            self.0
                .write(RetrySafety::NonIdempotent, move |tx| match command.action {
                    Action::Cancel => {
                        let step = step.expect("a step");
                        let revision = plans::plan_revision(tx.sql(), project)?;
                        let context = PlanContext {
                            project,
                            revision,
                            plan,
                        };
                        plans::step_cancel(
                            tx,
                            &context,
                            StepCancel {
                                expected_rev: None,
                                project: ProjectSelector::Id(project),
                                selection: StepSelection {
                                    steps: Some(vec![step]),
                                    tags: None,
                                },
                                reason: command.message,
                                author: Some(command.author.into()),
                            },
                        )
                        .map(|_| ())
                    }
                    Action::Dismiss | Action::Undismiss => sluice_store::messages::dismiss(
                        tx,
                        StepDismiss {
                            project,
                            step: step.expect("a step"),
                            dismissed: command.action == Action::Dismiss,
                        },
                    ),
                    Action::SetAside | Action::Restore => sluice_store::messages::needs_dismiss(
                        tx,
                        NeedsDismiss {
                            project,
                            items: command.items,
                            dismissed: command.action == Action::SetAside,
                        },
                    ),
                    _ => Err(PublicError::BadRequest {
                        message: "only cancels and marks here".into(),
                    }
                    .into()),
                })
                .await
        })
    }
}
fn router(f: &Fixture) -> Router {
    let plan = seed::compile(
        serde_json::from_str(PLAN).unwrap(),
        &board_fixture::Registry,
    );
    f.router()
        .layer(Extension(Commands(Arc::new(Store(f.writer.clone(), plan)))))
}
/// A browser's form post (no script): where it is sent back to.
async fn post(router: &Router, path: &str, form: &[(&str, &str)]) -> (StatusCode, String) {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form)
        .finish();
    let response = router
        .clone()
        .oneshot(
            Request::post(path)
                .header("content-type", "application/x-www-form-urlencoded")
                .header("accept", "text/html")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let location = response
        .headers()
        .get("location")
        .map(|l| l.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let _ = to_bytes(response.into_body(), usize::MAX).await;
    (status, location)
}
/// The hidden fields of the form posting `action` inside `html`.
fn form_fields(html: &str, action: &str) -> Vec<(String, String)> {
    let marker = format!("name=\"action\" value=\"{action}\"");
    let at = html
        .find(&marker)
        .unwrap_or_else(|| panic!("a form posting {action} in {html}"));
    let start = html[..at].rfind("<form").unwrap();
    let end = html[at..].find("</form>").unwrap() + at;
    html[start..end]
        .split("<input type=\"hidden\" name=\"")
        .skip(1)
        .map(|f| {
            let (name, rest) = f.split_once("\" value=\"").unwrap();
            (
                name.to_owned(),
                rest[..rest.find('"').unwrap()]
                    .replace("&#x2F;", "/")
                    .replace("&#35;", "#"),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_failed_card_offers_cancel_and_cancel_and_dismiss_whose_forms_work_without_script() {
    let f = Fixture::new().await;
    let id = advice(&f).await;
    let router = router(&f);
    let (_, plan) = f.get(&format!("/projects/id/{id}")).await;
    let plan_href = format!("/projects/id/{id}");
    // the step's head offers Cancel and Cancel and dismiss, each a confirmation
    let (_, page) = f.get(&format!("{plan_href}/steps/first")).await;
    let acts = between(&page, "<header id=\"step-band\"", "</header>").to_owned();
    let acts = acts.as_str();
    assert!(acts.contains("value=\"cancel_dismiss\""), "{acts}");
    assert!(
        acts.contains("heading=\"Cancel and dismiss Fails once?\"")
            && acts.contains("Undo brings back the cancel, not the failure.</p>"),
        "{acts}"
    );
    // on the plan, the failure's item is its own; the cancel's sets it aside as Dismiss does
    let card = plan_html::stopped(&plan, "first");
    assert!(card.contains("value=\"stop:first\""), "{card}");
    let cancelled = plan_html::stopped(&plan, "cxl");
    assert!(cancelled.contains("value=\"cancel:cxl\""), "{cancelled}");

    // Cancel and dismiss, posted without script, sent back to the plan
    let mut fields: Vec<(String, String)> = form_fields(acts, "cancel_dismiss")
        .into_iter()
        .filter(|(n, _)| n != "next")
        .collect();
    fields.push(("next".into(), format!("{plan_href}#undo-n-stop-first")));
    fields.push(("message".into(), "superseded".into()));
    let pairs: Vec<(&str, &str)> = fields
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let (status, location) =
        post(&router, &format!("{plan_href}/steps/first/actions"), &pairs).await;
    assert_eq!(
        (status, location.as_str()),
        (
            StatusCode::SEE_OTHER,
            format!("{plan_href}#undo-n-stop-first").as_str()
        )
    );
    // the plan: no longer an item, a Dismissed line with its Undo instead
    let (_, plan) = f.get(&plan_href).await;
    let well = plan_html::band(&plan, "plan-needs");
    assert!(
        plan_html::place(&plan, "first") == "done"
            && well.contains("id=\"undo-n-stop-first\"")
            && well.contains("<span>Dismissed Fails once (cancelled)</span>"),
        "{well}"
    );
    // its dependent is held as before, waiting on earlier work: counted, not a row
    assert_eq!(plan_html::place(&plan, "after"), "", "{plan}");
    // its page: cancelled, the failure it set aside kept on Overview and its run
    let (_, page) = f.get(&format!("{plan_href}/steps/first")).await;
    assert!(
        page.contains("<p class=\"err-said\">It had failed: process_lost: gone (<a href=\"#run-1\">run 1</a>)</p>"),
        "{page}"
    );
    let runs = between(&page, "<li id=\"run-1\"", "</li>");
    assert!(
        runs.contains("<p class=\"a-err\">Its process was lost.</p>")
            && runs
                .contains("<p class=\"a-why\">Cancelled after it failed, by you: superseded.</p>"),
        "{runs}"
    );

    // Undo, without script: the item is back as a cancel (not the failure), with its ×
    let (status, _) = post(
        &router,
        &format!("{plan_href}/steps/first/actions"),
        &[("action", "undismiss"), ("next", &plan_href)],
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, plan) = f.get(&plan_href).await;
    let card = plan_html::stopped(&plan, "first");
    assert!(
        card.contains("</b> cancelled after 4m. superseded.")
            && card.contains("It had failed: process_lost: gone")
            && card.contains("value=\"cancel:first\""),
        "{card}"
    );
    // Cancel alone, without script, on another failure from its head: it stays stopped, now
    // a cancel
    let (_, page) = f.get(&format!("{plan_href}/steps/rep")).await;
    let fields = form_fields(
        between(&page, "<header id=\"step-band\"", "</header>"),
        "cancel",
    );
    let pairs: Vec<(&str, &str)> = fields
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let (status, _) = post(&router, &format!("{plan_href}/steps/rep/actions"), &pairs).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, plan) = f.get(&plan_href).await;
    let card = plan_html::stopped(&plan, "rep");
    assert!(
        card.contains("</b> cancelled after 4m.")
            && card.contains("Cancelled after it failed.")
            && !card.contains("Run 1 failed the same way"),
        "{card}"
    );
    // Cancel and dismiss no longer applies to a cancel: refused, nothing done
    let (status, location) = post(
        &router,
        &format!("{plan_href}/steps/rep/actions"),
        &[("action", "cancel_dismiss"), ("revision", "1")],
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(location.contains("/steps/rep?notice="), "{location}");
}

async fn serve(router: Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, server)
}

/// The × on a stop in Needs you, in Chromium: posted in place (no navigation, no scroll jump),
/// the item gives way to its Dismissed line and Undo takes the focus; Undo brings it back.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_dismiss_a_stop_in_place_with_the_focus_on_undo() {
    let f = Fixture::new().await;
    let id = advice(&f).await;
    let (addr, server) = serve(router(&f)).await;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}")).unwrap();
        for (width, step) in [(1440, "first"), (390, "exited")] {
            browser.viewport(width, "light").unwrap();
            browser.wait(&format!("document.querySelector('#n-stop-{step} button.x')?.checkVisibility()")).unwrap();
            browser.eval("window.marker = 'same document'; window.navigations = performance.getEntriesByType('navigation').length").unwrap();
            browser.eval(&format!("document.querySelector('#n-stop-{step} button.x').click()")).unwrap();
            browser.wait(&format!("document.activeElement?.id === 'undo-n-stop-{step}' && !document.querySelector('#n-stop-{step}')")).unwrap();
            assert_eq!(
                browser.eval("[window.marker, performance.getEntriesByType('navigation').length === window.navigations, document.documentElement.scrollWidth <= innerWidth]").unwrap(),
                json!(["same document", true, true])
            );
            browser.eval(&format!("document.querySelector('#undo-n-stop-{step}').click()")).unwrap();
            browser.wait(&format!("document.activeElement?.id === 'n-stop-{step}'")).unwrap();
        }
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}
