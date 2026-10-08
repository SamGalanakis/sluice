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
        error.contains("Query (line 14)") && error.contains("no such table"),
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
        "<h3>A</h3>",
        "<h5>B</h5>",
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
    // the done one-step unit is a row on the shelf, naming its unit before its step
    assert!(escaped.contains("<section id=\"unit-finished\" class=\"box solo done shelf-row\""));
    assert!(
        escaped.contains("<span class=\"uid\">finished /</span><span class=\"fb-name\"><span class=\"sref\"><span class=\"sref-t\">done</span></span></span>"),
        "{escaped}"
    );
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
    // A wait whose source the search left out is said in words, shown where lines are drawn.
    assert!(
        html.contains(&format!("<p class=\"waits said\">Waits for <a href=\"{base}/steps/alpha-review\" data-opens=\"alpha-review\" data-from=\"s:alpha-review\" data-to=\"s:beta-review\">alpha-review</a>, not in this view</p>")),
        "{html}"
    );
    assert!(!page("").await.contains("not in this view"));
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
    assert!(head.contains("Updated <time data-ago=\""), "{head}");
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
    // A document edit after the plan edit brings the board's words up to date: its time is
    // the head's.
    let slot_at = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::board_doc_write(
                tx,
                &ProjectSelector::Id(id),
                projects::WriteBoardDoc {
                    markdown: "Now **green**.".into(),
                    expected_rev: None,
                    reason: None,
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
    // The written document's own line is then the board's one time: the head leaves it out.
    let (_, html) = f.get(&path).await;
    let head = between(&html, "<div class=\"board-head\">", "</div>").to_owned();
    assert!(!head.contains("Updated"), "{head}");
    let doc = between(&html, "<div class=\"board-doc\">", "</div></div>");
    assert!(
        doc.contains(&format!("Edited <time data-ago=\"{slot_at}\"")),
        "{slot_at} {doc}"
    );
    assert!(!doc.contains("the plan has changed since"), "{doc}");
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
    // Under the column's h2, without a title of its own: a level-2 Heading is h4, under the
    // h3 a level-1 would be; with a title, h3.
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
            && red.contains("live</span><time data-ago=\"2026-10-06T10:00:00.5Z\" datetime=\"2026-10-06T10:00:00.5Z\" title=\"2026-10-06 10:00 UTC\">2026-10-06 10:00 UTC</time>"),
        "{red}"
    );
    // A field the progress lacks shows the output, unmarked.
    let head = output(&html, "head");
    assert!(
        head.contains("<span class=\"v\">old</span>") && !head.contains("tag"),
        "{head}"
    );
    assert!(html.contains("<span class=\"metric-v\">2</span><span class=\"metric-l\">Red</span>"));
    // While it runs its live progress is part of what it is doing now (it leads: the step has
    // written no message), not a section of its own.
    let (_, detail) = f.get(&step).await;
    assert!(!detail.contains("d-progress"), "{detail}");
    let section = between(&detail, "<section class=\"d-sec d-now\">", "</section>");
    assert!(
        section.contains(">Now</") && section.contains("Progress, set"),
        "{section}"
    );
    assert!(
        !section.contains("It has sent no message yet."),
        "{section}"
    );
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

/// The relations the page hands `<sluice-board>` to draw (its `edges` attribute, unescaped).
fn edges(html: &str) -> Vec<Value> {
    let raw = between(html, "edges=\"", "\">");
    let json = raw["edges=\"".len()..]
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#38;", "&");
    serde_json::from_str(&json).unwrap()
}
/// Whether the relation `from` → `to` (step ids) is in the edges data, and is drawn as a line.
fn line(edges: &[Value], from: &str, to: &str) -> Option<(bool, bool)> {
    edges
        .iter()
        .find(|e| e["from"]["id"] == from && e["to"]["id"] == to)
        .map(|e| (e["cross"] == true, e["line"] == true))
}

/// The plan reads without a key: a one-step unit is its card once, a wait between units is a
/// line in the edges data (a satisfied one is not), nothing on the board needs a legend or an
/// arrow count, and Live first draws stopped, running, then waiting by depth, then one
/// shelf of done units; a card says in words what it waits for, for a phone.
#[tokio::test]
async fn the_plan_draws_waits_between_units_as_lines_and_one_step_units_once() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "graph",
            json!({"steps":{
                "k1":{"run":"custom.open","tags":["unit:k1"]},
                "old":{"run":"custom.open","tags":["unit:old"]},
                "stopped":{"run":"custom.open","tags":["unit:stopped"]},
                "k2":{"run":"custom.open","after":["k1"],"tags":["unit:k2"]},
                "k3":{"run":"custom.open","after":["k2"],"tags":["unit:k3"]},
                "k4":{"run":"custom.open","after":["k3","k1"],"tags":["unit:k4"]},
                "compile":{"run":"custom.open","after":["k2"],"tags":["unit:build"]},
                "lane-fork":{"run":"custom.open","tags":["unit:lane"]},
                "lane-work":{"run":"custom.open","after":["lane-fork","k2"],"tags":["unit:lane"]},
                "pkg":{"run":"custom.open","tags":["unit:pkg"]},
                "pkg-rm":{"run":"custom.open","after":["pkg"],"tags":["unit:pkg"]}}}),
            &[
                ("k1", "succeeded"),
                ("old", "succeeded"),
                ("stopped", "failed"),
                ("k2", "running"),
                ("lane-fork", "succeeded"),
                ("pkg", "succeeded"),
                ("pkg-rm", "succeeded"),
            ],
        )
        .await;
    let base = format!("/projects/id/{id}");
    let (status, html) = f.get(&base).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let board = between(&html, "<sluice-board", "</sluice-board>");
    let plane = between(&html, "<div class=\"plane\"", "</sluice-board");
    // one-step units are their card alone; a step named apart from its unit names both
    assert!(
        board.contains("<section id=\"unit-k3\" class=\"box solo\""),
        "{board}"
    );
    assert_eq!(
        board.matches("<span class=\"sid\">k3</span>").count(),
        1,
        "{board}"
    );
    assert!(!board.contains("box-label\">k3<"), "{board}");
    assert!(
        board.contains("<span class=\"uid\">build /</span><span class=\"sid\">compile</span>"),
        "{board}"
    );
    assert!(
        board.contains("<p class=\"box-label\"><span class=\"u-meta\"><code class=\"u-id\">lane</code></span></p>"),
        "a unit of steps keeps its box"
    );
    // a step named as its unit is that unit's mark alone in a done line
    assert!(board.contains("title=\"✓ rm✓\""), "{board}");
    // waits between units are lines; a satisfied one is not drawn
    let edges = edges(&html);
    assert_eq!(line(&edges, "k2", "k3"), Some((true, true)));
    assert_eq!(line(&edges, "k3", "k4"), Some((true, true)));
    assert_eq!(line(&edges, "k2", "lane-work"), Some((true, true)));
    assert_eq!(line(&edges, "k2", "compile"), Some((true, true)));
    assert_eq!(
        line(&edges, "k1", "k4"),
        Some((true, false)),
        "k1 is through"
    );
    assert_eq!(line(&edges, "k1", "k2"), Some((true, false)));
    assert_eq!(line(&edges, "lane-fork", "lane-work"), Some((false, true)));
    // no key, no arrow counts, no chips
    for gone in ["legend", "lg-line", "xout", "xref", "→"] {
        assert!(!plane.contains(gone), "{gone}: {plane}");
    }
    // the waits in words, named and linked, with what a source is doing; a satisfied one unsaid
    let to = |step: &str, from: &str| {
        format!(
            "<a href=\"{base}/steps/{from}\" data-opens=\"{from}\" data-from=\"s:{from}\" data-to=\"s:{step}\">{from}</a>"
        )
    };
    assert!(
        board.contains(&format!(
            "<p class=\"waits\">Waits for {} (running)</p>",
            to("k3", "k2")
        )),
        "{board}"
    );
    assert!(
        board.contains(&format!(
            "<p class=\"waits\">Waits for {}</p>",
            to("k4", "k3")
        )),
        "{board}"
    );
    // a finished step is never "next"
    assert!(
        board.contains("id=\"n-k1\" class=\"node shelf-step is-succeeded\" "),
        "{board}"
    );
    // Live first: stopped, running, waiting by depth (k4 under k3), then one done shelf
    let at = |s: &str| plane.find(s).unwrap_or_else(|| panic!("no {s}: {plane}"));
    let order = [
        "<h2 class=\"band-h\">Stopped</h2>",
        "id=\"unit-stopped\"",
        "<h2 class=\"band-h\">Running</h2>",
        "id=\"unit-k2\"",
        "<h2 class=\"band-h\">Waiting</h2>",
        "id=\"unit-k3\"",
        "id=\"unit-k4\"",
        "class=\"done-shelf\"",
        "id=\"unit-k1\"",
    ];
    for pair in order.windows(2) {
        assert!(at(pair[0]) < at(pair[1]), "{} before {}", pair[0], pair[1]);
    }
    assert!(plane[at("id=\"unit-k3\"")..at("id=\"unit-k4\"")].contains("<div class=\"layer\">"));
    assert_eq!(plane.matches("class=\"done-shelf\"").count(), 1);
    assert!(plane.contains("3 done units · 4 steps"), "{plane}");
    assert!(between(&html, "<div class=\"sumline\">", "</p>").contains("b-failed"));
    // Plan order: one band without a label, the same single shelf at the end
    let (_, html) = f.get(&format!("{base}?order=plan")).await;
    let plane = between(&html, "<div class=\"plane\"", "</sluice-board");
    assert!(!plane.contains("band-h"), "{plane}");
    assert_eq!(plane.matches("class=\"done-shelf\"").count(), 1);
    assert!(plane.find("id=\"unit-k4\"").unwrap() < plane.find("class=\"done-shelf\"").unwrap());
    // Attention keeps the stopped unit and leaves out running work
    let (_, html) = f.get(&format!("{base}?show=attention")).await;
    assert!(html.contains("id=\"unit-stopped\""), "{html}");
    assert!(!html.contains("id=\"unit-k2\""), "{html}");
    // a plan without steps has nothing to search and says so
    let (_, html) = f.get(&format!("/projects/id/{}", f.plain)).await;
    assert!(html.contains("The plan has no steps yet."), "{html}");
    assert!(!html.contains("class=\"board-tools\""), "{html}");
}

