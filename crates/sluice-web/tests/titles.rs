//! Steps and units by their titles (SPEC §13): every page names a step by its title with its id
//! after it in mono, a recipe with a view draws its live units as rows under its stages, the
//! stopped ones first, and the plan's find finds a step by its title.
mod board_fixture;
mod plan_html;
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
async fn a_recipe_with_a_view_draws_its_live_units_as_rows_under_its_stages_attention_first() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    let (status, html) = f.get(&format!("/projects/id/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    // l2 failed: a module under Stopped, above everything else; l1 runs, a row under Running;
    // l3, held by l2, a row under Waiting
    assert_eq!(plan_html::place(&html, "l2"), "stopped");
    assert_eq!(plan_html::place(&html, "l1"), "running");
    assert_eq!(plan_html::place(&html, "l3"), "waiting");
    let at = |needle: &str| {
        html.find(needle)
            .unwrap_or_else(|| panic!("{needle}: {html}"))
    };
    assert!(at(">Stopped</h2>") < at("<!--r:s-l2-->"));
    assert!(at("<!--r:s-l2-->") < at(">Running</h2>"));
    assert!(at("<!--r:u-l1-->") < at(">Waiting</h2>"));
    assert!(at(">Waiting</h2>") < at("<!--r:u-l3-->"));
    // the recipe's block heads its stages' columns
    let running = plan_html::band(&html, "plan-running");
    for stage in ["fork", "work", "land"] {
        assert!(
            running.contains(&format!("<span class=\"sh-stage\">{stage}</span>")),
            "{running}"
        );
    }
    // a cell a stage, each with its state; one nothing has reached is drawn empty
    assert!(plan_html::cell(&html, "l1-work").contains("data-state=\"running\""));
    let fork = plan_html::cell(&html, "l3-fork");
    assert!(
        fork.starts_with("<li class=\"sc sc-empty\" data-state=\"blocked\""),
        "{fork}"
    );
    // a wait to or from a row is said in its row, never drawn
    let l3 = plan_html::row(&html, "l3");
    assert!(
        l3.contains("<p class=\"pl-sub pl-waits\">fork waits for <a"),
        "{l3}"
    );
    assert!(!html.contains("board-edges"), "{html}");
    // its title first, its id after it, and a way to its unit's page
    let l1 = plan_html::row(&html, "l1");
    assert!(
        l1.contains("<span class=\"pl-t\">FIG-1: Fix the cron driver</span><span class=\"pl-m\"><b class=\"pl-id\">l1</b> · lane"),
        "{l1}"
    );
    assert!(
        l1.contains(&format!("href=\"/projects/id/{id}/units/l1\"")),
        "{l1}"
    );
    // the recipe's view draws its summary in the unit's row
    assert!(
        l1.contains("<span class=\"uv-param\" title=\"ticket\">FIG-1</span>"),
        "{l1}"
    );
    // a recipe whose view does not check: its units are still rows, with one note
    let waiting = plan_html::band(&html, "plan-waiting");
    assert_eq!(
        waiting.matches("view does not check").count(),
        1,
        "{waiting}"
    );
    assert_eq!(plan_html::place(&html, "r1"), "waiting");
    // a unit of no recipe draws its own steps: kit's two, a connector between them
    let kit = plan_html::row(&html, "kit");
    assert!(kit.contains("<div class=\"pl-graph\""), "{kit}");
    assert_eq!(kit.matches("<path ").count(), 1, "{kit}");
}

#[tokio::test]
async fn every_page_names_a_step_by_its_title_and_its_id_after_it() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    // the step's page: its stage and title the heading, its id under it, the tab its title
    let (status, step) = f.get(&format!("/projects/id/{id}/steps/l1-work")).await;
    assert_eq!(status, StatusCode::OK, "{step}");
    assert!(
        step.contains("<h1 id=\"d-title\"><span class=\"d-stage\">work ·</span> FIG-1: Fix the cron driver</h1><p class=\"d-id\"><sluice-copy value=\"l1-work\"><code>l1-work</code><button type=\"button\" class=\"copy needs-js\" aria-label=\"Copy step id\""),
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
        unit.contains("<code>l2</code><button type=\"button\" class=\"copy needs-js\" aria-label=\"Copy unit id\"")
            && unit.contains("</sluice-copy><span class=\"meta\"> · from recipe <code>lane</code>"),
        "{unit}"
    );
    // a view this short is said on the unit's meta line, not in a card of its own
    assert!(
        unit.contains(
            "<span aria-hidden=\"true\">·</span><div class=\"uv-line\"><div class=\"uv uv-page\">"
        ) && !unit.contains("class=\"unit-view\""),
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
    // the plan: a loose step's row is its title, its id after it
    let (_, board) = f.get(&format!("/projects/id/{id}")).await;
    let watch = match plan_html::place(&board, "watch") {
        "margin" => plan_html::region(&board, "m-watch"),
        _ => plan_html::row(&board, "watch"),
    };
    assert!(watch.contains("Watches main for red"), "{watch}");
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
    // "parser" is in no id: l2's title, which each of its stages carries, and probe's doc; each
    // unit is kept whole
    let (_, html) = f.get(&format!("/projects/id/{id}?q=parser")).await;
    assert!(html.contains("2 units match “parser”."), "{html}");
    for (unit, found) in [("l2", true), ("probe", true), ("l1", false), ("l3", false)] {
        assert_eq!(
            !plan_html::place(&html, unit).is_empty(),
            found,
            "{unit}: {html}"
        );
    }
    // a doc's title too
    let (_, html) = f.get(&format!("/projects/id/{id}?q=bare+heading")).await;
    assert!(html.contains("1 unit matches “bare heading”."), "{html}");
}
