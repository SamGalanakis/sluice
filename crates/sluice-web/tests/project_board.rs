//! The project's board (docs("board")): drawn beside the plan from the project's own data,
//! each data component filled on the server, a bad part an inline error box, and a Button a
//! say to the orchestrator refused once the board has moved on.
mod board_fixture;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use board_fixture::Fixture;
use serde_json::{Value, json};
use sluice_model::{commands::CommandRequest, ids::ProjectSelector};
use sluice_store::{RetrySafety, projects};
use tower::ServiceExt;

fn between<'a>(html: &'a str, start: &str, end: &str) -> &'a str {
    let from = html.find(start).unwrap_or_else(|| panic!("no {start}"));
    let to = from + html[from..].find(end).unwrap_or_else(|| panic!("no {end}"));
    &html[from..to]
}

#[tokio::test]
async fn every_data_component_is_filled_from_the_project_and_a_bad_query_is_an_error_box() {
    let f = Fixture::new().await;
    let (status, html) = f.get(&format!("/projects/id/{}", f.id)).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("class=\"project-page has-panel\""));
    let board = between(&html, "<aside id=\"board-pane\"", "</aside>");
    // Units: the units view's rows, each linking its unit.
    let units = between(board, "board-units", "</table>");
    assert!(
        units.contains(&format!("href=\"/projects/id/{}/units/alpha\"", f.id)),
        "{units}"
    );
    assert!(
        units.contains(">beta</a>") && units.contains("<span>failed</span>"),
        "{units}"
    );
    // Metric: the first cell, with ? bound to the project's id.
    assert!(
        board.contains("<span class=\"metric-v\">4</span><span class=\"metric-l\">Steps</span>"),
        "{board}"
    );
    assert!(
        board
            .contains("<span class=\"metric-v\">1</span><span class=\"metric-l\">Succeeded</span>")
    );
    // StepStatus: the step's chip with its state and why it stopped.
    let step = between(board, "<div class=\"board-step\">", "</div>");
    assert!(
        step.contains("is-failed") && step.contains(">beta-build<"),
        "{step}"
    );
    assert!(step.contains("tests failed"), "{step}");
    // Output: the value, escaped.
    assert!(
        board
            .contains("alpha-build/summary</span><span class=\"v\">Built &lt;12&gt; crates</span>"),
        "{board}"
    );
    // Query: a table with its caption.
    let table = between(board, "<caption>Every step</caption>", "</table>");
    assert_eq!(table.matches("<tr>").count(), 5, "{table}");
    // Chart: an SVG of bars, one per row, named for screen readers; and a line.
    let bars = between(board, "bar-chart", "</svg>");
    assert_eq!(bars.matches("<rect").count(), 3, "{bars}");
    assert!(
        bars.contains("aria-label=\"Steps by status: failed 1, pending 2, succeeded 1\""),
        "{bars}"
    );
    assert!(board.contains("<polyline class=\"c-line\""));
    // A bad query is an inline error box naming the component and its line; the rest draws.
    let error = between(board, "<div class=\"ou-error-box\"", "</div></div>");
    assert!(
        error.contains("Query (line 12)") && error.contains("no such table"),
        "{error}"
    );
    assert!(board.contains("<button type=\"submit\" name=\"button\" value=\"0\" class=\"primary\">Retry the lane</button>"));
    // The plan is still all there.
    assert!(html.contains("id=\"n-beta-review\""));
    // A project with no board draws none, and no switch.
    let (_, plain) = f.get(&format!("/projects/id/{}", f.plain)).await;
    assert!(
        !plain.contains("has-panel")
            && !plain.contains("board-pane")
            && !plain.contains("view-switch")
    );
}

