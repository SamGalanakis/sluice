//! A failure's retry advice and its setting aside, through the pages: the plan's Stopped card
//! and the step's page give one advice, read from the step's last two runs (a repeated failure
//! leads with Retry with feedback, the bare Retry quiet); an engine that exited partway is
//! told apart from one that would not start; a failed card's ⋯ offers Cancel and Cancel and
//! dismiss, whose forms post without script and in place with it (the focus on Undo).
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
mod plan_html;
mod seed;
use axum::{
    Extension, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::json;
use sluice_model::{
    commands::{StepCancel, StepDismiss, StepSelection},
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

/// The advice a stopped card says under its sentence ("" for none).
fn card_advice(card: &str) -> String {
    card.split("<p class=\"meta\">")
        .nth(1)
        .map(|rest| rest[..rest.find("</p>").unwrap()].to_owned())
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
            // a repeated failure: Retry with feedback leads on both, the bare Retry is quiet
            assert!(
                card.contains("<summary class=\"primary\">")
                    && card.contains(
                        "<button name=\"action\" value=\"retry\" class=\"quiet-act\">Retry</button>"
                    ),
                "{card}"
            );
            assert!(
                band.contains("<a class=\"d-fb\" href=\"#why-feedback\">")
                    && band.contains("value=\"retry\" class=\"quiet-act\""),
                "{band}"
            );
            assert!(page.contains("id=\"why-feedback\""), "{page}");
        } else {
            assert!(
                !card.contains("class=\"quiet-act\">Retry<"),
                "{step}: {card}"
            );
            assert!(!band.contains("d-fb"), "{step}: {band}");
        }
    }
    assert_eq!(
        said,
        [
            "Run 1 failed the same way, so a bare Retry would most likely fail again. Retry with feedback, or change its inputs.",
            "Its process ended with no result recorded, most often when sluice restarted: Retry.",
            "Its engine started, then exited partway through. Read what it said: a refusal or a limit will happen again, so Retry with feedback; a crash usually passes, so Retry.",
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

/// Cancel, Dismiss and Undismiss as the coordinator takes them: `step_cancel` and the owner's
/// mark in the store.
struct Store(Writer, Plan);
impl CommandService for Store {
    fn execute(
        &self,
        command: OwnerCommand,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicError>> + Send + '_>> {
        Box::pin(async move {
            let (project, step) = (command.project, command.step.expect("a step"));
            let plan = self.1.clone();
            self.0
                .write(RetrySafety::NonIdempotent, move |tx| match command.action {
                    Action::Cancel => {
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
                            step,
                            dismissed: command.action == Action::Dismiss,
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
    // a failure's card: Cancel and Cancel and dismiss in its ⋯, each a confirmation
    let card = plan_html::stopped(&plan, "first");
    let acts = between(card, "<div class=\"dm-acts\"", "</details></sluice-menu>");
    assert!(
        acts.contains("<p class=\"dm-h\">Set it aside</p>"),
        "{acts}"
    );
    assert!(
        acts.contains("heading=\"Cancel Fails once?\"")
            && acts.contains("It stays stopped and can be dismissed; Retry still works."),
        "{acts}"
    );
    assert!(
        acts.contains("heading=\"Cancel and dismiss Fails once?\"")
            && acts.contains("Undo brings back the cancel, not the failure; Retry still works."),
        "{acts}"
    );
    // no Dismiss on a failure, and nothing to set aside on a cancel
    assert!(!card.contains("pl-dismiss"), "{card}");
    let cancelled = plan_html::stopped(&plan, "cxl");
    assert!(
        !cancelled.contains("dm-acts") && cancelled.contains("pl-dismiss"),
        "{cancelled}"
    );
    // the step's head offers the same
    let (_, page) = f.get(&format!("{plan_href}/steps/first")).await;
    let band = between(&page, "<header id=\"step-band\"", "</header>");
    assert!(band.contains("value=\"cancel_dismiss\""), "{band}");

    // Cancel and dismiss, posted without script: back to the plan at its Undo
    let mut fields = form_fields(acts, "cancel_dismiss");
    assert!(
        fields
            .iter()
            .any(|(n, v)| n == "next" && v == &format!("{plan_href}#undo-first")),
        "{fields:?}"
    );
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
            format!("{plan_href}#undo-first").as_str()
        )
    );
    let (_, plan) = f.get(&plan_href).await;
    let stopped = plan_html::band(&plan, "plan-stopped");
    assert!(
        !plan_html::has_region(&plan, "s-first")
            && stopped.contains("id=\"undo-first\"")
            && stopped.contains("Dismissed: <a"),
        "{stopped}"
    );
    // its dependent is held as before, and the plan no longer counts a failure for it
    assert_eq!(plan_html::place(&plan, "after"), "waiting", "{plan}");
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

    // Undo, without script: the card is back as a cancel (not the failure), with its Dismiss
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
        card.contains("<b>cancelled</b>")
            && card.contains("<p class=\"pl-why\">Cancelled: superseded.</p>")
            && card.contains("It had failed: process_lost: gone")
            && card.contains("pl-dismiss")
            && !card.contains("dm-acts"),
        "{card}"
    );
    // Cancel alone, without script, on another failure: it stays stopped, now a cancel
    let (_, plan) = f.get(&plan_href).await;
    let fields = form_fields(
        between(
            plan_html::stopped(&plan, "rep"),
            "<div class=\"dm-acts\"",
            "</sluice-menu>",
        ),
        "cancel",
    );
    let pairs: Vec<(&str, &str)> = fields
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let (status, location) = post(&router, &format!("{plan_href}/steps/rep/actions"), &pairs).await;
    assert_eq!(
        (status, location.as_str()),
        (StatusCode::SEE_OTHER, format!("{plan_href}#s-rep").as_str())
    );
    let (_, plan) = f.get(&plan_href).await;
    let card = plan_html::stopped(&plan, "rep");
    assert!(
        card.contains("<b>cancelled</b>")
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

/// Cancel and dismiss from a failed card's ⋯, in Chromium: one confirmation, posted in place
/// (no navigation, no scroll jump), the card gives way to the Dismissed line and Undo takes the
/// focus.
#[tokio::test(flavor = "multi_thread")]
async fn chromium_cancel_and_dismiss_a_failed_card_in_place_with_the_focus_on_undo() {
    let f = Fixture::new().await;
    let id = advice(&f).await;
    let (addr, server) = serve(router(&f)).await;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}")).unwrap();
        for (width, step) in [(1440, "first"), (390, "exited")] {
            browser.viewport(width, "light").unwrap();
            browser.wait(&format!("customElements.get('sluice-confirm') && document.querySelector('#s-{step} details.dm > summary')?.checkVisibility()")).unwrap();
            browser.eval("window.marker = 'same document'; window.navigations = performance.getEntriesByType('navigation').length").unwrap();
            browser.eval(&format!("document.querySelector('#s-{step} details.dm > summary').click()")).unwrap();
            browser.wait(&format!("document.querySelector('#s-{step} .dm-acts sluice-confirm:last-of-type summary')?.checkVisibility()")).unwrap();
            browser.eval(&format!("document.querySelector('#s-{step} .dm-acts sluice-confirm:last-of-type summary').click()")).unwrap();
            browser.wait("document.querySelector('#confirmation').open").unwrap();
            assert_eq!(
                browser.eval("[document.querySelector('#confirmation-title').textContent, document.activeElement.matches('[data-keep]')]").unwrap()[1],
                json!(true)
            );
            browser.eval("window.scrollAt = scrollY; document.querySelector('#confirmation textarea').value = 'set aside'; document.querySelector('#confirmation button.primary').click()").unwrap();
            browser.wait(&format!("document.activeElement?.id === 'undo-{step}' && !document.querySelector('#s-{step}.pl-stop')")).unwrap();
            assert_eq!(
                browser.eval("[window.marker, performance.getEntriesByType('navigation').length === window.navigations, !document.querySelector('#confirmation').open, document.documentElement.scrollWidth <= innerWidth]").unwrap(),
                json!(["same document", true, true, true])
            );
        }
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}
