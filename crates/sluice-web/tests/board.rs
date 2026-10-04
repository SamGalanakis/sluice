use serde_json::json;
use sluice_model::{
    commands::StepStatus,
    gates::{StateSnapshot, StepState},
    ids::ProjectId,
    plan::{FnSignature, Plan, SignatureProvider},
};
use sluice_web::views::{
    self, Counts,
    board::{Endpoint, ProjectView, RelationKind},
};
struct Signatures;
impl SignatureProvider for Signatures {
    fn signature(&self, _: &str) -> Option<FnSignature> {
        Some(FnSignature {
            open: true,
            ..Default::default()
        })
    }
}
fn fixture() -> (Plan, StateSnapshot, views::ProjectView) {
    let plan=Plan::parse_json(&serde_json::to_vec(&json!({"inputs":{"enabled":"boolean"},"steps":{
        "start":{"run":"core.external","outputs":{"ok":"boolean","text":"string"},"tags":["unit:build","exit"]},
        "handoff":{"run":"core.external","in":{"data":{"source":"start/text"}},"tags":["unit:review"]},
        "order":{"run":"core.external","after":["start"],"tags":["unit:review"]},
        "yes":{"run":"core.external","after":["start/ok"],"tags":["unit:review"]},
        "no":{"run":"core.external","after":["!start/ok"],"tags":["unit:review"]},
        "cleanup":{"run":"core.external","after":["start?"],"tags":["unit:review"]},
        "unit-gate":{"run":"core.external","after":["unit:build"]},
        "unit-tolerant":{"run":"core.external","after":["unit:build?"]},
        "enabled-work":{"run":"core.external","after":["enabled"]},
        "done":{"run":"core.external","tags":["unit:finished"]}},"outputs":{"result":{"source":"start/text"}}})).unwrap(),&Signatures).unwrap();
    let mut state = StateSnapshot::default();
    state.steps.insert(
        "start".parse().unwrap(),
        StepState {
            status: StepStatus::Failed,
            error: Some("A <script>failure</script>".into()),
            ..Default::default()
        },
    );
    state.steps.insert(
        "done".parse().unwrap(),
        StepState {
            status: StepStatus::Skipped,
            ..Default::default()
        },
    );
    let project = views::ProjectView {
        id: ProjectId::new(),
        name: "board".into(),
        description: "Fixture <script>".into(),
        icon_text: String::new(),
        icon_url: String::new(),
        paused: false,
        archived: false,
        changed: "2026-10-03T12:00:00Z".into(),
        counts: Counts {
            pending: 8,
            failed: 1,
            skipped: 1,
            ..Default::default()
        },
        running: vec![],
        failed_steps: vec!["start".into()],
    };
    (plan, state, project)
}
#[test]
fn relation_forms_and_unit_identity_survive_cross_unit_dependencies() {
    let (plan, state, project) = fixture();
    let view = ProjectView::new(project, &plan, &state, 3);
    assert_eq!(view.units.len(), 6); // tagged build/review/finished plus three singletons
    let handoff = view
        .relations
        .iter()
        .find(|e| e.kind == RelationKind::Handoff)
        .unwrap();
    assert_eq!(handoff.label, "text → data");
    assert!(
        view.relations
            .iter()
            .any(|e| e.kind == RelationKind::Condition && e.label == "ok")
    );
    assert!(
        view.relations
            .iter()
            .any(|e| e.kind == RelationKind::NegatedCondition && e.label == "not ok")
    );
    assert!(
        view.relations
            .iter()
            .any(|e| e.kind == RelationKind::Ordering && e.tolerant)
    );
    let unit = view
        .relations
        .iter()
        .find(|e| e.kind == RelationKind::Unit && !e.tolerant)
        .unwrap();
    assert_eq!(unit.from, Endpoint::Unit("build".parse().unwrap()));
    assert!(
        view.units
            .iter()
            .find(|u| u.id.as_str() == "finished")
            .unwrap()
            .done
    );
    let review = view
        .units
        .iter()
        .find(|u| u.id.as_str() == "review")
        .unwrap();
    assert!(review.steps.iter().all(|s| s.blocked));
    assert!(
        review
            .blocked
            .iter()
            .any(|w| w == "after !start/ok (failed)")
    );
    assert!(
        view.units
            .iter()
            .find(|u| u.id.as_str() == "unit-gate")
            .unwrap()
            .blocked
            .iter()
            .any(|w| w == "after unit:build (exit start failed)")
    );
}
#[test]
fn html_escapes_values_and_folds_all_skipped_units() {
    let (plan, state, project) = fixture();
    let view = ProjectView::new(project, &plan, &state, 1);
    let html = view.body().unwrap();
    let html = html.as_str();
    assert!(html.contains("data-node=\"u:build\""));
    assert!(html.contains("fold-finished"));
    assert!(html.contains("data-preserve-attr=\"open\""));
    assert!(!html.contains("<script>failure"));
    assert!(html.contains("&lt;script&gt;"));
    let region = view.region().unwrap();
    assert!(!region.as_str().contains("sluice-drawer"));
    assert!(!region.as_str().contains("data-init"));
}
#[test]
fn header_is_a_passive_status_view_without_project_actions() {
    let (plan, state, mut project) = fixture();
    project.paused = true;
    project.archived = true;
    let view = ProjectView::new(project.clone(), &plan, &state, 1);
    let html = view.body().unwrap();
    let html = html.as_str();
    assert!(html.contains(">Paused</span>"));
    assert!(html.contains("Archived: listed apart"));
    assert!(!html.contains("switches"));
    assert!(!html.contains("/actions"));
    assert!(!html.contains("pause_project"));
    assert!(!html.contains("archive_project"));
    project.paused = false;
    project.archived = false;
    let view = ProjectView::new(project, &plan, &state, 1);
    let html = view.body().unwrap();
    assert!(!html.as_str().contains(">Paused</span>"));
    assert!(!html.as_str().contains("Archived: listed apart"));
}
#[test]
fn paused_and_queued_work_have_distinct_wait_projection() {
    let (plan, mut state, project) = fixture();
    state.steps.insert(
        "enabled-work".parse().unwrap(),
        StepState {
            queued: vec!["needs cpu 1 (0/0 held)".into()],
            ..Default::default()
        },
    );
    let view = ProjectView::new(project.clone(), &plan, &state, 1);
    let queued = view
        .units
        .iter()
        .flat_map(|u| &u.steps)
        .find(|s| s.id.as_str() == "enabled-work")
        .unwrap();
    assert_eq!(queued.caption(), "queued");
    assert!(!queued.blocked);
    state.paused = sluice_model::plan::Pause::Yes;
    let view = ProjectView::new(project, &plan, &state, 1);
    assert!(
        view.units
            .iter()
            .flat_map(|u| &u.steps)
            .filter(|s| s.status == "pending")
            .all(|s| s.mark == "paused" && !s.blocked)
    );
}
