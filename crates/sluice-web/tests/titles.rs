//! Steps and units by their titles (SPEC §13): every page names a step by its title with its id
//! after it in mono, a recipe with a view draws its live units as one lane matrix, the attention
//! rows first, and the board's search finds a step by its title.
mod board_fixture;
use axum::http::StatusCode;
use board_fixture::Fixture;
use sluice_model::{events::Event, ids::StepId};
use sluice_store::RetrySafety;

fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from} in {html}"));
    let end = html[start..].find(to).map_or(html.len(), |e| start + e);
    &html[start..end]
}

#[tokio::test]
async fn a_recipe_with_a_view_draws_its_live_units_as_a_lane_matrix_attention_first() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let (status, html) = f.get(&format!("/projects/id/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    // the matrix is split by band: Stopped (l2 failed, l3 held by it) above every other
    // matrix and unit, then Running (l1); a row a unit, its head counting its rows
    let rows_of = |m: &str| -> Vec<String> {
        m.split("<tr id=\"unit-")
            .skip(1)
            .map(|r| r[..r.find('"').unwrap()].to_owned())
            .collect()
    };
    let lanes: Vec<&str> = html
        .match_indices("data-matrix=\"lane\"")
        .map(|(at, _)| &html[at..at + html[at..].find("</section>").unwrap()])
        .collect();
    assert_eq!(lanes.len(), 2, "{html}");
    assert_eq!(rows_of(lanes[0]), ["l2", "l3"], "{}", lanes[0]);
    assert_eq!(rows_of(lanes[1]), ["l1"], "{}", lanes[1]);
    assert!(
        lanes[0].contains("2 units · 1 failed · 1 blocked"),
        "{}",
        lanes[0]
    );
    assert!(lanes[1].contains("1 unit · 1 running"), "{}", lanes[1]);
    let at = |needle: &str| {
        html.find(needle)
            .unwrap_or_else(|| panic!("{needle}: {html}"))
    };
    assert!(at(">Stopped</h2>") < at("data-matrix=\"lane\""));
    assert!(at(">Running</h2>") > at("<tr id=\"unit-l3\""));
    assert!(at(">Running</h2>") < at("<tr id=\"unit-l1\""));
    let matrix = format!("{}{}", lanes[0], lanes[1]);
    // the summary column is headed by what the view shows
    assert!(
        matrix.contains("<th scope=\"col\" class=\"mx-sum\">Ticket · last message</th>"),
        "{matrix}"
    );
    // a stage nothing has reached is a small mark, not a card
    assert!(
        matrix.contains("id=\"n-l3-fork\" class=\"node mx-dot is-pending is-blocked\""),
        "{matrix}"
    );
    // a wait to or from a row is said in its row, never drawn
    assert!(
        matrix.contains("<p class=\"waits said\">Waits for <a"),
        "{matrix}"
    );
    // the stages are columns, the frame sluice's: a pill per stage with the card's look
    for stage in ["fork", "work", "land"] {
        assert!(matrix.contains(&format!(
            "<th scope=\"col\" class=\"mx-stage\">{stage}</th>"
        )));
    }
    assert!(
        matrix.contains("id=\"n-l1-work\" class=\"node card is-running mx-pill\""),
        "{matrix}"
    );
    assert!(matrix.contains("data-step=\"l2-work\""), "{matrix}");
    // its title first, its id after in mono, a link to its unit
    assert!(
        matrix.contains(&format!(
            "href=\"/projects/id/{id}/units/l1\" aria-description=\"FIG-1: Fix the cron driver\">FIG-1: Fix the cron driver</a>"
        )),
        "{matrix}"
    );
    assert!(
        matrix.contains("<p class=\"mx-id\"><code>l1</code>"),
        "{matrix}"
    );
    // the recipe's view draws its summary; a phone reads the stages as a lane string
    assert!(
        matrix.contains("<span class=\"uv-param\" title=\"ticket\">FIG-2</span>"),
        "{matrix}"
    );
    assert!(
        matrix.contains("<p class=\"mx-lane fb-lane\" aria-label=\"Stages\">"),
        "{matrix}"
    );
    assert!(
        matrix.contains("fork<span class=\"lm\">✓</span>"),
        "{matrix}"
    );
    // its units are not drawn again as boxes
    assert!(!html.contains("id=\"unit-l1\" class=\"box"), "{html}");
    // a recipe whose view does not check: its units are still a matrix, with one note
    let rough = between(&html, "data-matrix=\"rough\"", "</section>");
    assert!(
        rough.contains("This recipe's view does not check"),
        "{rough}"
    );
    assert!(rough.contains("<tr id=\"unit-r1\""), "{rough}");
    // lines: none inside a row (its columns say the order), none to, from or across a matrix
    const EDGES: &str = "<sluice-board class=\"board\" edges=\"";
    let edges = between(&html, EDGES, "\"><div")[EDGES.len()..].to_owned();
    let edges: serde_json::Value = serde_json::from_str(&html_escape(&edges)).unwrap();
    let line = |from: &str, to: &str| {
        edges
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["from"]["id"] == from && e["to"]["id"] == to)
            .map(|e| e["line"].as_bool().unwrap())
    };
    assert_eq!(line("l1-fork", "l1-work"), Some(false));
    assert_eq!(line("l2-land", "l3-fork"), Some(false));
    assert_eq!(line("l1-land", "report"), Some(false));
    assert_eq!(line("probe", "l3-land"), Some(false));
    // inside a plain unit's box the line is drawn
    assert_eq!(line("kit-a", "kit-b"), Some(true));
}

