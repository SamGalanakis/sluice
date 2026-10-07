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
    let program = format!("root = Markdown({})", serde_json::to_string("## A\n#### B\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n<script>alert(1)</script>\n\n**text** <b>raw</b>\n\n- one\n  - two\n\n```rust\n<thing>\n```").unwrap());
    let response = f
        .router()
        .oneshot(
            Request::post(format!("{settings}/board/preview"))
                .body(Body::from(program))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    for text in [
        "<h4>A</h4>",
        "<h6>B</h6>",
        "<table>",
        "<strong>text</strong>",
        "&lt;script&gt;",
        "&lt;b&gt;raw&lt;/b&gt;",
        "language-rust",
        "&lt;thing&gt;",
    ] {
        assert!(html.contains(text), "{text}: {html}");
    }
    assert!(!html.contains("<script>"));
    assert_eq!(html.matches("<ul>").count(), 2);
    let problems = preview("root = Stack([a])").await;
    assert!(
        problems.contains("line 1: a is used but never defined"),
        "{problems}"
    );
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
    let escaped_id = f.project("escaped", json!({"steps":{
        "start":{"run":"core.external","doc":"A <script>failure</script>","outputs":{"text":"string"},"tags":["unit:build"]},
        "done":{"run":"core.external","tags":["unit:finished"]}
    }}), &[("start","failed"),("done","skipped")]).await;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let error = serde_json::to_string(&sluice_model::error::PublicError::BadRequest {
                message: "A <script>failure</script>".into(),
            })?;
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='start'",
                (escaped_id.to_string(), error),
            )?;
            tx.sql().execute(
                "UPDATE projects SET description='Fixture <script>' WHERE project_id=?1",
                [escaped_id.to_string()],
            )?;
            tx.changed(Some(escaped_id), "status");
            Ok(())
        })
        .await
        .unwrap();
    let (status, escaped) = f.get(&format!("/projects/id/{escaped_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(escaped.contains("data-node=\"u:build\""));
    assert!(escaped.contains("fold-finished"));
    assert!(escaped.contains("data-preserve-attr=\"open\""));
    assert!(!escaped.contains("<script>failure"));
    assert!(escaped.contains("&lt;script&gt;"));
    let region = between(&escaped, "id=\"project-board\"", "</sluice-board>");
    assert!(!region.contains("sluice-drawer"));
    assert!(!region.contains("data-init"));
    // By id: each unit keeps only its matching steps; the count says how many.
    let html = page("q=review").await;
    assert_eq!(cards(&html), ["beta-review", "alpha-review"], "live order");
    assert!(html.contains("2 steps match “review”."), "{html}");
    // By doc, in any case: a unit with no match is hidden whole.
    let html = page("q=PARSER").await;
    assert_eq!(cards(&html), ["beta-review"]);
    assert!(!html.contains("id=\"unit-alpha\""), "{html}");
    assert!(html.contains("1 step matches “PARSER”."));
    // A chip whose source the search left out says so in its title.
    assert!(
        html.contains("title=\"After alpha-review (not shown in this view)\""),
        "{html}"
    );
    assert!(!page("").await.contains("not shown in this view"));
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