#[tokio::test]
async fn a_cancel_reads_as_cancelled_in_the_units_table_and_on_its_card() {
    let f = Fixture::new().await;
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='beta-build'",
                (
                    id.to_string(),
                    json!({"error":"fn_failure","message":"cancelled: pivot (Sam)"}).to_string(),
                ),
            )?;
            tx.changed(Some(id), "plan");
            Ok(())
        })
        .await
        .unwrap();
    let (status, html) = f.get(&format!("/projects/id/{}", f.id)).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let board = between(&html, "<aside id=\"board-pane\"", "</aside>");
    let units = between(board, "board-units", "</table>");
    // the unit stopped only by a cancel: "cancelled", its step marked ■, never ✗ failed
    assert!(units.contains("<span>cancelled</span>"), "{units}");
    assert!(!units.contains("<span>failed</span>"), "{units}");
    assert!(units.contains("build■"), "{units}");
    let key = between(board, "u-key", "</p>");
    assert!(
        key.contains("■</span> cancelled") && !key.contains("failed"),
        "{key}"
    );
    // the board's own count agrees: a cancel is not a failure
    assert!(
        board.contains("<span class=\"metric-v\">0</span><span class=\"metric-l\">Failed</span>"),
        "{board}"
    );
    // its unit's label says what in it needs someone
    assert!(
        html.contains(
            "<code class=\"u-id\">beta</code> <span class=\"box-alarm\">· 1 cancelled</span>"
        ),
        "{html}"
    );
    // its card says so in words, after its id
    let card = between(&html, "id=\"n-beta-build\"", "</a>");
    assert!(
        card.contains("is-cancelled") && card.contains(">cancelled</span>"),
        "{card}"
    );
}

