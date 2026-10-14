//! A bypass preset on the dashboard (SPEC §13): a pending step with one waiting carries a
//! "bypass · <author>" tag in its unit's stage cell and its band; once the preset settles it,
//! its manual result reads "bypassed" where set by hand would, on its plan row, its page and
//! its drawer, while a value set by hand still reads set by hand.
use super::plan_html;
use super::seed;
use axum::{
    Extension,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::json;
use sluice_model::{
    commands::{StepBypass, StepSetOutput},
    error::PublicError,
    ids::{ProjectId, ProjectSelector, Revision},
    plan::FnSignature,
};
use sluice_store::{
    ReadPool, RetrySafety, Writer, bypass,
    plans::{self, PlanContext},
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
                "custom.open".into(),
                FnSignature {
                    open: true,
                    ..Default::default()
                },
            )],
        })
    }
}

fn lane() -> serde_json::Value {
    json!({"steps":{
        "l-land":{"run":"custom.open","outputs":{"value":"int"},"tags":["unit:l"]},
        "l-close":{"run":"custom.open","outputs":{"ok":"boolean"},"after":["l-land"],"tags":["unit:l"]},
        "l-rm":{"run":"custom.open","outputs":{"done":"boolean"},"after":["l-close"],"tags":["unit:l"]}
    }})
}

async fn page(state: &views::DashboardState, uri: String) -> String {
    let app = views::dashboard_router(state.clone()).layer(Extension(Registry(Arc::new(Exact))));
    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1 << 22).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}
async fn drawer(state: &views::DashboardState, project: ProjectId, step: &str) -> String {
    let registry = Registry(Arc::new(Exact));
    let step = views::board::step_detail(
        state,
        project,
        Some(&registry),
        &step.parse().unwrap(),
        false,
    )
    .await
    .unwrap()
    .1
    .unwrap()
    .step;
    step.body("").unwrap().as_str().to_owned()
}

/// One step's row in a unit's step view.
fn sv_row<'a>(html: &'a str, step: &str) -> &'a str {
    let at = html
        .find(&format!("data-sv=\"{step}\""))
        .unwrap_or_else(|| panic!("no step view row for {step}: {html}"));
    &html[at..at + html[at..].find("</li>").unwrap()]
}

#[tokio::test]
async fn a_bypassed_step_reads_bypassed_and_a_waiting_one_carries_its_tag() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let project = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "lanes".parse().unwrap(),
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
    let context = move |revision: Revision| PlanContext {
        project,
        revision,
        plan: seed::compile(lane(), &seed::Open),
    };
    let rev = writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            seed::put(tx, project, lane())
        })
        .await
        .unwrap();
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            bypass::step_bypass(
                tx,
                &context(rev),
                StepBypass {
                    project: ProjectSelector::Id(project),
                    step: "l-close".parse().unwrap(),
                    outputs: Some(serde_json::from_value(json!({"ok": true})).unwrap()),
                    off: false,
                    reason: "the ticket stays open".into(),
                    author: Some("orchestrator".into()),
                },
            )
        })
        .await
        .unwrap();
    let state =
        views::DashboardState::new(ReadPool::open(home.path(), 2).unwrap(), Arc::new(Catalog));
    // Waiting, its first step ready: a row, whose steps carry the tag with who set it.
    let plan = page(&state, format!("/projects/id/{project}")).await;
    assert_eq!(plan_html::place(&plan, "l"), "waiting");
    let row = plan_html::row(&plan, "l");
    assert!(
        sv_row(row, "l-close").contains("<span class=\"pill sv-tag\">bypass · orchestrator</span>"),
        "{row}"
    );
    let waiting = drawer(&state, project, "l-close").await;
    assert!(
        waiting.contains("<span class=\"pill\">bypass · orchestrator</span>"),
        "{waiting}"
    );
    // land is set by hand: the preset settles close in the same transaction.
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::step_set_output(
                tx,
                &context(rev),
                StepSetOutput {
                    project: ProjectSelector::Id(project),
                    step: "l-land".parse().unwrap(),
                    outputs: serde_json::from_value(json!({"value": 1})).unwrap(),
                    force: false,
                    reason: "landed".into(),
                    author: Some("owner".into()),
                },
            )
        })
        .await
        .unwrap();
    let plan = page(&state, format!("/projects/id/{project}")).await;
    // begun: its row at work, its steps under it
    let row = plan_html::row(&plan, "l");
    // its unit's step view: the state cell says it
    let close = sv_row(row, "l-close");
    assert!(
        close.contains("aria-label=\"bypassed\"")
            && close.contains("<span class=\"st-w\" aria-hidden=\"true\">bypassed</span>"),
        "{close}"
    );
    assert!(!close.contains("sv-tag"), "{close}");
    assert!(
        sv_row(row, "l-land")
            .contains("<span class=\"st-w\" aria-hidden=\"true\">set by hand</span>"),
        "{row}"
    );
    for html in [
        drawer(&state, project, "l-close").await,
        page(&state, format!("/projects/id/{project}/steps/l-close")).await,
    ] {
        assert!(html.contains("<b>Bypassed</b>"), "{html}");
        assert!(
            html.contains("<span class=\"f-src\">Bypassed</span>"),
            "{html}"
        );
        assert!(!html.contains("bypass · orchestrator"), "{html}");
    }
    let land = drawer(&state, project, "l-land").await;
    assert!(land.contains("<b>Set by hand</b>"), "{land}");
    assert!(
        land.contains("<span class=\"f-src\">Set by hand</span>"),
        "{land}"
    );
}
