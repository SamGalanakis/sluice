//! The project's board (docs("board")): drawn beside the plan from the project's own data,
//! each data component filled on the server, a bad part an inline error box, and a Button a
//! say to the project's fallback session refused once the board has moved on.
use crate::board_fixture;
use crate::plan_html;
use crate::seed;
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
    // The plan is still all there: the failed unit stopped.
    assert!(html.contains("<li id=\"n-stop-beta-build\""), "{html}");
    // A project with no board draws none, and no switch.
    let (_, plain) = f.get(&format!("/projects/id/{}", f.plain)).await;
    assert!(
        !plain.contains("has-panel")
            && !plain.contains("board-pane")
            && !plain.contains("view-switch")
    );
}

#[tokio::test]
async fn a_button_says_to_the_fallback_session_and_a_stale_board_refuses_it() {
    let f = Fixture::new().await;
    let action = format!("/projects/id/{}/board/action", f.id);
    let reply = super::forms::text(
        f.router()
            .oneshot(super::forms::post(
                &action,
                "board-form",
                &[
                    ("board_rev", "1"),
                    ("button", "0"),
                    ("field-0", "gamma"),
                    ("field-1", "go now"),
                ],
            ))
            .await
            .unwrap(),
    )
    .await;
    // The coordinator resolves who it reaches; the receipt names it, in the form's notice.
    assert!(
        reply.contains(
            "<p id=\"board-form-said\" class=\"form-said\" role=\"status\">Sent to @lash-f4.</p>"
        ),
        "{reply}"
    );
    let sent = f.commands.0.lock().unwrap().clone();
    let [CommandRequest::Say(say)] = sent.as_slice() else {
        panic!("{sent:?}")
    };
    assert_eq!((say.to.as_str(), say.owner, say.run), ("", true, None));
    assert_eq!(say.project, ProjectSelector::Id(f.id));
    assert_eq!(say.body, "Board: Retry the lane");
    assert_eq!(
        serde_json::to_value(&say.data).unwrap(),
        json!({"board_rev": 1, "action": "retry_lane", "params": {"force": true},
               "values": {"lane": "gamma", "note": "go now"}})
    );
    // A primary button checks its fields' rules first; a secondary one does not. A refused field
    // says why under it, the form's notice the whole.
    let refused = super::forms::text(
        f.router()
            .oneshot(super::forms::post(
                &action,
                "board-form",
                &[
                    ("board_rev", "1"),
                    ("button", "0"),
                    ("field-0", ""),
                    ("field-1", "x"),
                ],
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        refused.contains("<p id=\"board-form-said\" class=\"form-said\" role=\"alert\">Not sent: ")
            && refused.contains("data: elements <span id=\"board-form-err-field-0\" class=\"ou-error\" data-error-for=\"field-0\" data-ignore-morph>"),
        "{refused}"
    );
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
    assert_eq!(status, StatusCode::NO_CONTENT);
    // The board moves on: a press on the page drawn before is refused, visibly, and nothing is sent.
    let checked_board = projects::SetBoard {
        program: Some("root = Button(\"Again\", \"again\")".into()),
        expected_rev: None,
        reason: None,
        author: "orch".into(),
    }
    .check()
    .unwrap();
    f.writer
        .write(RetrySafety::NonIdempotent, {
            let id = f.id;
            move |tx| projects::board_set(tx, &ProjectSelector::Id(id), checked_board)
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
        page.contains("<input type=\"hidden\" name=\"expected_rev\" value=\"1\">"),
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
                        .header("sluice-form", "board-program-form")
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
    // a refusal is the form's notice alone: the draft stays in its box, untouched
    let (status, html) = send(form("save", "0", "root = Text(\"draft\")")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("role=\"alert\">") && html.contains("the board changed"),
        "{html}"
    );
    assert!(!html.contains("<textarea"), "{html}");
    // A program that does not check lists its lines.
    let (_, html) = send(form("save", "1", "root = Nope()")).await;
    assert!(
        html.contains("line 1: Nope is not a board component"),
        "{html}"
    );
    // saved: the section drawn anew at its new revision, its notice saying so
    let (_, html) = send(form("save", "1", "root = Text(\"saved\")")).await;
    assert!(
        html.contains("Saved (rev 2).")
            && html.contains("<input type=\"hidden\" name=\"expected_rev\" value=\"2\">"),
        "{html}"
    );
    let (_, html) = send(form("clear", "2", "root = Text(\"saved\")")).await;
    assert!(
        html.contains("Cleared.")
            && html.contains("Board cleared")
            && html.contains("<button>Undo</button>"),
        "{html}"
    );
    let (_, plan) = f.get(&format!("/projects/id/{}", f.id)).await;
    assert!(!plan.contains("board-pane"));
    // The preview draws a draft with the project's data, or its problems.
    let preview = |program: &'static str| {
        let router = f.router();
        let path = format!("{settings}/board/preview");
        async move {
            let response = router
                .oneshot(super::forms::post(
                    path,
                    "board-program-form",
                    &[("program", program)],
                ))
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
        .oneshot(super::forms::post(
            format!("{settings}/board/preview"),
            "board-program-form",
            &[("program", &program)],
        ))
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

/// The units the plan draws, in its order: its items of Needs you and its rows (each one's
/// `data-unit`), its cards of what finished and its Done index (each one's link).
fn units(html: &str) -> Vec<String> {
    if !html.contains("<div class=\"plan-sheet\">") {
        return vec![];
    }
    let sheet = between(html, "<div class=\"plan-sheet\">", "<!--/r:plan-done-->");
    let mut out: Vec<String> = vec![];
    let mut push = |unit: &str| {
        if !out.iter().any(|u| u == unit) {
            out.push(unit.to_owned());
        }
    };
    for rest in sheet.split(" data-unit=\"").skip(1) {
        push(&rest[..rest.find('"').unwrap()]);
    }
    for list in ["<ul class=\"fin\">", "<ol class=\"pl-idx\">"] {
        let Some(at) = sheet.find(list) else { continue };
        let items = &sheet[at..];
        let items = &items[..items.find("</").map_or(items.len(), |_| items.len())];
        for item in items.split("<li").skip(1) {
            let Some(h) = item.find(" href=\"") else {
                continue;
            };
            let href = &item[h + 7..];
            let href = &href[..href.find('"').unwrap()];
            push(&href[href.rfind('/').unwrap() + 1..]);
        }
    }
    out
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

#[tokio::test]
async fn filtered_units_progress_done_and_paused() {
    let f = Fixture::new().await;
    {
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
        // the failed unit is stopped; the done one (skipped, it never ran) a line of the index
        assert_eq!(units(&escaped), ["build"], "{escaped}");
        let (_, index) = f.get(&format!("/projects/id/{escaped_id}?show=done")).await;
        assert!(units(&index).is_empty(), "{index}");
        assert!(index.contains("1 finished units · History →"));
        assert!(escaped.contains("<li id=\"n-stop-start\""), "{escaped}");
        assert!(!escaped.contains("<script>failure"));
        assert!(!escaped.contains("Fixture <script>"));
        assert!(escaped.contains("&lt;script&gt;"));
        let region = between(&escaped, "id=\"project-board\"", "<sluice-drawer");
        assert!(!region.contains("data-init"));
        // By a step's id: each unit whole, stopped before waiting; the count says how many.
        let html = page("q=review").await;
        assert_eq!(units(&html), ["beta", "alpha"]);
        assert!(html.contains("2 units match “review”."), "{html}");
        // By a step's doc, in any case: a unit with no match is hidden whole.
        let html = page("q=PARSER").await;
        assert_eq!(units(&html), ["beta"]);
        assert!(html.contains("1 unit matches “PARSER”."));
        // By unit id; words in any order, across ids, titles and docs.
        assert_eq!(units(&page("q=alpha").await), ["alpha"]);
        assert_eq!(units(&page("q=output+beta").await), ["beta"]);
        // With show: attention keeps the failed unit, and the find narrows it.
        assert_eq!(units(&page("q=build&show=attention").await), ["beta"]);
        let none = page("q=build&show=done").await;
        assert!(units(&none).is_empty(), "{none}");
        assert!(none.contains("No unit matches “build”."), "{none}");
        assert!(
            none.contains(&format!("<a href=\"{base}?show=done\">Clear the find</a>")),
            "{none}"
        );
        // The field keeps the find, escaped; the page's stream asks for the same view, the find
        // in the page's `q` signal.
        let html = page("q=%3Cb%3E+x&show=active").await;
        assert!(html.contains("value=\"&lt;b&gt; x\""), "{html}");
        assert!(html.contains("“&#60;b&#62; x”"), "{html}");
        assert!(
            html.contains(&format!(
                "data-init=\"@get(&#39;{base}/stream?show=active&#39;"
            )) && html.contains("&#34;q&#34;:&#34;&#60;b&#62; x&#34;"),
            "{html}"
        );
        // Without a find there is no count, and every unit shows.
        let html = page("").await;
        assert_eq!(units(&html).len(), 2);
        assert!(html.contains("<p class=\"match-note meta\" role=\"status\"></p>"));
        // The live stream draws the plan under the same find.
        use futures_util::StreamExt;
        let response = f
            .router()
            .oneshot(
                Request::get(format!(
                    "{base}/stream?datastar=%7B%22ver%22%3A%22old%22%2C%22q%22%3A%22parser%22%7D"
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
            first.contains("data-unit=\"beta\"") && !first.contains("data-unit=\"alpha\""),
            "{first}"
        );
        assert!(first.contains("1 unit matches “parser”."), "{first}");
    }
    {
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
        let checked_board = projects::SetBoard {
            program: Some("root = Stack([red, head, metric])\nred = Output(\"tests-main\", \"red\")\nhead = Output(\"tests-main\", \"head\")\nmetric = Metric(\"Red\", \"SELECT json_extract(progress, '$.red') FROM steps WHERE project_id = ? AND step_id = 'tests-main'\")".into()),
            expected_rev: Some(sluice_model::ids::Revision(0)),
            reason: None,
            author: "orch".into(),
        }
        .check()
        .unwrap();
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                projects::board_set(tx, &ProjectSelector::Id(id), checked_board)?;
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
        red.contains("<span class=\"badge running\">")
            && red.contains("live</span><time data-ago=\"2026-10-06T10:00:00.5Z\" datetime=\"2026-10-06T10:00:00.5Z\" title=\"2026-10-06 10:00 UTC\">")
            && red.contains(" ago</time>"),
        "{red}"
    );
        // A field the progress lacks shows the output, unmarked.
        let head = output(&html, "head");
        assert!(
            head.contains("<span class=\"v\">old</span>") && !head.contains("tag"),
            "{head}"
        );
        assert!(
            html.contains("<span class=\"metric-v\">2</span><span class=\"metric-l\">Red</span>")
        );
        // While it runs its live progress is part of what it is doing now (it leads: the step has
        // written no message), not a section of its own.
        let (_, detail) = f.get(&step).await;
        assert!(!detail.contains("d-progress"), "{detail}");
        let section = between(&detail, "<article class=\"mod d-sec d-now\"", "</article>");
        assert!(
            section.contains(">Now</") && section.contains("Live progress, set"),
            "{section}"
        );
        assert!(!section.contains("No message yet."), "{section}");
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
                && red.contains("<span class=\"badge muted\">progress</span>")
                && !red.contains("live"),
            "{red}"
        );
        let (_, detail) = f.get(&step).await;
        let section = between(
            &detail,
            "<article class=\"mod d-sec d-progress\"",
            "</article>",
        );
        assert!(section.contains("by its last run"), "{section}");
    }
    {
        // 75 one-step units, each after the one before, all done
        let mut steps = serde_json::Map::new();
        let mut statuses = vec![];
        for i in 0..75 {
            let id = format!("u{i:02}");
            let mut step = json!({"run":"custom.open","tags":[format!("unit:{id}")], "in":{"prompt":{"default":"Read /workspace/notes/cenote/ideation/CONTEXT.md first. Task: a /prospect of the entire unfinished workspace with detailed recommendations and all verification evidence included in the final report."}}});
            if i > 0 {
                step["after"] = json!([format!("u{:02}", i - 1)]);
            }
            if i == 74 {
                step["doc"] = json!("Ship the parser fix");
            }
            if i == 73 {
                step["in"]["prompt"]["default"] = json!(
                    "Investigate every unfinished workspace recommendation with detailed Unicode résumé context and verification evidence before preparing the report for the reviewer"
                );
            }
            steps.insert(id.clone(), step);
            statuses.push((&*Box::leak(id.into_boxed_str()), "succeeded"));
        }
        let id = f.project("shelf", json!({"steps": steps}), &statuses).await;
        // their runs ended a minute apart, u74 last
        f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            for i in 0..75 {
                let attempt = format!("019a2b3c-4d5e-7f01-8234-5678900000{i:02}");
                let run = format!("019a2b3c-4d5e-7f01-8234-5678901000{i:02}");
                tx.sql().execute(
                    "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?1,?2,?3,1,1,'terminal','{}','hash','now')",
                    (&attempt, id.to_string(), format!("u{i:02}")),
                )?;
                tx.sql().execute(
                    "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at,finished_at) VALUES (?1,?2,?3,?4,1,1,'2026-10-06T10:00:00Z','2026-10-06T10:00:00Z',?5)",
                    (&run, id.to_string(), &attempt, format!("u{i:02}"), format!("2026-10-06T{:02}:{:02}:00Z", 11 + i / 60, i % 60)),
                )?;
            }
            tx.changed(Some(id), "plan");
            Ok(())
        })
        .await
        .unwrap();
        let (status, html) = f.get(&format!("/projects/id/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(!html.contains("plan-finished"));
        let (_, history) = f.get(&format!("/projects/id/{id}/history")).await;
        assert_eq!(
            history.matches("<li class=\"history-row\"").count(),
            50,
            "{history}"
        );
        assert!(history.find("/units/u74\"").unwrap() < history.find("/units/u26\"").unwrap());
        assert!(history.contains("Read /workspace/notes/cenote/ideation/CONTEXT.md first."));
        assert!(!history.contains("Task: a /prospect"));
        assert!(history.contains(
            "Investigate every unfinished workspace recommendation with detailed Unicode…"
        ));
        assert!(!history.contains("résumé context"));
        assert!(!history.contains("/units/u00\""));
        let (_, older) = f.get(&format!("/projects/id/{id}/history?page=1")).await;
        assert_eq!(older.matches("<li class=\"history-row\"").count(), 25);
        assert!(older.contains("/units/u00\"") && !older.contains("/units/u74\""));
        let (_, step) = f.get(&format!("/projects/id/{id}/steps/u00")).await;
        assert!(step.contains("Task: a /prospect"), "{step}");
        let (_, home) = f.get("/").await;
        assert!(!home.contains("Task: a /prospect"));
        // Show: Done, the index: every one, newest first
        let (_, html) = f.get(&format!("/projects/id/{id}?show=done")).await;
        let done = plan_html::band(&html, "plan-done");
        assert!(done.contains("75 finished units · History →"), "{done}");
        assert!(!done.contains("pl-idx"), "{done}");
        let (_, history) = f.get(&format!("/projects/id/{id}/history")).await;
        assert!(history.contains("Ship the parser fix"), "{history}");
    }
    {
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
        let html = f.opened(&format!("/projects/id/{held}")).await;
        // the plan: the paused one waits, a row whose stage says paused; nothing is at work
        assert_eq!(plan_html::place(&html, "hold"), "waiting", "{html}");
        let stage = plan_html::stage(&html, "hold");
        assert!(stage.contains("data-state=\"paused\""), "{stage}");
        assert!(
            plan_html::summary(&html).contains("0 at work · 1 waiting · <a href="),
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
        // home's card says it paused, as its plan does
        let (_, home) = f.get("/").await;
        let card = home
            .split("<article class=\"pm card\"")
            .find(|m| m.contains("aria-label=\"held\""))
            .unwrap_or_else(|| panic!("no card for held: {home}"));
        assert!(
            card.contains("<p class=\"pm-size\"><span class=\"g g-paused\" role=\"img\" aria-label=\"paused\">")
                && card.contains(&format!("<p class=\"pm-summary\">0 at work · 1 waiting · <a href=\"/projects/id/{held}/history\">1 of 3 done</a></p>")),
            "{card}"
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
                "href=\"/projects/id/{held}/log?step=hold\">Its log</a>"
            )),
            "{step}"
        );
    }

    {
        let project = f
            .project(
                "pause-author",
                json!({"steps":{"work":{"run":"custom.open"}}}),
                &[],
            )
            .await;
        for (author, steps, expected) in [
            (
                "alice",
                json!({"work":{"run":"custom.open","paused":"review"}}),
                Some("alice"),
            ),
            (
                "bob",
                json!({"work":{"run":"custom.open","paused":"review","doc":"Changed title","tags":["ship"]}}),
                Some("alice"),
            ),
            ("carol", json!({}), None),
            (
                "dave",
                json!({"work":{"run":"custom.open","paused":true}}),
                Some("dave"),
            ),
            (
                "erin",
                json!({"work":{"run":"custom.open","paused":false}}),
                None,
            ),
        ] {
            f.writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    seed::put_by(
                        tx,
                        project,
                        json!({"steps":steps}),
                        author,
                        "pause attribution",
                    )
                })
                .await
                .unwrap();
            let (status, page) = f.get(&format!("/projects/id/{project}/steps/work")).await;
            if author == "carol" {
                assert_eq!(status, StatusCode::NOT_FOUND);
            } else {
                assert_eq!(status, StatusCode::OK);
                if let Some(expected) = expected {
                    assert!(page.contains(&format!("Paused by {expected}")), "{page}");
                } else {
                    assert!(!page.contains("Paused by"), "{page}");
                }
            }
        }
    }
}