#[tokio::test]
async fn the_shelf_draws_the_latest_done_units_and_sends_only_their_edges() {
    let f = Fixture::new().await;
    // 25 one-step units, each after the one before, all done
    let mut steps = serde_json::Map::new();
    let mut statuses = vec![];
    for i in 0..25 {
        let id = format!("u{i:02}");
        let mut step = json!({"run":"custom.open","tags":[format!("unit:{id}")]});
        if i > 0 {
            step["after"] = json!([format!("u{:02}", i - 1)]);
        }
        if i == 24 {
            step["doc"] = json!("Ship the parser fix");
        }
        steps.insert(id.clone(), step);
        statuses.push((&*Box::leak(id.into_boxed_str()), "succeeded"));
    }
    let id = f.project("shelf", json!({"steps": steps}), &statuses).await;
    // their runs ended a minute apart, u24 last
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            for i in 0..25 {
                let attempt = format!("019a2b3c-4d5e-7f01-8234-5678900000{i:02}");
                let run = format!("019a2b3c-4d5e-7f01-8234-5678901000{i:02}");
                tx.sql().execute(
                    "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?1,?2,?3,1,1,'terminal','{}','hash','now')",
                    (&attempt, id.to_string(), format!("u{i:02}")),
                )?;
                tx.sql().execute(
                    "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at,finished_at) VALUES (?1,?2,?3,?4,1,1,'2026-10-06T10:00:00Z','2026-10-06T10:00:00Z',?5)",
                    (&run, id.to_string(), &attempt, format!("u{i:02}"), format!("2026-10-06T11:{i:02}:00Z")),
                )?;
            }
            tx.changed(Some(id), "plan");
            Ok(())
        })
        .await
        .unwrap();
    let (status, html) = f.get(&format!("/projects/id/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let shelf = between(&html, "class=\"done-shelf\"", "class=\"shelf-more");
    assert!(shelf.contains("25 done units · 25 steps"), "{shelf}");
    assert_eq!(
        shelf.matches("class=\"box solo done shelf-row\"").count(),
        20,
        "{shelf}"
    );
    // newest first, each with when it finished
    assert!(
        shelf.find("id=\"n-u24\"").unwrap() < shelf.find("id=\"n-u23\"").unwrap(),
        "{shelf}"
    );
    assert!(!shelf.contains("id=\"n-u04\""), "{shelf}");
    // a row says its title first and its id after it in mono, once
    let row = between(shelf, "id=\"n-u24\"", "</a>");
    assert!(
        row.contains("<span class=\"fb-name\"><span class=\"sref\"><span class=\"sref-t\">Ship the parser fix</span> <code class=\"sref-id\">u24</code></span></span>"),
        "{row}"
    );
    assert_eq!(row.matches("Ship the parser fix").count(), 1, "{row}");
    assert!(
        row.contains("<span class=\"vh\"> took 1 hour 24 minutes</span>"),
        "{row}"
    );
    assert!(
        shelf.contains("<span class=\"fb-ago\"><time data-ago=\"2026-10-06T11:24:00Z\""),
        "{shelf}"
    );
    assert!(
        html.contains(&format!(
            "<a href=\"/projects/id/{id}?order=live&#38;show=done\">Show all 25</a>"
        )),
        "{html}"
    );
    // the edges data holds only the relations among what the page draws
    let drawn = edges(&html);
    assert!(drawn.len() < 24, "{drawn:?}");
    for e in &drawn {
        assert!(
            html.contains(&format!("id=\"n-{}\"", e["to"]["id"].as_str().unwrap())),
            "{e}"
        );
    }
    // Show: Done draws every one
    let (_, html) = f.get(&format!("/projects/id/{id}?show=done")).await;
    assert_eq!(
        html.matches("class=\"box solo done shelf-row\"").count(),
        25
    );
    assert_eq!(edges(&html).len(), 24);
}

