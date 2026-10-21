//! Steps and units by their titles (SPEC §13): every page names a step by its title with its id
//! after it in mono, a recipe with a view draws its live units as rows, the stops first, and the
//! plan's find finds a step by its title.
use crate::board_fixture;
use crate::plan_html;
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
async fn recipe_titles_and_search() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    {
        let html = f.opened(&format!("/projects/id/{id}")).await;
        // l2 failed: an item of Needs you, above everything else; l1 runs, a row at work; l3,
        // held by l2 and not begun, counted
        assert_eq!(plan_html::place(&html, "l2"), "stopped");
        assert_eq!(plan_html::place(&html, "l1"), "running");
        assert_eq!(plan_html::place(&html, "l3"), "");
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("{needle}: {html}"))
        };
        assert!(at("<!--r:n-stop-l2-work-->") < at("<!--r:u-l1-->"));
        // its row: its title, its state at its stage; open, its steps, each with its state, and
        // a way to its unit's page
        let l1 = plan_html::row(&html, "l1");
        assert!(
            l1.contains("<span class=\"ro-t\" title=\"FIG-1: Fix the cron driver\">FIG-1: Fix the cron driver</span>"),
            "{l1}"
        );
        assert!(
            plan_html::stage(l1, "l1-work").contains("data-state=\"running\""),
            "{l1}"
        );
        assert!(
            l1.contains(&format!("href=\"/projects/id/{id}/units/l1\"")),
            "{l1}"
        );
        assert!(!html.contains("board-edges"), "{html}");
        // the recipe's view draws its summary in the unit's row, a param not drawn again
        assert!(!l1.contains("uv-param"), "{l1}");
        // a recipe whose view does not check: its units are still rows, with one note
        let roster = plan_html::band(&html, "plan-roster");
        assert_eq!(roster.matches("view does not check").count(), 1, "{roster}");
        assert_ne!(plan_html::place(&html, "r1"), "");
        // a unit of no recipe draws its own steps: kit's two, a stage each on its track
        let kit = plan_html::row(&html, "kit");
        assert_eq!(kit.matches("class=\"current-stage").count(), 1, "{kit}");
        assert!(plan_html::stage(kit, "kit-a").contains(">a "), "{kit}");
    }
    {
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
    {
        // the step's page: its stage and title the heading, its id in its Details, the tab its
        // title
        let (status, step) = f.get(&format!("/projects/id/{id}/steps/l1-work")).await;
        assert_eq!(status, StatusCode::OK, "{step}");
        assert!(
        step.contains("<h1 id=\"d-title\" class=\"sh-t\" title=\"FIG-1: Fix the cron driver\"><span class=\"d-stage\">work ·</span> FIG-1: Fix the cron driver</h1><sluice-menu data-preserve-attr=\"data-rocket-host\"><details class=\"dm\"")
            && step.contains("<dt>Step</dt><dd><sluice-copy value=\"l1-work\" data-preserve-attr=\"data-rocket-host\"><code>l1-work</code><button type=\"button\" class=\"copy\" aria-label=\"Copy step\""),
        "{step}"
    );
        assert!(
            step.contains("<title>work · FIG-1: Fix the cron driver · titled · sluice</title>"),
            "{step}"
        );
        // a doc's first line is its title; the page says the rest of it once
        let (_, watch) = f.get(&format!("/projects/id/{id}/steps/watch")).await;
        assert!(
            watch.contains("<h1 id=\"d-title\" class=\"sh-t\" title=\"Watches main for red\">Watches main for red</h1>"),
            "{watch}"
        );
        assert!(
            watch.contains("<p class=\"sh-doc\">It says so on the thread.</p>"),
            "{watch}"
        );
        // a literal prompt's heading; and a step with neither is its id
        let (_, bare) = f.get(&format!("/projects/id/{id}/steps/bare")).await;
        assert!(
            bare.contains(
                "<h1 id=\"d-title\" class=\"sh-t\" title=\"Bare heading\">Bare heading</h1>"
            ),
            "{bare}"
        );
        let (_, plain) = f.get(&format!("/projects/id/{id}/steps/plain")).await;
        assert!(
            plain.contains("<h1 id=\"d-title\" class=\"sh-t\" title=\"plain\">plain</h1>")
                && !plain.contains("d-id"),
            "{plain}"
        );
        // the unit's page: its title the heading, its id, recipe and params in its Details; a view
        // of a param alone draws nothing on it
        let (status, unit) = f.get(&format!("/projects/id/{id}/units/l2")).await;
        assert_eq!(status, StatusCode::OK, "{unit}");
        assert!(
            unit.contains("<span>FIG-2: Stop the parser leak</span></h1>"),
            "{unit}"
        );
        assert!(
        unit.contains("<dt>Unit</dt><dd><sluice-copy value=\"l2\" data-preserve-attr=\"data-rocket-host\"><code>l2</code><button type=\"button\" class=\"copy\" aria-label=\"Copy unit\"")
            && unit.contains("<dt>Recipe</dt><dd><span>lane</span></dd>")
            && unit.contains("<dt>ticket</dt><dd><span>FIG-2</span></dd>"),
        "{unit}"
    );
        assert!(
            !unit.contains("uv-line")
                && !unit.contains("class=\"unit-view\"")
                && !unit.contains("uv-param"),
            "{unit}"
        );
        // its log is a link away
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
        // the plan: a loose step's row is its title
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
}