/// The board's head is the program's own title when a level-1 Heading leads it (drawn once,
/// not again under "Board"), else "Board"; under it, when the program was written, and that
/// the plan has changed since when a plan edit came after it.
#[tokio::test]
async fn the_board_head_takes_the_programs_title_and_says_when_its_words_last_changed() {
    let f = Fixture::new().await;
    let path = format!("/projects/id/{}", f.id);
    let (_, html) = f.get(&path).await;
    let board = between(&html, "<aside id=\"board-pane\"", "</aside>");
    let head = between(board, "<div class=\"board-head\">", "</div>");
    assert!(
        head.contains("<h2 id=\"board-h\" class=\"board-h\">") && head.contains("Lanes</h2>"),
        "{head}"
    );
    assert!(!board.contains("class=\"ou-h\">Lanes<"), "{board}");
    assert!(
        head.contains("Updated <time data-ago datetime=\""),
        "{head}"
    );
    assert!(!head.contains("the plan has changed since"), "{head}");
    // A plan edit after the board: its words may be behind.
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "INSERT INTO plan_edits(project_id,rev,seq,at,author,reason,ops) VALUES (?1,99,(SELECT max(seq)+1 FROM records),'2099-01-01T00:00:00Z','orch','later','[]')",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "plan");
            Ok(())
        })
        .await
        .unwrap();
    let (_, html) = f.get(&path).await;
    let head = between(&html, "<div class=\"board-head\">", "</div>").to_owned();
    assert!(head.contains("; the plan has changed since.</p>"), "{head}");
    // A slot set after the plan edit brings the board's words up to date: its time is the head's.
    let slot_at = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::board_slot_set(
                tx,
                &ProjectSelector::Id(id),
                projects::SetBoardSlot {
                    key: "phase".into(),
                    markdown: Some("Now **green**.".into()),
                    author: "orch".into(),
                },
            )?;
            Ok(tx.sql().query_row(
                "SELECT at FROM records WHERE kind='project.update' ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap();
    let (_, html) = f.get(&path).await;
    let head = between(&html, "<div class=\"board-head\">", "</div>").to_owned();
    assert!(
        head.contains(&format!("datetime=\"{slot_at}\"")),
        "{slot_at} {head}"
    );
    assert!(!head.contains("the plan has changed since"), "{head}");
    // A program without a title of its own: "Board", which the narrow switch already names.
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::board_set(
                tx,
                &ProjectSelector::Id(id),
                projects::SetBoard {
                    program: Some("root = Stack([Heading(\"Two\", 2), Units()])".into()),
                    expected_rev: None,
                    reason: None,
                    author: "orch".into(),
                },
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (_, html) = f.get(&path).await;
    let head = between(&html, "<div class=\"board-head\">", "</div>").to_owned();
    assert!(
        head.contains("class=\"board-h generic\">") && head.contains("Board</h2>"),
        "{head}"
    );
    assert!(!head.contains("the plan has changed since"), "{head}");
    assert!(html.contains("class=\"ou-h\">Two</h4>"), "{html}");
}

/// `Output` shows the freshest value: a running step's progress (marked live, with when it was
/// set) over its older outputs; its outputs once it has finished with them after the progress;
/// the progress again, marked as progress, when its run ended without outputs. The step's
/// page shows a Progress section while it is the fresher.
#[tokio::test]
async fn output_shows_progress_while_it_is_fresher_than_the_outputs() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "rolling",
            json!({"steps":{"tests-main":{"run":"custom.open","outputs":{"red":"int","head":"string"}}}}),
            &[("tests-main", "running")],
        )
        .await;
    // Older outputs (a previous run's), then this run's progress.
    let set = |sql: &'static str| {
        let writer = f.writer.clone();
        async move {
            writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    tx.sql().execute(sql, [id.to_string()])?;
                    tx.changed(Some(id), "status");
                    Ok(())
                })
                .await
                .unwrap()
        }
    };
    set("UPDATE steps SET outputs='{\"red\":9,\"head\":\"old\"}',progress='{\"red\":2}',progress_at='2026-10-06T10:00:00.5Z',progress_run='019a2b3c-4d5e-7f01-8234-56789abcdef0' WHERE project_id=?1").await;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::board_set(
                tx,
                &ProjectSelector::Id(id),
                projects::SetBoard {
                    program: Some("root = Stack([red, head, metric])\nred = Output(\"tests-main\", \"red\")\nhead = Output(\"tests-main\", \"head\")\nmetric = Metric(\"Red\", \"SELECT json_extract(progress, '$.red') FROM steps WHERE project_id = ? AND step_id = 'tests-main'\")".into()),
                    expected_rev: Some(sluice_model::ids::Revision(0)),
                    reason: None,
                    author: "orch".into(),
                },
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let page = format!("/projects/id/{id}");
    let step = format!("/projects/id/{id}/steps/tests-main");
    let output = |html: &str, field: &str| {
        between(html, &format!("tests-main/{field}</span>"), "</div>").to_owned()
    };
    let (_, html) = f.get(&page).await;
    let red = output(&html, "red");
    assert!(red.contains("<span class=\"v num\">2</span>"), "{red}");
    assert!(
        red.contains("<span class=\"tag live\">")
            && red.contains("live</span><time data-ago datetime=\"2026-10-06T10:00:00.5Z\">2026-10-06 10:00 UTC</time>"),
        "{red}"
    );
    // A field the progress lacks shows the output, unmarked.
    let head = output(&html, "head");
    assert!(
        head.contains("<span class=\"v\">old</span>") && !head.contains("tag"),
        "{head}"
    );
    assert!(html.contains("<span class=\"metric-v\">2</span><span class=\"metric-l\">Red</span>"));
    let (_, detail) = f.get(&step).await;
    let section = between(
        &detail,
        "<section class=\"d-sec d-progress\">",
        "</section>",
    );
    assert!(
        section.contains("<h3>Progress</h3>") && section.contains("live</span>"),
        "{section}"
    );
    assert!(section.contains("by its current run"), "{section}");
    assert!(
        section.contains("<span class=\"v num\">2</span>"),
        "{section}"
    );
    // Finished with outputs after the progress was set: the outputs are the fresher values.
    set("INSERT INTO step_results(result_id,project_id,step_id,generation,declaration,status,outputs,recorded_at) VALUES ('019a2b3c-4d5e-7f01-8234-56789abcdef1',?1,'tests-main',1,'{}','succeeded','{\"red\":0,\"head\":\"new\"}','2026-10-06T10:00:01Z')").await;
    set("UPDATE steps SET status='succeeded',outputs='{\"red\":0,\"head\":\"new\"}',result_id='019a2b3c-4d5e-7f01-8234-56789abcdef1' WHERE project_id=?1").await;
    let (_, html) = f.get(&page).await;
    let red = output(&html, "red");
    assert!(
        red.contains("<span class=\"v num\">0</span>") && !red.contains("tag"),
        "{red}"
    );
    let (_, detail) = f.get(&step).await;
    assert!(!detail.contains("d-progress"), "{detail}");
    // A run that ended without outputs leaves its progress the fresher, marked as progress.
    set("UPDATE steps SET status='failed',outputs=NULL WHERE project_id=?1").await;
    let (_, html) = f.get(&page).await;
    let red = output(&html, "red");
    assert!(
        red.contains("<span class=\"v num\">2</span>")
            && red.contains("<span class=\"tag muted\">progress</span>")
            && !red.contains("live"),
        "{red}"
    );
    let (_, detail) = f.get(&step).await;
    let section = between(
        &detail,
        "<section class=\"d-sec d-progress\">",
        "</section>",
    );
    assert!(section.contains("by its last run"), "{section}");
}