#[tokio::test]
async fn a_failure_is_counted_as_the_dashboard_counts_it_and_the_unit_page_draws_its_lines() {
    let f = Fixture::new().await;
    let (status, html) = f.get(&format!("/projects/id/{}", f.id)).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let board = between(&html, "<aside id=\"board-pane\"", "</aside>");
    assert!(
        board.contains("<span class=\"metric-v\">1</span><span class=\"metric-l\">Failed</span>"),
        "{board}"
    );
    // a failed step's unit says so at its head, so a phone reads it before its cards
    assert!(
        html.contains(
            "<code class=\"u-id\">beta</code> <span class=\"box-alarm\">· 1 failed</span>"
        ),
        "{html}"
    );
    // the unit's own page: its steps summed, and the lines inside it drawn
    let (status, page) = f.get(&format!("/projects/id/{}/units/alpha", f.id)).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(
        page.contains(&format!("<p class=\"meta unit-sum\">2 steps · 1 pending · 1 succeeded · <a href=\"/projects/id/{}/log?unit=alpha\">Log</a></p>", f.id)),
        "{page}"
    );
    let edges = between(&page, "<sluice-board class=\"board\" edges=\"", "\">");
    assert!(
        edges.contains("alpha-build") && edges.contains("alpha-review"),
        "{edges}"
    );
    assert!(
        !edges.contains("beta"),
        "only the lines inside the unit: {edges}"
    );
    assert!(page.contains("<svg class=\"edges\""), "{page}");
}