fn html_escape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[tokio::test]
async fn every_page_names_a_step_by_its_title_and_its_id_after_it() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    // the step's page: its stage and title the heading, its id under it, the tab its title
    let (status, step) = f.get(&format!("/projects/id/{id}/steps/l1-work")).await;
    assert_eq!(status, StatusCode::OK, "{step}");
    assert!(
        step.contains("<h1 id=\"d-title\"><span class=\"d-stage\">work ·</span> FIG-1: Fix the cron driver</h1><p class=\"d-id\"><code>l1-work</code></p>"),
        "{step}"
    );
    assert!(
        step.contains("<title>work · FIG-1: Fix the cron driver · titled · sluice</title>"),
        "{step}"
    );
    // a doc's first line is its title; the page says the rest of it once
    let (_, watch) = f.get(&format!("/projects/id/{id}/steps/watch")).await;
    assert!(
        watch.contains("<h1 id=\"d-title\">Watches main for red</h1>"),
        "{watch}"
    );
    assert!(
        watch.contains("<p class=\"d-doc\">It says so on the thread.</p>"),
        "{watch}"
    );
    // a literal prompt's heading; and a step with neither is its id
    let (_, bare) = f.get(&format!("/projects/id/{id}/steps/bare")).await;
    assert!(
        bare.contains("<h1 id=\"d-title\">Bare heading</h1>"),
        "{bare}"
    );
    let (_, plain) = f.get(&format!("/projects/id/{id}/steps/plain")).await;
    assert!(
        plain.contains("<h1 id=\"d-title\">plain</h1>") && !plain.contains("d-id"),
        "{plain}"
    );
    // the unit's page: its title the heading, its id and recipe under it, its view in full
    let (status, unit) = f.get(&format!("/projects/id/{id}/units/l2")).await;
    assert_eq!(status, StatusCode::OK, "{unit}");
    assert!(
        unit.contains("<span>FIG-2: Stop the parser leak</span></h1>"),
        "{unit}"
    );
    assert!(
        unit.contains("<code>l2</code><span class=\"meta\"> · from recipe <code>lane</code>"),
        "{unit}"
    );
    assert!(
        unit.contains(
            "<section class=\"unit-view\" aria-labelledby=\"uv-h\"><p id=\"uv-h\" class=\"uv-h\">Summary</p><div class=\"uv uv-page\">"
        ),
        "{unit}"
    );
    // its view's values are named on its page ("ticket FIG-2"), and its log is a link away
    assert!(
        unit.contains("<span class=\"uv-param\" title=\"ticket\"><span class=\"uv-k\">ticket</span> FIG-2</span>"),
        "{unit}"
    );
    assert!(
        unit.contains(&format!(
            "<a href=\"/projects/id/{id}/log?unit=l2\">Log</a>"
        )),
        "{unit}"
    );
    assert!(
        unit.contains("<title>FIG-2: Stop the parser leak · titled · sluice</title>"),
        "{unit}"
    );
    // the index: a running step by its title (its stage before it), its id after it
    let (_, home) = f.get("/").await;
    assert!(
        home.contains("<span class=\"sref\"><span class=\"sref-stage\">work ·</span> <span class=\"sref-t\">FIG-1: Fix the cron driver</span> <code class=\"sref-id\">l1-work</code></span>"),
        "{home}"
    );
    assert!(home.contains("<span class=\"sref-t\">Watches main for red</span> <code class=\"sref-id\">watch</code>"), "{home}");
    // a failed step in the stopped line too
    assert!(home.contains("<span class=\"sref-t\">FIG-2: Stop the parser leak</span> <code class=\"sref-id\">l2-work</code>"), "{home}");
    // a board's multi-step unit outside a matrix names its stages; a solo unit's title is over it
    let (_, board) = f.get(&format!("/projects/id/{id}")).await;
    assert!(
        board.contains(
            "<p class=\"solo-title\" title=\"Watches main for red\"><span>Watches main for red</span></p>"
        ),
        "{board}"
    );
    // the log names a record's step by its title, linked, its id after it
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.append_record(
                Some(id),
                Event::StepStatus {
                    step: StepId::new("l1-work").unwrap(),
                    from: Some(sluice_model::commands::StepStatus::Pending),
                    to: sluice_model::commands::StepStatus::Running,
                    error: None,
                    run_ids: vec![],
                    needs: Default::default(),
                },
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let log = sluice_web::views::log::load(
        &f.dashboard.reads,
        Some(id),
        sluice_web::views::log::LogQuery::parse("").unwrap(),
    )
    .await
    .unwrap()
    .body()
    .unwrap()
    .as_str()
    .to_owned();
    assert!(
        log.contains(&format!("<a href=\"/projects/id/{id}/steps/l1-work\" title=\"FIG-1: Fix the cron driver\"><span class=\"sref\"><span class=\"sref-stage\">work ·</span> <span class=\"sref-t\">FIG-1: Fix the cron driver</span> <code class=\"sref-id\">l1-work</code></span></a> pending → running")),
        "{}",
        between(&log, "<div id=\"log-view\">", "</nav></div>")
    );
}

#[tokio::test]
async fn the_search_finds_a_step_by_its_title() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    // "parser" is in no id: l2's title, which each of its stages carries, and probe's doc
    let (_, html) = f.get(&format!("/projects/id/{id}?q=parser")).await;
    assert!(html.contains("4 steps match “parser”."), "{html}");
    let matrix = between(&html, "data-matrix=\"lane\"", "</section>");
    assert!(
        matrix.contains("<tr id=\"unit-l2\"") && !matrix.contains("<tr id=\"unit-l1\""),
        "{matrix}"
    );
    // a doc's title too
    let (_, html) = f.get(&format!("/projects/id/{id}?q=bare+heading")).await;
    assert!(html.contains("1 step matches “bare heading”."), "{html}");
}