#[tokio::test]
async fn a_button_says_to_the_orchestrator_and_a_stale_board_refuses_it() {
    let f = Fixture::new().await;
    let action = format!("/projects/id/{}/board/action", f.id);
    let (status, reply) = f
        .post(
            &action,
            &[
                ("board_rev", "1"),
                ("button", "0"),
                ("field-0", "gamma"),
                ("field-1", "go now"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["message"], "Sent to the orchestrator.");
    let sent = f.commands.0.lock().unwrap().clone();
    let [CommandRequest::Say(say)] = sent.as_slice() else {
        panic!("{sent:?}")
    };
    assert_eq!(
        (say.to.as_str(), say.owner, say.run),
        ("orchestrator", true, None)
    );
    assert_eq!(say.project, ProjectSelector::Id(f.id));
    assert_eq!(say.body, "Board: Retry the lane");
    assert_eq!(
        serde_json::to_value(&say.data).unwrap(),
        json!({"board_rev": 1, "action": "retry_lane", "params": {"force": true},
               "values": {"lane": "gamma", "note": "go now"}})
    );
    // A primary button checks its fields' rules first; a secondary one does not.
    let (status, reply) = f
        .post(
            &action,
            &[
                ("board_rev", "1"),
                ("button", "0"),
                ("field-0", ""),
                ("field-1", "x"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{reply}");
    let errors: Vec<Value> = reply["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| serde_json::from_str(e.as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(errors[0]["field"], "field-0");
    assert_eq!(errors[1]["name"], "note");
    let (status, _) = f
        .post(
            &action,
            &[("board_rev", "1"), ("button", "1"), ("field-0", "")],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // The board moves on: a press on the page drawn before is refused, visibly, and nothing is sent.
    f.writer
        .write(RetrySafety::NonIdempotent, {
            let id = f.id;
            move |tx| {
                projects::board_set(
                    tx,
                    &ProjectSelector::Id(id),
                    projects::SetBoard {
                        program: Some("root = Button(\"Again\", \"again\")".into()),
                        expected_rev: None,
                        reason: None,
                        author: "orch".into(),
                    },
                )
            }
        })
        .await
        .unwrap();
    let before = f.commands.0.lock().unwrap().len();
    let (status, reply) = f
        .post(
            &action,
            &[("board_rev", "1"), ("button", "0"), ("field-0", "gamma")],
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{reply}");
    assert!(
        reply["message"]
            .as_str()
            .unwrap()
            .starts_with("The board changed since this page was drawn"),
        "{reply}"
    );
    assert_eq!(f.commands.0.lock().unwrap().len(), before);
}

#[tokio::test]
async fn settings_save_clear_and_preview_the_board() {
    let f = Fixture::new().await;
    let settings = format!("/projects/id/{}/settings", f.id);
    let (_, page) = f.get(&settings).await;
    assert!(
        page.contains("id=\"board-rev\" value=\"1\""),
        "{}",
        between(&page, "board-settings", "</section>")
    );
    assert!(page.contains("Lanes"));
    // A stale save is refused and keeps the draft.
    let form = |op: &'static str, rev: &'static str, program: &'static str| {
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([("op", op), ("expected_rev", rev), ("program", program)])
            .finish()
    };
    let send = |body: String| {
        let router = f.router();
        let path = format!("{settings}/board");
        async move {
            let response = router
                .oneshot(
                    Request::post(path)
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            (status, String::from_utf8(body.to_vec()).unwrap())
        }
    };
    let (status, html) = send(form("save", "0", "root = Text(\"draft\")")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let feedback = between(&html, "id=\"board-feedback\"", "</span>");
    assert!(feedback.contains("the board changed"), "{feedback}");
    assert!(
        html.contains(">root = Text(&#34;draft&#34;)</textarea>"),
        "{}",
        between(&html, "board-program", "</textarea>")
    );
    // A program that does not check lists its lines.
    let (status, html) = send(form("save", "1", "root = Nope()")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        html.contains("line 1: Nope is not a board component"),
        "{html}"
    );
    let (status, html) = send(form("save", "1", "root = Text(\"saved\")")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Saved (rev 2)") && html.contains("id=\"board-rev\" value=\"2\""));
    let (status, html) = send(form("clear", "2", "")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Cleared (rev 3)"));
    let (_, plan) = f.get(&format!("/projects/id/{}", f.id)).await;
    assert!(!plan.contains("board-pane"));
    // The preview draws a draft with the project's data, or its problems.
    let preview = |program: &'static str| {
        let router = f.router();
        let path = format!("{settings}/board/preview");
        async move {
            let response = router
                .oneshot(Request::post(path).body(Body::from(program)).unwrap())
                .await
                .unwrap();
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            String::from_utf8(body.to_vec()).unwrap()
        }
    };
    let drawn =
        preview("root = Metric(\"Steps\", \"SELECT count(*) FROM steps WHERE project_id = ?\")")
            .await;
    assert!(
        drawn.contains("<span class=\"metric-v\">4</span>"),
        "{drawn}"
    );
    let problems = preview("root = Stack([a])").await;
    assert!(
        problems.contains("line 1: a is used but never defined"),
        "{problems}"
    );
}

#[test]
fn the_board_docs_and_the_question_drawer_name_the_same_vocabulary() {
    use sluice_model::openui;
    let docs = include_str!("../../../docs/agent/board.md");
    for spec in openui::board_components() {
        assert!(
            docs.contains(&format!("`{}`", spec.signature())),
            "docs(\"board\") lacks {}",
            spec.signature()
        );
    }
    // A question's ui is drawn in the browser (assets/openui.js): it knows exactly the
    // question components, with the same props in the same order.
    let js = include_str!("../assets/openui.js");
    let vocab = &js[js.find("const VOCAB = ").unwrap() + "const VOCAB = ".len()..];
    let vocab: Value = serde_json::from_str(&vocab[..vocab.find("};").unwrap() + 1]).unwrap();
    let drawn: Vec<(String, Vec<String>)> = vocab["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap().to_owned(),
                c["props"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| p[0].as_str().unwrap().to_owned())
                    .collect(),
            )
        })
        .collect();
    let ours: Vec<(String, Vec<String>)> = openui::QUESTION_COMPONENTS
        .iter()
        .map(|c| {
            (
                c.name.to_owned(),
                c.props
                    .iter()
                    .map(|p| format!("{}{}", p.name, if p.required { "" } else { "?" }))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(drawn, ours);
    let inbox = include_str!("../../../docs/agent/inbox.md");
    for spec in openui::QUESTION_COMPONENTS {
        assert!(
            inbox.contains(&format!("`{}`", spec.signature())),
            "{}",
            spec.signature()
        );
    }
}

/// The cards a page draws on the plan, by step id, in page order.
fn cards(html: &str) -> Vec<&str> {
    let plan = between(html, "<sluice-board", "</sluice-board>");
    plan.split("data-step=\"")
        .skip(1)
        .map(|rest| &rest[..rest.find('"').unwrap()])
        .collect()
}

#[tokio::test]
async fn the_search_keeps_the_steps_whose_id_doc_or_unit_match_and_combines_with_show() {
    let f = Fixture::new().await;
    let base = format!("/projects/id/{}", f.id);
    let page = |query: &str| {
        let path = format!("{base}?{query}");
        let f = &f;
        async move {
            let (status, html) = f.get(&path).await;
            assert_eq!(status, StatusCode::OK, "{path}: {html}");
            html
        }
    };
    // By id: each unit keeps only its matching steps; the count says how many.
    let html = page("q=review").await;
    assert_eq!(cards(&html), ["beta-review", "alpha-review"], "live order");
    assert!(html.contains("2 steps match “review”."), "{html}");
    // By doc, in any case: a unit with no match is hidden whole.
    let html = page("q=PARSER").await;
    assert_eq!(cards(&html), ["beta-review"]);
    assert!(!html.contains("id=\"unit-alpha\""), "{html}");
    assert!(html.contains("1 step matches “PARSER”."));
    // By unit id; words in any order, across id, doc and unit.
    assert_eq!(
        cards(&page("q=alpha&order=plan").await),
        ["alpha-build", "alpha-review"]
    );
    assert_eq!(cards(&page("q=output+beta").await), ["beta-review"]);
    // With show: attention keeps the failed unit, and the search narrows it.
    assert_eq!(cards(&page("q=build&show=attention").await), ["beta-build"]);
    let none = page("q=build&show=done").await;
    assert!(cards(&none).is_empty());
    assert!(none.contains("No step matches “build”."), "{none}");
    assert!(
        none.contains(&format!(
            "<a href=\"{base}?order=live&#38;show=done\" data-clear-q>Clear the search</a>"
        )),
        "{none}"
    );
    // The field keeps the search, escaped; the page's stream asks for the same view.
    let html = page("q=%3Cb%3E+x&show=active").await;
    assert!(html.contains("value=\"&#60;b&#62; x\""), "{html}");
    assert!(html.contains("“&#60;b&#62; x”"), "{html}");
    assert!(
        html.contains(&format!(
            "@get('{base}/stream?order=live&#38;show=active&#38;q=%3Cb%3E+x'"
        )),
        "{html}"
    );
    // Without a search there is no count, and every card shows.
    let html = page("").await;
    assert_eq!(cards(&html).len(), 4);
    assert!(html.contains("<p class=\"match-note meta\" role=\"status\"></p>"));
    // The live stream draws the board under the same search.
    use futures_util::StreamExt;
    let response = f
        .router()
        .oneshot(
            Request::get(format!(
                "{base}/stream?q=parser&datastar=%7B%22ver%22%3A%22old%22%7D"
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let first = String::from_utf8(body.next().await.unwrap().unwrap().to_vec()).unwrap();
    assert!(first.contains("selector #project-board"), "{first}");
    assert!(
        first.contains("data-step=\"beta-review\"") && !first.contains("data-step=\"alpha-build\""),
        "{first}"
    );
    assert!(first.contains("1 step matches “parser”."), "{first}");
}

#[tokio::test]
async fn the_board_side_has_a_separator_and_the_page_works_without_script() {
    let f = Fixture::new().await;
    let (_, html) = f.get(&format!("/projects/id/{}", f.id)).await;
    // The splitter: a focusable vertical separator that controls the board pane.
    let side = between(&html, "<div class=\"board-side\">", "</aside>");
    let splitter = between(side, "<div class=\"splitter\"", ">");
    for attr in [
        "role=\"separator\"",
        "aria-orientation=\"vertical\"",
        "aria-controls=\"board-pane\"",
        "aria-label=\"Board width\"",
        "aria-valuemin=\"320\"",
        "aria-valuemax=",
        "aria-valuenow=",
        "tabindex=\"0\"",
    ] {
        assert!(splitter.contains(attr), "{attr}: {splitter}");
    }
    assert!(side.contains("<aside id=\"board-pane\""), "{side}");
    // Plan · Both · Board.
    let switch = between(&html, "<div class=\"view-switch\"", "</div>");
    assert_eq!(switch.matches("data-view-tab=").count(), 3, "{switch}");
    assert!(switch.contains("data-view-tab=\"both\""));
    // Without script: the tools are a GET form with its Apply button, and the description's
    // rest is a closed <details> after its lead paragraph.
    let tools = between(&html, "<form class=\"board-tools\"", "</form>");
    assert!(
        tools.contains("method=\"get\"")
            && tools.contains("<button class=\"apply\">Apply</button>")
    );
    assert!(
        tools.contains("name=\"q\"")
            && tools.contains("name=\"order\"")
            && tools.contains("name=\"show\"")
    );
    let about = between(&html, "<div class=\"about\">", "<form");
    assert!(
        about.starts_with(
            "<div class=\"about\"><div class=\"md\"><p>Lanes: two units of &lt;work&gt;.</p>"
        ),
        "{about}"
    );
    let more = between(about, "<details class=\"about-more\"", "</details>");
    assert!(!more.contains(" open"), "folded by default: {more}");
    assert!(
        more.contains("<span class=\"am-more\">More</span>"),
        "{more}"
    );
    assert!(
        more.contains("How work is done here:") && more.contains("<li>reviews follow builds</li>")
    );
    assert!(
        !about[..about.find("<details").unwrap()].contains("How work"),
        "{about}"
    );
    // The stylesheet hides Apply and shows the splitter only with script.
    let (_, css) = f.get("/static/style.css").await;
    assert!(css.contains("@media (scripting: enabled) { .board-tools .apply { display: none; } }"));
    assert!(css.contains(".splitter { display: none;"));
    // A project with a one-paragraph description has nothing to fold.
    let (_, plain) = f.get(&format!("/projects/id/{}", f.plain)).await;
    assert!(!plain.contains("about-more") && !plain.contains("class=\"splitter\""));
}