/// A paused step has one name, "paused", and is counted as paused everywhere it is counted:
/// the summary line and its bar, the home row, and the step page, which says who paused it
/// (from the plan edit that did) instead of "Waits on: paused".
#[tokio::test]
async fn a_paused_step_is_named_and_counted_paused_on_every_surface() {
    let f = Fixture::new().await;
    let held = f
        .project(
            "held",
            json!({"steps":{
                "hold":{"run":"custom.open","doc":"Hold the release","paused":true},
                "next":{"run":"custom.open","after":["hold"]},
                "done":{"run":"custom.open"}}}),
            &[("done", "succeeded")],
        )
        .await;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let edit: sluice_model::events::Event = serde_json::from_value(json!({
                "kind":"plan.edit","rev":2,"author":"owner","reason":"",
                "ops":[{"op":"add","path":"/steps/hold/paused","value":true}]}))
            .unwrap();
            tx.append_record(Some(held), edit)?;
            Ok(())
        })
        .await
        .unwrap();
    let (status, html) = f.get(&format!("/projects/id/{held}")).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let sum = between(&html, "class=\"sum-n", "</span>");
    assert!(sum.contains("3 steps · 1 succeeded · 1 paused"), "{sum}");
    assert!(
        html.contains("<i class=\"b-paused\" style=\"flex:1\">"),
        "{html}"
    );
    let reads = sluice_store::ReadPool::open(f._home.path(), 1).unwrap();
    let snapshot = reads
        .snapshot(|c| {
            sluice_web::views::load_snapshot(c, sluice_web::views::FunctionCatalog::default())
        })
        .await
        .unwrap();
    let project = snapshot.projects.iter().find(|p| p.id == held).unwrap();
    assert_eq!(
        (
            project.counts.get(sluice_web::views::ui::Shown::Paused),
            project.counts.get(sluice_web::views::ui::Shown::Pending),
            project.counts.total()
        ),
        (1, 1, 3)
    );
    let home = sluice_web::views::home::HomeView::new(&snapshot)
        .body()
        .unwrap();
    let home = home.as_str();
    let row = home
        .split("<li class=\"proj\">")
        .find(|row| row.contains(" held</span>"))
        .unwrap_or_else(|| panic!("no row for held: {home}"));
    assert!(
        row.contains("1 of 3 · <span class=\"p-paused\">1 paused</span>"),
        "{row}"
    );
    let (_, step) = f.get(&format!("/projects/id/{held}/steps/hold")).await;
    let hold = between(&step, "<p class=\"d-hold\">", "</p>");
    assert!(hold.contains("Paused by the owner"), "{hold}");
    assert!(
        !step.contains("Waits on: paused") && !step.contains(">paused</li>"),
        "{step}"
    );
    // its page links its log
    assert!(
        step.contains(&format!(
            "href=\"/projects/id/{held}/log?step=hold\">Log</a>"
        )),
        "{step}"
    );
}

/// A unit's log is what its steps did and said and its own records; the lease bookkeeping is
/// left out of All and kept for whoever asks for it.
#[tokio::test]
async fn a_units_log_keeps_to_its_steps_and_all_leaves_leases_out() {
    let f = Fixture::new().await;
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            for event in [
                json!({"kind":"step.status","step":"alpha-build","from":"running","to":"succeeded","error":null,"run_ids":[],"needs":{}}),
                json!({"kind":"step.status","step":"beta-build","from":"running","to":"failed","error":null,"run_ids":[],"needs":{}}),
                json!({"kind":"step.lease","step":"alpha-build","run":sluice_model::ids::RunId::new(),"lease":7,"resource":"cpu","amount":1,"state":"held","reason":null}),
            ] {
                let event: sluice_model::events::Event = serde_json::from_value(event).unwrap();
                tx.append_record(Some(id), event)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let reads = sluice_store::ReadPool::open(f._home.path(), 1).unwrap();
    let said = |raw: &'static str| {
        let reads = reads.clone();
        async move {
            let page = sluice_web::views::log::load(
                &reads,
                Some(id),
                sluice_web::views::log::LogQuery::parse(raw).unwrap(),
            )
            .await
            .unwrap();
            page.rows
                .iter()
                .map(|r| (r.kind.clone(), r.summary.clone()))
                .collect::<Vec<_>>()
        }
    };
    let alpha = said("unit=alpha").await;
    assert!(
        alpha.iter().any(|(_, s)| s.starts_with("alpha-build")),
        "{alpha:?}"
    );
    assert!(
        !alpha.iter().any(|(_, s)| s.contains("beta-build")),
        "{alpha:?}"
    );
    assert!(!alpha.iter().any(|(k, _)| k == "step.lease"), "{alpha:?}");
    let all = said("").await;
    assert!(all.iter().any(|(_, s)| s.contains("beta-build")), "{all:?}");
    assert!(!all.iter().any(|(k, _)| k == "step.lease"), "{all:?}");
    let leases = said("unit=alpha&kind=step.lease").await;
    assert_eq!(leases.len(), 1, "{leases:?}");
    let (_, unit) = f.get(&format!("/projects/id/{id}/units/alpha")).await;
    assert!(
        unit.contains(&format!(
            "href=\"/projects/id/{id}/log?unit=alpha\">Log</a>"
        )),
        "{unit}"
    );
}
