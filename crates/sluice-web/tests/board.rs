use serde_json::json;
use sluice_model::{
    commands::StepStatus,
    gates::{StateSnapshot, StepState},
    ids::ProjectId,
    plan::{FnSignature, Plan, SignatureProvider},
};
use sluice_web::views::{
    self, Counts,
    board::{BoardQuery, Endpoint, ProjectView, RelationKind},
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
        "summary":{"run":"core.external","after":["order"],"tags":["unit:review"]},
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

/// A relation whose ends are in two boxes (or in none: a plan input or output) is marked
/// `cross` and shown as a chip on its dependent's top edge, saying its kind ("after start")
/// and linking to its source; one within a box is drawn as before.
#[test]
fn relations_between_boxes_are_marked_and_shown_as_chips_on_their_dependents() {
    let (plan, state, project) = fixture();
    let id = project.id;
    let view = ProjectView::new(project, &plan, &state, 1);
    let edges: serde_json::Value = serde_json::from_str(&view.edges_json()).unwrap();
    let cross = |from: &str, to: &str| {
        edges
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["from"]["id"] == from && e["to"]["id"] == to)
            .map(|e| {
                e["cross"]
                    .as_bool()
                    .expect("every relation says whether it crosses")
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(cross("order", "summary"), [false]); // within review: drawn
    assert_eq!(cross("start", "handoff"), [true]); // build to review
    assert_eq!(cross("start", "yes"), [true]);
    assert_eq!(cross("build", "unit-gate"), [true]); // a unit gate
    assert_eq!(cross("enabled", "enabled-work"), [true]); // a plan input is in no box
    assert_eq!(cross("start", "result"), [true]); // nor is a plan output
    let html = view.body().unwrap();
    let html = html.as_str();
    let chip = |from: &str, to: &str| {
        let at = html
            .find(&format!("data-from=\"{from}\" data-to=\"{to}\""))
            .unwrap_or_else(|| panic!("a chip {from} -> {to}"));
        let start = html[..at].rfind("<a class=\"xref").unwrap();
        &html[start..start + html[start..].find("</a>").unwrap()]
    };
    let handoff = chip("s:start", "s:handoff");
    assert!(handoff.contains("class=\"xref k-handoff\""), "{handoff}");
    assert!(handoff.contains(&format!("href=\"/projects/id/{id}/steps/start\"")));
    assert!(handoff.contains("data-opens=\"start\""));
    assert!(handoff.contains("title=\"Handoff: start/text → data\""));
    assert!(handoff.contains("<span class=\"vh\">Handoff </span><span class=\"xm\">from </span><span class=\"xn\">start</span><span class=\"xm\">/text</span>"));
    let not = chip("s:start", "s:no");
    assert!(not.contains("k-negated_condition"), "{not}");
    assert!(not.contains("<span class=\"xm\">if not </span><span class=\"xn\">start</span><span class=\"xm\">/ok</span>"));
    let tolerant = chip("s:start", "s:cleanup");
    assert!(
        tolerant.contains("class=\"xref k-ordering tolerant\""),
        "{tolerant}"
    );
    // An ordering entry says what it is: "after start?", read as such (no hidden prefix).
    assert!(
        tolerant.contains("title=\"After start, even if it is skipped\"><span class=\"xm\">after </span><span class=\"xn\">start</span><span class=\"xm\">?</span>"),
        "{tolerant}"
    );
    let unit = chip("u:build", "s:unit-gate");
    assert!(
        unit.contains(&format!("href=\"/projects/id/{id}/units/build\"")),
        "{unit}"
    );
    assert!(
        unit.ends_with("\"><span class=\"xm\">after unit:</span><span class=\"xn\">build</span>"),
        "{unit}"
    );
    assert!(!unit.contains("data-opens"));
    let input = chip("i:enabled", "s:enabled-work");
    assert!(input.contains("href=\"#in-enabled\""), "{input}");
    assert!(html.contains("id=\"in-enabled\""));
    assert!(chip("s:start", "o:result").contains("title=\"Plan output result: start/text\""));
    // No chip points sideways: the words say the kind.
    assert!(!html.contains("←"), "an arrow glyph on a chip");
    // A card's chips come first in its stack, on its top edge, read before it; a card with
    // none is as before.
    for (to, card) in [("s:handoff", "n-handoff"), ("s:cleanup", "n-cleanup")] {
        let card = html.find(&format!("id=\"{card}\"")).unwrap();
        let stack = html[..card].rfind("<div class=\"stack\">").unwrap();
        let chips = &html[stack..card];
        assert!(
            chips.starts_with("<div class=\"stack\"><div class=\"xrefs\"><a class=\"xref")
                && chips.ends_with("</a></div><a "),
            "{chips}"
        );
        assert!(chips.contains(&format!("data-to=\"{to}\"")), "{chips}");
    }
    let summary = html.find("id=\"n-summary\"").unwrap();
    assert!(!html[..summary].ends_with("</a></div><a "));
    // A plan output's chips sit above it, as a card's do.
    let output = html.find("data-node=\"o:result\"").unwrap();
    let stack = html[..output].rfind("<div class=\"stack\">").unwrap();
    assert!(html[stack..output].contains("data-to=\"o:result\""));
    // No chip for a relation within a box.
    assert!(!html.contains("data-from=\"s:order\" data-to=\"s:summary\""));
    // A source counts its dependents in other boxes: start feeds five steps and the output;
    // the build unit, two unit gates.
    let start = &html[html.find("id=\"n-start\"").unwrap()..];
    let start = &start[..start.find("</a>").unwrap()];
    assert!(
        start.contains("<span aria-hidden=\"true\">→ 6</span>"),
        "{start}"
    );
    let label = &html[html.find("id=\"unit-build\"").unwrap()..];
    let label = &label[..label.find("</p>").unwrap()];
    assert!(label.contains("→ 2"), "{label}");
}

/// A chip whose source's card is folded away in a done unit says where it is.
#[test]
fn a_chip_says_where_its_source_is_when_a_done_unit_folds_its_card_away() {
    let plan = Plan::parse_json(
        &serde_json::to_vec(&json!({"steps":{
            "a":{"run":"core.external","outputs":{"text":"string"},"tags":["unit:one"]},
            "b":{"run":"core.external","in":{"x":{"source":"a/text"}},"tags":["unit:two"]}}}))
        .unwrap(),
        &Signatures,
    )
    .unwrap();
    let (_, mut state, project) = fixture();
    state.steps.clear();
    state.steps.insert(
        "a".parse().unwrap(),
        StepState {
            status: StepStatus::Succeeded,
            ..Default::default()
        },
    );
    let view = ProjectView::new(project, &plan, &state, 1);
    let html = view.body().unwrap();
    assert!(
        html.as_str()
            .contains("title=\"Handoff: a/text → x (in done unit one)\""),
        "{}",
        html.as_str()
    );
}

/// Done units read as one line each, "d2 · fork✓ rm–" in the lane strings' marks, and a run of
/// them (two or more) gathers on a closed shelf, "2 done units · 4 steps": under Live first
/// one shelf after the live work, under Plan order each run in its place. A search that
/// matches in a done unit draws the shelf and the unit open, under ids of their own, so a
/// shelf the page had closed opens to the matches.
#[test]
fn done_units_fold_to_lines_on_a_shelf_and_a_search_opens_them() {
    let plan = Plan::parse_json(
        &serde_json::to_vec(&json!({"steps":{
            "d1-fork":{"run":"core.external","tags":["unit:d1"]},
            "d1-rm":{"run":"core.external","after":["d1-fork"],"tags":["unit:d1"]},
            "live-work":{"run":"core.external","tags":["unit:live"]},
            "d2-fork":{"run":"core.external","tags":["unit:d2"]},
            "d2-rm":{"run":"core.external","after":["d2-fork"],"tags":["unit:d2"]},
            "d3-fork":{"run":"core.external","tags":["unit:d3"]},
            "d3-rm":{"run":"core.external","after":["d3-fork"],"tags":["unit:d3"]}}}))
        .unwrap(),
        &Signatures,
    )
    .unwrap();
    let (_, mut state, project) = fixture();
    state.steps.clear();
    for (id, status) in [
        ("d1-fork", StepStatus::Succeeded),
        ("d1-rm", StepStatus::Succeeded),
        ("d2-fork", StepStatus::Succeeded),
        ("d2-rm", StepStatus::Skipped),
        ("d3-fork", StepStatus::Succeeded),
        ("d3-rm", StepStatus::Succeeded),
    ] {
        state.steps.insert(
            id.parse().unwrap(),
            StepState {
                status,
                ..Default::default()
            },
        );
    }
    let query = |order: &str, q: &str| BoardQuery {
        order: Some(order.into()),
        q: Some(q.into()),
        ..Default::default()
    };
    let draw = |query: BoardQuery| {
        let mut view = ProjectView::new(project.clone(), &plan, &state, 1);
        query.apply(&mut view).unwrap();
        view.body().unwrap().as_str().to_owned()
    };
    // Live first: the live unit, then one closed shelf of the three done units.
    let html = draw(query("live", ""));
    let live = html.find("id=\"unit-live\"").unwrap();
    let shelf = html.find("<details id=\"shelf-d1\" class=\"done-shelf\" data-box=\"shelf:d1\" data-preserve-attr=\"open\">").expect("a closed shelf");
    assert!(live < shelf, "the shelf after the live work");
    assert!(html[shelf..].contains("3 done units · 6 steps"), "{html}");
    for unit in ["d1", "d2", "d3"] {
        assert!(
            html[shelf..].contains(&format!("id=\"unit-{unit}\"")),
            "{unit} on the shelf"
        );
        assert!(html.contains(&format!("<details id=\"fold-{unit}\" class=\"fold-box\" data-box=\"{unit}\" data-preserve-attr=\"open\">")));
    }
    // One line a unit: its glyph, id and lane string, the steps said in words too.
    assert!(html.contains("<span class=\"sid\">d2</span><span class=\"fb-meta\"><span class=\"fb-lane\" aria-hidden=\"true\" title=\"fork✓ rm–\">fork<span class=\"lm\">✓</span> rm<span class=\"lm\">–</span></span><span class=\"vh\">, 2 steps done, 1 skipped</span></span>"), "{html}");
    // Plan order: a lone done unit stays a line in place; the run after the live unit shelves.
    let html = draw(query("plan", ""));
    let d1 = html.find("id=\"unit-d1\"").unwrap();
    let live = html.find("id=\"unit-live\"").unwrap();
    let shelf = html.find("id=\"shelf-d2\"").expect("a shelf of d2 and d3");
    assert!(d1 < live && live < shelf, "{html}");
    assert!(
        !html[..live].contains("done-shelf"),
        "one done unit is not a shelf"
    );
    assert!(html[shelf..].contains("2 done units · 4 steps"));
    // A search matching in done units opens the shelf and them, under ids of their own.
    let html = draw(query("live", "rm"));
    assert!(html.contains("<details id=\"shelf-d1-open\" class=\"done-shelf\" data-box=\"shelf:d1\" open data-forced data-preserve-attr=\"open\">"), "{html}");
    assert!(html.contains("<details id=\"hit-d2\" class=\"fold-box\" data-box=\"d2\" open data-forced data-preserve-attr=\"open\">"), "{html}");
    assert!(html.contains("data-step=\"d2-rm\""));
    // Show: Done asks for them: its shelf is open too.
    let mut view = ProjectView::new(project.clone(), &plan, &state, 1);
    BoardQuery {
        show: Some("done".into()),
        ..Default::default()
    }
    .apply(&mut view)
    .unwrap();
    assert!(
        view.body()
            .unwrap()
            .as_str()
            .contains("id=\"shelf-d1-open\"")
    );
}

/// A lash lane's later steps (gated by conditions, afters and handoffs from earlier ones) read
/// their own status on a done unit's line: every step succeeded is a ✓, never the pending dot.
#[test]
fn a_done_lanes_line_marks_every_step_by_its_own_status() {
    let plan = Plan::parse_json(
        &serde_json::to_vec(&json!({"steps":{
            "l-fork":{"run":"core.external","outputs":{"path":"string"},"tags":["unit:l"]},
            "l-work":{"run":"core.external","in":{"cwd":{"source":"l-fork/path"}},"outputs":{"ready":"boolean","evidence":"string"},"tags":["unit:l"]},
            "l-land":{"run":"core.external","in":{"fork":{"source":"l-fork/path"},"ready":{"source":"l-work/ready"}},"outputs":{"landed":"boolean","landed_sha":"string"},"tags":["unit:l"]},
            "l-landed":{"run":"core.external","in":{"sha":{"source":"l-land/landed_sha"}},"after":["l-land/landed"],"tags":["unit:l","exit"]},
            "l-close":{"run":"core.external","in":{"evidence":{"source":"l-work/evidence"}},"after":["l-land/landed","l-landed"],"tags":["unit:l"]},
            "l-rm":{"run":"core.external","in":{"path":{"source":"l-fork/path"}},"after":["l-close"],"tags":["unit:l"]}}}))
        .unwrap(),
        &Signatures,
    )
    .unwrap();
    let (_, mut state, project) = fixture();
    state.steps.clear();
    for id in ["l-fork", "l-work", "l-land", "l-landed", "l-close", "l-rm"] {
        state.steps.insert(
            id.parse().unwrap(),
            StepState {
                status: StepStatus::Succeeded,
                ..Default::default()
            },
        );
    }
    let view = ProjectView::new(project, &plan, &state, 1);
    let unit = &view.units[0];
    assert!(unit.done);
    assert_eq!(unit.lane(), "fork✓ work✓ land✓ landed✓ close✓ rm✓");
    let html = view.body().unwrap();
    assert!(
        html.as_str().contains("land<span class=\"lm\">✓</span> landed<span class=\"lm\">✓</span> close<span class=\"lm\">✓</span> rm<span class=\"lm\">✓</span>"),
        "{}",
        html.as_str()
    );
}
